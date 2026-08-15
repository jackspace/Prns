use personal_rns::interfaces::rns_management::{RnsInterfaceVitalsEntry, RnsInterfaceVitalsReport};
use serde_json::{Map, Value};

/// One object per poll, so a cron entry appends it straight to a JSONL timeline.
pub fn render(report: &RnsInterfaceVitalsReport) -> Result<String, serde_json::Error> {
    let mut fields = Map::new();
    fields.insert(
        String::from("interfaces"),
        Value::Array(report.entries().iter().map(entry_value).collect()),
    );
    serde_json::to_string(&Value::Object(fields))
}

fn entry_value(entry: &RnsInterfaceVitalsEntry) -> Value {
    let vitals = &entry.vitals;
    let mut fields = Map::new();
    fields.insert(String::from("name"), Value::String(entry.name.clone()));
    fields.insert(String::from("id"), Value::String(hex(vitals.id.as_bytes())));
    fields.insert(
        String::from("connection"),
        Value::String(format!("{:?}", vitals.connection)),
    );
    fields.insert(
        String::from("failure_reason"),
        match vitals.failure_reason {
            Some(reason) => Value::String(String::from(reason)),
            None => Value::Null,
        },
    );
    fields.insert(String::from("rx_bytes"), vitals.rx_bytes.into());
    fields.insert(String::from("tx_bytes"), vitals.tx_bytes.into());
    fields.insert(
        String::from("rx_bps"),
        match vitals.transfer_rates {
            Some(rates) => rates.rx_bps.into(),
            None => Value::Null,
        },
    );
    fields.insert(
        String::from("tx_bps"),
        match vitals.transfer_rates {
            Some(rates) => rates.tx_bps.into(),
            None => Value::Null,
        },
    );
    // Null, not a zeroed object. A family that does not account for frames has said nothing
    // about them, and a reader that cannot tell that apart from a silent link is back to the
    // ambiguity this command was built to remove.
    fields.insert(
        String::from("frames"),
        match vitals.frames {
            Some(frames) => {
                let mut counters = Map::new();
                counters.insert(String::from("frames_in"), frames.frames_in.into());
                counters.insert(String::from("frames_out"), frames.frames_out.into());
                counters.insert(String::from("malformed"), frames.malformed.into());
                counters.insert(String::from("undecodable"), frames.undecodable.into());
                counters.insert(String::from("delivered"), frames.delivered.into());
                Value::Object(counters)
            }
            None => Value::Null,
        },
    );
    // Present only on a row relayed from another node, where it dates the sample against
    // that node's own clock rather than against the moment this poll happened to run.
    fields.insert(
        String::from("uptime_ms"),
        match vitals.uptime_ms {
            Some(uptime_ms) => uptime_ms.into(),
            None => Value::Null,
        },
    );
    // Same clock as uptime_ms, stamped when the interface last accepted an inbound frame.
    // uptime_ms - last_frame_in_at_ms is the age of the newest arrival from this one sample,
    // which is the number the frame counters alone can never give.
    fields.insert(
        String::from("last_frame_in_at_ms"),
        match vitals.last_frame_in_at_ms {
            Some(at_ms) => at_ms.into(),
            None => Value::Null,
        },
    );
    Value::Object(fields)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests;
