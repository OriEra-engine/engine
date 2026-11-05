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
//!
//! **npz bare export (M2, PLAN-embodied-general §9.1 U-8)**:
//! [`LeRobotV21::write_npz`] writes the same rows as one `numpy.load`-able
//! `.npz` per episode (`obs_{i}.npy` / `act_{i}.npy` float32 + a
//! `meta.json` text member) — hand-written stored ZIP + NPY v1.0, zero
//! new crates. There is no parquet bare export: Rust has no std-only
//! parquet writer, so parquet stays on the pyarrow converter above.

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
    /// Camera tracks of this episode (feature `cam` + M3-A): one entry
    /// per camera, frames captured at the camera cadence by the env.
    /// Index order follows recorder declaration, not push order.
    pub tracks: Vec<CamTrackData>,
}

/// One camera track's frames within one episode: RGB PNG bytes and —
/// when the camera exports depth — the parallel millimetre u16 PNG
/// (`depth.{name}`) bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CamTrackData {
    /// Camera name = LeRobot feature suffix.
    pub cam: String,
    /// RGB8 PNG bytes, one per captured tick.
    pub rgb: Vec<Vec<u8>>,
    /// Depth PNG bytes (16-bit grayscale, millimetres, 0 = no return),
    /// one per captured tick.
    pub depth: Vec<Vec<u8>>,
}

