//! Here we define the data structure that keeps track of backoffs and
//! ratelimits for each bot

use std::{
	collections::{HashMap, HashSet},
	sync::Arc,
	task::Poll,
	time::Duration,
};

use anyhow::{Context, Result};
use backoff::{backoff::Backoff, ExponentialBackoff, ExponentialBackoffBuilder};
use futures::Stream;
use matrix_sdk::ruma::{RoomId, UserId};
use prometheus::HistogramVec;
use serde::Deserialize;
use serde_with::{serde_as, DurationSeconds};
use tokio::{pin, time::Instant};
use tokio_util::time::DelayQueue;

use crate::{pairmap::PairMap, pairset::PairSet, settings::Settings};

#[derive(Debug)]
/// prometheus meters for recording the total backoff time for each bot and room
struct BackoffMetrics {
	/// meter for total federation backoff time
	federation_backoff: HistogramVec,
	/// meter for total (non federation) backoff time
	backoff: HistogramVec,
	/// meter for total ratelimit time
	ratelimit: HistogramVec,
}

use prometheus::{exponential_buckets, histogram_opts, register_histogram_vec};

impl BackoffMetrics {
	/// construct prometheus meters
	pub fn new() -> Result<Self, prometheus::Error> {
		let ratelimit = register_histogram_vec!(
			histogram_opts!(
				"ratelimit_seconds",
				"time bot has been ratelimited in room",
				exponential_buckets(1., 2., 12)?
			)
			.subsystem("bot")
			.namespace("howler"),
			&["room", "bot"]
		)?;

		let backoff = register_histogram_vec!(
			histogram_opts!(
				"backoff_seconds",
				"time bot has been backoffed because of server errors",
				exponential_buckets(0.5, 1.5, 12)?
			)
			.subsystem("bot")
			.namespace("howler"),
			&["room", "bot"]
		)?;

		let federation_backoff = register_histogram_vec!(
			histogram_opts!(
				"federationbackoff_seconds",
				"time bot has been backoffed because of server errors",
				exponential_buckets(0.5, 1.5, 12)?
			)
			.subsystem("bot")
			.namespace("howler"),
			&["bot"]
		)?;

		Ok(Self { ratelimit, federation_backoff, backoff })
	}

	/// record (non federation) backoff time
	pub fn record_backoff(&self, bot: &Arc<UserId>, room: &Arc<RoomId>, duration: Duration) {
		self.backoff
			.with_label_values(&[room.as_str(), bot.as_str()])
			.observe(duration.as_secs_f64());
	}

	/// record ratelimit time
	pub fn record_ratelimit(&self, bot: &Arc<UserId>, room: &Arc<RoomId>, duration: Duration) {
		self.ratelimit
			.with_label_values(&[room.as_str(), bot.as_str()])
			.observe(duration.as_secs_f64());
	}

	/// record federation backoff time
	pub fn record_federation_backoff(&self, bot: &Arc<UserId>, duration: Duration) {
		self.federation_backoff.with_label_values(&[bot.as_str()]).observe(duration.as_secs_f64());
	}
}

/// Here we store the federation backoffs
/// Apart from federation backoffs, backoffs and ratelimits are room specific
#[derive(Debug)]
pub struct Backoffs {
	/// prometheus metrics for recording total ratelimit and backoff times
	metrics: BackoffMetrics,

	/// used as starting interval for federation backoff
	federation_backoff_settings: FederationBackoffSettings,

	/// per room backoff
	backoff: PairMap<Arc<UserId>, Arc<RoomId>, ExponentialBackoff>,

	/// backoff if
	federation_backoff: HashMap<Arc<UserId>, ExponentialBackoff>,

	/// HashSet of active backoff intervals
	in_backoff: PairSet<Arc<UserId>, Arc<RoomId>>,
	/// HashSet of active federation backoff intervals
	in_federation_backoff: HashSet<Arc<UserId>>,
	/// HashSet of bots who are currently ratelimited for a specific room
	in_ratelimit: PairSet<Arc<UserId>, Arc<RoomId>>,

	/// this notifies us if a backoff or ratelimit interval is coming to it's
	/// end
	timeout_queue: DelayQueue<Timeout>,
}

impl Backoffs {
	/// construct `Backoffs`
	pub fn new() -> Result<Self> {
		Ok(Self {
			metrics: BackoffMetrics::new().context("failed to register prometheus meters")?,
			federation_backoff_settings: Settings::global().federation.backoff,

			timeout_queue: DelayQueue::new(),
			backoff: PairMap::new(),
			federation_backoff: HashMap::new(),
			in_backoff: PairSet::new(),
			in_federation_backoff: HashSet::new(),
			in_ratelimit: PairSet::new(),
		})
	}

	/// backoff `bot` after a message failed to send to `room`
	pub fn backoff(&mut self, bot: Arc<UserId>, room: Arc<RoomId>, from: Instant) {
		let backoff_duration = if let Some(backoff) = self.backoff.get_mut(&bot, &room) {
			#[allow(clippy::expect_used)]
			backoff.next_backoff().expect("ExponentialBackoff configured with infinite backoffs")
		} else {
			let mut backoff =
				ExponentialBackoffBuilder::default().with_max_elapsed_time(None).build();

			#[allow(clippy::expect_used)]
			let backoff_duration = backoff
				.next_backoff()
				.expect("ExponentialBackoff configured with infinite backoffs");
			self.backoff.insert(bot.clone(), room.clone(), backoff);

			backoff_duration
		};

		self.metrics.record_backoff(&bot, &room, backoff_duration);

		tracing::info!("{bot} in {room} in backoff for {backoff_duration:#?}");

		self.in_backoff.insert(bot.clone(), room.clone());
		self.timeout_queue.insert_at(Timeout::Backoff { bot, room }, from + backoff_duration);
	}

