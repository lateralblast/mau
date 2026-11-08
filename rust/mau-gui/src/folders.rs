//! Keeping access to the folders the user chose, across launches (macOS, for the sandboxed app).
//!
//! A sandboxed app can only use a folder the user chose, and that choice ends with the app. A
//! security-scoped bookmark is made from the folder while it is chosen, saved in the app's data folder,
//! and turned back into access at each launch (`restore`). Other systems have no sandbox: there these do
//! nothing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One remembered folder: its path (for the list, and to avoid saving it twice) and its bookmark, hex encoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Remembered {
    path: String,
    bookmark: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok()).collect()
}

fn load(store: &Path) -> Vec<Remembered> {
    std::fs::read_to_string(store).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn save(store: &Path, folders: &[Remembered]) -> Result<(), String> {
    if let Some(parent) = store.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(folders).map_err(|e| e.to_string())?;
    std::fs::write(store, text).map_err(|e| e.to_string())
}

/// Remember a folder the user chose: its bookmark is saved in `store` and access to it is kept for this launch.
pub fn remember(path: &str, store: &Path) -> Result<(), String> {
    let bookmark = imp::create(path)?;
    let mut folders = load(store);
    folders.retain(|f| f.path != path);
    folders.push(Remembered { path: path.to_string(), bookmark: hex(&bookmark) });
    save(store, &folders)?;
    imp::access(path, &bookmark).map(|_| ())
}

/// At launch: turn each remembered bookmark back into access to its folder. A bookmark that has gone stale (the
/// folder was moved) is made again while the access is held. Returns how many folders are open to the app.
pub fn restore(store: &Path) -> usize {
    let mut folders = load(store);
    let mut opened = 0;
    for folder in folders.iter_mut() {
        let Some(bookmark) = unhex(&folder.bookmark) else { continue };
        match imp::access_bookmark(&bookmark) {
            Ok((path, stale)) => {
                opened += 1;
                if stale {
                    if let Ok(fresh) = imp::create(&path) {
                        folder.path = path;
                        folder.bookmark = hex(&fresh);
                    }
                }
            }
            Err(_) => {}
        }
    }
    let _ = save(store, &folders);
    opened
}

/// The folders remembered, by path.
pub fn list(store: &Path) -> Vec<String> {
    load(store).into_iter().map(|f| f.path).collect()
}

/// Forget every remembered folder (the record is removed). The access the app already has lasts until it quits:
/// it is not kept for the next launch.
pub fn forget(store: &Path) -> Result<(), String> {
    match std::fs::remove_file(store) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// Where the remembered folders are kept, in the app's data folder.
pub fn store_in(data_folder: Option<PathBuf>) -> Option<PathBuf> {
    data_folder.map(|folder| folder.join("folders.json"))
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2_foundation::{
        NSData, NSString, NSURL, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions,
    };

    /// A bookmark for the folder at `path`, made while the app can use it.
    pub fn create(path: &str) -> Result<Vec<u8>, String> {
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        // the folder has to be opened to the app before a bookmark can be made from it
        let open = unsafe { url.startAccessingSecurityScopedResource() };
        let data = url.bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
            NSURLBookmarkCreationOptions::WithSecurityScope,
            None,
            None,
        );
        if open {
            unsafe { url.stopAccessingSecurityScopedResource() };
        }
        data.map(|d| d.to_vec()).map_err(|e| e.localizedDescription().to_string())
    }

    /// Open the folder of a bookmark to the app, and keep it open (for the rest of the launch).
    pub fn access_bookmark(bookmark: &[u8]) -> Result<(String, bool), String> {
        let data = NSData::with_bytes(bookmark);
        let mut stale = Bool::NO;
        let url: Retained<NSURL> = unsafe {
            NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
                &data,
                NSURLBookmarkResolutionOptions::WithSecurityScope,
                None,
                &mut stale,
            )
        }
        .map_err(|e| e.localizedDescription().to_string())?;
        let path = url.path().map(|p| p.to_string()).unwrap_or_default();
        if !unsafe { url.startAccessingSecurityScopedResource() } {
            return Err(format!("could not open {path}"));
        }
        // kept open until the app quits: the access is for the whole launch
        std::mem::forget(url);
        Ok((path, stale.as_bool()))
    }

    /// Open the folder at `path` with its bookmark (used right after the folder is chosen).
    pub fn access(path: &str, bookmark: &[u8]) -> Result<(), String> {
        let _ = path;
        access_bookmark(bookmark).map(|_| ())
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn create(_path: &str) -> Result<Vec<u8>, String> {
        Ok(Vec::new())
    }

    pub fn access_bookmark(_bookmark: &[u8]) -> Result<(String, bool), String> {
        Ok((String::new(), false))
    }

    pub fn access(_path: &str, _bookmark: &[u8]) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bookmark_is_saved_as_hex_and_read_back() {
        let bytes = [0u8, 15, 255, 128];
        assert_eq!(hex(&bytes), "000fff80");
        assert_eq!(unhex("000fff80"), Some(bytes.to_vec()));
        assert_eq!(unhex("abc"), None, "an odd length is refused");
        assert_eq!(unhex("zz"), None);
    }
}
