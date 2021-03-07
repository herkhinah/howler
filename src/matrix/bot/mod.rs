//! howler bots

use std::{cmp::Reverse, sync::Arc};

use anyhow::{Context, Result};
use backoff::{backoff::Backoff, ExponentialBackoff};
use matrix_sdk::{
    config::{ClientConfig, RequestConfig, SyncSettings},
    room::{Invited, Room},
    ruma::{
        api::{
            client::{
                error::{Error as ApiError, ErrorKind::LimitExceeded},
                r0::message::send_message_event::Response,
            },
            error::{FromHttpResponseError, ServerError},
        },
        events::AnyMessageEventContent,
        identifiers::RoomId,
        UserId,
    },
    Client,
    Error::Http,
    HttpError,
};
use tokio::{sync::mpsc, time::Instant};

use self::{
    event_handlers::{
        handle_stripped_room_member_event, handle_sync_room_message_event,
        handle_template_state_event, handle_webhook_access_token_state_event,
    },
    metrics::BotMetrics,
    settings::BotSettings,
};
use super::queuing::{QueueChannelMessage, SendError, UnconfirmedMessage};
use crate::{
    alert_renderer::AlertRendererChannelMessage, rendered_alert::RenderedAlert, settings::Settings,
};

pub mod custom_events;
pub mod event_handlers;
pub mod http_client;
pub mod settings;

mod metrics;

/// Messages for the Bot
///
/// there will be other stuff for the bots to do, like invite other bots, but for now this enum contains only one variant
#[derive(Debug, Clone)]
pub enum BotChannelMessage {
    Send {
        entries: Vec<Reverse<RenderedAlert>>,
        content: AnyMessageEventContent,
        room: Arc<RoomId>,
    },
}

pub struct Bot {
    client: Client,

    metrics: BotMetrics,

    rx_bot: mpsc::Receiver<BotChannelMessage>,
    tx_bot: mpsc::Sender<BotChannelMessage>,

    tx_queue: mpsc::Sender<QueueChannelMessage>,

    bot_id: Arc<UserId>,

    is_backup: bool,
}

impl Bot {
    pub async fn new(
        settings: &BotSettings,
        tx_renderer: mpsc::Sender<AlertRendererChannelMessage>,
        tx_queue: mpsc::Sender<QueueChannelMessage>,
        is_backup: bool,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::channel(64);

        let client_config = ClientConfig::new()
            .passphrase(settings.password.clone())
            .request_config(RequestConfig::new().disable_retry())
            .client(Arc::new(http_client::Client::new(&settings.user_id)));

        let client = Client::new_with_config(settings.homeserver.clone(), client_config)
            .await
            .context("failed to create client")?;

        client
            .login(settings.user_id.localpart(), &settings.password, None, None)
            .await
            .context("failed to login homeserver")?;

        let bot_id = Arc::from(
            client
                .user_id()
                .await
                .context("could not get UserId from Client")?,
        );

        tracing::info!("bot {bot_id} logged in");

        client
            .register_event_handler_context(Arc::clone(&bot_id))
            .register_event_handler_context(tx_queue.clone())
            .register_event_handler_context(tx_renderer.clone())
            .register_event_handler_context(&Settings::global().allow_invites)
            .register_event_handler(handle_stripped_room_member_event)
            .await
            .register_event_handler(handle_sync_room_message_event)
            .await
            .register_event_handler(handle_template_state_event)
            .await
            .register_event_handler(handle_webhook_access_token_state_event)
            .await;

        let metrics = BotMetrics::new(&bot_id);

        Ok(Self {
            client,

            metrics,

            tx_bot: tx,
            rx_bot: rx,

            tx_queue,

            bot_id,

            is_backup,
        })
    }

    pub fn is_backup(&self) -> bool {
        self.is_backup
    }

    pub fn get_channel(&self) -> mpsc::Sender<BotChannelMessage> {
        self.tx_bot.clone()
    }

    pub fn bot_id(&self) -> Arc<UserId> {
        self.bot_id.clone()
    }

