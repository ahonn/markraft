//! A notes service consumer with no GPUI application or editor view.

use markraft_notes::{NotesConfig, NotesError, NotesLibrary};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let notes_directory = temporary.path().join("notes");
    let state_directory = temporary.path().join("state");
    std::fs::create_dir_all(&notes_directory)?;
    std::fs::create_dir_all(&state_directory)?;
    let config = NotesConfig::new(notes_directory, state_directory);

    let mut library = NotesLibrary::open(config.clone())?;
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
    let path = saved.path.as_ref().expect("flush assigns a file path");
    assert!(std::fs::read_to_string(path)?.contains("Edited without a view."));
    futures::executor::block_on(library.close())?;

    // Closing releases the storage lock before the next service opens it.
    let mut reopened = NotesLibrary::open(config)?;
    let restored = reopened.search("Consumer note")?;
    assert!(restored.iter().any(|note| note.id == id));
    assert!(
        reopened
            .note(&id)?
            .markdown
            .contains("Edited without a view.")
    );
    futures::executor::block_on(reopened.close())?;
    println!("Created, searched, edited, saved, closed, and reopened a note without GPUI.");
    println!("All sample data lived in {}.", temporary.path().display());
    Ok(())
}
