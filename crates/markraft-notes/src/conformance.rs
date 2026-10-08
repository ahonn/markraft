//! Checks that a [`NotesBackend`] keeps the contract that the storage worker
//! relies on. A host runs them in its own tests, against its real storage.
//!
//! ```ignore
//! markraft_notes::conformance::check(|| {
//!     let directory = tempfile::tempdir().unwrap();
//!     move || MyBackend::open(directory.path().join("notes.db")).unwrap()
//! });
//! ```
use crate::{
    Asset, AssetId, BackendError, BackendMutation, BackendNote, NoteId, NotesBackend,
    StorageRevision,
};

/// Front matter, a setext heading, CRLF, trailing spaces, non-ASCII text, and no
/// final newline: everything a store could be tempted to normalize.
const SOURCE: &str = "---\ntitle: kept\n---\n\nHeading\r\n=======\r\n\n中文 café  \n\ttab";

fn put(id: &NoteId, expected: Option<&StorageRevision>, markdown: &str) -> BackendMutation {
    BackendMutation::Put {
        id: id.clone(),
        expected: expected.cloned(),
        markdown: markdown.into(),
        title: Some("A title".into()),
        logical_key: Some("daily:2026-10-08".into()),
        created_at: 11,
        updated_at: 22,
        pinned: true,
    }
}

fn one(backend: &mut impl NotesBackend, id: &NoteId) -> Option<BackendNote> {
    let mut notes = backend.read(std::slice::from_ref(id)).expect("read by ID");
    assert!(notes.len() <= 1, "read returned a note more than once");
    let note = notes.pop();
    if let Some(note) = &note {
        assert_eq!(&note.id, id, "read returned a note that was not asked for");
    }
    note
}

fn conflict(result: Result<StorageRevision, BackendError>, what: &str) -> Option<StorageRevision> {
    match result {
        Err(BackendError::Conflict { actual, .. }) => actual,
        other => panic!("{what} must be a conflict, got {other:?}"),
    }
}

/// Run every check. `store` makes a new, empty store and returns a function
/// that opens it. Each call of that function must give an independent handle to
/// the same store, the way a second connection or a restarted process would.
pub fn check<B: NotesBackend, O: FnMut() -> B>(mut store: impl FnMut() -> O) {
    records_round_trip(&mut store());
    writes_are_compare_and_swap(&mut store());
    deletion_leaves_an_absent_note(&mut store());
    assets_keep_their_content(&mut store());
}

fn records_round_trip<B: NotesBackend>(open: &mut impl FnMut() -> B) {
    let id = NoteId::new("round-trip");
    let other = NoteId::new("other");
    let mut writer = open();
    assert!(
        writer.load().expect("load an empty store").notes.is_empty(),
        "a new store must hold no notes"
    );
    let revision = writer
        .commit(put(&id, None, SOURCE))
        .expect("create a note");
    writer
        .commit(put(&other, None, "other"))
        .expect("create a second note");
    drop(writer);
    let mut reader = open();
    let loaded = reader.load().expect("load").notes;
    assert_eq!(loaded.len(), 2, "load must return every live note once");
    for note in [
        loaded
            .into_iter()
            .find(|note| note.id == id)
            .expect("load returns the note"),
        one(&mut reader, &id).expect("read returns the note"),
    ] {
        assert_eq!(
            note.markdown, SOURCE,
            "Markdown must be stored byte for byte"
        );
        assert_eq!(
            note.revision, revision,
            "a reopened store must return the committed revision"
        );
        assert_eq!(note.title.as_deref(), Some("A title"));
        assert_eq!(note.logical_key.as_deref(), Some("daily:2026-10-08"));
        assert_eq!(
            (note.created_at, note.updated_at, note.pinned),
            (11, 22, true)
        );
    }
    let missing = NoteId::new("missing");
    let found = reader
        .read(&[missing.clone(), other.clone()])
        .expect("read by ID");
    assert_eq!(
        found.len(),
        1,
        "read must leave out a note that does not exist"
    );
    assert_eq!(found[0].id, other);
    assert!(one(&mut reader, &missing).is_none());
}

