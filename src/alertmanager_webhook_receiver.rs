//! Here we receive the alerts from prometheus and send them to the
//! [AlertRenderer][crate::alert_renderer::AlertRenderer]

use std::{
	net::{IpAddr, SocketAddr},
	sync::Arc,
};

use anyhow::Result;
use axum::{
	extract::{rejection::JsonRejection, Extension, Json, Path},
	http::StatusCode,
	routing::post,
	Router,
};
use matrix_sdk::{locks::RwLock, ruma::RoomId};
use prometheus::IntCounterVec;
use serde::Deserialize;
use tokio::{sync::mpsc, time::Instant};

use crate::{
	alert, alert_renderer::AlertRendererChannelMessage, room_tokens::RoomTokenMap,
	settings::Settings,
};

#[derive(Debug, Deserialize, Clone)]
/// ip address and port for the alertmanager webhook receiver
pub struct AlertReceiverSettings {
	/// ip address the server binds to
	pub bind_address: IpAddr,
	/// port the server binds to
	pub port: u16,
}

impl AlertReceiverSettings {
	/// returns the config file settings
	pub fn global() -> &'static Self {
		&Settings::global().alert_webhook_receiver
	}

	/// constructs `SocketAddr` from configuration
	pub fn to_socket_addr(&self) -> SocketAddr {
		SocketAddr::new(self.bind_address, self.port)
	}
}

/// global state used by the handler
struct State {
	/// map for validating token used for target room
	token_map: &'static RwLock<RoomTokenMap>,
	/// channel for sending received alerts to the renderer
	tx_renderer: mpsc::Sender<AlertRendererChannelMessage>,
	///
	received_alerts_meter: IntCounterVec,
}

impl State {
	/// construct State used by `prometheus_receiver` handler
	pub fn new(
		tx_renderer: mpsc::Sender<AlertRendererChannelMessage>,
	) -> Result<Self, prometheus::Error> {
		use prometheus::{opts, register_int_counter_vec};
		let token_map = RoomTokenMap::global().to_owned();

		let received_alerts_meter = register_int_counter_vec!(
			opts!("received_alerts", "total number of deserialized alerts")
				.namespace("howler")
				.subsystem("alertmanager_webhook_receiver"),
			&["room"]
		)?;

		Ok(Self { tx_renderer, token_map, received_alerts_meter })
	}
}

/// axum handler for receiving alerts from prometheus alertmanager
async fn prometheus_receiver(
	Path((room_id, token)): Path<(String, String)>,
	alert: Result<Json<Box<alert::Data>>, JsonRejection>,
	Extension(state): Extension<Arc<State>>,
) -> StatusCode {
	let State { token_map, tx_renderer, received_alerts_meter: metric } = &*state;
	let room_id = match RoomId::parse_arc(room_id) {
		Ok(room_id) => room_id,
		Err(_) => {
			metric.with_label_values(&["invalid_room_id"]).inc();
			return StatusCode::NOT_FOUND;
		}
	};

	let alert = match alert {
		Ok(Json(alert)) => alert,
		Err(_) => {
			metric.with_label_values(&["invalid_alert"]).inc();
			return StatusCode::BAD_REQUEST;
		}
	};

	match token_map.read().await.get_token(&room_id) {
		Some(registered_token) => {
			if registered_token != &token {
				metric.with_label_values(&["invalid_token"]).inc();
				return StatusCode::NOT_FOUND;
			}

			metric.with_label_values(&[room_id.as_str()]).inc();
			#[allow(clippy::expect_used)]
			tx_renderer
				.send(AlertRendererChannelMessage::RenderAlert {
					room_id,
					alert,
					arrival: Instant::now(),
				})
				.await
				.expect("channel tx_renderer closed");
			StatusCode::OK
		}
		None => {
			metric.with_label_values(&["invalid_token"]).inc();
			StatusCode::NOT_FOUND
		}
	}
}

/// run the prometheus webhook receiver endpoint
pub async fn run_prometheus_receiver(
	tx_renderer: mpsc::Sender<AlertRendererChannelMessage>,
) -> Result<()> {
	let state = Arc::new(State::new(tx_renderer)?);
	let addr = AlertReceiverSettings::global().to_socket_addr();

	let app =
		Router::new().route("/:room_id/:token", post(prometheus_receiver)).layer(Extension(state));

	axum::Server::bind(&addr).serve(app.into_make_service()).await?;

	Ok(())
}
