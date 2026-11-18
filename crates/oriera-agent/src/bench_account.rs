//! P0-5 headless 验证回路的**三段分账**（BENCH-CONTRACT 附 A，方案 A
//! 最小竖切，覆盖缺口 G5/G6/G7-部分；设计输入纪要
//! `out/roadmap-expert-panel/w2-pre/p05-headless-loop-design.md` §7）。
//!
//! 本模块提供三样东西：
//!
//! * **分账结构（G6）**——[`SegmentAccount`]（单段一行：起点/终点/
//!   毫秒/段名/环境指纹/终态指纹）与 [`HeadlessRunAccount`]（一次回路
//!   的整本账：平台标签 + 环境指纹 + LLM/构建/仿真/渲染四段槽位），
//!   serde 序列化，`headless_loop` example 经 `json` feature 打印。
//! * **构建完成事件（G5）**——契约附 A 把「构建段」定义为「构建命令
//!   发出 → 构建完成事件」，而仓内 World 只是普通 Rust 构造、无回执
//!   可挂（G5 缺口）。本模块把回执落成账面事实：[`SegmentAccount::close`]
//!   在构建边界打时间戳并把构建产物的 [`World::state_hash`] 作为
//!   `state_fingerprint` 收进构建段——这就是「构建完成事件」的凭证，
//!   双跑逐位一致由单元测试锁定（物理金样同款纪律）。
//! * **回路本体（G7-部分）**——[`run_headless_loop`]：固定场景 →
//!   固定步数物理仿真（`Scheduler::with_fixed_dt`，#66 确定性路径）→
//!   分账返回。渲染段占位 `null`（World→ScenePart 桥是 G1，另行）；
//!   LLM 段只留分账接口槽位（真实 provider 计时包装是 G4，另行）。
//!
//! 计时口径（契约第 1 条）：本竖切用**进程侧墙钟**（`Instant` 差值 +
//! `Timestamp` 墙钟端点），属 fallback 口径——`timing_scope` 字段随身
//! 标注「含 host 开销、数字虚高」，报表消费方据此不把它当 profiler
//! Computing 口径。

use std::time::{Duration, Instant};

use oriera_common::Timestamp;
use oriera_core::Scheduler;
use oriera_ecs::{Position, Velocity, World};
use oriera_subsys::{Gravity, PhysicsSystem, TimeScale};
use serde::{Deserialize, Serialize};

/// 段名：LLM 段（brief 发出 → 最后一个 token/工具调用返回；本竖切
/// 只留接口槽位，真实计时是 G4）。
pub const SEG_LLM: &str = "llm";
/// 段名：引擎构建段（构建命令发出 → 构建完成事件，G5 回执凭证）。
pub const SEG_BUILD: &str = "build";
/// 段名：仿真段（固定步数物理；契约引擎侧「构建段」的更细分账）。
pub const SEG_SIM: &str = "sim";
/// 段名：渲染验证段（headless 渲染启动 → 截图+断言判定返回；占位）。
pub const SEG_RENDER: &str = "render";

/// 计时口径标注（契约第 1 条 fallback：墙钟含 host 开销，数字虚高）。
const TIMING_SCOPE: &str =
    "process-side wall clock (includes host overhead; BENCH-CONTRACT rule 1 fallback)";

/// 固定场景粒子数（studio_e2e 孪生场景同款「spawn 25 particles」）。
pub const SCENARIO_PARTICLES: usize = 25;
/// 固定仿真步数（物理金样两阶段总拍数 240 同量级）。
pub const SCENARIO_TICKS: u64 = 240;
/// 固定仿真步长（1/60 s，物理金样与 studio_e2e 同款）。
pub const SCENARIO_DT: f32 = 1.0 / 60.0;

