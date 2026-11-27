//! L9 场景编排：场景意图 → 确定性布局——真实场景批次 WP10。
//!
//! 职责："说一句话，布好一个可进入的场景"里**模型之后**的那一半。模型只
//! 负责把一句话变成结构化的场景意图（[`SceneIntent`]）；布局是否放得下、
//! 间距是否合规、朝向是否合理，由本模块用确定性算法保证——这是"提示词
//! 形状归引擎"在场景维度的延伸。入口只有一个纯函数 [`plan`]：输入结构化
//! 意图 + 可用资产名单，输出 [`SceneLayout`]；**不调用任何模型**，无 I/O、
//! 无时钟、无隐藏状态。
//!
//! # 契约
//! - 纯 std、wasm32 可编译，零新增依赖（serde 可用：意图与布局都要能进
//!   场景 spec，沿用"实验即文件"的家规）；不引用 `oriera_subsys` /
//!   `oriera_render`；
//! - 摆放统一用 [`oriera_common::Transform`]（#7 的跨层词汇，可直接序列化
//!   进场景 spec）；
//! - 先查可用性：缺失资产**一次报全**（[`PlannerError::AssetUnavailable`]，
//!   按意图中首次出现顺序、去重）；再布点：锚点按 intent 序**先摆**（精确
//!   落位，不做场地裁剪、不做间距让步），散布件走"抖动网格 + 最小间距拒
//!   绝"——每个候选对**全部已摆件**做水平间距检查，单件尝试上限 64 次
//!   （首选网格候选，重试为全场均匀采样），超限报
//!   [`PlannerError::Infeasible`]（带 intent 下标与人读原因）；
//! - item 序 = intent 序 × 件内序；带锚点的意图第 0 件落锚点，其余
//!   `count - 1` 件散布；`count = 0` 不产生件（锚点也不占位）；
//! - Transform 约定：散布件 `translation = (x, 0.0, z)`（平地假设 y = 0），
//!   锚点件用调用方给的完整 `pos`（含高程，精确落位优先）；rotation 是绕
//!   y 的单位四元数——锚点件用指定 yaw，散布件用种子派生的均匀随机 yaw；
//!   scale 恒为单位；
//! - 确定性：同种子同输入两次 `plan` 布局逐位一致（种子推进用自写
//!   [`Lcg`]，常数对齐仓库 seed 风格——MMIX 常数与 `oriera_infer` 的
//!   spawn RNG 同款，初始态异或仓库种子常数 `0x9E3779B97F4A7C15`）；
//!   **禁 `DefaultHasher`**，它跨版本不稳定，会破坏逐位复现；输出顺序只
//!   取决于输入。
//!
//! # 诚实边界
//! - **平地假设**：散布件 `y = 0`；地形贴高、放到桌面/台面上是调用方的
//!   后续步骤（读地形高程改 `translation.y` 即可，布局的 XZ 不变）。锚点
//!   件的 y 由调用方给定（`anchor` 的 `pos` 是 `[f32; 3]`，给了高程就照
//!   摆）；
//! - `min_separation` 是**中心点到中心点**的水平（XZ）距离，不含资产自身
//!   占地半径/碰撞体；负值视为 0（无约束）。锚点与锚点之间不检查间距（精
//!   确落位优先，两者都钉死是调用方的意图）；散布件对锚点照常让位；
//! - 64 次尝试**不保证找到所有可行解**：原则上可行但随机/网格都难命中的
//!   稠密摆放会诚实报 Infeasible——本模块是"尽力布局器"，不是放样求解
//!   器；布不下时调大场地、调小间距或件数；
//! - 低密度场景下首选候选落在网格上，布局带轻微网格感（抖动与全场均匀
//!   回退缓解，但不消除）；
//! - 散布件 yaw 是均匀随机派生，没有语义（不做"面向入口/朝向路径"）；
//! - 确定性指**同算法版本**下同种子同输入逐位一致；算法演进（网格、尝试
//!   上限、常数）会改变布局输出——布局不承诺跨版本稳定，要存档请连输入
//!   意图一起存（场景 spec 本来就该这么存）。

