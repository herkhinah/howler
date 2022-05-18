//! Here we define the data structure that stores sent alerts until federation
//! got confirmed by another bot or a timelimit is hit. If federation can't get
//! confirmed in time the alerts have to get requeued into
//! [super::batch::BatchQueue] and the bot that sent that message has to be put
//! into federation backoff via
//! [Backoffs::federation_backoff][super::backoffs::Backoffs::
//! federation_backoff]. This is done in the main event loop [super::run].

use std::{cmp::Reverse, pin::Pin, task::Poll, time::Duration};

use anyhow::{Context, Result};
use futures::Stream;
use hashbrown::{hash_map::Entry, HashMap};
use matrix_sdk::ruma::{OwnedEventId, OwnedRoomId, OwnedUserId, RoomId};
use prometheus::{HistogramVec, IntCounterVec};
use tokio::time::Instant;
use tokio_util::time::{delay_queue, DelayQueue};

use crate::{rendered_alert::RenderedAlert, settings::Settings};

/// metrics used by the FederationTimeoutQueue
#[derive(Debug, Clone)]
struct Metrics {
	/// turnaround time from alert being received to being sent
	/// only recorded after the alert was confirmed
	turnaround_sent: HistogramVec,
	/// turnaround time from alert being received and confirmed by another bot
	turnaround_confirmed: HistogramVec,
	/// number of sending attempts until alert got confirmed
	total_attempts: HistogramVec,

	/// number of alerts failed to confirm
	confirmation_timeout: IntCounterVec,
}

impl Metrics {
	/// construct metrics
	pub fn new() -> Result<Self, prometheus::Error> {
		use prometheus::{
			exponential_buckets, histogram_opts, opts, register_histogram_vec,
			register_int_counter_vec,
		};

		let turnaround_sent = register_histogram_vec!(
			histogram_opts!(
				"alert_turnaround_sent",
				"turnaround time of alert from arrival to successfull sending",
				exponential_buckets(0.1, 1.5, 24)?
			)
			.namespace("howler"),
			&["room"]
		)?;

		let turnaround_confirmed = register_histogram_vec!(
			histogram_opts!(
				"alert_turnaround_confirmed",
				"turnaround time of alert from arrival to federation confirmation",
				exponential_buckets(0.1, 1.5, 24)?
			)
			.namespace("howler"),
			&["room"]
		)?;

		let total_attempts = register_histogram_vec!(
			histogram_opts!(
				"alert_sending_attempts_total",
				"number of attempts to send alert",
				exponential_buckets(1., 2., 8)?
			)
			.namespace("howler"),
			&["room"]
		)?;

		let confirmation_timeout = register_int_counter_vec!(
			opts!(
				"alert_confirmation_timeout",
				"number of sent alerts failed to confirm via federation"
			)
			.namespace("howler"),
			&["room", "bot"]
		)?;

		Ok(Self { turnaround_sent, turnaround_confirmed, total_attempts, confirmation_timeout })
	}

	/// record turnaround and total sending attempts after a message got
	/// confirmed
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
			#[allow(clippy::expect_used)]
			self.turnaround_sent.with_label_values(&[room.as_str()]).observe(
				(metadata.last_sending_attempt.expect("last_sending_attempt missing")
					- metadata.arrival)
					.as_secs_f64(),
			);
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
	pub event_id: OwnedEventId,
	/// bot which sent the message
	pub bot_id: OwnedUserId,
	/// target room of message
	pub target_room: OwnedRoomId,
	/// rendered alerts which get requeued if federation isn't confirmed in time
	pub batch_entries: Vec<Reverse<RenderedAlert>>,
}

#[derive(Debug)]
/// UnconfirmedMessageQueue is used for queuing sent messages until they got
/// confirmed by another bot or the message timed out. The [RenderedAlert]s of
/// messages that timed out without confirmation are received via the [Stream]
/// interface.
pub struct FederationTimeoutQueue {
	/// how many other bots have to confirm each message
	required_confirmations: u64,
	/// prometheus meters
	metrics: Metrics,
	/// async stream that notifies us when a message has timed out without being
	/// confirmed
	queue: DelayQueue<UnconfirmedMessage>,
	/// a map from the event id of a message to the number of times a message
	/// still needs to be confirmed and the key used by the `DelayQueue` so we
	/// can remove the timeout after a message has been confirmed by enough bots
	event_ids: HashMap<OwnedEventId, (u64, delay_queue::Key)>,
	/// the duration during which a sent message can be confirmed before it's
	/// corresponding alerts are being requeued
	confirmation_timeout: Duration,
}

impl FederationTimeoutQueue {
	/// Returns new empty [FederationTimeoutQueue]
	pub fn new() -> Result<Self> {
		Ok(Self {
			required_confirmations: Settings::global().federation.required_confirmations,
			metrics: Metrics::new().context("failed to register prometheus meters")?,
			queue: DelayQueue::new(),
			event_ids: HashMap::new(),
			confirmation_timeout: Settings::global().federation.timeout,
		})
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
		self.event_ids.insert(event_id, (self.required_confirmations, key));
	}

	/// Tries confirming message. Returns [UserId] of bot of bot sending the
	/// message if the message wasn't confirmed yet and didn't timeout yet.
	///
	/// # Arguments
	///
	/// * `event_id` - EventId of the message to confirm. The caller must make
	///   sure, the EventId was sent by another bot
	pub fn confirm_message(&mut self, event_id: OwnedEventId) -> Option<OwnedUserId> {
		if let Entry::Occupied(mut o) = self.event_ids.entry(event_id) {
			let (required_confirmations, _) = o.get_mut();

			*required_confirmations -= 1;

			if *required_confirmations != 0 {
				return None;
			}

			let (_, key) = o.remove();

			let UnconfirmedMessage { bot_id, target_room, batch_entries, .. } =
				self.queue.remove(&key).into_inner();
			self.metrics.record_turnaround(&target_room, batch_entries);

			return Some(bot_id);
		}

		None
	}
}

/// This Stream gives us the messages that have been waiting for federation
/// confirmation but have timed out
impl Stream for FederationTimeoutQueue {
	type Item = (OwnedRoomId, OwnedUserId, Vec<Reverse<RenderedAlert>>);

	fn poll_next(
		mut self: Pin<&mut Self>,
		cx: &mut std::task::Context<'_>,
	) -> Poll<Option<Self::Item>> {
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
