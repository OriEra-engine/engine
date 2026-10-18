//! L9 embodied data collection (#80) — the world-as-benchmark export.
//!
//! An embodied policy needs three things the engine already provides:
//! a deterministic tick ([`oriera_core::Scheduler::with_fixed_dt`]),
//! per-entity actions (`Command::Nudge`, #69) and a world fingerprint
//! (`World::state_hash`, #67). This module adds the fourth: an
//! **episode recorder** that turns a playthrough into training data.
//!
//! The JSONL export aligns its column names with LeRobotDataset v3
//! (`episode_index`, `step_index`, `observation.state`,
//! `action.continuous`, `next_observation.state`, `reward`, `done`) so
//! the shape survives a future parquet conversion; `meta/info.json`
//! records the engine version, backend, dt and — the reproducibility
//! claim — the per-episode `state_hash`. Rows are hand-serialized:
//! numeric fields only, zero dependencies, wasm budget untouched.

use oriera_ecs::{Position, Velocity, World};

/// One observation: the robot's own state plus the nearest target in
/// relative terms (policy eyes, 8 floats total).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observation {
    pub robot_pos: [f32; 2],
    pub robot_vel: [f32; 2],
    /// Nearest target: relative offset + distance + its speed (or zeros
    /// when no target exists — a valid, honest observation).
    pub nearest: [f32; 4],
}

impl Observation {
    /// Flat state vector, LeRobot `observation.state` semantics.
    pub fn state(&self) -> [f32; 8] {
        [
            self.robot_pos[0],
            self.robot_pos[1],
            self.robot_vel[0],
            self.robot_vel[1],
            self.nearest[0],
            self.nearest[1],
            self.nearest[2],
            self.nearest[3],
        ]
    }
}

/// One RL transition: (s, a, r, s', done).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transition {
    pub episode: u32,
    pub step: u64,
    pub obs: Observation,
    pub action: [f32; 2],
    pub reward: f32,
    pub next: Observation,
    pub done: bool,
}

/// Records transitions per episode and exports them.
#[derive(Debug, Default)]
pub struct EpisodeRecorder {
    pub episodes: u32,
    pub successes: u32,
    transitions: Vec<Transition>,
    closed_transitions: usize,
    /// Per-episode fingerprints (#67) — the reproducibility receipt.
    pub episode_hashes: Vec<u64>,
}