    pub async fn run(mut self) {
        tokio::spawn({
            let client = self.client.clone();
            async move {
                client.sync(SyncSettings::default()).await;
            }
        });

        while let Some(message) = self.rx_bot.recv().await {
            let _meter = self.metrics.time_active.start_timer();

            match message {
                // we have a message ready for sending
                BotChannelMessage::Send {
                    mut entries,
                    content,
                    room,
                } => {
                    for Reverse(RenderedAlert { metadata, .. }) in &mut entries {
                        metadata.last_sending_attempt = Some(Instant::now());
                        metadata.sending_attempts += 1;
                    }

                    let send_result = self.send(room, content, entries).await;

                    self.tx_queue
                        .send(QueueChannelMessage::SendResult(send_result))
                        .await
                        .expect("tx_queue closed");
                }
            }
        }
    }

    /// send message
    async fn send(
        &mut self,
        room_id: Arc<RoomId>,
        message: AnyMessageEventContent,
        entries: Vec<Reverse<RenderedAlert>>,
    ) -> Result<UnconfirmedMessage, SendError> {
        let room = match self.client.get_room(&room_id) {
            Some(Room::Joined(room)) => room,
            other => {
                if let Some(Room::Invited(room)) = other {
                    join_room(self.client.clone(), self.bot_id(), Room::Invited(room));
                }
                return Err(SendError::Backoff {
                    room: room_id,
                    bot: self.bot_id(),
                    entries,
                    from: Instant::now(),
                });
            }
        };

        // send message and match result
        match room.send(message, None).await {
            // message was successfully sent and we've got an `EventId`
            Ok(Response { event_id, .. }) => {
                self.metrics.record_message_send_success(&room_id);
                Ok(UnconfirmedMessage {
                    event_id,
                    bot_id: self.bot_id(),
                    target_room: room_id,
                    batch_entries: entries,
                })
            }
            // message failed to send but we got a deserializable error message from the server
            Err(Http(HttpError::ClientApi(FromHttpResponseError::Http(ServerError::Known(
                err,
            ))))) => {
                self.metrics.record_message_send_error(&room_id, &err);
                // check if server returned M_LIMIT_EXCEEDED (ratelimit)
                match err {
                    // we've got ratelimited
                    ApiError {
                        kind:
                            LimitExceeded {
                                retry_after_ms: Some(duration),
                            },
                        ..
                    } => Err(SendError::Ratelimit {
                        room: room_id,
                        bot: self.bot_id(),
                        entries,
                        from: Instant::now(),
                        duration,
                    }),
                    // some other error occured
                    _ => Err(SendError::Backoff {
                        room: room_id,
                        bot: self.bot_id(),
                        entries,
                        from: Instant::now(),
                    }),
                }
            }
            // message failed to send for what ever reasons and we couldn't deserialize the server response if there was one
            Err(_) => Err(SendError::Backoff {
                room: room_id,
                bot: self.bot_id(),
                entries,
                from: Instant::now(),
            }),
        }
    }
}

fn reject_invitation(room: Invited) {
    tokio::spawn(async move {
        let mut backoff = ExponentialBackoff::default();

        while let Err(err) = room.reject_invitation().await {
            if let Http(HttpError::ClientApi(FromHttpResponseError::Http(ServerError::Known(
                ApiError {
                    kind:
                        LimitExceeded {
                            retry_after_ms: Some(duration),
                        },
                    ..
                },
            )))) = err
            {
                tokio::time::sleep(duration).await;
            } else if let Some(duration) = backoff.next_backoff() {
                tokio::time::sleep(duration).await;
            } else {
                return;
            }
        }
    });
}

fn join_room(client: Client, user_id: Arc<UserId>, room: Room) {
    tokio::spawn(async move {
        let mut backoff = ExponentialBackoff::default();

        while let Err(err) = client.join_room_by_id(room.room_id()).await {
            tracing::warn!(
                "bot {user_id} could not accept invitation for room {}: {err:?}",
                room.room_id()
            );

            if let HttpError::ClientApi(FromHttpResponseError::Http(ServerError::Known(
                ApiError {
                    kind:
                        LimitExceeded {
                            retry_after_ms: Some(duration),
                        },
                    ..
                },
            ))) = err
            {
                tokio::time::sleep(duration).await;
            } else if let Some(duration) = backoff.next_backoff() {
                tokio::time::sleep(duration).await;
            } else {
                return;
            }
        }
    });
}
