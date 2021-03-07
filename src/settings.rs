use std::time::Duration;

use anyhow::{Context, Result};
use clap::{App, Arg};
use config::Config;
use once_cell::sync::OnceCell;
use serde::Deserialize;
use serde_with::{serde_as, DurationSeconds};

use crate::{
    alertmanager_webhook_receiver::AlertReceiverSettings,
    log::LogSettings,
    matrix::{renderer::Template, settings::BotSettings},
    telemetry_endpoint::TelemetryEndpointSettings,
};

static SETTINGS: OnceCell<Settings> = OnceCell::new();

#[serde_as]
#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    #[serde_as(as = "DurationSeconds<f64>")]
    pub batch_interval: Duration,
    #[serde_as(as = "DurationSeconds<f64>")]
    pub message_timeout: Duration,
    pub url_api_prefix: String,
    pub bots: Vec<BotSettings>,
    pub alert_webhook_receiver: AlertReceiverSettings,
    pub store_path: String,
    pub default_templates: Template,
    pub log: LogSettings,
    pub telemetry_endpoint: TelemetryEndpointSettings,
}

impl Settings {
    pub fn global() -> &'static Self {
        SETTINGS.get_or_init(|| {
            match Self::load().context("failed to load config and command line arguments") {
                Ok(settings) => settings,
                Err(err) => {
                    // tracing wasn't setup yet
                    panic!("{:#?}", err);
                }
            }
        })
    }

    fn load() -> Result<Self> {
        let opts = App::new(clap::crate_name!())
            .version(clap::crate_version!())
            .about(clap::crate_description!())
            .author(clap::crate_authors!())
            .args(&[
                Arg::with_name("config")
                    .help("path of config file")
                    .takes_value(true)
                    .short("c")
                    .long("config")
                    .default_value("./config.yaml"),
                Arg::with_name("level")
                    .help("log level")
                    .possible_values(&["Error", "Warn", "Info", "Debug", "Trace"])
                    .case_insensitive(true)
                    .takes_value(true)
                    .long("log"),
            ])
            .get_matches();

        let config_path = opts.value_of("config").unwrap();

        let mut conf = Config::new();
        conf.merge(config::File::with_name(config_path))
            .context("can't load config")?;

        let mut settings: Settings = conf.try_into().context("can't load config")?;

        if let Some(level) = opts.value_of("level") {
            settings.log.level = level.to_string();
        }

        Ok(settings)
    }
}