/// 单段分账记录（BENCH-CONTRACT 附 A 的一行，G6）。
///
/// `start`/`end` 是 unix 毫秒墙钟端点；`ms` 是进程侧墙钟耗时；两套都
/// 是 fallback 口径（见模块头 `timing_scope`）。`state_fingerprint` 是
/// **段终点时刻的确定性世界指纹**（[`World::state_hash`]）：构建段即
/// 构建完成回执（G5），仿真段即终态——双跑/同构建位逐位一致的比对
/// 目标；墙钟字段不参与确定性比对。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentAccount {
    /// 段名（[`SEG_BUILD`] / [`SEG_SIM`] / [`SEG_LLM`] / [`SEG_RENDER`]）。
    pub segment: String,
    /// 起点（unix 毫秒；契约附 A「起点」）。
    pub start: u64,
    /// 终点（unix 毫秒；契约附 A「终点」——构建段即构建完成事件时刻）。
    pub end: u64,
    /// 段耗时（毫秒，进程侧墙钟）。
    pub ms: f64,
    /// 环境指纹（[`env_fingerprint`]）：随段入账，供契约第 2 条
    /// 「参考与被测同计时同环境」的事后审计——同进程双跑逐位一致。
    pub env_fingerprint: u64,
    /// 段终态确定性世界指纹（构建回执 / 仿真终态；G5 双跑比对目标）。
    pub state_fingerprint: u64,
}

impl SegmentAccount {
    /// 占位段（全零）——仅供开账；正式分账一律经 [`SegmentAccount::close`]。
    fn placeholder(segment: &'static str) -> Self {
        Self {
            segment: segment.to_string(),
            start: 0,
            end: 0,
            ms: 0.0,
            env_fingerprint: 0,
            state_fingerprint: 0,
        }
    }

    /// 在段边界收账：起点取传入的墙钟端点，耗时取进程内秒表差值
    /// （比墙钟端点差更准），终点时刻由本调用即时打戳——构建段的
    /// 这个即时打戳就是 G5 的「构建完成事件」。
    pub fn close(
        segment: &'static str,
        started_wall: Timestamp,
        elapsed: Duration,
        state_fingerprint: u64,
    ) -> Self {
        Self {
            segment: segment.to_string(),
            start: started_wall.0 / 1_000_000, // unix ns → unix ms
            end: Timestamp::now().0 / 1_000_000,
            ms: elapsed.as_secs_f64() * 1000.0,
            env_fingerprint: env_fingerprint(),
            state_fingerprint,
        }
    }
}

/// 一次 headless 回路跑的整本分账（G6 输出物）。
///
/// 契约附 A 的三段在这里各占一槽：`build`+`sim` 是引擎侧账（本竖切
/// 把「构建段」再拆出固定步物理的 `sim` 细账），`render` 本竖切占位
/// `null`（G1 桥与像素断言另行），`llm` 只留接口槽位（G4 另行）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadlessRunAccount {
    /// 平台标签（`os-arch`，物理金样 `platform` 行同款格式）。
    pub platform: String,
    /// 环境指纹（[`env_fingerprint`]）。
    pub env_fingerprint: u64,
    /// 计时口径标注（契约第 1 条 fallback 必须随身标注）。
    pub timing_scope: String,
    /// LLM 段：本竖切占位 `null`——分账接口槽位已留好，G4 接真实
    /// provider 计时包装后填入。
    pub llm: Option<SegmentAccount>,
    /// 引擎构建段（G5：构建完成事件已打时间戳 + 回执指纹入账）。
    pub build: SegmentAccount,
    /// 仿真段（固定步数物理，#66 `with_fixed_dt` 确定性路径）。
    pub sim: SegmentAccount,
    /// 渲染验证段：本竖切占位 `null`——World→ScenePart 桥（G1）与
    /// 截图+断言判定另行。
    pub render: Option<SegmentAccount>,
}

