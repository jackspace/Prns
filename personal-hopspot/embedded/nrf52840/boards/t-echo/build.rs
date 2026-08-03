#[path = "../nrf_build.rs"]
mod nrf_build;

fn main() {
    nrf_build::link_memory_layout("../../memory-s140-7.x");
}