fn writes_are_compare_and_swap<B: NotesBackend>(open: &mut impl FnMut() -> B) {
    let id = NoteId::new("contended");
    let mut first = open();
    let mut second = open();
    let created = first
        .commit(put(&id, None, "created"))
        .expect("create a note");
    let actual = conflict(
        second.commit(put(&id, None, "created twice")),
        "a second create",
    );
    assert_eq!(
        actual.as_ref(),
        Some(&created),
        "a conflict must report the live revision"
    );
    let edited = first
        .commit(put(&id, Some(&created), "edited"))
        .expect("update with the current revision");
    assert_ne!(edited, created, "every commit must return a new revision");
    let actual = conflict(
        second.commit(put(&id, Some(&created), "stale")),
        "a write that expects an old revision",
    );
    assert_eq!(actual.as_ref(), Some(&edited));
    let stored = one(&mut second, &id).expect("the note survives a refused write");
    assert_eq!(
        stored.markdown, "edited",
        "a refused write must change nothing"
    );
    assert_eq!(stored.revision, edited);
    second
        .commit(put(&id, Some(&edited), "from the second handle"))
        .expect("update from another handle with the current revision");
    let absent = NoteId::new("never-created");
    let unknown = StorageRevision("no-such-revision".into());
    let actual = conflict(
        first.commit(put(&absent, Some(&unknown), "x")),
        "an update of a note that does not exist",
    );
    assert_eq!(actual, None, "an absent note has no live revision");
    assert!(
        one(&mut first, &absent).is_none(),
        "a refused update must not create the note"
    );
}

fn deletion_leaves_an_absent_note<B: NotesBackend>(open: &mut impl FnMut() -> B) {
    let id = NoteId::new("deleted");
    let mut backend = open();
    let created = backend
        .commit(put(&id, None, "created"))
        .expect("create a note");
    let edited = backend
        .commit(put(&id, Some(&created), "edited"))
        .expect("update a note");
    let delete = |expected: &StorageRevision| BackendMutation::Delete {
        id: id.clone(),
        expected: expected.clone(),
        deleted_at: 33,
    };
    conflict(
        backend.commit(delete(&created)),
        "a delete that expects an old revision",
    );
    assert!(
        one(&mut backend, &id).is_some(),
        "a refused delete must keep the note"
    );
    let deleted = backend
        .commit(delete(&edited))
        .expect("delete with the current revision");
    assert!(
        one(&mut backend, &id).is_none(),
        "read must leave out a deleted note"
    );
    let mut reopened = open();
    assert!(
        reopened.load().expect("load").notes.is_empty(),
        "load must leave out a deleted note"
    );
    for stale in [&edited, &deleted] {
        let actual = conflict(
            reopened.commit(put(&id, Some(stale), "stale")),
            "an update of a deleted note",
        );
        assert_eq!(actual, None, "a deleted note has no live revision");
    }
    conflict(reopened.commit(delete(&edited)), "a second delete");
    let revived = reopened
        .commit(put(&id, None, "created again"))
        .expect("a create must replace a deleted note");
    let stored = one(&mut reopened, &id).expect("the note is live again");
    assert_eq!(stored.markdown, "created again");
    assert_eq!(stored.revision, revived);
}

fn assets_keep_their_content<B: NotesBackend>(open: &mut impl FnMut() -> B) {
    let mut writer = open();
    if !writer.capabilities().assets {
        return;
    }
    let asset = Asset::new("image/png", vec![0, 159, 146, 150, 255]);
    writer.write_asset(asset.clone()).expect("write an asset");
    writer
        .write_asset(asset.clone())
        .expect("writing the same asset again must succeed");
    let mut reader = open();
    let stored = reader
        .read_asset(&asset.id)
        .expect("read an asset from another handle");
    assert_eq!(
        stored.bytes, asset.bytes,
        "asset bytes must be stored exactly"
    );
    assert_eq!(stored.media_type, asset.media_type);
    assert_eq!(stored.id, asset.id);
    // The component never sends different bytes under one ID. A store that is
    // asked to must not replace what the ID already names.
    let forged = Asset {
        id: asset.id.clone(),
        media_type: asset.media_type.clone(),
        bytes: vec![1],
    };
    let _ = reader.write_asset(forged);
    assert_eq!(
        writer.read_asset(&asset.id).expect("read an asset").bytes,
        asset.bytes,
        "an asset ID must never come to name different content"
    );
    assert!(
        reader.read_asset(&AssetId("missing".into())).is_err(),
        "reading an asset that does not exist must fail"
    );
}
