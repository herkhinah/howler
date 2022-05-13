//! data structures for deserializing incoming alerts
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
/// data from prometheus received by the alertmanager webhook receiver
#[allow(clippy::missing_docs_in_private_items)]
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
#[allow(clippy::missing_docs_in_private_items)]
struct Alert {
	status: String,
	labels: HashMap<String, String>,
	annotations: HashMap<String, String>,
	starts_at: DateTime<Utc>,
	ends_at: DateTime<Utc>,
	#[serde(rename = "generatorURL")]
	generator_url: String,
}
