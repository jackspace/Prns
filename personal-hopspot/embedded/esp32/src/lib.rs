#![no_std]
#![cfg_attr(target_arch = "xtensa", feature(asm_experimental_arch))]

extern crate alloc;

#[cfg(all(target_arch = "xtensa", not(feature = "usb")))]
compile_error!(
    "ESP32-S3 firmware is built through a board package, which always selects usb (plus lora, bluetooth-auto, wifi-auto, tcp, and esp-now on boards that carry those radios)"
);

#[cfg(all(
    target_arch = "riscv32",
    not(all(
        feature = "bluetooth-auto",
        feature = "esp-now",
        feature = "usb",
        not(feature = "lora"),
        not(feature = "tcp"),
        not(feature = "wifi-auto")
    ))
))]
compile_error!(
    "ESP32-C6 firmware is built through its board package, which selects bluetooth-auto, esp-now, and usb"
);

#[cfg(all(
    feature = "bluetooth-auto",
    any(target_arch = "riscv32", target_arch = "xtensa")
))]
pub mod bluetooth_auto;
#[cfg(all(
    target_arch = "riscv32",
    feature = "bluetooth-auto",
    feature = "esp-now",
    feature = "usb",
    not(feature = "lora"),
    not(feature = "tcp"),
    not(feature = "wifi-auto")
))]
pub mod c6;
#[cfg(any(target_arch = "riscv32", target_arch = "xtensa"))]
mod flash;
#[cfg(any(target_arch = "riscv32", target_arch = "xtensa"))]
mod identity;
#[cfg(any(target_arch = "riscv32", target_arch = "xtensa"))]
mod persistence;
#[cfg(all(target_arch = "xtensa", feature = "usb"))]
pub mod s3;
#[cfg(all(any(test, target_arch = "xtensa"), feature = "wifi-auto"))]
mod station_recovery;
#[cfg(any(target_arch = "riscv32", target_arch = "xtensa"))]
mod storage;
#[cfg(all(any(test, target_arch = "xtensa"), feature = "wifi-auto"))]
mod wifi_data_path_recovery;
