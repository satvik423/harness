//! Connection settings and migrations (Tech spec 3.12).

use rusqlite::Connection;

use crate::backend;
use harness_ports::StoreError;

/// Migrations in order; the database's `user_version` counts those applied.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_task_controls.sql"),
];

/// The schema version this build writes.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// Settings applied to every connection.
///
/// WAL with `synchronous = NORMAL` survives a killed process (NFR-03); only a
/// power loss can lose the last commits. `busy_timeout` lets several harness
/// processes share the file (task controls are written from other terminals).
pub(crate) fn configure(conn: &Connection) -> Result<(), StoreError> {
    let mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(backend)?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::Backend(format!(
            "the database refused WAL mode (got {mode})"
        )));
    }
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(backend)?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(backend)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(backend)?;
    Ok(())
}

/// Applies the migrations the database has not seen yet, each in its own transaction.
///
/// # Errors
/// [`StoreError::Corrupt`] if the database is newer than this build, or a
/// backend error if a migration fails.
pub(crate) fn migrate(conn: &mut Connection) -> Result<u32, StoreError> {
    let current: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(backend)?;
    if current > SCHEMA_VERSION {
        return Err(StoreError::Corrupt(format!(
            "the database uses schema version {current}, but this harness knows only up to {SCHEMA_VERSION}; upgrade harness"
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction().map_err(backend)?;
        tx.execute_batch(sql).map_err(backend)?;
        tx.pragma_update(
            None,
            "user_version",
            u32::try_from(index + 1).unwrap_or(u32::MAX),
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)?;
    }
    Ok(SCHEMA_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_once_and_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(dir.path().join("t.db")).unwrap();
        configure(&conn).unwrap();
        assert_eq!(migrate(&mut conn).unwrap(), SCHEMA_VERSION);
        assert_eq!(migrate(&mut conn).unwrap(), SCHEMA_VERSION);
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('sessions','events','task_controls','snapshots')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 4);
    }

    #[test]
    fn a_newer_database_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(dir.path().join("t.db")).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(matches!(migrate(&mut conn), Err(StoreError::Corrupt(_))));
    }

    #[test]
    fn fts5_is_available() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE VIRTUAL TABLE t USING fts5(body)")
            .unwrap();
    }
}
