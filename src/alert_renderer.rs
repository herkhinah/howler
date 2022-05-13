//! Renders alerts via jira2 templates.
//!
//! Alerts are received from
//! [alertmanager_webhook_receiver](crate::alertmanager_webhook_receiver)
//! Rendered alerts are sent to [matrix::queuing][crate::matrix::queuing]

use std::{collections::HashSet, sync::Arc};

use anyhow::{Context, Result};
use matrix_sdk::ruma::{identifiers::RoomId, EventId};
use serde::Deserialize;
use tera::Tera;
use tokio::{sync::mpsc, time::Instant};

use crate::{
	alert,
	matrix::queuing::QueueChannelMessage,
	rendered_alert::{MessageContentError, RenderedAlert, RenderedAlertContent},
	settings::Settings,
};

/// Messages received by the AlertRenderer
#[derive(Debug, Clone)]
pub enum AlertRendererChannelMessage {
	/// register a template for a specified room
	RegisterTemplate {
		/// room to register the templates for
		room_id: Box<RoomId>,
		/// event id of the `com.famedly.howler_template` state event
		event_id: Box<EventId>,
		/// html jinja2 template
		html: String,
		/// plaintext jinja2 template
		plain: String,
	},
	/// render an alert for room_id
	RenderAlert {
		/// room to send the rendered alert to
		room_id: Arc<RoomId>,
		/// alert data to render
		alert: Box<alert::Data>,
		/// instant the alert was received
		arrival: Instant,
	},
}

/// Alert renderer
pub struct AlertRenderer {
	/// the rendering engine struct
	tera: Tera,
	/// event ids of `com.famedly.howler_template` state event so we don't
	/// register the same template multiple times
	template_requests: HashSet<Box<EventId>>,
	/// channel used to receive messages from bots and the webhook receiver
	tx_queue: mpsc::Sender<QueueChannelMessage>,
}

impl AlertRenderer {
	/// Return new renderer
	pub fn new(tx_queue: mpsc::Sender<QueueChannelMessage>) -> Result<Self> {
		let settings = &Settings::global().default_templates;

		let mut tera = Tera::default();

		tera.add_template_file(settings.plain.as_str(), Some("default.plain"))
			.context("could not load plain default template")?;
		tera.add_template_file(settings.html.as_str(), Some("default.html"))
			.context("could not load html default template")?;

		Ok(AlertRenderer { template_requests: HashSet::new(), tera, tx_queue })
	}

	/// main loop of [AlertRenderer]
	///
	/// receives unrendered alerts from the alertmanager webhook receiver and
	/// templates from the bot
	///
	/// * `rx` - the channel where we receive requests to render alerts and
	///   requests to register jira2 templates
	pub async fn run(mut self, mut rx: mpsc::Receiver<AlertRendererChannelMessage>) {
		while let Some(msg) = rx.recv().await {
			match msg {
				// Register jira2 templates for room
				AlertRendererChannelMessage::RegisterTemplate {
					room_id,
					event_id,
					html,
					plain,
				} => {
					self.register_template(event_id, room_id, html, plain).await;
				}
				// render alert and send it to queue
				AlertRendererChannelMessage::RenderAlert { room_id, alert, arrival } => {
					self.render_alert(room_id, alert, arrival).await;
				}
			}
		}
	}

