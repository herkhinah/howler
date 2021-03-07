#![feature(drain_filter)]
#![feature(unsized_tuple_coercion)]
#![feature(map_try_insert)]
#![feature(type_alias_impl_trait)]

use std::sync::{Arc, Barrier};

use anyhow::{Context, Result};

mod alert;

// not all modules have to be public
mod alertmanager_webhook_receiver;
mod log;
mod matrix;
mod room_tokens;
mod settings;
mod telemetry_endpoint;

async fn async_main() {
    log::setup_logging()
        .context("could not setup logging")
        .unwrap();

    telemetry_endpoint::init_telemetry();
    matrix::launch_howlers()
        .await
        .context("could not launch howlers")
        .unwrap();

    tokio::spawn(async {
        alertmanager_webhook_receiver::run_prometheus_receiver()
            .await
            .unwrap();
    });
}

fn setup_panic_handler(barrier: Arc<Barrier>) {
    let old_hook = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        old_hook(info);

        barrier.wait();
    }));
}

fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));

    setup_panic_handler(barrier.clone());

    runtime.block_on(async_main());

    barrier.wait();

    runtime.shutdown_background();

    Ok(())
}
