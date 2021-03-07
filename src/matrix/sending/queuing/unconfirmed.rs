use std::{collections::HashMap, pin::Pin, task::Poll, time::Duration};

use futures::Stream;
use matrix_sdk::ruma::{EventId, RoomId, UserId};
use prometheus::{HistogramVec, IntCounterVec};
use tokio::time::Instant;
use tokio_util::time::{delay_queue, DelayQueue};

use crate::{
    matrix::sending::batch_entries::{BatchEntries, BatchEntry},
    settings::Settings,
};

#[derive(Debug, Clone)]
struct Metrics {
    turnaround_sent: HistogramVec,
    turnaround_confirmed: HistogramVec,
    total_attempts: HistogramVec,

    confirmation_timeout: IntCounterVec,
}

impl Metrics {
    pub fn new() -> Self {
        use prometheus::{
            exponential_buckets, histogram_opts, opts, register_histogram_vec,
            register_int_counter_vec,
        };

        let turnaround_sent = register_histogram_vec!(
            histogram_opts!(
                "alert_turnaround_sent",
                "turnaround time of alert from arrival to successfull sending",
                exponential_buckets(0.1, 1.5, 24).unwrap()
            )
            .namespace("howler"),
            &["room"]
        )
        .unwrap();

        let turnaround_confirmed = register_histogram_vec!(
            histogram_opts!(
                "alert_turnaround_confirmed",
                "turnaround time of alert from arrival to federation confirmation",
                exponential_buckets(0.1, 1.5, 24).unwrap()
            )
            .namespace("howler"),
            &["room"]
        )
        .unwrap();

        let total_attempts = register_histogram_vec!(
            histogram_opts!(
                "alert_sending_attempts_total",
                "number of attempts to send alert",
                exponential_buckets(1., 2., 8).unwrap()
            )
            .namespace("howler"),
            &["room"]
        )
        .unwrap();

        let confirmation_timeout = register_int_counter_vec!(
            opts!(
                "alert_confirmation_timeout",
                "number of sent alerts failed to confirm via federation"
            )
            .namespace("howler"),
            &["room", "bot"]
        )
        .unwrap();

        Self {
            turnaround_sent,
            turnaround_confirmed,
            total_attempts,
            confirmation_timeout,
        }
    }
}

#[derive(Debug, Clone)]
/// Unconfirmed message
pub struct UnconfirmedMessage {
    /// [EventId] of sent message
    pub event_id: EventId,
    /// bot which sent the message
    pub bot_id: UserId,
    /// target room of message
    pub target_room: RoomId,
    /// [BatchEntries] this message is composed of
    pub batch_entries: BatchEntries,
}

#[derive(Debug)]
/// UnconfirmedMessageQueue is used for queuing sent messages until they got confirmed by another bot or the message timed out.
/// The [BatchEntry]s of messages that timed out without confirmation are received via the [Stream] interface.
pub struct UnconfirmedMessageQueue {
    metrics: Metrics,
    queue: DelayQueue<UnconfirmedMessage>,
    event_ids: HashMap<EventId, delay_queue::Key>,
    confirmation_timeout: Duration,
}

impl UnconfirmedMessageQueue {
    /// Returns new empty [UnconfirmedMessageQueue]
    pub fn new() -> Self {
        Self {
            metrics: Metrics::new(),
            queue: DelayQueue::new(),
            event_ids: HashMap::new(),
            confirmation_timeout: Settings::global().message_timeout,
        }
    }

    /// Queues unconfirmed message
    ///
    /// # Arguments
    ///
    /// * `unconfirmed` - Unconfirmed message to queue
    pub fn queue(&mut self, unconfirmed: UnconfirmedMessage) {
        let event_id = unconfirmed.event_id.clone();

        let key = self.queue.insert(unconfirmed, self.confirmation_timeout);
        self.event_ids.insert(event_id, key);
    }

    /// Tries confirming message. Returns [UserId] of bot of bot sending the message if the message wasn't confirmed yet and didn't timeout yet.
    ///
    /// # Arguments
    ///
    /// * `event_id` - EventId of the message to confirm. The caller must make sure, the EventId was sent by another bot
    pub fn confirm_message(&mut self, event_id: &EventId) -> Option<UserId> {
        if let Some(key) = &self.event_ids.remove(event_id) {
            let now = Instant::now();

            let Metrics {
                turnaround_confirmed,
                turnaround_sent,
                total_attempts,
                ..
            } = &self.metrics;

            let message = self.queue.remove(key).into_inner();

            for entry in message.batch_entries.into_iter() {
                let metadata = match entry {
                    BatchEntry::FileEntry(entry) => entry.metadata,
                    BatchEntry::MessageEntry(entry) => entry.metadata,
                };

                turnaround_confirmed
                    .with_label_values(&[message.target_room.as_str()])
                    .observe((now - metadata.arrival).as_secs_f64());
                turnaround_sent
                    .with_label_values(&[message.target_room.as_str()])
                    .observe(
                        (metadata.last_sending_attempt.unwrap() - metadata.arrival).as_secs_f64(),
                    );
                total_attempts
                    .with_label_values(&[message.target_room.as_str()])
                    .observe(metadata.sending_attempts as f64);
            }

            return Some(message.bot_id);
        }

        None
    }
}

impl Stream for UnconfirmedMessageQueue {
    type Item = UnconfirmedMessage;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let queue = &mut self.as_mut().queue;
        tokio::pin!(queue);

        match queue.poll_next(cx) {
            Poll::Ready(Some(res)) => match res {
                Ok(expired) => {
                    let unconfirmed = expired.into_inner();

                    self.metrics
                        .confirmation_timeout
                        .with_label_values(&[
                            unconfirmed.target_room.as_str(),
                            unconfirmed.bot_id.as_str(),
                        ])
                        .inc_by(unconfirmed.batch_entries.len() as u64);

                    self.event_ids.remove(&unconfirmed.event_id);
                    Poll::Ready(Some(unconfirmed))
                }
                Err(err) => {
                    panic!("UnconfirmedMessageQueue internal error: {:?}", err);
                }
            },
            _ => Poll::Pending,
        }
    }
}