impl HeadlessRunAccount {
    /// 开一本空账：平台标签 + 环境指纹入账；四段槽位待各段 `close`。
    pub fn new() -> Self {
        Self {
            platform: platform_label(),
            env_fingerprint: env_fingerprint(),
            timing_scope: TIMING_SCOPE.to_string(),
            llm: None,
            build: SegmentAccount::placeholder(SEG_BUILD),
            sim: SegmentAccount::placeholder(SEG_SIM),
            render: None,
        }
    }
}

impl Default for HeadlessRunAccount {
    fn default() -> Self {
        Self::new()
    }
}

/// 平台标签：`{os}-{arch}`（物理金样 `platform` 行同款，跨平台比对
/// 时据此分平台——rapier/积分的跨平台逐位差异由金样层另行容差）。
pub fn platform_label() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// 环境指纹：宿主环境位（os/arch/family/指针宽/断言档）+ 引擎版本 的
/// FNV-1a。纯环境函数——同进程双跑逐位一致；跨环境对拍可据此判
/// 「参考与被测是否同环境」（契约第 2 条）。种子与
/// [`oriera_ecs::World::state_hash`] 同源（"oriera" ASCII）。
pub fn env_fingerprint() -> u64 {
    let mut h: u64 = 0x0000_6f72_6965_7261;
    fn mix(h: &mut u64, s: &str) {
        for b in s.bytes() {
            *h ^= u64::from(b);
            *h = h.wrapping_mul(0x1000_0000_01b3);
        }
        *h ^= 0x1f; // 域分隔，防串接歧义
        *h = h.wrapping_mul(0x1000_0000_01b3);
    }
    mix(&mut h, std::env::consts::OS);
    mix(&mut h, std::env::consts::ARCH);
    mix(&mut h, std::env::consts::FAMILY);
    mix(
        &mut h,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    );
    mix(&mut h, env!("CARGO_PKG_VERSION"));
    h
}

/// 固定场景：5×5 粒子阵列 + 重力/时标资源。全部量由拍号闭式派生，
/// 无随机源——双跑与跨机逐位复现的前提（契约第 4 条固定种子精神）。
fn build_fixed_world() -> World {
    let mut world = World::new();
    world.insert_resource(Gravity(9.8));
    world.insert_resource(TimeScale(1.0));
    for i in 0..SCENARIO_PARTICLES {
        let (fx, fy) = ((i % 5) as f32, (i / 5) as f32);
        let e = world.spawn();
        world.insert(
            e,
            Position {
                x: 40.0 + fx * 7.0,
                y: 60.0 + fy * 3.0,
            },
        );
        world.insert(
            e,
            Velocity {
                vx: 1.5 + fx * 0.1,
                vy: -0.25 - fy * 0.2,
            },
        );
    }
    world
}

/// 固定回路调度器：60fps 预算 + 固定步长（#66——物理积分与墙钟解耦，
/// 同命令流双跑逐位一致）。
fn build_fixed_scheduler() -> Scheduler<World> {
    let mut sched = Scheduler::with_budget_ns(16_666_666).with_fixed_dt(SCENARIO_DT);
    sched.add_system(Box::new(PhysicsSystem::new(800.0, 600.0)));
    sched
}

