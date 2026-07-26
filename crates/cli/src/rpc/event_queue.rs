use coding_agent::api::event::CodingAgentProductEvent;
use tokio::sync::mpsc;

use super::limits::RPC_PRODUCT_EVENT_QUEUE_CAPACITY;

#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "bounded queue entries preserve ProductEvent ownership without another allocation"
)]
pub(super) enum RpcQueuedProductEvent {
    Event(CodingAgentProductEvent),
    Overflow { skipped: u64 },
}

#[derive(Clone)]
pub(super) struct RpcProductEventQueue {
    event_sender: mpsc::Sender<CodingAgentProductEvent>,
    control_sender: mpsc::Sender<RpcQueuedProductEvent>,
}

pub(super) struct RpcProductEventReceiver {
    event_receiver: mpsc::Receiver<CodingAgentProductEvent>,
    control_receiver: mpsc::Receiver<RpcQueuedProductEvent>,
}

impl RpcProductEventQueue {
    pub(super) fn new() -> (Self, RpcProductEventReceiver) {
        Self::with_capacity(RPC_PRODUCT_EVENT_QUEUE_CAPACITY)
    }

    fn with_capacity(capacity: usize) -> (Self, RpcProductEventReceiver) {
        let capacity = capacity.max(1);
        let (event_sender, event_receiver) = mpsc::channel(capacity);
        let (control_sender, control_receiver) = mpsc::channel(1);
        (
            Self {
                event_sender,
                control_sender,
            },
            RpcProductEventReceiver {
                event_receiver,
                control_receiver,
            },
        )
    }

    pub(super) async fn send_event(
        &self,
        event: CodingAgentProductEvent,
    ) -> Result<(), mpsc::error::SendError<CodingAgentProductEvent>> {
        self.event_sender.send(event).await
    }

    pub(super) async fn send_overflow(
        &self,
        skipped: u64,
    ) -> Result<(), mpsc::error::SendError<RpcQueuedProductEvent>> {
        self.control_sender
            .send(RpcQueuedProductEvent::Overflow { skipped })
            .await
    }
}

impl RpcProductEventReceiver {
    pub(super) async fn recv(&mut self) -> Option<RpcQueuedProductEvent> {
        if let Ok(item) = self.control_receiver.try_recv() {
            return Some(item);
        }
        tokio::select! {
            biased;
            control = self.control_receiver.recv() => control,
            event = self.event_receiver.recv() => event.map(RpcQueuedProductEvent::Event),
        }
    }

    pub(super) fn try_recv(&mut self) -> Result<RpcQueuedProductEvent, mpsc::error::TryRecvError> {
        match self.control_receiver.try_recv() {
            Ok(item) => Ok(item),
            Err(mpsc::error::TryRecvError::Empty) => self
                .event_receiver
                .try_recv()
                .map(RpcQueuedProductEvent::Event),
            Err(mpsc::error::TryRecvError::Disconnected) => self
                .event_receiver
                .try_recv()
                .map(RpcQueuedProductEvent::Event),
        }
    }
}
