use std::{collections::HashMap, io::Cursor, time::Duration};

use axum::http::StatusCode;
use backoff::{backoff::Backoff, ExponentialBackoff};
use matrix_sdk::{
    room::Joined,
    ruma::{
        api::error::FromHttpResponseError, events::AnyMessageEventContent, identifiers::RoomId,
        EventId, UserId,
    },
    Client,
};
use mime::Mime;
use prometheus::{Histogram, IntCounter, IntCounterVec};
use tokio::time::Instant;

use super::queuing::{BatchedContent, BatchedMessage, UnconfirmedMessage};
use crate::matrix::sending::batch_entries::BatchEntries;

#[derive(Debug, Clone)]
pub struct BotMetrics {
    http_requests: IntCounter,
    http_err: IntCounter,

    time_active: Histogram,
}

impl BotMetrics {
    pub fn new(bot_id: &UserId) -> Self {
        use prometheus::{
            exponential_buckets, histogram_opts, labels, opts, register_histogram,
            register_int_counter,
        };

        let http_requests = register_int_counter!(opts!(
            "http_requests_total",
            "total number of http requests made by bot",
            labels! {"bot" => bot_id.as_str()}
        )
        .namespace("howler")
        .subsystem("bot"))
        .unwrap();

        let http_err = register_int_counter!(opts!(
            "http_requests_failed",
            "total number of failed http requests",
            labels! {"bot" => bot_id.as_str()}
        )
        .namespace("howler")
        .subsystem("bot"))
        .unwrap();

        let time_active = register_histogram!(histogram_opts!(
            "active_seconds",
            "time bot is active",
            exponential_buckets(0.01, 2., 12).unwrap(),
            labels! {"bot".to_string() => bot_id.to_string()}
        )
        .namespace("howler")
        .subsystem("bot"))
        .unwrap();

        Self {
            http_requests,
            http_err,
            time_active,
        }
    }
}

#[derive(Debug)]
struct RoomData {
    send_ratelimited_until: Option<Instant>,
    backoff: Option<(Instant, ExponentialBackoff)>,

    metrics: RoomMetrics,
}

impl RoomData {
    pub fn new(bot_id: &UserId, room_id: &RoomId) -> Self {
        let metrics = RoomMetrics::new(bot_id, room_id);

        Self {
            send_ratelimited_until: None,
            backoff: None,
            metrics,
        }
    }

    pub fn ratelimit(&mut self, duration: Duration) {
        self.metrics.ratelimit.observe(duration.as_secs_f64());

        self.send_ratelimited_until = Some(Instant::now() + duration);
    }

    pub fn stop_backoff(&mut self) {
        self.backoff = None;
    }

    pub fn backoff(&mut self) {
        let duration = match &mut self.backoff {
            Some((retry_at, backoff)) => {
                // unwrap is safe because mbackoffax_elapsed_time is set to None
                let duration = backoff.next_backoff().unwrap();
                *retry_at = Instant::now() + duration;
                duration
            }
            None => {
                let mut backoff = ExponentialBackoff {
                    max_elapsed_time: None,
                    ..Default::default()
                };

                // unwrap is safe because we set max_elapsed_time to None
                let duration = backoff.next_backoff().unwrap();
                self.backoff = Some((Instant::now() + duration, backoff));
                duration
            }
        };

        self.metrics.backoff.observe(duration.as_secs_f64());
    }

    pub fn is_ready_at(&self, instant: Instant) -> bool {
        let ready_in = std::cmp::max(
            self.send_ratelimited_until,
            self.backoff.as_ref().map(|(instant, _)| *instant),
        );

        !matches!(ready_in, Some(ready_in) if ready_in > instant)
    }
}

#[derive(Debug, Clone)]
struct RoomMetrics {
    pub ratelimit: Histogram,
    pub backoff: Histogram,

    pub sending_failed_batch_entries: IntCounterVec,
    pub sending_failed_messages: IntCounterVec,
}

