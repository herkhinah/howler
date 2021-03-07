//! This module handles queuing, batching, sending and confirmation of alerts.
//! Alerts received from the webhook receiver are rendered via [MessageRenderer::render_alert] queued into [BatchQueue].
//! After a first alert was received for a specific room, the queue waits a certain amount of time for other alerts for the same room to arrive and batches them together. The batched message can then be received via the [futures::stream::Stream] interface of [BatchQueue].
//! The batched message is then sent via [Dispatcher] by one of the bots and after it was sent successfully a certain amount of time is waited for another bot to confirm the message. This ensures that messages are properly federated.
//! [Dispatcher] implements [futures::stream::Stream] to return messages which failed to get confirmed after the specified confirmation timeout interval.

use std::{iter, pin::Pin, time::Duration};

use futures::StreamExt;
use matrix_sdk::Client;
use tokio::time::Instant;

use self::{
    dispatcher::Dispatcher,
    queuing::{BatchQueue, UnconfirmedMessageQueue},
};
use crate::{
    matrix::{
        channels::{AlertChannel, EventIdChannel, OnceCellMPSC, TemplateChannel},
        renderer::MessageRenderer,
        sending::batch_entries::BatchEntry,
    },
    settings::Settings,
};

pub mod batch_entries;
pub mod bot;
pub mod dispatcher;
pub mod queuing;

pub struct MessageSender {
    dispatcher: Pin<Box<Dispatcher>>,

    batch_duration: Duration,
}

impl MessageSender {
    pub async fn new(clients: Vec<Client>) -> Self {
        Self {
            dispatcher: Box::pin(Dispatcher::new(clients).await),
            batch_duration: Settings::global().batch_interval,
        }
    }

    /// main event loop, incoming alerts are rendered and queued
    pub async fn run(&mut self) {
        #[allow(unused_mut)]
        let mut alert_queue = BatchQueue::new(self.batch_duration);
        tokio::pin!(alert_queue);

        #[allow(unused_mut)]
        let mut unconfirmed_message_queue = UnconfirmedMessageQueue::new();
        tokio::pin!(unconfirmed_message_queue);

        let mut rx_confirmation = EventIdChannel::receiver().await.unwrap();
        let mut rx_alert = AlertChannel::receiver().await.unwrap();
        let mut rx_template = TemplateChannel::receiver().await.unwrap();

        let mut renderer = MessageRenderer::new().unwrap();

        loop {
            tokio::select!(
                // sent message failed to confirm in specified time interval
                Some(unconfirmed) = unconfirmed_message_queue.next() => {
                    // backoff sending bot
                    self.dispatcher.federation_backoff(unconfirmed.bot_id);
                    // requeue batch entries
                    alert_queue.queue(unconfirmed.target_room, unconfirmed.batch_entries);
                }
                // bot received an event_id from another bot
                Some(event_id) = rx_confirmation.recv() => {
                    // try confirming message
                    if let Some(bot_id) = unconfirmed_message_queue.confirm_message(&event_id) {
                        // there was an unconfirmed message that was still waiting for confirmation
                        // remove federation backoff if existent
                        self.dispatcher.reset_federation_backoff(&bot_id);
                    }
                }
                // receive batched message from batch queue
                Some(message) = alert_queue.next() => {
                    // try to send message via a bot
                    if let Err((target_room, entries)) = self.dispatcher.send(message) {
                        // if it failed to send, requeue batch entries
                        alert_queue.queue(target_room, entries);
                    }
                }
                // bot finished sending a message
                Some(res) = self.dispatcher.next() => {
                    match res {
                        // sending was successfull
                        Ok(unconfirmed_message) => {
                            // wait for confirmation from another bot
                            unconfirmed_message_queue.queue(unconfirmed_message);
                        },
                        // sending failed
                        Err((room_id, entries)) => {
                            // requeue batch entries
                            alert_queue.queue(room_id, entries);
                        }
                    }
                }
                // received alert from prometheus webhook receiver
                Some((room_id, alert, arrival)) = rx_alert.recv() => {
                    // render alert
                    let batch_entry = BatchEntry::new(arrival, renderer.render_alert(&room_id, alert));

                    // queue rendered alert (or rendering error message)
                    alert_queue.queue(room_id, iter::once(batch_entry));
                }
                // alert rendering templates changed
                Some((event_id, room_id, template)) = rx_template.recv() => {
                    // try registering new templkates
                    if let Err(content) = renderer.register_template(event_id, &room_id, template) {
                        // switching to new templates failed. queue error message
                        let batch_entry = BatchEntry::new(Instant::now(), content);
                        alert_queue.queue(room_id, iter::once(batch_entry));
                    }
                }
                else => {
                    panic!("message_queue async loop error");
                }
            );
        }
    }
}
