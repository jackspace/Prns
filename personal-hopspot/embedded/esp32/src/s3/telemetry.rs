use super::*;

use personal_rns::wire::DestinationHash;

/// How long after boot the first beacon waits: enough for the radios to come up and the routing
/// table to refill, short enough that a freshly placed node shows up within minutes.
const FIRST_BEACON_DELAY: Duration = Duration::from_secs(120);

/// The steady beacon cadence. Each beacon is one announce-sized packet on every interface, so at
/// LoRa rates this constant is the whole airtime budget of the feature.
const BEACON_INTERVAL: Duration = Duration::from_secs(600);

const BATTERY_TAG_UNKNOWN: u32 = 0;
const BATTERY_TAG_LEVEL: u32 = 1;
const BATTERY_TAG_CHARGING: u32 = 2;
const BATTERY_TAG_SHIFT: u32 = 8;
const BATTERY_PERCENT_MASK: u32 = 0xFF;

/// The freshest render-loop readings, held as atomics so the render loop (single writer) and the
/// beacon task (single reader) share one `&'static` without locking.
pub(super) struct TelemetryShared {
    battery: AtomicU32,
    reachable_destinations: AtomicU32,
}

pub(super) static TELEMETRY_SHARED: TelemetryShared = TelemetryShared::new();

impl TelemetryShared {
    const fn new() -> Self {
        Self {
            battery: AtomicU32::new(BATTERY_TAG_UNKNOWN << BATTERY_TAG_SHIFT),
            reachable_destinations: AtomicU32::new(0),
        }
    }

    /// Record one render tick's view: the smoothed battery state and the interface snapshots whose
    /// routing-table destination counts sum into the beacon's reachable figure.
    pub(super) fn record(&self, battery: screen::BatteryState, snapshots: &[InterfaceSnapshot]) {
        let encoded = match battery {
            screen::BatteryState::Unknown => BATTERY_TAG_UNKNOWN << BATTERY_TAG_SHIFT,
            screen::BatteryState::Level(percent) => {
                (BATTERY_TAG_LEVEL << BATTERY_TAG_SHIFT) | u32::from(percent.get())
            }
            screen::BatteryState::Charging(percent) => {
                (BATTERY_TAG_CHARGING << BATTERY_TAG_SHIFT) | u32::from(percent.get())
            }
        };
        self.battery.store(encoded, Ordering::Relaxed);
        let reachable = snapshots.iter().fold(0u32, |sum, snapshot| {
            sum.saturating_add(snapshot.destinations)
        });
        self.reachable_destinations.store(reachable, Ordering::Relaxed);
    }

    fn battery(&self) -> screen::BatteryState {
        let encoded = self.battery.load(Ordering::Relaxed);
        let percent = screen::BatteryPercent::saturating((encoded & BATTERY_PERCENT_MASK) as u8);
        match encoded >> BATTERY_TAG_SHIFT {
            BATTERY_TAG_LEVEL => screen::BatteryState::Level(percent),
            BATTERY_TAG_CHARGING => screen::BatteryState::Charging(percent),
            _ => screen::BatteryState::Unknown,
        }
    }

    fn reading(&self) -> screen::TelemetryReading {
        screen::TelemetryReading {
            battery: self.battery(),
            uptime_seconds: uptime_seconds(),
            reachable_destinations: self.reachable_destinations.load(Ordering::Relaxed),
        }
    }
}

fn uptime_seconds() -> u32 {
    embassy_time::Instant::now()
        .as_secs()
        .min(u64::from(u32::MAX)) as u32
}

/// Re-announce the delivery destination on a fixed cadence with the telemetry element appended,
/// so a listener already watching `lxmf.delivery` announces learns this node's health without
/// polling it.
#[embassy_executor::task]
pub(super) async fn telemetry_beacon_task(
    handle: Handle,
    destination: DestinationHash,
    registered_app_data: &'static [u8],
) -> ! {
    Timer::after(FIRST_BEACON_DELAY).await;
    let mut cadence = Ticker::every(BEACON_INTERVAL);
    loop {
        let reading = TELEMETRY_SHARED.reading();
        match screen::delivery_app_data_with_telemetry(registered_app_data, &reading) {
            Ok(app_data) => {
                let queued = handle.issue(PrnsCommand::AnnounceNow(AnnounceNow {
                    destination,
                    target: AnnounceTarget::AllInterfaces,
                    app_data: AnnounceAppData::Data(app_data),
                }));
                log::info!(
                    "telemetry-beacon queued={} uptime_s={} reachable={}",
                    queued.is_some(),
                    reading.uptime_seconds,
                    reading.reachable_destinations
                );
            }
            Err(error) => log::error!("telemetry-beacon app_data rejected: {error:?}"),
        }
        cadence.next().await;
    }
}
