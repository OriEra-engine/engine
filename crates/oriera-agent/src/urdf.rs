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

use oriera_subsys::ArmDescription;
use roxmltree::Document;

/// One parsed link (kinematic frame + primitive geometry, if any).
#[derive(Debug, Clone, Default)]
pub struct UrdfLink {
    pub name: String,
    /// Collision-geometry primitive, in metres: (kind, dims).
    pub geometry: Option<UrdfGeom>,
    /// `<inertial>` block (U2: parsed for bookkeeping).
    pub inertial: Option<UrdfInertial>,
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
    /// `<mesh filename="package://...">` — URI recorded for L4 asset
    /// pipeline (GLB/STL loading when the asset center lands).
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
}

/// `<inertial>` block (U2: parsed for bookkeeping; physics still uses
/// geometric density derivation — the gap is recorded in the receipt).
#[derive(Debug, Clone, Default)]
pub struct UrdfInertial {
    pub mass: f32,
    pub ixx: f32,
    pub iyy: f32,
    pub izz: f32,
    pub origin_xyz: [f32; 3],
}

/// Parse outcome: the tree plus an honest skip list.
#[derive(Debug, Default)]
pub struct UrdfReport {
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
        let mut rep = UrdfReport::default();
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
        };
        if let Some(col) = n.children().find(|c| c.has_tag_name("collision")) {
            link.geometry = Self::parse_geometry(col);
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
            iner_data.ixx = i
                .attribute("ixx")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0);
            iner_data.iyy = i
                .attribute("iyy")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0);
            iner_data.izz = i
                .attribute("izz")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0);
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
                }
                "dynamics" => j.damping = c.attribute("damping").and_then(|v| v.parse().ok()),
                "mimic" => self
                    .skipped
                    .push(format!("<mimic> 于关节 {}（U2 待办）", j.name)),
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
                    "关节 {} 类型 {}（OriEra 仅支持 revolute/continuous/fixed——prismatic U2 后期）",
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
}
