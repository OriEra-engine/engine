//! L3 Object Model — minimal archetype ECS.
//!
//! This is the reference implementation. It is not a full ECS yet —
//! just enough to prove the data layout (SoA, archetype tables) works
//! and to benchmark against the Web demo.

use std::any::{Any, TypeId};
use std::collections::HashMap;

/// An entity is just an ID. Components live in archetype tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Entity(pub u32);

/// A component type registry. Each component gets a column in every
/// archetype table that contains it.
#[derive(Default)]
pub struct World {
    next_id: u32,
    /// Which components each entity has (for fast lookup).
    entities: HashMap<Entity, Vec<TypeId>>,
    /// Archetype tables: hash of component type ids -> columns.
    tables: HashMap<u64, ArchetypeTable>,
}

#[derive(Default)]
struct ArchetypeTable {
    columns: HashMap<TypeId, Column>,
    len: usize,
}

struct Column {
    data: Box<dyn Any>,
}

impl World {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn a fresh entity with no components.
    pub fn spawn(&mut self) -> Entity {
        let e = Entity(self.next_id);
        self.next_id += 1;
        self.entities.insert(e, Vec::new());
        e
    }

    /// Insert a component. SoA layout: one typed Vec per component type.
    pub fn insert<T: 'static>(&mut self, entity: Entity, component: T) {
        let type_id = TypeId::of::<T>();
        self.entities.entry(entity).or_default().push(type_id);

        let table_key = 1u64;
        let table = self.tables.entry(table_key).or_default();
        let col = table.columns.entry(type_id).or_insert_with(|| Column {
            data: Box::new(Vec::<Option<T>>::new()),
        });
        let vec = col.data.downcast_mut::<Vec<Option<T>>>().unwrap();
        while vec.len() <= entity.0 as usize {
            vec.push(None);
        }
        vec[entity.0 as usize] = Some(component);
        table.len = table.len.max(entity.0 as usize + 1);
    }

    /// Query all entities that have component T. Returns (Entity, &T).
    pub fn query<T: 'static>(&self) -> impl Iterator<Item = (Entity, &T)> + '_ {
        let type_id = TypeId::of::<T>();
        self.tables.values().flat_map(move |table| {
            table
                .columns
                .get(&type_id)
                .into_iter()
                .flat_map(move |col| {
                    let vec = col.data.downcast_ref::<Vec<Option<T>>>().unwrap();
                    vec.iter()
                        .enumerate()
                        .filter_map(|(i, v)| v.as_ref().map(|v| (Entity(i as u32), v)))
                })
        })
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }
}

// ---- Demo components ----
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_and_query() {
        let mut world = World::new();
        let e1 = world.spawn();
        let e2 = world.spawn();

        world.insert(e1, Position { x: 1.0, y: 2.0 });
        world.insert(e2, Position { x: 3.0, y: 4.0 });
        world.insert(e1, Velocity { vx: 0.5, vy: 0.0 });

        let positions: Vec<_> = world.query::<Position>().collect();
        assert_eq!(positions.len(), 2);

        let velocities: Vec<_> = world.query::<Velocity>().collect();
        assert_eq!(velocities.len(), 1);
        assert_eq!(velocities[0].0, e1);
    }

    #[test]
    fn bench_10k_entities() {
        let mut world = World::new();
        for i in 0..10_000 {
            let e = world.spawn();
            world.insert(e, Position { x: i as f32, y: 0.0 });
            world.insert(e, Velocity { vx: 0.01, vy: 0.0 });
        }
        assert_eq!(world.len(), 10_000);

        let count = world.query::<Position>().count();
        assert_eq!(count, 10_000);
    }
}
