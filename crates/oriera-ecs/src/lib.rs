//! L3 Object Model — ECS with SoA columns, generation-validated handles and
//! a resource store.
//!
//! # Design
//!
//! - **Entity** = (`index: u32`, `generation: u32`). Generation prevents
//!   use-after-free: when an entity is despawned, its index can be reused
//!   but the generation counter increments, so stale handles are rejected.
//! - **Component storage** is per-type: `Vec<Option<T>>` indexed directly by
//!   entity index. O(1) random access with no hashing; the `Option` slot
//!   doubles as "has this component" flag.
//! - **Resources** are singletons keyed by type (e.g. `Gravity`). Systems
//!   communicate shared state through resources on the world — never through
//!   globals (book T3 §3.8.1).
//! - **Despawn** calls `Column::clear(idx)` on every type-erased column, so
//!   memory is reclaimed immediately and queries never see stale data.
//! - This layer owns the object model only. Domain logic — physics stepping,
//!   rendering, inference — lives in the layers above (L5–L9).
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
//! world.for_each2_mut::<Position, Velocity>(|_entity, pos, vel| {
//!     pos.x += vel.vx * 0.016;
//! });
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

/// Holds columns taken out of the map during an iteration and puts
/// them back on drop — including panic unwind. Without this, a
/// panicking system closure would drop the Box and destroy every
/// component of that type in the world. Two slots cover the current
/// max join arity; the array lives on the stack (zero heap alloc).
struct ColumnsGuard<'a> {
    columns: &'a mut HashMap<TypeId, Box<dyn Column>>,
    slots: [Option<(TypeId, Box<dyn Column>)>; 2],
}

impl ColumnsGuard<'_> {
    fn take(&mut self, slot: usize, type_id: TypeId) {
        self.slots[slot] = self.columns.remove(&type_id).map(|col| (type_id, col));
    }

    fn slot_mut(&mut self, slot: usize) -> Option<&mut Box<dyn Column>> {
        self.slots[slot].as_mut().map(|(_, col)| col)
    }

    /// Disjoint mutable refs to both slots (split borrow, no aliasing).
    fn two_mut(
        &mut self,
    ) -> (
        Option<&mut Box<dyn Column>>,
        Option<&mut Box<dyn Column>>,
    ) {
        let (first, rest) = self.slots.split_first_mut().expect("slots always has 2 entries");
        (first.as_mut().map(|(_, col)| col), rest[0].as_mut().map(|(_, col)| col))
    }
}

