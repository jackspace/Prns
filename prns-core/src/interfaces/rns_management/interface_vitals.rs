use alloc::string::String;
use alloc::vec::Vec;

use rmp::Marker;

use crate::interfaces::{
    ConnectionState, FrameAccounting, InterfaceId, InterfaceVitals, TransferRates, INTERFACE_ID_LEN,
};

use super::message_pack::{MessagePackEncoder, MessagePackInteger, MessagePackReader};
use super::wire_names::{interface, vitals};
use super::{interface_name, RnsManagementEncodeError};

use RnsInterfaceVitalsDecodeError as Error;

/// Every interface's full `InterfaceVitals`, carried without the `InterfaceSnapshot`
/// squeeze that `interface_stats` applies. The squeeze drops `frames` and `uptime_ms`,
/// which are the two fields that tell "nothing arrived" apart from "arrived and was
/// discarded", so a caller that needs them needs its own verb rather than a wider
/// snapshot.
///
/// This is a Prns-native report. Stock RNS never asks for it, so the shape is chosen
/// for decodability rather than for compatibility with a Python peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RnsInterfaceVitalsReport {
    entries: Vec<RnsInterfaceVitalsEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RnsInterfaceVitalsEntry {
    pub name: String,
    pub vitals: InterfaceVitals,
}

/// Each entry is a fixed-width map, so the decoder never has to tolerate an absent key.
/// Optional values are carried as nil rather than as a missing field, because the
/// difference between "this family does not account for frames" and "it accounts and
/// counted zero" is the whole point of the report.
const ENTRY_FIELDS: usize = 10;

impl RnsInterfaceVitalsReport {
    pub fn new(entries: Vec<RnsInterfaceVitalsEntry>) -> Self {
        Self { entries }
    }

    pub fn of(inventory: impl IntoIterator<Item = (Option<String>, InterfaceVitals)>) -> Self {
        Self::new(
            inventory
                .into_iter()
                .map(|(name, vitals)| RnsInterfaceVitalsEntry {
                    name: name.unwrap_or_else(|| interface_name(vitals.id)),
                    vitals,
                })
                .collect(),
        )
    }

    #[must_use]
    pub fn entries(&self) -> &[RnsInterfaceVitalsEntry] {
        &self.entries
    }

    pub fn encode_message_pack(&self) -> Result<Vec<u8>, RnsManagementEncodeError> {
        let mut encoder = MessagePackEncoder::new();
        self.encode_into(&mut encoder)?;
        Ok(encoder.finish())
    }

    pub(crate) fn encode_into(
        &self,
        encoder: &mut MessagePackEncoder,
    ) -> Result<(), RnsManagementEncodeError> {
        encoder.map(1)?;
        encoder.field(interface::INTERFACES)?;
        encoder.array(self.entries.len())?;
        for entry in &self.entries {
            encode_entry(encoder, entry)?;
        }
        Ok(())
    }

    pub fn decode_message_pack(bytes: &[u8]) -> Result<Self, RnsInterfaceVitalsDecodeError> {
        let mut reader = MessagePackReader::new(bytes);
        let marker = reader.marker().map_err(|_| Error::InvalidMessagePack)?;
        if reader
            .map_length(marker)
            .map_err(|_| Error::InvalidMessagePack)?
            != Some(1)
        {
            return Err(Error::ExpectedReportMap);
        }

        let marker = reader.marker().map_err(|_| Error::InvalidMessagePack)?;
        let key = reader
            .string(marker)
            .map_err(|_| Error::InvalidMessagePack)?
            .ok_or(Error::ExpectedReportMap)?;
        if key != interface::INTERFACES {
            return Err(Error::UnknownField(String::from(key)));
        }

        let marker = reader.marker().map_err(|_| Error::InvalidMessagePack)?;
        let length = reader
            .array_length(marker)
            .map_err(|_| Error::InvalidMessagePack)?
            .ok_or(Error::ExpectedInterfacesArray)?;

        let mut entries = Vec::new();
        entries
            .try_reserve(length)
            .map_err(|_| Error::AllocationFailed { entries: length })?;
        for index in 0..length {
            entries.push(decode_entry(&mut reader, index)?);
        }

        if !reader.is_finished() {
            return Err(Error::TrailingData);
        }
        Ok(Self::new(entries))
    }
}

