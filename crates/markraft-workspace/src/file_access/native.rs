use super::bookmarks::{Backend, Resolved};
use objc2::{rc::Retained, runtime::Bool};
use objc2_foundation::{
    NSData, NSURL, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions,
};
use std::path::Path;

pub(super) struct Native;
pub(super) struct Scope(Retained<NSURL>);

impl Drop for Scope {
    fn drop(&mut self) {
        // Every successful start is balanced when the application state drops
        // its guard; switching notes never ends access for background work.
        unsafe { self.0.stopAccessingSecurityScopedResource() };
    }
}

fn bookmark(url: &NSURL) -> Result<Vec<u8>, String> {
    url.bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
        NSURLBookmarkCreationOptions::WithSecurityScope,
        None,
        None,
    )
    .map(|data| data.to_vec())
    .map_err(|e| e.localizedDescription().to_string())
}

impl Backend for Native {
    type Scope = Scope;

    fn capture(&self, path: &Path) -> Result<Vec<u8>, String> {
        let url = NSURL::from_file_path(path).ok_or("Invalid file path")?;
        bookmark(&url)
    }

    fn resolve(&self, bytes: &[u8]) -> Result<Resolved<Scope>, String> {
        let mut stale = Bool::NO;
        let url = unsafe {
            NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
                &NSData::with_bytes(bytes),
                NSURLBookmarkResolutionOptions::WithSecurityScope
                    | NSURLBookmarkResolutionOptions::WithoutUI
                    | NSURLBookmarkResolutionOptions::WithoutMounting,
                None,
                &mut stale,
            )
        }
        .map_err(|e| e.localizedDescription().to_string())?;
        if !unsafe { url.startAccessingSecurityScopedResource() } {
            return Err("Select this file or folder again to restore access".into());
        }
        let scope = Scope(url);
        let path = scope
            .0
            .to_file_path()
            .ok_or("The bookmark is not a local file")?;
        // Refresh only after starting access. Dropping the guard balances access
        // even when conversion or stale-bookmark refresh fails.
        let refreshed = if stale.as_bool() {
            Some(bookmark(&scope.0)?)
        } else {
            None
        };
        Ok(Resolved {
            path,
            refreshed,
            scope,
        })
    }
}
