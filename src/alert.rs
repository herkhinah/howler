use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
/// data received at the alertmanager webhook receiver
pub struct Data {
    version: String,
    group_key: String,

    receiver: String,
    status: String,
    alerts: Vec<Alert>,
    group_labels: HashMap<String, String>,
    common_labels: HashMap<String, String>,
    common_annotations: HashMap<String, String>,
    #[serde(rename = "externalURL")]
    external_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Alert {
    status: String,
    labels: HashMap<String, String>,
    annotations: HashMap<String, String>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    #[serde(rename = "generatorURL")]
    generator_url: String,
}