fn encode_entry(
    encoder: &mut MessagePackEncoder,
    entry: &RnsInterfaceVitalsEntry,
) -> Result<(), RnsManagementEncodeError> {
    let vitals = &entry.vitals;
    encoder.map(ENTRY_FIELDS)?;
    encoder.string_field(interface::NAME, &entry.name)?;
    encoder.field(vitals::ID)?;
    encoder.binary(vitals.id.as_bytes())?;
    encoder.unsigned_field(interface::STATUS, u64::from(vitals.connection.as_u8()))?;
    encoder.field(vitals::FAILURE_REASON)?;
    match vitals.failure_reason {
        Some(reason) => encoder.string(reason)?,
        None => encoder.nil(),
    }
    encoder.unsigned_field(interface::RECEIVE_BYTES, vitals.rx_bytes)?;
    encoder.unsigned_field(interface::TRANSMIT_BYTES, vitals.tx_bytes)?;
    encoder.field(vitals::TRANSFER_RATES)?;
    match vitals.transfer_rates {
        Some(rates) => {
            encoder.array(2)?;
            encoder.unsigned(u64::from(rates.rx_bps));
            encoder.unsigned(u64::from(rates.tx_bps));
        }
        None => encoder.nil(),
    }
    encoder.field(vitals::FRAMES)?;
    match vitals.frames {
        Some(frames) => {
            encoder.array(5)?;
            encoder.unsigned(frames.frames_in);
            encoder.unsigned(frames.frames_out);
            encoder.unsigned(frames.malformed);
            encoder.unsigned(frames.undecodable);
            encoder.unsigned(frames.delivered);
        }
        None => encoder.nil(),
    }
    encoder.field(vitals::UPTIME_MS)?;
    match vitals.uptime_ms {
        Some(uptime_ms) => encoder.unsigned(uptime_ms),
        None => encoder.nil(),
    }
    encoder.field(vitals::LAST_FRAME_IN_AT_MS)?;
    match vitals.last_frame_in_at_ms {
        Some(at_ms) => encoder.unsigned(at_ms),
        None => encoder.nil(),
    }
    Ok(())
}

fn decode_entry(
    reader: &mut MessagePackReader<'_>,
    index: usize,
) -> Result<RnsInterfaceVitalsEntry, RnsInterfaceVitalsDecodeError> {
    let marker = reader.marker().map_err(|_| Error::InvalidMessagePack)?;
    if reader
        .map_length(marker)
        .map_err(|_| Error::InvalidMessagePack)?
        != Some(ENTRY_FIELDS)
    {
        return Err(Error::ExpectedInterfaceMap { index });
    }

    let name = String::from(expect_key_then_string(reader, index, interface::NAME)?);
    let id = {
        expect_key(reader, index, vitals::ID)?;
        let marker = reader.marker().map_err(|_| Error::InvalidMessagePack)?;
        let bytes = reader
            .binary(marker)
            .map_err(|_| Error::InvalidMessagePack)?
            .ok_or(Error::InvalidFieldType {
                index,
                field: vitals::ID,
            })?;
        let bytes: [u8; INTERFACE_ID_LEN] =
            bytes.try_into().map_err(|_| Error::InvalidIdLength {
                index,
                actual: bytes.len(),
            })?;
        InterfaceId::new(bytes)
    };
    let connection = {
        let code = expect_key_then_unsigned(reader, index, interface::STATUS)?;
        ConnectionState::from_u8(u8::try_from(code).unwrap_or(u8::MAX))
    };
    // A relayed failure reason cannot become a `&'static str` again, so the report keeps
    // only whether one was present. The `vitals` CLI prints the flag rather than inventing
    // a reason it never received.
    let failure_reason = expect_key_then_optional_string(reader, index, vitals::FAILURE_REASON)?;
    let rx_bytes = expect_key_then_unsigned(reader, index, interface::RECEIVE_BYTES)?;
    let tx_bytes = expect_key_then_unsigned(reader, index, interface::TRANSMIT_BYTES)?;
    let transfer_rates =
        expect_key_then_optional_array::<2>(reader, index, vitals::TRANSFER_RATES)?.map(|values| {
            TransferRates {
                rx_bps: u32::try_from(values[0]).unwrap_or(u32::MAX),
                tx_bps: u32::try_from(values[1]).unwrap_or(u32::MAX),
            }
        });
    let frames =
        expect_key_then_optional_array::<5>(reader, index, vitals::FRAMES)?.map(|values| {
            FrameAccounting {
                frames_in: values[0],
                frames_out: values[1],
                malformed: values[2],
                undecodable: values[3],
                delivered: values[4],
            }
        });
    let uptime_ms = expect_key_then_optional_unsigned(reader, index, vitals::UPTIME_MS)?;
    let last_frame_in_at_ms =
        expect_key_then_optional_unsigned(reader, index, vitals::LAST_FRAME_IN_AT_MS)?;

    Ok(RnsInterfaceVitalsEntry {
        name,
        vitals: InterfaceVitals {
            id,
            connection,
            failure_reason: failure_reason.then_some(RELAYED_FAILURE),
            rx_bytes,
            tx_bytes,
            transfer_rates,
            frames,
            uptime_ms,
            last_frame_in_at_ms,
        },
    })
}

