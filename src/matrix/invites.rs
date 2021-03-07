//! This module handles invitation of bots into a room.
//! After a first bot joined a room, howler tries to automatically invite all other missing bots.

/* TODO: this is ugly overcomplicated and should get replaced by something simpler and more robust.
 * Once knocking is available in the SDK, just let every bot knock on every room it hasn't got any invitations yet
 * and let an already joined bot invite it
 *
 * The motivation for this implementation was to try avoid running into invite ratelimits, so a lot of rooms can get created at once without running into huge ratelimits.
 */

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use backoff::{backoff::Backoff, ExponentialBackoff};
use futures::{stream::FuturesUnordered, StreamExt};
use matrix_sdk::{
    ruma::{
        api::client::error::ErrorKind,
        identifiers::{RoomId, UserId},
    },
    Client,
};
use rand::{prelude::StdRng, Rng, SeedableRng};
use tokio::{sync::mpsc, task::JoinHandle, time::Instant};

use super::channels::{InviteChannel, JoinChannel, OnceCellMPSC};

#[derive(Debug, Clone)]
pub struct Invite {
    pub room: RoomId,
    pub sender: UserId,
    pub recipient: UserId,
}

/// Bot that can do invitations. Keeps track of ratelimits and backoffs
struct InviteBot {
    client: Client,
    bot_id: UserId,

    backoff: Option<(Instant, ExponentialBackoff)>,

    user_ratelimits: HashMap<UserId, Instant>,
    room_ratelimits: HashMap<RoomId, Instant>,

    last_failed_invite: Option<(UserId, RoomId)>,
}

impl InviteBot {
    pub async fn new(client: Client) -> Self {
        let bot_id = client.user_id().await.unwrap();

        Self {
            client,
            bot_id,

            backoff: None,

            user_ratelimits: HashMap::new(),
            room_ratelimits: HashMap::new(),

            last_failed_invite: None,
        }
    }

    /// Check if bot is ready. If bot was in backoff or was ratelimited returns the [Instant] at which bot becomes ready again. This [Instant] may live in the past.
    ///
    /// # Arguments
    ///
    /// * `user_id` - UserId of bot to invite
    ///
    /// * `room_id` - RoomId of room to invite [user_id] into
    pub fn is_ready_in(&mut self, user_id: &UserId, room_id: &RoomId) -> Option<Instant> {
        let mut ready_in = self.backoff.as_ref().map(|(instant, _)| *instant);
        ready_in = std::cmp::max(self.user_ratelimits.get(user_id).cloned(), ready_in);
        ready_in = std::cmp::max(self.room_ratelimits.get(room_id).cloned(), ready_in);

        if let Some(ready_in) = ready_in {
            if ready_in > Instant::now() {
                self.user_ratelimits.remove(user_id);
                self.room_ratelimits.remove(room_id);

                return Some(ready_in);
            }
        }

        None
    }

    /// Get [UserId] of bot
    pub fn bot_id(&self) -> &UserId {
        &self.bot_id
    }

    /// Check if bot has joined room
    pub fn has_joined_room(&self, room_id: &RoomId) -> bool {
        self.client.get_joined_room(room_id).is_some()
    }

    /// Invite bot into room
    ///
    /// # Arguments
    ///
    /// * `target_room` - room to invite bot into
    ///
    /// * `user_id` - bot to invite
    pub async fn invite(mut self, target_room: RoomId, user_id: UserId) -> Self {
        use matrix_sdk::{
            ruma::api::{
                client::error::Error as ClientError,
                error::{FromHttpResponseError, ServerError},
            },
            Error::Http,
        };

        let joined = match self.client.get_joined_room(&target_room) {
            Some(joined) => joined,
            None => {
                self.last_failed_invite = Some((user_id, target_room));
                return self;
            }
        };

        match joined.invite_user_by_id(&user_id).await {
            Ok(_) => {
                let block_until = Instant::now() + Duration::from_secs(1);

                self.user_ratelimits.insert(user_id, block_until);
                self.room_ratelimits.insert(target_room, block_until);

                self.backoff.take();
            }
            Err(Http(matrix_sdk::HttpError::ClientApi(FromHttpResponseError::Http(
                ServerError::Known(ClientError {
                    kind: ErrorKind::Forbidden,
                    ..
                }),
            )))) if joined.get_member(&user_id).await.unwrap().is_some() => {}
            Err(Http(matrix_sdk::HttpError::ClientApi(FromHttpResponseError::Http(
                ServerError::Known(ClientError {
                    kind:
                        ErrorKind::LimitExceeded {
                            retry_after_ms: Some(retry_after),
                        },
                    ..
                }),
            )))) => {
                self.last_failed_invite = Some((user_id.clone(), target_room.clone()));
                let instant = Instant::now() + retry_after;
                self.user_ratelimits.insert(user_id, instant);
                self.room_ratelimits.insert(target_room, instant);
            }
            Err(_) => {
                self.last_failed_invite = Some((user_id, target_room));
                self.backoff();
            }
        }
        self
    }

