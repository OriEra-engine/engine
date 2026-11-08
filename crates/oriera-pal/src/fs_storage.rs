//! Filesystem-backed [`Storage`] for native targets (issue #13).
//!
//! Web stays out of scope on purpose: OPFS/IndexedDB is its own issue,
//! and the browser build must not pull `std::fs` in.
//!
//! # Key → filename
//!
//! The mapping is one-way and deliberately lossy-looking:
//!
//! ```text
//! key "save/../slot 1"  ->  "save____slot_1-<fnv1a64 hex>"
//! ```
//!
//! Two properties follow, and both are acceptance criteria in #13:
//!
//! - **No traversal.** The emitted name only ever contains ASCII
//!   alphanumerics, `-` and `_`: `..`, `/`, `\`, `%`, `:` and drive
//!   letters all fold to `_`, and a leading dot cannot survive because
//!   `.` is not in the allowed set. `../escape`, `..`, `a/../../b` and
//!   `C:\Windows\x` are therefore ordinary names *inside* the root.
//! - **Bounded length.** `40 prefix bytes + '-' + 16 hex` regardless of
//!   key length, i.e. well under every POSIX/Windows path limit.
//!
//! The 64-bit FNV-1a suffix is what keeps the fold injective enough:
//! `a/b` and `a_b` share a prefix but not a name, and keys longer than
//! the prefix stay distinct. A collision would need two live keys with
//! the same FNV-1a hash; there is no dependency to buy SipHash here and
//! the store is keyed by opaque ids (asset hashes, save slots), not by
//! user-visible paths. State the limit, don't hide it.

use std::io;
use std::path::{Path, PathBuf};

use super::{Storage, StorageTier};

/// How many leading key bytes survive into the readable filename prefix.
const PREFIX_BYTES: usize = 40;

/// A [`Storage`] tier that persists under one root directory.
///
/// Reads and writes go through `std::fs` synchronously — this is the PC /
/// console boundary, called from load/save paths, not from the hot loop.
/// `Debug` prints the root only; it holds no open handles.
#[derive(Debug, Clone)]
pub struct FileStorage {
    root: PathBuf,
}

impl FileStorage {
    /// Open (and create, if needed) a store rooted at `root`.
    ///
    /// Every key written through this instance resolves to a file directly
    /// under this directory — never a subdirectory, never a parent.
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// The directory every blob lands in (useful for diagnostics and tests).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve the on-disk path a key maps to, without touching the disk.
    pub fn path_for(&self, key: &str) -> PathBuf {
        self.root.join(file_name(key))
    }

    /// Fallible read: distinguishes "absent" (`Ok(None)`) from "the OS
    /// refused" (`Err`). The [`Storage`] impl flattens the latter to `None`.
    pub fn try_read(&mut self, key: &str) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.path_for(key)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Fallible write. [`Storage::write`] swallows the error because its
    /// signature has nowhere to put it; callers that care (save games,
    /// run manifests) should use this one.
    pub fn try_write(&mut self, key: &str, data: &[u8]) -> io::Result<()> {
        std::fs::write(self.path_for(key), data)
    }
}

impl Storage for FileStorage {
    fn read(&mut self, key: &str) -> Option<Vec<u8>> {
        self.try_read(key).ok().flatten()
    }

    fn write(&mut self, key: &str, data: &[u8]) {
        // Trait signature is infallible by contract; see `try_write`.
        let _ = self.try_write(key, data);
    }

    fn remove(&mut self, key: &str) {
        // Absent is not an error (trait contract), and neither is a
        // permission problem we cannot report.
        let _ = std::fs::remove_file(self.path_for(key));
    }

    fn tier(&self) -> StorageTier {
        StorageTier::Local
    }
}

