use std::{
    collections::{HashMap, LinkedList},
    pin::Pin,
    task::Poll,
    time::Duration,
};

use backoff::{backoff::Backoff, ExponentialBackoff};
use futures::{stream::FuturesUnordered, Stream};
use matrix_sdk::{
    ruma::{identifiers::RoomId, UserId},
    Client,
};
use prometheus::HistogramVec;
use tokio::{pin, task::JoinHandle, time::Instant};

use super::{
    bot::Bot,
    queuing::{BatchedMessage, UnconfirmedMessage},
};
use crate::{matrix::sending::batch_entries::BatchEntries, settings::Settings};

/// The [Dispatcher] struct is used to send messages via one of the bots.
/// Too choose a bot [Dispatcher] stores a [LinkedList] of idle bots. When [Dispatcher] tries to send a message, it goes through this list and picks the first bot that isn't in backoff for the specified room. If a bot is finished sending a message it get's added again to the end of the list if it's a backup bot, else it get's inserted to the front of the list.
/// After a message was sent, an [UnconfirmedMessage] can be retrieved via the [Stream] interface.

struct DispatcherMetrics {
    pub available_bots: HistogramVec,
}

impl DispatcherMetrics {
    pub fn new(bot_count: usize) -> Self {
        let available_bots = prometheus::register_histogram_vec!(
            prometheus::histogram_opts!(
                "bots_available",
                "number of available bots for sending attempt",
                prometheus::linear_buckets(0., 1., bot_count + 1).unwrap()
            )
            .namespace("howler")
            .subsystem("bot"),
            &["room"]
        )
        .unwrap();

        Self { available_bots }
    }
}

type SendResult = Result<UnconfirmedMessage, (RoomId, BatchEntries)>;

/// Sends messages/files. Returns send results via the [Stream] interface.
pub struct Dispatcher {
    metrics: DispatcherMetrics,

    /// List of inactive bots
    idle_bots: LinkedList<Bot>,

    /// federation backoff data is stored here and not in `Bot` as at the time the federation confirmation for a sent message times out, the [Bot] that sent the message might be currently moved into a [JoinHandle] inside [Self::active_bots]
    federation_backoff: HashMap<UserId, (Instant, ExponentialBackoff)>,

    /// List of bots currently trying to send a message/file.
    active_bots: FuturesUnordered<JoinHandle<(Bot, SendResult)>>,
}

impl Dispatcher {
    /// Constructs [Dispatcher] from Vec of bots. The main bot must be the first bot.
    ///
    /// # Arguments
    ///
    /// * `clients` - The bots used to send messages/files. The first [Client] in clients is the main bot, all other clients are backup bots.
    pub async fn new(clients: Vec<Client>) -> Self {
        let mut bots = LinkedList::new();

        let mut is_backup = false;

        for client in clients.into_iter() {
            let bot = Bot::new(client, is_backup).await;
            bots.push_back(bot);
            is_backup = true;
        }

        let metrics = DispatcherMetrics::new(bots.len());

        Self {
            idle_bots: bots,
            metrics,

            federation_backoff: HashMap::new(),

            active_bots: FuturesUnordered::new(),
        }
    }

    /// Backoff bot when a sent message couldn't be confirmed by any other bot.
    ///
    /// # Arguments
    ///
    /// * `bot_id` - The [UserId] of the bot to backoff
    pub fn federation_backoff(&mut self, bot_id: UserId) {
        // A bot can be unavailable (moved into a JoinHandle inside [Self::active_bots]) at the time the federation confirmation times out.
        // Therefore we store the federation backoff data in the `Dispatcher` struct and not directly in [Bot].

        match self.federation_backoff.get_mut(&bot_id) {
            Some((instant, backoff)) => {
                let duration = backoff.next_backoff().unwrap();

                *instant = Instant::now() + duration;
            }
            None => {
                let mut backoff = ExponentialBackoff {
                    max_elapsed_time: None,
                    initial_interval: Settings::global().batch_interval,
                    max_interval: Duration::from_secs(900),
                    ..Default::default()
                };

                let duration = backoff.next_backoff().unwrap();
                self.federation_backoff
                    .insert(bot_id, (Instant::now() + duration, backoff))
                    .unwrap();
            }
        }
    }

    /// Reset the federation backoff if existent for a bot after a sent message was confirmed in a specified time interval by another bot.
    ///
    /// # Arguments
    ///
    /// * `bot_id` - The [UserId] of the bot who's message got confirmed in time.
    pub fn reset_federation_backoff(&mut self, bot_id: &UserId) {
        self.federation_backoff.remove(bot_id);
    }

    /// Try sending a message.
    ///
    /// # Arguments
    ///
    /// * `message` - The [BatchedMessage] to send.
    pub fn send(&mut self, message: BatchedMessage) -> Result<(), (RoomId, BatchEntries)> {
        let upload = match message.content {
            super::queuing::BatchedContent::File(_) => true,
            super::queuing::BatchedContent::Message(_) => false,
        };

        let choose_bot = |bot: &Bot, room_id: &RoomId| {
            let bot_id = bot.user_id();
            if let Some((instant, _)) = &self.federation_backoff.get(bot_id) {
                if Instant::now() < *instant {
                    return false;
                }
            }

            bot.is_ready(room_id, upload)
        };

        let bot = self
            .idle_bots
            .drain_filter(|bot| choose_bot(bot, &message.target_room))
            .next();

        match bot {
            Some(bot) => {
                let bot_count = 1 + self
                    .idle_bots
                    .iter()
                    .filter(|bot| choose_bot(*bot, &message.target_room))
                    .count();
                self.metrics
                    .available_bots
                    .with_label_values(&[message.target_room.as_str()])
                    .observe(bot_count as f64);

                self.active_bots
                    .push(tokio::spawn(async move { bot.send(message).await }));

                Ok(())
            }
            None => {
                self.metrics
                    .available_bots
                    .with_label_values(&[message.target_room.as_str()])
                    .observe(0f64);
                tracing::warn!("no bot available for room {}", message.target_room);

                Err((message.target_room, message.entries))
            }
        }
    }
}

impl Stream for Dispatcher {
    type Item = Result<UnconfirmedMessage, (RoomId, BatchEntries)>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let Dispatcher {
            idle_bots: bots,
            active_bots: futures,
            ..
        } = &mut *self.as_mut();

        pin!(futures);

        match futures.poll_next(cx) {
            Poll::Ready(Some(result)) => {
                let (bot, send_result) = result.unwrap();

                if bot.is_backup() {
                    bots.push_back(bot);
                } else {
                    bots.push_front(bot);
                }

                Poll::Ready(Some(send_result))
            }
            _ => Poll::Pending,
        }
    }
}
