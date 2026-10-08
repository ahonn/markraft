//! A notes service consumer with no GPUI application or editor view.

use markraft_notes::{NotesConfig, NotesError, NotesLibrary};
use markraft_sqlite_example::SqliteNotesBackend;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let sqlite = match arguments.as_slice() {
        [] => false,
        [flag, value] if flag == "--storage" && value == "markdown" => false,
        [flag, value] if flag == "--storage" && value == "sqlite" => true,
        _ => return Err("Usage: markraft-notes-consumer [--storage markdown|sqlite]".into()),
    };
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("notes.sqlite3");
    let notes_directory = temporary.path().join("notes");
    let state_directory = temporary.path().join("state");
    if !sqlite {
        std::fs::create_dir_all(&notes_directory)?;
        std::fs::create_dir_all(&state_directory)?;
    }
    let config = NotesConfig::new(notes_directory, state_directory);

    let mut library = if sqlite {
        NotesLibrary::from_backend(Box::new(SqliteNotesBackend::open(&database)?))?
    } else {
        NotesLibrary::open(config.clone())?
    };
    let id = library.create("# Consumer note\n\nCreated without a view.\n")?;
    let snapshot = library.note(&id)?;
    library.edit(
        &id,
        snapshot.revision,
        "# Consumer note\n\nEdited without a view.\n",
    )?;
    assert!(matches!(
        library.edit(
            &id,
            snapshot.revision,
            "An outdated caller must not overwrite this note."
        ),
        Err(NotesError::StaleRevision { .. })
    ));
    let matches = library.search("Consumer note")?;
    assert!(matches.iter().any(|note| note.id == id));
    futures::executor::block_on(library.flush())?;
    let saved = library.note(&id)?;
    if sqlite {
        assert!(saved.path.is_none());
        assert!(
            SqliteNotesBackend::open(&database)?
                .pending_sync()?
                .iter()
                .any(|change| change.id == id)
        );
        assert!(!config.notes_dir.exists());
    } else {
        let path = saved.path.as_ref().expect("flush assigns a file path");
        assert!(std::fs::read_to_string(path)?.contains("Edited without a view."));
    }
    futures::executor::block_on(library.close())?;

    // Closing releases the storage lock before the next service opens it.
    let mut reopened = if sqlite {
        NotesLibrary::from_backend(Box::new(SqliteNotesBackend::open(&database)?))?
    } else {
        NotesLibrary::open(config)?
    };
    let restored = reopened.search("Consumer note")?;
    assert!(restored.iter().any(|note| note.id == id));
    assert!(
        reopened
            .note(&id)?
            .markdown
            .contains("Edited without a view.")
    );
    futures::executor::block_on(reopened.close())?;
    println!(
        "Created, searched, edited, saved, closed, and reopened a note without GPUI using {}.",
        if sqlite { "SQLite" } else { "Markdown" }
    );
    println!("All sample data lived in {}.", temporary.path().display());
    Ok(())
}
