use std::{cmp::Reverse, collections::BinaryHeap, io::Cursor, time::Duration};

use matrix_sdk::ruma::{events::AnyMessageEventContent, RoomId};
use prometheus::Histogram;
use tar::Header;
use tokio_util::time::{delay_queue, DelayQueue};

use crate::matrix::sending::batch_entries::{
    BatchEntries, BatchEntry, FileBatchEntry, MessageBatchEntry, MessageBatchEntryContent,
};

#[derive(Debug)]
struct RoomMetrics {
    queued_alerts: Histogram,
}

impl RoomMetrics {
    pub fn new(room_id: &RoomId) -> Self {
        use prometheus::{exponential_buckets, histogram_opts, labels, register_histogram};

        let queued_alerts = register_histogram!(histogram_opts!(
            "queued_alerts",
            "number of queued alerts",
            exponential_buckets(1., 2., 12).unwrap(),
            labels! { "bot".to_string() => room_id.to_string() }
        )
        .namespace("howler")
        .subsystem("queue"))
        .unwrap();

        Self { queued_alerts }
    }

    pub fn record_queued_alerts(&self, count: usize) {
        self.queued_alerts.observe(count as f64);
    }
}

#[derive(Debug)]
/// queues messages and files for a single room
pub struct RoomQueue {
    room_id: RoomId,
    batch_duration: Duration,
    messages: BinaryHeap<Reverse<MessageBatchEntry>>,
    files: BinaryHeap<Reverse<FileBatchEntry>>,
    key: Option<delay_queue::Key>,
    metrics: RoomMetrics,
}

impl RoomQueue {
    /// Creates an empty [RoomQueue]
    ///
    /// # Arguments
    ///
    /// * `room_id` - the RoomId of the target room, used for prometheus metrics
    ///
    /// * `batch_duration` - the time the queue waits for other alerts to arrive, before batching them. For this [BatchEntryMetadata::arrival][crate::matrix::sending::batch_entries::BatchEntryMetadata::arrival] of the oldest queued [BatchEntry] is used.
    pub fn new(room_id: RoomId, batch_duration: Duration) -> Self {
        let metrics = RoomMetrics::new(&room_id);

        metrics.record_queued_alerts(0);

        Self {
            room_id,
            batch_duration,
            messages: BinaryHeap::new(),
            files: BinaryHeap::new(),
            key: None,
            metrics,
        }
    }

    /// updates batch timeout. must be called everytime entries are removed or added to the queue
    fn update_batch_timeout(&mut self, delay_queue: &mut DelayQueue<RoomId>) {
        let instant = std::cmp::min(
            Reverse(
                self.messages
                    .peek()
                    .map(|Reverse(entry)| Reverse(entry.metadata.arrival)),
            ),
            Reverse(
                self.files
                    .peek()
                    .map(|Reverse(entry)| Reverse(entry.metadata.arrival)),
            ),
        )
        .0
        .map(|Reverse(instant)| instant);

        if let Some(key) = &self.key {
            match instant {
                Some(instant) => {
                    delay_queue.reset_at(key, instant + self.batch_duration);
                }
                None => {
                    delay_queue.remove(key);
                    self.key = None;
                }
            }
        } else if let Some(instant) = instant {
            self.key =
                Some(delay_queue.insert_at(self.room_id.clone(), instant + self.batch_duration));
        }
    }

    /// batches queued [FileBatchEntry]s together and removes them from the queue
    fn batch_files(&mut self) -> (BatchEntries, BatchedContent) {
        let mut file = Cursor::new(Vec::new());
        let mut entries = Vec::new();

        {
            let mut builder = tar::Builder::new(&mut file);

            let mut index = 0;
            let log_10 = |mut i: usize| {
                let mut len = 0;
                while i > 0 {
                    i /= 10;
                    len += 1;
                }

                len
            };
            let len: usize = log_10(self.files.len());

            while let Some(Reverse(entry)) = self.files.pop() {
                let root = format!("{0:01$}", index, len);

                let path = format!("{}__{}", root, entry.content.filename.as_str());

                let mut header = Header::new_gnu();
                header.set_path(path.as_str()).unwrap();
                header.set_size(entry.content.content.len() as u64);
                header.set_mode(0o660);
                header.set_cksum();
                builder
                    .append(&header, Cursor::new(&entry.content.content))
                    .unwrap();

                index += 1;

                entries.push(entry);
            }

            builder.finish().unwrap();
        }

        self.metrics
            .record_queued_alerts(self.messages.len() + self.files.len());

        (
            BatchEntries::FileEntries(entries),
            BatchedContent::File(file.into_inner()),
        )
    }

    /// batches queued [MessageBatchEntry]s together and removes them from queue
    fn batch_messages(&mut self) -> (BatchEntries, BatchedContent) {
        let mut batched_message = MessageBatchEntryContent::default();

        let mut entries = Vec::new();

        while let Some(Reverse(entry)) = self.messages.pop() {
            if batched_message.append(&entry.content).is_err() {
                self.messages.push(Reverse(entry));
                break;
            } else {
                entries.push(entry);
            }
        }

        (
            BatchEntries::MessageEntries(entries),
            BatchedContent::Message(batched_message.into()),
        )
    }

    /// Batches [BatchEntry]s together and removes them from the [RoomQueue]. Updates the next batch timeout.
    /// If both [MessageBatchEntry]s and [FileBatchEntry]s are available only one of them get's batched together.
    /// If queue was empty None is returned.
    ///
    /// # Arguments
    ///
    /// * `delay_queue` - [DelayQueue] to register/update the next batch timeout for this room
    pub fn batch(
        &mut self,
        delay_queue: &mut DelayQueue<RoomId>,
    ) -> Option<(BatchEntries, BatchedContent)> {
        self.key = None;

        let (entries, content) = match (self.messages.peek(), self.files.peek()) {
            (None, None) => {
                return None;
            }
            (Some(_), None) => self.batch_messages(),
            (Some(Reverse(message_entry)), Some(Reverse(file_entry)))
                if message_entry.metadata.arrival <= file_entry.metadata.arrival =>
            {
                self.batch_messages()
            }
            _ => self.batch_files(),
        };

        self.update_batch_timeout(delay_queue);

        Some((entries, content))
    }

    /// Adds [BatchEntry]s to the queue and updates the batch interval timeout for this room.
    ///
    /// # Arguments
    ///
    /// * `delay_queue` - The queue that tracks the batch timeout for each room
    pub fn queue<IntoIter>(&mut self, entries: IntoIter, delay_queue: &mut DelayQueue<RoomId>)
    where
        IntoIter: IntoIterator<Item = BatchEntry>,
    {
        for entry in entries.into_iter() {
            match entry {
                BatchEntry::FileEntry(entry) => {
                    self.files.push(Reverse(entry));
                }
                BatchEntry::MessageEntry(entry) => {
                    self.messages.push(Reverse(entry));
                }
            }
        }

        self.metrics
            .record_queued_alerts(self.messages.len() + self.files.len());

        self.update_batch_timeout(delay_queue)
    }
}

#[derive(Debug, Clone)]
/// Result of a batching operation.
pub enum BatchedContent {
    File(Vec<u8>),
    Message(AnyMessageEventContent),
}
