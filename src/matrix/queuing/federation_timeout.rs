//! Here we define the data structure that stores sent alerts until federation got confirmed by another bot or a timelimit is hit.
//! If federation can't get confirmed in time the alerts have to get requeued into [super::batch::BatchQueue] and the bot that sent that message has to be put into federation backoff via [Backoffs::federation_backoff][super::backoffs::Backoffs::federation_backoff]. This is done in the main event loop [super::run].

use std::{cmp::Reverse, pin::Pin, sync::Arc, task::Poll, time::Duration};

use futures::Stream;
use hashbrown::{hash_map::Entry, HashMap};
use matrix_sdk::ruma::{EventId, RoomId, UserId};
use prometheus::{HistogramVec, IntCounterVec};
use tokio::time::Instant;
use tokio_util::time::{delay_queue, DelayQueue};

use crate::{rendered_alert::RenderedAlert, settings::Settings};

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

    /// record turnaround and total sending attempts after a message got confirmed
    pub fn record_turnaround(
        &self,
        room: &RoomId,
        alerts: impl IntoIterator<Item = Reverse<RenderedAlert>>,
    ) {
        let now = Instant::now();

        for Reverse(alert) in alerts.into_iter() {
            let metadata = alert.metadata;

            self.turnaround_confirmed
                .with_label_values(&[room.as_str()])
                .observe((now - metadata.arrival).as_secs_f64());
            self.turnaround_sent
                .with_label_values(&[room.as_str()])
                .observe((metadata.last_sending_attempt.unwrap() - metadata.arrival).as_secs_f64());
            self.total_attempts
                .with_label_values(&[room.as_str()])
                .observe(metadata.sending_attempts as f64);
        }
    }
}

#[derive(Debug, Clone)]
/// sent alerts waiting for federation confirmation
pub struct UnconfirmedMessage {
    /// [EventId] of sent message
    pub event_id: Box<EventId>,
    /// bot which sent the message
    pub bot_id: Arc<UserId>,
    /// target room of message
    pub target_room: Arc<RoomId>,
    /// rendered alerts which get requeued if federation isn't confirmed in time
    pub batch_entries: Vec<Reverse<RenderedAlert>>,
}

#[derive(Debug)]
/// UnconfirmedMessageQueue is used for queuing sent messages until they got confirmed by another bot or the message timed out.
/// The [RenderedAlert]s of messages that timed out without confirmation are received via the [Stream] interface.
pub struct FederationTimeoutQueue {
    required_confirmations: u64,
    metrics: Metrics,
    queue: DelayQueue<UnconfirmedMessage>,
    event_ids: HashMap<Box<EventId>, (u64, delay_queue::Key)>,
    confirmation_timeout: Duration,
}

impl FederationTimeoutQueue {
    /// Returns new empty [FederationTimeoutQueue]
    pub fn new() -> Self {
        Self {
            required_confirmations: Settings::global().federation.required_confirmations,
            metrics: Metrics::new(),
            queue: DelayQueue::new(),
            event_ids: HashMap::new(),
            confirmation_timeout: Settings::global().federation.timeout,
        }
    }

    /// Queues unconfirmed message
    ///
    /// # Arguments
    ///
    /// * `unconfirmed` - Unconfirmed message to queue
    pub fn queue(&mut self, unconfirmed: UnconfirmedMessage) {
        if self.required_confirmations == 0 {
            return;
        }

        let event_id = unconfirmed.event_id.clone();

        let key = self.queue.insert(unconfirmed, self.confirmation_timeout);
        self.event_ids
            .insert(event_id, (self.required_confirmations, key));
    }

    /// Tries confirming message. Returns [UserId] of bot of bot sending the message if the message wasn't confirmed yet and didn't timeout yet.
    ///
    /// # Arguments
    ///
    /// * `event_id` - EventId of the message to confirm. The caller must make sure, the EventId was sent by another bot
    pub fn confirm_message(&mut self, event_id: Box<EventId>) -> Option<Arc<UserId>> {
        if let Entry::Occupied(mut o) = self.event_ids.entry(event_id) {
            let (required_confirmations, _) = o.get_mut();

            *required_confirmations -= 1;

            if *required_confirmations != 0 {
                return None;
            }

            let (_, key) = o.remove();

            let UnconfirmedMessage {
                bot_id,
                target_room,
                batch_entries,
                ..
            } = self.queue.remove(&key).into_inner();
            self.metrics.record_turnaround(&target_room, batch_entries);

            return Some(bot_id);
        }

        None
    }
}

/// This Stream gives us the messages that have been waiting for federation confirmation but have timed out
impl Stream for FederationTimeoutQueue {
    type Item = (Arc<RoomId>, Arc<UserId>, Vec<Reverse<RenderedAlert>>);

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let queue = &mut self.as_mut().queue;
        tokio::pin!(queue);

        match queue.poll_next(cx) {
            Poll::Ready(Some(expired)) => {
                let unconfirmed = expired.into_inner();

                self.metrics
                    .confirmation_timeout
                    .with_label_values(&[
                        unconfirmed.target_room.as_str(),
                        unconfirmed.bot_id.as_str(),
                    ])
                    .inc_by(unconfirmed.batch_entries.len() as u64);

                self.event_ids.remove(&unconfirmed.event_id);
                Poll::Ready(Some((
                    unconfirmed.target_room,
                    unconfirmed.bot_id,
                    unconfirmed.batch_entries,
                )))
            }
            _ => Poll::Pending,
        }
    }
}
