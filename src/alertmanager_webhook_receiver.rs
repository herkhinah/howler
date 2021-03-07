use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use anyhow::{Context, Result};
use axum::{
    extract::{rejection::JsonRejection, Extension, Json, Path},
    handler::post,
    http::StatusCode,
    AddExtensionLayer, Router,
};
use matrix_sdk::ruma::identifiers::RoomId;
use prometheus::IntCounterVec;
use serde::Deserialize;
use tokio::{sync::mpsc::Sender, time::Instant};

use crate::{
    alert,
    matrix::channels::{AlertChannel, OnceCellMPSC},
    room_tokens::RoomTokenMap,
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
    token_map: RoomTokenMap,
    tx: Sender<(RoomId, alert::Data, Instant)>,
    metric: IntCounterVec,
}

impl State {
    pub fn new() -> Result<Self> {
        use prometheus::{opts, register_int_counter_vec};
        let token_map = RoomTokenMap::global().to_owned();

        let metric = register_int_counter_vec!(
            opts!("received_alerts", "total number of deserialized alerts")
                .namespace("howler")
                .subsystem("alertmanager_webhook"),
            &["room"]
        )?;

        let tx = AlertChannel::sender();

        Ok(Self {
            tx,
            token_map,
            metric,
        })
    }
}

async fn prometheus_receiver(
    Extension(state): Extension<Arc<State>>,
    Path((token, ..)): Path<(String,)>,
    alert: Result<Json<alert::Data>, JsonRejection>,
) -> StatusCode {
    let State {
        token_map,
        tx,
        metric,
    } = &*state;

    match alert {
        Ok(Json(alert)) => match token_map.get_room_id(token.as_str()).unwrap() {
            Some(room_id) => {
                metric.with_label_values(&[room_id.as_str()]).inc();
                tx.send((room_id, alert, Instant::now())).await.unwrap();
                StatusCode::OK
            }
            None => {
                metric.with_label_values(&["invalid_token"]).inc();
                StatusCode::NOT_FOUND
            }
        },
        Err(err) => {
            tracing::debug!("failed to deserialize alert: {:?}", err);
            StatusCode::BAD_REQUEST
        }
    }
}

pub async fn run_prometheus_receiver() -> Result<()> {
    let state = Arc::new(State::new()?);
    let addr = AlertReceiverSettings::global().to_socket_addr();

    let app = Router::new()
        .route("/:token", post(prometheus_receiver))
        .layer(AddExtensionLayer::new(state));

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .context("prometheus endpoint crashed")?;

    Ok(())
}
