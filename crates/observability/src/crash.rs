use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use thiserror::Error;

use crate::scrub::OutboundPolicy;
use crate::telemetry::{ObservabilityState, SafeEvent, is_safe_field};

const CRASH_SCHEMA_VERSION: u16 = 1;
const DEFAULT_MAX_REPORT_BYTES: usize = 64 * 1024;
static REPORT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct CrashReportConfig {
    pub directory: PathBuf,
    pub max_report_bytes: usize,
    pub outbound: OutboundPolicy,
}

impl CrashReportConfig {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            max_report_bytes: DEFAULT_MAX_REPORT_BYTES,
            outbound: OutboundPolicy::default(),
        }
    }

    pub fn with_policy(mut self, outbound: OutboundPolicy) -> Self {
        self.outbound = outbound;
        self
    }

    pub fn with_max_report_bytes(mut self, max_report_bytes: usize) -> Self {
        self.max_report_bytes = max_report_bytes.max(1024);
        self
    }
}

#[derive(Debug, Error)]
pub enum CrashReportError {
    #[error("crash report directory must be an absolute path")]
    RelativeDirectory,
    #[error("cannot prepare crash report directory: {0}")]
    PrepareDirectory(#[source] io::Error),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CrashReport {
    schema_version: u16,
    timestamp_unix_ms: u64,
    panic_payload_kind: &'static str,
    thread_name: Option<String>,
    package_version: &'static str,
    recent_events: Vec<SafeEvent>,
}

pub(crate) fn install_panic_hook(
    config: CrashReportConfig,
    state: Arc<ObservabilityState>,
) -> Result<(), CrashReportError> {
    prepare_panic_hook(&config)?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload_kind = if info.payload().downcast_ref::<&str>().is_some() {
            "str"
        } else if info.payload().downcast_ref::<String>().is_some() {
            "string"
        } else {
            "opaque"
        };
        let thread_name = std::thread::current()
            .name()
            .map(|name| config.outbound.sanitize_with_budget(name, 128));
        let report = bounded_report(
            payload_kind,
            thread_name,
            state.snapshot(),
            config.max_report_bytes,
            &config.outbound,
        );
        let _ = write_report_atomic(&config.directory, &report, config.max_report_bytes);
        previous(info);
    }));
    Ok(())
}

pub(crate) fn prepare_panic_hook(config: &CrashReportConfig) -> Result<(), CrashReportError> {
    prepare_directory(&config.directory)
}

fn prepare_directory(directory: &Path) -> Result<(), CrashReportError> {
    if !directory.is_absolute() {
        return Err(CrashReportError::RelativeDirectory);
    }
    fs::create_dir_all(directory).map_err(CrashReportError::PrepareDirectory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(CrashReportError::PrepareDirectory)?;
    }
    Ok(())
}

fn bounded_report(
    panic_payload_kind: &'static str,
    thread_name: Option<String>,
    recent_events: Vec<SafeEvent>,
    max_bytes: usize,
    outbound: &OutboundPolicy,
) -> CrashReport {
    let mut report = CrashReport {
        schema_version: CRASH_SCHEMA_VERSION,
        timestamp_unix_ms: unix_millis(),
        panic_payload_kind,
        thread_name,
        package_version: env!("CARGO_PKG_VERSION"),
        recent_events: recent_events
            .into_iter()
            .map(|event| sanitize_event(event, outbound))
            .collect(),
    };
    while serialize_report(&report).is_ok_and(|payload| payload.len() > max_bytes) {
        if report.recent_events.is_empty() {
            report.thread_name = None;
            break;
        }
        report.recent_events.remove(0);
    }
    report
}

fn sanitize_event(mut event: SafeEvent, outbound: &OutboundPolicy) -> SafeEvent {
    event.level = outbound.sanitize_with_budget(&event.level, 32);
    event.target = outbound.sanitize_field(&event.target);
    event.fields = event
        .fields
        .into_iter()
        .filter(|(key, _)| is_safe_field(key))
        .map(|(key, value)| {
            (
                outbound.sanitize_field(&key),
                outbound.sanitize_field(&value),
            )
        })
        .collect();
    event
}

fn write_report_atomic(directory: &Path, report: &CrashReport, max_bytes: usize) -> io::Result<()> {
    let payload = serialize_report(report)?;
    if payload.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "crash report exceeds its byte budget",
        ));
    }
    let timestamp = report.timestamp_unix_ms;
    let sequence = REPORT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(".{timestamp}-{sequence}.tmp"));
    let destination = directory.join(format!("crash-{timestamp}-{sequence}.json"));
    let mut file = private_file(&temporary)?;
    file.write_all(&payload)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    sync_directory(directory)?;
    Ok(())
}

fn serialize_report(report: &CrashReport) -> Result<Vec<u8>, io::Error> {
    serde_json::to_vec(report).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> io::Result<()> {
    Ok(())
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn report_never_contains_panic_message_prompt_file_content_key_or_path() {
        let temp = tempfile::tempdir().unwrap();
        let events = vec![SafeEvent {
            timestamp_unix_ms: 1,
            level: "error".into(),
            target: "evo::lifecycle".into(),
            fields: BTreeMap::from([
                (
                    "owner_id".into(),
                    "sk-abcdefghijklmnopqrstuvwxyz123456 /home/user/private crates/source.rs"
                        .into(),
                ),
                ("prompt".into(), "my exact prompt".into()),
                ("content".into(), "fn secret_file_content() {}".into()),
            ]),
        }];
        let report = bounded_report(
            "string",
            Some("worker".into()),
            events,
            4096,
            &OutboundPolicy::default(),
        );
        write_report_atomic(temp.path(), &report, 4096).unwrap();
        let path = fs::read_dir(temp.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = fs::read_to_string(path).unwrap();
        assert!(!text.contains("my exact prompt"));
        assert!(!text.contains("fn secret_file_content"));
        assert!(!text.contains("sk-abcdefghijklmnopqrstuvwxyz123456"));
        assert!(!text.contains("/home/user/private"));
        assert!(!text.contains("crates/source.rs"));
        assert!(text.contains("panicPayloadKind"));
        assert!(text.contains("recentEvents"));
    }

    #[test]
    fn report_budget_drops_oldest_events() {
        let events = (0..100)
            .map(|index| SafeEvent {
                timestamp_unix_ms: index,
                level: "info".into(),
                target: "evo::lifecycle".into(),
                fields: BTreeMap::from([("owner_id".into(), "x".repeat(200))]),
            })
            .collect();
        let report = bounded_report("str", None, events, 2048, &OutboundPolicy::default());
        assert!(serialize_report(&report).unwrap().len() <= 2048);
        assert!(report.recent_events.len() < 100);
    }

    #[test]
    fn relative_directory_is_rejected() {
        assert!(matches!(
            prepare_directory(Path::new("relative")),
            Err(CrashReportError::RelativeDirectory)
        ));
    }
}
