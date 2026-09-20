#[cfg(target_os = "android")]
mod android;
mod app;
mod backend;
mod edits;
#[cfg(not(target_os = "android"))]
mod flash;
mod identity_clone;
mod roster_sync;

pub fn launch_controller() {
    init_controller_logging();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let _enter = runtime.enter();

    #[cfg(all(feature = "mobile", target_os = "android"))]
    {
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Debug)
                .with_filter(
                    android_logger::FilterBuilder::new()
                        .parse("warn,prns_interfaces_tokio::wifi_auto=debug,prns_ffi::mdns=debug")
                        .build(),
                ),
        );
    }

    #[cfg(feature = "desktop")]
    {
        use dioxus::desktop::{Config, WindowBuilder};
        dioxus::LaunchBuilder::desktop()
            .with_cfg(
                Config::new().with_window(
                    WindowBuilder::new()
                        .with_title("PRNS Controller")
                        .with_always_on_top(false),
                ),
            )
            .launch(app::App);
    }

    #[cfg(feature = "mobile")]
    {
        dioxus::LaunchBuilder::mobile().launch(app::App);
    }
}

fn init_controller_logging() {
    #[cfg(not(target_os = "android"))]
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        // Prefer Wi-Fi Auto / mDNS over BLE and runtime mesh chatter while debugging LL discovery.
        // Override with RUST_LOG. BLE wire stderr is opt-in via PRNS_BLE_WIRE_LOG=1.
        let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new(
                "warn,prns_interfaces_tokio::wifi_auto=debug,prns_ffi::mdns=debug",
            )
        });
        let _ = tracing_log::LogTracer::init();
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .try_init();
    }
}