/// Map a key to a collision-resistant, traversal-proof filename.
fn file_name(key: &str) -> String {
    let mut prefix = String::with_capacity(PREFIX_BYTES + 17);
    for byte in key.bytes().take(PREFIX_BYTES) {
        let folded = byte.to_ascii_lowercase();
        let safe = folded.is_ascii_alphanumeric() || folded == b'-' || folded == b'_';
        prefix.push(if safe { folded as char } else { '_' });
    }
    let hash = fnv1a64(key.as_bytes());
    if prefix.is_empty() {
        // Empty key (and keys starting with >0 unsafe bytes produce '_',
        // so this is only the empty string).
        format!("k-{hash:016x}")
    } else {
        format!("{prefix}-{hash:016x}")
    }
}

/// 64-bit FNV-1a — dependency-free, stable across builds, unlike
/// `DefaultHasher` whose algorithm is not a compatibility promise.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, FileStorage) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileStorage::new(dir.path().join("store")).expect("store");
        (dir, store)
    }

    #[test]
    fn round_trip_write_read_remove_miss() {
        let (_dir, mut store) = temp_store();
        assert_eq!(store.tier(), StorageTier::Local);
        assert!(store.read("missing").is_none());

        store.write("slot/one", b"hello");
        assert_eq!(store.read("slot/one"), Some(b"hello".to_vec()));

        store.remove("slot/one");
        assert!(store.read("slot/one").is_none());
        // Removing an absent key is a no-op, not a panic.
        store.remove("slot/one");
    }

    #[test]
    fn data_survives_a_new_instance() {
        let (dir, mut store) = temp_store();
        store.write("save-1", b"\x00\xff binary \x80");
        drop(store);

        let mut reopened = FileStorage::new(dir.path().join("store")).expect("reopen");
        assert_eq!(
            reopened.read("save-1"),
            Some(b"\x00\xff binary \x80".to_vec())
        );
    }

    #[test]
    fn traversal_keys_never_escape_the_root() {
        let (dir, mut store) = temp_store();
        // The parent of the root is the tempdir; it must stay empty.
        let hostile = [
            "../escape",
            "..",
            "../../etc/passwd",
            "a/../../b",
            "%2e%2e%2fescape",
            "C:\\Windows\\System32\\cfg",
            "/absolute/root",
            "...",
        ];
        for key in hostile {
            store.write(key, b"payload");
            assert_eq!(
                store.read(key),
                Some(b"payload".to_vec()),
                "key {key:?} must round-trip, not error out"
            );
            let path = store.path_for(key);
            // 1. the resolved path is a direct child of the root;
            // 2. its file name holds no separator and no leading dot.
            assert_eq!(path.parent(), Some(store.root()));
            let name = path.file_name().expect("file name").to_string_lossy();
            assert!(
                !name.contains(['/', '\\', ':']),
                "name {name:?} carries a separator"
            );
            assert!(!name.starts_with('.'), "name {name:?} starts with a dot");
        }
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read tempdir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "nothing may be created beside the store: {entries:?}"
        );
    }

    #[test]
    fn long_keys_get_bounded_file_names() {
        let (_dir, mut store) = temp_store();
        let key = "x".repeat(300);
        store.write(&key, b"long");
        assert_eq!(store.read(&key), Some(b"long".to_vec()));
        let name = store
            .path_for(&key)
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        assert_eq!(name.len(), PREFIX_BYTES + 1 + 16, "prefix + '-' + 16 hex");
    }

    #[test]
    fn empty_key_round_trips() {
        let (_dir, mut store) = temp_store();
        store.write("", b"empty");
        assert_eq!(store.read(""), Some(b"empty".to_vec()));
        let name = store
            .path_for("")
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("k-"),
            "unnamed key gets the k- prefix, got {name:?}"
        );
    }

    #[test]
    fn keys_sharing_a_prefix_stay_distinct() {
        let (_dir, mut store) = temp_store();
        // Same folded prefix ("a_b"), different keys — the hash suffix is
        // what keeps them apart.
        store.write("a/b", b"slash");
        store.write("a_b", b"underscore");
        assert_eq!(store.read("a/b"), Some(b"slash".to_vec()));
        assert_eq!(store.read("a_b"), Some(b"underscore".to_vec()));
        assert_ne!(store.path_for("a/b"), store.path_for("a_b"));
    }
}
