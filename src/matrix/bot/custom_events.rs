//! custom state events to configure templates used by [crate::alert_renderer]
//! and access tokens used by [crate::alertmanager_webhook_receiver]

use matrix_sdk::ruma::events::macros::EventContent;
use serde::{Deserialize, Serialize};

/// custom state event used to configure jira2 templates for rendered alerts
#[derive(Debug, Clone, Serialize, Deserialize, EventContent)]
#[ruma_event(type = "com.famedly.howler_template", kind = State, state_key_type = String)]
pub struct TemplateEventContent {
	/// html jira2 template
	pub html: String,
	/// plaintext jira2 template
	pub plain: String,
}

/// custom state event used to configure the access token used in the
/// alertmanager webhook receiver url
#[derive(Debug, Clone, Serialize, Deserialize, EventContent)]
#[ruma_event(type = "com.famedly.howler_webhook_access_token", kind = State, state_key_type = String)]
pub struct WebhookAccessTokenEventContent {
	/// uri token used in the target uri for the room
	pub token: String,
}
