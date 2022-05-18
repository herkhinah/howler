//! per room access tokens for the alertmanager webhook receiver (set via
//! com.famedly.howler_webhook_access_token state event)

use hashbrown::HashMap;
use matrix_sdk::{
	locks::RwLock,
	ruma::{OwnedRoomId, RoomId},
};
use once_cell::sync::OnceCell;

/// map to get the token set for a room
static ROOM_TOKENS: OnceCell<RwLock<RoomTokenMap>> = OnceCell::new();

/// the access token set via the state event
/// com.famedly.howler_webhook_access_token for each room
#[derive(Debug)]
pub struct RoomTokenMap {
	/// maps room ids to tokens (set in the room via
	/// com.famedly.howler_webhook_access_token)
	room_to_token: HashMap<OwnedRoomId, String>,
}

impl RoomTokenMap {
	/// Constructs an empty map
	fn new() -> Self {
		Self { room_to_token: HashMap::new() }
	}

	/// Get a reference to the map (only one instance is used in the whole
	/// program)
	pub fn global() -> &'static RwLock<Self> {
		ROOM_TOKENS.get_or_init(|| RwLock::new(Self::new()))
	}

	/// register access token for a room
	pub fn register_token(&mut self, room_id: OwnedRoomId, token: String) {
		self.room_to_token.insert(room_id, token);
	}

	/// get access token for a room
	pub fn get_token(&self, room_id: &RoomId) -> Option<&String> {
		self.room_to_token.get(room_id)
	}
}
