//! config file options for bot

use matrix_sdk::ruma::identifiers::UserId;
use serde::Deserialize;
use url::Url;

#[derive(Debug, Deserialize, Clone)]
/// login data for the bot
pub struct BotSettings {
	/// user id of bot
	pub user_id: Box<UserId>,
	/// homeserver url of bot
	pub homeserver: Url,
	/// password of bot
	pub password: String,
}
