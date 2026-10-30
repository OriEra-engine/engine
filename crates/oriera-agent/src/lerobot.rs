//! LeRobotDataset v2.1 export (PLAN-embodied-sim) — the training-side
//! contract.
//!
//! v2.1 is the format read natively by `lerobot-train`
//! (ACT/Diffusion/SmolVLA/π0), by openpi's norm-stats pipeline and by
//! GR00T N1's "LeRobot v2 flavor" data prep — exporting it is what
//! makes OriEra episodes directly trainable. The writer produces the
//! full v2.1 file tree with **one honest substitution**: rows land as
//! JSONL (numeric-only, zero dependencies, the #80 house style) where
//! v2.1 stores parquet. `scripts/jsonl_to_lerobot_v21.py` converts
//! them 1:1 (column names and dtypes are already the contract), after
//! which `LeRobotDataset(repo_id)` reads the directory as-is.
//!
//! ```text
//! <root>/
//! ├── meta/info.json, stats.json, episodes.jsonl, tasks.jsonl
//! ├── meta/oriera_receipt.json        # engine/dt/per-episode fingerprints
//! ├── README.md                       # dataset card
//! └── data/chunk-000/episode_000000.jsonl → .parquet (via script)
//! ```

use std::io::Write as _;
use std::path::Path;

/// One recorded step: the observation *before* the action, and the
/// action taken. Scalars (timestamp/frame indices) are derived.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub state: Vec<f32>,
    pub action: Vec<f32>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Episode {
    pub task_index: u32,
    pub rows: Vec<Row>,
    /// Top-down camera PNG frames of this episode (feature `cam`),
    /// captured at the camera cadence by the env.
    pub frames: Vec<Vec<u8>>,
}

/// Camera track declaration (feature `cam`): one fixed top-down view.
#[derive(Debug, Clone)]
pub struct CameraTrack {
    /// LeRobot feature key suffix: `observation.images.<name>`.
    pub name: String,
    pub w: u32,
    pub h: u32,
    /// Frame rate of the track (Hz) — half the control rate by default.
    pub fps: f32,
}

/// A dataset under construction. Features: `observation.state`
/// (float32 vector) and `action` (float32 vector), plus the five
/// v2.1 bookkeeping columns.
#[derive(Debug, Default)]
pub struct LeRobotV21 {
    pub fps: f32,
    pub robot_type: String,
    pub obs_names: Vec<String>,
    pub action_names: Vec<String>,
    pub tasks: Vec<String>,
    pub episodes: Vec<Episode>,
    current: Option<Episode>,
    /// Camera track (feature `cam`); None ⇒ state-only dataset.
    pub camera: Option<CameraTrack>,
    /// Global row counter (`index` column).
    next_index: u64,
}

impl LeRobotV21 {
    pub fn new(
        fps: f32,
        robot_type: &str,
        obs_names: Vec<String>,
        action_names: Vec<String>,
    ) -> Self {
        Self {
            fps,
            robot_type: robot_type.to_string(),
            obs_names,
            action_names,
            tasks: Vec::new(),
            episodes: Vec::new(),
            current: None,
            camera: None,
            next_index: 0,
        }
    }

    /// Attach a camera track to the dataset (must be set before the
    /// first `start_episode`).
    pub fn set_camera(&mut self, track: CameraTrack) {
        self.camera = Some(track);
    }

    /// Record one camera frame (PNG bytes) for the open episode.
    pub fn push_frame(&mut self, png: Vec<u8>) -> Result<(), String> {
        let ep = self
            .current
            .as_mut()
            .ok_or("push_frame 前须 start_episode")?;
        ep.frames.push(png);
        Ok(())
    }

    /// Start an episode; repeated task strings deduplicate into
    /// tasks.jsonl (v2.1 semantics).
    pub fn start_episode(&mut self, task: &str) {
        let task_index = match self.tasks.iter().position(|t| t == task) {
            Some(i) => i as u32,
            None => {
                self.tasks.push(task.to_string());
                (self.tasks.len() - 1) as u32
            }
        };
        self.current = Some(Episode {
            task_index,
            rows: Vec::new(),
            frames: Vec::new(),
        });
    }

