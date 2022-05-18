//! Here we define the data structure that is responsible for queuing and
//! batching incoming alerts and requeuing alerts that couldn't be sent or
//! failed to federate.
use std::{cmp::Reverse, collections::BinaryHeap};

use matrix_sdk::ruma::OwnedRoomId;
use rand::prelude::*;
use tokio::time::Instant;

use super::BatchQueue;
use crate::{matrix::queuing::batch::WaitingRoomEntries, rendered_alert::RenderedAlert};

/// The idea of this Iterator is that we can iterate over the ready to send
/// rendered alerts, remove the ones we are able to send in one message from the
/// [BinaryHeap] and the iterator manages
/// * requeuing the remaining alerts into [super::BatchQueue::waiting] if
///   they're not older than one [super::BatchQueue::batch_interval]
/// * if some but not all alerts could be sent in one message and the remaining
///   alerts are older than one [super::BatchQueue::batch_interval] return the
///   remaining alerts in a future call of [ReadyIterator::next]
#[derive(Debug)]
pub struct ReadyIterator<'a> {
	/// the queue that gave us the iterator
	queue: &'a mut BatchQueue,
	/// index and original size of the `IndexMap` entry returned by the last
	/// `next` call
	last_viewed: Option<LastViewed>,
}

impl<'a> ReadyIterator<'a> {
	/// construct a new iterator
	pub fn new(queue: &'a mut BatchQueue) -> Self {
		Self { last_viewed: None, queue }
	}

	/// get ready alerts for a room
	/// if at least some but not all alerts returned from a previous next call
	/// have been removed from the returned [ReadyAlerts] struct, they will come
	/// up in a later call to [Self::next] at least some but not all alerts
	/// being removed from the returned [ReadyAlerts] struct is interpreted as a
	/// bot was found who could send messages to that room but not all ready
	/// alerts could be batched into a single message if no alert was removed
	/// from the returned [ReadyAlerts] struct there was no bot available that
	/// could send a message to that room so the alerts won't be returned in a
	/// future next call of this iterator
	pub fn next(&mut self) -> Option<(&mut OwnedRoomId, ReadyAlerts<'_>)> {
		let next = match self.last_viewed {
			Some(last) => last.next(self.queue),
			None => LastViewed::first(self.queue),
		};

		match next {
			Some(next) => {
				let ready_index = next.index;
				self.last_viewed = Some(next);
				self.queue.ready.get_index_mut(ready_index).map(|(k, v)| (k, ReadyAlerts(v)))
			}
			None => None,
		}
	}
}

impl<'a> Drop for ReadyIterator<'a> {
	/// on drop call [`Self::next()`] to ensure that the alerts returned from
	/// the last [`ReadyIterator::next`] call get requeued into
	/// [`BatchQueue::waiting`] if necessary
	fn drop(&mut self) {
		self.next();
	}
}

/// struct used to store which [BatchQueue::ready] entry was returned by the
/// last next call and how many alerts were originally stored at the time the
/// BatchEntries we're returned
#[derive(Debug, Clone, Copy)]
struct LastViewed {
	/// index to [`BatchQueue::ready`] entry returned by the last
	/// [`ReadyIterator::next`] call
	index: usize,
	/// length of the [BinaryHeap] before being returned by the last
	/// [`ReadyIterator::next`] call
	original_length: usize,
}

impl LastViewed {
	/// first [BatchQueue::ready] entry to be returned by next
	fn first(batch_queue: &mut BatchQueue) -> Option<Self> {
		if batch_queue.ready.is_empty() {
			None
		} else {
			// randomly choose one of the remaining IndexMap entries so we don't introduce a
			// bias and rooms are served fairly
			batch_queue.ready.swap_indices(0, thread_rng().gen_range(0..batch_queue.ready.len()));

			Some(Self { index: 0, original_length: batch_queue.ready[0].len() })
		}
	}

	/// get next [BatchQueue::ready] entry and requeuee, remove or keep the
	/// previous [BatchQueue::ready] entry
	fn next(&self, batch_queue: &mut BatchQueue) -> Option<Self> {
		/// what to do with entry returned by the last [`ReadyIterator::next`]
		/// call
		enum Action {
			/// remove entries from [`batch_queue.ready`]
			Remove,
			/// requeue entries into [`batch_queue.waiting`]
			Requeue,
			/// keep entries in [`batch_queue.ready`]
			Keep {
				/// if the caller of [`ReadyIterator::next`] took some of the
				/// returned [`RenderedAlert`] entries but there are still some
				/// left which are too old to requeue into
				/// [`batch_queue.waiting`] we will return the remaining entries
				/// in a upcoming call to [`ReadyIterator::next`]
				reiterate: bool,
			},
		}

		let BatchQueue { waiting: batch_entries, ready, timeout_queue, batch_interval } =
			batch_queue;

		let action = {
			let entries = &ready[self.index];

			// the caller of the iterator couldn't send any of the [`RenderedAlert`]
			// entries, we keep them in [`BatchQueue::ready`] but we won't return them
			// during the lifetime of this Iterator
			if self.original_length == entries.len() {
				Action::Keep { reiterate: false }
			} else {
				// the caller of the iterator was able to send some or all of the
				// [`RenderedAlert`] entries
				match entries.peek() {
					// the caller couldn't send all [`RenderedAlert`] entries and the remaining ones
					// are young enough to be requeued into [`BatchQueue::waiting`]
					Some(Reverse(entry))
						if entry.metadata.arrival + *batch_interval > Instant::now() =>
					{
						Action::Requeue
					}
					// the caller of the iterator couldn't send all [`RenderedAlert`] entries but
					// the remaining ones already lived at least for the duration of
					// [`BatchQueue::batch_interval`] we will return them in a future
					// [`ReadyIterator::next`] call
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
				#[allow(clippy::expect_used)]
				let (room, entries) = ready.swap_remove_index(self.index).expect("invalid index");
				#[allow(clippy::expect_used)]
				let oldest = entries
					.peek()
					.expect("only non empty BinaryHeaps are requeued")
					.0
					.metadata
					.arrival;
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
			// keep the entries in [`BatchQueue::ready`] but don't return them in a call to
			// [`ReadyIterator::next`] for the remaining iterator lifetime
			Action::Keep { reiterate: false } => self.index + 1,
			// keep the entries in [`BatchQueue::ready`] and return them in a future call to
			// [`ReadyIterator::next`] of this iterator
			Action::Keep { reiterate: true } => self.index,
		};

		if ready.len() >= next {
			None
		} else {
			// randomly choose one of the remaining IndexMap entries so we don't introduce a
			// bias and rooms are served fairly
			ready.swap_indices(next, thread_rng().gen_range(next..ready.len()));

			Some(LastViewed { index: next, original_length: ready[next].len() })
		}
	}
}

/// Wrapper around [BinaryHeap] so ensuring the user of [ReadyIterator] can only
/// modify the underlying [BinaryHeap] by popping elements from the heap
pub struct ReadyAlerts<'a>(&'a mut BinaryHeap<Reverse<RenderedAlert>>);

impl<'a> ReadyAlerts<'a> {
	/// returns the oldest alert in the binary heap, or `None` if empty
	pub fn peek(&self) -> Option<&Reverse<RenderedAlert>> {
		self.0.peek()
	}

	/// removes the oldest alert in the binary heap and returns it, or `None` if
	/// empty
	pub fn pop(&mut self) -> Option<Reverse<RenderedAlert>> {
		self.0.pop()
	}
}
