//! We have two types of batchable entries: Files and Messages. Files are used as a fallback if the rendered alert or an error message is too large to send as a single message.

use matrix_sdk::ruma::events::{room::message::MessageEventContent, AnyMessageEventContent};
use thiserror::Error;
use tokio::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchEntryContent {
    MessageContent(MessageBatchEntryContent),
    FileContent(FileBatchEntryContent),
}

const MAX_MESSAGE_CONTENT_LEN: usize = 60000;

#[derive(Debug, Clone, PartialEq, Eq)]
/// metadata of batch entry
pub struct BatchEntryMetadata {
    /// number of total sending attempts
    pub sending_attempts: u64,
    /// point in time the batch entry was created (e.g. point in time the alert was received by the prometheus webhook receiver)
    pub arrival: Instant,
    /// point in time of last sending attempt
    pub last_sending_attempt: Option<Instant>,
}

impl BatchEntryMetadata {
    /// Constructs metadata for an unqueued BatchEntry
    ///
    /// # Arguments
    ///
    /// * `arrival` - instant batch entry was received/created
    pub fn new(arrival: Instant) -> Self {
        Self {
            sending_attempts: 0,
            last_sending_attempt: None,
            arrival,
        }
    }
}

impl PartialOrd for BatchEntryMetadata {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BatchEntryMetadata {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.arrival.cmp(&other.arrival)
    }
}

#[derive(Error, Debug, Clone)]
pub enum MessageContentError {
    #[error("maximum message size exceeded")]
    MaxSizeExceeded(Option<(String, String)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Content of a message batch entry
pub struct MessageBatchEntryContent {
    html: String,
    plain: String,
}

impl MessageBatchEntryContent {
    /// Tries constructing a new [MessageBatchEntryContent] from message content. Returns [Result::Err] if the resulting [MessageBatchEntryContent] would be too big.
    ///
    /// # Arguments
    ///
    /// * `html` - the formatted message content
    ///
    /// * `plain` - the unformatted message content
    pub fn new(html: String, plain: String) -> Result<Self, MessageContentError> {
        if html.len() + plain.len() > MAX_MESSAGE_CONTENT_LEN {
            return Err(MessageContentError::MaxSizeExceeded(Some((html, plain))));
        }

        Ok(Self { html, plain })
    }

    /// Tries appending a [MessageBatchEntryContent]. Returns an [Result::Err] if the resulting [MessageBatchEntryContent] would be too large. In this case `self` remains unmodified.
    ///
    /// # Arguments
    ///
    /// * `content` - the [MessageBatchEntryContent] to append
    pub fn append(
        &mut self,
        content: &MessageBatchEntryContent,
    ) -> Result<(), MessageContentError> {
        if self.html.is_empty() && self.plain.is_empty() {
            self.plain.push_str(content.plain.as_str());
            self.html.push_str(content.html.as_str());
            return Ok(());
        }

        let delimiter = MessageBatchEntryContent {
            html: String::from("\n<br>\n"),
            plain: String::from("\n"),
        };

        if self.len() + delimiter.len() + content.len() > MAX_MESSAGE_CONTENT_LEN {
            return Err(MessageContentError::MaxSizeExceeded(None));
        }

        self.html.push_str(delimiter.html.as_str());
        self.html.push_str(content.html.as_str());

        self.plain.push_str(delimiter.plain.as_str());
        self.plain.push_str(content.plain.as_str());

        Ok(())
    }

    pub fn len(&self) -> usize {
        self.html.len() + self.plain.len()
    }
}

impl Default for MessageBatchEntryContent {
    fn default() -> Self {
        Self {
            html: String::new(),
            plain: String::new(),
        }
    }
}

impl From<MessageBatchEntryContent> for AnyMessageEventContent {
    fn from(msg: MessageBatchEntryContent) -> Self {
        AnyMessageEventContent::RoomMessage(MessageEventContent::text_html(msg.plain, msg.html))
    }
}
impl From<&MessageBatchEntryContent> for AnyMessageEventContent {
    fn from(msg: &MessageBatchEntryContent) -> AnyMessageEventContent {
        AnyMessageEventContent::RoomMessage(MessageEventContent::text_html(
            msg.plain.clone(),
            msg.html.clone(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Batchable file content. Multiple [FileBatchEntryContent] get batched into a tar archive, by prefixing each filename with a strictly monotonically increasing natural number. This structure will only be used if the content would be too large to send as a single message.
pub struct FileBatchEntryContent {
    pub filename: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// [FileBatchEntryContent] together with [BatchEntryMetadata] to track throughput time and sending attempts.
pub struct FileBatchEntry {
    pub metadata: BatchEntryMetadata,
    pub content: FileBatchEntryContent,
}

impl Ord for FileBatchEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.metadata.cmp(&other.metadata)
    }
}

impl PartialOrd for FileBatchEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.metadata.partial_cmp(&other.metadata)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// [MessageBatchEntryContent] together with [BatchEntryMetadata] to track throughput time and sending attempts.
pub struct MessageBatchEntry {
    pub metadata: BatchEntryMetadata,
    pub content: MessageBatchEntryContent,
}

impl Ord for MessageBatchEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.metadata.cmp(&other.metadata)
    }
}

impl PartialOrd for MessageBatchEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.metadata.partial_cmp(&other.metadata)
    }
}

#[derive(Debug, Clone, PartialEq)]
/// List of multiple batch entries of the same kind.
pub enum BatchEntries {
    FileEntries(Vec<FileBatchEntry>),
    MessageEntries(Vec<MessageBatchEntry>),
}

impl BatchEntries {
    /// Get number of batched entries.    
    pub fn len(&self) -> usize {
        match self {
            BatchEntries::FileEntries(entries) => entries.len(),
            BatchEntries::MessageEntries(entries) => entries.len(),
        }
    }

    /// Get iterator over &mut references of the [BatchEntryMetadata]
    pub fn metadata<'a>(&'a mut self) -> Box<dyn Iterator<Item = &'a mut BatchEntryMetadata> + 'a> {
        match self {
            BatchEntries::FileEntries(entries) => {
                Box::new(entries.iter_mut().map(|entry| &mut entry.metadata))
            }
            BatchEntries::MessageEntries(entries) => {
                Box::new(entries.iter_mut().map(|entry| &mut entry.metadata))
            }
        }
    }
}

impl IntoIterator for BatchEntries {
    type Item = BatchEntry;

    type IntoIter = Box<dyn Iterator<Item = BatchEntry>>;

    fn into_iter(self) -> Self::IntoIter {
        match self {
            BatchEntries::FileEntries(entries) => {
                Box::new(entries.into_iter().map(BatchEntry::FileEntry))
            }
            BatchEntries::MessageEntries(entries) => {
                Box::new(entries.into_iter().map(BatchEntry::MessageEntry))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum BatchEntry {
    FileEntry(FileBatchEntry),
    MessageEntry(MessageBatchEntry),
}

impl BatchEntry {
    pub fn new(arrival: Instant, content: BatchEntryContent) -> Self {
        let metadata = BatchEntryMetadata::new(arrival);

        match content {
            BatchEntryContent::MessageContent(content) => {
                BatchEntry::MessageEntry(MessageBatchEntry { metadata, content })
            }
            BatchEntryContent::FileContent(content) => {
                BatchEntry::FileEntry(FileBatchEntry { metadata, content })
            }
        }
    }
}
