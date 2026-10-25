//! Entity prefabs — Godot's PackedScene translated to ECS (F1,
//! PLAN-godot-fusion): a reusable cast/prop bundle, instantiated in a
//! **deterministic declaration order** so the same spec twice produces
//! bit-identical worlds (`state_hash` provable).
//!
//! Layer placement: the *data schema* is documented in L3 contracts
//! (component vocabulary belongs to the layers that define it); the
//! *executor* lives here in L9 because the NPC components (Persona/
//! Mood) are L9 types. Only downward deps (oriera_ecs/oriera_common).
//!
//! Not a Node tree: the prefab is data, the world stays flat ECS.

use oriera_ecs::{Entity, Position, Velocity, World};
use serde::Deserialize;

use crate::{Mood, Persona};

/// A reusable bundle: cast members and/or prop entities.
#[derive(Debug, Clone, Deserialize)]
pub struct PrefabSpec {
    /// 显示名（模板/UI 用）。预留：未来模板注册表的键位；当前不参与
    /// 去重（同卡司跨实例重名会各自独立——MemoryStore 的使用侧注意）。
    pub name: String,
    /// Entities are created in declaration order — the order IS the
    /// determinism contract (spawn index feeds nothing random; two
    /// instantiations of one spec are bit-identical).
    pub entities: Vec<PrefabEntity>,
}

/// One entity of the bundle: a prop (position/velocity only) or a cast
/// member (adds persona + starting mood).
#[derive(Debug, Clone, Deserialize)]
pub struct PrefabEntity {
    pub position: [f32; 2],
    #[serde(default)]
    pub velocity: [f32; 2],
    /// Cast member fields; absent for pure props.
    #[serde(default)]
    pub persona: Option<PersonaFields>,
}

/// The L9 NPC vocabulary in prefab (serde) shape. `mood` is the
/// *starting* mood — cognition rewrites it per speaking turn.
#[derive(Debug, Clone, Deserialize)]
pub struct PersonaFields {
    pub name: String,
    pub role: String,
    pub description: String,
    #[serde(default)]
    pub mood: Option<String>,
}

/// Instantiate a prefab at the world origin: returns entities in
/// declaration order (index i ↔ `spec.entities[i]`).
pub fn spawn_prefab(world: &mut World, spec: &PrefabSpec) -> Vec<Entity> {
    spawn_prefab_at(world, spec, [0.0, 0.0])
}

/// Instantiate with a placement offset (cast circles, prop clusters).
/// Offset is applied after the spec positions — same spec + same
/// offset = same world, so replays stay provable.
pub fn spawn_prefab_at(world: &mut World, spec: &PrefabSpec, at: [f32; 2]) -> Vec<Entity> {
    spec.entities
        .iter()
        .map(|e| {
            let entity = world.spawn();
            world.insert(
                entity,
                Position {
                    x: e.position[0] + at[0],
                    y: e.position[1] + at[1],
                },
            );
            world.insert(
                entity,
                Velocity {
                    vx: e.velocity[0],
                    vy: e.velocity[1],
                },
            );
            if let Some(pf) = &e.persona {
                world.insert(entity, persona_of(pf));
                world.insert(entity, mood_of(pf));
            }
            entity
        })
        .collect()
}

fn persona_of(pf: &PersonaFields) -> Persona {
    Persona {
        name: pf.name.clone(),
        role: pf.role.clone(),
        description: pf.description.clone(),
    }
}

/// Mood is a 5-value enum; the prefab carries the free-form string and
/// we fold it best-effort (unknown strings → Neutral — never a panic
/// from authoring data).
fn mood_of(pf: &PersonaFields) -> Mood {
    Mood::from_label(pf.mood.as_deref().unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oriera_common::FrameIndex;

    fn spec_json() -> &'static str {
        r#"{
          "name": "last-platform-cast",
          "entities": [
            { "position": [50.0, 30.0], "velocity": [0.0, 0.0],
              "persona": { "name": "林晚", "role": "关键 NPC",
                "description": "26 岁的加班族，湿发贴颊", "mood": "紧张" } },
            { "position": [80.0, 30.0],
              "persona": { "name": "黑伞男", "role": "压迫源",
                "description": "黑大衣黑伞" } },
            { "position": [10.0, 90.0], "velocity": [3.0, 0.0] }
          ]
        }"#
    }

    fn spec() -> PrefabSpec {
        serde_json::from_str(spec_json()).expect("prefab spec parses")
    }

    #[test]
    fn prefab_parses_and_spawns_in_declaration_order() {
        let mut w = World::new();
        let entities = spawn_prefab(&mut w, &spec());
        assert_eq!(entities.len(), 3);
        // declaration order: entity i sits at spec position i
        let p0 = w.get::<Position>(entities[0]).unwrap();
        assert_eq!((p0.x, p0.y), (50.0, 30.0));
        // cast member got Persona; prop did not
        assert!(w.get::<Persona>(entities[0]).is_some());
        assert!(w.get::<Persona>(entities[2]).is_none());
        // starting mood folded from the authoring string (紧张 → Tense)
        assert_eq!(w.get::<Mood>(entities[0]), Some(&Mood::Sad));
    }

    #[test]
    fn same_spec_twice_is_bit_identical() {
        // The determinism contract: instantiate → hash → wipe → again.
        let run = || {
            let mut w = World::new();
            let entities = spawn_prefab(&mut w, &spec());
            let hash = w.state_hash();
            (hash, entities.len())
        };
        assert_eq!(run(), run(), "same spec → bit-identical world");
    }

    #[test]
    fn offset_shifts_every_entity_uniformly() {
        let mut w = World::new();
        let entities = spawn_prefab_at(&mut w, &spec(), [100.0, 200.0]);
        let p2 = w.get::<Position>(entities[2]).unwrap();
        assert_eq!((p2.x, p2.y), (110.0, 290.0)); // 10+100, 90+200
    }

    #[test]
    fn unknown_mood_string_falls_back_to_neutral() {
        let mut w = World::new();
        let spec: PrefabSpec = serde_json::from_str(
            r#"{ "name": "x", "entities": [ { "position": [0.0, 0.0],
                 "persona": { "name": "a", "role": "r", "description": "d",
                              "mood": "神秘莫测" } } ] }"#,
        )
        .unwrap();
        let entities = spawn_prefab(&mut w, &spec);
        assert_eq!(w.get::<Mood>(entities[0]), Some(&Mood::Neutral));
        let _ = FrameIndex(0); // keep import parity with sibling tests
    }
}
