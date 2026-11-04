//! URDF import (feature `urdf`) — the model-interoperability on-ramp
//! (PLAN-embodied-sim M1, design doc: docs/URDF-IMPORT.md).
//!
//! Scope (U0 skeleton): `<robot>` → `<link>`/`<joint>` tree parsing,
//! **linear revolute chains** mapped onto the OriEra
//! [`ArmDescription`]: joint origins give link lengths, `<limit>` gives
//! per-stage revolute limits, `<dynamics damping>` maps to passive
//! joint damping. Primitives (box/cylinder/sphere) are parsed for
//! geometry bookkeeping; **meshes are recorded and skipped** (M1/U2),
//! as are mimic/floating/planar joints — every skip is reported in
//! [`UrdfReport::skipped`], never silent.
//!
//! URDF conventions honored: SI units (metres, radians), `rpy`
//! extrinsic XYZ, joint `origin` = parent-frame pose of the child
//! frame, revolute axis in the joint/child frame.
//!
//! Meshes: URIs recorded since U2; U3 lets them 上岗 via
//! [`crate::meshres`]（URI 解析 + STL 全量/GLB 探针），联检入口
//! [`UrdfReport::validate_chain_with_meshes`]。
//!
//! **U4（PLAN-embodied-general M0a 工作包 D）**：URDF → [`RobotSpec`]
//! 全链导入——不再把 N 轴投影到 4 轴 `ArmDescription`，而是从根 link
//! 走串联链生成 N 个 [`JointSpec`]（fixed 透明吸收、rpy 把 `<axis>`
//! 转到父系、`<limit>` 四元全接、`<dynamics damping>` 进被动阻尼）、
//! 每个链体一个 [`LinkSpec`]（primitive 直映 / mesh **诚实降级**为链段
//! 包络、`<inertial mass>` 反推密度使质量等价）、两指 prismatic（或
//! `<mimic>` 对）→ [`GripperSpec`] 运动学双板近似。入口
//! [`UrdfReport::to_robot_spec`] / [`UrdfReport::to_robot_spec_with_gripper`]；
//! 既有 `first_revolute_chain`/`validate_chain` 一字不动。
//!
//! **M3（PLAN-embodied-general §四 M3「移动底盘（prismatic）」）**：串联链
//! 上的 `<joint type="prismatic">` 不再断链报错，而是真导入为
//! [`JointKind::Prismatic`] 的 [`JointSpec`]（限位/effort/velocity 按 URDF
//! 原单位 m / N / m/s 直接接；PD 增益取 `opts.default_kp_prismatic` /
//! `default_kd_prismatic`（N/m、N·s/m，与转动关节的 N·m/rad 量纲分开），
//! `per_joint` 按名覆盖照旧）。两指夹爪的 prismatic 手指对仍由 GripperSpec
//! 检测消费、不入链；`<transmission>` 等非动力学元素继续忽略。
//!
//! **P1-B1（PLAN-embodied-v3 §二 #12，PUGC 第一公里）**：CLI 侧全链入口
//! `embodied_sim_collect --urdf-robot`（见 examples/embodied_sim_collect.rs
//! 与 docs/URDF-IMPORT.md「全链 CLI」节）；本模块只补了
//! [`pitfall_warnings`]——导入结果的人话坑位警告（gap_closed≈0 /
//! home=URDF 零位），解析与映射零改动。

use oriera_subsys::{
    ArmDescription, GripperKind, GripperSpec, Inertial, JointKind, JointSpec, LinkShape, LinkSpec,
    RobotSpec,
};
use roxmltree::Document;
use std::collections::BTreeMap;
use std::f32::consts::PI;
use std::path::{Path, PathBuf};

/// One parsed link (kinematic frame + primitive geometry, if any).
#[derive(Debug, Clone, Default)]
pub struct UrdfLink {
    pub name: String,
    /// Collision-geometry primitive, in metres: (kind, dims).
    pub geometry: Option<UrdfGeom>,
    /// `<inertial>` block (U2: parsed for bookkeeping).
    pub inertial: Option<UrdfInertial>,
    /// `<collision><origin xyz rpy>`（U4）：碰撞体在 link 系内的位姿，
    /// 缺省零（URDF 语义）。`to_robot_spec` 用它定 `collider_offset`
    /// 与 primitive 的世界朝向。
    pub collision_origin_xyz: [f32; 3],
    pub collision_origin_rpy: [f32; 3],
}

/// Parsed geometry primitives (meshes are reported and skipped).
#[derive(Debug, Clone)]
pub enum UrdfGeom {
    Box([f32; 3]),
    Cylinder {
        radius: f32,
        length: f32,
    },
    Sphere {
        radius: f32,
    },
    /// `<mesh filename="package://...">` — URI 登记于 U2；U3 起可经
    /// [`crate::meshres::load_urdf_mesh`] 按扩展名加载（STL 全量 /
    /// GLB 探针）。
    Mesh {
        uri: String,
    },
}

/// One parsed joint.
#[derive(Debug, Clone, Default)]
pub struct UrdfJoint {
    pub name: String,
    pub kind: String,
    pub parent: String,
    pub child: String,
    /// Child frame pose in the parent frame: `[x, y, z]`, rpy `[r, p, y]`.
    pub origin_xyz: [f32; 3],
    pub origin_rpy: [f32; 3],
    pub axis: [f32; 3],
    pub limit_lower: Option<f32>,
    pub limit_upper: Option<f32>,
    pub damping: Option<f32>,
    /// `<limit effort>` — maps to `MOTOR_MAX_FORCE` reference (U2).
    pub effort: Option<f32>,
    /// `<limit velocity>`（U4）→ [`JointSpec::velocity_limit`]（rad/s，
    /// prismatic 手指为 m/s）。此前被丢弃——工作包 D 补齐。
    pub velocity: Option<f32>,
    /// `<mimic joint multiplier offset>`（U4）：夹爪两指耦合。解析进字段
    /// 的同时仍按既有行为登记一条 `skipped`（兼容既有输出面）。
    pub mimic: Option<UrdfMimic>,
}

/// `<mimic>` 耦合：`q_this = multiplier · q_joint + offset`。
#[derive(Debug, Clone, PartialEq)]
pub struct UrdfMimic {
    pub joint: String,
    /// URDF 缺省 1。
    pub multiplier: f32,
    /// URDF 缺省 0。
    pub offset: f32,
}

impl Default for UrdfMimic {
    fn default() -> Self {
        Self {
            joint: String::new(),
            multiplier: 1.0,
            offset: 0.0,
        }
    }
}

/// `<inertial>` block (U2: parsed for bookkeeping; physics still uses
/// geometric density derivation — the gap is recorded in the receipt).
#[derive(Debug, Clone, Default)]
pub struct UrdfInertial {
    pub mass: f32,
    pub ixx: f32,
    pub iyy: f32,
    pub izz: f32,
    /// 惯量张量非对角项（MJCF 交叉验证 2026-11-01 指出的表达力缺口：
    /// menagerie Panda 连杆带 0.019 kg·m² 级非对角，此前被静默丢弃）。
    pub ixy: f32,
    pub ixz: f32,
    pub iyz: f32,
    pub origin_xyz: [f32; 3],
}

/// Parse outcome: the tree plus an honest skip list.
#[derive(Debug, Default)]
pub struct UrdfReport {
    /// `<robot name>`（U4；缺省空串）→ [`RobotSpec::name`]。
    pub name: String,
    pub links: Vec<UrdfLink>,
    pub joints: Vec<UrdfJoint>,
    /// Root link (the link no joint references as child).
    pub root: String,
    pub skipped: Vec<String>,
}

impl UrdfReport {
    /// Parse a URDF document from text.
    pub fn parse(xml: &str) -> Result<Self, String> {
        let doc = Document::parse(xml).map_err(|e| format!("URDF XML 非法：{e}"))?;
        let root = doc.root_element();
        let robot = if root.has_tag_name("robot") {
            root
        } else {
            root.children()
                .find(|n| n.has_tag_name("robot"))
                .ok_or("URDF 缺 <robot> 根元素")?
        };
        let mut rep = UrdfReport {
            name: robot.attribute("name").unwrap_or_default().to_string(),
            ..UrdfReport::default()
        };
        for n in robot.children() {
            if !n.is_element() {
                continue;
            }
            match n.tag_name().name() {
                "link" => rep.parse_link(n),
                "joint" => rep.parse_joint(n),
                "material" | "transmission" | "gazebo" => {}
                other => rep.skipped.push(format!("<{other}> 元素未消费（U2 待办）")),
            }
        }
        rep.root = rep.find_root();
        Ok(rep)
    }

    fn parse_link(&mut self, n: roxmltree::Node) {
        let mut link = UrdfLink {
            name: n.attribute("name").unwrap_or_default().to_string(),
            geometry: None,
            inertial: None,
            collision_origin_xyz: [0.0; 3],
            collision_origin_rpy: [0.0; 3],
        };
        if let Some(col) = n.children().find(|c| c.has_tag_name("collision")) {
            link.geometry = Self::parse_geometry(col);
            if let Some(o) = col.children().find(|c| c.has_tag_name("origin")) {
                link.collision_origin_xyz = Self::vec3(o.attribute("xyz"));
                link.collision_origin_rpy = Self::vec3(o.attribute("rpy"));
            }
        }
        if let Some(iner) = n.children().find(|c| c.has_tag_name("inertial")) {
            link.inertial = Self::parse_inertial(iner);
        }
        self.links.push(link);
    }

