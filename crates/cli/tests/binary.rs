//! End-to-end checks of the `harness` binary.

// Test-only file: helpers may unwrap (clippy's test exemption covers only #[test] fns).
#![allow(clippy::unwrap_used)]

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
    // First run with a fresh home: nothing about the log directory may reach the terminal.
    assert!(
        out.stderr.is_empty(),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

#[test]
fn run_help_lists_its_flags() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &["run", "--help"], &[]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in ["--json", "--model", "--max-turns", "--approval"] {
        assert!(text.contains(flag), "missing {flag} in\n{text}");
    }
}

#[test]
fn run_without_a_key_explains_how_to_store_one() {
    let home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harness"));
    let out = cmd
        .args(["run", "hello"])
        .current_dir(home.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("ANTHROPIC_API_KEY")
        // Point the key at an unset variable so the real keyring is never read.
        .env(
            "HARNESS_PROVIDERS__ANTHROPIC__API_KEY",
            "env:HARNESS_TEST_MISSING_KEY",
        )
        .output()
        .expect("the harness binary should start");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no Anthropic API key found") && err.contains("secret-tool store"),
        "{err}"
    );
}

#[test]
fn unknown_providers_are_rejected() {
    let home = tempfile::tempdir().unwrap();
    let out = run(
        home.path(),
        &["run", "--model", "google/gemini", "hello"],
        &[],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown provider"));
}

#[test]
fn openai_without_a_key_explains_how_to_store_one() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_harness"))
        .args(["run", "--model", "openai/gpt-x", "hello"])
        .current_dir(home.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("OPENAI_API_KEY")
        // Point the key at an unset variable so the real keyring is never read.
        .env(
            "HARNESS_PROVIDERS__OPENAI__API_KEY",
            "env:HARNESS_TEST_MISSING_KEY",
        )
        .output()
        .expect("the harness binary should start");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no OpenAI API key found") && err.contains("username openai"),
        "{err}"
    );
}