impl EpisodeRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe the world from the robot's eyes: own pos/vel + nearest
    /// other entity with Position (relative offset, distance, speed).
    pub fn observe(world: &World, robot: oriera_ecs::Entity) -> Observation {
        let (p, v) = match (world.get::<Position>(robot), world.get::<Velocity>(robot)) {
            (Some(p), Some(v)) => (*p, *v),
            _ => {
                return Observation {
                    robot_pos: [0.0; 2],
                    robot_vel: [0.0; 2],
                    nearest: [0.0; 4],
                }
            }
        };
        let mut best: Option<(f32, [f32; 4])> = None;
        for (e, tp) in world.query::<Position>() {
            if e == robot {
                continue;
            }
            let dx = tp.x - p.x;
            let dy = tp.y - p.y;
            let d = (dx * dx + dy * dy).sqrt();
            if best.is_none_or(|(bd, _)| d < bd) {
                let spd = world
                    .get::<Velocity>(e)
                    .map(|tv| (tv.vx * tv.vx + tv.vy * tv.vy).sqrt())
                    .unwrap_or(0.0);
                best = Some((d, [dx, dy, d, spd]));
            }
        }
        Observation {
            robot_pos: [p.x, p.y],
            robot_vel: [v.vx, v.vy],
            nearest: best.map(|(_, n)| n).unwrap_or([0.0; 4]),
        }
    }

    /// Record one transition. The caller owns the loop; the recorder
    /// owns the bookkeeping.
    pub fn record(&mut self, t: Transition) {
        self.transitions.push(t);
    }

    /// Close the episode: bump counters and pin the fingerprint.
    pub fn finish_episode(&mut self, success: bool, world_hash: u64) {
        self.episodes += 1;
        self.successes += success as u32;
        self.closed_transitions = self.transitions.len();
        self.episode_hashes.push(world_hash);
    }

    pub fn len_transitions(&self) -> usize {
        self.transitions.len()
    }

    /// JSONL export: one transition per line, column names aligned with
    /// LeRobotDataset v3. Numeric fields only — no escaping needed.
    pub fn to_jsonl(&self) -> String {
        let mut out = String::new();
        for t in &self.transitions {
            let s = t.obs.state();
            let n = t.next.state();
            out.push_str(&format!(
                concat!(
                    "{{\"episode_index\":{},\"step_index\":{},",
                    "\"observation.state\":{:?},\"action.continuous\":{:?},",
                    "\"next_observation.state\":{:?},\"reward\":{:.6},\"done\":{}}}\n"
                ),
                t.episode, t.step, s, t.action, n, t.reward, t.done
            ));
        }
        out
    }

    /// Dataset metadata (`meta/info.json` shape): what ran, how, and
    /// the fingerprint receipt per episode.
    pub fn info_json(&self, engine_version: &str, backend: &str, dt: f32) -> String {
        format!(
            concat!(
                "{{\"codebase_version\":\"oriera-embodied-v1\",",
                "\"robot\":\"oriera-diff-dot-v1\",",
                "\"engine\":\"{}\",\"backend\":\"{}\",\"dt\":{:.8},",
                "\"total_episodes\":{},\"total_steps\":{},\"successes\":{},",
                "\"episode_state_hashes\":{:?}}}\n"
            ),
            engine_version,
            backend,
            dt,
            self.episodes,
            self.transitions.len(),
            self.successes,
            self.episode_hashes
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oriera_ecs::Entity;

    fn world_with(robot: (f32, f32), target: (f32, f32)) -> (World, Entity, Entity) {
        let mut w = World::new();
        let r = w.spawn();
        w.insert(
            r,
            Position {
                x: robot.0,
                y: robot.1,
            },
        );
        w.insert(r, Velocity { vx: 0.5, vy: -0.25 });
        let t = w.spawn();
        w.insert(
            t,
            Position {
                x: target.0,
                y: target.1,
            },
        );
        w.insert(t, Velocity { vx: -1.0, vy: 0.0 });
        (w, r, t)
    }

    #[test]
    fn observation_sees_the_nearest_target_relatively() {
        let (mut w, r, _t) = world_with((10.0, 10.0), (40.0, 22.0));
        let far = w.spawn();
        w.insert(far, Position { x: 400.0, y: 400.0 });
        w.insert(far, Velocity { vx: 0.0, vy: 0.0 });
        let o = EpisodeRecorder::observe(&w, r);
        assert_eq!(o.robot_pos, [10.0, 10.0]);
        assert_eq!(o.robot_vel, [0.5, -0.25]);
        // nearest = (40,22): offset (30,12), dist = sqrt(900+144) ≈ 32.31
        assert!((o.nearest[0] - 30.0).abs() < 1e-4);
        assert!((o.nearest[1] - 12.0).abs() < 1e-4);
        assert!((o.nearest[2] - 32.31).abs() < 0.01);
        assert!((o.nearest[3] - 1.0).abs() < 1e-4, "target speed read");
    }

    #[test]
    fn no_targets_is_an_honest_zero_observation() {
        let mut w = World::new();
        let r = w.spawn();
        w.insert(r, Position { x: 5.0, y: 5.0 });
        w.insert(r, Velocity { vx: 1.0, vy: 1.0 });
        let o = EpisodeRecorder::observe(&w, r);
        assert_eq!(o.nearest, [0.0; 4]);
    }

    #[test]
    fn jsonl_rows_parse_and_align_with_lerobot_columns() {
        let (w, r, _t) = world_with((0.0, 0.0), (3.0, 4.0));
        let s = EpisodeRecorder::observe(&w, r);
        let mut rec = EpisodeRecorder::new();
        rec.record(Transition {
            episode: 0,
            step: 7,
            obs: s,
            action: [0.25, -0.25],
            reward: -1.5,
            next: s,
            done: true,
        });
        rec.finish_episode(true, 0xbeef);
        let line = rec.to_jsonl().trim().to_string();
        for col in [
            "\"episode_index\":0",
            "\"step_index\":7",
            "\"observation.state\":[",
            "\"action.continuous\":[0.25, -0.25]",
            "\"next_observation.state\":[",
            "\"reward\":-1.500000",
            "\"done\":true",
        ] {
            assert!(line.contains(col), "missing {col} in {line}");
        }
        // balanced braces — hand-serialization stays well-formed
        assert_eq!(line.matches('{').count(), line.matches('}').count());
        assert_eq!(line.matches('[').count(), line.matches(']').count());
        let info = rec.info_json("0.3.0", "rigid-rapier2d", 1.0 / 60.0);
        assert!(info.contains("\"total_episodes\":1"));
        assert!(info.contains("\"successes\":1"));
        // 0xbeef serializes decimal via {:?} on the hash vec.
        assert!(info.contains("48879"), "hash receipt missing: {info}");
    }
}
