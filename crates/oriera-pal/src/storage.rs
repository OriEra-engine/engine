//! Local persistence boundary (L1).
//!
//! Contract: `oriera.pal.storage.v1` (Table B-1: OpenAssetStream —
//! supports cancel and range resume). Web target uses OPFS/IndexedDB,
//! native targets use direct FS, cloud sync rides on top of L1 net.

/// Where a stored blob lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageTier {
    /// Ephemeral — may be dropped between sessions (browser memory).
    Volatile,
    /// Local persistent storage.
    Local,
}

/// Key-value byte storage with tiered persistence.
pub trait Storage {
    /// Read a key. Returns None when absent.
    fn read(&mut self, key: &str) -> Option<Vec<u8>>;

    /// Write a key. Content-addressable layers (L4) hash before calling.
    fn write(&mut self, key: &str, data: &[u8]);

    /// Delete a key; no error when absent.
    fn remove(&mut self, key: &str);

    fn tier(&self) -> StorageTier;
}

/// In-memory storage for tests and volatile sessions.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    map: std::collections::HashMap<String, Vec<u8>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Storage for MemoryStorage {
    fn read(&mut self, key: &str) -> Option<Vec<u8>> {
        self.map.get(key).cloned()
    }
    fn write(&mut self, key: &str, data: &[u8]) {
        self.map.insert(key.to_string(), data.to_vec());
    }
    fn remove(&mut self, key: &str) {
        self.map.remove(key);
    }
    fn tier(&self) -> StorageTier {
        StorageTier::Volatile
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_storage_round_trip() {
        let mut s = MemoryStorage::new();
        assert!(s.read("missing").is_none());
        s.write("a", b"hello");
        assert_eq!(s.read("a"), Some(b"hello".to_vec()));
        s.remove("a");
        assert!(s.read("a").is_none());
        assert_eq!(s.tier(), StorageTier::Volatile);
    }
}