/// Camera track declaration (feature `cam`): the legacy fixed top-down
/// view, or one spec camera (M3-A). `depth` adds the `depth.{name}`
/// millimetre u16 PNG track next to `observation.images.{name}`.
#[derive(Debug, Clone)]
pub struct CameraTrack {
    /// LeRobot feature key suffix: `observation.images.<name>`.
    pub name: String,
    pub w: u32,
    pub h: u32,
    /// Frame rate of the track (Hz) — half the control rate by default.
    pub fps: f32,
    /// Also export `depth.<name>` (16-bit grayscale PNG, millimetres).
    pub depth: bool,
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
    /// Camera tracks (feature `cam`; M3-A multi-camera): each becomes
    /// `observation.images.{name}` (+ `depth.{name}` when declared).
    /// Empty ⇒ state-only dataset.
    pub cameras: Vec<CameraTrack>,
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
            cameras: Vec::new(),
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
            cameras: Vec::new(),
            next_index: 0,
        }
    }

    /// Attach a single camera track (the legacy one-view form; must be
    /// set before the first `start_episode`). Multi-camera datasets
    /// use [`Self::add_camera`].
    pub fn set_camera(&mut self, track: CameraTrack) {
        self.cameras = vec![track];
    }

    /// Add one camera track (M3-A multi-video: one entry per spec
    /// camera, declaration order = feature order). Duplicate names are
    /// a contract error — they are LeRobot feature keys.
    pub fn add_camera(&mut self, track: CameraTrack) -> Result<(), String> {
        if self.cameras.iter().any(|c| c.name == track.name) {
            return Err(format!(
                "相机轨 {:?} 重复：camera 名是 LeRobot 特性键，必须唯一",
                track.name
            ));
        }
        self.cameras.push(track);
        Ok(())
    }

    /// Record one camera frame (the env's [`CameraFrame`]) for the open
    /// episode: RGB and/or depth PNG bytes, routed by camera name. The
    /// name must be declared via `set_camera`/`add_camera` first.
    pub fn push_frame(&mut self, frame: crate::embodied_env::CameraFrame) -> Result<(), String> {
        let crate::embodied_env::CameraFrame {
            cam,
            rgb_png,
            depth_png,
        } = frame;
        let track = self.cameras.iter().find(|c| c.name == cam).ok_or_else(|| {
            format!("push_frame：相机 {cam:?} 未声明（先 set_camera/add_camera）")
        })?;
        let ep = self
            .current
            .as_mut()
            .ok_or("push_frame 前须 start_episode")?;
        if depth_png.is_some() && !track.depth {
            return Err(format!(
                "push_frame：相机 {cam:?} 送来 depth 帧但未声明 depth 轨（CameraTrack.depth = false）"
            ));
        }
        let data = match ep.tracks.iter_mut().find(|t| t.cam == cam) {
            Some(t) => t,
            None => {
                ep.tracks.push(CamTrackData {
                    cam: cam.clone(),
                    rgb: Vec::new(),
                    depth: Vec::new(),
                });
                ep.tracks.last_mut().expect("just pushed")
            }
        };
        if let Some(png) = rgb_png {
            data.rgb.push(png);
        }
        if let Some(png) = depth_png {
            data.depth.push(png);
        }
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
            tracks: Vec::new(),
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
    /// (dtypes/shapes/names derive from the layouts — M0a P-8). One
    /// video entry per camera + one image entry per declared depth
    /// track (M3-A multi-video).
    fn features_json(&self) -> String {
        let state_shape = self.obs_names.len().to_string();
        let action_shape = self.action_names.len().to_string();
        let obs_names = json_string_array(&self.obs_names);
        let action_names = json_string_array(&self.action_names);
        // Camera features (feature `cam`): video entries whose frames
        // the converter assembles into mp4; depth tracks ride as u16
        // PNG sequences (dtype "image" — no mp4 for depth).
        let mut cam_feats = String::new();
        for c in &self.cameras {
            cam_feats.push_str(&format!(
                ",\"observation.images.{name}\":{{\"dtype\":\"video\",\"shape\":[{h},{w},3],\"names\":null,\"info\":{{\"video.fps\":{fps},\"video.codec\":\"libx264\",\"video.pix_fmt\":\"yuv420p\",\"has_audio\":false}}}}",
                name = c.name,
                h = c.h,
                w = c.w,
                fps = c.fps
            ));
            if c.depth {
                cam_feats.push_str(&format!(
                    ",\"depth.{name}\":{{\"dtype\":\"image\",\"shape\":[{h},{w},1],\"names\":null,\"info\":{{\"png.bit_depth\":16,\"png.color_type\":\"grayscale\",\"oriera.unit\":\"millimetre\",\"oriera.no_return\":0}}}}",
                    name = c.name,
                    h = c.h,
                    w = c.w
                ));
            }
        }
        let cam_feat = cam_feats.as_str();
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
            .cameras
            .iter()
            .map(|c| {
                self.episodes
                    .iter()
                    .filter(|e| !track_rgb(e, &c.name).is_empty())
                    .count()
            })
            .sum::<usize>()
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
        // Camera tracks: PNG frame dirs — the converter assembles mp4
        // (v2.1 video features live outside the parquet tables). Depth
        // tracks land as `depth.{name}` dirs of u16 PNGs (M3-A).
        // DE-P2：RGB 与深度**各自独立判空落盘**——RGB 轨空而深度轨非空
        // 的 episode（丢帧/RGB 关闭的采集）也要写出深度目录；旧代码
        // `if track_rgb(..).is_empty() { continue }` 会把整台相机的深度
        // 轨一并丢掉。（write_dir_v3 的深度循环本就独立，无此问题。）
        for cam in &self.cameras {
            for (i, ep) in self.episodes.iter().enumerate() {
                let rgb = track_rgb(ep, &cam.name);
                if !rgb.is_empty() {
                    let dir = root.join(format!(
                        "videos/chunk-000/observation.images.{}/episode_{i:06}",
                        cam.name
                    ));
                    std::fs::create_dir_all(&dir)?;
                    for (k, png) in rgb.iter().enumerate() {
                        let rel = format!(
                            "videos/chunk-000/observation.images.{}/episode_{i:06}/frame_{k:06}.png",
                            cam.name
                        );
                        put(&rel, png)?;
                    }
                }
                let depth = track_depth(ep, &cam.name);
                if !depth.is_empty() {
                    let ddir = root.join(format!(
                        "videos/chunk-000/depth.{}/episode_{i:06}",
                        cam.name
                    ));
                    std::fs::create_dir_all(&ddir)?;
                    for (k, png) in depth.iter().enumerate() {
                        let rel = format!(
                            "videos/chunk-000/depth.{}/episode_{i:06}/frame_{k:06}.png",
                            cam.name
                        );
                        put(&rel, png)?;
                    }
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
        // DE-P1 提交点语义：本清单是「引擎直出目录」的提交点。转换
        // （scripts/jsonl_to_lerobot_v21.py）把清单指向的行文件归档进
        // raw_jsonl/ 后，会在同级落 `MANIFEST.converted.json`（原清单
        // 条目 + converted/converted_at 标记）作为**转换后目录**的提交
        // 点——本清单届时退行为 raw_jsonl 归档内容的回执（哈希仍可对
        // raw_jsonl/ 里的文件逐条对账）。
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
    /// every episode's fingerprint. `scheme`/`v2_hashes` carry the
    /// M0b identity scheme（v1=身份三元组折叠；v2=state⊕identity⊕spec）；
    /// v2 为空数组时省略该字段（v1 采集路径字节不变）。
    #[allow(clippy::too_many_arguments)] // 回执字段平铺即文档；打包 struct 反而藏 shape
    pub fn write_receipt(
        root: &Path,
        engine: &str,
        backend: &str,
        dt: f32,
        spec_hash: &str,
        episode_hashes: &[u64],
        successes: u32,
        scheme: &str,
        v2_hashes: &[u64],
    ) -> std::io::Result<()> {
        let meta = root.join("meta");
        std::fs::create_dir_all(&meta)?;
        let mut f = std::fs::File::create(meta.join("oriera_receipt.json"))?;
        let hashes: Vec<String> = episode_hashes.iter().map(|h| h.to_string()).collect();
        let v2s: Vec<String> = v2_hashes.iter().map(|h| h.to_string()).collect();
        let v2_field = if v2_hashes.is_empty() {
            String::new()
        } else {
            format!(
                "\"fingerprint_scheme\":\"{}\",\"episode_fingerprints_v2\":[{}],",
                scheme,
                v2s.join(",")
            )
        };
        // P0-4（20 角色审查 DE-1）：`episode_state_hashes` 一名两义——
        // receipt 里是 v1 身份折叠、eval_report 里是纯状态哈希，跨文件
        // 对账静默踩坑。规范名改 `episode_fingerprints_v1`；旧键保留为
        // 兼容别名（同值），存量脚本（CI 矩阵/golden check）零改动。
        write!(
            f,
            concat!(
                "{{\"codebase_version\":\"oriera-embodied-v2\",",
                "\"engine\":\"{}\",\"backend\":\"{}\",\"dt\":{:.8},",
                "\"spec_hash\":\"{}\",",
                "{}",
                "\"episode_fingerprints_v1\":[{}],",
                "\"episode_state_hashes\":[{}],",
                "\"episode_state_hashes_note\":\"legacy alias of episode_fingerprints_v1\",",
                "\"successes\":{}}}\n"
            ),
            engine,
            backend,
            dt,
            spec_hash,
            v2_field,
            hashes.join(","),
            hashes.join(","),
            successes
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
        std::fs::write(meta.join("modality.json"), self.modality_json())
    }

    /// The `modality.json` body (shared by [`Self::write_modality`] and the
    /// npz `meta.json`, which embeds it as `"modality"`).
    fn modality_json(&self) -> String {
        if !self.obs_slices.is_empty() || !self.action_slices.is_empty() {
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
        }
    }

    // ===================== npz bare export (M2, PLAN §9.1 U-8) =====================
    //
    // `numpy.load` reads an `.npz` = a plain ZIP archive whose members are
    // `.npy` arrays; members without the NPY magic come back as raw bytes.
    // No compression is required — `np.savez` itself writes ZIP_STORED —
    // so the whole format is two fixed-layout headers, hand-written below
    // ([`npy_f32_2d`], [`zip_stored`], [`crc32`]; std only, the #80 house
    // style). One file per episode:
    //
    //   <root>/episode_000000.npz
    //     ├── obs_0.npy   float32 [N, obs_dim]   observation.state rows
    //     ├── act_0.npy   float32 [N, act_dim]   action rows
    //     └── meta.json   UTF-8 JSON (np.load(p)["meta.json"] → bytes)
    //   <root>/MANIFEST.jsonl                    content hashes, written last
    //
    // The arrays are the SAME `Row`s the LeRobot trees serialize (one
    // dataset, several encodings), as raw little-endian f32 — no text
    // round-trip, so `obs_i[k]` equals `observation.state` of row k of
    // episode i bit for bit. Camera frames are not part of the npz (the
    // PNG/mp4 track stays on the LeRobot exports).

    /// Per-episode `meta.json` of the npz export: which member is which,
    /// shapes, dim names and the modality slices (no LeRobot needed to
    /// slice the vectors), plus the episode's place in the dataset.
    fn npz_meta_json(&self, ep_index: usize, ep: &Episode, from_index: u64) -> String {
        let n = ep.rows.len();
        format!(
            concat!(
                "{{\"format\":\"oriera-npz/1\",\"episode_index\":{i},",
                "\"obs_key\":\"obs_{i}\",\"act_key\":\"act_{i}\",\"dtype\":\"float32\",",
                "\"obs_shape\":[{n},{od}],\"act_shape\":[{n},{ad}],\"length\":{n},",
                "\"dataset_from_index\":{from},\"dataset_to_index\":{to},",
                "\"fps\":{fps},\"robot_type\":\"{robot}\",\"task_index\":{ti},\"task\":\"{task}\",",
                "\"obs_names\":{on},\"action_names\":{an},\"modality\":{modality}}}\n"
            ),
            i = ep_index,
            n = n,
            od = self.obs_names.len(),
            ad = self.action_names.len(),
            from = from_index,
            to = from_index + n as u64,
            fps = self.fps,
            robot = json_escape(&self.robot_type),
            ti = ep.task_index,
            task = json_escape(&self.tasks[ep.task_index as usize]),
            on = json_string_array(&self.obs_names),
            an = json_string_array(&self.action_names),
            modality = self.modality_json().trim_end(),
        )
    }

    /// Write one `.npz` per episode under `root` (`episode_{i:06}.npz`
    /// holding `obs_{i}.npy`, `act_{i}.npy`, `meta.json`) and commit a
    /// `MANIFEST.jsonl` last via tmp+rename — the same protocol as
    /// [`Self::write_dir`]. Output is byte-deterministic (fixed ZIP
    /// timestamps), so re-exports are idempotent. Returns the number of
    /// episodes written.
    pub fn write_npz(&self, root: &Path) -> std::io::Result<u32> {
        // Contract first (rows are pub — push_step's check can be
        // bypassed): a ragged row must fail before any file is written.
        let (od, ad) = (self.obs_names.len(), self.action_names.len());
        for (i, ep) in self.episodes.iter().enumerate() {
            for (k, row) in ep.rows.iter().enumerate() {
                for (what, got, want) in [
                    ("state", row.state.len(), od),
                    ("action", row.action.len(), ad),
                ] {
                    if got != want {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("npz：第 {i} 集第 {k} 行 {what} 维度 {got} ≠ {want}"),
                        ));
                    }
                }
            }
        }
        std::fs::create_dir_all(root)?;
        let mut files: Vec<(String, u64)> = Vec::new();
        let mut from_index = 0u64;
        for (i, ep) in self.episodes.iter().enumerate() {
            let n = ep.rows.len();
            let obs = npy_f32_2d(n, od, ep.rows.iter().map(|r| r.state.as_slice()));
            let act = npy_f32_2d(n, ad, ep.rows.iter().map(|r| r.action.as_slice()));
            let meta = self.npz_meta_json(i, ep, from_index);
            let zip = zip_stored(&[
                (format!("obs_{i}.npy"), obs),
                (format!("act_{i}.npy"), act),
                ("meta.json".to_string(), meta.into_bytes()),
            ])?;
            let rel = format!("episode_{i:06}.npz");
            std::fs::write(root.join(&rel), &zip)?;
            files.push((rel, content_hash(&zip)));
            from_index += n as u64;
        }
        let mut manifest = String::new();
        for (rel, hash) in &files {
            let line = format!("{{\"file\":\"{rel}\",\"hash\":\"{hash:016x}\"}}\n");
            manifest.push_str(&line);
        }
        let tmp = root.join("MANIFEST.jsonl.tmp");
        std::fs::write(&tmp, manifest.as_bytes())?;
        std::fs::rename(&tmp, root.join("MANIFEST.jsonl"))?;
        Ok(self.episodes.len() as u32)
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
        let video_path = if self.cameras.is_empty() {
            "null".to_string()
        } else {
            "\"videos/{video_key}/chunk-{chunk_index:03d}/file-{file_index:03d}.mp4\"".to_string()
        };
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
        let video_meta = {
            let from_t = from_index as f32 / self.fps;
            let to_t = to_index as f32 / self.fps;
            let mut meta = String::new();
            for c in &self.cameras {
                if track_rgb(ep, &c.name).is_empty() {
                    continue;
                }
                meta.push_str(&format!(
                    ",\"videos/observation.images.{name}/chunk_index\":0,\
                     \"videos/observation.images.{name}/file_index\":0,\
                     \"videos/observation.images.{name}/from_timestamp\":{ft:.6},\
                     \"videos/observation.images.{name}/to_timestamp\":{tt:.6}",
                    name = c.name,
                    ft = from_t,
                    tt = to_t
                ));
            }
            meta
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
        // mp4 the episodes metadata points into. Depth tracks get a
        // parallel `depth.{name}` dir of the same u16 PNG frames.
        for cam in &self.cameras {
            let dir = root.join(format!(
                "videos/observation.images.{}/chunk-000/file-000",
                cam.name
            ));
            std::fs::create_dir_all(&dir)?;
            let mut k = 0u64;
            for ep in &self.episodes {
                for png in track_rgb(ep, &cam.name) {
                    let rel = format!(
                        "videos/observation.images.{}/chunk-000/file-000/frame_{k:06}.png",
                        cam.name
                    );
                    put(&rel, png)?;
                    k += 1;
                }
            }
            if cam.depth {
                let ddir = root.join(format!("videos/depth.{}/chunk-000/file-000", cam.name));
                std::fs::create_dir_all(&ddir)?;
                let mut k = 0u64;
                for ep in &self.episodes {
                    for png in track_depth(ep, &cam.name) {
                        let rel = format!(
                            "videos/depth.{}/chunk-000/file-000/frame_{k:06}.png",
                            cam.name
                        );
                        put(&rel, png)?;
                        k += 1;
                    }
                }
            }
        }

        // atomic commit, same as v2.1
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

/// An episode's RGB frames for one camera (empty when that camera
/// captured nothing this episode).
fn track_rgb<'a>(ep: &'a Episode, cam: &str) -> &'a [Vec<u8>] {
    ep.tracks
        .iter()
        .find(|t| t.cam == cam)
        .map(|t| t.rgb.as_slice())
        .unwrap_or(&[])
}

/// An episode's depth frames for one camera (empty without depth).
fn track_depth<'a>(ep: &'a Episode, cam: &str) -> &'a [Vec<u8>] {
    ep.tracks
        .iter()
        .find(|t| t.cam == cam)
        .map(|t| t.depth.as_slice())
        .unwrap_or(&[])
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

/// NPY v1.0 encoding of a C-order little-endian float32 `[rows, cols]`
/// array (numpy `format.py`): 6-byte magic `\x93NUMPY`, version 1.0,
/// u16-LE header length, then a Python-literal dict padded with spaces
/// and closed by `\n` so magic+header is a multiple of 64 bytes (numpy's
/// ARRAY_ALIGN), then the raw values. The dict text is the one `np.save`
/// writes (numpy pads a little differently; any 64-aligned padding is
/// valid NPY). Rows must already be `cols` wide (checked by the caller).
fn npy_f32_2d<'a>(rows: usize, cols: usize, data: impl Iterator<Item = &'a [f32]>) -> Vec<u8> {
    let dict = format!("{{'descr': '<f4', 'fortran_order': False, 'shape': ({rows}, {cols}), }}");
    // 10 = magic(6) + version(2) + header_len(2); +1 for the closing '\n'.
    let pad = (64 - (10 + dict.len() + 1) % 64) % 64;
    let header_len = (dict.len() + pad + 1) as u16; // < 128 B: fits v1.0's u16
    let mut out = Vec::with_capacity(10 + header_len as usize + rows * cols * 4);
    out.extend_from_slice(b"\x93NUMPY\x01\x00");
    out.extend_from_slice(&header_len.to_le_bytes());
    out.extend_from_slice(dict.as_bytes());
    out.resize(out.len() + pad, b' ');
    out.push(b'\n');
    for row in data {
        for v in row {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

/// A minimal ZIP32 archive of STORED (uncompressed) members — all that
/// `numpy.load` needs for an `.npz`. Layout per PKWARE APPNOTE 4.3:
/// `[local file header + name + data]*`, then `[central directory
/// header + name]*`, then the end-of-central-directory record. All
/// integers little-endian; no data descriptors (sizes and CRC are known
/// up front, flag bit 3 clear), no extra fields, ASCII names. A fixed
/// DOS timestamp (1980-01-01 00:00) keeps the bytes deterministic.
/// Past the ZIP32 limits (4 GiB member/archive offsets, 65535 entries)
/// this errors instead of writing a corrupt file — ZIP64 is not worth
/// hand-writing for a per-episode file of ~60 KB.
fn zip_stored(members: &[(String, Vec<u8>)]) -> std::io::Result<Vec<u8>> {
    const VERSION: u16 = 20; // 2.0 = "needed to extract"; "made by" MS-DOS 2.0
    const DOS_TIME: u16 = 0; // 00:00:00
    const DOS_DATE: u16 = (1 << 5) | 1; // (year-1980)<<9 | month<<5 | day = 1980-01-01
    let limit = |what: &str| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("npz：{what} 超出 ZIP32 上限（4 GiB / 65535 项）——大数据请走 LeRobot 出口"),
        )
    };
    let le16 = |out: &mut Vec<u8>, v: u16| out.extend_from_slice(&v.to_le_bytes());
    let le32 = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in members {
        let crc = crc32(data);
        let size = u32::try_from(data.len()).map_err(|_| limit(name))?;
        let offset = u32::try_from(out.len()).map_err(|_| limit("归档"))?;
        let name_len = u16::try_from(name.len()).map_err(|_| limit(name))?;
        // Local file header (30 B + name), then the member bytes.
        le32(&mut out, 0x0403_4b50); // "PK\x03\x04"
        le16(&mut out, VERSION); // version needed to extract
        le16(&mut out, 0); // general-purpose flags
        le16(&mut out, 0); // compression method 0 = stored
        le16(&mut out, DOS_TIME);
        le16(&mut out, DOS_DATE);
        le32(&mut out, crc);
        le32(&mut out, size); // compressed size (= uncompressed: stored)
        le32(&mut out, size); // uncompressed size
        le16(&mut out, name_len);
        le16(&mut out, 0); // extra field length
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        // Central directory header (46 B + name) — same facts + the offset.
        le32(&mut central, 0x0201_4b50); // "PK\x01\x02"
        le16(&mut central, VERSION); // version made by
        le16(&mut central, VERSION); // version needed to extract
        le16(&mut central, 0); // flags
        le16(&mut central, 0); // method
        le16(&mut central, DOS_TIME);
        le16(&mut central, DOS_DATE);
        le32(&mut central, crc);
        le32(&mut central, size);
        le32(&mut central, size);
        le16(&mut central, name_len);
        le16(&mut central, 0); // extra field length
        le16(&mut central, 0); // file comment length
        le16(&mut central, 0); // disk number start
        le16(&mut central, 0); // internal attributes
        le32(&mut central, 0); // external attributes
        le32(&mut central, offset); // where the local header starts
        central.extend_from_slice(name.as_bytes());
    }
    let count = u16::try_from(members.len()).map_err(|_| limit("成员数"))?;
    let cd_offset = u32::try_from(out.len()).map_err(|_| limit("归档"))?;
    let cd_size = u32::try_from(central.len()).map_err(|_| limit("中央目录"))?;
    out.extend_from_slice(&central);
    // End of central directory record (22 B, no archive comment).
    le32(&mut out, 0x0605_4b50); // "PK\x05\x06"
    le16(&mut out, 0); // number of this disk
    le16(&mut out, 0); // disk where the central directory starts
    le16(&mut out, count); // entries on this disk
    le16(&mut out, count); // entries in total
    le32(&mut out, cd_size);
    le32(&mut out, cd_offset);
    le16(&mut out, 0); // comment length
    Ok(out)
}

/// CRC-32 (IEEE 802.3 / ZIP: reflected polynomial 0xEDB88320, init and
/// final XOR 0xFFFFFFFF), table-driven; the table is built at compile
/// time. Check value: `crc32(b"123456789") == 0xCBF43926`.
fn crc32(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut t = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    };
    let mut c = 0xFFFF_FFFFu32;
    for &b in bytes {
        c = TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
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

    /// RGB-only [`CameraFrame`] (the legacy top view's shape).
    fn rgb_frame(cam: &str, png: Vec<u8>) -> crate::embodied_env::CameraFrame {
        crate::embodied_env::CameraFrame {
            cam: cam.into(),
            rgb_png: Some(png),
            depth_png: None,
        }
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
        LeRobotV21::write_receipt(
            &root,
            "test/0.1",
            "rigid3d",
            1.0 / 60.0,
            "abc123",
            &[0xbeef],
            1,
            "v1",
            &[],
        )
        .expect("receipt");
        let receipt = std::fs::read_to_string(root.join("meta/oriera_receipt.json")).unwrap();
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
        assert_eq!(
            body, LEGACY_MODALITY_21_5,
            "derived modality must be byte-identical"
        );
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
            depth: false,
        });
        ds.start_episode("pick up the cube");
        ds.push_step(&[0.0], &[0.5]).unwrap();
        ds.push_step(&[0.1], &[0.4]).unwrap();
        ds.push_frame(rgb_frame("top", vec![1, 2, 3])).unwrap();
        ds.finish_episode();
        ds.start_episode("pick up the cube");
        ds.push_step(&[0.2], &[0.3]).unwrap();
        ds.finish_episode();
        let root = std::env::temp_dir().join(format!("oriera-lerobot-v3-{}", std::process::id()));
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
        let ep0 =
            std::fs::read_to_string(root.join("meta/episodes/chunk-000/file-000.jsonl")).unwrap();
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
        let data = std::fs::read_to_string(root.join("data/chunk-000/file-000.jsonl")).unwrap();
        assert_eq!(data.lines().count(), 3);
        LeRobotV21::write_card_v3(&root, "pick up the cube").expect("card v3");
        assert!(root.join("README.md").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ----- M3-A: multi-camera video + depth tracks -----

    /// The single-camera features block must stay byte-identical to the
    /// pre-M3 form (the golden collect datasets hang on it).
    #[test]
    fn single_camera_features_stay_byte_stable() {
        let mut ds = LeRobotV21::new(60.0, "t", vec!["s".into()], vec!["a".into()]);
        ds.set_camera(CameraTrack {
            name: "top".into(),
            w: 168,
            h: 168,
            fps: 60.0,
            depth: false,
        });
        let info = ds.info_json();
        let want = ",\"observation.images.top\":{\"dtype\":\"video\",\"shape\":[168,168,3],\"names\":null,\"info\":{\"video.fps\":60,\"video.codec\":\"libx264\",\"video.pix_fmt\":\"yuv420p\",\"has_audio\":false}}";
        assert!(info.contains(want), "{info}");
        assert!(!info.contains("depth."), "no depth track declared");
    }

    #[test]
    fn multi_camera_tracks_declare_route_and_write() {
        let mut ds = LeRobotV21::new(60.0, "t", vec!["s".into()], vec!["a".into()]);
        ds.add_camera(CameraTrack {
            name: "top".into(),
            w: 8,
            h: 6,
            fps: 60.0,
            depth: false,
        })
        .unwrap();
        ds.add_camera(CameraTrack {
            name: "wrist".into(),
            w: 4,
            h: 4,
            fps: 60.0,
            depth: true,
        })
        .unwrap();
        assert!(
            ds.add_camera(CameraTrack {
                name: "wrist".into(),
                w: 4,
                h: 4,
                fps: 60.0,
                depth: true,
            })
            .is_err(),
            "duplicate camera names are a contract error"
        );
        ds.start_episode("pick up the cube");
        ds.push_frame(rgb_frame("top", vec![1])).unwrap();
        ds.push_frame(crate::embodied_env::CameraFrame {
            cam: "wrist".into(),
            rgb_png: Some(vec![2]),
            depth_png: Some(vec![3]),
        })
        .unwrap();
        // undeclared camera / undeclared depth are contract errors
        assert!(ds.push_frame(rgb_frame("nope", vec![9])).is_err());
        ds.finish_episode();

        let info = ds.info_json_v3();
        assert!(info.contains("\"observation.images.top\""), "{info}");
        assert!(info.contains("\"observation.images.wrist\""), "{info}");
        assert!(info.contains("\"depth.wrist\""), "{info}");
        assert!(
            !info.contains("\"depth.top\""),
            "depth only where declared: {info}"
        );

        let root = std::env::temp_dir().join(format!("oriera-multicam-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        ds.write_dir_v3(&root).expect("v3 tree");
        for rel in [
            "videos/observation.images.top/chunk-000/file-000/frame_000000.png",
            "videos/observation.images.wrist/chunk-000/file-000/frame_000000.png",
            "videos/depth.wrist/chunk-000/file-000/frame_000000.png",
        ] {
            assert!(root.join(rel).exists(), "missing {rel}");
        }
        assert!(
            !root.join("videos/depth.top").exists(),
            "no depth dir for a depth=false camera"
        );
        // v2.1 tree: per-episode dirs incl. depth
        let root21 = std::env::temp_dir().join(format!("oriera-multicam21-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root21);
        ds.write_dir(&root21).expect("v21 tree");
        for rel in [
            "videos/chunk-000/observation.images.top/episode_000000/frame_000000.png",
            "videos/chunk-000/observation.images.wrist/episode_000000/frame_000000.png",
            "videos/chunk-000/depth.wrist/episode_000000/frame_000000.png",
        ] {
            assert!(root21.join(rel).exists(), "missing {rel}");
        }
        // MANIFEST covers the depth frames too
        let manifest = std::fs::read_to_string(root21.join("MANIFEST.jsonl")).unwrap();
        assert!(
            manifest.contains("videos/chunk-000/depth.wrist/episode_000000/frame_000000.png"),
            "{manifest}"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&root21);
    }

    /// DE-P2 回归：RGB 轨空（帧全 `rgb_png: None`）而深度轨非空时，
    /// 深度目录仍须落盘——旧代码在 `write_dir` 里以 RGB 判空整集
    /// `continue`，会把该相机的深度轨一并丢掉。
    #[test]
    fn depth_tracks_write_independently_of_empty_rgb() {
        let mut ds = LeRobotV21::new(60.0, "t", vec!["s".into()], vec!["a".into()]);
        ds.add_camera(CameraTrack {
            name: "wrist".into(),
            w: 4,
            h: 4,
            fps: 60.0,
            depth: true,
        })
        .unwrap();
        ds.start_episode("pick up the cube");
        ds.push_step(&[0.0], &[0.5]).unwrap();
        // RGB 缺席、深度在场的一帧（push_frame 对 None rgb 合法放行）
        ds.push_frame(crate::embodied_env::CameraFrame {
            cam: "wrist".into(),
            rgb_png: None,
            depth_png: Some(vec![7, 8, 9]),
        })
        .unwrap();
        ds.finish_episode();

        let root = std::env::temp_dir().join(format!("oriera-depth-only-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        ds.write_dir(&root).expect("v21 tree");
        assert!(
            root.join("videos/chunk-000/depth.wrist/episode_000000/frame_000000.png")
                .exists(),
            "depth frames must land even with an empty RGB track"
        );
        assert!(
            !root
                .join("videos/chunk-000/observation.images.wrist")
                .exists(),
            "no RGB dir when the RGB track is empty"
        );
        // 深度帧进 MANIFEST（提交点覆盖深度轨）
        let manifest = std::fs::read_to_string(root.join("MANIFEST.jsonl")).unwrap();
        assert!(
            manifest.contains("videos/chunk-000/depth.wrist/episode_000000/frame_000000.png"),
            "{manifest}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ----- M2: npz bare export (U-8) -----

    #[test]
    fn crc32_matches_the_ieee_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    /// Test-side reader (an independent walk of the format): EOCD →
    /// central directory → local headers; asserts the two headers agree,
    /// the member is STORED and its CRC matches. Returns (name, bytes).
    fn unzip_stored(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
        let u16_at = |p: usize| u16::from_le_bytes([zip[p], zip[p + 1]]) as usize;
        let u32_at = |p: usize| u32::from_le_bytes(zip[p..p + 4].try_into().unwrap());
        assert_eq!(
            &zip[..4],
            b"PK\x03\x04",
            "numpy detects an npz by this prefix"
        );
        let eocd = zip.len() - 22;
        assert_eq!(u32_at(eocd), 0x0605_4b50, "EOCD signature");
        let (count, cd_size, cd_off) = (
            u16_at(eocd + 10),
            u32_at(eocd + 12) as usize,
            u32_at(eocd + 16) as usize,
        );
        assert_eq!(cd_off + cd_size, eocd, "central directory ends at the EOCD");
        let (mut p, mut out) = (cd_off, Vec::new());
        for _ in 0..count {
            assert_eq!(u32_at(p), 0x0201_4b50, "central header signature");
            let (crc, size) = (u32_at(p + 16), u32_at(p + 20) as usize);
            assert_eq!(
                size,
                u32_at(p + 24) as usize,
                "stored: compressed == uncompressed"
            );
            let name_len = u16_at(p + 28);
            let local = u32_at(p + 42) as usize;
            let name = String::from_utf8(zip[p + 46..p + 46 + name_len].to_vec()).unwrap();
            assert_eq!(u32_at(local), 0x0403_4b50, "local header signature");
            assert_eq!(u16_at(local + 8), 0, "method 0 = stored");
            assert_eq!(u32_at(local + 14), crc, "local CRC == central CRC");
            assert_eq!(&zip[local + 30..local + 30 + name_len], name.as_bytes());
            let start = local + 30 + name_len;
            let data = zip[start..start + size].to_vec();
            assert_eq!(crc32(&data), crc, "CRC of {name}");
            out.push((name, data));
            p += 46 + name_len;
        }
        out
    }

    /// Decode a float32 NPY v1.0 payload → (header dict text, values).
    fn npy_decode_f32(npy: &[u8]) -> (String, Vec<f32>) {
        assert_eq!(&npy[..8], b"\x93NUMPY\x01\x00", "magic + version 1.0");
        let hl = u16::from_le_bytes([npy[8], npy[9]]) as usize;
        assert_eq!((10 + hl) % 64, 0, "header is 64-byte aligned");
        assert_eq!(npy[10 + hl - 1], b'\n', "header ends with a newline");
        let header = String::from_utf8(npy[10..10 + hl].to_vec()).unwrap();
        let body = &npy[10 + hl..];
        assert_eq!(body.len() % 4, 0);
        let vals = body
            .chunks(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        (header, vals)
    }

    #[test]
    fn write_npz_round_trips_rows_bit_exact_and_deterministically() {
        let mut ds = sample(); // ep 0: 2 rows, state 2-D, action 1-D
        ds.start_episode("place the cube");
        // bit-pattern edge cases a text round-trip could blur
        ds.push_step(&[f32::MIN_POSITIVE, -0.0], &[1.0e-30])
            .unwrap();
        ds.finish_episode();
        let root = std::env::temp_dir().join(format!("oriera-npz-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(ds.write_npz(&root).expect("write_npz"), 2);
        let mut from = 0usize;
        for (i, ep) in ds.episodes.iter().enumerate() {
            let zip = std::fs::read(root.join(format!("episode_{i:06}.npz"))).unwrap();
            let members = unzip_stored(&zip);
            let names: Vec<String> = members.iter().map(|(n, _)| n.clone()).collect();
            assert_eq!(
                names,
                [
                    format!("obs_{i}.npy"),
                    format!("act_{i}.npy"),
                    "meta.json".into()
                ]
            );
            let (oh, obs) = npy_decode_f32(&members[0].1);
            let (ah, act) = npy_decode_f32(&members[1].1);
            let n = ep.rows.len();
            assert!(
                oh.starts_with("{'descr': '<f4', 'fortran_order': False, 'shape': ("),
                "{oh}"
            );
            assert!(oh.contains(&format!("'shape': ({n}, 2), }}")), "{oh}");
            assert!(ah.contains(&format!("'shape': ({n}, 1), }}")), "{ah}");
            let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
            let want_obs: Vec<f32> = ep.rows.iter().flat_map(|r| r.state.clone()).collect();
            let want_act: Vec<f32> = ep.rows.iter().flat_map(|r| r.action.clone()).collect();
            assert_eq!(bits(&obs), bits(&want_obs), "obs bit-exact (ep {i})");
            assert_eq!(bits(&act), bits(&want_act), "act bit-exact (ep {i})");
            let meta: serde_json::Value = serde_json::from_slice(&members[2].1).unwrap();
            assert_eq!(meta["format"], "oriera-npz/1");
            assert_eq!(meta["episode_index"], i);
            assert_eq!(meta["obs_key"], format!("obs_{i}"));
            assert_eq!(meta["act_key"], format!("act_{i}"));
            assert_eq!(meta["length"], n);
            assert_eq!(meta["obs_shape"], serde_json::json!([n, 2]));
            assert_eq!(meta["dataset_from_index"], from);
            assert_eq!(meta["dataset_to_index"], from + n);
            assert_eq!(meta["task"], ds.tasks[ep.task_index as usize].as_str());
            assert_eq!(meta["obs_names"], serde_json::json!(["j0", "j1"]));
            assert!(meta["modality"]["state"].is_object(), "{meta}");
            from += n;
        }
        // MANIFEST is the commit point; no tmp left behind.
        let manifest = std::fs::read_to_string(root.join("MANIFEST.jsonl")).unwrap();
        assert_eq!(manifest.lines().count(), 2, "{manifest}");
        assert!(!root.join("MANIFEST.jsonl.tmp").exists());
        // Deterministic: a second export is byte-identical.
        let first = std::fs::read(root.join("episode_000001.npz")).unwrap();
        ds.write_npz(&root).expect("re-export");
        assert_eq!(
            std::fs::read(root.join("episode_000001.npz")).unwrap(),
            first
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_npz_rejects_a_ragged_row_instead_of_writing_garbage() {
        let mut ds = sample();
        // bypass push_step's contract check through the pub fields
        ds.episodes.push(Episode {
            task_index: 0,
            rows: vec![Row {
                state: vec![1.0],
                action: vec![0.0],
            }],
            tracks: Vec::new(),
        });
        let root = std::env::temp_dir().join(format!("oriera-npz-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let err = ds.write_npz(&root).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("第 1 集第 0 行 state 维度 1 ≠ 2"),
            "{err}"
        );
        assert!(!root.exists(), "contract errors write nothing");
    }
}
