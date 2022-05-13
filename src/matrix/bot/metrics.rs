//! prometheus meters for bot

use std::sync::Arc;

use matrix_sdk::ruma::{api::client::Error as ApiError, identifiers::RoomId, UserId};
use prometheus::{
	exponential_buckets, histogram_opts, labels, opts, register_histogram,
	register_int_counter_vec, Histogram, IntCounterVec,
};

#[derive(Debug)]
/// prometheus meters for matrix-sdk bot
pub(crate) struct BotMetrics {
	/// total number of sent m.room.message requests
	pub(crate) m_room_message_req: IntCounterVec,
	/// number of failed m.room.message requests
	pub(crate) m_room_message_err: IntCounterVec,
	/// total time bot is trying to send messages (time spent in
	/// [Bot::send](super::Bot::send))
	pub(crate) time_active: Histogram,
}

impl BotMetrics {
	/// construct prometheus meters
	pub(crate) fn new(bot_id: &Arc<UserId>) -> Result<Self, prometheus::Error> {
		let m_room_message_req = register_int_counter_vec!(
			opts!(
				"m_room_message_requests_total",
				"total number of m.room.message requests made by bot",
				labels! {"bot" => bot_id.as_str() }
			)
			.namespace("howler")
			.subsystem("bot"),
			&["room"]
		)?;

		let m_room_message_err = register_int_counter_vec!(
			opts!(
				"m_room_message_requests_error_api",
				"unsuccessfull m.room.message requests made by bot",
				labels! {"bot" => bot_id.as_str() }
			)
			.namespace("howler")
			.subsystem("bot"),
			&["room", "http_status", "errcode"]
		)?;

		let time_active = register_histogram!(histogram_opts!(
			"active_seconds",
			"time bot is active",
			exponential_buckets(0.01, 2., 12)?,
			labels! {"bot".to_owned() => bot_id.to_string()}
		)
		.namespace("howler")
		.subsystem("bot"))?;

		Ok(Self { m_room_message_err, m_room_message_req, time_active })
	}

	/// counts total number of m.room.message events sent
	pub(crate) fn record_message_send(&self, room: &Arc<RoomId>) {
		self.m_room_message_req.with_label_values(&[room.as_str()]).inc();
	}

	/// counts number of failed m.room.message event requests
	pub(crate) fn record_message_send_error(&self, room: &Arc<RoomId>, error: Option<&ApiError>) {
		let (status_code, error_code) =
			error.map_or(("", ""), |error| (error.status_code.as_str(), error.kind.as_ref()));

		self.m_room_message_err.with_label_values(&[room.as_str(), status_code, error_code]).inc();
	}
}
