//! L4 landing — the hot-swap wiring declared in `ARCHITECTURE.md` as
//! the L8 landing pad. The application shell owns the
//! [`oriera_asset::AssetStore`]; after each scheduler tick it calls
//! [`apply_pending`], which drains [`crate::PendingSwaps`] and lands
//! every validated asset behind its logical name via `insert_named`:
//! same name, same handle, new payload — no entity rebuilds.

use crate::{AssetSwap, PendingSwaps};
use oriera_asset::AssetStore;
use oriera_ecs::World;

/// The typed payload stored in the L4 registry. Consumers resolve
/// generated content with
/// `store.get_named::<GenAsset>("npc_prop")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenAsset {
    pub format: crate::AssetFormat,
    pub bytes: Vec<u8>,
    pub provider_id: String,
    pub model: String,
    /// True when these bytes are a placeholder standing in for a
    /// failed generation (ADR 0003 criterion 3) — renderers may want
    /// to flag it in debug views.
    pub placeholder: bool,
}

impl From<&AssetSwap> for GenAsset {
    fn from(swap: &AssetSwap) -> Self {
        Self {
            format: swap.asset.format,
            bytes: swap.asset.payload.clone(),
            provider_id: swap.asset.provider_id.clone(),
            model: swap.asset.model.clone(),
            placeholder: swap.placeholder,
        }
    }
}

/// Drain [`PendingSwaps`] into the store. Returns how many assets
/// landed (placeholder or not). Idempotent per tick: an empty or
/// missing resource lands nothing.
pub fn apply_pending(store: &mut AssetStore, world: &mut World) -> usize {
    let swaps = match world.resource_mut::<PendingSwaps>() {
        Some(pending) => std::mem::take(&mut pending.queue),
        None => return 0,
    };
    let landed = swaps.len();
    for swap in &swaps {
        store.insert_named(&swap.name, GenAsset::from(swap));
    }
    landed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetFormat, GeneratedAsset};

    fn swap(name: &str, payload: Vec<u8>) -> AssetSwap {
        AssetSwap {
            name: name.into(),
            asset: GeneratedAsset {
                format: AssetFormat::Glb,
                payload,
                provider_id: "test".into(),
                model: String::new(),
                usage: None,
            },
            placeholder: false,
        }
    }

    #[test]
    fn lands_behind_the_logical_name() {
        let mut w = World::new();
        let mut store = AssetStore::new();
        w.resource_or_default::<PendingSwaps>()
            .queue
            .push(swap("npc_prop", vec![1, 2, 3]));

        assert_eq!(apply_pending(&mut store, &mut w), 1);
        let landed = store.get_named::<GenAsset>("npc_prop").unwrap();
        assert_eq!(landed.bytes, vec![1, 2, 3]);
        assert!(!landed.placeholder);

        // Second application drains nothing: idempotent per tick.
        assert_eq!(apply_pending(&mut store, &mut w), 0);
    }

    #[test]
    fn regeneration_keeps_one_entry_and_swaps_payload() {
        let mut w = World::new();
        let mut store = AssetStore::new();
        let q = w.resource_or_default::<PendingSwaps>();
        q.queue.push(swap("hero", vec![1]));
        apply_pending(&mut store, &mut w);

        w.resource_or_default::<PendingSwaps>()
            .queue
            .push(swap("hero", vec![2, 2]));
        apply_pending(&mut store, &mut w);

        assert_eq!(store.len(), 1); // no orphaned old payload
        assert_eq!(
            store.get_named::<GenAsset>("hero").unwrap().bytes,
            vec![2, 2]
        );
    }

    #[test]
    fn bare_world_lands_nothing() {
        let mut w = World::new();
        let mut store = AssetStore::new();
        assert_eq!(apply_pending(&mut store, &mut w), 0);
        assert!(store.is_empty());
    }
}
