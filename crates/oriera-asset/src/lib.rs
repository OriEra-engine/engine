//! L4 Asset — resource handle registry.
//!
//! v0.1 registry plus the two operations AI-native content needs today:
//! **named hot swap** and **unload**. Generation pipelines (L8, 2028)
//! re-emit assets by logical name (`npc_face_v2`, `bgm_rain_loop`);
//! `insert_named` swaps the payload behind the name while callers keep
//! resolving through [`AssetStore::get_named`] — no entity rebuilds, no
//! handle churn. Content-addressable handles (`hash == identity`,
//! book T7) remain the v0.2 plan.

use std::any::{Any, TypeId};
use std::collections::HashMap;

pub mod glb;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssetHandle(pub u64);

#[derive(Default)]
pub struct AssetStore {
    next: u64,
    data: HashMap<(TypeId, u64), Box<dyn Any + Send + Sync>>,
    /// Logical name → handle, type-tagged so names can repeat across
    /// asset kinds ("hero" mesh vs "hero" texture).
    names: HashMap<(TypeId, String), AssetHandle>,
}

impl AssetStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load<T: Any + Send + Sync + 'static>(&mut self, data: T) -> AssetHandle {
        let id = AssetHandle(self.next);
        self.next += 1;
        self.data.insert((TypeId::of::<T>(), id.0), Box::new(data));
        id
    }

    pub fn get<T: Any + Send + Sync + 'static>(&self, handle: AssetHandle) -> Option<&T> {
        self.data
            .get(&(TypeId::of::<T>(), handle.0))?
            .downcast_ref::<T>()
    }

    /// Register `data` under a logical `name`. Re-registering the same
    /// (type, name) atomically swaps the payload: every later
    /// `get_named` sees the new bytes through the same name, so a
    /// regenerated asset lands without touching any entity that uses
    /// it. Returns the stable handle for the name.
    pub fn insert_named<T: Any + Send + Sync + 'static>(
        &mut self,
        name: &str,
        data: T,
    ) -> AssetHandle {
        let key = (TypeId::of::<T>(), name.to_string());
        let handle = match self.names.get(&key) {
            Some(&h) => h,
            None => {
                let h = AssetHandle(self.next);
                self.next += 1;
                self.names.insert(key, h);
                h
            }
        };
        self.data
            .insert((TypeId::of::<T>(), handle.0), Box::new(data));
        handle
    }

    /// Resolve the current payload behind a logical name. Always reads
    /// the latest `insert_named` — that is the hot-swap contract.
    pub fn get_named<T: Any + Send + Sync + 'static>(&self, name: &str) -> Option<&T> {
        let key = (TypeId::of::<T>(), name.to_string());
        let handle = self.names.get(&key)?;
        self.get::<T>(*handle)
    }

    /// Drop a handle's payload (name registrations for other handles
    /// stay). Evicting an asset the renderer still references is a
    /// use-`None` situation, not a crash — callers re-load on miss.
    pub fn unload(&mut self, handle: AssetHandle) {
        self.data.retain(|(_, id), _| *id != handle.0);
        self.names.retain(|_, h| *h != handle);
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Texture {
        rev: u32,
    }

    #[test]
    fn named_assets_hot_swap_behind_the_same_name() {
        let mut store = AssetStore::new();
        let h1 = store.insert_named("npc_face", Texture { rev: 1 });
        assert_eq!(
            store.get_named::<Texture>("npc_face"),
            Some(&Texture { rev: 1 })
        );

        // Regeneration: same name, new payload, SAME handle.
        let h2 = store.insert_named("npc_face", Texture { rev: 2 });
        assert_eq!(h1, h2);
        assert_eq!(
            store.get_named::<Texture>("npc_face"),
            Some(&Texture { rev: 2 })
        );
        assert_eq!(store.len(), 1); // no orphaned old payload
    }

    #[test]
    fn names_are_independent_per_type() {
        #[derive(Debug, PartialEq)]
        struct Mesh {
            tris: u32,
        }

        let mut store = AssetStore::new();
        store.insert_named("hero", Texture { rev: 1 });
        store.insert_named("hero", Mesh { tris: 900 });
        assert_eq!(
            store.get_named::<Texture>("hero"),
            Some(&Texture { rev: 1 })
        );
        assert_eq!(store.get_named::<Mesh>("hero"), Some(&Mesh { tris: 900 }));
    }

    #[test]
    fn unload_frees_payload_and_registration() {
        let mut store = AssetStore::new();
        let h = store.load(Texture { rev: 7 });
        assert!(store.get::<Texture>(h).is_some());
        store.unload(h);
        assert!(store.get::<Texture>(h).is_none());
        assert!(store.is_empty());

        // A miss is None, never a panic — callers re-load on demand.
        let named = store.insert_named("bgm", Texture { rev: 9 });
        store.unload(named);
        assert_eq!(store.get_named::<Texture>("bgm"), None);
    }
}
