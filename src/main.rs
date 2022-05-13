//! prometheus alertmanager receiver that forwards alerts into matrix rooms
//!
//! Features:
//! - forwards different webhook url paths into different matrix room
//! - per room configurable jinja2 templates (configured via state events)
//! - backup bots which also confirm if messages fedarate
#![feature(linked_list_cursors)]

use anyhow::{Context, Result};
use matrix::{bot::Bot, queuing};
use settings::Settings;
use tokio::sync::mpsc;

use crate::alert_renderer::AlertRenderer;

mod alert;
mod alert_renderer;
mod alertmanager_webhook_receiver;
mod allow_list;
mod log;
mod matrix;
mod pairmap;
mod pairset;
mod rendered_alert;
mod room_tokens;
mod settings;
mod telemetry_endpoint;

/// exit the complete program if one thread panics
fn setup_panic_handler() {
	let default_panic = std::panic::take_hook();
	std::panic::set_hook(Box::new(move |info| {
		default_panic(info);
		std::process::exit(1);
	}));
}

/// the entry point of the program
#[tokio::main]
pub async fn main() -> Result<()> {
	setup_panic_handler();

	log::setup_logging().context("could not setup logging")?;

	let (tx_queue, rx_queue) = mpsc::channel(64);
	let (tx_renderer, rx_renderer) = mpsc::channel(64);

	let bots = {
		let bot_settings = &Settings::global().bots;
		let mut bots = Vec::new();

		let mut backup = false;
		for settings in bot_settings.iter() {
			let bot = Bot::new(settings, tx_renderer.clone(), tx_queue.clone(), backup)
				.await
				.context(format!("failed to spawn bot {}", settings.user_id))?;

			bots.push(bot);

			backup = true;
		}

		bots
	};

	let alert_renderer =
		AlertRenderer::new(tx_queue.clone()).context("failed to construct alert renderer")?;

	tokio::spawn(alert_renderer.run(rx_renderer));

	#[allow(clippy::expect_used)]
	tokio::spawn(queuing::run(rx_queue, &bots).expect("alert queue crashed"));

	for bot in bots.into_iter() {
		tokio::spawn(bot.run());
	}

	tokio::spawn(async {
		#[allow(clippy::expect_used)]
		alertmanager_webhook_receiver::run_prometheus_receiver(tx_renderer)
			.await
			.expect("prometheus alertmanager receiver endpoint failed to start or crashed");
	});

	telemetry_endpoint::run_telemetry_endpoint().await;

	Ok(())
}
