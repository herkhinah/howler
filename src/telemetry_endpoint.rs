//! Here we expose prometheus metrics about howler
use std::net::{IpAddr, SocketAddr};

use axum::{
	http::{header::CONTENT_TYPE, StatusCode},
	response::{IntoResponse, Response},
	routing::get,
	Router,
};
use prometheus::{Encoder, TextEncoder};
use serde::Deserialize;

use crate::settings::Settings;

#[derive(Debug, Deserialize, Clone)]
/// Settings for the prometheus telemetry endpoint
pub struct TelemetryEndpointSettings {
	/// bind address
	pub bind_address: IpAddr,
	/// port
	pub port: u16,
}

impl TelemetryEndpointSettings {
	/// get the configured telemetry settings
	pub fn global() -> &'static Self {
		&Settings::global().telemetry_endpoint
	}

	/// Creates `SocketAddr` from configuration
	pub fn to_socket_addr(&self) -> SocketAddr {
		SocketAddr::new(self.bind_address, self.port)
	}
}

#[allow(clippy::unused_async)]
/// request handler that gathers and encodes the registered prometheus metrics
async fn metrics_handler() -> Response {
	let mut buffer = vec![];
	let encoder = TextEncoder::new();
	let metric_families = prometheus::gather();

	if let Err(err) = encoder.encode(&metric_families, &mut buffer) {
		tracing::error!("failed to encode metrics: {err}");
		return StatusCode::INTERNAL_SERVER_ERROR.into_response();
	}

	([(CONTENT_TYPE, encoder.format_type()); 1], Into::<bytes::Bytes>::into(buffer)).into_response()
}

/// run the telemetry endpoint server
pub async fn run_telemetry_endpoint() {
	let app = Router::new().route("/metrics", get(metrics_handler));
	#[allow(clippy::expect_used)]
	axum::Server::bind(&TelemetryEndpointSettings::global().to_socket_addr())
		.serve(app.into_make_service())
		.await
		.expect("prometheus telemetry endpoint crashed");
}
