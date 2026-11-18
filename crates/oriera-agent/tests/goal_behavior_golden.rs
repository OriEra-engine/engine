//! P0-5「确定性验证扩面」AI 行为金样（战略路线图 02-STATE §2）。
//!
//! 渲染与物理两侧金样已入 CI；本测试把同一纪律推广到 **AI 决策层**：
//! [`GoalSystem`](oriera_agent::goal::GoalSystem) 是 NPC 目标规划的
//! 确定性内核（每拍评估成功条件 / 截止受挫，受挫写入记忆形成情绪
//! 动力学闭环）。金样锁定的是一条脚本化变量时间线下的**决策轨迹**：
//! 何时完成、何时受挫、记忆记账多少——重构目标评估器或故事表达式
//! 时，任何行为漂移都会在这里现形。
//!
//! - 同构建位确定性：脚本时间线连跑两次，事件日志逐字节相等；
//! - 金样比对：事件日志文本与入库金样逐行相等（纯逻辑层，无浮点，
//!   不需要容差）。
//!
//! 重锚（仅在有意改变目标语义时）：
//! `ORIERA_AGENT_BLESS=1 cargo test -p oriera-agent --test goal_behavior_golden`

use oriera_agent::goal::{Goal, GoalLog, GoalSystem, GoalVars};
use oriera_agent::memory::MemoryStore;
use oriera_common::{FrameIndex, Grants, Timestamp};
use oriera_core::{FrameCtx, System};
use oriera_ecs::World;

const TURNS: u64 = 60;

fn ctx(turn: u64) -> FrameCtx<'static> {
    FrameCtx {
        frame: FrameIndex(turn),
        dt: 1.0 / 60.0,
        now: Timestamp(0),
        grants: Grants::EMPTY,
    }
}

/// 三目标剧本（覆盖完成 / 受挫 / 双达成时序交错三条行为路径）：
/// - `g_coins`  亲和无关的收集目标，t=20 达成（`coins = t/5`）；
/// - `g_reach`  亲和目标，t=30 达成（`affinity = t/10`）；
/// - `g_hard`   永远差一步的目标，deadline=30 → t=31 受挫一次并入记忆。
fn build_world() -> World {
    let mut world = World::new();
    let mut log = GoalLog::default();
    log.add(Goal {
        id: "g_coins".into(),
        description: "收集四枚金币".into(),
        success_when: "coins >= 4".into(),
        priority: 1,
        deadline_turn: Some(100),
    });
    log.add(Goal {
        id: "g_reach".into(),
        description: "把亲和练到三".into(),
        success_when: "affinity >= 3".into(),
        priority: 2,
        deadline_turn: None,
    });
    log.add(Goal {
        id: "g_hard".into(),
        description: "把亲和练到九百九十九".into(),
        success_when: "affinity >= 999".into(),
        priority: 3,
        deadline_turn: Some(30),
    });
    world.insert_resource(log);
    world.insert_resource(GoalVars::default());
    world.insert_resource(MemoryStore::new());
    world
}

/// 跑一遍剧本，返回逐拍事件日志与末拍状态行（纯文本，确定序）。
fn run_script() -> Vec<String> {
    let mut world = build_world();
    let mut sys = GoalSystem::new();
    let mut events = Vec::new();
    for t in 1..=TURNS {
        {
            let vars = world.resource_mut::<GoalVars>().expect("vars inserted");
            vars.set_int("__turn", t as i64);
            vars.set_int("affinity", (t / 10) as i64);
            vars.set_int("coins", (t / 5) as i64);
        }
        sys.run(&ctx(t), &mut world);
        // 读取本拍新事件（GoalLog 已被 GoalSystem 更新）。
        let log = world.resource::<GoalLog>().expect("log inserted");
        for (id, done_t) in &log.completed {
            if *done_t == t {
                events.push(format!("t={t} done {id}"));
            }
        }
        for (id, fr_t) in &log.frustrated {
            if *fr_t == t {
                events.push(format!("t={t} frustrated {id}"));
            }
        }
    }
    let log = world.resource::<GoalLog>().expect("log inserted");
    let mem = world.resource::<MemoryStore>().expect("mem inserted");
    events.sort();
    events.dedup();
    events.push(format!(
        "final completed={} frustrated={} memory={}",
        log.completed.len(),
        log.frustrated.len(),
        mem.len_total()
    ));
    events
}

#[test]
fn goal_behavior_is_deterministic_and_matches_golden() {
    let events = run_script();

    // 覆盖率哨兵：剧本必须真走过三条行为路径，金样不许是空集。
    assert!(
        events.iter().any(|e| e.contains("done g_coins")),
        "scenario never completed g_coins"
    );
    assert!(
        events.iter().any(|e| e.contains("done g_reach")),
        "scenario never completed g_reach"
    );
    assert!(
        events.iter().any(|e| e.contains("frustrated g_hard")),
        "scenario never frustrated g_hard"
    );

    // 同构建位确定性：第二次独立运行日志逐字节一致。
    let events2 = run_script();
    assert_eq!(
        events, events2,
        "identical script produced different behavior"
    );

    // 金样比对。
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/goal_behavior_60t.txt");
    if std::env::var_os("ORIERA_AGENT_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut buf = String::from(
            "# oriera-agent AI 行为金样：GoalSystem 三目标剧本 60 拍（完成/受挫/记忆记账）\n",
        );
        for e in &events {
            buf.push_str(e);
            buf.push('\n');
        }
        std::fs::write(&path, buf).unwrap();
    }
    let golden = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {} ({e}); bless with ORIERA_AGENT_BLESS=1",
            path.display()
        )
    });
    let want: Vec<&str> = golden
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .collect();
    let got: Vec<String> = events;
    assert_eq!(
        want.len(),
        got.len(),
        "golden line count {} vs run {} —\ngolden:\n{}\nrun:\n{}",
        want.len(),
        got.len(),
        want.join("\n"),
        got.join("\n")
    );
    for (w, g) in want.iter().zip(&got) {
        assert_eq!(*w, g, "behavior drift: golden '{w}' vs run '{g}'");
    }
}
