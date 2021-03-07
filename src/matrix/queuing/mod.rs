//! Queues and batches incoming alerts, sends them to one of the bots for sending and waits for another bot to confirm federation.
//!
//! If a bot fails to send the message the renderered alerts get requeued but without having to wait for the batch duration timeout.
//! If a message got successfully sent, the rendered alerts get queued into a [FederationTimeoutQueue] to wait for another bot to confirm that the message got federated to at least another server.

pub mod backoffs;
mod batch;
mod federation_timeout;

use std::{cmp::Reverse, sync::Arc, time::Duration};

use federation_timeout::FederationTimeoutQueue;
pub use federation_timeout::UnconfirmedMessage;
use futures::StreamExt;
use matrix_sdk::ruma::{EventId, RoomId, UserId};
use tokio::{sync::mpsc, time::Instant};

use self::{backoffs::Backoffs, batch::BatchQueue};
use super::bot_queue::BotQueue;
use crate::{
    matrix::bot::{Bot, BotChannelMessage},
    rendered_alert::{RenderedAlert, RenderedAlertContent},
    settings::Settings,
};

/// this component receives send results and new incoming alerts via a channel
#[derive(Debug, Clone)]
pub enum QueueChannelMessage {
    /// a [bot][crate::matrix::bot::Bot] finished it's ending attempt
    SendResult(Result<UnconfirmedMessage, SendError>),
    /// the [AlertRenderer][crate::alert_renderer::AlertRenderer] has a rendered alert for us to queue
    QueueEntry {
        room: Arc<RoomId>,
        entry: RenderedAlert,
    },
    /// a [bot][crate::matrix::bot::Bot] has confirmed that a message not sent by themself has arrived
    ConfirmFederation(Box<EventId>),
}

#[derive(Debug, Clone)]
pub enum SendError {
    Backoff {
        room: Arc<RoomId>,
        bot: Arc<UserId>,
        entries: Vec<Reverse<RenderedAlert>>,
        from: Instant,
    },
    Ratelimit {
        room: Arc<RoomId>,
        bot: Arc<UserId>,
        entries: Vec<Reverse<RenderedAlert>>,
        from: Instant,
        duration: Duration,
    },
}

/// Main event loop of this module
pub fn run(
    mut rx: mpsc::Receiver<QueueChannelMessage>,
    bots: &[Bot],
) -> impl std::future::Future<Output = ()> {
    // successfully sent alerts get stored until another bot confirmed that the message got federated
    // if they didn't get confirmed in a specified time interval they get requeued
    let mut federation_timeout_queue = FederationTimeoutQueue::new();

    // batch_queue is responsible for queuing new incoming alerts until a given batch timeout
    // or if there are already alerts ready to send for the target room they will get sent with them
    let mut batch_queue = BatchQueue::new(Settings::global().batch_interval);

    // here we keep track of the federation backoffs, room backoffs and room ratelimits for each bot
    let mut backoffs = Backoffs::new();

    // this is the bot queue
    // if a bot has finished it's sending attempt get's requeued to the back if it's a backup bot or to the front if it's the main bot
    let mut bot_queue = BotQueue::new(bots);

    async move {
        loop {
            tokio::select!(
                // the federation of a message couldn't get confirmed by another bot in time
                // we requeue the messages and set a federation backoff
                Some((room, bot, entries)) = federation_timeout_queue.next() => {
                    backoffs.federation_backoff(bot, Instant::now());
                    batch_queue.requeue(room, entries);
                }

                Some(message) = rx.recv() => {
                    match message {
                        // a bot sent us the send result
                        QueueChannelMessage::SendResult(result) => {
                            match result {
                                // the message could be successfully sent
                                Ok(unconfirmed) => {
                                    // requeue the bot
                                    bot_queue.queue(&unconfirmed.bot_id);

                                    // stop running backoffs
                                    backoffs.stop_backoff(&unconfirmed.bot_id, &unconfirmed.target_room);

                                    // queue the unconfirmed message (an event_id, the alerts and the target room) into the federation timeout queue
                                    federation_timeout_queue.queue(unconfirmed);
                                }
                                // the message couldn't get sent and the server didn't respond with a ratelimit
                                Err(SendError::Backoff { room, bot, entries, from }) => {
                                    // requeue the bot
                                    bot_queue.queue(&bot);

                                    // register the backoff
                                    backoffs.backoff(bot, room.clone(), from);

                                    // requeue the alerts
                                    batch_queue.requeue(room, entries);
                                },
                                // the message couldn't get sent and the server responded with a ratelimit
                                Err(SendError::Ratelimit { room, bot, entries, from, duration }) => {
                                    // requeue the bot
                                    bot_queue.queue(&bot);

                                    // register the ratelimit
                                    backoffs.ratelimit(bot, room.clone(), from, duration);

                                    // requeue the alerts
                                    batch_queue.requeue(room, entries);
                                },
                            }
                        },
                        // the alert renderer sent us a rendered alert or error message
                        QueueChannelMessage::QueueEntry { room, entry } => { batch_queue.queue(room, entry) }

                        // a bot received a message not sent by themself
                        QueueChannelMessage::ConfirmFederation(event_id) => {
                            // if there was a message waiting for federation confirmation stop any running federation backoff for that bot
                            if let Some(bot) = federation_timeout_queue.confirm_message(event_id) {
                                backoffs.stop_federation_backoff(&bot);
                            }
                        },
                    }
                }

                // a bot finished it's federation or room backoff interval or finished it's ratelimit
                // if there are messages ready to send we can retry
                _ = backoffs.next() => { }

                // the batch queue had queued alerts that finished their batch timeout intervall
                _ = batch_queue.next() => { }
            );

            let mut bot_cursor = bot_queue.cursor_mut();

            'bot: while bot_cursor.current().is_some() && batch_queue.has_ready() {
                let mut ready_iter = batch_queue.ready_iter();

                while let Some((room, mut entries)) = ready_iter.next() {
                    if backoffs.is_ready(&bot_cursor.current().unwrap().user_id, room) {
                        // the batched message
                        let mut content = RenderedAlertContent::default();
                        // the rendered alerts that got batched together
                        let mut dispatched_entries = Vec::new();

                        while let Some(Reverse(entry)) = entries.peek() {
                            if content.append(&entry.content).is_err() {
                                // we couldn't append the alert to the batched message, because the resulting message would get to big
                                break;
                            } else {
                                dispatched_entries.push(entries.pop().unwrap());
                            }
                        }

                        let content = content.into();
                        let room = room.clone();

                        // we use this bot, so remove it from the queue
                        let tx_bot = bot_cursor.remove().unwrap().channel.clone();

                        // send the batched message together with the list of alerts (for the case they need to get requeued because the sending or federating failed) and target room to the selected bot
                        tx_bot
                            .send(BotChannelMessage::Send {
                                content,
                                entries: dispatched_entries,
                                room,
                            })
                            .await
                            .expect("tx_bot closed");

                        // we removed the bot, so continue the bot loop so we don't call bot_cursor.move_next()
                        continue 'bot;
                    }
                }

                // select the next bot
                bot_cursor.move_next();
            }
        }
    }
}
