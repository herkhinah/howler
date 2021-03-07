//! per room access tokens for the alertmanager webhook receiver
use std::sync::Arc;

use hashbrown::HashMap;
use matrix_sdk::{locks::RwLock, ruma::identifiers::RoomId};
use once_cell::sync::OnceCell;

static ROOM_TOKENS: OnceCell<RwLock<RoomTokenMap>> = OnceCell::new();

/// the access token for each room
#[derive(Debug)]
pub struct RoomTokenMap {
    room_to_token: HashMap<Arc<RoomId>, String>,
}

impl RoomTokenMap {
    fn new() -> Self {
        Self {
            room_to_token: HashMap::new(),
        }
    }

    pub fn global() -> &'static RwLock<Self> {
        ROOM_TOKENS.get_or_init(|| RwLock::new(Self::new()))
    }

    /// register access token for a room
    pub fn register_token(&mut self, room_id: Arc<RoomId>, token: String) {
        self.room_to_token.insert(room_id, token);
    }

    /// get access token for a room
    pub fn get_token(&self, room_id: &Arc<RoomId>) -> Option<&String> {
        self.room_to_token.get(room_id)
    }
}