    /// Record one step of the open episode. Dimension mismatches
    /// against the declared features are contract errors, not panics.
    pub fn push_step(&mut self, state: &[f32], action: &[f32]) -> Result<(), String> {
        if state.len() != self.obs_names.len() || action.len() != self.action_names.len() {
            return Err(format!(
                "维度契约不符：state {}/{}，action {}/{}",
                state.len(),
                self.obs_names.len(),
                action.len(),
                self.action_names.len()
            ));
        }
        let ep = self
            .current
            .as_mut()
            .ok_or("push_step 前须 start_episode")?;
        ep.rows.push(Row {
            state: state.to_vec(),
            action: action.to_vec(),
        });
        self.next_index += 1;
        Ok(())
    }

    /// Discard the open episode (degenerate runs — e.g. a spawn that
    /// lands in the goal zone and terminates on step 1 — must not
    /// become a 1-row/1-frame dataset entry).
    pub fn drop_episode(&mut self) {
        if let Some(ep) = self.current.take() {
            self.next_index -= ep.rows.len() as u64;
        }
    }

    /// Close the open episode.
    pub fn finish_episode(&mut self) {
        if let Some(ep) = self.current.take() {
            self.episodes.push(ep);
        }
    }

    pub fn total_frames(&self) -> usize {
        self.episodes.iter().map(|e| e.rows.len()).sum()
    }

    fn row_jsonl(&self, ep_index: u32, ep: &Episode, row_i: usize, global_index: u64) -> String {
        let row = &ep.rows[row_i];
        let timestamp = row_i as f32 / self.fps;
        format!(
            concat!(
                "{{\"observation.state\":{:?},\"action\":{:?},",
                "\"timestamp\":{:.6},\"frame_index\":{},\"episode_index\":{},",
                "\"index\":{},\"task_index\":{}}}\n"
            ),
            row.state, row.action, timestamp, row_i, ep_index, global_index, ep.task_index
        )
    }

    /// `meta/info.json` — the v2.1 schema header. `data_path` already
    /// points at the parquet layout the converter produces.
    fn info_json(&self) -> String {
        let n_eps = self.episodes.len();
        let state_shape = self.obs_names.len().to_string();
        let action_shape = self.action_names.len().to_string();
        let obs_names = json_string_array(&self.obs_names);
        let action_names = json_string_array(&self.action_names);
        // Camera feature (feature `cam`): a v2.1 video entry whose
        // frames the converter assembles into mp4.
        let cam_feat = self
            .camera
            .as_ref()
            .map(|c| {
                format!(
                    ",\"observation.images.{name}\":{{\"dtype\":\"video\",\"shape\":[{h},{w},3],\"names\":null,\"info\":{{\"video.fps\":{fps},\"video.codec\":\"libx264\",\"video.pix_fmt\":\"yuv420p\",\"has_audio\":false}}}}",
                    name = c.name,
                    h = c.h,
                    w = c.w,
                    fps = c.fps
                )
            })
            .unwrap_or_default();
        let total_videos = self.camera.as_ref().map_or(0, |_| self.episodes.len());
        let total_videos = total_videos.to_string();
        format!(
            concat!(
                "{{\"codebase_version\":\"v2.1\",\"robot_type\":\"{robot}\",\"license\":\"apache-2.0\",",
                "\"total_episodes\":{eps},\"total_frames\":{frames},",
                "\"total_tasks\":{tasks},\"total_videos\":{videos},\"total_chunks\":1,",
                "\"chunks_size\":1000,\"fps\":{fps},\"splits\":{{\"train\":\"0:{eps}\"}},",
                "\"data_path\":\"data/chunk-{{episode_chunk:03d}}/episode_{{episode_index:06d}}.parquet\",",
                "\"video_path\":\"videos/chunk-{{episode_chunk:03d}}/{{video_key}}/episode_{{episode_index:06d}}.mp4\",",
                "\"features\":{{",
                "\"observation.state\":{{\"dtype\":\"float32\",\"shape\":[{sd}],\"names\":{{\"joints\":{on}}}}},",
                "\"action\":{{\"dtype\":\"float32\",\"shape\":[{ad}],\"names\":{{\"joints\":{an}}}}},",
                "\"timestamp\":{{\"dtype\":\"float32\",\"shape\":[1],\"names\":null}},",
                "\"frame_index\":{{\"dtype\":\"int64\",\"shape\":[1],\"names\":null}},",
                "\"episode_index\":{{\"dtype\":\"int64\",\"shape\":[1],\"names\":null}},",
                "\"index\":{{\"dtype\":\"int64\",\"shape\":[1],\"names\":null}},",
                "\"task_index\":{{\"dtype\":\"int64\",\"shape\":[1],\"names\":null}}",
                "{cam_feat}",
                "}}}}\n"
        ),
            robot = self.robot_type,
            eps = n_eps,
            frames = self.total_frames(),
            tasks = self.tasks.len(),
            videos = total_videos,
            fps = self.fps,
            sd = state_shape,
            ad = action_shape,
            on = obs_names,
            an = action_names,
            cam_feat = cam_feat,
        )
    }