use std::fmt;

use oriera_common::math::Quat;
use oriera_common::{Transform, Vec3};
use serde::{Deserialize, Serialize};

/// 单件散布的最大尝试次数（含首选网格候选）。超过即报
/// [`PlannerError::Infeasible`]——宁可诚实失败，不做无界搜索。
const MAX_ATTEMPTS: u32 = 64;

/// 单个摆放意图：把某资产摆 `count` 件，散布件彼此及与全部已摆件保持
/// `min_separation`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacementIntent {
    /// 要摆的资产名，对调用方给的 `available` 名单做精确匹配。
    pub asset_name: String,
    /// 本意图放置的总件数。带锚点时第 0 件落锚点，其余散布。
    pub count: u32,
    /// 本意图散布件与**所有已摆件**的最小水平间距（中心距，同一 world
    /// 单位）。≤ 0 视为无约束；非有限值直接判不可满足。
    pub min_separation: f32,
    /// 锚点：`Some((pos, yaw_deg))` 时第 0 件按 `pos` 精确落位（含高程，
    /// 不做场地裁剪——出界/悬空是调用方的意图，照摆）、朝向 `yaw_deg`
    /// （度，绕 y 右手系），其余件散布并让开锚点。`None` 时全部散布。
    pub anchor: Option<([f32; 3], f32)>,
}

/// 一句话场景的结构化形态（模型输出 → 本模块输入）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneIntent {
    /// 方形场地半边长：场地中心在原点，散布件全部落在
    /// `[-area_half, area_half]²` 内。
    pub area_half: f32,
    /// 布局种子。同种子同意图两次 [`plan`] 逐位一致，异种子布局不同。
    /// 种子只影响散布——锚点精确落位，与种子无关。
    pub seed: u64,
    /// 摆放意图，按序处理；输出 item 序 = 这里的下标 × 件内序。
    pub placements: Vec<PlacementIntent>,
}

/// 布局失败的两类原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannerError {
    /// 有意图引用了不在 `available` 名单里的资产。名单**一次报全**（按
    /// 意图中的首次出现顺序、去重），不挤牙膏。
    AssetUnavailable(Vec<String>),
    /// 第 `intent_index` 个 placement 布不下去。
    Infeasible {
        /// 出错的 placement 在 [`SceneIntent::placements`] 里的下标。
        intent_index: usize,
        /// 人读原因：尝试用尽 / 参数非有限等。
        reason: String,
    },
}

impl fmt::Display for PlannerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlannerError::AssetUnavailable(names) => write!(
                f,
                "资产不可用：缺失 [{}]（{} 项，名单一次报全）",
                names.join(", "),
                names.len()
            ),
            PlannerError::Infeasible {
                intent_index,
                reason,
            } => write!(
                f,
                "场景不可满足：第 {intent_index} 个 placement —— {reason}"
            ),
        }
    }
}

impl std::error::Error for PlannerError {}

/// 一个已落位的资产实例：布局输出的最小单元，可直接进场景 spec。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacedItem {
    /// 摆的是哪个资产（回填自 intent，供加载侧选 mesh/ prefab）。
    pub asset_name: String,
    /// 摆放变换（TRS，见 [`oriera_common::Transform`] 的合成顺序契约）。
    pub transform: Transform,
}

/// 一次布局的产出：所有已落位实例，item 序 = intent 序 × 件内序。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneLayout {
    /// 已落位实例，顺序只取决于输入。
    pub items: Vec<PlacedItem>,
}