/// Stands in for a failure reason that arrived over the wire. `InterfaceVitals` holds a
/// `&'static str`, so the original text cannot be reconstructed on the receiving side.
pub const RELAYED_FAILURE: &str = "reported by the remote node";

fn expect_key(
    reader: &mut MessagePackReader<'_>,
    index: usize,
    field: &'static str,
) -> Result<(), RnsInterfaceVitalsDecodeError> {
    let marker = reader.marker().map_err(|_| Error::InvalidMessagePack)?;
    let key = reader
        .string(marker)
        .map_err(|_| Error::InvalidMessagePack)?
        .ok_or(Error::InvalidMapKey { index })?;
    if key == field {
        Ok(())
    } else {
        Err(Error::UnexpectedField {
            index,
            expected: field,
            actual: String::from(key),
        })
    }
}

fn expect_key_then_string<'a>(
    reader: &mut MessagePackReader<'a>,
    index: usize,
    field: &'static str,
) -> Result<&'a str, RnsInterfaceVitalsDecodeError> {
    expect_key(reader, index, field)?;
    let marker = reader.marker().map_err(|_| Error::InvalidMessagePack)?;
    reader
        .string(marker)
        .map_err(|_| Error::InvalidMessagePack)?
        .ok_or(Error::InvalidFieldType { index, field })
}

/// Returns whether a string was present, not the string itself: the only consumer needs
/// the presence flag, and keeping the borrow would tie the entry to the input buffer.
fn expect_key_then_optional_string(
    reader: &mut MessagePackReader<'_>,
    index: usize,
    field: &'static str,
) -> Result<bool, RnsInterfaceVitalsDecodeError> {
    expect_key(reader, index, field)?;
    let marker = reader.marker().map_err(|_| Error::InvalidMessagePack)?;
    if marker == Marker::Null {
        return Ok(false);
    }
    reader
        .string(marker)
        .map_err(|_| Error::InvalidMessagePack)?
        .ok_or(Error::InvalidFieldType { index, field })?;
    Ok(true)
}

fn expect_key_then_unsigned(
    reader: &mut MessagePackReader<'_>,
    index: usize,
    field: &'static str,
) -> Result<u64, RnsInterfaceVitalsDecodeError> {
    expect_key(reader, index, field)?;
    read_unsigned(reader, index, field)
}

fn expect_key_then_optional_unsigned(
    reader: &mut MessagePackReader<'_>,
    index: usize,
    field: &'static str,
) -> Result<Option<u64>, RnsInterfaceVitalsDecodeError> {
    expect_key(reader, index, field)?;
    let marker = reader.marker().map_err(|_| Error::InvalidMessagePack)?;
    if marker == Marker::Null {
        return Ok(None);
    }
    unsigned_from(reader, marker, index, field).map(Some)
}

