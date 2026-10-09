//! Reference host-owned SQLite storage. This crate has no GPUI or CloudKit dependency.
//! A successful commit atomically stores content, history, a change, and a dirty marker.

use markraft_notes::{
    Asset, AssetId, BackendCapabilities, BackendError, BackendMutation, BackendNote,
    BackendSnapshot, NoteId, NotesBackend, StorageRevision, WorkspaceSettings,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, time::Duration};

pub struct SqliteNotesBackend {
    connection: Connection,
}

/// A durable local change. Deleted notes remain available to the synchronization worker.
#[derive(Debug, Clone)]
pub struct Change {
    pub sequence: u64,
    pub id: NoteId,
    pub revision: StorageRevision,
    pub deleted: bool,
}

#[derive(Debug, Clone)]
pub struct Revision {
    pub revision: StorageRevision,
    pub parent: Option<StorageRevision>,
    pub markdown: Option<String>,
    pub deleted: bool,
}

fn sql_integer(value: u64) -> Result<i64, BackendError> {
    i64::try_from(value)
        .map_err(|_| BackendError::Invalid("Value exceeds SQLite integer range".into()))
}

fn unsigned(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

const NOTE_COLUMNS: &str =
    "id, markdown, revision, created_at, updated_at, pinned, title, logical_key";

fn note(row: &rusqlite::Row<'_>) -> rusqlite::Result<BackendNote> {
    Ok(BackendNote {
        id: NoteId::new(row.get::<_, String>(0)?),
        markdown: row.get(1)?,
        revision: StorageRevision(row.get(2)?),
        created_at: unsigned(row, 3)?,
        updated_at: unsigned(row, 4)?,
        pinned: row.get(5)?,
        title: row.get(6)?,
        logical_key: row.get(7)?,
    })
}

fn unavailable(error: impl std::fmt::Display) -> BackendError {
    BackendError::Unavailable(error.to_string())
}

impl SqliteNotesBackend {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, BackendError> {
        let connection = Connection::open(path).map_err(unavailable)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(unavailable)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
            )
            .map_err(unavailable)?;
        let schema_version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(unavailable)?;
        if schema_version > 1 {
            return Err(BackendError::Invalid(format!(
                "Unsupported notes schema version {schema_version}"
            )));
        }
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS notes (
                 id TEXT PRIMARY KEY, markdown TEXT NOT NULL, revision TEXT NOT NULL,
                 title TEXT, logical_key TEXT,
                 created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                 pinned INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0,
                 dirty INTEGER NOT NULL DEFAULT 1
             );
             CREATE TABLE IF NOT EXISTS note_revisions (
                 revision TEXT PRIMARY KEY, note_id TEXT NOT NULL REFERENCES notes(id),
                 parent TEXT, markdown TEXT, deleted INTEGER NOT NULL, changed_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS changes (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 note_id TEXT NOT NULL REFERENCES notes(id), revision TEXT NOT NULL,
                 deleted INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS workspace (
                 singleton INTEGER PRIMARY KEY CHECK(singleton = 1), active_id TEXT, settings TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS assets (
                 id TEXT PRIMARY KEY, media_type TEXT NOT NULL, content BLOB NOT NULL
             );
             PRAGMA user_version=1;
             COMMIT;",
        ).map_err(unavailable)?;
        Ok(Self { connection })
    }

    /// Read the durable change stream. Retain the cursor only after handling all returned changes.
    pub fn changes_since(&self, cursor: u64) -> Result<Vec<Change>, BackendError> {
        let mut statement = self.connection.prepare(
            "SELECT sequence, note_id, revision, deleted FROM changes WHERE sequence > ? ORDER BY sequence",
        ).map_err(unavailable)?;
        statement
            .query_map([sql_integer(cursor)?], |row| {
                Ok(Change {
                    sequence: unsigned(row, 0)?,
                    id: NoteId::new(row.get::<_, String>(1)?),
                    revision: StorageRevision(row.get(2)?),
                    deleted: row.get(3)?,
                })
            })
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)
    }

    /// Pending records survive process restarts. This example does not upload them.
    pub fn pending_sync(&self) -> Result<Vec<Change>, BackendError> {
        let mut statement = self.connection.prepare(
            "SELECT (SELECT MAX(sequence) FROM changes WHERE note_id=n.id), id, revision, deleted FROM notes n WHERE dirty=1 ORDER BY id",
        ).map_err(unavailable)?;
        statement
            .query_map([], |row| {
                Ok(Change {
                    sequence: unsigned(row, 0)?,
                    id: NoteId::new(row.get::<_, String>(1)?),
                    revision: StorageRevision(row.get(2)?),
                    deleted: row.get(3)?,
                })
            })
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)
    }

    /// A receipt for an older upload must never mark a newer local edit as synchronized.
    pub fn acknowledge(
        &mut self,
        id: &NoteId,
        sent: &StorageRevision,
    ) -> Result<bool, BackendError> {
        self.connection
            .execute(
                "UPDATE notes SET dirty=0 WHERE id=? AND revision=? AND dirty=1",
                params![id.as_str(), sent.0],
            )
            .map(|changed| changed == 1)
            .map_err(unavailable)
    }

    pub fn history(&self, id: &NoteId) -> Result<Vec<Revision>, BackendError> {
        let mut statement = self.connection.prepare(
            "SELECT revision, parent, markdown, deleted FROM note_revisions WHERE note_id=? ORDER BY rowid",
        ).map_err(unavailable)?;
        statement
            .query_map([id.as_str()], |row| {
                Ok(Revision {
                    revision: StorageRevision(row.get(0)?),
                    parent: row.get::<_, Option<String>>(1)?.map(StorageRevision),
                    markdown: row.get(2)?,
                    deleted: row.get(3)?,
                })
            })
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)
    }
}