    fn parse_inertial(iner: roxmltree::Node) -> Option<UrdfInertial> {
        let mut iner_data = UrdfInertial::default();
        if let Some(m) = iner.children().find(|c| c.has_tag_name("mass")) {
            iner_data.mass = m.attribute("value")?.parse().ok()?;
        }
        if let Some(i) = iner.children().find(|c| c.has_tag_name("inertia")) {
            let attr = |name: &str| {
                i.attribute(name)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.0)
            };
            iner_data.ixx = attr("ixx");
            iner_data.iyy = attr("iyy");
            iner_data.izz = attr("izz");
            // 非对角项（MJCF 交叉验证缺口闭环）：URDF 惯例可省略（对称
            // 张量在关节轴对齐时合法），缺省 0 与语义一致。
            iner_data.ixy = attr("ixy");
            iner_data.ixz = attr("ixz");
            iner_data.iyz = attr("iyz");
        }
        if let Some(o) = iner.children().find(|c| c.has_tag_name("origin")) {
            iner_data.origin_xyz = Self::vec3(o.attribute("xyz"));
        }
        Some(iner_data)
    }

    fn parse_geometry(col: roxmltree::Node) -> Option<UrdfGeom> {
        let geo = col.children().find(|c| c.has_tag_name("geometry"))?;
        if let Some(b) = geo.children().find(|c| c.has_tag_name("box")) {
            let size: Vec<f32> = b
                .attribute("size")?
                .split(' ')
                .take(3)
                .filter_map(|v| v.parse().ok())
                .collect();
            if size.len() == 3 {
                return Some(UrdfGeom::Box([size[0], size[1], size[2]]));
            }
        }
        if let Some(c) = geo.children().find(|c| c.has_tag_name("cylinder")) {
            return Some(UrdfGeom::Cylinder {
                radius: c.attribute("radius")?.parse().ok()?,
                length: c.attribute("length")?.parse().ok()?,
            });
        }
        if let Some(s) = geo.children().find(|c| c.has_tag_name("sphere")) {
            return Some(UrdfGeom::Sphere {
                radius: s.attribute("radius")?.parse().ok()?,
            });
        }
        if let Some(m) = geo.children().find(|c| c.has_tag_name("mesh")) {
            // U2: mesh URI 记录（L4 资产管线上岗时按 URI 加载 GLB/STL）
            let uri = m.attribute("filename").unwrap_or_default().to_string();
            if !uri.is_empty() {
                return Some(UrdfGeom::Mesh { uri });
            }
        }
        None
    }

    fn parse_joint(&mut self, n: roxmltree::Node) {
        let mut j = UrdfJoint {
            name: n.attribute("name").unwrap_or_default().to_string(),
            kind: n.attribute("type").unwrap_or_default().to_string(),
            ..Default::default()
        };
        for c in n.children() {
            if !c.is_element() {
                continue;
            }
            match c.tag_name().name() {
                "parent" => j.parent = c.attribute("link").unwrap_or_default().to_string(),
                "child" => j.child = c.attribute("link").unwrap_or_default().to_string(),
                "origin" => {
                    j.origin_xyz = Self::vec3(c.attribute("xyz"));
                    j.origin_rpy = Self::vec3(c.attribute("rpy"));
                }
                "axis" => j.axis = Self::vec3(c.attribute("xyz")),
                "limit" => {
                    j.limit_lower = c.attribute("lower").and_then(|v| v.parse().ok());
                    j.limit_upper = c.attribute("upper").and_then(|v| v.parse().ok());
                    j.effort = c.attribute("effort").and_then(|v| v.parse().ok());
                    j.velocity = c.attribute("velocity").and_then(|v| v.parse().ok());
                }
                "dynamics" => j.damping = c.attribute("damping").and_then(|v| v.parse().ok()),
                "mimic" => {
                    // U4：解析进字段（夹爪两指耦合）；既有 skipped 行保留
                    // ——下游（chain dump / crosscheck）读的是这条输出面。
                    j.mimic = Some(UrdfMimic {
                        joint: c.attribute("joint").unwrap_or_default().to_string(),
                        multiplier: c
                            .attribute("multiplier")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1.0),
                        offset: c
                            .attribute("offset")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0.0),
                    });
                    self.skipped
                        .push(format!("<mimic> 于关节 {}（U2 待办）", j.name));
                }
                _ => {}
            }
        }
        if !matches!(
            j.kind.as_str(),
            "revolute" | "continuous" | "fixed" | "prismatic"
        ) {
            self.skipped.push(format!(
                "关节 {} 类型 {}（floating/planar U2 待办）",
                j.name, j.kind
            ));
        }
        self.joints.push(j);
    }

    fn vec3(attr: Option<&str>) -> [f32; 3] {
        let mut out = [0.0f32; 3];
        if let Some(v) = attr {
            for (i, part) in v.split(' ').take(3).enumerate() {
                out[i] = part.parse().unwrap_or(0.0);
            }
        }
        out
    }

    fn find_root(&self) -> String {
        let children: Vec<&str> = self.joints.iter().map(|j| j.child.as_str()).collect();
        self.links
            .iter()
            .map(|l| l.name.as_str())
            .find(|n| !children.contains(n))
            .unwrap_or_default()
            .to_string()
    }

    /// U2 链校验报告：断链/环/类型错误/轴偏移——人话列表。
    pub fn validate_chain(&self, want: usize) -> Vec<String> {
        let mut issues = Vec::new();
        // 根识别
        if self.root.is_empty() {
            issues.push("无法识别根 link（可能存在环或孤立 link）".into());
            return issues;
        }
        // 链走查
        let mut visited = std::collections::BTreeSet::new();
        let mut link = self.root.clone();
        let mut actuated = 0;
        loop {
            if !visited.insert(link.clone()) {
                issues.push(format!("检测到环：link {link} 被访问两次"));
                break;
            }
            let Some(j) = self.joints.iter().find(|j| j.parent == link) else {
                break;
            };
            if j.kind == "revolute" || j.kind == "continuous" || j.kind == "prismatic" {
                actuated += 1;
                // 轴校验（拒绝式——sim-engine review：非常规轴静默错映）
                let a = j.axis;
                let mag = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
                if mag < 0.9 {
                    issues.push(format!(
                        "关节 {} 轴向量模长 {:.2} ≈ 0（归一化你的 URDF）",
                        j.name, mag
                    ));
                } else if a[2].abs() / mag < 0.99 {
                    // OriEra 链约定关节轴沿 ±Z（关节系 Z 主轴）；非常规轴需 U2 重映射
                    issues.push(format!(
                        "关节 {} 轴 ({:.3},{:.3},{:.3}) 非 ±Z 主轴（OriEra 映射假设 Z 轴，U2 支持非常规轴重定向）",
                        j.name, a[0], a[1], a[2]
                    ));
                }
            } else if j.kind == "floating" {
                issues.push(format!(
                    "关节 {} 类型 {}（OriEra 支持 revolute/continuous/prismatic/fixed——floating/planar 不在范围）",
                    j.name, j.kind
                ));
            }
            link = j.child.clone();
            if actuated >= want {
                break;
            }
        }
        if actuated == 0 {
            issues.push("根 link 下未找到任何 revolute/continuous 关节".into());
        }
        issues
    }

    /// U3 mesh 联检：[`Self::validate_chain`] 之上，把每条 `<mesh>`
    /// 资产的解析结果（三角形数/包围盒尺寸/primitives 数）并进报告。
    /// 资产缺位同样是链的"断点"（可视化裸奔、惯性靠猜）——按人话入列，
    /// 绝不 panic。`base_dir`/`packages` 语义见
    /// [`crate::meshres::resolve_mesh_uri`]。
    ///
    /// 注意：追加的 mesh 行刻意避开「环/断链」字样——上层对链校验输出
    /// 按关键词分诊（见 tests）。
    pub fn validate_chain_with_meshes(
        &self,
        want: usize,
        base_dir: &Path,
        packages: &BTreeMap<String, PathBuf>,
    ) -> Vec<String> {
        let mut out = self.validate_chain(want);
        for l in &self.links {
            let Some(UrdfGeom::Mesh { uri }) = &l.geometry else {
                continue;
            };
            match crate::meshres::load_urdf_mesh(uri, base_dir, packages) {
                Ok(crate::meshres::UrdfMesh::Stl { mesh, source_bytes }) => {
                    let [mn, mx] = mesh.bounds;
                    out.push(format!(
                        "mesh {}：{} → {} 三角形（{} 顶点）· 包围盒 {:.3}×{:.3}×{:.3} m · 源 {} KiB",
                        l.name,
                        uri,
                        mesh.indices.len(),
                        mesh.vertices.len(),
                        mx[0] - mn[0],
                        mx[1] - mn[1],
                        mx[2] - mn[2],
                        source_bytes.div_ceil(1024)
                    ));
                }
                Ok(crate::meshres::UrdfMesh::Probed { info, source_bytes }) => {
                    out.push(format!(
                        "mesh {}：{} → GLB 探针 {} meshes / {} primitives（全量解码走 L4）· 源 {} KiB",
                        l.name,
                        uri,
                        info.mesh_count,
                        info.primitive_count,
                        source_bytes.div_ceil(1024)
                    ));
                }
                Err(e) => out.push(format!("mesh {}：{} → 不可用：{e}", l.name, uri)),
            }
        }
        out
    }

    /// Extract the FIRST linear chain of actuated revolute/continuous
    /// joints from the root (up to `want`; shorter chains are padded
    /// with defaults — an empty chain errors), and map it onto
    /// [`ArmDescription`] — the bridge into `TableTopEnv`/`SceneSpec`.
    ///
    /// Mapping: consecutive joint-origin displacements give link
    /// lengths (‖xyz‖ of each child origin), limits map 1:1, damping
    /// maps to the arm's passive joint damping. Non-revolute branches
    /// terminate the chain (fixed joints are absorbed transparently).
    pub fn first_revolute_chain(
        &self,
        want: usize,
    ) -> Result<(ArmDescription, Vec<String>), String> {
        // 根向下走：parent link → 其关节（线性链假设：每 parent 取
        // 第一个 actuated 关节；分支机器人 U2）。
        let by_parent: std::collections::HashMap<&str, &UrdfJoint> =
            self.joints.iter().map(|j| (j.parent.as_str(), j)).collect();
        let mut chain: Vec<&UrdfJoint> = Vec::new();
        let mut link = self.root.clone();
        let mut skipped_names = Vec::new();
        while chain.len() < want {
            let Some(j) = by_parent.get(link.as_str()) else {
                break;
            };
            match j.kind.as_str() {
                "revolute" | "continuous" => chain.push(j),
                "fixed" => {}
                other => {
                    skipped_names.push(format!("{} ({})", j.name, other));
                    break;
                }
            }
            link = j.child.clone();
        }
        if chain.is_empty() {
            return Err(format!(
                "URDF 链提取失败：根 {root} 下无可用的 revolute 关节（skipped: {skipped_names:?}）",
                root = self.root
            ));
        }
        // 不足 want 时按实得轴数返回（短链沿用默认限位/杆长补齐）——真实
        // 描述文件轴数各异，宁可部分映射也不整份拒绝。
        let mut desc = ArmDescription::default();
        for (i, j) in chain.iter().enumerate() {
            let len = (j.origin_xyz[0] * j.origin_xyz[0]
                + j.origin_xyz[1] * j.origin_xyz[1]
                + j.origin_xyz[2] * j.origin_xyz[2])
                .sqrt();
            if i < 3 {
                desc.links[i] = len.max(0.01);
            }
            if let (Some(lo), Some(hi)) = (j.limit_lower, j.limit_upper) {
                if i < 4 {
                    // continuous 关节没有 limit：保持默认 ±π
                    desc.limits[i.min(3)] = (lo.min(hi), lo.max(hi));
                }
            }
            if let Some(d) = j.damping {
                // URDF <dynamics damping> 是被动关节摩擦（齿轮箱黏性），
                // 不是 PD 控制器的 kd——sim-engine review 语义修正。
                desc.passive_damping = Some(d.abs());
            }
        }
        Ok((desc, skipped_names))
    }
}

// ─────────────────────────────────────────────────────────────────────
// U4：URDF → RobotSpec 全链（PLAN-embodied-general M0a 工作包 D）
//
// 坐标约定：RobotSpec 的每个 link 体系在 home 位姿**与世界对齐**
// （robot.rs：`origin[child] == origin[parent] + anchor`，anchor/axis
// 均在"父体局部 = 世界"系）。于是导入要做的事就是：在 URDF 世界里做
// 一次 home 位姿 FK（T_child = T_parent · Trans(xyz) · Rz(y)Ry(p)Rx(r)），
// 再把每个关节的位置差、旋转后的轴向量、碰撞体位姿统统表达成世界向量
// ——与 `From<&ArmDescription>` 里逐段累加 origin 的写法同一思路，只是
// 从 URDF 树走出来而非写死。
// ─────────────────────────────────────────────────────────────────────

type Mat3 = [[f32; 3]; 3];
type V3 = [f32; 3];

const MAT_IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
/// Rx(−90°)：URDF Z-up → OriEra Y-up，`(x, y, z) ↦ (x, z, −y)`。
const MAT_Z_UP_TO_Y_UP: Mat3 = [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]];

/// URDF 文件的"上"轴。URDF/REP-103 约定 Z-up；OriEra 3D 世界 Y-up
/// （rigid3d 重力沿 −Y）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpAxis {
    /// URDF 按 REP-103 Z-up 书写（真机描述文件的常态）→ 整体施加
    /// Rx(−90°)：URDF +Z 变 OriEra +Y，URDF +Y 变 OriEra −Z。
    #[default]
    Z,
    /// URDF 已按 Y-up 书写（如本 crate 内嵌 4 轴 fixture）→ 不旋转。
    Y,
}

/// [`UrdfReport::to_robot_spec`] 的输入旋钮 + 输出清单。
///
/// URDF 1.0 没有 PD 增益 / armature 语义，这几项只能来自调用方（转动关节
/// 缺省即 `ArmDescription` 预设同款 60 / 4 / 0.02）；`per_joint` 按关节名
/// 覆盖 `(kp, kd)`。**M3**：prismatic 链关节的增益量纲是 N/m、N·s/m（与
/// 转动关节的 N·m/rad 不是一回事——不做隐式换算），缺省取独立字段
/// `default_kp_prismatic` / `default_kd_prismatic`。
#[derive(Debug, Clone, PartialEq)]
pub struct RobotSpecOpts {
    /// ForceBased PD 刚度 / 阻尼缺省（N·m/rad, N·m·s/rad）——转动关节。
    pub default_kp: f32,
    pub default_kd: f32,
    /// prismatic 链关节的 ForceBased PD 缺省（N/m, N·s/m）。缺省
    /// 2000 / 200：10 kg 级滑台 ω≈14 rad/s、ζ≈0.7（载荷差异大时用
    /// `per_joint` 按名覆盖）。
    pub default_kp_prismatic: f32,
    pub default_kd_prismatic: f32,
    /// 转子反射惯量缺省（kg·m²；prismatic 关节按 kg 解读同一数值）。
    pub default_armature: f32,
    /// 按关节名覆盖 `(kp, kd)`。
    pub per_joint: BTreeMap<String, (f32, f32)>,
    /// URDF 根 link 系原点落在 OriEra 世界的位置（→ [`RobotSpec::base`]，
    /// 全部 link `origin` 由此累加）。
    pub mount: [f32; 3],
    pub up_axis: UpAxis,
    /// 引擎 tick 频率（Hz）：只用来把手指 `<limit velocity>`（m/s）换成
    /// [`GripperSpec::gap_rate`]（m/tick）。
    pub tick_hz: f32,
    /// **输出**：每一次降级 / 缺省 / 未消费的人话记录（mesh 包络、
    /// 分支关节、缺 `<collision>`、轴缺省……），只追加不清空。
    pub skipped: Vec<String>,
    /// STL mesh 解析根目录（M2 U-3）：`Some` 时 `<collision><mesh>` 的
    /// STL 经 `meshres` 全量解析，顶点旋到 OriEra 体轴后填进
    /// [`LinkShape::ConvexHull`]（凸包——链体碰撞策略，接触稳定）；
    /// `None` / GLB / 解析失败 → 沿用链段包络降级并记 `skipped`。
    pub mesh_base_dir: Option<PathBuf>,
    /// `package://<pkg>/...` 的包根表（同 `meshres::resolve_mesh_uri`）。
    pub mesh_packages: BTreeMap<String, PathBuf>,
}

