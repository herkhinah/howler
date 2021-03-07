//! Here we define the data structure that is responsible for queuing and batching incoming alerts and requeuing alerts that couldn't be sent or failed to federate.
use std::{cmp::Reverse, collections::BinaryHeap, pin::Pin, sync::Arc, task::Poll, time::Duration};

use futures::Stream;
use hashbrown::{hash_map::Entry, HashMap};
use indexmap::IndexMap;
use matrix_sdk::ruma::RoomId;
use tokio::{pin, time::Instant};
use tokio_util::time::{delay_queue, DelayQueue};

use self::ready_iterator::ReadyIterator;
use crate::rendered_alert::RenderedAlert;

pub mod ready_iterator;

/// The rendered alerts of a single room waiting for the batch duration timeout starting from the oldest queued alert
#[derive(Debug)]
struct WaitingRoomEntries {
    /// the key for the [tokio_util::time::delay_queue]
    timeout_queue_key: delay_queue::Key,

    /// the rendered alerts that want to be batched together
    entries: Vec<Reverse<RenderedAlert>>,

    /// the age of the oldest queued alert
    oldest: Instant,
}

/// Incoming alerts get queued into the BatchQueue
///
/// We have two places where we queue alerts.
/// There is [Self::ready] where alerts that are ready to send get queued.
/// And there is [Self::waiting]. If a freshly rendered alert arrives and there are no alerts for the target room ready to send they get inserted here.
/// The queue waits [Self::batch_interval] of time for other alerts for the same target room to arrive and then moves them into [Self::ready].
///
/// If alerts that failed to send or failed to federate get requeued they get directly inserted into [Self::ready] and the [Self::waiting] value for that room get emptied without waiting for the batch duration timeout.
#[derive(Debug)]
pub struct BatchQueue {
    /// alerts waiting one batch interval beginning from the arrival of the oldest entry for each room before getting moved into [Self::ready]
    waiting: HashMap<Arc<RoomId>, WaitingRoomEntries>,

    /// alerts ready to send
    ready: IndexMap<Arc<RoomId>, BinaryHeap<Reverse<RenderedAlert>>>,

    /// This notifies us if a [Self::waiting] entry has finished it's batch interval
    timeout_queue: DelayQueue<Arc<RoomId>>,
    batch_interval: Duration,
}

impl BatchQueue {
    /// Constructs a [BatchQueue]
    ///
    /// # Arguments
    ///
    /// * `batch_interval` - the time interval the queue waits between the first alert arriving for a room to batch
    pub fn new(batch_interval: Duration) -> Self {
        Self {
            waiting: HashMap::new(),
            timeout_queue: DelayQueue::new(),
            ready: IndexMap::new(),
            batch_interval,
        }
    }

    /// queues new incoming alerts from the [crate::alert_renderer::AlertRenderer]
    ///
    /// # Arguments
    ///
    /// * `room_id` - the target room of the rendered alert
    ///
    /// * `entry` - the renderer alert to queue
    pub fn queue(&mut self, room_id: Arc<RoomId>, mut entry: RenderedAlert) {
        // if we already have alerts ready for sending for the specified target room don't wait for the batch timeout
        if let Some(entries) = self.ready.get_mut(&room_id) {
            entries.push(Reverse(entry));
            return;
        }

        // if there are no alerts ready for wait for the batch timeout
        match self.waiting.entry(room_id.clone()) {
            Entry::Occupied(mut occupied) => {
                let WaitingRoomEntries {
                    timeout_queue_key: delay_queue_key,
                    entries,
                    oldest,
                } = occupied.get_mut();

                if oldest > &mut entry.metadata.arrival {
                    *oldest = entry.metadata.arrival;
                    self.timeout_queue
                        .reset_at(delay_queue_key, *oldest + self.batch_interval);
                }

                entries.push(Reverse(entry));
            }
            Entry::Vacant(vacant) => {
                let oldest = entry.metadata.arrival;
                let delay_queue_key = self
                    .timeout_queue
                    .insert_at(room_id, oldest + self.batch_interval);

                vacant.insert(WaitingRoomEntries {
                    timeout_queue_key: delay_queue_key,
                    entries: vec![Reverse(entry)],
                    oldest,
                });
            }
        }
    }

    /// requeue alerts that have already been tried to send
    ///
    /// # Arguments
    ///
    /// * `room_id` - the target room
    ///
    /// * `entries` - the alerts to requeue
    pub fn requeue(&mut self, room_id: Arc<RoomId>, entries: Vec<Reverse<RenderedAlert>>) {
        if let Some(heap) = self.ready.get_mut(&room_id) {
            heap.extend(entries.into_iter());
        } else {
            let mut entries = BinaryHeap::from(entries);

            if let Some(WaitingRoomEntries {
                timeout_queue_key: delay_queue_key,
                entries: waiting,
                ..
            }) = self.waiting.remove(&room_id)
            {
                self.timeout_queue.remove(&delay_queue_key);
                entries.extend(waiting.into_iter());
            }

            self.ready.insert(room_id, entries);
        }
    }

    /// get an iterator over the ready to send entries. the order of the rooms is random so we don't prefer certain rooms because of their hash value
    pub fn ready_iter(&mut self) -> ReadyIterator<'_> {
        ReadyIterator::new(self)
    }

    /// returns true if there are rendered alerts ready to send
    pub(crate) fn has_ready(&self) -> bool {
        !self.ready.is_empty()
    }
}

/// This Stream is responsible for putting alerts from the batch queue into the ready hashmap and notifies the caller that there are new alerts ready for sending
impl Stream for BatchQueue {
    type Item = ();

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let timeout_queue = &mut self.as_mut().timeout_queue;
        pin!(timeout_queue);

        match timeout_queue.poll_next(cx) {
            Poll::Ready(Some(expired)) => {
                let room = expired.into_inner();
                let WaitingRoomEntries { entries, .. } = self
                    .waiting
                    .remove(&room)
                    .expect("there must be entries queued");
                self.ready.insert(room, BinaryHeap::from(entries));
                Poll::Ready(Some(()))
            }
            _ => Poll::Pending,
        }
    }
}
