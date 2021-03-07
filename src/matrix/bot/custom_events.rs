//! custom state events to configure templates used by [crate::alert_renderer] and access tokens used by [crate::alertmanager_webhook_receiver]

use matrix_sdk::ruma::events::macros::EventContent;
use serde::{Deserialize, Serialize};

/// custom state event used to configure jira2 templates for rendered alerts
#[derive(Debug, Clone, Serialize, Deserialize, EventContent)]
#[ruma_event(type = "com.famedly.howler_template", kind = State)]
pub struct TemplateEventContent {
    pub html: String,
    pub plain: String,
}

/// custom state event used to configure the access token used in the alertmanager webhook receiver url
#[derive(Debug, Clone, Serialize, Deserialize, EventContent)]
#[ruma_event(type = "com.famedly.howler_webhook_access_token", kind = State)]
pub struct WebhookAccessTokenEventContent {
    pub token: String,
}