impl Default for RobotSpecOpts {
    fn default() -> Self {
        Self {
            default_kp: 60.0,
            default_kd: 4.0,
            default_kp_prismatic: 2000.0,
            default_kd_prismatic: 200.0,
            default_armature: 0.02,
            per_joint: BTreeMap::new(),
            mount: [0.0; 3],
            up_axis: UpAxis::Z,
            tick_hz: 60.0,
            skipped: Vec::new(),
            mesh_base_dir: None,
            mesh_packages: BTreeMap::new(),
        }
    }
}

fn mat_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let cell = |i: usize, j: usize| a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
    [
        [cell(0, 0), cell(0, 1), cell(0, 2)],
        [cell(1, 0), cell(1, 1), cell(1, 2)],
        [cell(2, 0), cell(2, 1), cell(2, 2)],
    ]
}

fn mat_vec(m: &Mat3, v: V3) -> V3 {
    let row = |i: usize| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2];
    [row(0), row(1), row(2)]
}

/// URDF `rpy`（extrinsic XYZ）：R = Rz(yaw)·Ry(pitch)·Rx(roll)——与
/// `scripts/mjcf_crosscheck.py` 的 FK 约定同源。
fn rpy_to_mat(rpy: V3) -> Mat3 {
    let (sr, cr) = rpy[0].sin_cos();
    let (sp, cp) = rpy[1].sin_cos();
    let (sy, cy) = rpy[2].sin_cos();
    let rx = [[1.0, 0.0, 0.0], [0.0, cr, -sr], [0.0, sr, cr]];
    let ry = [[cp, 0.0, sp], [0.0, 1.0, 0.0], [-sp, 0.0, cp]];
    let rz = [[cy, -sy, 0.0], [sy, cy, 0.0], [0.0, 0.0, 1.0]];
    mat_mul(&rz, &mat_mul(&ry, &rx))
}

fn v_add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn v_sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn v_scale(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn v_dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn v_len(a: V3) -> f32 {
    v_dot(a, a).sqrt()
}

fn v_normalize(a: V3) -> V3 {
    let l = v_len(a);
    if l > 1e-12 {
        v_scale(a, 1.0 / l)
    } else {
        a
    }
}

/// |R|·half：旋转后盒体的轴对齐包围半尺寸（90° 整数倍旋转时精确，
/// 其他角度保守）。
fn aabb_half(r: &Mat3, half: V3) -> V3 {
    let row =
        |i: usize| r[i][0].abs() * half[0] + r[i][1].abs() * half[1] + r[i][2].abs() * half[2];
    [row(0), row(1), row(2)]
}

/// 惯量张量的坐标架旋转（M2 U-1）：URDF 张量在 link 系，OriEra 体轴
/// 是世界对齐的——I_body = R·I_link·Rᵀ（R 把 link 轴旋到体轴）。
/// 返回 `(ixx, iyy, izz, ixy, ixz, iyz)`。
#[allow(clippy::too_many_arguments)]
fn rotate_inertia(
    r: &Mat3,
    ixx: f32,
    iyy: f32,
    izz: f32,
    ixy: f32,
    ixz: f32,
    iyz: f32,
) -> (f32, f32, f32, f32, f32, f32) {
    let m = [[ixx, ixy, ixz], [ixy, iyy, iyz], [ixz, iyz, izz]];
    let ri = |a: usize, b: usize| (0..3).map(|k| r[a][k] * m[k][b]).sum::<f32>(); // (R·I)[a][b]
    let out = |a: usize, b: usize| (0..3).map(|k| ri(a, k) * r[b][k]).sum::<f32>(); // (R·I·Rᵀ)[a][b]
    (
        out(0, 0),
        out(1, 1),
        out(2, 2),
        out(0, 1),
        out(0, 2),
        out(1, 2),
    )
}

/// 轴向 `u`、半长 `half_len`、半径 `r` 的圆柱的轴对齐包围半尺寸。
fn cylinder_aabb_half(u: V3, half_len: f32, r: f32) -> V3 {
    let f = |c: f32| half_len * c.abs() + r * (1.0 - c * c).max(0.0).sqrt();
    [f(u[0]), f(u[1]), f(u[2])]
}

fn box_volume(h: V3) -> f32 {
    8.0 * h[0] * h[1] * h[2]
}

fn capsule_volume(half_len: f32, r: f32) -> f32 {
    PI * r * r * (2.0 * half_len) + 4.0 / 3.0 * PI * r * r * r
}

/// URDF 世界里的一个刚体系（home 位姿）。
#[derive(Debug, Clone, Copy)]
struct Frame {
    r: Mat3,
    p: V3,
}

impl Frame {
    const IDENTITY: Frame = Frame {
        r: MAT_IDENTITY,
        p: [0.0; 3],
    };

    /// `self · Trans(xyz) · R(rpy)`：子系在世界的位姿。
    fn child(&self, xyz: V3, rpy: V3) -> Frame {
        Frame {
            r: mat_mul(&self.r, &rpy_to_mat(rpy)),
            p: v_add(self.p, mat_vec(&self.r, xyz)),
        }
    }

    fn compose(&self, other: &Frame) -> Frame {
        Frame {
            r: mat_mul(&self.r, &other.r),
            p: v_add(self.p, mat_vec(&self.r, other.p)),
        }
    }
}

/// 链上的一个刚体 = 一个 URDF link 加上经 fixed 关节吸收进来的子 link。
#[derive(Debug, Clone, Copy)]
struct ChainBody {
    /// 体的命名 link（`UrdfReport::links` 下标）。
    link: usize,
    /// 体系（= 命名 link 系）在 URDF 世界的 home 位姿。
    frame: Frame,
    /// 最深一个被吸收的 fixed 子 link 的系（无则 = `frame`）——工具法兰，
    /// 外部夹爪的挂点。
    tip: Frame,
}

struct ChainJoint<'a> {
    parent: usize,
    child: usize,
    joint: &'a UrdfJoint,
    child_frame: Frame,
}

/// 检测到的两指 prismatic 手指关节对。
struct Fingers<'a> {
    f1: &'a UrdfJoint,
    f2: &'a UrdfJoint,
}

/// 夹爪挂点：手指父 link 所在的链体，以及该 link 系在（臂的）URDF 世界
/// 中的位姿。
struct GripperSite {
    body: usize,
    body_frame: Frame,
    mount: Frame,
}

/// 链走查结果：链体、链关节（拓扑序）、手指挂点（若有）。
struct ChainWalk<'a> {
    bodies: Vec<ChainBody>,
    joints: Vec<ChainJoint<'a>>,
    site: Option<GripperSite>,
}

struct LinkCtx<'a> {
    r_up: Mat3,
    preset_root: &'a LinkSpec,
    preset_dyn: &'a LinkSpec,
    default_radius: f32,
    mesh_base_dir: Option<PathBuf>,
    mesh_packages: &'a BTreeMap<String, PathBuf>,
}

fn axis_or_default(j: &UrdfJoint, skipped: &mut Vec<String>) -> V3 {
    if v_len(j.axis) > 1e-6 {
        j.axis
    } else {
        skipped.push(format!("关节 {} 缺 <axis> → URDF 缺省 (1,0,0)", j.name));
        [1.0, 0.0, 0.0]
    }
}

fn joint_limits(j: &UrdfJoint, skipped: &mut Vec<String>) -> (f32, f32) {
    if j.kind == "continuous" {
        return (-PI, PI);
    }
    match (j.limit_lower, j.limit_upper) {
        (Some(lo), Some(hi)) if lo.is_finite() && hi.is_finite() => (lo.min(hi), lo.max(hi)),
        // URDF 规范：prismatic 必须带 <limit>；缺了不虚构 ±π 米——给一个
        // 有限的 ±1 m 行程并登记（RobotSpec 要求 prismatic 限位有限）。
        _ if j.kind == "prismatic" => {
            skipped.push(format!(
                "prismatic 关节 {} 缺 <limit lower/upper>（URDF 规范要求）→ ±1 m",
                j.name
            ));
            (-1.0, 1.0)
        }
        _ => {
            skipped.push(format!("关节 {} 缺 <limit lower/upper> → ±π", j.name));
            (-PI, PI)
        }
    }
}

/// mesh / 缺 `<collision>` 的诚实降级：沿链段（本体原点 → 下一关节原点）
/// 造包络。半径由 `<inertial mass>` 在引擎默认密度下反推
/// （r = √(m / (π·ρ·L))），再按体积反推密度使质量**精确等价**；无质量
/// 用预设杆径 + 默认密度。链段 ≈ ±X 用 Capsule（LinkShape 唯一有向
/// primitive，沿局部 +X），否则 Box 轴对齐包络；点状体（链段 < 2 cm，
/// 如 Panda link1——joint2 原点与 joint1 重合）→ 质量等价立方。
fn envelope(seg: V3, mass: Option<f32>, rho: f32, default_radius: f32) -> (LinkShape, V3, f32) {
    let len = v_len(seg);
    if len < 0.02 {
        let r = mass
            .map_or(default_radius, |m| (m / rho).cbrt() * 0.5)
            .clamp(0.015, 0.10);
        let half = [r; 3];
        return (LinkShape::Box { half }, [0.0; 3], box_volume(half));
    }
    let r = mass
        .map_or(default_radius, |m| (m / (PI * rho * len)).sqrt())
        .clamp(0.015, 0.10);
    let u = v_scale(seg, 1.0 / len);
    let off = v_scale(seg, 0.5);
    if u[0].abs() > 0.99 {
        (
            LinkShape::Capsule {
                half_len: len * 0.5,
                radius: r,
            },
            off,
            capsule_volume(len * 0.5, r),
        )
    } else {
        let half = cylinder_aabb_half(u, len * 0.5, r);
        (LinkShape::Box { half }, off, box_volume(half))
    }
}

impl UrdfReport {
    /// URDF → [`RobotSpec`]（U4 全链）：从根 link 走串联链，N 不限。
    ///
    /// - **关节**：fixed 透明吸收（link 前进、不生成关节）；revolute /
    ///   continuous → [`JointSpec`]（continuous 限位 ±π）；`anchor` =
    ///   子系原点相对父体原点的世界向量；`axis` = rpy 旋转到世界后的
    ///   `<axis>`（非 ±单位轴也接受，归一化）；`<limit effort/velocity>`
    ///   → `effort`/`velocity_limit`；`<dynamics damping>` → 被动阻尼
    ///   （缺省引擎默认）；kp/kd/armature 来自 `opts`。**prismatic 链关节
    ///   （M3）→ [`JointKind::Prismatic`]**：限位 m（缺 `<limit>` → ±1 m 并
    ///   登记）、effort N、velocity m/s、增益取 `opts.default_kp_prismatic`
    ///   / `default_kd_prismatic`（N/m、N·s/m）；floating/planar 断链报错。
    /// - **link**：`<collision>` box → Box（旋转后取轴对齐包络）、
    ///   cylinder → Capsule（世界轴 ≈ ±X 时）否则 Box 包络、sphere →
    ///   Box 包络、**mesh → STL 且给了 `opts.mesh_base_dir` 时经
    ///   `meshres` 全量解析为 [`LinkShape::ConvexHull`]（顶点旋到体轴，
    ///   `mesh_uri` 留原文作 provenance）；GLB/缺失/未给根目录 → 链段
    ///   包络降级（URI 进 `opts.skipped`）**；`<inertial>` 全张量直进
    ///   [`LinkSpec::inertial`]（M2 U-1：质量/质心/惯量旋到体轴，构建
    ///   端显式 MassProperties），无 inertial 才按体积反推 `density`
    ///   使质量等价；`added_mass` 有质量即 0；`origin` = home 位姿 FK
    ///   累加。
    /// - **夹爪**：`<mimic>` 关节对或同父两条 prismatic → [`GripperSpec`]
    ///   运动学双板近似（见 [`Self::to_robot_spec_with_gripper`]）。无 →
    ///   `None`。
    /// - **上轴**：缺省把 URDF 的 Z-up 旋到 OriEra Y-up（[`UpAxis`]）。
    ///
    /// 返回的 spec **未**经 `validate()`——gripper-only 的 URDF（无臂关节）
    /// 也能返回 Ok 以便把它的 GripperSpec 移植到别的臂上；要进
    /// `MultibodySystem::from_robot` 的 spec 由 builder 校验。
    pub fn to_robot_spec(&self, opts: &mut RobotSpecOpts) -> Result<RobotSpec, String> {
        self.build(None, opts)
    }

