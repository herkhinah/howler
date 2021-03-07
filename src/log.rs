//! set up logging via [tracing]

use std::str::FromStr;

use anyhow::Result;
use serde::Deserialize;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

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

#[cfg(feature = "console")]
pub fn setup_logging() -> Result<()> {
    console_subscriber::init();
    Ok(())
}

#[cfg(not(feature = "console"))]
pub fn setup_logging() -> Result<()> {
    let level = tracing::Level::from_str(LogSettings::global().level.as_str()).unwrap();

    let fmt_layer = fmt::layer();

    let filter_layer = EnvFilter::default()
        .add_directive(LevelFilter::from_level(level).into())
        .add_directive("sled=warn".parse()?)
        .add_directive("matrix_sdk=warn".parse()?)
        .add_directive("hyper=warn".parse()?)
        .add_directive("reqwest=warn".parse()?)
        .add_directive("matrix_sdk_crypto=error".parse()?);

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();

    Ok(())
}
