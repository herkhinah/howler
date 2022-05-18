//! howler bots

use std::{cmp::Reverse, sync::Arc};

use anyhow::{Context, Result};
use backoff::{backoff::Backoff, ExponentialBackoff};
use matrix_sdk::{
	config::{RequestConfig, SyncSettings},
	room::{Invited, Room},
	ruma::{
		api::{
			client::{error::ErrorKind::LimitExceeded, message::send_message_event::v3::Response},
			error::{FromHttpResponseError, ServerError},
		},
		events::AnyMessageLikeEventContent,
		OwnedRoomId, OwnedUserId,
	},
	Client,
	Error::Http,
	HttpError, RumaApiError,
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
/// there will be other stuff for the bots to do, like invite other bots, but
/// for now this enum contains only one variant
#[derive(Debug, Clone)]
pub enum BotChannelMessage {
	/// send message to room
	Send {
		/// the entries that where batched together into a single message
		entries: Vec<Reverse<RenderedAlert>>,
		/// the batched message to send
		content: AnyMessageLikeEventContent,
		/// the target room
		room: OwnedRoomId,
	},
}

/// struct representing a bot
pub struct Bot {
	/// matrix-sdk client of bot
	client: Client,

	/// prometheus meters used by bot
	metrics: BotMetrics,

	/// channel to receive messages to send from the
	/// [BatchQueue](crate::matrix::queuing::batch::BatchQueue)
	rx_bot: mpsc::Receiver<BotChannelMessage>,
	/// channel the [BatchQueue](crate::matrix::queuing::batch::BatchQueue) uses
	/// to send us messages for us to send
	tx_bot: mpsc::Sender<BotChannelMessage>,

	/// channel to notify the
	/// [BatchQueue](crate::matrix::queuing::batch::BatchQueue) about received
	/// messages for federation confirmation or the result of a message sending
	/// attempt
	tx_queue: mpsc::Sender<QueueChannelMessage>,

	/// the user id of the bot
	bot_id: OwnedUserId,

	/// is the bot a backup bot
	is_backup: bool,
}

impl Bot {
	/// construct a bot
	pub async fn new(
		settings: &BotSettings,
		tx_renderer: mpsc::Sender<AlertRendererChannelMessage>,
		tx_queue: mpsc::Sender<QueueChannelMessage>,
		is_backup: bool,
	) -> Result<Self> {
		let (tx, rx) = mpsc::channel(64);

		let client = Client::builder()
			.homeserver_url(&settings.homeserver)
			.request_config(RequestConfig::new().disable_retry())
			.http_client(Arc::new(
				http_client::Client::new(&settings.user_id)
					.context("failed to construct http client")?,
			))
			.build()
			.await
			.context("failed to construct client")?;

		client
			.login(settings.user_id.localpart(), &settings.password, None, None)
			.await
			.context("failed to login homeserver")?;

		let bot_id = client.user_id().await.context("could not get UserId from Client")?;

		tracing::info!("bot {bot_id} logged in");

		client
			.register_event_handler_context(bot_id.clone())
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

		let metrics = BotMetrics::new(&bot_id).context("failed to register prometheus meters")?;

		Ok(Self { client, metrics, tx_bot: tx, rx_bot: rx, tx_queue, bot_id, is_backup })
	}

	/// check if bot is a backup bot
	pub fn is_backup(&self) -> bool {
		self.is_backup
	}

	/// get channel to send messages to bot
	pub fn get_channel(&self) -> mpsc::Sender<BotChannelMessage> {
		self.tx_bot.clone()
	}

	/// get user id of bot
	pub fn bot_id(&self) -> OwnedUserId {
		self.bot_id.clone()
	}

	/// run the main event loop of the bot
	/// the event loop consists of the bot receiving messages and trying to send
	/// them
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
				BotChannelMessage::Send { mut entries, content, room } => {
					for Reverse(RenderedAlert { metadata, .. }) in &mut entries {
						metadata.last_sending_attempt = Some(Instant::now());
						metadata.sending_attempts += 1;
					}

					let send_result = self.send(room, content, entries).await;

					#[allow(clippy::expect_used)]
					self.tx_queue
						.send(QueueChannelMessage::SendResult(send_result))
						.await
						.expect("channel tx_queue closed");
				}
			}
		}
	}

	/// send message
	async fn send(
		&mut self,
		room_id: OwnedRoomId,
		message: AnyMessageLikeEventContent,
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

		self.metrics.record_message_send(&room_id);

		// send message and match result
		match room.send(message, None).await {
			// message was successfully sent and we've got an `EventId`
			Ok(Response { event_id, .. }) => Ok(UnconfirmedMessage {
				event_id,
				bot_id: self.bot_id(),
				target_room: room_id,
				batch_entries: entries,
			}),
			// message failed to send but we got a deserializable error message from the server
			Err(Http(HttpError::Api(FromHttpResponseError::Server(ServerError::Known(
				RumaApiError::ClientApi(err),
			))))) => {
				self.metrics.record_message_send_error(&room_id, Some(&err));
				// check if server returned M_LIMIT_EXCEEDED (ratelimit)
				match err {
					// we've got ratelimited
					matrix_sdk::ruma::api::client::Error {
						kind: LimitExceeded { retry_after_ms: Some(duration) },
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
			// message failed to send for what ever reasons and we couldn't deserialize the server
			// response if there was one
			Err(_) => {
				self.metrics.record_message_send_error(&room_id, None);
				Err(SendError::Backoff {
					room: room_id,
					bot: self.bot_id(),
					entries,
					from: Instant::now(),
				})
			}
		}
	}
}

/// try to reject room invitation repeatedly via backoff
fn reject_invitation(room: Invited) {
	tokio::spawn(async move {
		let mut backoff = ExponentialBackoff::default();

		while let Err(err) = room.reject_invitation().await {
			if let Http(HttpError::Api(FromHttpResponseError::Server(ServerError::Known(
				RumaApiError::ClientApi(matrix_sdk::ruma::api::client::Error {
					kind: LimitExceeded { retry_after_ms: Some(duration) },
					..
				}),
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

/// try to join room repeatedly via backoff
fn join_room(client: Client, user_id: OwnedUserId, room: Room) {
	tokio::spawn(async move {
		let mut backoff = ExponentialBackoff::default();

		while let Err(err) = client.join_room_by_id(room.room_id()).await {
			tracing::warn!(
				"bot {user_id} could not accept invitation for room {}: {err:?}",
				room.room_id()
			);

			if let HttpError::Api(FromHttpResponseError::Server(ServerError::Known(
				RumaApiError::ClientApi(matrix_sdk::ruma::api::client::Error {
					kind: LimitExceeded { retry_after_ms: Some(duration) },
					..
				}),
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