    /// `meta/stats.json` — per-feature min/max/mean/std/count over all
    /// rows (the normalisation stats training pipelines expect).
    fn stats_json(&self) -> String {
        let mut states: Vec<Vec<f32>> = Vec::new();
        let mut actions: Vec<Vec<f32>> = Vec::new();
        let mut timestamps: Vec<f32> = Vec::new();
        for ep in &self.episodes {
            for (i, row) in ep.rows.iter().enumerate() {
                states.push(row.state.clone());
                actions.push(row.action.clone());
                timestamps.push(i as f32 / self.fps);
            }
        }
        format!(
            "{{\"observation.state\":{},\"action\":{},\"timestamp\":{}}}\n",
            vec_stats(&states),
            vec_stats(&actions),
            scalar_stats(&timestamps),
        )
    }

    /// Write the whole tree under `root`. Returns the number of
    /// episodes written.
    pub fn write_dir(&self, root: &Path) -> std::io::Result<u32> {
        let meta = root.join("meta");
        std::fs::create_dir_all(root.join("data/chunk-000"))?;
        std::fs::create_dir_all(&meta)?;
        let mut info = std::fs::File::create(meta.join("info.json"))?;
        info.write_all(self.info_json().as_bytes())?;
        let mut stats = std::fs::File::create(meta.join("stats.json"))?;
        stats.write_all(self.stats_json().as_bytes())?;

        let mut tasks = std::fs::File::create(meta.join("tasks.jsonl"))?;
        for (i, t) in self.tasks.iter().enumerate() {
            writeln!(
                tasks,
                "{{\"task_index\":{},\"task\":\"{}\"}}",
                i,
                json_escape(t)
            )?;
        }
        let mut episodes = std::fs::File::create(meta.join("episodes.jsonl"))?;
        for (i, ep) in self.episodes.iter().enumerate() {
            writeln!(
                episodes,
                "{{\"episode_index\":{},\"tasks\":[\"{}\"],\"length\":{}}}",
                i,
                json_escape(&self.tasks[ep.task_index as usize]),
                ep.rows.len()
            )?;
        }
        // Rows: one JSONL per episode, the converter's input.
        let mut global = 0u64;
        for (i, ep) in self.episodes.iter().enumerate() {
            let path = root.join(format!("data/chunk-000/episode_{i:06}.jsonl"));
            let mut f = std::fs::File::create(path)?;
            for (ri, _row) in ep.rows.iter().enumerate() {
                let line = self.row_jsonl(i as u32, ep, ri, global);
                f.write_all(line.as_bytes())?;
                global += 1;
            }
        }
        // Camera track: PNG frame dirs — the converter assembles mp4
        // (v2.1 video features live outside the parquet tables).
        if let Some(cam) = &self.camera {
            for (i, ep) in self.episodes.iter().enumerate() {
                if ep.frames.is_empty() {
                    continue;
                }
                let dir = root.join(format!(
                    "videos/chunk-000/observation.images.{}/episode_{i:06}",
                    cam.name
                ));
                std::fs::create_dir_all(&dir)?;
                for (k, png) in ep.frames.iter().enumerate() {
                    std::fs::write(dir.join(format!("frame_{k:06}.png")), png)?;
                }
            }
        }
        self.write_modality(root)?;
        Ok(self.episodes.len() as u32)
    }

    /// `meta/oriera_receipt.json` — the reproducibility claim: what
    /// ran, with which backend/step, and every episode's fingerprint.
    pub fn write_receipt(
        root: &Path,
        engine: &str,
        backend: &str,
        dt: f32,
        episode_hashes: &[u64],
        successes: u32,
    ) -> std::io::Result<()> {
        let meta = root.join("meta");
        std::fs::create_dir_all(&meta)?;
        let mut f = std::fs::File::create(meta.join("oriera_receipt.json"))?;
        let hashes: Vec<String> = episode_hashes.iter().map(|h| h.to_string()).collect();
        write!(
            f,
            concat!(
                "{{\"codebase_version\":\"oriera-embodied-v2\",",
                "\"engine\":\"{}\",\"backend\":\"{}\",\"dt\":{:.8},",
                "\"successes\":{},\"episode_state_hashes\":[{}]}}\n"
            ),
            engine,
            backend,
            dt,
            successes,
            hashes.join(",")
        )
    }

