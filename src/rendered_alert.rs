//! We have two types of batchable entries: Files and Messages. Files are used
//! as a fallback if the rendered alert or an error message is too large to send
//! as a single message.

use matrix_sdk::ruma::events::{
	room::message::RoomMessageEventContent, AnyMessageLikeEventContent,
};
use thiserror::Error;
use tokio::time::Instant;

/// size limit of [RenderedAlertContent] in bytes
pub const MAX_MESSAGE_CONTENT_LEN: usize = 40000;

#[derive(Debug, Clone, PartialEq, Eq)]
/// metadata of batch entry
pub struct RenderedAlertMetadata {
	/// number of total sending attempts
	pub sending_attempts: u64,
	/// point in time the batch entry was created (e.g. point in time the alert
	/// was received by the prometheus webhook receiver)
	pub arrival: Instant,
	/// point in time of last sending attempt
	pub last_sending_attempt: Option<Instant>,
}

impl RenderedAlertMetadata {
	/// Constructs metadata for an unqueued BatchEntry
	///
	/// # Arguments
	///
	/// * `arrival` - instant batch entry was received/created
	pub fn new(arrival: Instant) -> Self {
		Self { sending_attempts: 0, last_sending_attempt: None, arrival }
	}
}

impl PartialOrd for RenderedAlertMetadata {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for RenderedAlertMetadata {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		self.arrival.cmp(&other.arrival)
	}
}

/// Error occuring when creating or appending [RenderedAlertContent]
#[derive(Error, Debug, Clone)]
pub enum MessageContentError<T: Sized> {
	/// the size limit specified by [MAX_MESSAGE_CONTENT_LEN] was exceeded
	#[error("maximum message size exceeded")]
	MaxSizeExceeded(T),
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
/// Content of a message batch entry
pub struct RenderedAlertContent {
	/// html message content
	html: String,
	/// plaintext message content
	plain: String,
}

impl RenderedAlertContent {
	/// Tries constructing a new [RenderedAlertContent] from message content.
	/// Returns [Result::Err] if the resulting [RenderedAlertContent] would be
	/// too big.
	///
	/// # Arguments
	///
	/// * `html` - the formatted message content
	///
	/// * `plain` - the unformatted message content
	pub fn new(html: String, plain: String) -> Result<Self, MessageContentError<Self>> {
		if html.len() + plain.len() > MAX_MESSAGE_CONTENT_LEN {
			return Err(MessageContentError::MaxSizeExceeded(Self::truncated(plain)));
		}

		Ok(Self { html, plain })
	}

	/// truncate message if it exceed MAX_MESSAGE_CONTENT_LEN
	/// we only allow to specify a plain text message because it's hard to
	/// truncate html messages without breaking stuff
	pub fn truncated(mut plain: String) -> Self {
		plain.truncate(MAX_MESSAGE_CONTENT_LEN / 2);

		Self { html: plain.clone(), plain }
	}

	/// Tries appending a [RenderedAlertContent]. Returns an [Result::Err] if
	/// the resulting [RenderedAlertContent] would be too large. In this case
	/// `self` remains unmodified.
	///
	/// # Arguments
	///
	/// * `content` - the [RenderedAlertContent] to append
	pub fn append(&mut self, content: &Self) -> Result<(), MessageContentError<()>> {
		if self.html.is_empty() && self.plain.is_empty() {
			self.plain.push_str(content.plain.as_str());
			self.html.push_str(content.html.as_str());
			return Ok(());
		}

		let delimiter = Self { html: String::from("\n<br>\n"), plain: String::from("\n") };

		if self.len() + delimiter.len() + content.len() > MAX_MESSAGE_CONTENT_LEN {
			return Err(MessageContentError::MaxSizeExceeded(()));
		}

		self.html.push_str(delimiter.html.as_str());
		self.html.push_str(content.html.as_str());

		self.plain.push_str(delimiter.plain.as_str());
		self.plain.push_str(content.plain.as_str());

		Ok(())
	}

	/// the combined length in bytes of html and plain text message
	pub fn len(&self) -> usize {
		self.html.len() + self.plain.len()
	}
}

impl From<RenderedAlertContent> for AnyMessageLikeEventContent {
	fn from(msg: RenderedAlertContent) -> Self {
		AnyMessageLikeEventContent::RoomMessage(RoomMessageEventContent::text_html(
			msg.plain, msg.html,
		))
	}
}
impl From<&RenderedAlertContent> for AnyMessageLikeEventContent {
	fn from(msg: &RenderedAlertContent) -> AnyMessageLikeEventContent {
		AnyMessageLikeEventContent::RoomMessage(RoomMessageEventContent::text_html(
			msg.plain.clone(),
			msg.html.clone(),
		))
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// [RenderedAlertContent] together with [RenderedAlertMetadata] to track
/// throughput time and sending attempts.
pub struct RenderedAlert {
	/// metadata of the alert (sending attempts, instant of arrival)
	pub metadata: RenderedAlertMetadata,
	/// rendered alert content
	pub content: RenderedAlertContent,
}

impl RenderedAlert {
	/// construct RenderedAlert
	pub fn new(arrival: Instant, content: RenderedAlertContent) -> Self {
		let metadata = RenderedAlertMetadata::new(arrival);

		Self { metadata, content }
	}
}

impl Ord for RenderedAlert {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		self.metadata.cmp(&other.metadata)
	}
}

impl PartialOrd for RenderedAlert {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		self.metadata.partial_cmp(&other.metadata)
	}
}
