use std::str::FromStr;

use anyhow::Result;
use serde::Deserialize;
use tracing_subscriber::{
    filter::LevelFilter, fmt, prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt,
    EnvFilter,
};

use crate::settings::Settings;

#[derive(Debug, Clone, Deserialize)]
pub struct LogSettings {
    pub level: String,
}

impl LogSettings {
    pub fn global() -> &'static Self {
        &Settings::global().log
    }
}

pub fn setup_logging() -> Result<()> {
    let level = tracing::Level::from_str(LogSettings::global().level.as_str()).unwrap();

    let fmt_layer = fmt::layer();

    let filter_layer = EnvFilter::default()
        .add_directive(LevelFilter::from_level(level).into())
        .add_directive("sled=warn".parse()?)
        .add_directive("matrix_sdk=warn".parse()?)
        .add_directive("hyper=warn".parse()?)
        .add_directive("reqwest=warn".parse()?);

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();

    Ok(())
}
