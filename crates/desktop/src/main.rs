fn main() {
    let _observability = install_observability();
    desktop::run(desktop::DesktopApplicationOptions::projectless());
}

fn install_observability() -> Option<observability::ObservabilityRuntime> {
    let crash_directory =
        coding_agent::api::embedding::global_config_directory().join("crash-reports");
    let config = observability::ObservabilityConfig::default()
        .with_crash_report(observability::CrashReportConfig::new(crash_directory));
    match observability::ObservabilityRuntime::install_global(
        config,
        std::sync::Arc::new(observability::NoopTelemetrySink),
    ) {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            eprintln!("observability initialization failed: {error}");
            None
        }
    }
}