    /// 臂 URDF + **外部**两指夹爪 URDF → 一份 [`RobotSpec`]：夹爪 URDF
    /// 的根系挂到臂链末体的工具法兰（最深一个 fixed 吸收 link，如 Panda
    /// 的 `attachment`；无则末体原点）。臂 URDF 自带的夹爪被覆盖（入
    /// `skipped`）。
    ///
    /// GripperSpec 的推法（**运动学双板近似**——真夹爪力控 M2）：
    /// - `gap_open/closed`：两指板中心沿开合轴（手指 1 的轴）的间距，在
    ///   手指 1 限位两端各算一次，大者开、小者闭；手指 2 的位移按
    ///   `<mimic multiplier/offset>` 耦合（无 mimic 按行程比例同步）。
    /// - `finger_half`：手指 link 的 box 碰撞体半尺寸，按（接近轴, 其余,
    ///   开合轴）排进 builder 的板系 `[X, Y, Z]`；缺 box 用引擎默认。
    /// - `tcp_offset`：两指板中心中点（q=0），表达为挂载体系（世界对齐）
    ///   的向量——几何正确；`finger_x` 取其沿接近轴的距离（builder 把板放
    ///   在挂载 link 局部 +X 上——GripperSpec 无板系朝向字段，见文档）。
    /// - `effort`：手指 1 `<limit effort>`（缺省引擎默认）；`gap_rate`：
    ///   手指 `<limit velocity>` × (1+|multiplier|) / `tick_hz`（缺省引擎默认）；
    ///   `friction` 引擎默认（URDF 无此语义）。
    pub fn to_robot_spec_with_gripper(
        &self,
        gripper: &UrdfReport,
        opts: &mut RobotSpecOpts,
    ) -> Result<RobotSpec, String> {
        self.build(Some(gripper), opts)
    }

    /// 两指夹爪手指关节名 `(finger_1, finger_2)`：`<mimic>` 配对的 prismatic
    /// 对优先，其次同父两条 prismatic（文档序）。无 → `None`。
    pub fn gripper_finger_joints(&self) -> Option<(String, String)> {
        self.detect_fingers()
            .map(|f| (f.f1.name.clone(), f.f2.name.clone()))
    }

