//! L3 Object Model — ECS with SoA columns and generation-validated handles.
//!
//! # Design
//!
//! - **Entity** = (`index: u32`, `generation: u32`). Generation prevents
//!   use-after-free: when an entity is despawned, its index can be reused
//!   but the generation counter increments, so stale handles are rejected.
//! - **Component storage** is per-type: `Vec<Option<T>>` indexed directly by
//!   entity index. O(1) random access with no hashing; the `Option` slot
//!   doubles as "has this component" flag.
//! - **Despawn** calls `Column::clear(idx)` on every type-erased column, so
//!   memory is reclaimed immediately and queries never see stale data.
//! - **v0.1 note**: This is a flat-column ECS, not multi-archetype.
//!   Archetype relocation (moving entities between tables when components
//!   are added/removed) is deferred to v0.2. The public API stays the same.
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
// Type-erased column
// ---------------------------------------------------------------------------

trait Column: Any {
    fn clear(&mut self, idx: usize);
    #[allow(dead_code)]
    fn len(&self) -> usize;
    fn resize(&mut self, new_len: usize);
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: 'static> Column for Vec<Option<T>> {
    fn clear(&mut self, idx: usize) {
        if idx < self.len() {
            self[idx] = None;
        }
    }
    fn len(&self) -> usize {
        Vec::len(self)
    }
    fn resize(&mut self, new_len: usize) {
        // Grow with None; shrink just truncates.
        let old_len = self.len();
        if new_len > old_len {
            self.reserve(new_len - old_len);
            for _ in old_len..new_len {
                self.push(None);
            }
        } else {
            self.truncate(new_len);
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
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
    live_count: usize,
    columns: HashMap<TypeId, Box<dyn Column>>,
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
            let new_len = idx as usize + 1;
            for col in self.columns.values_mut() {
                col.resize(new_len);
            }
            idx
        };
        let generation = self.generations[index as usize];
        self.live_count += 1;
        Entity { index, generation }
    }

    /// Remove an entity and all its components.
    pub fn despawn(&mut self, entity: Entity) {
        if !self.valid(entity) {
            return;
        }
        self.generations[entity.index as usize] += 1;
        self.live_count -= 1;
        self.free.push(entity.index);
        let idx = entity.index as usize;
        for col in self.columns.values_mut() {
            col.clear(idx);
        }
    }

    // ---- insert / remove ----

    /// Insert or replace a component on an entity.
    pub fn insert<T: 'static>(&mut self, entity: Entity, component: T) {
        if !self.valid(entity) {
            return;
        }
        let type_id = TypeId::of::<T>();
        let idx = entity.index as usize;
        let cap = idx + 1;
        let col = self.columns.entry(type_id).or_insert_with(|| {
            let mut v: Vec<Option<T>> = Vec::new();
            v.resize(cap);
            Box::new(v) as Box<dyn Column>
        });
        col.resize(cap);
        let vec = col.as_any_mut().downcast_mut::<Vec<Option<T>>>().unwrap();
        vec[idx] = Some(component);
    }

    /// Remove a component from an entity.
    pub fn remove<T: 'static>(&mut self, entity: Entity) {
        if !self.valid(entity) {
            return;
        }
        let type_id = TypeId::of::<T>();
        if let Some(col) = self.columns.get_mut(&type_id) {
            col.clear(entity.index as usize);
        }
    }

    /// Get a component ref.
    pub fn get<T: 'static>(&self, entity: Entity) -> Option<&T> {
        if !self.valid(entity) {
            return None;
        }
        let type_id = TypeId::of::<T>();
        let col = self.columns.get(&type_id)?;
        let vec = col.as_any().downcast_ref::<Vec<Option<T>>>().unwrap();
        vec.get(entity.index as usize)?.as_ref()
    }

    /// Get a component mut ref.
    pub fn get_mut<T: 'static>(&mut self, entity: Entity) -> Option<&mut T> {
        if !self.valid(entity) {
            return None;
        }
        let type_id = TypeId::of::<T>();
        let col = self.columns.get_mut(&type_id)?;
        let vec = col.as_any_mut().downcast_mut::<Vec<Option<T>>>().unwrap();
        vec.get_mut(entity.index as usize)?.as_mut()
    }

    // ---- query ----

    /// Query all entities that have component T.
    pub fn query<T: 'static>(&self) -> impl Iterator<Item = (Entity, &T)> + '_ {
        let type_id = TypeId::of::<T>();
        let gens = &self.generations;
        self.columns.get(&type_id).into_iter().flat_map(move |col| {
            let vec = col.as_any().downcast_ref::<Vec<Option<T>>>().unwrap();
            vec.iter().enumerate().filter_map(move |(idx, slot)| {
                slot.as_ref().map(|v| {
                    (
                        Entity {
                            index: idx as u32,
                            generation: gens[idx],
                        },
                        v,
                    )
                })
            })
        })
    }

    /// Query all entities that have both A and B.
    pub fn query2<A: 'static, B: 'static>(&self) -> impl Iterator<Item = (Entity, &A, &B)> + '_ {
        let ta = TypeId::of::<A>();
        let tb = TypeId::of::<B>();
        let gens = &self.generations;
        let acol = self
            .columns
            .get(&ta)
            .map(|c| c.as_any().downcast_ref::<Vec<Option<A>>>().unwrap());
        let bcol = self
            .columns
            .get(&tb)
            .map(|c| c.as_any().downcast_ref::<Vec<Option<B>>>().unwrap());

        match (acol, bcol) {
            (Some(a), Some(b)) => {
                let len = a.len().min(b.len());
                Box::new((0..len).filter_map(move |idx| {
                    let av = a[idx].as_ref()?;
                    let bv = b[idx].as_ref()?;
                    Some((
                        Entity {
                            index: idx as u32,
                            generation: gens[idx],
                        },
                        av,
                        bv,
                    ))
                })) as Box<dyn Iterator<Item = (Entity, &A, &B)> + '_>
            }
            _ => Box::new(std::iter::empty()),
        }
    }

    // ---- helpers ----

    pub fn len(&self) -> usize {
        self.live_count
    }

    pub fn is_empty(&self) -> bool {
        self.live_count == 0
    }

    fn valid(&self, entity: Entity) -> bool {
        (entity.index as usize) < self.generations.len()
            && self.generations[entity.index as usize] == entity.generation
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
    fn despawn_clears_components() {
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 1.0, y: 2.0 });
        world.insert(e, Health(100.0));

        assert_eq!(world.len(), 1);
        world.despawn(e);
        assert_eq!(world.len(), 0);
        assert!(world.query::<Position>().next().is_none());
        assert!(world.query::<Health>().next().is_none());
    }

    #[test]
    fn stale_handle_rejected() {
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 1.0, y: 2.0 });
        world.despawn(e);

        // e is now stale — insert/get must reject it.
        world.insert(e, Position { x: 99.0, y: 99.0 });
        assert!(world.get::<Position>(e).is_none());

        // Spawn a new entity — may reuse index but new generation.
        let e2 = world.spawn();
        assert_ne!(e, e2);
        // The stale handle must not see e2's components.
        assert!(world.get::<Position>(e).is_none());
    }

    #[test]
    fn generation_invalidates_stale_handles() {
        let mut world = World::new();
        let e = world.spawn();
        world.despawn(e);

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
