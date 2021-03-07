//! Here we receive the alerts from prometheus and send them to the [AlertRenderer][crate::alert_renderer::AlertRenderer]

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
pub struct AlertReceiverSettings {
    pub bind_address: IpAddr,
    pub port: u16,
}

impl AlertReceiverSettings {
    pub fn global() -> &'static Self {
        &Settings::global().alert_webhook_receiver
    }

    pub fn to_socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind_address, self.port)
    }
}

struct State {
    token_map: &'static RwLock<RoomTokenMap>,
    tx_renderer: mpsc::Sender<AlertRendererChannelMessage>,
    metric: IntCounterVec,
}

impl State {
    pub fn new(tx_renderer: mpsc::Sender<AlertRendererChannelMessage>) -> Self {
        use prometheus::{opts, register_int_counter_vec};
        let token_map = RoomTokenMap::global().to_owned();

        let metric = register_int_counter_vec!(
            opts!("received_alerts", "total number of deserialized alerts")
                .namespace("howler")
                .subsystem("alertmanager_webhook_receiver"),
            &["room"]
        )
        .unwrap();

        Self {
            tx_renderer,
            token_map,
            metric,
        }
    }
}

async fn prometheus_receiver(
    Path((room_id, token)): Path<(String, String)>,
    alert: Result<Json<Box<alert::Data>>, JsonRejection>,
    Extension(state): Extension<Arc<State>>,
) -> StatusCode {
    let State {
        token_map,
        tx_renderer,
        metric,
    } = &*state;
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
            tx_renderer
                .send(AlertRendererChannelMessage::RenderAlert {
                    room_id,
                    alert,
                    arrival: Instant::now(),
                })
                .await
                .expect("tx_renderer closed");
            StatusCode::OK
        }
        None => {
            metric.with_label_values(&["invalid_token"]).inc();
            StatusCode::NOT_FOUND
        }
    }
}

pub async fn run_prometheus_receiver(tx_renderer: mpsc::Sender<AlertRendererChannelMessage>) {
    let state = Arc::new(State::new(tx_renderer));
    let addr = AlertReceiverSettings::global().to_socket_addr();

    let app = Router::new()
        .route("/:room_id/:token", post(prometheus_receiver))
        .layer(Extension(state));

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .expect("prometheus endpoint crashed");
}