	/// Renders alert into and sends it's to the queue.
	///
	/// # Arguments
	///
	/// * `room_id` - target room of alert
	///
	/// * `alert` - unrendered alert
	///
	/// * `arrival` - time of arrival of the alert
	pub async fn render_alert(
		&self,
		room_id: Arc<RoomId>,
		alert: Box<alert::Data>,
		arrival: Instant,
	) {
		let room = room_id;
		#[allow(clippy::expect_used)]
		let context = tera::Context::from_serialize(&alert).expect("failed to serialize alert");

		let plain = {
			// check if there's a plaintext template registered for this room.
			// if not fall back to default templates
			let mut plain_key = format!("{room}.plain");
			if !self.tera.templates.contains_key(&plain_key) {
				plain_key = String::from("default.plain");
			}

			// render alert
			self.tera.render(&plain_key, &context)
		};

		// same but for html
		let html = {
			let mut html_key = format!("{room}.html");
			if !self.tera.templates.contains_key(&html_key) {
				html_key = String::from("default.html");
			}

			self.tera.render(&html_key, &context)
		};

		match (plain, html) {
			// bot formatted and unformatted message parts successfully rendered
			(Ok(plain), Ok(html)) => match RenderedAlertContent::new(html, plain) {
				Ok(content) | Err(MessageContentError::MaxSizeExceeded(content)) => {
					#[allow(clippy::expect_used)]
					self.tx_queue
						.send(QueueChannelMessage::QueueEntry {
							room,
							entry: RenderedAlert::new(arrival, content),
						})
						.await
						.expect("tx_queue closed");
				}
			},

			// a render error occured. send render error as message into the queue
			(plain, html) => {
				for (format, result) in [("plaintext", plain), ("html", html)] {
					if let Err(err) = result {
						let html = format!("<strong><font color=\"#ff0000\">failed to render alert to {format}:</font></strong>\n<br>\n<code>{}</code>", tera::escape_html(&format!("{err:#?}")));
						let plain = format!("failed to render alert to {format}:\n{err:#?}");
						let room = room.clone();

						match RenderedAlertContent::new(html, plain) {
							Ok(content) | Err(MessageContentError::MaxSizeExceeded(content)) => {
								#[allow(clippy::expect_used)]
								self.tx_queue
									.send(QueueChannelMessage::QueueEntry {
										room,
										entry: RenderedAlert::new(arrival, content),
									})
									.await
									.expect("tx_queue closed");
							}
						}
					}
				}
			}
		}
	}

	/// Tries registering new templates for a specified room. If the new
	/// template can't be registered it sends an error message to queue.
	///
	/// # Arguments
	///
	/// * `event_id` - [EventId] for the "com.famedly.howler_template" event to
	///   prevent registering the template multiple times (one time for each
	///   bot)
	///
	/// * `room_id` - [RoomId] of the room for wich the templates are registered
	///
	/// * `html` - html template to register
	///
	/// * `plain` - plain template to register
	pub async fn register_template(
		&mut self,
		event_id: Box<EventId>,
		room_id: Box<RoomId>,
		html: String,
		plain: String,
	) {
		// make sure we don't register the same template multiple times
		if !self.template_requests.insert(event_id) {
			return;
		}

		let room = Arc::from(room_id);

		if let Err(err) = self.tera.add_raw_template(&format!("{room}.plain"), &plain) {
			// failed to register plaintext template, send error message to the queue
			let html = format!("<strong><font color=\"#ff0000\">failed to register plaintext template:</font></strong>\n<br>\n<code>{}</code>", tera::escape_html(&format!("{err:#?}")));
			let plain = format!("failed to register plaintext template:\n{err:#?}");
			let room = Arc::clone(&room);

			match RenderedAlertContent::new(html, plain) {
				Ok(content) | Err(MessageContentError::MaxSizeExceeded(content)) => {
					#[allow(clippy::expect_used)]
					self.tx_queue
						.send(QueueChannelMessage::QueueEntry {
							room,
							entry: RenderedAlert::new(Instant::now(), content),
						})
						.await
						.expect("tx_queue closed");
				}
			}
		}

		if let Err(err) = self.tera.add_raw_template(&format!("{room}.html"), &html) {
			// failed to register html template, send error message to the queue
			let html = format!("<strong><font color=\"#ff0000\">failed to register html template:</font></strong>\n<br>\n<code>{}</code>", tera::escape_html(&format!("{err:#?}")));
			let plain = format!("failed to register html template:\n{err:#?}");
			let room = room.clone();

			match RenderedAlertContent::new(html, plain) {
				Ok(content) | Err(MessageContentError::MaxSizeExceeded(content)) => {
					#[allow(clippy::expect_used)]
					self.tx_queue
						.send(QueueChannelMessage::QueueEntry {
							room,
							entry: RenderedAlert::new(Instant::now(), content),
						})
						.await
						.expect("tx_queue closed");
				}
			}
		}
	}
}

/// Template used in the config file
#[derive(Debug, Clone, Deserialize)]
pub struct TemplateConfig {
	/// jinja2 template for plaintext messages
	plain: String,
	/// jinja2 template for html messages
	html: String,
}
