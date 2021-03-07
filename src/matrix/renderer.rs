use std::{collections::HashSet, fs::File, io::Write, path::PathBuf};

use anyhow::{Context, Result};
use matrix_sdk::ruma::{identifiers::RoomId, EventId};
use serde::Deserialize;
use tera::Tera;

use super::sending::batch_entries::{BatchEntryContent, MessageContentError};
use crate::{
    alert,
    matrix::sending::batch_entries::{FileBatchEntryContent, MessageBatchEntryContent},
    settings::Settings,
};

/// Alert renderer
pub struct MessageRenderer {
    tera: Tera,
    template_requests: HashSet<EventId>,
    template_dir: std::path::PathBuf,
}

impl MessageRenderer {
    /// Return new renderer
    pub fn new() -> Result<Self> {
        let settings = &Settings::global().default_templates;

        let store_path = Settings::global().store_path.as_str();

        let mut path_buf = PathBuf::from(store_path);
        path_buf.push("templates");

        std::fs::create_dir_all(&path_buf)?;

        // globwalk get's confused by paths starting with "./"
        // as a workaround canonicalize to absolute path
        let path_buf = std::fs::canonicalize(path_buf)?;

        let path = format!(
            "{}/*.{{plain,html}}",
            path_buf
                .to_str()
                .context(format!("path {:?} is not valid unicode", &path_buf))?
        );

        tracing::info!("load templates from {}", path.as_str());

        let mut tera = Tera::new(path.as_str()).context("could not parse templates")?;

        tera.add_template_file(settings.plain.as_str(), Some("default.plain"))
            .context("could not load plain default template")?;
        tera.add_template_file(settings.html.as_str(), Some("default.html"))
            .context("could not load html default template")?;

        Ok(MessageRenderer {
            template_requests: HashSet::new(),
            tera,
            template_dir: path_buf,
        })
    }

    /// Render alert into BatchEntryContent. If rendering fails return error message as `BatchEntryContent`.
    /// This function tries to return [BatchEntryContent::MessageContent] but returns a [BatchEntryContent::FileContent] if the error message or rendered content doesn't fit into a single message.
    ///
    /// # Arguments
    ///
    /// * `room_id` - target room of alert
    ///
    /// * `alert` - unrendered alert
    pub fn render_alert(&self, room_id: &RoomId, alert: alert::Data) -> BatchEntryContent {
        let slug = slug::slugify(room_id.as_str());

        let plain = {
            let mut plain_key = format!("{}.plain", slug.as_str());
            if !self.tera.templates.contains_key(&plain_key) {
                plain_key = String::from("default.plain");
            }
            self.tera
                .render(&plain_key, &tera::Context::from_serialize(&alert).unwrap())
        };

        let html = {
            let mut html_key = format!("{}.html", slug.as_str());
            if !self.tera.templates.contains_key(&html_key) {
                html_key = String::from("default.html");
            }

            self.tera
                .render(&html_key, &tera::Context::from_serialize(&alert).unwrap())
        };

        match (plain, html) {
            (Ok(plain), Ok(html)) => match MessageBatchEntryContent::new(html, plain) {
                Ok(content) => BatchEntryContent::MessageContent(content),
                Err(MessageContentError::MaxSizeExceeded(content)) => {
                    use std::time::SystemTime;

                    use chrono::{offset::Utc, DateTime};

                    let (content, _) = content.unwrap();
                    let datetime: DateTime<Utc> = SystemTime::now().into();
                    let filename = format!("alert_{}.html", datetime.format("%+"));

                    BatchEntryContent::FileContent(FileBatchEntryContent { filename, content })
                }
            },
            (plain, html) => {
                let mut plain_out = "alert failed to render:".to_string();
                let mut html_out = "alert failed to render".to_string();

                if let Err(err) = plain {
                    plain_out.push_str(format!("\n{:#?}", err).as_str());
                    html_out.push_str(
                        format!(
                            "\nplain\n<pre><code>{}</code></pre>",
                            tera::escape_html(format!("{:#?}", err).as_str())
                        )
                        .as_str(),
                    );
                }

                if let Err(err) = html {
                    plain_out.push_str(format!("\n{:#?}", err).as_str());
                    html_out.push_str(
                        format!(
                            "\nplain\n<pre><code>{}</code></pre>",
                            tera::escape_html(format!("{:#?}", err).as_str())
                        )
                        .as_str(),
                    );
                }

                match MessageBatchEntryContent::new(html_out, plain_out) {
                    Ok(content) => BatchEntryContent::MessageContent(content),
                    Err(MessageContentError::MaxSizeExceeded(content)) => {
                        use std::time::SystemTime;

                        use chrono::{offset::Utc, DateTime};

                        let (content, _) = content.unwrap();
                        let datetime: DateTime<Utc> = SystemTime::now().into();
                        let filename = format!("alert_render_log_{}.html", datetime.format("%+"),);

                        BatchEntryContent::FileContent(FileBatchEntryContent { filename, content })
                    }
                }
            }
        }
    }

    /// Tries registering new templates for a specified room. If the new template can't be registered returns an error message as [BatchEntryContent].
    ///
    /// # Arguments
    ///
    /// * `event_id` - [EventId] for the "com.famedly.howler.template" event to prevent registering the template multiple times (one time for each bot)
    ///
    /// * `room_id` - [RoomId] of the room for wich the templates are registered
    ///
    /// * `template` - [Template] to register
    pub fn register_template(
        &mut self,
        event_id: EventId,
        room_id: &RoomId,
        template: Template,
    ) -> Result<(), BatchEntryContent> {
        if self.template_requests.insert(event_id) {
            return Ok(());
        }

        let slug = slug::slugify(room_id.as_str());

        self.tera
            .add_raw_templates(vec![
                (format!("{}.plain", slug.as_str()), &template.plain),
                (format!("{}.html", slug.as_str()), &template.html),
            ])
            .context("failed to compile template")
            .map_err(|err| {
                let err_plain = format!("{:#?}", err);
                let err_html = tera::escape_html(err_plain.as_str());

                let html = format!(
                    "failed to register template:\n<pre><code>{}</code></pre>",
                    err_html
                );
                let plain = format!("failed to register template:\n{:?}", err_plain);

                match MessageBatchEntryContent::new(html, plain) {
                    Ok(content) => BatchEntryContent::MessageContent(content),
                    Err(MessageContentError::MaxSizeExceeded(content)) => {
                        use std::time::SystemTime;

                        use chrono::{offset::Utc, DateTime};

                        let (content, _) = content.unwrap();
                        let datetime: DateTime<Utc> = SystemTime::now().into();
                        let filename =
                            format!("template_register_log_{}.html", datetime.format("%+"),);

                        BatchEntryContent::FileContent(FileBatchEntryContent { filename, content })
                    }
                }
            })?;

        let mut path = self.template_dir.clone();
        std::fs::create_dir_all(&path)
            .context(format!("could not create path {:?}", path))
            .unwrap();

        path.push(format!("{}.plain", slug.as_str()));

        File::create(&path)
            .context(format!("failed to create file {:?}", path))
            .unwrap()
            .write(template.plain.as_bytes())
            .context(format!("failed to write template to file {:?}", path))
            .unwrap();

        path.pop();
        path.push(format!("{}.html", slug.as_str()));

        File::create(&path)
            .context(format!("failed to create file {:?}", path))
            .unwrap()
            .write(template.html.as_bytes())
            .context(format!("failed to write template to file {:?}", path))
            .unwrap();

        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
/// Template for rendering alerts
pub struct Template {
    pub plain: String,
    pub html: String,
}
