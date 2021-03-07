//! Here we define the data structure that keeps track of backoffs and ratelimits for each bot

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    task::Poll,
    time::Duration,
};

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
struct BackoffMetrics {
    federation_backoff: HistogramVec,
    backoff: HistogramVec,
    ratelimit: HistogramVec,
}

use prometheus::{exponential_buckets, histogram_opts, register_histogram_vec};

impl BackoffMetrics {
    pub fn new() -> Self {
        let ratelimit = register_histogram_vec!(
            histogram_opts!(
                "ratelimit_seconds",
                "time bot has been ratelimited in room",
                exponential_buckets(1., 2., 12).unwrap()
            )
            .subsystem("bot")
            .namespace("howler"),
            &["room", "bot"]
        )
        .unwrap();

        let backoff = register_histogram_vec!(
            histogram_opts!(
                "backoff_seconds",
                "time bot has been backoffed because of server errors",
                exponential_buckets(0.5, 1.5, 12).unwrap()
            )
            .subsystem("bot")
            .namespace("howler"),
            &["room", "bot"]
        )
        .unwrap();

        let federation_backoff = register_histogram_vec!(
            histogram_opts!(
                "federationbackoff_seconds",
                "time bot has been backoffed because of server errors",
                exponential_buckets(0.5, 1.5, 12).unwrap()
            )
            .subsystem("bot")
            .namespace("howler"),
            &["bot"]
        )
        .unwrap();

        Self {
            ratelimit,
            federation_backoff,
            backoff,
        }
    }

    pub fn record_backoff(&self, bot: &Arc<UserId>, room: &Arc<RoomId>, duration: Duration) {
        self.backoff
            .with_label_values(&[room.as_str(), bot.as_str()])
            .observe(duration.as_secs_f64());
    }

    pub fn record_ratelimit(&self, bot: &Arc<UserId>, room: &Arc<RoomId>, duration: Duration) {
        self.ratelimit
            .with_label_values(&[room.as_str(), bot.as_str()])
            .observe(duration.as_secs_f64());
    }

    pub fn record_federation_backoff(&self, bot: &Arc<UserId>, duration: Duration) {
        self.federation_backoff
            .with_label_values(&[bot.as_str()])
            .observe(duration.as_secs_f64());
    }
}

/// Here we store the federation backoffs
/// Apart from federation backoffs, backoffs and ratelimits are room specific
#[derive(Debug)]
pub struct Backoffs {
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

    /// this notifies us if a backoff or ratelimit interval is coming to it's end
    timeout_queue: DelayQueue<Timeout>,
}

impl Backoffs {
    pub fn new() -> Self {
        Self {
            metrics: BackoffMetrics::new(),
            federation_backoff_settings: Settings::global().federation.backoff,

            timeout_queue: DelayQueue::new(),
            backoff: PairMap::new(),
            federation_backoff: HashMap::new(),
            in_backoff: PairSet::new(),
            in_federation_backoff: HashSet::new(),
            in_ratelimit: PairSet::new(),
        }
    }

    pub fn backoff(&mut self, bot: Arc<UserId>, room: Arc<RoomId>, from: Instant) {
        let backoff_duration = if let Some(backoff) = self.backoff.get_mut(&bot, &room) {
            backoff.next_backoff().unwrap()
        } else {
            let mut backoff = ExponentialBackoffBuilder::default()
                .with_max_elapsed_time(None)
                .build();

            let backoff_duration = backoff.next_backoff().unwrap();
            self.backoff.insert(bot.clone(), room.clone(), backoff);

            backoff_duration
        };

        self.metrics.record_backoff(&bot, &room, backoff_duration);

        tracing::info!("{bot} in {room} in backoff for {backoff_duration:#?}");

        self.in_backoff.insert(bot.clone(), room.clone());
        self.timeout_queue
            .insert_at(Timeout::Backoff { bot, room }, from + backoff_duration);
    }

    pub fn stop_backoff(&mut self, bot: &Arc<UserId>, room: &Arc<RoomId>) {
        self.backoff.remove(bot, room);
    }

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
        self.timeout_queue
            .insert_at(Timeout::Ratelimit { bot, room }, from + duration);
    }

    pub fn federation_backoff(&mut self, bot: Arc<UserId>, from: Instant) {
        if self.in_federation_backoff.contains(&bot) {
            return;
        }

        let backoff_duration = if let Some(backoff) = self.federation_backoff.get_mut(&bot) {
            backoff.next_backoff().unwrap()
        } else {
            let mut backoff = self.federation_backoff_settings.build();
            let backoff_duration = backoff.next_backoff().unwrap();

            self.federation_backoff.insert(bot.clone(), backoff);

            backoff_duration
        };

        self.metrics
            .record_federation_backoff(&bot, backoff_duration);

        tracing::info!("{bot} in federation backoff for {backoff_duration:?}");

        self.in_federation_backoff.insert(bot.clone());

        self.timeout_queue
            .insert_at(Timeout::FederationBackoff { bot }, from + backoff_duration);
    }

    pub fn stop_federation_backoff(&mut self, bot: &Arc<UserId>) {
        if !self.in_federation_backoff.contains(bot) {
            self.federation_backoff.remove(bot);
        }
    }

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
    FederationBackoff { bot: Arc<UserId> },
    Backoff { bot: Arc<UserId>, room: Arc<RoomId> },
    Ratelimit { bot: Arc<UserId>, room: Arc<RoomId> },
}

/// A Stream that notifies us if a ratelimit or backoff interval has finished so we can try sending alerts ready for sending
impl Stream for Backoffs {
    type Item = ();

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let Backoffs {
            in_federation_backoff,
            in_backoff,
            in_ratelimit,
            timeout_queue,
            ..
        } = &mut *self.as_mut();
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
pub struct FederationBackoffSettings {
    #[serde_as(as = "DurationSeconds<f64>")]
    pub starting_interval: Duration,
    #[serde_as(as = "DurationSeconds<f64>")]
    pub max_interval: Duration,
    pub multiplier: f64,
}

impl FederationBackoffSettings {
    pub fn build(&self) -> ExponentialBackoff {
        ExponentialBackoffBuilder::default()
            .with_max_elapsed_time(None)
            .with_initial_interval(self.starting_interval)
            .with_max_interval(self.max_interval)
            .with_multiplier(self.multiplier)
            .with_randomization_factor(0f64)
            .build()
    }
}