/// 场景意图 → 确定性布局。纯函数：不调模型、无 I/O、无时钟。
///
/// 流程：① 资产可用性一次报全；② 锚点按 intent 序先摆（精确落位，占住
/// 位置）；③ 散布件按 intent 序 × 件内序走"抖动网格 + 最小间距拒绝"，
/// 对全部已摆件（含锚点）做水平间距检查，单件最多尝试
/// [`MAX_ATTEMPTS`] 次。
///
/// # Examples
///
/// ```
/// use oriera_agent::scene_planner::{plan, PlacementIntent, SceneIntent};
///
/// let intent = SceneIntent {
///     area_half: 8.0,
///     seed: 42,
///     placements: vec![PlacementIntent {
///         asset_name: "oak".to_string(),
///         count: 4,
///         min_separation: 2.0,
///         anchor: None,
///     }],
/// };
/// let oak = "oak".to_string();
/// let layout = plan(&intent, &[oak.clone()]).unwrap();
/// assert_eq!(layout.items.len(), 4);
/// // 同种子同输入重跑，逐位一致。
/// let again = plan(&intent, &[oak]).unwrap();
/// assert_eq!(layout.items, again.items);
/// ```
///
/// # Errors
///
/// - [`PlannerError::AssetUnavailable`]：意图引用了名单外的资产（一次报全）。
/// - [`PlannerError::Infeasible`]：某意图的散布件在尝试上限内布不下，或
///   锚点/间距参数非有限。
pub fn plan(intent: &SceneIntent, available: &[String]) -> Result<SceneLayout, PlannerError> {
    // ① 资产可用性：缺失名单一次报全（按首次出现顺序、去重）。与件数无
    // 关——引用了未知资产的意图本身就是坏的，哪怕 count = 0。
    let mut missing: Vec<String> = Vec::new();
    for placement in &intent.placements {
        let known = available.iter().any(|a| a == &placement.asset_name);
        if !known && !missing.iter().any(|m| m == &placement.asset_name) {
            missing.push(placement.asset_name.clone());
        }
    }
    if !missing.is_empty() {
        return Err(PlannerError::AssetUnavailable(missing));
    }

    // ② 锚点先行：按 intent 序精确落位并占住位置——后续所有散布件（包括
    // 更早 intent 的）都必须让开全部锚点。锚点件不消耗随机流。
    let mut placed: Vec<Vec3> = Vec::with_capacity(intent.placements.len());
    let mut anchors: Vec<Option<Transform>> = Vec::with_capacity(intent.placements.len());
    for (index, placement) in intent.placements.iter().enumerate() {
        let anchor = match &placement.anchor {
            Some(_) if placement.count == 0 => None,
            Some((pos, yaw_deg)) => {
                if ![pos[0], pos[1], pos[2], *yaw_deg]
                    .iter()
                    .all(|v| v.is_finite())
                {
                    return Err(PlannerError::Infeasible {
                        intent_index: index,
                        reason: format!("锚点参数非有限：pos={pos:?}，yaw_deg={yaw_deg}"),
                    });
                }
                let transform = Transform {
                    translation: Vec3::from_array(*pos),
                    rotation: Quat::from_rotation_y(yaw_deg.to_radians()),
                    scale: Vec3::ONE,
                };
                placed.push(transform.translation);
                Some(transform)
            }
            None => None,
        };
        anchors.push(anchor);
    }

    // ③ 散布：按 intent 序 × 件内序；先推锚点件（该意图第 0 件），再推
    // 散布件。随机流只有一个，按同样的顺序推进——同输入必同布局。
    let mut rng = Lcg::new(intent.seed);
    let mut items: Vec<PlacedItem> = Vec::new();
    for (index, placement) in intent.placements.iter().enumerate() {
        if let Some(transform) = anchors[index] {
            items.push(PlacedItem {
                asset_name: placement.asset_name.clone(),
                transform,
            });
        }
        let scattered = match &placement.anchor {
            Some(_) => placement.count.saturating_sub(1),
            None => placement.count,
        };
        for k in 0..scattered {
            let transform = scatter(
                index,
                k as usize,
                scattered as usize,
                placement.min_separation,
                intent.area_half,
                &placed,
                &mut rng,
            )?;
            placed.push(transform.translation);
            items.push(PlacedItem {
                asset_name: placement.asset_name.clone(),
                transform,
            });
        }
    }
    Ok(SceneLayout { items })
}

