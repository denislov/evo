use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, value::RawValue};
use sha2::{Digest, Sha256};

use crate::error::{JournalError, JournalErrorKind};

pub const MAX_JOURNAL_RECORD_BYTES: usize = 1024 * 1024;
pub const MAX_JOURNAL_PAYLOAD_BYTES: usize = MAX_JOURNAL_RECORD_BYTES - 4096;
const FRAME_SCHEMA: &str = "evo.session.frame";
const FRAME_VERSION: u32 = 2;

#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FrameMetadata {
    schema: String,
    version: u32,
    payload_bytes: u32,
    sha256: String,
}

#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableFrame {
    #[serde(rename = "_evo_frame")]
    metadata: FrameMetadata,
    payload: Box<RawValue>,
}

pub fn encode_json_record(record: &impl Serialize, kind: &str) -> Result<Vec<u8>, JournalError> {
    let payload = serde_json::to_vec(record).map_err(|error| {
        JournalError::new(
            JournalErrorKind::Codec,
            format!("failed to serialize {kind}: {error}"),
        )
    })?;
    if payload.len() > MAX_JOURNAL_PAYLOAD_BYTES {
        return Err(JournalError::new(
            JournalErrorKind::WriteRejected,
            format!("{kind} payload exceeds {MAX_JOURNAL_PAYLOAD_BYTES} bytes"),
        ));
    }
    let payload = String::from_utf8(payload).map_err(|error| {
        JournalError::new(
            JournalErrorKind::Codec,
            format!("failed to encode {kind} as UTF-8: {error}"),
        )
    })?;
    let raw_payload = RawValue::from_string(payload).map_err(|error| {
        JournalError::new(
            JournalErrorKind::Codec,
            format!("failed to frame {kind} payload: {error}"),
        )
    })?;
    let payload_bytes = raw_payload.get().as_bytes();
    let metadata = FrameMetadata {
        schema: FRAME_SCHEMA.into(),
        version: FRAME_VERSION,
        payload_bytes: payload_bytes.len().try_into().map_err(|_| {
            JournalError::new(
                JournalErrorKind::WriteRejected,
                format!("{kind} payload length overflowed"),
            )
        })?,
        sha256: format!("{:x}", Sha256::digest(payload_bytes)),
    };
    let mut framed = serde_json::to_vec(&DurableFrame {
        metadata,
        payload: raw_payload,
    })
    .map_err(|error| {
        JournalError::new(
            JournalErrorKind::Codec,
            format!("failed to frame {kind}: {error}"),
        )
    })?;
    if framed.len().saturating_add(1) > MAX_JOURNAL_RECORD_BYTES {
        return Err(JournalError::new(
            JournalErrorKind::WriteRejected,
            format!("framed {kind} exceeds {MAX_JOURNAL_RECORD_BYTES} bytes"),
        ));
    }
    framed.push(b'\n');
    Ok(framed)
}

pub fn decode_json_record<T: DeserializeOwned>(
    line: &str,
    line_number: usize,
    kind: &str,
) -> Result<T, JournalError> {
    let value = decode_json_value(line, line_number, kind)?;
    serde_json::from_value(value).map_err(|error| {
        JournalError::new(
            JournalErrorKind::Codec,
            format!("failed to decode {kind} at line {line_number}: {error}"),
        )
    })
}

pub fn decode_json_value(
    line: &str,
    line_number: usize,
    kind: &str,
) -> Result<Value, JournalError> {
    let frame: DurableFrame = serde_json::from_str(line).map_err(|error| {
        JournalError::new(
            JournalErrorKind::Corrupt,
            format!(
                "failed to parse required v{FRAME_VERSION} {kind} frame at line {line_number}: {error}"
            ),
        )
    })?;
    let DurableFrame { metadata, payload } = frame;
    if metadata.schema != FRAME_SCHEMA || metadata.version != FRAME_VERSION {
        return Err(JournalError::new(
            JournalErrorKind::Unsupported,
            format!(
                "unsupported {kind} frame at line {line_number}: schema={}, version={}",
                metadata.schema, metadata.version
            ),
        ));
    }
    let payload_bytes = payload.get().as_bytes();
    if payload_bytes.len() > MAX_JOURNAL_PAYLOAD_BYTES {
        return Err(JournalError::new(
            JournalErrorKind::Corrupt,
            format!(
                "{kind} payload exceeds {MAX_JOURNAL_PAYLOAD_BYTES} bytes at line {line_number}"
            ),
        ));
    }
    if usize::try_from(metadata.payload_bytes).ok() != Some(payload_bytes.len()) {
        return Err(JournalError::new(
            JournalErrorKind::Corrupt,
            format!("{kind} frame length mismatch at line {line_number}"),
        ));
    }
    let actual_sha256 = format!("{:x}", Sha256::digest(payload_bytes));
    if metadata.sha256 != actual_sha256 {
        return Err(JournalError::new(
            JournalErrorKind::Corrupt,
            format!("{kind} frame checksum mismatch at line {line_number}"),
        ));
    }
    serde_json::from_str(payload.get()).map_err(|error| {
        JournalError::new(
            JournalErrorKind::Codec,
            format!("failed to decode verified {kind} payload at line {line_number}: {error}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips_and_detects_tampering() {
        let encoded = encode_json_record(&serde_json::json!({"value": 7}), "test").unwrap();
        let line = std::str::from_utf8(&encoded[..encoded.len() - 1]).unwrap();
        assert_eq!(
            decode_json_record::<Value>(line, 1, "test").unwrap(),
            serde_json::json!({"value": 7})
        );

        let mut frame: Value = serde_json::from_str(line).unwrap();
        frame["payload"]["value"] = serde_json::json!(8);
        let tampered = serde_json::to_string(&frame).unwrap();
        assert_eq!(
            decode_json_record::<Value>(&tampered, 1, "test")
                .unwrap_err()
                .kind(),
            JournalErrorKind::Corrupt
        );
    }
}