fn expect_key_then_optional_array<const N: usize>(
    reader: &mut MessagePackReader<'_>,
    index: usize,
    field: &'static str,
) -> Result<Option<[u64; N]>, RnsInterfaceVitalsDecodeError> {
    expect_key(reader, index, field)?;
    let marker = reader.marker().map_err(|_| Error::InvalidMessagePack)?;
    if marker == Marker::Null {
        return Ok(None);
    }
    if reader
        .array_length(marker)
        .map_err(|_| Error::InvalidMessagePack)?
        != Some(N)
    {
        return Err(Error::InvalidFieldType { index, field });
    }
    let mut values = [0u64; N];
    for value in &mut values {
        *value = read_unsigned(reader, index, field)?;
    }
    Ok(Some(values))
}

fn read_unsigned(
    reader: &mut MessagePackReader<'_>,
    index: usize,
    field: &'static str,
) -> Result<u64, RnsInterfaceVitalsDecodeError> {
    let marker = reader.marker().map_err(|_| Error::InvalidMessagePack)?;
    unsigned_from(reader, marker, index, field)
}

fn unsigned_from(
    reader: &mut MessagePackReader<'_>,
    marker: Marker,
    index: usize,
    field: &'static str,
) -> Result<u64, RnsInterfaceVitalsDecodeError> {
    match reader
        .integer(marker)
        .map_err(|_| Error::InvalidMessagePack)?
    {
        Some(MessagePackInteger::Nonnegative(value)) => Ok(value),
        _ => Err(Error::InvalidFieldType { index, field }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RnsInterfaceVitalsDecodeError {
    InvalidMessagePack,
    ExpectedReportMap,
    ExpectedInterfacesArray,
    ExpectedInterfaceMap {
        index: usize,
    },
    InvalidMapKey {
        index: usize,
    },
    UnknownField(String),
    UnexpectedField {
        index: usize,
        expected: &'static str,
        actual: String,
    },
    InvalidFieldType {
        index: usize,
        field: &'static str,
    },
    InvalidIdLength {
        index: usize,
        actual: usize,
    },
    AllocationFailed {
        entries: usize,
    },
    TrailingData,
}

impl core::fmt::Display for RnsInterfaceVitalsDecodeError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidMessagePack => formatter.write_str("invalid MessagePack vitals reply"),
            Self::ExpectedReportMap => {
                formatter.write_str("vitals reply must be a one-entry MessagePack map")
            }
            Self::ExpectedInterfacesArray => {
                formatter.write_str("vitals field interfaces must be an array")
            }
            Self::ExpectedInterfaceMap { index } => write!(
                formatter,
                "vitals field interfaces[{index}] must be a {ENTRY_FIELDS}-entry map"
            ),
            Self::InvalidMapKey { index } => write!(
                formatter,
                "vitals field interfaces[{index}] contains a non-string field name"
            ),
            Self::UnknownField(field) => {
                write!(formatter, "vitals reply contains an unknown field {field}")
            }
            Self::UnexpectedField {
                index,
                expected,
                actual,
            } => write!(
                formatter,
                "vitals field interfaces[{index}] has {actual} where {expected} was expected"
            ),
            Self::InvalidFieldType { index, field } => write!(
                formatter,
                "vitals reply has the wrong value type at interfaces[{index}].{field}"
            ),
            Self::InvalidIdLength { index, actual } => write!(
                formatter,
                "vitals reply has {actual} bytes at interfaces[{index}].id, expected {INTERFACE_ID_LEN}"
            ),
            Self::AllocationFailed { entries } => write!(
                formatter,
                "vitals reply declares {entries} interfaces, but storage could not be allocated"
            ),
            Self::TrailingData => formatter.write_str("vitals reply has trailing data"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RnsInterfaceVitalsDecodeError {}

#[cfg(test)]
mod tests;
