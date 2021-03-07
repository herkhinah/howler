//! howler settings
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Arg, Command};
use config::Config;
use once_cell::sync::OnceCell;
use serde::Deserialize;
use serde_with::{serde_as, DurationSeconds};

use crate::{
    alert_renderer::Template,
    alertmanager_webhook_receiver::AlertReceiverSettings,
    allow_list::AllowList,
    log::LogSettings,
    matrix::{bot::settings::BotSettings, queuing::backoffs::FederationBackoffSettings},
    telemetry_endpoint::TelemetryEndpointSettings,
};

static SETTINGS: OnceCell<Settings> = OnceCell::new();

/// settings regarding federation confirmation and federation backoff
#[serde_as]
#[derive(Debug, Clone, Deserialize)]
pub struct Federation {
    #[serde_as(as = "DurationSeconds<f64>")]
    pub timeout: Duration,
    pub required_confirmations: u64,
    pub backoff: FederationBackoffSettings,
}

/// howler settings
#[serde_as]
#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    /// allow list matching either server name or a full user id, because of this we use String as the underlying data type
    pub allow_invites: AllowList,

    pub federation: Federation,
    #[serde_as(as = "DurationSeconds<f64>")]
    pub batch_interval: Duration,
    /// login data for bots
    pub bots: Vec<BotSettings>,
    pub alert_webhook_receiver: AlertReceiverSettings,
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
        let opts = Command::new(clap::crate_name!())
            .version(clap::crate_version!())
            .about(clap::crate_description!())
            .author(clap::crate_authors!())
            .args(&[
                Arg::new("config")
                    .help("path of config file")
                    .takes_value(true)
                    .short('c')
                    .long("config")
                    .default_value("./config.yaml"),
                Arg::new("level")
                    .help("log level")
                    .possible_values(&["Error", "Warn", "Info", "Debug", "Trace"])
                    .ignore_case(true)
                    .takes_value(true)
                    .long("log"),
            ])
            .get_matches();

        let config_path = opts.value_of("config").unwrap();

        let conf = Config::builder()
            .add_source(config::File::with_name(config_path))
            .build()
            .context("can't load config")?;

        let mut settings: Settings = conf.try_deserialize().context("can't deserialize config")?;

        anyhow::ensure!(
            settings.federation.required_confirmations < settings.bots.len() as u64,
            "`federation.required_confirmations` must be less than the total amount of bots"
        );

        if let Some(level) = opts.value_of("level") {
            settings.log.level = level.to_string();
        }

        Ok(settings)
    }
}
