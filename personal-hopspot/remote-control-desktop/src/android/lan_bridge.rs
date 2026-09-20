use std::sync::{Condvar, Mutex};

use personal_rns::wifi_auto::{AutoWifiStatus, HostLanInventory};

pub struct AndroidLanBridge {
    inventory: HostLanInventory,
    wifi_status: Mutex<Option<AutoWifiStatus>>,
    wait: Condvar,
}

impl AndroidLanBridge {
    pub fn new() -> Self {
        Self {
            inventory: HostLanInventory::new(),
            wifi_status: Mutex::new(None),
            wait: Condvar::new(),
        }
    }

    pub fn inventory(&self) -> HostLanInventory {
        self.inventory.clone()
    }

    pub fn attach_wifi_status(&self, status: AutoWifiStatus) {
        if let Ok(mut wifi_status) = self.wifi_status.lock() {
            *wifi_status = Some(status);
        }
        self.wait.notify_all();
    }

    pub fn should_hold_multicast_lock(&self) -> bool {
        self.wifi_status
            .lock()
            .ok()
            .and_then(|wifi_status| wifi_status.as_ref().map(AutoWifiStatus::is_enabled))
            .unwrap_or(false)
    }

    pub fn wait_for_work(&self, timeout: std::time::Duration) {
        let Ok(guard) = self.wifi_status.lock() else {
            return;
        };
        let _ = self.wait.wait_timeout(guard, timeout);
    }
}

impl Default for AndroidLanBridge {
    fn default() -> Self {
        Self::new()
    }
}
