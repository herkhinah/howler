use std::{collections::HashMap, pin::Pin, task::Poll, time::Duration};

use futures::Stream;
use matrix_sdk::ruma::identifiers::RoomId;
use tokio::pin;
use tokio_util::time::DelayQueue;

use super::room::{BatchedContent, RoomQueue};
use crate::matrix::sending::batch_entries::{BatchEntries, BatchEntry};

/// Incoming alerts a queued into the BatchQueue. [BatchQueue] stores a [RoomQueue] for each target room.
/// If a [BatchEntry] get's queued into a [RoomQueue], the [RoomQueue] registers a timeout, after which [BatchQueue] calls the [RoomQueue] to batch as much [BatchEntry]s together as it can and updates the timeout.
/// The batched messages/files are returned via the [Stream] implementation of [BatchQueue]
pub struct BatchQueue {
    room_queues: HashMap<RoomId, RoomQueue>,

    timeout_queue: DelayQueue<RoomId>,
    batch_duration: Duration,
}

impl BatchQueue {
    /// Creates an empty [BatchQueue]
    ///
    /// # Arguments
    ///
    /// * `batch_duration` - The duration the queue waits after the oldest `BatchEntry::metadata().arrival` queued.
    pub fn new(batch_duration: Duration) -> Self {
        Self {
            room_queues: HashMap::new(),
            timeout_queue: DelayQueue::new(),
            batch_duration,
        }
    }

    /// Queues [BatchEntry]s
    ///
    /// # Arguments
    ///
    /// * `room_id` - The target room of `entries`
    ///
    /// * `entries` - [BatchEntry]s to queue
    pub fn queue<IntoIter>(&mut self, room_id: RoomId, entries: IntoIter)
    where
        IntoIter: IntoIterator<Item = BatchEntry>,
    {
        let queue = match self.room_queues.get_mut(&room_id) {
            Some(queue) => queue,
            None => {
                let queue = RoomQueue::new(room_id.clone(), self.batch_duration);
                self.room_queues.try_insert(room_id, queue).unwrap()
            }
        };

        queue.queue(entries, &mut self.timeout_queue);
    }

    /// Batches together [BatchEntry]s
    /// called from [Stream::poll_next]
    fn batch(&mut self, target_room: RoomId) -> Option<BatchedMessage> {
        if let Some(queue) = self.room_queues.get_mut(&target_room) {
            if let Some((entries, content)) = queue.batch(&mut self.timeout_queue) {
                return Some(BatchedMessage {
                    content,
                    entries,
                    target_room,
                });
            }
        }

        None
    }
}

impl Stream for BatchQueue {
    type Item = BatchedMessage;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let timeout_queue = &mut self.as_mut().timeout_queue;
        pin!(timeout_queue);

        match timeout_queue.poll_next(cx) {
            Poll::Ready(Some(Ok(expired))) => {
                let target_room = expired.into_inner();
                let message = self.batch(target_room).unwrap();
                Poll::Ready(Some(message))
            }
            Poll::Ready(Some(Err(err))) => {
                panic!("BatchQueue internal error: {:?}", err);
            }
            _ => Poll::Pending,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BatchedMessage {
    pub content: BatchedContent,
    pub entries: BatchEntries,
    pub target_room: RoomId,
}