/// 给第 `intent_index` 个意图摆第 `k`（共 `n`）件散布件。
///
/// 首选候选来自覆盖场地的近方形抖动网格（格距 ≥ 间距时，同意图网格对天
/// 然满足间距，只在撞上锚点/其他意图已摆件时才走重试）；重试为全场均匀
/// 采样。每个候选对 `placed` 全量做水平间距检查；`MAX_ATTEMPTS` 次用尽
/// 即诚实报不可满足。
fn scatter(
    intent_index: usize,
    k: usize,
    n: usize,
    min_separation: f32,
    area_half: f32,
    placed: &[Vec3],
    rng: &mut Lcg,
) -> Result<Transform, PlannerError> {
    if !area_half.is_finite() || area_half < 0.0 {
        return Err(PlannerError::Infeasible {
            intent_index,
            reason: format!("area_half={area_half} 非有限或为负，散布没有场地"),
        });
    }
    if !min_separation.is_finite() {
        return Err(PlannerError::Infeasible {
            intent_index,
            reason: format!("min_separation={min_separation} 非有限，任何候选都会被拒"),
        });
    }
    // 负间距视为无约束（先判非有限再钳制：f32::max 会吞掉 NaN）。
    let sep = min_separation.max(0.0);
    let sep_sq = sep * sep;

    // 近方形网格最大化最小格距；格距 ≥ sep 时把抖动钳到 (格距 - sep)/2，
    // 相邻格候选的最坏距离仍有 sep。
    let nx = ((n as f32).sqrt().ceil() as usize).max(1);
    let ny = n.div_ceil(nx).max(1);
    let cell_x = 2.0 * area_half / nx as f32;
    let cell_z = 2.0 * area_half / ny as f32;
    let jitter_x = ((cell_x - sep) * 0.5).max(0.0);
    let jitter_z = ((cell_z - sep) * 0.5).max(0.0);
    let base_x = -area_half + (k % nx) as f32 * cell_x + cell_x * 0.5;
    let base_z = -area_half + (k / nx) as f32 * cell_z + cell_z * 0.5;

    for attempt in 0..MAX_ATTEMPTS {
        let (x, z) = if attempt == 0 {
            (
                base_x + rng.next_sym(jitter_x),
                base_z + rng.next_sym(jitter_z),
            )
        } else {
            (rng.next_sym(area_half), rng.next_sym(area_half))
        };
        if is_clear(x, z, sep_sq, placed) {
            let yaw_deg = rng.next_f32() * 360.0;
            return Ok(Transform {
                translation: Vec3::new(x, 0.0, z),
                rotation: Quat::from_rotation_y(yaw_deg.to_radians()),
                scale: Vec3::ONE,
            });
        }
    }
    Err(PlannerError::Infeasible {
        intent_index,
        reason: format!(
            "散布第 {k}/{n} 件：{MAX_ATTEMPTS} 次尝试内未找到与已摆 {} 件水平距离均 ≥ {sep} 的位置（场地半边 {area_half}，网格 {nx}×{ny}）",
            placed.len()
        ),
    })
}

/// 候选点 (x, z) 与全部已摆件的平方水平距离是否都 ≥ `sep_sq`。
/// O(|placed|) 一次；整个布局是 O(n²) 全对检查。NaN 会使比较为假——
/// 候选被拒，最终走向 Infeasible，不会把坏值摆进场景。
fn is_clear(x: f32, z: f32, sep_sq: f32, placed: &[Vec3]) -> bool {
    placed.iter().all(|p| {
        let dx = p.x - x;
        let dz = p.z - z;
        dx * dx + dz * dz >= sep_sq
    })
}

