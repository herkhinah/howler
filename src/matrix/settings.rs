use matrix_sdk::ruma::identifiers::UserId;
use serde::Deserialize;
use url::Url;

#[derive(Debug, Deserialize, Clone)]
/// bot specific settings
pub struct BotSettings {
    pub user_id: UserId,
    pub homeserver: Url,
    pub password: String,
}
