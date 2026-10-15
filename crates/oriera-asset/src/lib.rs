//! L4 Asset — resource handle registry.

use std::any::{Any, TypeId};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssetHandle(pub u64);

#[derive(Default)]
pub struct AssetStore {
    next: u64,
    data: HashMap<(TypeId, u64), Box<dyn Any + Send + Sync>>,
}

impl AssetStore {
    pub fn new() -> Self { Self::default() }

    pub fn load<T: Any + Send + Sync + 'static>(&mut self, data: T) -> AssetHandle {
        let id = AssetHandle(self.next);
        self.next += 1;
        self.data.insert((TypeId::of::<T>(), id.0), Box::new(data));
        id
    }

    pub fn get<T: Any + Send + Sync + 'static>(&self, handle: AssetHandle) -> Option<&T> {
        self.data.get(&(TypeId::of::<T>(), handle.0))?.downcast_ref::<T>()
    }

    pub fn len(&self) -> usize { self.data.len() }
    pub fn is_empty(&self) -> bool { self.data.is_empty() }
}
