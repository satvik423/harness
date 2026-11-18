//! Deleting a session (`harness session delete`) and freeing the blobs that
//! nothing references any more (ADR-041).

use std::collections::HashSet;
use std::path::Path;

use harness_ports::StoreError;
use harness_types::{BlobRef, SessionId};
use rusqlite::{Connection, params};
use serde_json::Value;

use crate::backend;
use crate::lock::{LockError, SessionLock};

/// Removes every row of `session`; returns how many events went.
///
/// # Errors
/// [`StoreError::NotFound`] for an unknown session, [`StoreError::Conflict`]
/// if other sessions were forked from it.
pub(crate) fn delete_rows(conn: &Connection, session: SessionId) -> Result<u64, StoreError> {
    let id = session.to_string();
    let exists: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions WHERE id = ?1", [&id], |r| {
            r.get(0)
        })
        .map_err(backend)?;
    if exists == 0 {
        return Err(StoreError::NotFound(format!("session {id}")));
    }
    let forks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE parent_id = ?1",
            [&id],
            |r| r.get(0),
        )
        .map_err(backend)?;
    if forks > 0 {
        return Err(StoreError::Conflict(format!(
            "{forks} session(s) were forked from session {id}; delete them first"
        )));
    }
    // Children before parents; the writer's savepoint makes this all-or-nothing.
    let statements = [
        "DELETE FROM task_controls WHERE session_id = ?1",
        "DELETE FROM snapshots WHERE session_id = ?1",
        "DELETE FROM usage WHERE task_id IN (SELECT id FROM tasks WHERE session_id = ?1)
           OR task_id IN (SELECT task_id FROM events WHERE session_id = ?1 AND task_id IS NOT NULL)",
        "DELETE FROM events_fts WHERE rowid IN (SELECT id FROM events WHERE session_id = ?1)",
    ];
    for sql in statements {
        conn.execute(sql, [&id]).map_err(backend)?;
    }
    let events = conn
        .execute("DELETE FROM events WHERE session_id = ?1", [&id])
        .map_err(backend)?;
    for sql in [
        "DELETE FROM tasks WHERE session_id = ?1",
        "DELETE FROM segments WHERE session_id = ?1",
        "DELETE FROM anchors WHERE session_id = ?1",
        "DELETE FROM pinned_facts WHERE session_id = ?1",
        "DELETE FROM sessions WHERE id = ?1",
    ] {
        conn.execute(sql, [&id]).map_err(backend)?;
    }
    Ok(u64::try_from(events).unwrap_or(0))
}

/// Collects every 64-hex-digit string in a JSON value: that is how a
/// [`BlobRef`] is serialized inside event payloads.
fn blob_refs(value: &Value, out: &mut HashSet<String>) {
    match value {
        Value::String(s) if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) => {
            out.insert(s.to_ascii_lowercase());
        }
        Value::Array(items) => items.iter().for_each(|v| blob_refs(v, out)),
        Value::Object(map) => map.values().for_each(|v| blob_refs(v, out)),
        _ => {}
    }
}

/// Deletes the rows of blobs that no event or snapshot references and returns
/// them, so their files can be removed once the deletion is committed.
///
/// One pass over all payloads: a full scan, but deleting is rare and this keeps
/// the rule simple (every blob must be reachable from the log or a snapshot).
pub(crate) fn unreferenced_blobs(conn: &Connection) -> Result<Vec<BlobRef>, StoreError> {
    let mut live = HashSet::new();
    let mut stmt = conn
        .prepare("SELECT payload FROM events")
        .map_err(backend)?;
    let mut rows = stmt.query([]).map_err(backend)?;
    while let Some(row) = rows.next().map_err(backend)? {
        let payload: String = row.get(0).map_err(backend)?;
        let value: Value = serde_json::from_str(&payload)
            .map_err(|e| StoreError::Corrupt(format!("event payload: {e}")))?;
        blob_refs(&value, &mut live);
    }
    let mut stmt = conn
        .prepare("SELECT blob_hash FROM snapshots WHERE blob_hash IS NOT NULL")
        .map_err(backend)?;
    for hash in stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(backend)?
    {
        live.insert(hash.map_err(backend)?.to_ascii_lowercase());
    }
    let all: Vec<String> = conn
        .prepare("SELECT hash FROM blobs")
        .map_err(backend)?
        .query_map([], |r| r.get(0))
        .map_err(backend)?
        .collect::<Result<_, _>>()
        .map_err(backend)?;
    let mut dead = Vec::new();
    for hash in all.into_iter().filter(|h| !live.contains(h)) {
        conn.execute("DELETE FROM blobs WHERE hash = ?1", params![hash])
            .map_err(backend)?;
        // A hash that does not parse was never written by this store; skip its file.
        if let Ok(blob) = hash.parse() {
            dead.push(blob);
        }
    }
    Ok(dead)
}

/// `true` if a harness process holds any session lock other than `except`'s.
///
/// Blobs are content-addressed and shared: an open session may be writing a
/// blob whose bytes equal one about to be freed (the file is written before
/// its row), so blobs are only freed when no other session is open.
pub(crate) fn other_sessions_open(locks_dir: &Path, except: SessionId) -> bool {
    let Ok(entries) = std::fs::read_dir(locks_dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|n| n.strip_suffix(".lock"))
            .and_then(|n| n.parse::<SessionId>().ok())
        else {
            return false;
        };
        id != except
            && matches!(
                SessionLock::acquire(locks_dir, id),
                Err(LockError::Held(_) | LockError::Io { .. })
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_refs_are_found_at_any_depth_and_other_strings_are_not() {
        let hex = "ab".repeat(32);
        let value = serde_json::json!({
            "frame": hex,
            "sent": [{"text": "hello"}, {"blob": "CD".repeat(32)}],
            "short": "abcd",
            "not_hex": "zz".repeat(32),
        });
        let mut out = HashSet::new();
        blob_refs(&value, &mut out);
        assert_eq!(out, HashSet::from([hex, "cd".repeat(32)]));
    }
}
