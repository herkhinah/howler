use matrix_sdk::ruma::identifiers::UserId;
use serde::Deserialize;
use url::Url;

#[derive(Debug, Deserialize, Clone)]
/// login data for the bot
pub struct BotSettings {
    pub user_id: Box<UserId>,
    pub homeserver: Url,
    pub password: String,
}
