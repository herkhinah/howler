//! Channels mainly used for communication between the [EventHandlers][crate::matrix::event_handler::HowlerEventHandler] and [MessageSender][crate::matrix::sending::MessageSender] or [InviteScheduler][crate::matrix::invites::InviteScheduler]
//! In my opinion this looks cleaner than passing around lots of channels between functions during initialization.

use matrix_sdk::{
    async_trait,
    ruma::{EventId, RoomId, UserId},
};
use once_cell::sync::OnceCell;
use tokio::{
    sync::{mpsc, Mutex},
    time::Instant,
};

use super::{invites::Invite, renderer::Template};
use crate::alert;

type MpscOnceCellInner<T> = (mpsc::Sender<T>, Mutex<Option<mpsc::Receiver<T>>>);
type MpscOnceCell<T> = OnceCell<MpscOnceCellInner<T>>;

mod private {
    use tokio::sync::{mpsc, Mutex};

    use super::{MpscOnceCell, MpscOnceCellInner};

    pub trait OnceCellMPSC<'a> {
        type Output: 'static + Send + Sized;

        fn get() -> &'a MpscOnceCell<Self::Output>;

        fn get_or_init() -> &'a MpscOnceCellInner<Self::Output> {
            Self::get().get_or_init(|| {
                let (tx, rx) = mpsc::channel(64);

                (tx, Mutex::new(Some(rx)))
            })
        }
    }
}

#[async_trait]
pub trait OnceCellMPSC<'a>: private::OnceCellMPSC<'a> {
    fn sender() -> mpsc::Sender<Self::Output> {
        Self::get_or_init().0.clone()
    }

    async fn receiver() -> Option<mpsc::Receiver<Self::Output>> {
        Self::get_or_init().1.lock().await.take()
    }
}

static INVITE_CHANNEL: MpscOnceCell<Invite> = OnceCell::new();
/// Invite channel.
pub struct InviteChannel();

impl private::OnceCellMPSC<'static> for InviteChannel {
    type Output = Invite;

    fn get() -> &'static MpscOnceCell<Self::Output> {
        &INVITE_CHANNEL
    }
}

impl OnceCellMPSC<'static> for InviteChannel {}

static EVENT_ID_CHANNEL: MpscOnceCell<EventId> = OnceCell::new();
/// Channel used for sending [EventId]s between [MessageSender][crate::matrix::sending::MessageSender] and [HowlerEventHandler][crate::matrix::event_handler::HowlerEventHandler] to confirm the federation of sent messages.
pub struct EventIdChannel();

impl private::OnceCellMPSC<'static> for EventIdChannel {
    type Output = EventId;

    fn get() -> &'static MpscOnceCell<Self::Output> {
        &EVENT_ID_CHANNEL
    }
}

impl OnceCellMPSC<'static> for EventIdChannel {}

static JOIN_CHANNEL: MpscOnceCell<(UserId, RoomId)> = OnceCell::new();
/// Channel used for notifying [InviteScheduler][crate::matrix::invites::InviteScheduler] that a bot has joined a room
pub struct JoinChannel();

impl private::OnceCellMPSC<'static> for JoinChannel {
    type Output = (UserId, RoomId);

    fn get() -> &'static MpscOnceCell<Self::Output> {
        &JOIN_CHANNEL
    }
}

impl OnceCellMPSC<'static> for JoinChannel {}

static TEMPLATE_CHANNEL: MpscOnceCell<(EventId, RoomId, Template)> = OnceCell::new();
/// Channel used for registering new templates. Templates are registered via a custom "com.famedly.howler.template" room event.
pub struct TemplateChannel();

impl private::OnceCellMPSC<'static> for TemplateChannel {
    type Output = (EventId, RoomId, Template);

    fn get() -> &'static MpscOnceCell<Self::Output> {
        &TEMPLATE_CHANNEL
    }
}

impl OnceCellMPSC<'static> for TemplateChannel {}

static ALERT_CHANNEL: MpscOnceCell<(RoomId, alert::Data, Instant)> = OnceCell::new();
/// Channel used for sending received alerts from the prometheus webhook receiver to [MessageSender][crate::matrix::sending::MessageSender].
pub struct AlertChannel();

impl private::OnceCellMPSC<'static> for AlertChannel {
    type Output = (RoomId, alert::Data, Instant);

    fn get() -> &'static MpscOnceCell<Self::Output> {
        &ALERT_CHANNEL
    }
}

impl OnceCellMPSC<'static> for AlertChannel {}
