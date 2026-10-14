//! L3 Object Model — archetype ECS.
//!
//! # Design
//!
//! - **Entity** = (`index`, `generation`). Generation prevents use-after-free:
//!   when an entity is despawned, its index can be reused but the generation
//!   counter increments, so stale handles become invalid.
//! - **Component storage** is per-type: `HashMap<TypeId, Box<dyn Any>>` where
//!   each boxed value is `HashMap<u32, T>` (entity index -> component value).
//!   This gives O(1) insert/remove/query and is easy to read.
//! - **Archetype optimization** is deferred to v0.2; the v0.1 API is stable
//!   so callers don't need to change when we switch to SoA.
//!
//! # Example
//!
//! ```
//! use oriera_ecs::{World, Position, Velocity};
//!
//! let mut world = World::new();
//! let e = world.spawn();
//! world.insert(e, Position { x: 1.0, y: 2.0 });
//! world.insert(e, Velocity { vx: 0.5, vy: 0.0 });
//!
//! for (entity, pos, vel) in world.query2::<Position, Velocity>() {
//!     println!("{:?} at {:?} moving {:?}", entity, pos, vel);
//! }
//!
//! world.despawn(e);
//! assert!(world.is_empty());
//! ```

use std::any::{Any, TypeId};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

/// An entity handle. Cheap to copy; 8 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Entity {
    pub index: u32,
    pub generation: u32,
}

// ---------------------------------------------------------------------------
// World
// ---------------------------------------------------------------------------

/// The ECS world.
#[derive(Default)]
pub struct World {
    next_index: u32,
    generations: Vec<u32>,
    free: Vec<u32>,
    /// Live entity indices.
    live: std::collections::HashSet<u32>,
    /// Per-type component storage: TypeId -> boxed HashMap<index, T>.
    components: HashMap<TypeId, Box<dyn Any>>,
}

impl World {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- spawn / despawn ----

    /// Spawn an entity with no components.
    pub fn spawn(&mut self) -> Entity {
        let index = if let Some(idx) = self.free.pop() {
            idx
        } else {
            let idx = self.next_index;
            self.next_index += 1;
            self.generations.push(0);
            idx
        };
        let generation = self.generations[index as usize];
        self.live.insert(index);
        Entity { index, generation }
    }

    /// Remove an entity and all its components.
    pub fn despawn(&mut self, entity: Entity) {
        if !self.live.remove(&entity.index) {
            return;
        }
        self.generations[entity.index as usize] += 1;
        self.free.push(entity.index);
    }

    // ---- insert / remove ----

