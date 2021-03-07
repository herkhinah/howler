//! Here we expose prometheus metrics about howler
use std::net::{IpAddr, SocketAddr};

use axum::{
    body::Body,
    http::{header::CONTENT_TYPE, Response},
    routing::get,
    Router,
};
use prometheus::{Encoder, TextEncoder};
use serde::Deserialize;

use crate::settings::Settings;

#[derive(Debug, Deserialize, Clone)]
pub struct TelemetryEndpointSettings {
    pub bind_address: IpAddr,
    pub port: u16,
}

impl TelemetryEndpointSettings {
    pub fn global() -> &'static Self {
        &Settings::global().telemetry_endpoint
    }

    pub fn to_socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind_address, self.port)
    }
}

async fn metrics_handler() -> Response<Body> {
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();

    encoder.encode(&metric_families, &mut buffer).unwrap();

    Response::builder()
        .status(200)
        .header(CONTENT_TYPE, encoder.format_type())
        .body(Body::from(buffer))
        .unwrap()
}

pub async fn run_telemetry_endpoint() {
    let app = Router::new().route("/metrics", get(metrics_handler));
    axum::Server::bind(&TelemetryEndpointSettings::global().to_socket_addr())
        .serve(app.into_make_service())
        .await
        .unwrap();
}
