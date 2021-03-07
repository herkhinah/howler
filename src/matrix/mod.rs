use std::{fs::File, io::BufReader, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use matrix_sdk::{Client, ClientConfig, RequestConfig, Session, SyncSettings};
pub use settings::BotSettings;

use self::{invites::InviteScheduler, sending::MessageSender};
use crate::{matrix::event_handler::HowlerEventHandler, settings::Settings};

pub mod channels;
pub mod renderer;
pub mod settings;

mod event_handler;
mod http_client;
mod invites;
mod sending;

pub async fn spawn_bots() -> Result<(MessageSender, InviteScheduler)> {
    let bot_settings = &Settings::global().bots;

    let mut bots = Vec::new();

    let mut client_user_ids = Vec::new();

    for settings in bot_settings.iter() {
        let client = build_client(settings)
            .await
            .context("failed to build client")?;

        let user_id = client.user_id().await.context("could not get user_id")?;
        client_user_ids.push(user_id);
        bots.push(client.clone());
    }

    let invite_scheduler = InviteScheduler::new(&bots).await;
    let message_scheduler = MessageSender::new(bots).await;

    Ok((message_scheduler, invite_scheduler))
}

pub async fn launch_howlers() -> Result<()> {
    let (mut message_scheduler, invite_scheduler) =
        spawn_bots().await.context("failed to launch bots")?;

    tokio::spawn(async move {
        invite_scheduler.run().await;
    });

    tokio::spawn(async move {
        message_scheduler.run().await;
    });

    Ok(())
}

async fn build_client(settings: &BotSettings) -> Result<Client> {
    let client = login_client(settings)
        .await
        .context(format!("login failed for {}", settings.user_id))?;

    let event_handler = Box::new(HowlerEventHandler::new(client.clone()).await?);

    client.set_event_handler(event_handler).await;

    tokio::spawn({
        let client = client.clone();
        async move {
            client.sync(SyncSettings::default()).await;
        }
    });

    Ok(client)
}

async fn login_client(settings: &BotSettings) -> Result<Client> {
    use matrix_sdk::ruma::api::client::r0::session::login::Response;

    let store_path = Settings::global().store_path.as_str();

    let mut path = PathBuf::from(store_path);
    path.push(settings.user_id.as_str());

    let client_config = ClientConfig::new()
        .passphrase(settings.password.clone())
        .store_path(&path)
        .request_config(RequestConfig::new().disable_retry())
        .client(Arc::new(http_client::Client::new(&settings.user_id)));

    let client = Client::new_with_config(settings.homeserver.clone(), client_config)
        .context("failed to create client")?;

    path.push("session.json");

    match File::open(path.as_path()) {
        Ok(f) => {
            let reader = BufReader::new(f);
            let session: Session =
                serde_json::from_reader(reader).context("corrupt session file")?;
            let _ = client
                .restore_login(session.clone())
                .await
                .context("unable to restore login")?;

            tracing::info!("restored login for {}", settings.user_id);
        }
        Err(_) => {
            let Response {
                access_token,
                user_id,
                device_id,
                ..
            } = client
                .login(settings.user_id.localpart(), &settings.password, None, None)
                .await
                .context("failed to login homeserver")?;

            tracing::info!("logged in as {}", user_id);

            let f = File::create(path.as_path())?;
            let writer = std::io::BufWriter::new(f);
            serde_json::to_writer(
                writer,
                &Session {
                    access_token,
                    user_id,
                    device_id,
                },
            )
            .context("could not write session to file")?;
        }
    };

    Ok(client)
}
