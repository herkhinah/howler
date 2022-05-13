//! Here we define the data structure for queuing bots in idle mode. Backup bots
//! get queued to the end of the queue after becoming idle, the main bot get's
//! queued to the front when becoming idle.
use std::{
	collections::{linked_list::CursorMut, LinkedList},
	sync::Arc,
};

use hashbrown::HashMap;
use matrix_sdk::ruma::UserId;
use tokio::sync::mpsc;

use super::bot::{Bot, BotChannelMessage};

/// A entry of the bot queue. Stores the [UserId] and [channel][mpsc::Sender] of
/// the queued bot.
#[derive(Debug)]
pub struct BotQueueEntry {
	/// the user id of the bot
	pub user_id: Arc<UserId>,
	/// the channel to communicate witht the bot
	pub channel: mpsc::Sender<BotChannelMessage>,
	/// true if it's a backup bot
	backup: bool,
}

impl BotQueueEntry {
	/// Constructs a `BotQueueEntry` for usage inside `BotQueue` from a `Bot`
	/// struct
	pub fn new(bot: &Bot) -> Self {
		Self { user_id: bot.bot_id(), channel: bot.get_channel(), backup: bot.is_backup() }
	}
}

#[derive(Debug)]
/// stores busy bots and a queue of idle bots
pub struct BotQueue {
	/// linked list of idle bots
	/// bots are always removed from the front
	/// backup bots are inserted at the end
	/// the main bot is inserted to the front
	idle: LinkedList<BotQueueEntry>,
	/// map of busy bots
	busy: HashMap<Arc<UserId>, BotQueueEntry>,
}

impl BotQueue {
	/// constructs a `BotQueue` from array of `Bot`s
	pub fn new(bots: &[Bot]) -> Self {
		let mut idle = LinkedList::new();

		for bot in bots {
			let entry = BotQueueEntry::new(bot);
			if entry.backup {
				idle.push_back(entry);
			} else {
				idle.push_front(entry);
			}
		}

		Self { idle, busy: HashMap::new() }
	}

	/// queues an idle bot, backup bots get queued to the end the main bot to
	/// the front
	pub fn queue(&mut self, user_id: &Arc<UserId>) {
		if let Some(entry) = self.busy.remove(user_id) {
			if entry.backup {
				self.idle.push_back(entry);
			} else {
				self.idle.push_front(entry);
			}
		}
	}

	/// returns a cursor pointing to the first idle bot
	pub fn cursor_mut(&mut self) -> Cursor<'_> {
		Cursor { idle: self.idle.cursor_front_mut(), busy: &mut self.busy }
	}
}

/// A Cursor structure for iterating through the bots in idle mode.

#[derive(Debug)]
pub struct Cursor<'a> {
	/// mutable reference to the map of busy bots
	busy: &'a mut HashMap<Arc<UserId>, BotQueueEntry>,
	/// cursor over the idle bots
	idle: CursorMut<'a, BotQueueEntry>,
}

impl<'a> Cursor<'a> {
	/// gives us a reference to the bot the cursor is currently pointing at
	pub fn current(&mut self) -> Option<&'_ mut BotQueueEntry> {
		self.idle.current()
	}

	/// removes the bot from the idle queue if the cursor hasn't reached the end
	/// and the passed function `f` returns true
	pub fn remove_if<F>(&mut self, f: F) -> Option<&'_ mut BotQueueEntry>
	where
		F: Fn(&BotQueueEntry) -> bool,
	{
		if let Some(entry) = self.current() {
			if f(entry) {
				#[allow(clippy::expect_used)]
				let entry = self
					.idle
					.remove_current()
					.expect("we've checked the existence in the outer if let Some(..) match");
				#[allow(clippy::expect_used)]
				return Some(self.busy.try_insert(entry.user_id.clone(), entry).expect(
					"invariant that each bot can only appear in either busy or ready violated",
				));
			}
		}

		None
	}

	/// moves the cursor to the next element
	pub fn move_next(&mut self) {
		self.idle.move_next();
	}
}