/// 确定性随机流：Knuth MMIX 常数 LCG（乘数 6364136223846793005、增量
/// 1442695040888963407），与 `oriera_infer` 的 spawn RNG 同款；初始态与
/// 仓库种子常数 [`Lcg::GOLDEN`]（黄金比例，`oriera_infer` 的默认种子）异
/// 或，避免 seed = 0 直通低质量低位。自写而不用 `DefaultHasher`：它的
/// 内部算法跨 Rust 版本可能变化，会破坏逐位复现。
struct Lcg(u64);

impl Lcg {
    /// 黄金比例常数（仓库 seed 风格，0x9E37 系）。
    const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

    fn new(seed: u64) -> Self {
        Lcg(seed ^ Self::GOLDEN)
    }

    /// 推进并返回原始 64 位状态。
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }

    /// [0, 1) 均匀 f32：取高 24 位（LCG 低位周期短，不可用）。
    fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 * (1.0 / (1u64 << 24) as f32)
    }

    /// `[-radius, radius]` 均匀 f32。
    fn next_sym(&mut self, radius: f32) -> f32 {
        (self.next_f32() * 2.0 - 1.0) * radius
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    fn placement(
        asset: &str,
        count: u32,
        sep: f32,
        anchor: Option<([f32; 3], f32)>,
    ) -> PlacementIntent {
        PlacementIntent {
            asset_name: asset.to_string(),
            count,
            min_separation: sep,
            anchor,
        }
    }

    /// 两件之间的水平距离平方（与 `is_clear` 同一算式，逐位可比）。
    fn horizontal_sq(a: &Transform, b: &Transform) -> f32 {
        let dx = a.translation.x - b.translation.x;
        let dz = a.translation.z - b.translation.z;
        dx * dx + dz * dz
    }

    #[test]
    fn anchor_lands_exactly() {
        let intent = SceneIntent {
            area_half: 10.0,
            seed: 7,
            placements: vec![placement("hearth", 3, 2.0, Some(([1.5, 0.25, -2.5], 45.0)))],
        };
        let layout = plan(&intent, &available(&["hearth"])).unwrap();
        assert_eq!(layout.items.len(), 3, "锚点 1 件 + 散布 2 件");
        let first = &layout.items[0];
        assert_eq!(first.asset_name, "hearth");
        let t = &first.transform;
        assert_eq!(
            t.translation,
            Vec3::new(1.5, 0.25, -2.5),
            "锚点精确落位（含调用方给的高程）"
        );
        assert_eq!(
            t.rotation,
            Quat::from_rotation_y(45.0f32.to_radians()),
            "yaw 用指定值"
        );
        assert_eq!(t.scale, Vec3::ONE);
        for item in &layout.items[1..] {
            assert_eq!(item.transform.translation.y, 0.0, "散布件贴平地");
        }
    }

    #[test]
    fn anchor_only_count_places_one_item() {
        let intent = SceneIntent {
            area_half: 10.0,
            seed: 1,
            placements: vec![placement("gate", 1, 2.0, Some(([0.0, 0.0, 8.0], 180.0)))],
        };
        let layout = plan(&intent, &available(&["gate"])).unwrap();
        assert_eq!(layout.items.len(), 1);
        assert_eq!(
            layout.items[0].transform.translation,
            Vec3::new(0.0, 0.0, 8.0)
        );
        assert_eq!(
            layout.items[0].transform.rotation,
            Quat::from_rotation_y(180.0f32.to_radians())
        );
    }

    #[test]
    fn scattered_pairs_respect_min_separation() {
        let intent = SceneIntent {
            area_half: 20.0,
            seed: 0xA11CE,
            placements: vec![
                placement("tree", 6, 3.0, None),
                placement("rock", 5, 3.0, None),
                placement("bush", 4, 3.0, Some(([5.0, 0.0, -5.0], 0.0))),
            ],
        };
        let layout = plan(&intent, &available(&["tree", "rock", "bush"])).unwrap();
        assert_eq!(layout.items.len(), 15);
        // O(n²) 全对断言：只有一件锚点，任何一对都含散布件——散布件摆入
        // 前对全部已摆件做过检查，所以每一对都必须满足。
        for (i, a) in layout.items.iter().enumerate() {
            for b in &layout.items[i + 1..] {
                let d2 = horizontal_sq(&a.transform, &b.transform);
                assert!(
                    d2 >= 9.0,
                    "间距违例：{}@{:?} 与 {}@{:?} 距离² {d2} < 9",
                    a.asset_name,
                    a.transform.translation,
                    b.asset_name,
                    b.transform.translation
                );
            }
        }
        for item in &layout.items {
            let t = &item.transform;
            assert!(
                t.translation.x.abs() <= 20.0 && t.translation.z.abs() <= 20.0,
                "散布件不越界"
            );
            assert_eq!(t.translation.y, 0.0);
            assert_eq!(t.scale, Vec3::ONE);
            assert!(t.rotation.is_normalized(), "绕 y 单位四元数");
        }
    }

    #[test]
    fn missing_assets_reported_completely() {
        let intent = SceneIntent {
            area_half: 10.0,
            seed: 1,
            placements: vec![
                placement("tree", 2, 1.0, None),
                placement("dragon_statue", 1, 1.0, None),
                placement("bush", 1, 1.0, None),
                placement("dragon_statue", 1, 1.0, None),
            ],
        };
        match plan(&intent, &available(&["tree"])) {
            Err(PlannerError::AssetUnavailable(missing)) => assert_eq!(
                missing,
                vec!["dragon_statue".to_string(), "bush".to_string()],
                "缺失名单一次报全、按首现顺序、去重"
            ),
            other => panic!("期望 AssetUnavailable，得到 {other:?}"),
        }
    }

    #[test]
    fn infeasible_when_separation_exceeds_area() {
        // 场地对角线约 28.3 < 100：第二件无论怎么撒都够不到 100 间距。
        let intent = SceneIntent {
            area_half: 10.0,
            seed: 9,
            placements: vec![
                placement("tree", 2, 1.0, None),
                placement("lighthouse", 2, 100.0, None),
            ],
        };
        let err = plan(&intent, &available(&["tree", "lighthouse"])).unwrap_err();
        match &err {
            PlannerError::Infeasible {
                intent_index,
                reason,
            } => {
                assert_eq!(*intent_index, 1, "报第一个布不下的 intent 下标");
                assert!(!reason.is_empty(), "带人读原因");
            }
            other => panic!("期望 Infeasible，得到 {other:?}"),
        }
        assert!(err.to_string().contains('1'), "Display 里带 intent 下标");
    }

    #[test]
    fn infeasible_when_area_too_crowded() {
        // 4×4 场地摆 24 件中心距 ≥ 1：原则上可行（5×5 网格恰好 25 点），
        // 但网格首选格距 0.8 < 1、64 次均匀重试也难命中近点阵的排布——
        // 诚实报 Infeasible。这是"尽力布局器"的已知边界（见模块文档）。
        let intent = SceneIntent {
            area_half: 2.0,
            seed: 11,
            placements: vec![placement("chair", 24, 1.0, None)],
        };
        assert!(matches!(
            plan(&intent, &available(&["chair"])),
            Err(PlannerError::Infeasible {
                intent_index: 0,
                ..
            })
        ));
    }

    #[test]
    fn double_run_is_bit_identical() {
        let intent = SceneIntent {
            area_half: 15.0,
            seed: 0x5EED,
            placements: vec![
                placement("table", 3, 2.0, Some(([-4.0, 0.0, 3.0], 30.0))),
                placement("tree", 8, 2.5, None),
                placement("rock", 6, 1.0, None),
            ],
        };
        let avail = available(&["table", "tree", "rock"]);
        let a = plan(&intent, &avail).unwrap();
        let b = plan(&intent, &avail).unwrap();
        assert_eq!(a, b, "同种子同输入双跑逐位一致");
        // serde 视角也逐位一致（Transform 是精确 f32 比较，JSON 文本相等
        // 排除任何重排/浮点重解释）。
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn different_seeds_diverge() {
        let make = |seed: u64| SceneIntent {
            area_half: 12.0,
            seed,
            placements: vec![
                placement("tree", 8, 2.0, None),
                placement("rock", 8, 2.0, None),
            ],
        };
        let a = plan(&make(1), &available(&["tree", "rock"])).unwrap();
        let b = plan(&make(2), &available(&["tree", "rock"])).unwrap();
        assert_ne!(a, b, "异种子布局不同（锚点才与种子无关）");
    }

    #[test]
    fn empty_intent_yields_empty_layout() {
        let intent = SceneIntent {
            area_half: 10.0,
            seed: 3,
            placements: vec![],
        };
        assert!(plan(&intent, &[]).unwrap().items.is_empty());

        // count = 0 不产生件；带锚点也不占位。
        let intent = SceneIntent {
            area_half: 10.0,
            seed: 3,
            placements: vec![
                placement("tree", 0, 1.0, Some(([1.0, 0.0, 1.0], 0.0))),
                placement("rock", 0, 1.0, None),
            ],
        };
        let layout = plan(&intent, &available(&["tree", "rock"])).unwrap();
        assert!(layout.items.is_empty());
    }

    #[test]
    fn layout_serde_round_trip() {
        let intent = SceneIntent {
            area_half: 10.0,
            seed: 77,
            placements: vec![
                placement("stall", 2, 2.0, Some(([0.0, 0.0, 0.0], 90.0))),
                placement("crate", 5, 1.5, None),
            ],
        };
        let layout = plan(&intent, &available(&["stall", "crate"])).unwrap();
        let json = serde_json::to_string(&layout).unwrap();
        let back: SceneLayout = serde_json::from_str(&json).unwrap();
        assert_eq!(back, layout, "布局可 serde 往返");

        // 意图本身也要能进场景 spec（"实验即文件"）。
        let intent_json = serde_json::to_string(&intent).unwrap();
        let intent_back: SceneIntent = serde_json::from_str(&intent_json).unwrap();
        assert_eq!(intent_back, intent);
    }

    #[test]
    fn item_order_follows_intent_then_item_index() {
        let intent = SceneIntent {
            area_half: 10.0,
            seed: 5,
            placements: vec![
                placement("tree", 2, 2.0, None),
                placement("bench", 3, 1.5, Some(([3.0, 0.0, 3.0], 12.0))),
            ],
        };
        let layout = plan(&intent, &available(&["tree", "bench"])).unwrap();
        let names: Vec<&str> = layout
            .items
            .iter()
            .map(|item| item.asset_name.as_str())
            .collect();
        assert_eq!(names, vec!["tree", "tree", "bench", "bench", "bench"]);
        assert_eq!(
            layout.items[2].transform.translation,
            Vec3::new(3.0, 0.0, 3.0),
            "锚点件是该意图的第 0 件"
        );
    }

    #[test]
    fn nonfinite_anchor_is_rejected() {
        let intent = SceneIntent {
            area_half: 10.0,
            seed: 1,
            placements: vec![placement(
                "statue",
                1,
                1.0,
                Some(([f32::NAN, 0.0, 0.0], 0.0)),
            )],
        };
        assert!(matches!(
            plan(&intent, &available(&["statue"])),
            Err(PlannerError::Infeasible {
                intent_index: 0,
                ..
            })
        ));
    }

    #[test]
    fn error_messages_are_human_readable() {
        let missing =
            PlannerError::AssetUnavailable(vec!["dragon".to_string(), "bridge".to_string()]);
        let msg = missing.to_string();
        assert!(msg.contains("dragon") && msg.contains("bridge") && msg.contains('2'));
        let infeasible = PlannerError::Infeasible {
            intent_index: 2,
            reason: "64 次尝试用尽".to_string(),
        };
        let msg = infeasible.to_string();
        assert!(msg.contains('2') && msg.contains("64"));
    }
}