/// 跑一次 headless 回路：固定场景 → 固定步数物理仿真 → 整本分账
/// （G7-部分）。渲染段/LLM 段保持占位 `null`（G1/G4 另行）。
///
/// 双跑同构建位确定性由调用方断言（见 `tests` 与 `headless_loop`
/// example——物理金样同款「两次独立运行逐位相等」纪律）。
pub fn run_headless_loop() -> HeadlessRunAccount {
    let mut account = HeadlessRunAccount::new();

    // LLM 段（G4 另行）：brief 发出 → 最后一个 token/工具调用返回，
    // 归模型侧账。本竖切离线零调用，接口槽位 [`HeadlessRunAccount::llm`]
    // 保持 `null`——接真实 provider 计时包装（G4）后在此收账。

    // ── 引擎构建段（契约附 A「构建命令发出 → 构建完成事件」，G5）──
    let build_started = Timestamp::now();
    let build_clock = Instant::now();
    let mut world = build_fixed_world();
    let mut sched = build_fixed_scheduler();
    // G5 构建完成事件：即刻打戳 + 构建产物指纹收进构建段。
    account.build = SegmentAccount::close(
        SEG_BUILD,
        build_started,
        build_clock.elapsed(),
        world.state_hash(),
    );

    // ── 仿真段：固定步数（契约引擎侧构建段的更细分账，G6 输出物）──
    let sim_started = Timestamp::now();
    let sim_clock = Instant::now();
    for _ in 0..SCENARIO_TICKS {
        sched.tick(&mut world);
    }
    account.sim = SegmentAccount::close(
        SEG_SIM,
        sim_started,
        sim_clock.elapsed(),
        world.state_hash(),
    );

    // ── 渲染验证段（契约附 A ≤30s 门禁对象）：占位 `null`——
    // World→ScenePart 桥（G1）+ HeadlessRenderer 出图 + 像素断言另行。
    account.render = None;

    account
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 竖切验收单元测试（ask 指定）：① 分账 JSON 结构可序列化（G6
    /// 字段清单齐全、往返无损、占位段为 null）；② 双跑构建段同构建
    /// 位一致（G5 构建回执/仿真终态逐位相等，墙钟字段允许漂移——
    /// 物理金样同款双层设计）。
    #[test]
    fn account_json_roundtrips_and_double_run_build_bits_match() {
        // ── 第一次独立运行 ──
        let a = run_headless_loop();

        // ① G6：序列化 → 解析往返无损，字段清单齐全。
        let json = serde_json::to_string_pretty(&a).expect("分账结构可序列化");
        let back: HeadlessRunAccount =
            serde_json::from_str(&json).expect("分账 JSON 可反序列化（往返）");
        assert_eq!(back, a, "分账 JSON 往返必须无损");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        for seg in ["build", "sim"] {
            let s = &v[seg];
            for field in [
                "segment",
                "start",
                "end",
                "ms",
                "env_fingerprint",
                "state_fingerprint",
            ] {
                assert!(
                    s.get(field).is_some(),
                    "{seg} 段缺字段 {field}（G6 字段清单）"
                );
            }
        }
        assert_eq!(v["build"]["segment"], "build");
        assert_eq!(v["sim"]["segment"], "sim");
        assert!(v["render"].is_null(), "渲染段占位必须为 null（G7-部分）");
        assert!(v["llm"].is_null(), "LLM 段本竖切为占位接口（G4 另行）");
        assert!(!v["platform"].as_str().unwrap().is_empty());
        // 墙钟合理性：非负耗时、端点有序。
        assert!(a.build.ms.is_finite() && a.build.ms >= 0.0);
        assert!(a.sim.ms.is_finite() && a.sim.ms >= 0.0);
        assert!(a.build.start <= a.build.end);
        assert!(a.sim.start <= a.sim.end);
        // 段内环境指纹与整本账一致（同进程同环境）。
        assert_eq!(a.build.env_fingerprint, a.env_fingerprint);

        // ② G5/G7-部分：第二次独立运行，确定性字段逐位一致。
        let b = run_headless_loop();
        assert_eq!(
            a.build.state_fingerprint, b.build.state_fingerprint,
            "构建完成回执双跑漂移——同一构建位必须逐位一致"
        );
        assert_eq!(
            a.sim.state_fingerprint, b.sim.state_fingerprint,
            "固定步仿真终态双跑漂移——#66 with_fixed_dt 契约被破坏"
        );
        assert_eq!(a.env_fingerprint, b.env_fingerprint);
        assert_eq!(a.platform, b.platform);
        assert_ne!(
            a.build.state_fingerprint, a.sim.state_fingerprint,
            "固定步仿真没有改变世界——反假执行哨兵（契约第 5 条精神）失效"
        );
    }
}
