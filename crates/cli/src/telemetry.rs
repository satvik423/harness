//! Logging setup (P0-T6): `tracing` to a daily-rotated file, filtered by `HARNESS_LOG`.
//!
//! Logs never go to stdout or stderr, because the TUI owns the terminal.

use std::path::Path;

use thiserror::Error;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

/// Environment variable holding the log filter, for example `harness=debug,info`.
pub const LOG_ENV: &str = "HARNESS_LOG";

/// Filter used when [`LOG_ENV`] is unset or empty.
pub const DEFAULT_FILTER: &str = "info";

/// Number of daily log files kept.
pub const MAX_LOG_FILES: usize = 7;

/// Errors from setting up logging.
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// The filter string is not valid `EnvFilter` syntax.
    #[error("invalid {LOG_ENV} value {value:?}: {message}")]
    InvalidFilter {
        /// The rejected filter.
        value: String,
        /// The parser's explanation.
        message: String,
    },
    /// The log directory or file could not be created.
    #[error("cannot open log files in {dir}: {message}")]
    Appender {
        /// The log directory.
        dir: String,
        /// The underlying error.
        message: String,
    },
    /// A global subscriber was already installed in this process.
    #[error("logging is already initialised")]
    AlreadyInitialised,
}

/// Parses a log filter, using [`DEFAULT_FILTER`] when `value` is `None` or blank.
///
/// # Errors
/// [`TelemetryError::InvalidFilter`] if the filter does not parse.
///
/// # Examples
/// ```
/// use harness_cli::telemetry::filter_from;
/// assert_eq!(filter_from(None).unwrap().to_string(), "info");
/// assert!(filter_from(Some("harness=notalevel")).is_err());
/// ```
pub fn filter_from(value: Option<&str>) -> Result<EnvFilter, TelemetryError> {
    let value = value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(DEFAULT_FILTER);
    EnvFilter::try_new(value).map_err(|e| TelemetryError::InvalidFilter {
        value: value.to_owned(),
        message: e.to_string(),
    })
}

/// Creates the rotating file writer: `harness.YYYY-MM-DD.log` in `dir`, daily,
/// keeping [`MAX_LOG_FILES`] files. Creates `dir` if needed.
///
/// # Errors
/// [`TelemetryError::Appender`] if the directory cannot be created or opened.
pub fn file_appender(dir: &Path) -> Result<RollingFileAppender, TelemetryError> {
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("harness")
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(dir)
        .map_err(|e| TelemetryError::Appender {
            dir: dir.display().to_string(),
            message: e.to_string(),
        })
}

/// Installs the global `tracing` subscriber writing to [`file_appender`]`(dir)`.
///
/// Keep the returned guard alive for the whole program: dropping it flushes
/// and stops the background writer.
///
/// # Errors
/// [`TelemetryError::Appender`] if the files cannot be opened, or
/// [`TelemetryError::AlreadyInitialised`] if called twice in one process.
pub fn init(dir: &Path, filter: EnvFilter) -> Result<WorkerGuard, TelemetryError> {
    let (writer, guard) = tracing_appender::non_blocking(file_appender(dir)?);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .try_init()
        .map_err(|_| TelemetryError::AlreadyInitialised)?;
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn missing_or_blank_filter_uses_default() {
        assert_eq!(filter_from(None).unwrap().to_string(), DEFAULT_FILTER);
        assert_eq!(filter_from(Some("  ")).unwrap().to_string(), DEFAULT_FILTER);
    }

    #[test]
    fn valid_filter_is_kept() {
        let f = filter_from(Some("harness=debug,warn")).unwrap().to_string();
        assert!(f.contains("harness=debug") && f.contains("warn"), "{f}");
    }

    #[test]
    fn invalid_filter_reports_the_value() {
        let err = filter_from(Some("harness=notalevel")).unwrap_err();
        assert!(err.to_string().contains("harness=notalevel"), "{err}");
    }

    #[test]
    fn appender_creates_directory_and_named_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("logs");
        let mut writer = file_appender(&dir).unwrap();
        writer.write_all(b"line\n").unwrap();
        writer.flush().unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(
            names[0].starts_with("harness.") && names[0].ends_with(".log"),
            "{names:?}"
        );
    }
}
