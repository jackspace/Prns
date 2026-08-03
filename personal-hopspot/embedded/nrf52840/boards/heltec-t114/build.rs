#[path = "../nrf_build.rs"]
mod nrf_build;

fn main() {
    // The T114 runs Prns only after re-bootloadering to S140 7.3.0, at which point its
    // application base is 0x27000 and it shares the T-Echo's layout. The stock bootloader's
    // S140 6.1.1 base of 0x26000 is unusable here: nrf-softdevice binds S140 v7 only.
    nrf_build::link_memory_layout("../../memory-s140-7.x");
}
