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
    let mut previous_ingested: Option<u64> = None;
    // (submitted, completed, verdicts_owed, backpressure_deferrals)
    let mut previous_crypto: Option<(u64, u64, u32, u64)> = None;
    let mut reported_pool_absent = false;
    loop {
        interval.tick().await;
        let Some(snapshot) = handle.metrics_snapshot().await else {
            return;
        };

        // Reported separately from the ignore reasons, and this separation is the whole point.
        // An ignore count that stays flat has two very different causes: the engine looked at the
        // packet and had no reason to drop it, or the packet never reached the engine at all.
        // Only `ingested` tells those apart, and on a multi-hop link the second case is what a
        // silent hop looks like from here.
        let ingested = snapshot.engine.ingested_packets;
        if previous_ingested != Some(ingested) {
            let delta = ingested.saturating_sub(previous_ingested.unwrap_or(0));
            tracing::info!(event = "ingested_packets", total = ingested, delta = delta);
        }
        previous_ingested = Some(ingested);

        // The deferred-crypto window is the one place a Single-destination packet can be
        // ingested, classified as nothing, and never delivered. These counters bracket it:
        // submitted-minus-completed growing means jobs are stuck in the pool, while matched
        // totals alongside a missing delivery means the loss is in the resume path.
        match snapshot.crypto {
            Some(crypto) => {
                let key = (
                    crypto.submitted_jobs,
                    crypto.completed_jobs,
                    crypto.packet_verdicts_owed,
                    crypto.backpressure_deferrals,
                );
                if previous_crypto != Some(key) {
                    tracing::info!(
                        event = "crypto_jobs",
                        submitted = crypto.submitted_jobs,
                        completed = crypto.completed_jobs,
                        outstanding = crypto.submitted_jobs.saturating_sub(crypto.completed_jobs),
                        queue_depth = crypto.queue_depth,
                        max_queue_depth = crypto.maximum_queue_depth,
                        verdicts_owed = crypto.packet_verdicts_owed,
                        backpressure_deferrals = crypto.backpressure_deferrals,
                    );
                    previous_crypto = Some(key);
                }
            }
            None => {
                // Reported once. If there is no pool the driver ingests inline and there is no
                // deferred window at all, which would falsify the whole hypothesis.
                if !reported_pool_absent {
                    reported_pool_absent = true;
                    tracing::info!(event = "crypto_jobs", pool = "absent");
                }
            }
        }

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
    render(current.iter().map(|(reason, count)| {
        let before = previous.map_or(0, |previous| previous.get(reason));
        (format!("{reason:?}"), count, before)
    }))
}

/// Split out from the counter type so the suppression rules can be tested directly. The
/// counters themselves can only be advanced from inside the engine, so a test that had to
/// build a populated `IgnoreReasonCounts` could not exercise any of this.
///
/// `had_previous` is carried per entry as `before`, with a first sample passing 0, because the
/// first sample after start must print a non-zero counter rather than suppress it as unchanged.
fn render(entries: impl Iterator<Item = (String, u64, u64)>) -> Option<String> {
    let mut parts = Vec::new();
    for (name, count, before) in entries {
        if count == 0 || count == before {
            continue;
        }
        parts.push(format!("{name}={count}(+{})", count.saturating_sub(before)));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::render;

    fn line(entries: &[(&str, u64, u64)]) -> Option<String> {
        render(
            entries
                .iter()
                .map(|(name, count, before)| ((*name).to_string(), *count, *before)),
        )
    }

    #[test]
    fn a_quiet_node_prints_nothing() {
        assert_eq!(line(&[("NotForUs", 0, 0), ("OtherInstance", 0, 0)]), None);
    }

    /// The whole point: a counter that has not moved must not reappear every ten seconds, or
    /// the log fills and the operator stops reading it.
    #[test]
    fn an_unchanged_counter_is_suppressed() {
        assert_eq!(line(&[("NotForUs", 14, 14)]), None);
    }

    #[test]
    fn a_moving_counter_prints_its_total_and_delta() {
        assert_eq!(
            line(&[("NotForUs", 14, 3)]),
            Some(String::from("NotForUs=14(+11)"))
        );
    }

    /// A non-zero counter on the first sample has `before` 0, and must print rather than be
    /// mistaken for unchanged — otherwise a daemon that was already dropping traffic before the
    /// logger started would look silent.
    #[test]
    fn the_first_sample_prints_an_already_nonzero_counter() {
        assert_eq!(
            line(&[("OtherInstance", 27, 0)]),
            Some(String::from("OtherInstance=27(+27)"))
        );
    }

    #[test]
    fn only_the_reasons_that_moved_appear() {
        assert_eq!(
            line(&[
                ("Duplicate", 5, 5),
                ("NotForUs", 12, 1),
                ("NoRoute", 0, 0),
                ("OtherInstance", 3, 2),
            ]),
            Some(String::from("NotForUs=12(+11) OtherInstance=3(+1)"))
        );
    }
}