impl RoomMetrics {
    pub fn new(bot_id: &UserId, room_id: &RoomId) -> Self {
        use prometheus::{
            exponential_buckets, histogram_opts, labels, opts, register_histogram,
            register_int_counter_vec,
        };

        let ratelimit = register_histogram!(histogram_opts!(
            "bot_roomratelimit_seconds",
            "time bot has been ratelimited in room",
            exponential_buckets(1., 2., 12).unwrap(),
            labels! {
                "room".to_string() => room_id.to_string(),
                "bot".to_string() => bot_id.to_string()
            }
        ))
        .unwrap();

        let backoff = register_histogram!(histogram_opts!(
            "bot_roombackoff_seconds",
            "time bot has been backoffed because of server errors",
            exponential_buckets(0.5, 1.5, 12).unwrap(),
            labels! {
                "room".to_string() => room_id.to_string(),
                "bot".to_string() => bot_id.to_string()
            }
        ))
        .unwrap();

        let sending_failed_batch_entries = register_int_counter_vec!(
            opts!(
                "send_failed_batch_entries",
                "amount of alerts failed to send ignoring federation confirmation",
                labels! {
                    "room" => room_id.as_str(),
                    "bot" => bot_id.as_str()
                }
            ),
            &["http_status"]
        )
        .unwrap();

        let sending_failed_messages = register_int_counter_vec!(
            opts!(
                "send_failed_messages",
                "amount of messages failed to send ignoring federation confirmation",
                labels! {
                    "room" => room_id.as_str(),
                    "bot" => bot_id.as_str()
                }
            ),
            &["http_status"]
        )
        .unwrap();

        Self {
            ratelimit,
            backoff,

            sending_failed_batch_entries,

            sending_failed_messages,
        }
    }
}

pub struct Bot {
    client: Client,

    metrics: BotMetrics,

    rooms: HashMap<RoomId, RoomData>,

    bot_id: UserId,
    upload_ratelimit: Option<Instant>,

    is_backup: bool,
}

impl Bot {
    /// creates new bot
    pub async fn new(client: Client, is_backup: bool) -> Self {
        let bot_id = client.user_id().await.unwrap();
        let metrics = BotMetrics::new(&bot_id);

        Self {
            client,
            metrics,
            rooms: HashMap::new(),
            bot_id,
            upload_ratelimit: None,

            is_backup,
        }
    }

    /// check if bot is ready to send message to room
    pub fn is_ready(&self, room_id: &RoomId, upload: bool) -> bool {
        let now = Instant::now();

        if upload {
            if let Some(instant) = self.upload_ratelimit {
                if instant > now {
                    return false;
                }
            }
        }

        match self.rooms.get(room_id) {
            Some(room) => room.is_ready_at(now),
            None => true,
        }
    }

