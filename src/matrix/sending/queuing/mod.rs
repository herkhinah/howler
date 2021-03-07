//! This module implements the two main queues used by howler.
//! Alerts or other arbitrary messages get queued as [BatchEntry][super::batch_entries::BatchEntry]s into a central [batch::BatchQueue]. This [batch::BatchQueue] stores a [RoomQueue] for each target room.
//! Alerts that have been sent, but not yet confirmed by another bot get queued into [UnconfirmedMessageQueue]. If a message couldn't be confirmed by another bot in the given time interval, the [UnconfirmedMessage] can be retrieved via the [futures::stream::Stream] interface.

mod batch;
mod room;
mod unconfirmed;

pub use batch::{BatchQueue, BatchedMessage};
pub use room::{BatchedContent, RoomQueue};
pub use unconfirmed::{UnconfirmedMessage, UnconfirmedMessageQueue};
