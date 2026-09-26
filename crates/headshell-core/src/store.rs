//! Files the core keeps in the data directory beside the library: the cover
//! cache (D-076) and the lyrics cache (D-078). What they share is here, once
//! — a second copy of "write whole or not at all" would drift (D-051):
//!
//! - **A torn write.** A file is written to a temporary name and renamed
//!   into place; a crash leaves the old file or the new one, never half.
//! - **Two writers in one process.** The background worker and a command
//!   write from two threads: every write has its own temporary name.
//! - **Knowing its own.** A cache removes only names it writes — a content
//!   hash, an index, a temporary of either — never a file it did not name.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::diag::Stage;
use crate::error::{Result, io_err};

/// Writes next to the target and renames: the target is the old file or the
/// new one, never a torn one.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], stage: Stage) -> Result<()> {
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = path.with_extension(format!("tmp-{}-{sequence}", std::process::id()));
    std::fs::write(&temporary, bytes).map_err(|source| io_err(stage, &temporary, source))?;
    std::fs::rename(&temporary, path).map_err(|source| {
        let _ = std::fs::remove_file(&temporary);
        io_err(stage, path, source)
    })
}

/// The name a content-addressed file goes by: the SHA-256 of its bytes, in
/// lower-case hex.
pub(crate) fn content_name(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Is `stem` a name [`content_name`] gives?
pub(crate) fn is_content_name(stem: &str) -> bool {
    stem.len() == 64
        && stem
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A write a cache began and never finished (`index.tmp-<pid>-<n>`,
/// `<64 hex>.tmp-<pid>-<n>`): a crash in the middle leaves one behind.
pub(crate) fn is_our_temporary(name: &str) -> bool {
    let Some((stem, suffix)) = name.split_once(".tmp-") else {
        return false;
    };
    let numbered = suffix.split_once('-').is_some_and(|(pid, n)| {
        !pid.is_empty()
            && !n.is_empty()
            && (pid.bytes().chain(n.bytes())).all(|b| b.is_ascii_digit())
    });
    (stem == "index" || is_content_name(stem)) && numbered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn only_names_a_cache_writes_are_its_temporaries() {
        let hash = "a".repeat(64);
        assert!(is_our_temporary("index.tmp-4242-7"));
        assert!(is_our_temporary(&format!("{hash}.tmp-4242-0")));
        assert!(
            !is_our_temporary("index.tmp-4242"),
            "not a name a cache writes"
        );
        assert!(!is_our_temporary("notes.tmp-1-2"));
        assert!(!is_our_temporary(&format!("{hash}.tmp-x-1")));
        assert!(is_content_name(&content_name(b"x")));
        assert!(
            !is_content_name(&"A".repeat(64)),
            "upper-case hex is not ours"
        );
    }

    #[test]
    fn a_write_lands_whole_and_leaves_no_temporary() {
        let dir = TempDir::new("store-write");
        let path = dir.path().join("index.json");
        write_atomic(&path, b"one", Stage::LyricsStore).unwrap();
        write_atomic(&path, b"two", Stage::LyricsStore).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["index.json"]);
    }
}