    /// `meta/modality.json` — the GR00T N1 "LeRobot v2 flavor"
    /// dimension slicing: which state/action dims belong to which
    /// semantic group. The OriEra arm layout is fixed (see
    /// `OBS_NAMES`/`ACTION_NAMES`); other layouts fall back to one
    /// honest full-range group.
    pub fn write_modality(&self, root: &Path) -> std::io::Result<()> {
        let meta = root.join("meta");
        std::fs::create_dir_all(&meta)?;
        let (obs_n, act_n) = (self.obs_names.len(), self.action_names.len());
        let body = if obs_n == 21 && act_n == 5 {
            concat!(
                "{\"state\":{",
                "\"arm\":{\"start\":0,\"end\":4},",
                "\"arm_vel\":{\"start\":4,\"end\":8},",
                "\"gripper\":{\"start\":8,\"end\":9},",
                "\"ee_pose\":{\"start\":9,\"end\":12},",
                "\"object_rel\":{\"start\":12,\"end\":15},",
                "\"goal_rel\":{\"start\":15,\"end\":18},",
                "\"flags\":{\"start\":18,\"end\":21}},",
                "\"action\":{",
                "\"arm\":{\"start\":0,\"end\":4},",
                "\"gripper\":{\"start\":4,\"end\":5}}}",
                "
"
            )
            .to_string()
        } else {
            format!(
                "{{\"state\":{{\"full\":{{\"start\":0,\"end\":{obs_n}}}}},\"action\":{{\"full\":{{\"start\":0,\"end\":{act_n}}}}}}}\n"
            )
        };
        std::fs::write(meta.join("modality.json"), body)
    }

