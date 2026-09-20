mod jni_ble;
mod jni_usb;
mod jni_wifi;
mod lan_bridge;
mod usb_bridge;

use std::sync::OnceLock;

use prns_ffi::bluetooth_auto::android::AndroidBleBridge;

use lan_bridge::AndroidLanBridge;
use usb_bridge::AndroidUsbBridge;

const ANDROID_USB_PORT: &str = "android-usb";

pub struct AndroidPlatform {
    pub ble: AndroidBleBridge,
    pub usb: AndroidUsbBridge,
    pub lan: AndroidLanBridge,
}

impl AndroidPlatform {
    fn new() -> Self {
        Self {
            ble: AndroidBleBridge::new(),
            usb: AndroidUsbBridge::new(),
            lan: AndroidLanBridge::new(),
        }
    }
}

static PLATFORM: OnceLock<AndroidPlatform> = OnceLock::new();

pub fn platform() -> &'static AndroidPlatform {
    PLATFORM.get_or_init(AndroidPlatform::new)
}

pub fn ble_bridge() -> AndroidBleBridge {
    platform().ble.clone()
}

pub fn usb_bridge() -> AndroidUsbBridge {
    platform().usb.clone()
}

pub fn lan_bridge() -> &'static AndroidLanBridge {
    &platform().lan
}

pub fn android_usb_port() -> &'static str {
    ANDROID_USB_PORT
}
