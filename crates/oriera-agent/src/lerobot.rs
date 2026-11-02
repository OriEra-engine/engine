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
/// v2.1 bookkeeping columns. M0a (P-8): the feature shapes/names, the
/// modality.json groups and `robot_type` are all derived from the
/// env's [`ObsLayout`]/RobotSpec via [`LeRobotV21::from_layout`] —
/// nothing is hardcoded per-dimension any more.
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
    /// `(slice name, start, end)` runs over the state vector and the
    /// action vector — the modality.json / v3-metadata source. Empty ⇒
    /// the legacy fixed-layout fallback (byte-stable 21/5 output).
    pub obs_slices: Vec<(String, usize, usize)>,
    pub action_slices: Vec<(String, usize, usize)>,
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
            obs_slices: Vec::new(),
            action_slices: Vec::new(),
            next_index: 0,
        }
    }

    /// M0a constructor: everything derives from the env's layouts.
    /// `robot_type` is the RobotSpec name; the obs/action dim names
    /// come from [`crate::embodied_env::ObsLayout`]; the modality
    /// slices from its slice offsets.
    pub fn from_layout(
        fps: f32,
        robot_type: &str,
        obs_layout: &crate::embodied_env::ObsLayout,
        action_layout: &crate::embodied_env::ObsLayout,
    ) -> Self {
        Self {
            fps,
            robot_type: robot_type.to_string(),
            obs_names: obs_layout.flat_names(),
            action_names: action_layout.flat_names(),
            obs_slices: obs_layout.slices_with_offsets(),
            action_slices: action_layout.slices_with_offsets(),
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

    /// The `features` block shared by the v2.1 and v3.0 info.json
    /// (dtypes/shapes/names derive from the layouts — M0a P-8).
    fn features_json(&self) -> String {
        let state_shape = self.obs_names.len().to_string();
        let action_shape = self.action_names.len().to_string();
        let obs_names = json_string_array(&self.obs_names);
        let action_names = json_string_array(&self.action_names);
        // Camera feature (feature `cam`): a video entry whose frames
        // the converter assembles into mp4.
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
        format!(
            concat!(
                "\"observation.state\":{{\"dtype\":\"float32\",\"shape\":[{sd}],\"names\":{{\"joints\":{on}}}}},",
                "\"action\":{{\"dtype\":\"float32\",\"shape\":[{ad}],\"names\":{{\"joints\":{an}}}}},",
                "\"timestamp\":{{\"dtype\":\"float32\",\"shape\":[1],\"names\":null}},",
                "\"frame_index\":{{\"dtype\":\"int64\",\"shape\":[1],\"names\":null}},",
                "\"episode_index\":{{\"dtype\":\"int64\",\"shape\":[1],\"names\":null}},",
                "\"index\":{{\"dtype\":\"int64\",\"shape\":[1],\"names\":null}},",
                "\"task_index\":{{\"dtype\":\"int64\",\"shape\":[1],\"names\":null}}",
                "{cam_feat}"
        ),
            sd = state_shape,
            ad = action_shape,
            on = obs_names,
            an = action_names,
            cam_feat = cam_feat,
        )
    }

    /// `meta/info.json` — the v2.1 schema header. `data_path` already
    /// points at the parquet layout the converter produces.
    fn info_json(&self) -> String {
        let n_eps = self.episodes.len();
        let total_videos = self
            .camera
            .as_ref()
            .map_or(0, |_| {
                self.episodes
                    .iter()
                    .filter(|e| !e.frames.is_empty())
                    .count()
            })
            .to_string();
        format!(
            concat!(
                "{{\"codebase_version\":\"v2.1\",\"robot_type\":\"{robot}\",\"license\":\"apache-2.0\",",
                "\"total_episodes\":{eps},\"total_frames\":{frames},",
                "\"total_tasks\":{tasks},\"total_videos\":{videos},\"total_chunks\":1,",
                "\"chunks_size\":1000,\"fps\":{fps},\"splits\":{{\"train\":\"0:{eps}\"}},",
                "\"data_path\":\"data/chunk-{{episode_chunk:03d}}/episode_{{episode_index:06d}}.parquet\",",
                "\"video_path\":\"videos/chunk-{{episode_chunk:03d}}/{{video_key}}/episode_{{episode_index:06d}}.mp4\",",
                "\"features\":{{{features}}}}}\n"
        ),
            robot = self.robot_type,
            eps = n_eps,
            frames = self.total_frames(),
            tasks = self.tasks.len(),
            videos = total_videos,
            fps = self.fps,
            features = self.features_json(),
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
    /// episodes written. Every file is content-hashed into
    /// `MANIFEST.jsonl` — written LAST via tmp+rename as the atomic
    /// commit point: a reader verifies the manifest before trusting
    /// the tree, and a crashed run leaves no valid commit (deterministic
    /// re-export is byte-identical, so re-runs are naturally idempotent).
    pub fn write_dir(&self, root: &Path) -> std::io::Result<u32> {
        let meta = root.join("meta");
        std::fs::create_dir_all(root.join("data/chunk-000"))?;
        std::fs::create_dir_all(&meta)?;
        let mut files: Vec<(String, u64)> = Vec::new();
        let mut put = |rel: &str, content: &[u8]| -> std::io::Result<()> {
            std::fs::write(root.join(rel), content)?;
            files.push((rel.to_string(), content_hash(content)));
            Ok(())
        };
        put("meta/info.json", self.info_json().as_bytes())?;
        put("meta/stats.json", self.stats_json().as_bytes())?;

        let mut tasks = String::new();
        for (i, t) in self.tasks.iter().enumerate() {
            tasks.push_str(&format!(
                "{{\"task_index\":{},\"task\":\"{}\"}}\n",
                i,
                json_escape(t)
            ));
        }
        put("meta/tasks.jsonl", tasks.as_bytes())?;

        let mut episodes = String::new();
        for (i, ep) in self.episodes.iter().enumerate() {
            episodes.push_str(&format!(
                "{{\"episode_index\":{},\"tasks\":[\"{}\"],\"length\":{}}}\n",
                i,
                json_escape(&self.tasks[ep.task_index as usize]),
                ep.rows.len()
            ));
        }
        put("meta/episodes.jsonl", episodes.as_bytes())?;

        // Rows: one JSONL per episode, the converter's input.
        let mut global = 0u64;
        for (i, ep) in self.episodes.iter().enumerate() {
            let mut body = String::new();
            for (ri, _row) in ep.rows.iter().enumerate() {
                body.push_str(&self.row_jsonl(i as u32, ep, ri, global));
                global += 1;
            }
            put(
                &format!("data/chunk-000/episode_{i:06}.jsonl"),
                body.as_bytes(),
            )?;
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
                    let rel = format!(
                        "videos/chunk-000/observation.images.{}/episode_{i:06}/frame_{k:06}.png",
                        cam.name
                    );
                    put(&rel, png)?;
                }
            }
        }
        self.write_modality(root)?;
        // write_modality writes its own file — hash it too (re-read).
        let modality_rel = "meta/modality.json";
        let modality_bytes = std::fs::read(root.join(modality_rel))?;
        files.push((modality_rel.to_string(), content_hash(&modality_bytes)));

        // Atomic commit: MANIFEST last, via tmp+rename (the only
        // multi-word op; everything else is already durable).
        let mut manifest = String::new();
        for (rel, hash) in &files {
            manifest.push_str(&format!(
                "{{\"file\":\"{rel}\",\"hash\":\"{hash:016x}\"}}\n"
            ));
        }
        let manifest_path = root.join("MANIFEST.jsonl");
        let tmp = root.join("MANIFEST.jsonl.tmp");
        std::fs::write(&tmp, manifest.as_bytes())?;
        std::fs::rename(&tmp, &manifest_path)?;
        Ok(self.episodes.len() as u32)
    }

    /// `meta/oriera_receipt.json` — the reproducibility claim: what
    /// ran, with which backend/step, the spec's identity hash and
    /// every episode's fingerprint.
    pub fn write_receipt(
        root: &Path,
        engine: &str,
        backend: &str,
        dt: f32,
        spec_hash: &str,
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
                "\"spec_hash\":\"{}\",",
                "\"successes\":{},\"episode_state_hashes\":[{}]}}\n"
            ),
            engine,
            backend,
            dt,
            spec_hash,
            successes,
            hashes.join(",")
        )
    }

    /// `meta/modality.json` — the GR00T N1 "LeRobot v2 flavor"
    /// dimension slicing: which state/action dims belong to which
    /// semantic group. M0a: derived from the [`ObsLayout`] slices
    /// (P-8); without them the legacy fixed-layout fallback keeps the
    /// byte-stable 21/5 output.
    pub fn write_modality(&self, root: &Path) -> std::io::Result<()> {
        let meta = root.join("meta");
        std::fs::create_dir_all(&meta)?;
        let body = if !self.obs_slices.is_empty() || !self.action_slices.is_empty() {
            let group = |slices: &[(String, usize, usize)]| -> String {
                slices
                    .iter()
                    .map(|(n, s, e)| format!("\"{n}\":{{\"start\":{s},\"end\":{e}}}"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            format!(
                "{{\"state\":{{{}}},\"action\":{{{}}}}}\n",
                group(&self.obs_slices),
                group(&self.action_slices)
            )
        } else {
            let (obs_n, act_n) = (self.obs_names.len(), self.action_names.len());
            if obs_n == 21 && act_n == 5 {
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
            }
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

    // ===================== LeRobotDataset v3.0 export (P-7) =====================
    //
    // lerobot 0.4 (2025-10) reads v3.0 only — v2.1 exports are rejected.
    // The v3.0 layout as documented publicly (docs + real datasets like
    // lerobot/droid_1.0.1; NOT verified against a local lerobot install
    // — see scripts/lerobot_v3_selfcheck.py for the structural audit):
    //
    //   meta/info.json            codebase_version "v3.0"; v3 adds
    //                             data/video_files_size_in_mb, drops
    //                             total_chunks/total_videos; data_path
    //                             template becomes chunk/file based
    //   meta/tasks.parquet        stand-in: meta/tasks.jsonl (converter → parquet)
    //   meta/episodes/chunk-000/file-000.parquet
    //                             stand-in: same-path .jsonl; one row per
    //                             episode with dataset_from/to_index,
    //                             data|videos|meta chunk/file pointers,
    //                             from/to timestamps and per-episode stats
    //   data/chunk-000/file-000.parquet
    //                             stand-in: .jsonl; episodes CONSOLIDATED
    //                             into shared files (v3 shards, "a filename
    //                             is not an episode boundary")
    //   videos/<video_key>/chunk-000/file-000.mp4
    //                             stand-in: PNG frame dir; the converter
    //                             concatenates episodes into one mp4 per
    //                             file using the episodes' from/to timestamps
    //
    // Small datasets (≤1000 episodes, far below 100 MB) land in exactly
    // one chunk and one file — the templates stay v3-correct.

    /// v3.0 `meta/info.json`. Per the public format: chunk/file templates
    /// + `data_files_size_in_mb`/`video_files_size_in_mb`; no
    ///   total_chunks/total_videos; `video_path` null without a camera.
    fn info_json_v3(&self) -> String {
        let n_eps = self.episodes.len();
        let video_path = self
            .camera
            .as_ref()
            .map_or_else(
                || "null".to_string(),
                |_| {
                    "\"videos/{video_key}/chunk-{chunk_index:03d}/file-{file_index:03d}.mp4\""
                        .to_string()
                },
            );
        format!(
            concat!(
                "{{\"codebase_version\":\"v3.0\",\"robot_type\":\"{robot}\",\"license\":\"apache-2.0\",",
                "\"total_episodes\":{eps},\"total_frames\":{frames},\"total_tasks\":{tasks},",
                "\"chunks_size\":1000,\"data_files_size_in_mb\":100,\"video_files_size_in_mb\":500,",
                "\"fps\":{fps},\"splits\":{{\"train\":\"0:{eps}\"}},",
                "\"data_path\":\"data/chunk-{{chunk_index:03d}}/file-{{file_index:03d}}.parquet\",",
                "\"video_path\":{video_path},",
                "\"features\":{{{features}}}}}\n"
        ),
            robot = self.robot_type,
            eps = n_eps,
            frames = self.total_frames(),
            tasks = self.tasks.len(),
            fps = self.fps,
            video_path = video_path,
            features = self.features_json(),
        )
    }

    /// One v3 `meta/episodes` row per episode: the episode boundary and
    /// pointer metadata the v3 reader reconstructs episodes from, plus
    /// this episode's per-feature stats (`stats/<col>/min|...`).
    fn episode_row_v3(&self, ep_index: usize, ep: &Episode, from_index: u64) -> String {
        let length = ep.rows.len();
        let to_index = from_index + length as u64;
        let mut rows: Vec<Vec<f32>> = Vec::new();
        let mut acts: Vec<Vec<f32>> = Vec::new();
        let mut stamps: Vec<f32> = Vec::new();
        for (i, row) in ep.rows.iter().enumerate() {
            rows.push(row.state.clone());
            acts.push(row.action.clone());
            stamps.push(i as f32 / self.fps);
        }
        let st = stats_of(&rows);
        let ac = stats_of(&acts);
        let ts = stats_of(&stamps.iter().map(|v| vec![*v]).collect::<Vec<_>>());
        let video_meta = match self.camera.as_ref() {
            Some(c) if !ep.frames.is_empty() => {
                let from_t = from_index as f32 / self.fps;
                let to_t = to_index as f32 / self.fps;
                format!(
                    ",\"videos/observation.images.{name}/chunk_index\":0,\
                     \"videos/observation.images.{name}/file_index\":0,\
                     \"videos/observation.images.{name}/from_timestamp\":{ft:.6},\
                     \"videos/observation.images.{name}/to_timestamp\":{tt:.6}",
                    name = c.name,
                    ft = from_t,
                    tt = to_t
                )
            }
            _ => String::new(),
        };
        let task = json_escape(&self.tasks[ep.task_index as usize]);
        format!(
            concat!(
                "{{\"episode_index\":{ep},\"tasks\":[\"{task}\"],\"length\":{len},",
                "\"dataset_from_index\":{from},\"dataset_to_index\":{to},",
                "\"data/chunk_index\":0,\"data/file_index\":0{video_meta},",
                "\"meta/episodes/chunk_index\":0,\"meta/episodes/file_index\":0,",
                "\"stats/observation.state/min\":{smin},\"stats/observation.state/max\":{smax},",
                "\"stats/observation.state/mean\":{smean},\"stats/observation.state/std\":{sstd},",
                "\"stats/observation.state/count\":{scount},",
                "\"stats/action/min\":{amin},\"stats/action/max\":{amax},",
                "\"stats/action/mean\":{amean},\"stats/action/std\":{astd},",
                "\"stats/action/count\":{acount},",
                "\"stats/timestamp/min\":{tmin},\"stats/timestamp/max\":{tmax},",
                "\"stats/timestamp/mean\":{tmean},\"stats/timestamp/std\":{tstd},",
                "\"stats/timestamp/count\":{tcount}}}\n"
            ),
            ep = ep_index,
            task = task,
            len = length,
            from = from_index,
            to = to_index,
            video_meta = video_meta,
            smin = fmt_f32s(&st.min),
            smax = fmt_f32s(&st.max),
            smean = fmt_f32s(&st.mean),
            sstd = fmt_f32s(&st.std),
            scount = st.count,
            amin = fmt_f32s(&ac.min),
            amax = fmt_f32s(&ac.max),
            amean = fmt_f32s(&ac.mean),
            astd = fmt_f32s(&ac.std),
            acount = ac.count,
            tmin = fmt_f32s(&ts.min),
            tmax = fmt_f32s(&ts.max),
            tmean = fmt_f32s(&ts.mean),
            tstd = fmt_f32s(&ts.std),
            tcount = ts.count,
        )
    }

    /// Write the v3.0 tree under `root`. Same honesty as [`Self::write_dir`]:
    /// rows land as JSONL stand-ins at the exact v3 paths (numeric-only,
    /// zero dependencies); `scripts/jsonl_to_lerobot_v21.py` converts them
    /// to parquet/mp4 in place. Returns the number of episodes.
    pub fn write_dir_v3(&self, root: &Path) -> std::io::Result<u32> {
        let meta = root.join("meta");
        std::fs::create_dir_all(root.join("data/chunk-000"))?;
        std::fs::create_dir_all(meta.join("episodes/chunk-000"))?;
        let mut files: Vec<(String, u64)> = Vec::new();
        let mut put = |rel: &str, content: &[u8]| -> std::io::Result<()> {
            std::fs::write(root.join(rel), content)?;
            files.push((rel.to_string(), content_hash(content)));
            Ok(())
        };
        put("meta/info.json", self.info_json_v3().as_bytes())?;
        put("meta/stats.json", self.stats_json().as_bytes())?;
        // write_modality writes its own file — hash it too (re-read),
        // mirroring the v2.1 write_dir bookkeeping.
        self.write_modality(root)?;
        let modality_rel = "meta/modality.json";
        let modality_bytes = std::fs::read(root.join(modality_rel))?;
        put(modality_rel, &modality_bytes)?;

        // tasks: v3 parquet stand-in (index=task string, column task_index)
        let mut tasks = String::new();
        for (i, t) in self.tasks.iter().enumerate() {
            tasks.push_str(&format!(
                "{{\"task_index\":{},\"task\":\"{}\"}}\n",
                i,
                json_escape(t)
            ));
        }
        put("meta/tasks.jsonl", tasks.as_bytes())?;

        // episodes metadata: one row per episode (v3 shards by chunk/file;
        // everything below 1000 episodes / 100 MB is chunk-000/file-000)
        let mut ep_meta = String::new();
        let mut from_index = 0u64;
        for (i, ep) in self.episodes.iter().enumerate() {
            ep_meta.push_str(&self.episode_row_v3(i, ep, from_index));
            from_index += ep.rows.len() as u64;
        }
        put("meta/episodes/chunk-000/file-000.jsonl", ep_meta.as_bytes())?;

        // data rows: ALL episodes consolidated into one shared file —
        // in v3 a filename is not an episode boundary (the episodes
        // metadata carries dataset_from/to_index).
        let mut global = 0u64;
        let mut body = String::new();
        for (i, ep) in self.episodes.iter().enumerate() {
            for (ri, _row) in ep.rows.iter().enumerate() {
                body.push_str(&self.row_jsonl(i as u32, ep, ri, global));
                global += 1;
            }
        }
        put("data/chunk-000/file-000.jsonl", body.as_bytes())?;

        // camera: one shared PNG frame dir per video file; the
        // converter concatenates every episode's run into the single
        // mp4 the episodes metadata points into.
        if let Some(cam) = &self.camera {
            let dir = root.join(format!(
                "videos/observation.images.{}/chunk-000/file-000",
                cam.name
            ));
            std::fs::create_dir_all(&dir)?;
            let mut k = 0u64;
            for ep in &self.episodes {
                for png in &ep.frames {
                    let rel = format!(
                        "videos/observation.images.{}/chunk-000/file-000/frame_{k:06}.png",
                        cam.name
                    );
                    put(&rel, png)?;
                    k += 1;
                }
            }
        }

        // atomic commit, same as v2.1
        let mut manifest = String::new();
        for (rel, hash) in &files {
            manifest.push_str(&format!("{{\"file\":\"{rel}\",\"hash\":\"{hash:016x}\"}}\n"));
        }
        let manifest_path = root.join("MANIFEST.jsonl");
        let tmp = root.join("MANIFEST.jsonl.tmp");
        std::fs::write(&tmp, manifest.as_bytes())?;
        std::fs::rename(&tmp, &manifest_path)?;
        Ok(self.episodes.len() as u32)
    }

    /// v3 dataset card.
    pub fn write_card_v3(root: &Path, task: &str) -> std::io::Result<()> {
        let card = format!(
            concat!(
                "---\n",
                "task_categories: [robotics]\n",
                "license: apache-2.0\n",
                "tags: [oriera, embodied-ai, lerobot, simulation, lerobot-v3]\n",
                "---\n\n",
                "# OriEra 具身仿真数据集（LeRobotDataset v3.0）\n\n",
                "由 OriEra 世界引擎的 rigid3d 具身仿真后端采集（确定性 tick + 每集世界指纹回执，见 meta/oriera_receipt.json）。\n\n",
                "**任务**: {task}\n\n",
                "## 转换为 parquet/mp4（LeRobotDataset v3.0 直读）\n\n",
                "```bash\npython scripts/jsonl_to_lerobot_v21.py <本目录>```\n\n",
                "（转换脚本自动识别 v2.1 / v3.0 目录。）\n\n",
                "## 结构自检（未装 lerobot 时）\n\n",
                "```bash\npython scripts/lerobot_v3_selfcheck.py <本目录>```\n\n",
                "## 训练（HuggingFace lerobot ≥ 0.4）\n\n",
                "```bash\nlerobot-train --dataset.repo_id=<user>/<dataset> --policy.type=act\n",
                "# 或 --policy.type=smolvla（消费级 GPU 友好）```\n"
            ),
            task = task
        );
        std::fs::write(root.join("README.md"), card)
    }
}

/// Content hash for MANIFEST (splitmix64 byte-fold - corruption
/// detection, not cryptographic; swap to sha2 for public distribution).
/// Self-contained: lerobot.rs is a vendored single-file module (studio
/// single-source copy) and must not couple to oriera-subsys internals.
fn mix64(mut h: u64, x: u64) -> u64 {
    h ^= x;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 29;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^= h >> 32;
    h
}

fn content_hash(bytes: &[u8]) -> u64 {
    let mut h = 0x0000_6f72_6965_7261;
    for chunk in bytes.chunks(8) {
        let mut buf = [0u8; 8];
        buf[..chunk.len()].copy_from_slice(chunk);
        h = mix64(h, u64::from_le_bytes(buf));
    }
    mix64(h, bytes.len() as u64)
}

fn json_escape(s: &str) -> String {
    // 控制字符全转义（安全深检：task 含 \n 可产生非法 JSONL 毒化下游）。
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn json_string_array(items: &[String]) -> String {
    let inner: Vec<String> = items
        .iter()
        .map(|s| format!("\"{}\"", json_escape(s)))
        .collect();
    format!("[{}]", inner.join(","))
}

/// Per-column batch statistics (min/max/mean/std over ragged float
/// rows, columns padded by first-row width — features are fixed-width
/// by construction). Shared by stats.json and the v3 episodes metadata.
struct Stats {
    min: Vec<f32>,
    max: Vec<f32>,
    mean: Vec<f32>,
    std: Vec<f32>,
    count: usize,
}

fn stats_of(rows: &[Vec<f32>]) -> Stats {
    let Some(first) = rows.first() else {
        return Stats {
            min: vec![],
            max: vec![],
            mean: vec![],
            std: vec![],
            count: 0,
        };
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
    let mean: Vec<f32> = sum.iter().map(|s| (s / n as f64) as f32).collect();
    let std: Vec<f32> = sum_sq
        .iter()
        .zip(sum.iter())
        .map(|(sq, s)| {
            let var = (sq / n as f64) - (s / n as f64) * (s / n as f64);
            (var.max(0.0).sqrt()) as f32
        })
        .collect();
    Stats {
        min,
        max,
        mean,
        std,
        count: rows.len(),
    }
}

/// `{:?}` of a float vec (Rust's shortest round-trip form, same as the
/// pre-M0a stats.json formatting).
fn fmt_f32s(v: &[f32]) -> String {
    format!("{:?}", v.to_vec())
}

fn vec_stats(rows: &[Vec<f32>]) -> String {
    let st = stats_of(rows);
    if st.count == 0 {
        return "{\"min\":[],\"max\":[],\"mean\":[],\"std\":[],\"count\":0}".to_string();
    }
    let mean: Vec<String> = st.mean.iter().map(|v| format!("{v:.7}")).collect();
    let std: Vec<String> = st.std.iter().map(|v| format!("{v:.7}")).collect();
    format!(
        "{{\"min\":{:?},\"max\":{:?},\"mean\":[{}],\"std\":[{}],\"count\":{}}}",
        st.min,
        st.max,
        mean.join(","),
        std.join(","),
        st.count
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
        LeRobotV21::write_receipt(&root, "test/0.1", "rigid3d", 1.0 / 60.0, "abc123", &[0xbeef], 1)
            .expect("receipt");
        let receipt =
            std::fs::read_to_string(root.join("meta/oriera_receipt.json")).unwrap();
        assert!(receipt.contains("\"spec_hash\":\"abc123\""), "{receipt}");
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

    // ----- M0a: layout-derived metadata (P-8) -----

    /// The legacy 21/5 modality.json body — the derived output must be
    /// byte-identical for the default layout.
    const LEGACY_MODALITY_21_5: &str = concat!(
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
        "\"gripper\":{\"start\":4,\"end\":5}}}\n"
    );

    #[test]
    fn layout_derived_modality_is_byte_identical_for_the_default() {
        let robot = oriera_subsys::RobotSpec::from(&oriera_subsys::ArmDescription::default());
        let task = crate::embodied_env::make_task(
            "pick_place@v1",
            crate::embodied_env::TaskParams::default(),
        )
        .unwrap();
        let obs = crate::embodied_env::ObsLayout::derive(&robot, task.as_ref());
        let act = crate::embodied_env::action_layout(&robot);
        let mut ds = LeRobotV21::from_layout(60.0, &robot.name, &obs, &act);
        ds.start_episode("pick up the cube");
        ds.push_step(&[0.0; 21], &[0.5; 5]).unwrap();
        ds.finish_episode();
        let root = std::env::temp_dir().join(format!("oriera-modality-{}", std::process::id()));
        ds.write_modality(&root).expect("write_modality");
        let body = std::fs::read_to_string(root.join("meta/modality.json")).unwrap();
        assert_eq!(body, LEGACY_MODALITY_21_5, "derived modality must be byte-identical");
        // info.json: names/shapes/robot_type all from the layout
        let info = ds.info_json();
        assert!(info.contains("\"robot_type\":\"tabletop-4dof\""), "{info}");
        assert!(info.contains("\"shape\":[21]"), "{info}");
        assert!(info.contains("\"base_yaw.pos\""), "{info}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_dir_v3_produces_the_v30_tree() {
        let mut ds = LeRobotV21::new(60.0, "tabletop-4dof", vec!["s0".into()], vec!["a0".into()]);
        ds.set_camera(CameraTrack {
            name: "top".into(),
            w: 4,
            h: 4,
            fps: 60.0,
        });
        ds.start_episode("pick up the cube");
        ds.push_step(&[0.0], &[0.5]).unwrap();
        ds.push_step(&[0.1], &[0.4]).unwrap();
        ds.push_frame(vec![1, 2, 3]).unwrap();
        ds.finish_episode();
        ds.start_episode("pick up the cube");
        ds.push_step(&[0.2], &[0.3]).unwrap();
        ds.finish_episode();
        let root =
            std::env::temp_dir().join(format!("oriera-lerobot-v3-{}", std::process::id()));
        let n = ds.write_dir_v3(&root).expect("write_dir_v3");
        assert_eq!(n, 2);
        for rel in [
            "meta/info.json",
            "meta/stats.json",
            "meta/tasks.jsonl",
            "meta/modality.json",
            "meta/episodes/chunk-000/file-000.jsonl",
            "data/chunk-000/file-000.jsonl",
            "videos/observation.images.top/chunk-000/file-000/frame_000000.png",
        ] {
            assert!(root.join(rel).exists(), "missing {rel}");
        }
        let info = std::fs::read_to_string(root.join("meta/info.json")).unwrap();
        for key in [
            "\"codebase_version\":\"v3.0\"",
            "\"chunks_size\":1000",
            "\"data_files_size_in_mb\":100",
            "\"video_files_size_in_mb\":500",
            "\"data_path\":\"data/chunk-{chunk_index:03d}/file-{file_index:03d}.parquet\"",
            "\"video_path\":\"videos/{video_key}/chunk-{chunk_index:03d}/file-{file_index:03d}.mp4\"",
            "\"total_episodes\":2",
            "\"total_frames\":3",
        ] {
            assert!(info.contains(key), "missing {key} in {info}");
        }
        let ep0 = std::fs::read_to_string(root.join("meta/episodes/chunk-000/file-000.jsonl"))
            .unwrap();
        for col in [
            "\"episode_index\":0",
            "\"length\":2",
            "\"dataset_from_index\":0",
            "\"dataset_to_index\":2",
            "\"data/chunk_index\":0",
            "\"data/file_index\":0",
            "\"videos/observation.images.top/from_timestamp\":0.000000",
            "\"videos/observation.images.top/to_timestamp\":0.033333",
            "\"meta/episodes/chunk_index\":0",
            "\"meta/episodes/file_index\":0",
            "\"stats/observation.state/min\":[0.0]",
            "\"stats/timestamp/count\":2",
        ] {
            assert!(ep0.contains(col), "missing {col} in {ep0}");
        }
        // episode 2 continues the global index
        let ep1_row = ep0.lines().nth(1).unwrap();
        assert!(ep1_row.contains("\"dataset_from_index\":2"), "{ep1_row}");
        assert!(ep1_row.contains("\"dataset_to_index\":3"), "{ep1_row}");
        // consolidated data file holds every row
        let data =
            std::fs::read_to_string(root.join("data/chunk-000/file-000.jsonl")).unwrap();
        assert_eq!(data.lines().count(), 3);
        LeRobotV21::write_card_v3(&root, "pick up the cube").expect("card v3");
        assert!(root.join("README.md").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