impl NotesBackend for SqliteNotesBackend {
    fn load(&mut self) -> Result<BackendSnapshot, BackendError> {
        // One read transaction keeps the notes consistent with the workspace state.
        let transaction = self.connection.transaction().map_err(unavailable)?;
        let notes = {
            let mut statement = transaction
                .prepare(&format!(
                    "SELECT {NOTE_COLUMNS} FROM notes WHERE deleted=0 ORDER BY created_at, id"
                ))
                .map_err(unavailable)?;
            statement
                .query_map([], note)
                .map_err(unavailable)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(unavailable)?
        };
        let settings: Option<(Option<String>, String)> = transaction
            .query_row(
                "SELECT active_id, settings FROM workspace WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(unavailable)?;
        let (active_id, workspace) = match settings {
            Some((active, json)) => (
                active.map(NoteId::new),
                serde_json::from_str(&json).map_err(unavailable)?,
            ),
            None => (None, WorkspaceSettings::default()),
        };
        transaction.commit().map_err(unavailable)?;
        Ok(BackendSnapshot {
            notes,
            active_id,
            workspace,
        })
    }

    fn read(&mut self, ids: &[NoteId]) -> Result<Vec<BackendNote>, BackendError> {
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT {NOTE_COLUMNS} FROM notes WHERE deleted=0 AND id=?"
            ))
            .map_err(unavailable)?;
        let mut notes = Vec::new();
        for id in ids {
            let found = statement
                .query_row([id.as_str()], note)
                .optional()
                .map_err(unavailable)?;
            notes.extend(found);
        }
        Ok(notes)
    }

    fn commit(&mut self, mutation: BackendMutation) -> Result<StorageRevision, BackendError> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let (id, expected) = match &mutation {
            BackendMutation::Put { id, expected, .. } => (id, expected.as_ref()),
            BackendMutation::Delete { id, expected, .. } => (id, Some(expected)),
        };
        let current: Option<(String, bool)> = transaction
            .query_row(
                "SELECT revision, deleted FROM notes WHERE id=?",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(unavailable)?;
        // A deleted note counts as absent: a create replaces its tombstone, and
        // every write that expects a revision is stale.
        let actual = current
            .as_ref()
            .filter(|(_, deleted)| !deleted)
            .map(|(revision, _)| StorageRevision(revision.clone()));
        if actual.as_ref() != expected {
            return Err(BackendError::Conflict {
                id: id.clone(),
                actual,
            });
        }
        // History continues from the tombstone when a create replaces one.
        let parent = current.map(|(revision, _)| revision);
        let revision = StorageRevision(uuid::Uuid::new_v4().to_string());
        let (markdown, deleted, timestamp) = match &mutation {
            BackendMutation::Put {
                markdown,
                created_at,
                updated_at,
                pinned,
                title,
                logical_key,
                ..
            } => {
                transaction.execute(
                    "INSERT INTO notes (id,markdown,revision,created_at,updated_at,pinned,title,logical_key,deleted,dirty) VALUES (?,?,?,?,?,?,?,?,0,1)
                     ON CONFLICT(id) DO UPDATE SET markdown=excluded.markdown, revision=excluded.revision,
                     created_at=excluded.created_at, updated_at=excluded.updated_at, pinned=excluded.pinned,
                     title=excluded.title, logical_key=excluded.logical_key, deleted=0, dirty=1",
                    params![id.as_str(), markdown, revision.0, sql_integer(*created_at)?, sql_integer(*updated_at)?, pinned, title, logical_key],
                ).map_err(unavailable)?;
                (Some(markdown.as_str()), false, *updated_at)
            }
            BackendMutation::Delete { deleted_at, .. } => {
                transaction
                    .execute(
                        "UPDATE notes SET revision=?, updated_at=?, deleted=1, dirty=1 WHERE id=?",
                        params![revision.0, sql_integer(*deleted_at)?, id.as_str()],
                    )
                    .map_err(unavailable)?;
                (None, true, *deleted_at)
            }
        };
        transaction.execute(
            "INSERT INTO note_revisions (revision,note_id,parent,markdown,deleted,changed_at) VALUES (?,?,?,?,?,?)",
            params![revision.0, id.as_str(), parent, markdown, deleted, sql_integer(timestamp)?],
        ).map_err(unavailable)?;
        transaction
            .execute(
                "INSERT INTO changes (note_id,revision,deleted) VALUES (?,?,?)",
                params![id.as_str(), revision.0, deleted],
            )
            .map_err(unavailable)?;
        transaction.commit().map_err(unavailable)?;
        Ok(revision)
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities { assets: true }
    }

    fn save_workspace(
        &mut self,
        active_id: Option<&NoteId>,
        workspace: &WorkspaceSettings,
    ) -> Result<(), BackendError> {
        let json = serde_json::to_string(workspace).map_err(unavailable)?;
        self.connection.execute("INSERT INTO workspace (singleton,active_id,settings) VALUES (1,?,?) ON CONFLICT(singleton) DO UPDATE SET active_id=excluded.active_id, settings=excluded.settings",
            params![active_id.map(NoteId::as_str), json]).map_err(unavailable)?;
        Ok(())
    }

    fn read_asset(&mut self, id: &AssetId) -> Result<Asset, BackendError> {
        self.connection
            .query_row(
                "SELECT media_type,content FROM assets WHERE id=?",
                [&id.0],
                |row| {
                    Ok(Asset {
                        id: id.clone(),
                        media_type: row.get(0)?,
                        bytes: row.get(1)?,
                    })
                },
            )
            .map_err(unavailable)
    }

    fn write_asset(&mut self, asset: Asset) -> Result<(), BackendError> {
        let changed = self.connection.execute(
            "INSERT INTO assets (id,media_type,content) VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET id=excluded.id WHERE media_type=excluded.media_type AND content=excluded.content",
            params![asset.id.0, asset.media_type, asset.bytes],
        ).map_err(unavailable)?;
        if changed == 0 {
            return Err(BackendError::Invalid(
                "An asset ID cannot identify different content".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(id: &str, expected: Option<StorageRevision>, source: &str) -> BackendMutation {
        BackendMutation::Put {
            id: NoteId::new(id),
            expected,
            markdown: source.into(),
            created_at: 10,
            updated_at: 20,
            pinned: false,
            title: None,
            logical_key: None,
        }
    }

    #[test]
    fn the_backend_keeps_the_notes_contract() {
        markraft_notes::conformance::check(|| {
            let data = tempfile::tempdir().unwrap();
            move || SqliteNotesBackend::open(data.path().join("notes.sqlite3")).unwrap()
        });
    }

    #[test]
    fn exact_source_and_revision_survive_reopen() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("notes.sqlite3");
        let source = "---\ntitle: untouched\n---\n\nSetext title\n============\n\n中文 café 🚀  \n";
        let mut backend = SqliteNotesBackend::open(&path).unwrap();
        let revision = backend.commit(put("note", None, source)).unwrap();
        drop(backend);
        let mut reopened = SqliteNotesBackend::open(path).unwrap();
        let snapshot = reopened.load().unwrap();
        assert_eq!(snapshot.notes[0].markdown, source);
        assert_eq!(snapshot.notes[0].revision, revision);
        assert_eq!(reopened.pending_sync().unwrap().len(), 1);
        assert_eq!(reopened.changes_since(0).unwrap().len(), 1);
    }

    #[test]
    fn stale_connection_cannot_overwrite_and_old_receipt_cannot_clear_dirty() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("notes.sqlite3");
        let mut first = SqliteNotesBackend::open(&path).unwrap();
        let original = first.commit(put("note", None, "original")).unwrap();
        let mut second = SqliteNotesBackend::open(path).unwrap();
        let edited = first
            .commit(put("note", Some(original.clone()), "edited"))
            .unwrap();
        assert!(matches!(
            second.commit(put("note", Some(original.clone()), "stale")),
            Err(BackendError::Conflict { .. })
        ));
        assert!(!second.acknowledge(&NoteId::new("note"), &original).unwrap());
        assert_eq!(second.pending_sync().unwrap()[0].revision, edited);
        assert!(second.acknowledge(&NoteId::new("note"), &edited).unwrap());
        assert!(first.pending_sync().unwrap().is_empty());
        let history = first.history(&NoteId::new("note")).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].parent.as_ref(), Some(&original));
        assert_eq!(history[0].markdown.as_deref(), Some("original"));
    }

    #[test]
    fn deletion_retains_tombstone_history_and_sync_intent() {
        let data = tempfile::tempdir().unwrap();
        let mut backend = SqliteNotesBackend::open(data.path().join("notes.sqlite3")).unwrap();
        let original = backend.commit(put("note", None, "original")).unwrap();
        let deleted = backend
            .commit(BackendMutation::Delete {
                id: NoteId::new("note"),
                expected: original,
                deleted_at: 30,
            })
            .unwrap();
        assert!(backend.load().unwrap().notes.is_empty());
        let pending = backend.pending_sync().unwrap();
        assert_eq!(pending[0].revision, deleted);
        assert!(pending[0].deleted);
        assert!(backend.history(&NoteId::new("note")).unwrap()[1].deleted);
        let changes = backend.changes_since(1).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(changes[0].deleted);
        assert!(backend.read(&[NoteId::new("note")]).unwrap().is_empty());
        // A write that expects the tombstone's revision is stale.
        assert!(matches!(
            backend.commit(put("note", Some(deleted.clone()), "stale")),
            Err(BackendError::Conflict { actual: None, .. })
        ));
        // A create replaces the tombstone and continues its history.
        let created = backend.commit(put("note", None, "created again")).unwrap();
        let current = backend.read(&[NoteId::new("note")]).unwrap();
        assert_eq!(current[0].markdown, "created again");
        assert_eq!(current[0].revision, created);
        let history = backend.history(&NoteId::new("note")).unwrap();
        assert_eq!(history[2].parent.as_ref(), Some(&deleted));
    }

    #[test]
    fn failed_history_write_rolls_back_content_and_change_log() {
        let data = tempfile::tempdir().unwrap();
        let mut backend = SqliteNotesBackend::open(data.path().join("notes.sqlite3")).unwrap();
        let original = backend.commit(put("note", None, "original")).unwrap();
        backend.connection.execute_batch("CREATE TRIGGER reject_history BEFORE INSERT ON note_revisions BEGIN SELECT RAISE(ABORT, 'injected storage failure'); END;").unwrap();
        assert!(
            backend
                .commit(put("note", Some(original.clone()), "must roll back"))
                .is_err()
        );
        let current = &backend.load().unwrap().notes[0];
        assert_eq!(current.markdown, "original");
        assert_eq!(current.revision, original);
        assert_eq!(backend.changes_since(0).unwrap().len(), 1);
        assert_eq!(backend.history(&NoteId::new("note")).unwrap().len(), 1);
    }

    #[test]
    fn asset_identity_is_immutable_and_survives_reopen() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("notes.sqlite3");
        let mut backend = SqliteNotesBackend::open(&path).unwrap();
        let asset = Asset {
            id: AssetId("asset-one".into()),
            media_type: "image/png".into(),
            bytes: vec![1, 2, 3],
        };
        backend.write_asset(asset.clone()).unwrap();
        backend.write_asset(asset.clone()).unwrap();
        let mut changed = asset.clone();
        changed.bytes.push(4);
        assert!(matches!(
            backend.write_asset(changed),
            Err(BackendError::Invalid(_))
        ));
        drop(backend);
        let mut reopened = SqliteNotesBackend::open(path).unwrap();
        assert_eq!(reopened.read_asset(&asset.id).unwrap().bytes, asset.bytes);
    }
}
