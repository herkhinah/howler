//! event handlers for the matrix-sdk client

use matrix_sdk::{
	event_handler::Ctx,
	room::Room,
	ruma::{
		events::{
			room::{
				member::{MembershipState, StrippedRoomMemberEvent},
				message::SyncRoomMessageEvent,
			},
			SyncStateEvent,
		},
		OwnedUserId,
	},
	Client,
};
use tokio::sync::mpsc;

use super::{
	custom_events::{TemplateEventContent, WebhookAccessTokenEventContent},
	join_room, reject_invitation,
};
use crate::{
	alert_renderer::AlertRendererChannelMessage, allow_list::AllowList,
	matrix::queuing::QueueChannelMessage, room_tokens::RoomTokenMap,
};

/// We've received a message. If it was not sent by the bot themself forward the
/// [EventId][matrix_sdk::ruma::EventId] to potentially confirm the federation
/// of a message.
pub async fn handle_sync_room_message_event(
	ev: SyncRoomMessageEvent,
	Ctx(user_id): Ctx<OwnedUserId>,
	Ctx(tx_queue): Ctx<mpsc::Sender<QueueChannelMessage>>,
) {
	if ev.sender() == user_id {
		return;
	}

	#[allow(clippy::expect_used)]
	tx_queue
		.send(QueueChannelMessage::ConfirmFederation(ev.event_id().to_owned()))
		.await
		.expect("channel tx_queue closed");
}

/// Accept room invitation
#[allow(clippy::unused_async)]
pub async fn handle_stripped_room_member_event(
	ev: StrippedRoomMemberEvent,
	room: Room,
	client: Client,
	Ctx(user_id): Ctx<OwnedUserId>,
	Ctx(allow_list): Ctx<&AllowList>,
) {
	if ev.state_key != user_id.as_str() {
		return;
	}

	if let MembershipState::Invite = &ev.content.membership {
		if allow_list.contains(&ev.sender) {
			join_room(client, user_id, room);
		} else if let Room::Invited(room) = room {
			reject_invitation(room);
		}
	}
}

/// An `com.famedly.howler_template` state event was set.
///
/// Sends the template to [AlertRenderer][crate::alert_renderer::AlertRenderer]
pub async fn handle_template_state_event(
	ev: SyncStateEvent<TemplateEventContent>,
	room: Room,
	Ctx(tx_renderer): Ctx<mpsc::Sender<AlertRendererChannelMessage>>,
) {
	if let SyncStateEvent::Original(ev) = ev {
		let plain = ev.content.plain;
		let html = ev.content.html;

		let room_id = room.room_id().to_owned();
		let event_id = ev.event_id;
		#[allow(clippy::expect_used)]
		tx_renderer
			.send(AlertRendererChannelMessage::RegisterTemplate { room_id, event_id, plain, html })
			.await
			.expect("channel tx_renderer closed");
	}
}

/// An `com.famedly.howler.webhook_access_token` state event was set.
pub async fn handle_webhook_access_token_state_event(
	ev: SyncStateEvent<WebhookAccessTokenEventContent>,
	room: Room,
) {
	if let SyncStateEvent::Original(ev) = ev {
		RoomTokenMap::global()
			.write()
			.await
			.register_token(room.room_id().to_owned(), ev.content.token);
	}
}
