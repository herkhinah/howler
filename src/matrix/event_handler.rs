use anyhow::{Context, Result};
use backoff::{future::retry, Error as RetryError, ExponentialBackoff};
use matrix_sdk::{
    self, async_trait,
    room::{Joined, Room},
    ruma::{
        events::{
            room::{
                member::{MemberEventContent, MembershipState},
                message::MessageEventContent,
            },
            AnyMessageEventContent, StrippedStateEvent, SyncMessageEvent,
        },
        identifiers::{EventId, RoomId, UserId},
    },
    Client, CustomEvent, EventHandler,
};
use tokio::sync::mpsc;

use super::{
    channels::{EventIdChannel, InviteChannel, JoinChannel, OnceCellMPSC, TemplateChannel},
    invites::Invite,
    renderer::Template,
};
use crate::{room_tokens::RoomTokenMap, settings::Settings};

#[derive(Clone)]
pub struct HowlerEventHandler {
    client: Client,
    user_id: UserId,
    tx_event_id: mpsc::Sender<EventId>,
    tx_template: mpsc::Sender<(EventId, RoomId, Template)>,

    tx_invite: mpsc::Sender<Invite>,
    tx_join: mpsc::Sender<(UserId, RoomId)>,
}

impl HowlerEventHandler {
    pub async fn new(client: Client) -> Result<Self> {
        let user_id = client
            .user_id()
            .await
            .context("could not get UserId from Client")?;

        Ok(Self {
            client,
            user_id,

            tx_event_id: EventIdChannel::sender(),
            tx_template: TemplateChannel::sender(),
            tx_invite: InviteChannel::sender(),
            tx_join: JoinChannel::sender(),
        })
    }

    fn send_token(&self, room: Joined) {
        let token_map = RoomTokenMap::global().clone();

        tokio::spawn(async move {
            let backoff = ExponentialBackoff {
                max_elapsed_time: None,
                ..Default::default()
            };

            let url_api_prefix = Settings::global().url_api_prefix.as_str();
            let room = &room;

            let token = &token_map
                .get_or_create_token(room.room_id())
                .context("failed to get or generate token")
                .unwrap();

            retry(backoff, || async move {
                let content = AnyMessageEventContent::RoomMessage(MessageEventContent::text_plain(
                    format!("{}/{}", url_api_prefix, token),
                ));

                room.send(content, None)
                    .await
                    .map_err(RetryError::Transient)
            })
            .await
            .unwrap();
        });
    }

    fn join_room(&self, room_id: &RoomId) {
        let room_id = room_id.clone();
        let client = self.client.clone();

        tokio::spawn(async move {
            let room_id = &room_id;
            let client = &client;

            let backoff = ExponentialBackoff {
                max_elapsed_time: None,
                ..Default::default()
            };

            retry(backoff, || async move {
                client
                    .join_room_by_id(room_id)
                    .await
                    .map_err(RetryError::Transient)
            })
            .await
            .unwrap();
        });
    }
}

#[async_trait]
impl EventHandler for HowlerEventHandler {
    async fn on_room_message(&self, _: Room, event: &SyncMessageEvent<MessageEventContent>) {
        if event.sender == self.user_id {
            return;
        }

        let _ = self.tx_event_id.send(event.event_id.clone()).await.unwrap();
    }

    async fn on_state_member(
        &self,
        room: Room,
        event: &matrix_sdk::ruma::events::SyncStateEvent<MemberEventContent>,
    ) {
        if event.state_key != self.user_id {
            return;
        }

        if let (MembershipState::Join, Room::Joined(room)) = (&event.content.membership, room) {
            self.tx_join
                .send((self.user_id.clone(), room.room_id().clone()))
                .await
                .unwrap();

            self.send_token(room);
        }
    }

    async fn on_stripped_state_member(
        &self,
        room: Room,
        event: &StrippedStateEvent<MemberEventContent>,
        _: Option<MemberEventContent>,
    ) {
        if event.state_key != self.user_id {
            return;
        }

        if let (MembershipState::Invite, Room::Invited(room)) = (&event.content.membership, &room) {
            let sender = event.sender.clone();
            let room_id = room.room_id();

            self.tx_invite
                .send(Invite {
                    sender: sender.clone(),
                    recipient: self.user_id.clone(),
                    room: room_id.clone(),
                })
                .await
                .unwrap();

            self.join_room(room_id);
        }
    }

    async fn on_custom_event(&self, room: Room, event: &CustomEvent<'_>) {
        match event {
            CustomEvent::Message(content) => {
                if content.sender != self.user_id {
                    self.tx_event_id
                        .send(content.event_id.clone())
                        .await
                        .unwrap();
                }
            }
            CustomEvent::State(content) => {
                if content.content.event_type == "com.famedly.howler.template" {
                    if let (Some(plain), Some(html)) = (
                        content.content.data.get("plain"),
                        content.content.data.get("html"),
                    ) {
                        let room_id = room.room_id().clone();
                        let event_id = content.event_id.clone();
                        let template = Template {
                            plain: plain.to_string(),
                            html: html.to_string(),
                        };

                        self.tx_template
                            .send((event_id, room_id, template))
                            .await
                            .unwrap();
                    }
                }
            }
            _ => {}
        }
    }
}