    fn detect_fingers(&self) -> Option<Fingers<'_>> {
        let prismatic = |j: &&UrdfJoint| j.kind == "prismatic";
        for j2 in self.joints.iter().filter(prismatic) {
            if let Some(m) = &j2.mimic {
                if let Some(j1) = self
                    .joints
                    .iter()
                    .find(|j| j.name == m.joint && j.kind == "prismatic")
                {
                    return Some(Fingers { f1: j1, f2: j2 });
                }
            }
        }
        for (i, j1) in self.joints.iter().enumerate() {
            if j1.kind != "prismatic" {
                continue;
            }
            if let Some(j2) = self
                .joints
                .iter()
                .skip(i + 1)
                .find(|j| j.kind == "prismatic" && j.parent == j1.parent)
            {
                return Some(Fingers { f1: j1, f2: j2 });
            }
        }
        None
    }

    /// 根向下走串联链：fixed 吸收、revolute/continuous/prismatic（M3）成体、手指关节处
    /// 记挂点并止步（手指不入链）、同父多关节取文档序首个其余入 skipped。
    fn walk_chain<'a>(
        &'a self,
        fingers: Option<&Fingers<'a>>,
        skipped: &mut Vec<String>,
    ) -> Result<ChainWalk<'a>, String> {
        let link_index = |name: &str| self.links.iter().position(|l| l.name == name);
        let root_idx = link_index(&self.root)
            .ok_or_else(|| format!("无法识别根 link（`{}`）：可能存在环或无 link", self.root))?;
        let is_finger =
            |j: &UrdfJoint| fingers.is_some_and(|f| j.name == f.f1.name || j.name == f.f2.name);
        let mut bodies = vec![ChainBody {
            link: root_idx,
            frame: Frame::IDENTITY,
            tip: Frame::IDENTITY,
        }];
        let mut joints = Vec::new();
        let mut site = None;
        let mut cur_link = self.root.as_str();
        let mut cur_body = 0usize;
        let mut cur_frame = Frame::IDENTITY;
        let mut visited = std::collections::BTreeSet::new();
        loop {
            if !visited.insert(cur_link.to_string()) {
                return Err(format!("检测到环：link {cur_link} 被访问两次"));
            }
            let cands: Vec<&UrdfJoint> = self
                .joints
                .iter()
                .filter(|j| j.parent == cur_link)
                .collect();
            if site.is_none() && cands.iter().any(|j| is_finger(j)) {
                site = Some(GripperSite {
                    body: cur_body,
                    body_frame: bodies[cur_body].frame,
                    mount: cur_frame,
                });
            }
            let mut rest = cands.iter().filter(|j| !is_finger(j));
            let Some(j) = rest.next() else { break };
            for other in rest {
                skipped.push(format!(
                    "分支关节 {}（父 {cur_link}）未消费——M0a 单串联链，分支树待 M0b",
                    other.name
                ));
            }
            let child_frame = cur_frame.child(j.origin_xyz, j.origin_rpy);
            match j.kind.as_str() {
                "fixed" => bodies[cur_body].tip = child_frame,
                // M3：prismatic 链关节与 revolute 同样成体入链（JointKind 在
                // `build` 里按 `j.kind` 区分）——不再断链报错。
                "revolute" | "continuous" | "prismatic" => {
                    let child_idx = link_index(&j.child).ok_or_else(|| {
                        format!("关节 {} 的 child link `{}` 未定义", j.name, j.child)
                    })?;
                    bodies.push(ChainBody {
                        link: child_idx,
                        frame: child_frame,
                        tip: child_frame,
                    });
                    let c = bodies.len() - 1;
                    joints.push(ChainJoint {
                        parent: cur_body,
                        child: c,
                        joint: j,
                        child_frame,
                    });
                    cur_body = c;
                }
                other => {
                    return Err(format!(
                        "关节 {} 类型 `{other}`：串联链在此断开（floating/planar 不在范围——\
                         OriEra 支持 revolute/continuous/prismatic/fixed）",
                        j.name
                    ))
                }
            }
            cur_link = j.child.as_str();
            cur_frame = child_frame;
        }
        Ok(ChainWalk {
            bodies,
            joints,
            site,
        })
    }

    fn link_spec(
        &self,
        body: &ChainBody,
        next_p: Option<V3>,
        is_root: bool,
        ctx: &LinkCtx<'_>,
        skipped: &mut Vec<String>,
    ) -> LinkSpec {
        let l = &self.links[body.link];
        let mass = l
            .inertial
            .as_ref()
            .map(|i| i.mass)
            .filter(|m| m.is_finite() && *m > 0.0);
        let rho = ctx.preset_dyn.density;
        let r_body = mat_mul(&ctx.r_up, &body.frame.r);
        // 链段（OriEra 世界）：到下一关节原点；末体退用最深吸收 fixed 子系；再无则零。
        let seg_end = next_p
            .or_else(|| (v_len(v_sub(body.tip.p, body.frame.p)) > 1e-6).then_some(body.tip.p));
        let seg = mat_vec(
            &ctx.r_up,
            seg_end.map_or([0.0; 3], |p| v_sub(p, body.frame.p)),
        );
        let col_off = mat_vec(&r_body, l.collision_origin_xyz);
        let (shape, offset, volume, mesh_uri) = match &l.geometry {
            Some(UrdfGeom::Box(size)) => {
                let rc = mat_mul(&r_body, &rpy_to_mat(l.collision_origin_rpy));
                let half = aabb_half(&rc, v_scale(*size, 0.5));
                (LinkShape::Box { half }, col_off, box_volume(half), None)
            }
            Some(UrdfGeom::Cylinder { radius, length }) => {
                let rc = mat_mul(&r_body, &rpy_to_mat(l.collision_origin_rpy));
                // URDF cylinder 沿 link-Z
                let u = mat_vec(&rc, [0.0, 0.0, 1.0]);
                if u[0].abs() > 0.99 {
                    (
                        LinkShape::Capsule {
                            half_len: length * 0.5,
                            radius: *radius,
                        },
                        col_off,
                        capsule_volume(length * 0.5, *radius),
                        None,
                    )
                } else {
                    skipped.push(format!(
                        "link {} cylinder 世界轴 ({:.2},{:.2},{:.2}) 非 ±X → Box 轴对齐包络（LinkShape 无旋转字段，M0b）",
                        l.name, u[0], u[1], u[2]
                    ));
                    let half = cylinder_aabb_half(u, length * 0.5, *radius);
                    (LinkShape::Box { half }, col_off, box_volume(half), None)
                }
            }
            Some(UrdfGeom::Sphere { radius }) => {
                let half = [*radius; 3];
                (LinkShape::Box { half }, col_off, box_volume(half), None)
            }
            Some(UrdfGeom::Mesh { uri }) => {
                match self.resolve_mesh_hull(uri, &r_body, col_off, ctx) {
                    Some(points) => {
                        let (_, _, vol) = envelope(seg, mass, rho, ctx.default_radius);
                        skipped.push(format!(
                        "link {} mesh {uri} → ConvexHull（{} 点，STL 顶点旋到体轴；凸包≈网格的保守近似）",
                        l.name,
                        points.len()
                    ));
                        (
                            LinkShape::ConvexHull { points },
                            col_off,
                            vol,
                            Some(uri.clone()),
                        )
                    }
                    None => {
                        skipped.push(format!(
                        "link {} mesh {uri} → 链段包络降级（GLB→探针仅 M3 全量解码 / 文件缺失或格式不支持 / 未给 opts.mesh_base_dir）",
                        l.name
                    ));
                        let (s, o, v) = envelope(seg, mass, rho, ctx.default_radius);
                        (s, o, v, None)
                    }
                }
            }
            None => {
                skipped.push(format!("link {} 无 <collision> → 链段包络", l.name));
                let (s, o, v) = envelope(seg, mass, rho, ctx.default_radius);
                (s, o, v, None)
            }
        };
        // M2 U-1：`<inertial>` 全张量直进 [`LinkSpec::inertial`]（质量/
        // 质心/惯量在 URDF link 系，旋到 OriEra 体轴），构建端显式设置
        // MassProperties——**有 inertial 就直给，没有才走体积反推**。
        let inertial = l.inertial.as_ref().and_then(|i| {
            let ok = |v: f32| v.is_finite();
            if !(ok(i.mass)
                && i.mass > 0.0
                && ok(i.ixx)
                && ok(i.iyy)
                && ok(i.izz)
                && ok(i.ixy)
                && ok(i.ixz)
                && ok(i.iyz))
            {
                return None;
            }
            let (ixx, iyy, izz, ixy, ixz, iyz) =
                rotate_inertia(&r_body, i.ixx, i.iyy, i.izz, i.ixy, i.ixz, i.iyz);
            Some(Inertial {
                mass: i.mass,
                com: mat_vec(&r_body, i.origin_xyz),
                ixx,
                iyy,
                izz,
                ixy,
                ixz,
                iyz,
            })
        });
        // 密度是 inertial=None 路径的质量载体；inertial 直给时它退为
        // 簿记缺省（构建端忽略），不再反推。
        let density = if inertial.is_some() {
            rho
        } else {
            mass.map(|m| m / volume)
                .filter(|d| d.is_finite() && *d > 0.0)
                .unwrap_or(rho)
        };
        LinkSpec {
            name: l.name.clone(),
            shape,
            origin: [0.0; 3],
            collider_offset: offset,
            density,
            friction: if is_root {
                ctx.preset_root.friction
            } else {
                ctx.preset_dyn.friction
            },
            added_mass: if is_root || mass.is_some() {
                0.0
            } else {
                ctx.preset_dyn.added_mass
            },
            inertial,
            mesh_uri,
        }
    }

    /// STL mesh → [`LinkShape::ConvexHull`] 点集（M2 U-3）。URI 解析 +
    /// `meshres::parse_stl` 全量，顶点从 URDF link 系旋到 OriEra 体轴
    /// （`r_body = r_up ∘ body.frame.r`），再减碰撞体偏移（点集相对
    /// collider origin）。GLB（探针→M3 全量解码）/文件缺失/格式不支持/
    /// 未给 `opts.mesh_base_dir` → `None`（调用方沿用包络降级）。
    fn resolve_mesh_hull(
        &self,
        uri: &str,
        r_body: &Mat3,
        col_off: V3,
        ctx: &LinkCtx<'_>,
    ) -> Option<Vec<[f32; 3]>> {
        let base = ctx.mesh_base_dir.as_ref()?;
        match crate::meshres::load_urdf_mesh(uri, base, ctx.mesh_packages) {
            Ok(crate::meshres::UrdfMesh::Stl { mesh, .. }) => Some(
                mesh.vertices
                    .iter()
                    .map(|v| {
                        let b = mat_vec(r_body, *v);
                        [b[0] - col_off[0], b[1] - col_off[1], b[2] - col_off[2]]
                    })
                    .collect(),
            ),
            Ok(crate::meshres::UrdfMesh::Probed { .. }) => None,
            Err(_) => None,
        }
    }

    /// 两指 prismatic 对 → GripperSpec（运动学双板近似）。`self` 是手指所在
    /// 的 report（外部夹爪时即夹爪 report）。
    fn build_gripper(
        &self,
        f: &Fingers<'_>,
        site: &GripperSite,
        r_up: &Mat3,
        preset: &GripperSpec,
        tick_hz: f32,
        skipped: &mut Vec<String>,
    ) -> GripperSpec {
        let (f1, f2) = (f.f1, f.f2);
        let r1 = rpy_to_mat(f1.origin_rpy);
        let r2 = rpy_to_mat(f2.origin_rpy);
        let a1 = mat_vec(&r1, axis_or_default(f1, skipped));
        let a2 = mat_vec(&r2, axis_or_default(f2, skipped));
        // 开合方向 = 手指 1 的轴（父系）
        let u = v_normalize(a1);
        // 指板中心（q=0，父系）= 关节原点 + 手指 link 碰撞体原点；box 半尺寸（父系包络）
        let pad = |fj: &UrdfJoint, r: &Mat3| -> (V3, Option<V3>) {
            let Some(link) = self.links.iter().find(|l| l.name == fj.child) else {
                return (fj.origin_xyz, None);
            };
            let half = match &link.geometry {
                Some(UrdfGeom::Box(size)) => {
                    let rc = mat_mul(r, &rpy_to_mat(link.collision_origin_rpy));
                    Some(aabb_half(&rc, v_scale(*size, 0.5)))
                }
                _ => None,
            };
            (
                v_add(fj.origin_xyz, mat_vec(r, link.collision_origin_xyz)),
                half,
            )
        };
        let (p1, half1) = pad(f1, &r1);
        let (p2, _) = pad(f2, &r2);
        let lim = |fj: &UrdfJoint, fallback: (f32, f32)| match (fj.limit_lower, fj.limit_upper) {
            (Some(lo), Some(hi)) if lo.is_finite() && hi.is_finite() => (lo.min(hi), lo.max(hi)),
            _ => fallback,
        };
        let (lo1, hi1) = lim(f1, (0.0, 0.0));
        let (lo2, hi2) = lim(f2, (lo1, hi1));
        // 手指 2 的位移：mimic 耦合；无 mimic 按行程比例同步
        let q2_of = |q1: f32| -> f32 {
            if let Some(m) = f2.mimic.as_ref().filter(|m| m.joint == f1.name) {
                m.multiplier * q1 + m.offset
            } else if let Some(m) = f1.mimic.as_ref().filter(|m| m.joint == f2.name) {
                if m.multiplier.abs() > 1e-9 {
                    (q1 - m.offset) / m.multiplier
                } else {
                    q1
                }
            } else {
                let t1 = hi1 - lo1;
                if t1.abs() > 1e-9 {
                    lo2 + (q1 - lo1) / t1 * (hi2 - lo2)
                } else {
                    lo2
                }
            }
        };
        let gap_at = |q1: f32| -> f32 {
            let x1 = v_dot(p1, u) + q1 * v_dot(a1, u);
            let x2 = v_dot(p2, u) + q2_of(q1) * v_dot(a2, u);
            (x1 - x2).abs()
        };
        let (ga, gb) = (gap_at(lo1), gap_at(hi1));
        let (mut gap_open, mut gap_closed) = (ga.max(gb), ga.min(gb));
        if !(gap_open.is_finite() && gap_closed.is_finite()) {
            skipped.push(format!(
                "夹爪 {}/{} 间距不可计算 → 引擎默认 gap",
                f1.name, f2.name
            ));
            gap_open = preset.gap_open;
            gap_closed = preset.gap_closed;
        }
        // 接近轴 ≈ 手指原点相对父系的方向（Franka：+Z）
        let approach = f1.origin_xyz;
        // M2 工作包 A（M0b 板系朝向字段接线）：接近/开合方向从手指父系
        // （= 挂载系）经 `r_up ∘ site.mount.r` 旋到 OriEra 挂载体系——
        // builder 的双板按这两个数据向量摆放，不再是恒 +X/+Z 约定。
        let r_mount = mat_mul(r_up, &site.mount.r);
        let approach_axis = v_normalize(mat_vec(&r_mount, approach));
        let gap_axis = v_normalize(mat_vec(&r_mount, u));
        // finger_half：手指 box 半尺寸（挂载系 AABB）旋成挂载体系 AABB
        // ——板系朝向已由 approach/gap_axis 数据给出，半尺寸跟随体轴，
        // 不再做 [接近,其余,开合] 的轴重排（90° 整数倍旋转精确，任意
        // 角度保守包络，与链体碰撞体同一纪律）。
        let finger_half = match half1 {
            Some(h) => aabb_half(&r_mount, h),
            None => {
                skipped.push(format!(
                    "手指 link `{}` 无 box 碰撞体 → finger_half 用引擎默认",
                    f1.child
                ));
                preset.finger_half
            }
        };
        // TCP = 两指板中心中点（q=0），手指父系 → 挂载体系（世界对齐）
        let tcp_local = v_scale(v_add(p1, p2), 0.5);
        let tcp_world = v_add(
            v_sub(site.mount.p, site.body_frame.p),
            mat_vec(&site.mount.r, tcp_local),
        );
        let tcp_offset = mat_vec(r_up, tcp_world);
        let finger_x = {
            let d = v_dot(tcp_local, v_normalize(approach)).abs();
            if d > 1e-6 {
                d
            } else {
                v_len(tcp_local)
            }
        };
        let effort = f1
            .effort
            .filter(|e| e.is_finite() && *e > 0.0)
            .unwrap_or(preset.effort);
        let gap_rate = match f1.velocity.filter(|v| v.is_finite() && *v > 0.0) {
            Some(v) => {
                let m = f2.mimic.as_ref().map_or(1.0, |m| m.multiplier.abs());
                (v * (1.0 + m) / tick_hz.max(1.0)).max(1e-5)
            }
            None => preset.gap_rate,
        };
        GripperSpec {
            attach_link: site.body,
            finger_x,
            tcp_offset,
            finger_half,
            gap_open,
            gap_closed,
            friction: preset.friction,
            effort,
            gap_rate,
            kind: GripperKind::ParallelPlates,
            // M0b 板系朝向字段（M2 工作包 A 接线完毕）：接近/开合方向
            // 经 `r_up ∘ site.mount.r` 旋到挂载体系——外部夹爪（Panda
            // 法兰朝下构型）的板与 TCP 在同一坐标系内全对齐。
            approach_axis: Some(approach_axis),
            gap_axis: Some(gap_axis),
            // M2 物理保真旋钮：导入器不擅自开启——夹爪接触面网格
            // （contact_mesh）与接触级抓取判据（Contact）由调用方在
            // spec 上按需 opt-in。
            contact_mesh: false,
            holding_criterion: preset.holding_criterion,
        }
    }

    fn build(
        &self,
        external: Option<&UrdfReport>,
        opts: &mut RobotSpecOpts,
    ) -> Result<RobotSpec, String> {
        // 引擎默认值不从 rigid3d 的 pub(crate) 常量拿，而是读 4 轴预设投影
        // ——robot.rs/rigid3d.rs 冻结，这条路零耦合且随预设同步。
        let preset = RobotSpec::from(&ArmDescription::default());
        let preset_grip = preset
            .gripper
            .as_ref()
            .ok_or("预设 RobotSpec 缺 gripper（robot.rs 契约变化）")?;
        let ctx = LinkCtx {
            r_up: match opts.up_axis {
                UpAxis::Z => MAT_Z_UP_TO_Y_UP,
                UpAxis::Y => MAT_IDENTITY,
            },
            preset_root: &preset.links[0],
            preset_dyn: &preset.links[1],
            default_radius: ArmDescription::default().link_radius,
            mesh_base_dir: opts.mesh_base_dir.clone(),
            mesh_packages: &opts.mesh_packages,
        };
        let mut skipped = Vec::new();

        let fingers = self.detect_fingers();
        if fingers.is_none() {
            if let Some(j) = self.joints.iter().find(|j| j.mimic.is_some()) {
                skipped.push(format!(
                    "关节 {} 带 <mimic> 但非 prismatic 两指对（Robotiq 式 revolute 连杆夹爪待 M2）",
                    j.name
                ));
            }
        }
        let ChainWalk {
            bodies,
            joints: chain,
            site,
        } = self.walk_chain(fingers.as_ref(), &mut skipped)?;
        if chain.is_empty() && site.is_none() && external.is_none() {
            return Err(format!(
                "URDF 链提取失败：根 {} 下无可用的 revolute/continuous/prismatic 关节，也未检测到两指夹爪",
                self.root
            ));
        }

        // links：形状 / 碰撞偏移 / 密度；origin 稍后沿链累加
        let mut links: Vec<LinkSpec> = bodies
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let next_p = chain
                    .iter()
                    .find(|cj| cj.parent == i)
                    .map(|cj| cj.child_frame.p);
                self.link_spec(b, next_p, i == 0, &ctx, &mut skipped)
            })
            .collect();

        // joints + origin FK（与 From<&ArmDescription> 同款：子 origin = 父 origin + anchor，
        // 逐位满足 robot.rs 的链一致性约定）
        links[0].origin = opts.mount;
        let mut joints = Vec::with_capacity(chain.len());
        for cj in &chain {
            let j = cj.joint;
            let anchor = mat_vec(
                &ctx.r_up,
                v_sub(cj.child_frame.p, bodies[cj.parent].frame.p),
            );
            let axis_urdf = axis_or_default(j, &mut skipped);
            // <axis> 在关节/子系；经子系旋转到父系（= 世界）后归一化
            let axis = v_normalize(mat_vec(&ctx.r_up, mat_vec(&cj.child_frame.r, axis_urdf)));
            let limits = joint_limits(j, &mut skipped);
            let kind = if j.kind == "prismatic" {
                JointKind::Prismatic
            } else {
                JointKind::Revolute
            };
            // 增益量纲随关节类型（N·m/rad vs N/m）：两套缺省互不换算，
            // `per_joint` 按名覆盖对两类都生效。
            let default_gains = match kind {
                JointKind::Revolute => (opts.default_kp, opts.default_kd),
                JointKind::Prismatic => (opts.default_kp_prismatic, opts.default_kd_prismatic),
            };
            let (kp, kd) = opts
                .per_joint
                .get(&j.name)
                .copied()
                .unwrap_or(default_gains);
            links[cj.child].origin = v_add(links[cj.parent].origin, anchor);
            joints.push(JointSpec {
                name: j.name.clone(),
                kind,
                parent_link: cj.parent,
                child_link: cj.child,
                anchor,
                axis,
                limits,
                effort: j.effort.filter(|e| e.is_finite() && *e > 0.0),
                velocity_limit: j.velocity.filter(|v| v.is_finite() && *v > 0.0),
                kp,
                kd,
                armature: opts.default_armature,
                // <dynamics damping> 是被动黏性摩擦，不是 PD 的 kd（sim-engine review 语义）
                damping: j.damping.map(f32::abs).unwrap_or(preset.joints[0].damping),
            });
        }

        // gripper：外部夹爪挂末体法兰；否则自带手指对；否则 None
        let gripper = if let Some(ext) = external {
            let ef = ext.detect_fingers().ok_or(
                "外部夹爪 URDF 未检测到两指 prismatic 对（需同父两条 prismatic 或 <mimic> 配对）",
            )?;
            let ext_walk = ext.walk_chain(Some(&ef), &mut skipped)?;
            if !ext_walk.joints.is_empty() {
                skipped.push(format!(
                    "外部夹爪 URDF 含 {} 个 revolute 链关节——未并入臂链（仅取手指对）",
                    ext_walk.joints.len()
                ));
            }
            let es = ext_walk.site.ok_or("外部夹爪：手指父 link 不在其根链上")?;
            if site.is_some() {
                skipped.push("臂 URDF 自带夹爪被外部夹爪覆盖".into());
            }
            let last = bodies.len() - 1;
            Some(ext.build_gripper(
                &ef,
                &GripperSite {
                    body: last,
                    body_frame: bodies[last].frame,
                    mount: bodies[last].tip.compose(&es.mount),
                },
                &ctx.r_up,
                preset_grip,
                opts.tick_hz,
                &mut skipped,
            ))
        } else if let (Some(f), Some(s)) = (fingers.as_ref(), site.as_ref()) {
            Some(self.build_gripper(f, s, &ctx.r_up, preset_grip, opts.tick_hz, &mut skipped))
        } else {
            None
        };

        let n = joints.len();
        // home：URDF 零位；限位不含零时（Panda joint4 ∈ [−3.07, −0.07]）钳到最近限位
        let home = joints
            .iter()
            .map(|j| 0.0f32.clamp(j.limits.0, j.limits.1))
            .collect();
        opts.skipped.append(&mut skipped);
        Ok(RobotSpec {
            name: if self.name.is_empty() {
                "urdf".into()
            } else {
                self.name.clone()
            },
            base: opts.mount,
            links,
            joints,
            gripper,
            home,
            mass_scale: 1.0,
            // 与 4 轴预设同款（rad / 单位动作）；包装层旋钮，物理不消费
            action_scale: vec![2.0; n],
            // 自碰撞默认关（legacy 行为）；bin_pick 类任务由 spec 显式
            // opt-in AdjacentExempt（M2 U-9）
            self_collision: oriera_subsys::SelfCollision::Off,
        })
    }
}

