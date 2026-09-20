use core::cell::Cell;
use core::sync::atomic::{AtomicU8, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use personal_rns::engine::{NetworkTransport, PrnsCommand};
use personal_rns::remote_control::{
    RemoteControlNetworkTransport, RemoteControlNetworkTransportOutcome,
};

/// Shared gate so display-core Remote Control can describe/set transport
/// while the manifold core applies [`PrnsCommand::SetNetworkTransport`].
pub struct HopspotNetworkTransportGate {
    current: AtomicU8,
    pending: BlockingMutex<CriticalSectionRawMutex, Cell<Option<NetworkTransport>>>,
}

impl HopspotNetworkTransportGate {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            current: AtomicU8::new(RemoteControlNetworkTransport::Enabled as u8),
            pending: BlockingMutex::new(Cell::new(None)),
        }
    }

    #[must_use]
    pub fn current(&self) -> RemoteControlNetworkTransport {
        RemoteControlNetworkTransport::from_wire(self.current.load(Ordering::Relaxed))
            .unwrap_or(RemoteControlNetworkTransport::Disabled)
    }

    pub fn set(
        &self,
        transport: RemoteControlNetworkTransport,
    ) -> RemoteControlNetworkTransportOutcome {
        let network = match transport {
            RemoteControlNetworkTransport::Enabled => NetworkTransport::Enabled,
            RemoteControlNetworkTransport::Disabled => NetworkTransport::Disabled,
        };
        self.current.store(transport as u8, Ordering::Relaxed);
        self.pending.lock(|cell| cell.set(Some(network)));
        RemoteControlNetworkTransportOutcome::Applied
    }

    pub fn take_pending(&self) -> Option<NetworkTransport> {
        self.pending.lock(|cell| cell.replace(None))
    }
}

impl Default for HopspotNetworkTransportGate {
    fn default() -> Self {
        Self::new()
    }
}

pub static NETWORK_TRANSPORT: HopspotNetworkTransportGate = HopspotNetworkTransportGate::new();

pub fn apply_pending_network_transport(mut issue: impl FnMut(PrnsCommand)) {
    if let Some(network) = NETWORK_TRANSPORT.take_pending() {
        issue(PrnsCommand::SetNetworkTransport(network));
    }
}
