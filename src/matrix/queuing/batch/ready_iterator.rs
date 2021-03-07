//! Here we define the data structure that is responsible for queuing and batching incoming alerts and requeuing alerts that couldn't be sent or failed to federate.
use std::{cmp::Reverse, collections::BinaryHeap, sync::Arc};

use matrix_sdk::ruma::RoomId;
use rand::prelude::*;
use tokio::time::Instant;

use super::BatchQueue;
use crate::{matrix::queuing::batch::WaitingRoomEntries, rendered_alert::RenderedAlert};

/// The idea of this Iterator is that we can iterate over the ready to send rendered alerts, remove the ones we are able to send in one message from the [BinaryHeap] and the iterator manages
/// * requeuing the remaining alerts into [super::BatchQueue::waiting] if they're not older than one [super::BatchQueue::batch_interval]
/// * if some but not all alerts could be sent in one message and the remaining alerts are older than one [super::BatchQueue::batch_interval] return the remaining alerts in a future call of [ReadyIterator::next]
#[derive(Debug)]
pub struct ReadyIterator<'a> {
    queue: &'a mut BatchQueue,
    /// index and original size of the `IndexMap` entry returned by the last `next` call
    last_viewed: Option<LastViewed>,
}

impl<'a> ReadyIterator<'a> {
    pub fn new(queue: &'a mut BatchQueue) -> Self {
        Self {
            last_viewed: None,
            queue,
        }
    }

    pub fn next(&mut self) -> Option<(&mut Arc<RoomId>, ReadyAlerts<'_>)> {
        let finished = match self.last_viewed {
            Some(last) => match last.next(self.queue) {
                Some(next) => {
                    self.last_viewed = Some(next);
                    false
                }
                None => true,
            },
            None => {
                self.last_viewed = LastViewed::new(self.queue);
                self.last_viewed.is_none()
            }
        };

        if finished {
            None
        } else {
            let ready_index = self.last_viewed.unwrap().index;

            self.queue
                .ready
                .get_index_mut(ready_index)
                .map(|(k, v)| (k, ReadyAlerts(v)))
        }
    }
}

impl<'a> Drop for ReadyIterator<'a> {
    /// on drop call [`Self::next()`] to ensure that the alerts returned from the last [`ReadyIterator::next`] call get requeued into [`BatchQueue::waiting`] if necessary
    fn drop(&mut self) {
        self.next();
    }
}

#[derive(Debug, Clone, Copy)]
struct LastViewed {
    /// index to [`BatchQueue::ready`] entry returned by the last [`ReadyIterator::next`] call
    index: usize,
    /// length of the [BinaryHeap] before being returned by the last [`ReadyIterator::next`] call
    original_length: usize,
}

impl LastViewed {
    pub fn new(batch_queue: &mut BatchQueue) -> Option<Self> {
        if batch_queue.ready.is_empty() {
            None
        } else {
            // randomly choose one of the remaining IndexMap entries so we don't introduce a bias and rooms are served fairly
            batch_queue
                .ready
                .swap_indices(0, thread_rng().gen_range(0..batch_queue.ready.len()));

            Some(Self {
                index: 0,
                original_length: batch_queue.ready[0].len(),
            })
        }
    }

    pub fn next(&self, batch_queue: &mut BatchQueue) -> Option<Self> {
        /// what to do with entry returned by the last [`ReadyIterator::next`] call
        enum Action {
            /// remove entries from [`batch_queue.ready`]
            Remove,
            /// requeue entries into [`batch_queue.waiting`]
            Requeue,
            /// keep entries in [`batch_queue.ready`]
            Keep {
                /// if the caller of [`ReadyIterator::next`] took some of the returned [`RenderedAlert`] entries but there are still some left which are too old to requeue into [`batch_queue.waiting`] we will return the remaining entries in a upcoming call to [`ReadyIterator::next`]
                reiterate: bool,
            },
        }

        let BatchQueue {
            waiting: batch_entries,
            ready,
            timeout_queue,
            batch_interval,
        } = batch_queue;

        let action = {
            let entries = &ready[self.index];

            // the caller of the iterator couldn't send any of the [`RenderedAlert`] entries, we keep them in [`BatchQueue::ready`] but we won't return them during the lifetime of this Iterator
            if self.original_length == entries.len() {
                Action::Keep { reiterate: false }
            } else {
                // the caller of the iterator was able to send some or all of the [`RenderedAlert`] entries
                match entries.peek() {
                    // the caller couldn't send all [`RenderedAlert`] entries and the remaining ones are young enough to be requeued into [`BatchQueue::waiting`]
                    Some(Reverse(entry))
                        if entry.metadata.arrival + *batch_interval > Instant::now() =>
                    {
                        Action::Requeue
                    }
                    // the caller of the iterator couldn't send all [`RenderedAlert`] entries but the remaining ones already lived at least for the duration of [`BatchQueue::batch_interval`]
                    // we will return them in a future [`ReadyIterator::next`] call
                    Some(_) => Action::Keep { reiterate: true },
                    // the caller of the iterator was able to send all [`RenderedAlert`] entries
                    // remove the empty [BinaryHeap] from the [IndexMap]
                    None => Action::Remove,
                }
            }
        };

        let next = match action {
            // remove the entries from [`BatchQueue::ready`]
            Action::Remove => {
                ready.swap_remove_index(self.index);
                self.index
            }
            // requeue the entries into [`BatchQueue::waiting`]
            Action::Requeue => {
                let (room, entries) = ready.swap_remove_index(self.index).unwrap();
                let oldest = entries.peek().unwrap().0.metadata.arrival;
                let delay_queue_key =
                    timeout_queue.insert_at(room.clone(), oldest + *batch_interval);

                batch_entries.insert(
                    room,
                    WaitingRoomEntries {
                        timeout_queue_key: delay_queue_key,
                        entries: entries.into_vec(),
                        oldest,
                    },
                );

                self.index
            }
            // keep the entries in [`BatchQueue::ready`] but don't return them in a call to [`ReadyIterator::next`] for the remaining iterator lifetime
            Action::Keep { reiterate: false } => self.index + 1,
            // keep the entries in [`BatchQueue::ready`] and return them in a future call to [`ReadyIterator::next`] of this iterator
            Action::Keep { reiterate: true } => self.index,
        };

        if ready.len() >= next {
            None
        } else {
            // randomly choose one of the remaining IndexMap entries so we don't introduce a bias and rooms are served fairly
            ready.swap_indices(next, thread_rng().gen_range(next..ready.len()));

            Some(LastViewed {
                index: next,
                original_length: ready[next].len(),
            })
        }
    }
}

/// Wrapper around [BinaryHeap] so ensuring the user of [ReadyIterator] can only modify the underlying [BinaryHeap] by popping elements from the heap
pub struct ReadyAlerts<'a>(&'a mut BinaryHeap<Reverse<RenderedAlert>>);

impl<'a> ReadyAlerts<'a> {
    pub fn peek(&self) -> Option<&Reverse<RenderedAlert>> {
        self.0.peek()
    }

    pub fn pop(&mut self) -> Option<Reverse<RenderedAlert>> {
        self.0.pop()
    }
}