/// P1-B1 坑位治理（用户角色集群 PUGC-2/3 实测两坑）：全链导入结果的人话
/// 警告清单——**非致命**，CLI 打印后照跑。第三坑（base 朝向）无法从
/// [`RobotSpec`] 判定（导入只平移不旋转），由 CLI 面固定提示 + 文档写明。
///
/// - **gap_closed ≈ 0**（< 0.01 m）：指板闭合位相触——运行时一碰方块就
///   把它挤飞，抓取永不成立；给 preset 参考值与 `--set` 覆盖路径。
/// - **home 全零**：home = URDF 零位钳限位的结果，竖直/奇异构型是常态；
///   给 `--set robot.home=…` 覆盖路径。
pub fn pitfall_warnings(robot: &RobotSpec) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(g) = &robot.gripper {
        if g.gap_closed < 0.01 {
            out.push(format!(
                "夹爪 gap_closed = {:.4} m ≈ 0：指板闭合位相触——运行时会挤飞方块，抓取永不成立；\
                 参考 preset 的 0.046 m，可用 --set robot.gripper.gap_closed=0.046 覆盖",
                g.gap_closed
            ));
        }
    }
    if !robot.joints.is_empty() && robot.home.iter().all(|h| h.abs() < 1e-6) {
        out.push(
            "home 全零 = URDF 零位（常见为竖直/奇异构型，脚本/策略从这里出发可能够不着桌面）\
             ——可用 --set \"robot.home=[q1,q2,...]\" 覆盖（JSON 数组，分量数须等于关节数）"
                .to_string(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal OriEra-arm-shaped fixture (4 revolute joints on a base).
    const FIXTURE: &str = r#"<?xml version="1.0"?>
    <robot name="oriera_test_arm">
      <link name="base">
        <collision><geometry><box size="0.08 0.12 0.12"/></geometry></collision>
      </link>
      <link name="l1"><collision><geometry><cylinder radius="0.022" length="0.20"/></geometry></collision></link>
      <link name="l2"><collision><geometry><cylinder radius="0.022" length="0.18"/></geometry></collision></link>
      <link name="l3"><collision><geometry><cylinder radius="0.022" length="0.09"/></geometry></collision></link>
      <link name="tool"><collision><geometry><sphere radius="0.01"/></geometry></collision></link>
      <joint name="j1" type="revolute">
        <parent link="base"/><child link="l1"/>
        <origin xyz="0 0 0.12" rpy="0 0 0"/><axis xyz="0 1 0"/>
        <limit lower="-3.14" upper="3.14" effort="80" velocity="2"/>
        <dynamics damping="0.05"/>
      </joint>
      <joint name="j2" type="revolute">
        <parent link="l1"/><child link="l2"/>
        <origin xyz="0.20 0 0" rpy="0 0 0"/><axis xyz="0 0 1"/>
        <limit lower="-0.35" upper="1.45" effort="80" velocity="2"/>
      </joint>
      <joint name="j3" type="revolute">
        <parent link="l2"/><child link="l3"/>
        <origin xyz="0.18 0 0" rpy="0 0 0"/><axis xyz="0 0 1"/>
        <limit lower="-2.5" upper="0.35" effort="80" velocity="2"/>
      </joint>
      <joint name="j4" type="fixed">
        <parent link="l3"/><child link="tool"/>
        <origin xyz="0.09 0 0" rpy="0 0 0"/>
      </joint>
    </robot>"#;

    #[test]
    fn parses_links_joints_and_root() {
        let rep = UrdfReport::parse(FIXTURE).expect("fixture must parse");
        assert_eq!(rep.links.len(), 5);
        assert_eq!(rep.joints.len(), 4);
        assert_eq!(rep.root, "base");
        let l1 = rep.links.iter().find(|l| l.name == "l1").unwrap();
        if let Some(UrdfGeom::Cylinder { radius, length }) = &l1.geometry {
            assert_eq!(*radius, 0.022);
            assert_eq!(*length, 0.20);
        } else {
            panic!("l1 must be a cylinder");
        }
        let j2 = rep.joints.iter().find(|j| j.name == "j2").unwrap();
        assert_eq!(j2.limit_upper, Some(1.45));
        assert_eq!(j2.origin_xyz, [0.20, 0.0, 0.0]);
    }

    #[test]
    fn fixed_joint_is_absorbed_chain_extracts_limits() {
        let rep = UrdfReport::parse(FIXTURE).unwrap();
        // chain walk: base →(j1)→ l1 →(j2)→ l2 →(j3)→ l3 →(j4 fixed)→ tool
        // want=4 stops at 3 revolute + fixed absorb; expect Err on 4.
        let (desc, skipped) = rep.first_revolute_chain(3).expect("3 revolute chain");
        assert!(skipped.is_empty());
        assert!(
            (desc.links[0] - 0.12).abs() < 1e-5,
            "j1 origin ‖xyz‖ = 0.12"
        );
        assert!((desc.links[1] - 0.20).abs() < 1e-5);
        assert!((desc.limits[1].1 - 1.45).abs() < 1e-5, "j2 upper limit");
        assert!(
            (desc.limits[2].0 - (-2.5)).abs() < 1e-5,
            "j3 lower limit (Rust round-4 肘限位)"
        );
    }

    #[test]
    fn inertial_and_effort_parsed() {
        let rep = UrdfReport::parse(FIXTURE).unwrap();
        let l1 = rep.links.iter().find(|l| l.name == "base").unwrap();
        assert!(l1.inertial.is_none(), "fixture base has no <inertial>");
        let j1 = rep.joints.iter().find(|j| j.name == "j1").unwrap();
        assert_eq!(j1.effort, Some(80.0));
    }

    #[test]
    fn chain_validation_catches_issues() {
        let rep = UrdfReport::parse(FIXTURE).unwrap();
        let issues = rep.validate_chain(4);
        // fixture 的 j1 轴是 +Y（非常规 ±Z）——校验应捕获
        assert!(
            issues.iter().any(|i| i.contains("非 ±Z 主轴")),
            "y-axis must be flagged: {issues:?}"
        );
        // 无环/断链
        assert!(!issues
            .iter()
            .any(|i| i.contains("环") || i.contains("断链")));
    }

    #[test]
    fn malformed_xml_is_an_error_not_a_panic() {
        assert!(UrdfReport::parse("<robot><link").is_err());
        assert!(UrdfReport::parse("<urdf/>").is_err());
    }

    #[test]
    fn inertial_offdiagonals_parsed_not_dropped() {
        // MJCF 交叉验证（2026-11-01）指出的表达力缺口：非对角惯量此前
        // 被静默丢弃——menagerie Panda 连杆带 0.019 kg·m² 级 ixy/ixz/iyz。
        let xml = r#"<?xml version="1.0"?>
<robot name="offdiag">
  <link name="l0">
    <inertial>
      <mass value="0.63"/>
      <inertia ixx="0.00315" iyy="0.00388" izz="0.004285"
               ixy="8.2904e-7" ixz="0.00015" iyz="8.2299e-6"/>
    </inertial>
  </link>
</robot>"#;
        let rep = UrdfReport::parse(xml).unwrap();
        let l0 = rep.links.iter().find(|l| l.name == "l0").unwrap();
        let i = l0.inertial.as_ref().unwrap();
        assert_eq!(i.ixy, 8.2904e-7);
        assert_eq!(i.ixz, 0.00015);
        assert_eq!(i.iyz, 8.2299e-6);
        // URDF 惯例允许省略非对角（对称张量在轴对齐时合法）→ 缺省 0。
        let xml2 = xml.replace(" ixy=\"8.2904e-7\" ixz=\"0.00015\" iyz=\"8.2299e-6\"", "");
        let rep2 = UrdfReport::parse(&xml2).unwrap();
        let i2 = rep2.links[0].inertial.as_ref().unwrap();
        assert_eq!((i2.ixy, i2.ixz, i2.iyz), (0.0, 0.0, 0.0));
    }

    /// mesh 联检 fixture：base 挂 STL、l1 挂裸 glTF（探针）。
    const MESH_FIXTURE: &str = r#"<?xml version="1.0"?>
    <robot name="mesh_arm">
      <link name="base"><collision><geometry><mesh filename="package://oriera_arm/meshes/base.stl"/></geometry></collision></link>
      <link name="l1"><collision><geometry><mesh filename="package://oriera_arm/meshes/l1.gltf"/></geometry></collision></link>
      <joint name="j1" type="revolute">
        <parent link="base"/><child link="l1"/>
        <origin xyz="0 0 0.12"/><axis xyz="0 0 1"/>
      </joint>
    </robot>"#;

    /// 唯一临时目录 + Drop 整树删除（与 meshres::tests 同款，自足）。
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let d = std::env::temp_dir().join(format!(
                "oriera_urdf_{}_{}_{}",
                std::process::id(),
                tag,
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&d).expect("创建临时目录");
            Self(d)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 四面体二进制 STL（与 meshres::tests 同一 fixture 语义）。
    fn tetra_stl_bytes() -> Vec<u8> {
        let tris: [[[f32; 3]; 3]; 4] = [
            [[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
            [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            [[0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        ];
        let mut out = vec![0u8; 80];
        out.extend_from_slice(&4u32.to_le_bytes());
        for t in &tris {
            for v in std::iter::once(&[0.0f32; 3]).chain(t.iter()) {
                for c in v {
                    out.extend_from_slice(&c.to_le_bytes());
                }
            }
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        out
    }

    #[test]
    fn mesh_assets_enter_the_chain_report() {
        let t = TempDir::new("chain_mesh");
        let packages =
            std::collections::BTreeMap::from([("oriera_arm".to_string(), t.0.join("oriera_arm"))]);
        std::fs::create_dir_all(t.0.join("oriera_arm/meshes")).unwrap();
        let stl_path = t.0.join("oriera_arm/meshes/base.stl");
        std::fs::write(&stl_path, tetra_stl_bytes()).unwrap();
        std::fs::write(
            t.0.join("oriera_arm/meshes/l1.gltf"),
            r#"{"meshes":[{"name":"l1","primitives":[{},{}]}]}"#,
        )
        .unwrap();

        let rep = UrdfReport::parse(MESH_FIXTURE).unwrap();
        // 链本身无问题（轴 ±Z），两条 mesh 资产行各就各位
        let issues = rep.validate_chain_with_meshes(1, &t.0, &packages);
        assert_eq!(issues.len(), 2, "仅 mesh 行、无链问题：{issues:?}");
        assert!(
            issues[0].contains("base.stl") && issues[0].contains("4 三角形"),
            "STL 行需带三角形计数：{issues:?}"
        );
        assert!(
            issues[0].contains("包围盒"),
            "STL 行需带包围盒尺寸：{issues:?}"
        );
        assert!(
            issues[1].contains("l1.gltf") && issues[1].contains("primitives"),
            "探针行需带 primitives 计数：{issues:?}"
        );

        // 资产缺位 → 人话报错，不 panic、不静默
        std::fs::remove_file(&stl_path).unwrap();
        let issues = rep.validate_chain_with_meshes(1, &t.0, &packages);
        assert!(issues.iter().any(|i| i.contains("不可用")), "{issues:?}");
    }

    // ── U4：velocity / mimic / collision origin 解析 + RobotSpec 全链 ──

    #[test]
    fn velocity_and_robot_name_are_parsed() {
        let rep = UrdfReport::parse(FIXTURE).unwrap();
        assert_eq!(rep.name, "oriera_test_arm");
        let j1 = rep.joints.iter().find(|j| j.name == "j1").unwrap();
        assert_eq!(
            j1.velocity,
            Some(2.0),
            "<limit velocity> 此前被丢弃——U4 补齐"
        );
        let j4 = rep.joints.iter().find(|j| j.name == "j4").unwrap();
        assert_eq!(j4.velocity, None, "fixed 关节无 <limit>");
    }

    #[test]
    fn mimic_is_parsed_and_still_reported() {
        let xml = r#"<robot name="g">
          <link name="hand"/><link name="f1"/><link name="f2"/>
          <joint name="finger_1" type="prismatic"><parent link="hand"/><child link="f1"/>
            <axis xyz="0 1 0"/><limit lower="0" upper="0.04" effort="20" velocity="0.2"/></joint>
          <joint name="finger_2" type="prismatic"><parent link="hand"/><child link="f2"/>
            <axis xyz="0 -1 0"/><limit lower="0" upper="0.04" effort="20" velocity="0.2"/>
            <mimic joint="finger_1" multiplier="-1" offset="0.001"/></joint>
        </robot>"#;
        let rep = UrdfReport::parse(xml).unwrap();
        let f2 = rep.joints.iter().find(|j| j.name == "finger_2").unwrap();
        let m = f2.mimic.as_ref().expect("mimic parsed into the joint");
        assert_eq!(
            (m.joint.as_str(), m.multiplier, m.offset),
            ("finger_1", -1.0, 0.001)
        );
        // 既有输出面不变：skipped 仍登记一条 <mimic> 行
        assert!(rep
            .skipped
            .iter()
            .any(|s| s.contains("<mimic>") && s.contains("finger_2")));
        assert_eq!(
            rep.gripper_finger_joints(),
            Some(("finger_1".into(), "finger_2".into()))
        );
        // multiplier 缺省 1、offset 缺省 0
        let rep2 =
            UrdfReport::parse(&xml.replace(r#" multiplier="-1" offset="0.001""#, "")).unwrap();
        let m2 = rep2.joints[1].mimic.as_ref().unwrap();
        assert_eq!((m2.multiplier, m2.offset), (1.0, 0.0));
    }

    #[test]
    fn collision_origin_is_parsed() {
        let xml = r#"<robot name="c"><link name="l">
          <collision><origin xyz="0.1 0.2 0.3" rpy="0 1.5707963 0"/>
            <geometry><box size="0.2 0.4 0.6"/></geometry></collision>
        </link></robot>"#;
        let rep = UrdfReport::parse(xml).unwrap();
        assert_eq!(rep.links[0].collision_origin_xyz, [0.1, 0.2, 0.3]);
        assert!((rep.links[0].collision_origin_rpy[1] - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    }

    #[test]
    fn opts_defaults_match_the_preset_gains() {
        // 任务书钉死 60 / 4 / 0.02；与 ArmDescription 预设同源，漂移即红
        let o = RobotSpecOpts::default();
        let preset = RobotSpec::from(&ArmDescription::default());
        assert_eq!(
            (o.default_kp, o.default_kd),
            (preset.joints[0].kp, preset.joints[0].kd)
        );
        assert_eq!(o.default_armature, preset.joints[0].armature);
        assert_eq!(o.up_axis, UpAxis::Z);
    }

    #[test]
    fn rpy_matrix_follows_urdf_extrinsic_xyz() {
        // Rx(−90°)·ez = +Y（Panda joint2 的轴进父系）
        let r = rpy_to_mat([-PI / 2.0, 0.0, 0.0]);
        let v = mat_vec(&r, [0.0, 0.0, 1.0]);
        assert!(
            (v[0]).abs() < 1e-6 && (v[1] - 1.0).abs() < 1e-6 && v[2].abs() < 1e-6,
            "{v:?}"
        );
        // Rz(90°)·ex = +Y
        let r = rpy_to_mat([0.0, 0.0, PI / 2.0]);
        let v = mat_vec(&r, [1.0, 0.0, 0.0]);
        assert!(v[0].abs() < 1e-6 && (v[1] - 1.0).abs() < 1e-6, "{v:?}");
        // 复合顺序 Rz·Ry·Rx：rpy=(90°, 0, 90°) 作用于 ey → Rz(90)·Rx(90)·ey = Rz(90)·ez = ez
        let r = rpy_to_mat([PI / 2.0, 0.0, PI / 2.0]);
        let v = mat_vec(&r, [0.0, 1.0, 0.0]);
        assert!((v[2] - 1.0).abs() < 1e-6, "{v:?}");
        // 上轴换算：URDF +Z → OriEra +Y
        let v = mat_vec(&MAT_Z_UP_TO_Y_UP, [0.0, 0.0, 1.0]);
        assert_eq!(v, [0.0, 1.0, 0.0]);
    }

    #[test]
    fn fixture_maps_to_a_three_joint_robot_spec_not_four() {
        // 内嵌 fixture 实际 3 revolute + 1 fixed：全链导入按实得 3 轴，
        // 不再像 first_revolute_chain 那样投影/补齐到 4 轴。
        let rep = UrdfReport::parse(FIXTURE).unwrap();
        let mut opts = RobotSpecOpts {
            up_axis: UpAxis::Y,
            mount: [0.0, 0.5, 0.0],
            ..RobotSpecOpts::default()
        };
        let spec = rep.to_robot_spec(&mut opts).expect("fixture → RobotSpec");
        assert_eq!(spec.name, "oriera_test_arm");
        assert_eq!(spec.joints.len(), 3, "{:?}", spec.joint_names());
        assert_eq!(
            spec.links.len(),
            4,
            "base, l1, l2, l3（tool 被 fixed 吸收进 l3）"
        );
        assert!(spec.gripper.is_none());
        spec.validate().expect("valid chain");
        // j1：anchor = (0,0,0.12)（Y-up 不旋转）、轴 +Y、限位/effort/velocity/damping 全接
        let j1 = &spec.joints[0];
        assert_eq!(j1.anchor, [0.0, 0.0, 0.12]);
        assert_eq!(j1.axis, [0.0, 1.0, 0.0]);
        // 限位 = URDF 字面值回环（fixture 写的是 ±3.14，不是 π）
        let u1 = rep.joints.iter().find(|j| j.name == "j1").unwrap();
        assert_eq!(
            j1.limits,
            (u1.limit_lower.unwrap(), u1.limit_upper.unwrap())
        );
        assert_eq!((j1.effort, j1.velocity_limit), (Some(80.0), Some(2.0)));
        assert_eq!(j1.damping, 0.05);
        assert_eq!((j1.kp, j1.kd, j1.armature), (60.0, 4.0, 0.02));
        // j2 无 <dynamics> → 引擎默认阻尼（预设 0.05）
        assert_eq!(
            spec.joints[1].damping,
            RobotSpec::from(&ArmDescription::default()).joints[0].damping
        );
        // origin 沿链累加：base=mount，l1 = base + anchor
        assert_eq!(spec.links[0].origin, [0.0, 0.5, 0.0]);
        assert_eq!(spec.links[1].origin, [0.0, 0.5, 0.12]);
        assert_eq!(spec.links[2].origin, [0.2, 0.5, 0.12]);
        // 碰撞体：base box 直映（半尺寸）；l1 cylinder 沿 link-Z、非 ±X → Box 包络（登记）
        assert_eq!(
            spec.links[0].shape,
            LinkShape::Box {
                half: [0.04, 0.06, 0.06]
            }
        );
        assert!(matches!(spec.links[1].shape, LinkShape::Box { .. }));
        assert!(opts
            .skipped
            .iter()
            .any(|s| s.contains("cylinder") && s.contains("l1")));
        // 无 <inertial> → 引擎默认密度 / added_mass
        let preset = RobotSpec::from(&ArmDescription::default());
        assert_eq!(spec.links[1].density, preset.links[1].density);
        assert_eq!(spec.links[1].added_mass, preset.links[1].added_mass);
        assert_eq!(spec.links[0].added_mass, 0.0, "根不加质量");
        // per_joint 覆盖
        let mut opts2 = RobotSpecOpts::default();
        opts2.per_joint.insert("j2".into(), (120.0, 9.0));
        let spec2 = rep.to_robot_spec(&mut opts2).unwrap();
        assert_eq!((spec2.joints[1].kp, spec2.joints[1].kd), (120.0, 9.0));
        assert_eq!((spec2.joints[0].kp, spec2.joints[0].kd), (60.0, 4.0));
    }

    #[test]
    fn z_up_conversion_rotates_anchor_and_axis() {
        // j1 origin (0,0,0.12) 轴 +Y：Z-up→Y-up 后 anchor (0,0.12,0)、轴 (0,0,−1)
        let rep = UrdfReport::parse(FIXTURE).unwrap();
        let mut opts = RobotSpecOpts::default();
        let spec = rep.to_robot_spec(&mut opts).unwrap();
        let j1 = &spec.joints[0];
        assert_eq!(j1.anchor, [0.0, 0.12, 0.0]);
        assert_eq!(j1.axis, [0.0, 0.0, -1.0]);
        assert_eq!(
            spec.joints[1].axis,
            [0.0, 1.0, 0.0],
            "URDF +Z 轴 → OriEra +Y"
        );
    }

    #[test]
    fn mesh_links_degrade_to_envelopes_and_inertial_flows_through() {
        let xml = r#"<robot name="m">
          <link name="base"><collision><geometry><mesh filename="package://x/base.stl"/></geometry></collision></link>
          <link name="l1">
            <inertial><mass value="3.0"/><inertia ixx="1" iyy="2" izz="3" ixy="0.1"/></inertial>
            <collision><geometry><mesh filename="package://x/l1.stl"/></geometry></collision>
          </link>
          <link name="l2"><inertial><mass value="1.5"/><inertia ixx="1" iyy="1" izz="1"/></inertial></link>
          <joint name="j1" type="revolute"><parent link="base"/><child link="l1"/>
            <origin xyz="0 0 0.1"/><axis xyz="0 0 1"/><limit lower="-1" upper="1" effort="10" velocity="1"/></joint>
          <joint name="j2" type="revolute"><parent link="l1"/><child link="l2"/>
            <origin xyz="0.3 0 0"/><axis xyz="0 1 0"/><limit lower="-1" upper="1" effort="10" velocity="1"/></joint>
        </robot>"#;
        let rep = UrdfReport::parse(xml).unwrap();
        let mut opts = RobotSpecOpts::default();
        let spec = rep.to_robot_spec(&mut opts).unwrap();
        spec.validate()
            .expect("mesh_uri stays None so validate passes");
        assert!(spec.links.iter().all(|l| l.mesh_uri.is_none()));
        let mesh_rows: Vec<_> = opts.skipped.iter().filter(|s| s.contains("mesh")).collect();
        assert_eq!(mesh_rows.len(), 2, "{:?}", opts.skipped);
        assert!(mesh_rows.iter().any(|s| s.contains("package://x/l1.stl")));
        assert!(opts
            .skipped
            .iter()
            .any(|s| s.contains("l2") && s.contains("无 <collision>")));
        // l1：链段 (0.3,0,0) 沿 X → Capsule；`<inertial>` 全张量直进
        // LinkSpec.inertial（M2 U-1），density 退为簿记缺省、added_mass=0
        let l1 = &spec.links[1];
        let LinkShape::Capsule {
            half_len,
            radius: _,
        } = l1.shape
        else {
            panic!("X 向链段应为 Capsule：{:?}", l1.shape)
        };
        assert!((half_len - 0.15).abs() < 1e-6);
        let i = l1.inertial.expect("inertial passthrough");
        assert_eq!(i.mass, 3.0);
        // Z-up→Y-up 是共轭：I_body = M·I_urdf·Mᵀ——对角置换 (1,3,2)，
        // ixy(0.1) 落到体 (0,2) 分量带符号翻 → ixz' = −0.1。
        assert_eq!(
            (i.ixx, i.iyy, i.izz, i.ixy, i.ixz, i.iyz),
            (1.0, 3.0, 2.0, 0.0, -0.1, 0.0)
        );
        assert_eq!(
            l1.density,
            RobotSpec::from(&ArmDescription::default()).links[1].density
        );
        assert_eq!(l1.added_mass, 0.0);
        assert_eq!(l1.collider_offset, [0.15, 0.0, 0.0]);
        // l2：末体、无链段 → 包络立方；inertial 直给
        let l2 = &spec.links[2];
        assert!(matches!(l2.shape, LinkShape::Box { .. }));
        assert_eq!(l2.inertial.as_ref().map(|i| i.mass), Some(1.5));
        // j1：URDF +Z 轴 → OriEra +Y；j2：URDF +Y 轴 → OriEra −Z
        assert_eq!(spec.joints[0].axis, [0.0, 1.0, 0.0]);
        assert_eq!(spec.joints[1].axis, [0.0, 0.0, -1.0]);
    }

    /// M2 U-1：惯量张量的坐标架旋转是共轭——恒等不变、90° 置换把对角
    /// 重排并把非对角按符号翻（解析可手算的两种情形）。
    #[test]
    fn rotate_inertia_is_the_conjugation() {
        // 恒等：逐项原样
        assert_eq!(
            rotate_inertia(&MAT_IDENTITY, 1.0, 2.0, 3.0, 0.1, 0.2, 0.3),
            (1.0, 2.0, 3.0, 0.1, 0.2, 0.3)
        );
        // Z-up→Y-up 置换 M：(x,y,z) ↦ (x,z,−y)。I = diag(1,2,3) + ixy:
        // M·I·Mᵀ 对角 = (1, 3, 2)；ixy 分量 (0,1) ← 老 (0,2)=ixz 带符号。
        let m = MAT_Z_UP_TO_Y_UP;
        let (ixx, iyy, izz, ixy, ixz, iyz) = rotate_inertia(&m, 1.0, 2.0, 3.0, 0.0, 0.5, 0.0);
        assert!((ixx - 1.0).abs() < 1e-6 && (iyy - 3.0).abs() < 1e-6 && (izz - 2.0).abs() < 1e-6);
        // M 把 ey ↦ −ez、ez ↦ ey：I[0][1]' = Σ R[0][k] λ_k R[1][k]；
        // 直接对照手算：新 ixz'（体 (0,2)）= −老 ixy？——不手推符号，
        // 用不变量核：迹与 Frobenius 范数必须逐项守恒。
        let tr = ixx + iyy + izz;
        assert!((tr - 6.0).abs() < 1e-5, "trace invariant: {tr}");
        let frob = ixx * ixx + iyy * iyy + izz * izz + 2.0 * (ixy * ixy + ixz * ixz + iyz * iyz);
        let want = 1.0 + 4.0 + 9.0 + 2.0 * 0.25;
        assert!(
            (frob - want).abs() < 1e-4,
            "Frobenius invariant: {frob} vs {want}"
        );
    }

    /// M2 U-3：给了 `opts.mesh_base_dir` 的 STL mesh → ConvexHull（顶点
    /// 旋到体轴、`mesh_uri` 留原文）；GLB 仍探针 → 包络降级。
    #[test]
    fn stl_mesh_resolves_to_a_convex_hull() {
        let t = TempDir::new("stl_hull");
        let cube_stl = cube_stl_bytes(0.05);
        std::fs::write(t.0.join("cube.stl"), &cube_stl).unwrap();
        std::fs::write(t.0.join("hand.glb"), b"glTF-mock-not-parsed").unwrap_or(());
        let xml = r#"<robot name="h">
          <link name="base"/>
          <link name="l1"><collision><geometry><mesh filename="cube.stl"/></geometry></collision></link>
          <link name="l2"><collision><geometry><mesh filename="hand.glb"/></geometry></collision></link>
          <joint name="j1" type="revolute"><parent link="base"/><child link="l1"/>
            <origin xyz="0 0 0.1"/><axis xyz="0 0 1"/><limit lower="-1" upper="1" effort="10" velocity="1"/></joint>
          <joint name="j2" type="revolute"><parent link="l1"/><child link="l2"/>
            <origin xyz="0.2 0 0"/><axis xyz="0 0 1"/><limit lower="-1" upper="1" effort="10" velocity="1"/></joint>
        </robot>"#;
        let rep = UrdfReport::parse(xml).unwrap();
        let mut opts = RobotSpecOpts {
            mesh_base_dir: Some(t.0.clone()),
            ..RobotSpecOpts::default()
        };
        let spec = rep.to_robot_spec(&mut opts).expect("全链导入");
        spec.validate()
            .expect("hull spec 合法（mesh_uri 是 metadata）");
        let l1 = &spec.links[1];
        let LinkShape::ConvexHull { points } = &l1.shape else {
            panic!("STL 应解析为 ConvexHull：{:?}", l1.shape)
        };
        assert_eq!(points.len(), 8, "立方体 12 三角去重后 8 顶点：{points:?}");
        let lo = [f32::MIN; 3];
        let _ = lo;
        let mut mn = [f32::MAX; 3];
        let mut mx = [f32::MIN; 3];
        for p in points {
            for k in 0..3 {
                mn[k] = mn[k].min(p[k]);
                mx[k] = mx[k].max(p[k]);
            }
        }
        for k in 0..3 {
            assert!(
                ((mx[k] - mn[k]) - 0.1).abs() < 1e-5,
                "AABB 边长 = 0.1（半 0.05 立方），axis {k}: {mn:?} {mx:?}"
            );
        }
        assert_eq!(l1.mesh_uri.as_deref(), Some("cube.stl"), "provenance 保留");
        // GLB：探针-only → 包络降级、mesh_uri None
        let l2 = &spec.links[2];
        assert!(
            !matches!(l2.shape, LinkShape::ConvexHull { .. }),
            "{:?}",
            l2.shape
        );
        assert!(l2.mesh_uri.is_none());
        assert!(opts.skipped.iter().any(|s| s.contains("hand.glb")));
        // 构建过闸：from_robot 接受 hull 链体
        let sys =
            oriera_subsys::MultibodySystem::from_robot(spec, oriera_subsys::Scene3D::default());
        assert!(sys.is_ok(), "hull builds: {:?}", sys.err());
    }

    /// 中心在原点、半边 `h` 的立方体二进制 STL（12 三角，法线占位零）。
    fn cube_stl_bytes(h: f32) -> Vec<u8> {
        let s = h;
        let v: [[f32; 3]; 8] = [
            [-s, -s, -s],
            [s, -s, -s],
            [s, s, -s],
            [-s, s, -s],
            [-s, -s, s],
            [s, -s, s],
            [s, s, s],
            [-s, s, s],
        ];
        let q: [[usize; 3]; 12] = [
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [3, 6, 2],
            [3, 7, 6],
            [1, 2, 6],
            [1, 6, 5],
            [0, 4, 7],
            [0, 7, 3],
        ];
        let mut out = vec![0u8; 80];
        out.extend_from_slice(&12u32.to_le_bytes());
        for t in &q {
            for k in std::iter::once(&[0.0f32; 3]).chain(t.iter().map(|i| &v[*i])) {
                for c in k {
                    out.extend_from_slice(&c.to_le_bytes());
                }
            }
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        out
    }

    /// M3：串联链上的 prismatic 关节真导入（不再断链报错）——类型、米制限位、
    /// N 制 effort、m/s 速度、独立的 N/m 增益缺省、per_joint 覆盖、缺
    /// `<limit>` 的有限兜底；floating 仍断链报错，空 URDF 仍人话报错。
    #[test]
    fn prismatic_chain_joint_imports_and_broken_chain_errors() {
        let xml = r#"<robot name="p">
          <link name="a"/><link name="b"/><link name="c"/>
          <joint name="slide" type="prismatic"><parent link="a"/><child link="b"/>
            <axis xyz="1 0 0"/><limit lower="0" upper="0.1" effort="1" velocity="1"/></joint>
          <joint name="j" type="revolute"><parent link="b"/><child link="c"/>
            <axis xyz="0 0 1"/><limit lower="-1" upper="1" effort="1" velocity="1"/></joint>
        </robot>"#;
        let rep = UrdfReport::parse(xml).unwrap();
        let mut opts = RobotSpecOpts::default();
        let spec = rep
            .to_robot_spec(&mut opts)
            .expect("prismatic 链关节真导入");
        assert_eq!(spec.joints.len(), 2, "{:?}", spec.joint_names());
        let s = &spec.joints[0];
        assert_eq!(s.kind, JointKind::Prismatic);
        assert_eq!(s.limits, (0.0, 0.1), "限位按米直接接");
        assert_eq!(
            (s.effort, s.velocity_limit),
            (Some(1.0), Some(1.0)),
            "N / m/s"
        );
        assert_eq!(s.axis, [1.0, 0.0, 0.0], "URDF +X 经 Z-up→Y-up 仍为 +X");
        assert_eq!(
            (s.kp, s.kd),
            (opts.default_kp_prismatic, opts.default_kd_prismatic),
            "prismatic 增益取独立的 N/m 缺省"
        );
        assert_eq!((s.kp, s.kd), (2000.0, 200.0));
        let r = &spec.joints[1];
        assert_eq!(r.kind, JointKind::Revolute);
        assert_eq!((r.kp, r.kd), (opts.default_kp, opts.default_kd));
        spec.validate().expect("prismatic + revolute 链合法");
        assert!(
            oriera_subsys::MultibodySystem::from_robot(spec, oriera_subsys::Scene3D::default())
                .is_ok(),
            "prismatic 链能进 multibody"
        );
        // per_joint 按名覆盖对 prismatic 同样生效
        let mut o2 = RobotSpecOpts::default();
        o2.per_joint.insert("slide".into(), (750.0, 55.0));
        let spec2 = rep.to_robot_spec(&mut o2).unwrap();
        assert_eq!((spec2.joints[0].kp, spec2.joints[0].kd), (750.0, 55.0));
        // 缺 <limit> 的 prismatic：有限 ±1 m 兜底并登记（不虚构 ±π 米）
        let no_lim = xml.replace(
            r#"<limit lower="0" upper="0.1" effort="1" velocity="1"/></joint>
          <joint name="j""#,
            r#"</joint>
          <joint name="j""#,
        );
        let mut o3 = RobotSpecOpts::default();
        let spec3 = UrdfReport::parse(&no_lim)
            .unwrap()
            .to_robot_spec(&mut o3)
            .unwrap();
        assert_eq!(spec3.joints[0].limits, (-1.0, 1.0));
        assert!(
            o3.skipped
                .iter()
                .any(|s| s.contains("slide") && s.contains("±1 m")),
            "{:?}",
            o3.skipped
        );
        spec3.validate().expect("兜底限位有限");

        let xml2 = xml.replace(r#"type="prismatic""#, r#"type="floating""#);
        let rep2 = UrdfReport::parse(&xml2).unwrap();
        let err2 = rep2
            .to_robot_spec(&mut RobotSpecOpts::default())
            .unwrap_err();
        assert!(err2.contains("floating") && err2.contains("断开"), "{err2}");
        // 无关节无夹爪 → 人话错
        let rep3 = UrdfReport::parse(r#"<robot name="e"><link name="only"/></robot>"#).unwrap();
        assert!(rep3.to_robot_spec(&mut RobotSpecOpts::default()).is_err());
    }
}