    /// send message
    pub async fn send(
        mut self,
        mut message: BatchedMessage,
    ) -> (Self, Result<UnconfirmedMessage, (RoomId, BatchEntries)>) {
        use matrix_sdk::{
            ruma::api::{
                client::error::{Error as ApiError, ErrorKind::LimitExceeded},
                error::ServerError,
            },
            Error::Http,
            HttpError,
        };

        let _timer = self.metrics.time_active.start_timer();

        if let Some(room) = self.client.get_joined_room(&message.target_room) {
            let room_data = match self.rooms.get_mut(&message.target_room) {
                Some(room_data) => room_data,
                None => {
                    let room_data = RoomData::new(&self.bot_id, &message.target_room);
                    self.rooms
                        .try_insert(message.target_room.clone(), room_data)
                        .unwrap()
                }
            };

            let result = match message.content {
                BatchedContent::File(content) => {
                    Self::send_batched_files(room, room_data, &mut self.upload_ratelimit, content)
                        .await
                }
                BatchedContent::Message(content) => {
                    Self::send_batched_messages(room, room_data, content).await
                }
            };

            let now = Instant::now();

            for metadata in message.entries.metadata() {
                metadata.last_sending_attempt = Some(now);
                metadata.sending_attempts += 1;
            }

            match result {
                Ok(event_id) => {
                    let unconfirmed = UnconfirmedMessage {
                        event_id,
                        bot_id: self.bot_id.clone(),
                        target_room: message.target_room,
                        batch_entries: message.entries,
                    };

                    (self, Ok(unconfirmed))
                }

                Err(err) => {
                    tracing::warn!(
                        "bot {} failed to send message or file: {:?}",
                        self.bot_id,
                        err
                    );

                    let status_code = match err {
                        Http(HttpError::Server(code)) => Some(code),
                        Http(HttpError::ClientApi(FromHttpResponseError::Http(err))) => {
                            if let ServerError::Known(ApiError {
                                kind:
                                    LimitExceeded {
                                        retry_after_ms: Some(retry_after),
                                    },
                                ..
                            }) = &err
                            {
                                room_data.ratelimit(*retry_after);
                            } else {
                                room_data.backoff();
                            }

                            match err {
                                ServerError::Known(err) => Some(err.status_code),
                                ServerError::Unknown(_) => None,
                            }
                        }
                        _ => None,
                    };
                    let status_code = status_code.as_ref().map_or("unknown", StatusCode::as_str);

                    room_data
                        .metrics
                        .sending_failed_batch_entries
                        .with_label_values(&[status_code])
                        .inc_by(message.entries.len() as u64);
                    room_data
                        .metrics
                        .sending_failed_messages
                        .with_label_values(&[status_code])
                        .inc();

                    (self, Err((message.target_room, message.entries)))
                }
            }
        } else {
            tracing::error!("bot {} not in room {}", self.bot_id, message.target_room);
            // TODO: invite bot

            (self, Err((message.target_room, message.entries)))
        }
    }

    async fn send_batched_messages(
        room: Joined,
        room_data: &mut RoomData,
        message: AnyMessageEventContent,
    ) -> Result<EventId, matrix_sdk::Error> {
        use matrix_sdk::{
            ruma::api::{
                client::{
                    error::{Error as ApiError, ErrorKind::LimitExceeded},
                    r0::message::send_message_event::Response,
                },
                error::ServerError,
            },
            Error::Http,
            HttpError,
        };

        let result = match room.send(message, None).await {
            Ok(Response { event_id, .. }) => {
                room_data.stop_backoff();

                Ok(event_id)
            }
            Err(err) => {
                if let Http(HttpError::ClientApi(FromHttpResponseError::Http(
                    ServerError::Known(ApiError {
                        kind:
                            LimitExceeded {
                                retry_after_ms: Some(retry_after),
                            },
                        ..
                    }),
                ))) = &err
                {
                    room_data.ratelimit(*retry_after);
                } else {
                    room_data.backoff();
                }

                Err(err)
            }
        };

        result
    }

    async fn send_batched_files(
        room: Joined,
        room_data: &mut RoomData,
        upload_ratelimit: &mut Option<Instant>,
        file: Vec<u8>,
    ) -> Result<EventId, matrix_sdk::Error> {
        use matrix_sdk::{
            ruma::api::{
                client::{
                    error::{Error as ApiError, ErrorKind::LimitExceeded},
                    r0::message::send_message_event::Response,
                },
                error::ServerError,
            },
            Error::Http,
            HttpError,
        };

        let mime: Mime = "application/x-tar".parse().unwrap();
        let mut reader = Cursor::new(file);

        match room
            .send_attachment("error_log.tar", &mime, &mut reader, None)
            .await
        {
            Ok(Response { event_id, .. }) => {
                room_data.stop_backoff();

                Ok(event_id)
            }
            Err(err) => {
                if let Http(HttpError::ClientApi(FromHttpResponseError::Http(
                    ServerError::Known(ApiError {
                        kind:
                            LimitExceeded {
                                retry_after_ms: Some(retry_after),
                            },
                        ..
                    }),
                ))) = &err
                {
                    room_data.ratelimit(*retry_after);

                    *upload_ratelimit = Some(Instant::now() + *retry_after);
                } else {
                    room_data.backoff();
                }

                Err(err)
            }
        }
    }

    /// checks if bot is backup bot
    pub fn is_backup(&self) -> bool {
        self.is_backup
    }

    /// get [UserId] of bot
    pub fn user_id(&self) -> &UserId {
        &self.bot_id
    }
}
