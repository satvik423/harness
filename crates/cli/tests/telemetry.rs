//! `init` installs a global subscriber, so it gets its own test process.

use harness_cli::telemetry::{TelemetryError, filter_from, init};

#[test]
fn init_writes_events_to_the_log_file_and_refuses_a_second_call() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("logs");
    let guard = init(&dir, filter_from(Some("info")).unwrap()).unwrap();
    tracing::info!("hello-from-telemetry-test");
    tracing::debug!("debug-is-filtered-out");
    assert!(matches!(
        init(&dir, filter_from(None).unwrap()),
        Err(TelemetryError::AlreadyInitialised)
    ));
    drop(guard); // flushes the background writer

    let text: String = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    assert!(text.contains("hello-from-telemetry-test"), "{text}");
    assert!(!text.contains("debug-is-filtered-out"), "{text}");
}