	/// stop backoff of `bot` for `room` after a message was successfully sent
	/// (but not necesarrily confirmed by other bots)
	pub fn stop_backoff(&mut self, bot: &Arc<UserId>, room: &Arc<RoomId>) {
		self.backoff.remove(bot, room);
	}

	/// ratelimit `bot` for `room` after server responded with
	/// `M_LIMIT_EXCEEDED` and also specified a timeout
	pub fn ratelimit(
		&mut self,
		bot: Arc<UserId>,
		room: Arc<RoomId>,
		from: Instant,
		duration: Duration,
	) {
		tracing::info!("{bot} in {room} ratelimited for {duration:#?}");
		self.metrics.record_ratelimit(&bot, &room, duration);
		self.in_ratelimit.insert(bot.clone(), room.clone());
		self.timeout_queue.insert_at(Timeout::Ratelimit { bot, room }, from + duration);
	}

	/// put `bot` into federation backoff after a message failed to confirm
	pub fn federation_backoff(&mut self, bot: Arc<UserId>, from: Instant) {
		if self.in_federation_backoff.contains(&bot) {
			return;
		}

		let backoff_duration = if let Some(backoff) = self.federation_backoff.get_mut(&bot) {
			#[allow(clippy::expect_used)]
			backoff.next_backoff().expect("ExponentialBackoff configured with infinite backoffs")
		} else {
			let mut backoff = self.federation_backoff_settings.build();
			#[allow(clippy::expect_used)]
			let backoff_duration = backoff
				.next_backoff()
				.expect("ExponentialBackoff configured with infinite backoffs");

			self.federation_backoff.insert(bot.clone(), backoff);

			backoff_duration
		};

		self.metrics.record_federation_backoff(&bot, backoff_duration);

		tracing::info!("{bot} in federation backoff for {backoff_duration:?}");

		self.in_federation_backoff.insert(bot.clone());

		self.timeout_queue.insert_at(Timeout::FederationBackoff { bot }, from + backoff_duration);
	}

	/// stop federation backoff for `bot` after message was confirmed in time
	pub fn stop_federation_backoff(&mut self, bot: &Arc<UserId>) {
		if !self.in_federation_backoff.contains(bot) {
			self.federation_backoff.remove(bot);
		}
	}

	/// check if `bot` can send messages to `room` or is blocked by a backoff or
	/// ratelimit
	pub fn is_ready(&self, bot: &Arc<UserId>, room: &Arc<RoomId>) -> bool {
		if self.in_federation_backoff.contains(bot) {
			return false;
		}

		if self.in_ratelimit.contains(bot, room) {
			return false;
		}

		if self.in_backoff.contains(bot, room) {
			return false;
		}

		true
	}
}

/// Backoff and ratelimit timeout events
#[derive(Debug, Clone)]
enum Timeout {
	/// federation backoff interval for `bot` elapsed
	FederationBackoff {
		/// bot who's federation backoff interval elapsed
		bot: Arc<UserId>,
	},
	/// backoff interval for `bot` in `room` elapsed
	Backoff {
		/// bot who's room backoff elapsed
		bot: Arc<UserId>,
		/// the specific room the bot was in backoff
		room: Arc<RoomId>,
	},
	/// ratelimit interval for `bot` in `room` elapsed
	Ratelimit {
		/// bot who was ratelimited in `room`
		bot: Arc<UserId>,
		/// the specific room the bot was ratelimited in
		room: Arc<RoomId>,
	},
}

/// A Stream that notifies us if a ratelimit or backoff interval has finished so
/// we can try sending alerts ready for sending
impl Stream for Backoffs {
	type Item = ();

	fn poll_next(
		mut self: std::pin::Pin<&mut Self>,
		cx: &mut std::task::Context<'_>,
	) -> Poll<Option<Self::Item>> {
		let Backoffs { in_federation_backoff, in_backoff, in_ratelimit, timeout_queue, .. } =
			&mut *self.as_mut();
		pin!(timeout_queue);

		if let Poll::Ready(Some(expired)) = timeout_queue.poll_next(cx) {
			let expired = expired.into_inner();
			match expired {
				Timeout::FederationBackoff { bot } => {
					in_federation_backoff.remove(&bot);
				}
				Timeout::Backoff { bot, room } => {
					in_backoff.remove(&bot, &room);
				}
				Timeout::Ratelimit { bot, room } => {
					in_ratelimit.remove(&bot, &room);
				}
			}

			return Poll::Ready(Some(()));
		}

		Poll::Pending
	}
}

#[serde_as]
#[derive(Debug, Clone, Copy, Deserialize)]
/// settings used for backoff
pub struct FederationBackoffSettings {
	/// the mean duration (it's randomized) of the first backoff interval
	#[serde_as(as = "DurationSeconds<f64>")]
	pub starting_interval: Duration,
	/// the maximum duration of a single backoff interval
	#[serde_as(as = "DurationSeconds<f64>")]
	pub max_interval: Duration,
	/// the factor by which to increase each next backoff intervall until
	/// `max_duration` is reached
	pub multiplier: f64,
}

impl FederationBackoffSettings {
	/// construct an `ExponentialBackoff` by the configured settings
	pub fn build(&self) -> ExponentialBackoff {
		ExponentialBackoffBuilder::default()
			.with_max_elapsed_time(None)
			.with_initial_interval(self.starting_interval)
			.with_max_interval(self.max_interval)
			.with_multiplier(self.multiplier)
			.with_randomization_factor(0_f64)
			.build()
	}
}
