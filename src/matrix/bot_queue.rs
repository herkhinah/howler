//! Here we define the data structure for queuing bots in idle mode. Backup bots get queued to the end of the queue after becoming idle, the main bot get's queued to the front when becoming idle.
use std::{
    collections::{linked_list::CursorMut, LinkedList},
    sync::Arc,
};

use hashbrown::HashMap;
use matrix_sdk::ruma::UserId;
use tokio::sync::mpsc;

use super::bot::{Bot, BotChannelMessage};

/// A entry of the bot queue. Stores the [UserId] and [channel][mpsc::Sender] of the queued bot.
#[derive(Debug)]
pub struct BotQueueEntry {
    pub user_id: Arc<UserId>,
    pub channel: mpsc::Sender<BotChannelMessage>,
    backup: bool,
}

impl BotQueueEntry {
    pub fn new(bot: &Bot) -> Self {
        Self {
            user_id: bot.bot_id(),
            channel: bot.get_channel(),
            backup: bot.is_backup(),
        }
    }
}

#[derive(Debug)]
pub struct BotQueue {
    idle: LinkedList<BotQueueEntry>,
    busy: HashMap<Arc<UserId>, BotQueueEntry>,
}

impl BotQueue {
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

        Self {
            idle,
            busy: HashMap::new(),
        }
    }

    /// queues an idle bot, backup bots get queued to the end the main bot to the front
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
        Cursor {
            inner: self.idle.cursor_front_mut(),
            busy: &mut self.busy,
        }
    }
}

/// A Cursor structure for iterating through the bots in idle mode.

#[derive(Debug)]
pub struct Cursor<'a> {
    busy: &'a mut HashMap<Arc<UserId>, BotQueueEntry>,
    inner: CursorMut<'a, BotQueueEntry>,
}

impl<'a> Cursor<'a> {
    /// gives us a reference to the bot the cursor is currently pointing at
    pub fn current(&mut self) -> Option<&'_ mut BotQueueEntry> {
        self.inner.current()
    }

    /// removes a bot from the idle queue and inserts them into the hashmap of busy bots
    pub fn remove(&mut self) -> Option<&'_ mut BotQueueEntry> {
        if let Some(entry) = self.inner.remove_current() {
            return Some(self.busy.try_insert(entry.user_id.clone(), entry).unwrap());
        }

        None
    }

    /// moves the cursor to the next element
    pub fn move_next(&mut self) {
        self.inner.move_next();
    }
}