    /// Dataset card — what this is and the exact training recipe.
    pub fn write_card(root: &Path, task: &str) -> std::io::Result<()> {
        let card = format!(
            concat!(
                "---\n",
                "task_categories: [robotics]\n",
                "license: apache-2.0\n",
                "tags: [oriera, embodied-ai, lerobot, simulation]\n",
                "---\n\n",
                "# OriEra 具身仿真数据集（LeRobotDataset v2.1）\n\n",
                "由 OriEra 世界引擎的 rigid3d 具身仿真后端采集（确定性 tick + 每集世界指纹回执，见 meta/oriera_receipt.json）。\n\n",
                "**任务**: {task}\n\n",
                "## 转换为 parquet（LeRobotDataset 直读）\n\n",
                "```bash\npython scripts/jsonl_to_lerobot_v21.py <本目录>```\n\n",
                "## 训练（HuggingFace lerobot）\n\n",
                "```bash\nlerobot-train --dataset.repo_id=<user>/<dataset> --policy.type=act\n",
                "# 或 --policy.type=smolvla（消费级 GPU 友好）```\n"
            ),
            task = task
        );
        std::fs::write(root.join("README.md"), card)
    }
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn json_string_array(items: &[String]) -> String {
    let inner: Vec<String> = items
        .iter()
        .map(|s| format!("\"{}\"", json_escape(s)))
        .collect();
    format!("[{}]", inner.join(","))
}

/// Welford-free batch stats over ragged float vectors (columns padded
/// by first-row width — features are fixed-width by construction).
fn vec_stats(rows: &[Vec<f32>]) -> String {
    let Some(first) = rows.first() else {
        return "{\"min\":[],\"max\":[],\"mean\":[],\"std\":[],\"count\":0}".to_string();
    };
    let dim = first.len();
    let n = rows.len() as f32;
    let mut min = vec![f32::INFINITY; dim];
    let mut max = vec![f32::NEG_INFINITY; dim];
    let mut sum = vec![0.0f64; dim];
    let mut sum_sq = vec![0.0f64; dim];
    for row in rows {
        for (i, v) in row.iter().take(dim).enumerate() {
            min[i] = min[i].min(*v);
            max[i] = max[i].max(*v);
            sum[i] += *v as f64;
            sum_sq[i] += (*v as f64) * (*v as f64);
        }
    }
    let mean: Vec<String> = sum
        .iter()
        .map(|s| format!("{:.7}", (s / n as f64) as f32))
        .collect();
    let std: Vec<String> = sum_sq
        .iter()
        .zip(sum.iter())
        .map(|(sq, s)| {
            let var = (sq / n as f64) - (s / n as f64) * (s / n as f64);
            format!("{:.7}", (var.max(0.0).sqrt()) as f32)
        })
        .collect();
    format!(
        "{{\"min\":{:?},\"max\":{:?},\"mean\":[{}],\"std\":[{}],\"count\":{}}}",
        min,
        max,
        mean.join(","),
        std.join(","),
        rows.len()
    )
}

fn scalar_stats(vals: &[f32]) -> String {
    let vecs: Vec<Vec<f32>> = vals.iter().map(|v| vec![*v]).collect();
    vec_stats(&vecs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> LeRobotV21 {
        let mut ds = LeRobotV21::new(
            60.0,
            "oriera-arm-6",
            vec!["j0".to_string(), "j1".to_string()],
            vec!["a0".to_string()],
        );
        ds.start_episode("pick up the cube");
        ds.push_step(&[0.0, 1.0], &[0.5]).unwrap();
        ds.push_step(&[0.1, 0.9], &[-0.5]).unwrap();
        ds.finish_episode();
        ds
    }

    #[test]
    fn info_json_carries_the_v21_contract() {
        let ds = sample();
        let info = ds.info_json();
        for key in [
            "\"codebase_version\":\"v2.1\"",
            "\"robot_type\":\"oriera-arm-6\"",
            "\"total_episodes\":1",
            "\"total_frames\":2",
            "\"fps\":60",
            "\"observation.state\":{\"dtype\":\"float32\",\"shape\":[2]",
            "\"action\":{\"dtype\":\"float32\",\"shape\":[1]",
            "episode_{episode_index:06d}.parquet",
        ] {
            assert!(info.contains(key), "missing {key} in {info}");
        }
    }

    #[test]
    fn stats_are_exact_on_toy_data() {
        let mut ds = LeRobotV21::new(60.0, "t", vec!["x".into()], vec!["a".into()]);
        ds.start_episode("t");
        ds.push_step(&[1.0], &[0.0]).unwrap();
        ds.push_step(&[3.0], &[0.0]).unwrap();
        ds.finish_episode();
        let stats = ds.stats_json();
        // state column: min 1, max 3, mean 2, std 1, count 2
        assert!(stats.contains("\"min\":[1.0]"), "{stats}");
        assert!(stats.contains("\"max\":[3.0]"), "{stats}");
        assert!(stats.contains("\"mean\":[2.0000000]"), "{stats}");
        assert!(stats.contains("\"std\":[1.0000000]"), "{stats}");
        assert!(stats.contains("\"count\":2"), "{stats}");
    }

    #[test]
    fn rows_and_meta_are_wellformed() {
        let ds = sample();
        let line = ds.row_jsonl(0, &ds.episodes[0], 1, 1);
        assert_eq!(line.matches('{').count(), line.matches('}').count());
        for col in [
            "\"observation.state\":[0.1, 0.9]",
            "\"action\":[-0.5]",
            "\"timestamp\":0.016667",
            "\"frame_index\":1",
            "\"episode_index\":0",
            "\"index\":1",
            "\"task_index\":0",
        ] {
            assert!(line.contains(col), "missing {col} in {line}");
        }
        assert!(ds.episodes[0].task_index == 0);
    }

    #[test]
    fn write_dir_produces_the_v21_tree() {
        let ds = sample();
        let root = std::env::temp_dir().join(format!("oriera-lerobot-test-{}", std::process::id()));
        let n = ds.write_dir(&root).expect("write_dir");
        assert_eq!(n, 1);
        for rel in [
            "meta/info.json",
            "meta/stats.json",
            "meta/tasks.jsonl",
            "meta/episodes.jsonl",
            "data/chunk-000/episode_000000.jsonl",
        ] {
            assert!(root.join(rel).exists(), "missing {rel}");
        }
        LeRobotV21::write_receipt(&root, "test/0.1", "rigid3d", 1.0 / 60.0, &[0xbeef], 1)
            .expect("receipt");
        assert!(root.join("meta/oriera_receipt.json").exists());
        LeRobotV21::write_card(&root, "pick up the cube").expect("card");
        assert!(root.join("README.md").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn repeated_tasks_deduplicate() {
        let mut ds = LeRobotV21::new(30.0, "t", vec!["x".into()], vec!["a".into()]);
        ds.start_episode("same task");
        ds.finish_episode();
        ds.start_episode("same task");
        ds.finish_episode();
        assert_eq!(ds.tasks.len(), 1, "tasks must dedupe");
        assert_eq!(ds.episodes.len(), 2);
    }
}
