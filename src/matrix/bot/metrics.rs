use std::sync::Arc;

use matrix_sdk::ruma::{api::client::Error as ApiError, identifiers::RoomId, UserId};
use prometheus::{
    exponential_buckets, histogram_opts, labels, opts, register_histogram,
    register_int_counter_vec, Histogram, IntCounterVec,
};

#[derive(Debug)]
pub(crate) struct BotMetrics {
    pub(crate) m_room_message_req: IntCounterVec,
    pub(crate) m_room_message_err: IntCounterVec,
    pub(crate) time_active: Histogram,
}

impl BotMetrics {
    pub(crate) fn new(bot_id: &Arc<UserId>) -> Self {
        let m_room_message_req = register_int_counter_vec!(
            opts!(
                "m_room_message_requests_total",
                "total number of m.room.message requests made by bot",
                labels! {"bot" => bot_id.as_str() }
            )
            .namespace("howler")
            .subsystem("bot"),
            &["room"]
        )
        .unwrap();

        let m_room_message_err = register_int_counter_vec!(
            opts!(
                "m_room_message_requests_failed",
                "total number of failed m.room.message requests made by bot",
                labels! {"bot" => bot_id.as_str() }
            )
            .namespace("howler")
            .subsystem("bot"),
            &["room", "http_status", "errcode"]
        )
        .unwrap();

        let time_active = register_histogram!(histogram_opts!(
            "active_seconds",
            "time bot is active",
            exponential_buckets(0.01, 2., 12).unwrap(),
            labels! {"bot".to_string() => bot_id.to_string()}
        )
        .namespace("howler")
        .subsystem("bot"))
        .unwrap();

        Self {
            m_room_message_err,
            m_room_message_req,

            time_active,
        }
    }

    pub(crate) fn record_message_send_error(&self, room: &Arc<RoomId>, error: &ApiError) {
        let status_code = error.status_code.as_str();
        let error_code = error.kind.as_ref();

        self.m_room_message_req
            .with_label_values(&[room.as_str()])
            .inc();
        self.m_room_message_err
            .with_label_values(&[room.as_str(), status_code, error_code])
            .inc();
    }

    pub(crate) fn record_message_send_success(&self, room: &Arc<RoomId>) {
        self.m_room_message_req
            .with_label_values(&[room.as_str()])
            .inc();
    }
}
