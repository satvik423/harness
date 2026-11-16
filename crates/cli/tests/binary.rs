//! End-to-end checks of the `harness` binary.

use std::path::Path;
use std::process::{Command, Output};

/// Runs the binary with HOME pointed at `home` so nothing touches the real user dirs.
fn run(home: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harness"));
    cmd.args(args)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("HARNESS_LOG");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("the harness binary should start")
}

#[test]
fn version_flag_prints_the_crate_version() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &["--version"], &[]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("harness {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn no_arguments_prints_help_and_writes_a_log_file() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &[], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("Usage: harness"));
    let logs = home.path().join(".local/share/harness/logs");
    let text: String = std::fs::read_dir(&logs)
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    assert!(text.contains("harness started"), "{text}");
}

#[test]
fn invalid_log_filter_fails_with_a_clear_message() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &[], &[("HARNESS_LOG", "harness=notalevel")]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("HARNESS_LOG"));
}