impl Drop for ColumnsGuard<'_> {
    fn drop(&mut self) {
        for slot in self.slots.iter_mut() {
            if let Some((type_id, col)) = slot.take() {
                self.columns.insert(type_id, col);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// World
// ---------------------------------------------------------------------------

/// The ECS world: entities, components and resources.
#[derive(Default)]
pub struct World {
    next_index: u32,
    generations: Vec<u32>,
    free: Vec<u32>,
    live_count: usize,
    columns: HashMap<TypeId, Box<dyn Column>>,
    resources: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
    /// Typed single-frame event queues (see [`World::write_event`]).
    events: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
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

    /// Despawn every entity, keeping column capacity for reuse.
    pub fn clear(&mut self) {
        for idx in 0..self.generations.len() {
            for col in self.columns.values_mut() {
                col.clear(idx);
            }
        }
        self.live_count = 0;
        // Bump every generation so all outstanding handles go stale,
        // then recycle every index for future spawns.
        for g in self.generations.iter_mut() {
            *g += 1;
        }
        self.free = (0..self.next_index).collect();
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

    // ---- resources ----

    /// Insert or replace a singleton resource (e.g. `Gravity(9.8)`).
    pub fn insert_resource<T: Send + Sync + 'static>(&mut self, res: T) {
        self.resources.insert(TypeId::of::<T>(), Box::new(res));
    }

    /// Read a singleton resource.
    pub fn resource<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.resources.get(&TypeId::of::<T>())?.downcast_ref::<T>()
    }

    /// Mutate a singleton resource.
    pub fn resource_mut<T: Send + Sync + 'static>(&mut self) -> Option<&mut T> {
        self.resources
            .get_mut(&TypeId::of::<T>())?
            .downcast_mut::<T>()
    }

    /// Mutate a singleton resource, inserting `T::default()` when the
    /// application never registered one. Engine systems use this so a
    /// missing registration degrades to an empty resource instead of
    /// panicking mid-frame — the app shell keeps zero boilerplate.
    pub fn resource_or_default<T: Default + Send + Sync + 'static>(&mut self) -> &mut T {
        self.resources
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(T::default()))
            .downcast_mut::<T>()
            .expect("entry keyed by TypeId of T")
    }

    // ---- events ----

    /// Per-type cap on undrained events. A queue nobody reads is a
    /// leak vector, so the oldest events are dropped past this bound —
    /// a flood that overruns the cap is itself a bug worth a warning.
    pub const MAX_PENDING_EVENTS: usize = 1024;

    /// Push `event` onto the typed queue for `T`. Events are the
    /// engine-standard channel for cross-system signals (an NPC spoke,
    /// an inference landed, a beat fired): writers append during their
    /// system, readers call [`World::take_events`] later in the frame
    /// order (L7 infer → L6 physics → L5 render) and own the drained
    /// batch. Undrained queues hold at most [`MAX_PENDING_EVENTS`]
    /// entries; the oldest are dropped (with a warning) when a writer
    /// floods a type nobody consumes.
    pub fn write_event<T: Send + Sync + 'static>(&mut self, event: T) {
        let queue = self
            .events
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(Vec::<T>::new()) as Box<dyn Any + Send + Sync>);
        let v = queue
            .downcast_mut::<Vec<T>>()
            .expect("event queue keyed by TypeId of T");
        v.push(event);
        if v.len() > Self::MAX_PENDING_EVENTS {
            v.remove(0);
            tracing::warn!(
                ty = std::any::type_name::<T>(),
                cap = Self::MAX_PENDING_EVENTS,
                "L3 event queue flooded — dropping oldest; add a take_events reader"
            );
        }
    }

    /// Drain every pending event of type `T` in write order. The caller
    /// owns the returned batch; a second call in the same frame sees
    /// only what was written after the first drain.
    pub fn take_events<T: Send + Sync + 'static>(&mut self) -> Vec<T> {
        match self.events.get_mut(&TypeId::of::<T>()) {
            Some(queue) => std::mem::take(
                queue
                    .downcast_mut::<Vec<T>>()
                    .expect("event queue keyed by TypeId of T"),
            ),
            None => Vec::new(),
        }
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

    /// Mutable query: call a closure on each entity that has component T.
    ///
    /// Zero-alloc per call: the column is taken out of the map for the
    /// iteration (same pattern as [`World::for_each2_mut`]) instead of
    /// cloning the generation vector every frame. The closure receives
    /// `&mut T`, not `&mut World`, so the borrow checker already
    /// prevents despawns/inserts during iteration.
    ///
    /// Panic safety: a [`ColumnsGuard`] reinserts the column on unwind,
    /// so a panicking system closure costs the frame, never the column.
    pub fn for_each_mut<T: 'static>(&mut self, mut f: impl FnMut(Entity, &mut T)) {
        let type_id = TypeId::of::<T>();
        let mut guard = ColumnsGuard {
            columns: &mut self.columns,
            slots: [None, None],
        };
        guard.take(0, type_id);
        if let Some(c) = guard.slot_mut(0) {
            let vec = c.as_any_mut().downcast_mut::<Vec<Option<T>>>().unwrap();
            for (idx, slot) in vec.iter_mut().enumerate() {
                if let Some(v) = slot.as_mut() {
                    f(
                        Entity {
                            index: idx as u32,
                            generation: self.generations[idx],
                        },
                        v,
                    );
                }
            }
        }
    }

    /// Mutable two-component join: call a closure on each entity that has
    /// both A and B. No cloning — the closure gets `&mut` to both columns.
    ///
    /// Panic safety: same [`ColumnsGuard`] contract — both columns are
    /// reinserted even if the closure panics.
    pub fn for_each2_mut<A: 'static, B: 'static>(
        &mut self,
        mut f: impl FnMut(Entity, &mut A, &mut B),
    ) {
        let ta = TypeId::of::<A>();
        let tb = TypeId::of::<B>();
        // Take both columns out of the map so both can be borrowed at once.
        let mut guard = ColumnsGuard {
            columns: &mut self.columns,
            slots: [None, None],
        };
        guard.take(0, ta);
        guard.take(1, tb);
        if let (Some(a), Some(b)) = guard.two_mut() {
            let avec = a.as_any_mut().downcast_mut::<Vec<Option<A>>>().unwrap();
            let bvec = b.as_any_mut().downcast_mut::<Vec<Option<B>>>().unwrap();
            let len = avec.len().min(bvec.len());
            for idx in 0..len {
                if let (Some(av), Some(bv)) = (avec[idx].as_mut(), bvec[idx].as_mut()) {
                    f(
                        Entity {
                            index: idx as u32,
                            generation: self.generations[idx],
                        },
                        av,
                        bv,
                    );
                }
            }
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
// Engine-standard components (L3 object vocabulary)
// ---------------------------------------------------------------------------

/// Standard transform component. Owned by L3 so that render (L5) and
/// physics (L6) share one object vocabulary without depending on each other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

/// Standard motion component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Velocity {
    pub vx: f32,
    pub vy: f32,
}

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

        assert_eq!(world.len(), 1);
        world.despawn(e);
        assert_eq!(world.len(), 0);
        assert!(world.query::<Position>().next().is_none());
    }

    #[test]
    fn clear_despawns_all_and_stales_handles() {
        let mut world = World::new();
        let e1 = world.spawn();
        let e2 = world.spawn();
        world.insert(e1, Position { x: 1.0, y: 2.0 });
        world.insert(e2, Position { x: 3.0, y: 4.0 });

        world.clear();
        assert!(world.is_empty());
        assert_eq!(world.query::<Position>().count(), 0);

        // Old handles must be rejected after clear.
        world.insert(e1, Position { x: 9.0, y: 9.0 });
        assert!(world.get::<Position>(e1).is_none());
        assert!(world.get::<Position>(e2).is_none());
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
    fn for_each2_mut_joins_without_clone() {
        let mut world = World::new();
        let e1 = world.spawn();
        world.insert(e1, Position { x: 0.0, y: 0.0 });
        world.insert(e1, Velocity { vx: 1.0, vy: 0.0 });
        let e2 = world.spawn();
        world.insert(e2, Position { x: 5.0, y: 5.0 }); // no velocity — skipped

        world.for_each2_mut::<Position, Velocity>(|_e, pos, vel| {
            pos.x += vel.vx;
            pos.y += vel.vy;
        });

        assert_eq!(world.get::<Position>(e1).unwrap().x, 1.0);
        // e2 untouched by the join.
        assert_eq!(world.get::<Position>(e2).unwrap().x, 5.0);
    }

    #[test]
    fn resources_are_type_keyed_singletons() {
        #[derive(Debug, PartialEq)]
        struct Gravity(f32);

        let mut world = World::new();
        assert!(world.resource::<Gravity>().is_none());

        world.insert_resource(Gravity(9.8));
        assert_eq!(world.resource::<Gravity>().unwrap(), &Gravity(9.8));

        world.resource_mut::<Gravity>().unwrap().0 = 1.6;
        assert_eq!(world.resource::<Gravity>().unwrap(), &Gravity(1.6));
    }

    #[test]
    fn for_each_mut_joins_without_clone() {
        let mut world = World::new();
        let e1 = world.spawn();
        world.insert(e1, Position { x: 0.0, y: 0.0 });
        let e2 = world.spawn();
        world.insert(e2, Position { x: 1.0, y: 1.0 });

        let mut seen = 0;
        world.for_each_mut::<Position>(|_e, p| {
            seen += 1;
            p.x += 1.0;
        });
        assert_eq!(seen, 2);
        assert_eq!(world.get::<Position>(e1).unwrap().x, 1.0);
        assert_eq!(world.get::<Position>(e2).unwrap().x, 2.0);
    }

    #[test]
    fn resource_or_default_inserts_when_missing() {
        #[derive(Debug, Default, PartialEq)]
        struct Gravity(f32);

        let mut world = World::new();
        assert!(world.resource::<Gravity>().is_none());
        world.resource_or_default::<Gravity>().0 = 4.2;
        assert_eq!(world.resource::<Gravity>().unwrap(), &Gravity(4.2));
    }

    #[test]
    fn events_drain_in_write_order() {
        #[derive(Debug, PartialEq)]
        struct NpcSpoke {
            who: &'static str,
        }

        let mut world = World::new();
        assert!(world.take_events::<NpcSpoke>().is_empty());

        world.write_event(NpcSpoke { who: "老周" });
        world.write_event(NpcSpoke { who: "Aria" });
        let batch = world.take_events::<NpcSpoke>();
        assert_eq!(
            batch,
            vec![NpcSpoke { who: "老周" }, NpcSpoke { who: "Aria" }]
        );
        // Second drain sees nothing until written again.
        assert!(world.take_events::<NpcSpoke>().is_empty());
    }

    #[test]
    fn event_types_are_independent_queues() {
        #[derive(Debug, PartialEq)]
        struct Beat(String);
        #[derive(Debug, PartialEq)]
        struct InferDone(u64);

        let mut world = World::new();
        world.write_event(Beat("雨季".into()));
        world.write_event(InferDone(7));
        assert_eq!(world.take_events::<InferDone>(), vec![InferDone(7)]);
        assert_eq!(world.take_events::<Beat>(), vec![Beat("雨季".into())]);
    }

    #[test]
    fn event_flood_is_capped_and_keeps_the_newest() {
        #[derive(Debug, PartialEq)]
        struct Loud(u32);

        let mut world = World::new();
        for i in 0..(World::MAX_PENDING_EVENTS + 64) as u32 {
            world.write_event(Loud(i));
        }
        let drained = world.take_events::<Loud>();
        assert_eq!(drained.len(), World::MAX_PENDING_EVENTS);
        // The oldest 64 were dropped; the newest survivor is exactly at
        // the boundary, and the very newest is the last write.
        assert_eq!(drained[0].0, 64);
        assert_eq!(
            drained.last().unwrap().0,
            (World::MAX_PENDING_EVENTS + 64) as u32 - 1
        );
    }

    #[test]
    fn panicking_closure_does_not_lose_the_column() {
        let mut world = World::new();
        let e1 = world.spawn();
        world.insert(e1, Position { x: 1.0, y: 1.0 });
        let e2 = world.spawn();
        world.insert(e2, Position { x: 2.0, y: 2.0 });
        world.insert(e2, Velocity { vx: 0.0, vy: 0.0 });

        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            world.for_each_mut::<Position>(|_e, _p| panic!("system bug"));
        }));
        assert!(r.is_err());
        // The column survived the unwind — data intact, world still usable.
        assert_eq!(world.query::<Position>().count(), 2);
        assert_eq!(world.get::<Position>(e1).unwrap().x, 1.0);
        world.for_each_mut::<Position>(|_e, p| p.x += 1.0);
        assert_eq!(world.get::<Position>(e1).unwrap().x, 2.0);

        // Same contract for the two-column join.
        let r2 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            world.for_each2_mut::<Position, Velocity>(|_e, _p, _v| panic!("join bug"));
        }));
        assert!(r2.is_err());
        assert_eq!(world.query::<Position>().count(), 2);
        assert_eq!(world.query::<Velocity>().count(), 1);
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