    /// backoff bot
    fn backoff(&mut self) {
        if let Some((instant, backoff)) = &mut self.backoff {
            *instant = Instant::now() + backoff.next_backoff().unwrap();
        } else {
            let mut backoff = ExponentialBackoff {
                max_elapsed_time: None,
                ..Default::default()
            };

            let instant = Instant::now() + backoff.next_backoff().unwrap();
            self.backoff = Some((instant, backoff));
        }
    }

    /// take target room and userid of last failed invite
    pub fn take_last_failed_invite(&mut self) -> Option<(UserId, RoomId)> {
        self.last_failed_invite.take()
    }
}

pub struct InviteScheduler {
    bots: Vec<InviteBot>,
    bot_ids: HashSet<UserId>,

    active_bots: FuturesUnordered<JoinHandle<InviteBot>>,

    pending_invites: Vec<(RoomId, UserId)>,
    rng: StdRng,

    rx_invite: mpsc::Receiver<Invite>,
    rx_join: mpsc::Receiver<(UserId, RoomId)>,
}

impl InviteScheduler {
    /// Constructs [InviteScheduler] from slice of client bots.
    pub async fn new(clients: &[Client]) -> Self {
        let mut bots = Vec::new();
        let mut bot_ids = HashSet::new();
        let rng = rand::rngs::StdRng::from_rng(rand::rngs::OsRng::default()).unwrap();

        for client in clients {
            let bot = InviteBot::new(client.clone()).await;
            let bot_id = bot.bot_id().clone();

            bots.push(bot);
            bot_ids.insert(bot_id);
        }

        Self {
            bots,
            bot_ids,

            active_bots: FuturesUnordered::new(),

            pending_invites: Vec::new(),
            rng,

            rx_invite: InviteChannel::receiver().await.unwrap(),
            rx_join: JoinChannel::receiver().await.unwrap(),
        }
    }

    /// Try to invite still uninvited bot. If not all bots can get invited return [Instant] at which invites should be retried.
    fn try_invite(&mut self) -> Option<Instant> {
        let mut retry_in = None;
        let mut remaining_invites = Vec::new();
        let mut remaining_bots = Vec::new();

        while !self.pending_invites.is_empty() {
            let (target_room, user_id) = self
                .pending_invites
                .swap_remove(self.rng.gen_range(0..self.pending_invites.len()));

            std::mem::swap(&mut remaining_bots, &mut self.bots);
            let mut invite_ready_in = None;
            let mut chosen_bot = None;

            while !remaining_bots.is_empty() {
                let mut bot =
                    remaining_bots.swap_remove(self.rng.gen_range(0..remaining_bots.len()));

                if !bot.has_joined_room(&target_room) {
                    self.bots.push(bot);
                    continue;
                }

                if let ready_in @ Some(_) = bot.is_ready_in(&user_id, &target_room) {
                    if ready_in < invite_ready_in {
                        invite_ready_in = ready_in;
                    }

                    self.bots.push(bot);
                    continue;
                }

                chosen_bot = Some(bot);
                break;
            }

            if let Some(bot) = chosen_bot {
                let future = tokio::spawn(async move { bot.invite(target_room, user_id).await });

                self.active_bots.push(future);
            } else {
                remaining_invites.push((target_room, user_id));
                if invite_ready_in < retry_in {
                    retry_in = invite_ready_in;
                }
            }
        }

        retry_in
    }

    /// Invite main loop
    pub async fn run(mut self) {
        let mut retry_in: Option<Instant> = None;

        loop {
            tokio::select! {
                Some(Ok(bot)) = self.active_bots.next(), if !self.active_bots.is_empty() => {
                    let mut bot = bot;

                    if let Some((user_id, room_id)) = bot.take_last_failed_invite() {
                        self.pending_invites.push((room_id, user_id));
                    }

                    self.bots.push(bot);
                }

                Some(invite) = self.rx_invite.recv() => {
                    if !self.bot_ids.contains(&invite.sender) {
                        for bot_id in &self.bot_ids {
                            if *bot_id != invite.recipient {
                                tracing::warn!("invite {} to room {}", bot_id, invite.room);

                                self.pending_invites.push((invite.room.clone(), bot_id.clone()));
                            }
                        }
                    }
                }

                _ = tokio::time::sleep_until(retry_in.unwrap_or_else(Instant::now)), if retry_in.is_some() => { }

                _ = tokio::time::sleep(Duration::from_secs(1)), if !self.pending_invites.is_empty() => {}

                Some(_) = self.rx_join.recv() => {}
            };

            retry_in = self.try_invite();
        }
    }
}