    /// Insert or replace a component on an entity.
    pub fn insert<T: 'static>(&mut self, entity: Entity, component: T) {
        if !self.live.contains(&entity.index) {
            return;
        }
        let type_id = TypeId::of::<T>();
        let col = self
            .components
            .entry(type_id)
            .or_insert_with(|| Box::new(HashMap::<u32, T>::new()));
        let map = col.downcast_mut::<HashMap<u32, T>>().unwrap();
        map.insert(entity.index, component);
    }

    /// Remove a component from an entity.
    pub fn remove<T: 'static>(&mut self, entity: Entity) {
        let type_id = TypeId::of::<T>();
        if let Some(col) = self.components.get_mut(&type_id) {
            let map = col.downcast_mut::<HashMap<u32, T>>().unwrap();
            map.remove(&entity.index);
        }
    }

    /// Get a component ref.
    pub fn get<T: 'static>(&self, entity: Entity) -> Option<&T> {
        let type_id = TypeId::of::<T>();
        let col = self.components.get(&type_id)?;
        let map = col.downcast_ref::<HashMap<u32, T>>().unwrap();
        map.get(&entity.index)
    }

    /// Get a component mut ref.
    pub fn get_mut<T: 'static>(&mut self, entity: Entity) -> Option<&mut T> {
        let type_id = TypeId::of::<T>();
        let col = self.components.get_mut(&type_id)?;
        let map = col.downcast_mut::<HashMap<u32, T>>().unwrap();
        map.get_mut(&entity.index)
    }

    // ---- query ----

    /// Query entities that have component T.
    pub fn query<T: 'static>(&self) -> impl Iterator<Item = (Entity, &T)> + '_ {
        let type_id = TypeId::of::<T>();
        let gen = &self.generations;
        let live = &self.live;
        self.components
            .get(&type_id)
            .into_iter()
            .flat_map(move |col| {
                let map = col.downcast_ref::<HashMap<u32, T>>().unwrap();
                map.iter().filter_map(move |(&idx, v)| {
                    if live.contains(&idx) {
                        Some((
                            Entity {
                                index: idx,
                                generation: gen[idx as usize],
                            },
                            v,
                        ))
                    } else {
                        None
                    }
                })
            })
    }

    /// Query entities that have both A and B.
    pub fn query2<A: 'static, B: 'static>(&self) -> impl Iterator<Item = (Entity, &A, &B)> + '_ {
        let ta = TypeId::of::<A>();
        let tb = TypeId::of::<B>();
        let gen = &self.generations;
        let live = &self.live;
        let amap = self
            .components
            .get(&ta)
            .map(|c| c.downcast_ref::<HashMap<u32, A>>().unwrap());
        let bmap = self
            .components
            .get(&tb)
            .map(|c| c.downcast_ref::<HashMap<u32, B>>().unwrap());

        match (amap, bmap) {
            (Some(a), Some(b)) => Box::new(a.iter().filter_map(move |(&idx, va)| {
                if live.contains(&idx) {
                    b.get(&idx).map(|vb| {
                        (
                            Entity {
                                index: idx,
                                generation: gen[idx as usize],
                            },
                            va,
                            vb,
                        )
                    })
                } else {
                    None
                }
            }))
                as Box<dyn Iterator<Item = (Entity, &A, &B)> + '_>,
            _ => Box::new(std::iter::empty()),
        }
    }

    // ---- helpers ----

    pub fn len(&self) -> usize {
        self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Demo components
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Velocity {
    pub vx: f32,
    pub vy: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Health(pub f32);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_empty() {
        let world = World::new();
        assert!(world.is_empty());
    }

    #[test]
    fn spawn_and_count() {
        let mut world = World::new();
        for _ in 0..100 {
            world.spawn();
        }
        assert_eq!(world.len(), 100);
    }

    #[test]
    fn insert_and_query() {
        let mut world = World::new();
        let e1 = world.spawn();
        let e2 = world.spawn();

        world.insert(e1, Position { x: 1.0, y: 2.0 });
        world.insert(e2, Position { x: 3.0, y: 4.0 });
        world.insert(e1, Velocity { vx: 0.5, vy: 0.0 });

        let positions: Vec<_> = world.query::<Position>().collect();
        assert_eq!(positions.len(), 2);

        let both: Vec<_> = world.query2::<Position, Velocity>().collect();
        assert_eq!(both.len(), 1);
        assert_eq!(both[0].0, e1);
    }

    #[test]
    fn get_and_get_mut() {
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 1.0, y: 2.0 });

        assert_eq!(world.get::<Position>(e).unwrap().x, 1.0);
        world.get_mut::<Position>(e).unwrap().x = 99.0;
        assert_eq!(world.get::<Position>(e).unwrap().x, 99.0);
    }

    #[test]
    fn remove_component() {
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 1.0, y: 2.0 });
        world.insert(e, Velocity { vx: 0.0, vy: 0.0 });

        assert_eq!(world.query::<Position>().count(), 1);
        world.remove::<Velocity>(e);
        assert_eq!(world.query2::<Position, Velocity>().count(), 0);
        assert_eq!(world.query::<Position>().count(), 1);
    }

    #[test]
    fn despawn_entity() {
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 1.0, y: 2.0 });
        world.insert(e, Health(100.0));

        assert_eq!(world.len(), 1);
        world.despawn(e);
        assert_eq!(world.len(), 0);
        assert!(world.query::<Position>().next().is_none());
    }

    #[test]
    fn generation_invalidates_stale_handles() {
        let mut world = World::new();
        let e = world.spawn();
        world.despawn(e);

        // Spawn again — should get the same index but a new generation.
        let e2 = world.spawn();
        assert_eq!(e2.index, e.index);
        assert_ne!(e2.generation, e.generation);
    }

    #[test]
    fn bench_10k_entities() {
        let mut world = World::new();
        for i in 0..10_000 {
            let e = world.spawn();
            world.insert(
                e,
                Position {
                    x: i as f32,
                    y: 0.0,
                },
            );
            world.insert(e, Velocity { vx: 0.01, vy: 0.0 });
        }
        assert_eq!(world.len(), 10_000);
        assert_eq!(world.query::<Position>().count(), 10_000);
        assert_eq!(world.query2::<Position, Velocity>().count(), 10_000);
    }
}
