//! Periodic log of the engine's ignore counters.
//!
//! Every packet the engine declines to act on is counted by reason, but the only exporter
//! for those counters is OpenTelemetry, so reading them has meant standing up a collector.
//! For diagnosing a link where traffic provably arrives and then disappears, the counters
//! are the whole answer and the collector is pure overhead. This writes them into prnsd's
//! own log instead, next to the events an operator is already reading.
//!
//! Only reasons with a non-zero count are printed, and only when a count has changed since
//! the previous sample, so a quiet node stays quiet.

use std::time::Duration;

use personal_rns::engine::IgnoreReasonCounts;
use personal_rns::runtime::PrnsNodeHandle;

/// Slow enough to be background noise, fast enough to bracket a manual send.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

/// Detached deliberately: the loop ends on its own when the node stops answering, so there
/// is nothing for the shutdown path to hold or cancel.
pub(crate) fn spawn(handle: PrnsNodeHandle) {
    tokio::spawn(run(handle));
}

async fn run(handle: PrnsNodeHandle) {
    let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut previous: Option<IgnoreReasonCounts> = None;
    loop {
        interval.tick().await;
        let Some(snapshot) = handle.metrics_snapshot().await else {
            return;
        };
        let current = snapshot.engine.ignored_packets;
        if let Some(line) = changed_line(previous.as_ref(), &current) {
            tracing::info!(event = "ignored_packets", counts = %line);
        }
        previous = Some(current);
    }
}

/// `None` when nothing moved, so an idle node does not fill the log. The reason names come
/// from the enum's own `Debug`, which keeps this in step with the variant list rather than
/// duplicating a table that could drift out of it.
fn changed_line(
    previous: Option<&IgnoreReasonCounts>,
    current: &IgnoreReasonCounts,
) -> Option<String> {
    let mut parts = Vec::new();
    for (reason, count) in current.iter() {
        if count == 0 {
            continue;
        }
        let before = previous.map_or(0, |previous| previous.get(reason));
        if previous.is_some() && before == count {
            continue;
        }
        parts.push(format!("{reason:?}={count}(+{})", count.saturating_sub(before)));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}
