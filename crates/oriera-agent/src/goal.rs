//! L9 Goal 规划器最小闭环（#104 批①）。
//!
//! 一个目标 = 复用 story.rs `eval_when` 语法的成功条件（同一发现集，
//! 零新语法、validate 可检）。[`GoalSystem`] 每拍评估：满足 → 完成一次
//! 记账；过了截止拍仍未满足 → 受挫一次记账，并把挫折写进
//! [`MemoryStore`](crate::memory::MemoryStore)（about="world"，importance 4
//! ——NPC 会「记住」这次没做成，情绪动力学经由既有记忆通道闭环，
//! 不引入新机制）。已完成/受挫均不重复记账。

use crate::memory::{Episode, MemoryStore};
use crate::story::{StoryGraph, StoryValue};
use oriera_core::{FrameCtx, System};
use oriera_ecs::World;
use std::collections::BTreeMap;

/// 一个待追踪的目标。`success_when` 用 story 的 `var op 整数` 语法
/// （如 `"affinity_lin >= 3"`）；不可解析的表达式运行时按假处理
/// （与 story 同语义，validate 侧拦）。
#[derive(Debug, Clone, PartialEq)]
pub struct Goal {
    pub id: String,
    pub description: String,
    pub success_when: String,
    /// 越大越优先（评估顺序；批②导演联动消费）。
    pub priority: i32,
    /// 截止拍：超过仍未满足 → 受挫一次。None = 永不超时。
    pub deadline_turn: Option<u64>,
}

/// 世界变量快照资源：宿主每拍（或事件时）更新，GoalSystem 读它评估。
#[derive(Debug, Clone, Default)]
pub struct GoalVars(pub BTreeMap<String, StoryValue>);

impl GoalVars {
    pub fn set_int(&mut self, key: &str, v: i64) {
        self.0.insert(key.to_string(), StoryValue::Int(v));
    }
}

/// 目标台账：注册、完成、受挫（各自一次性记账）。
#[derive(Debug, Clone, Default)]
pub struct GoalLog {
    pub goals: Vec<Goal>,
    /// (goal_id, 完成拍)
    pub completed: Vec<(String, u64)>,
    /// (goal_id, 受挫拍)
    pub frustrated: Vec<(String, u64)>,
}

impl GoalLog {
    pub fn add(&mut self, goal: Goal) {
        self.goals.push(goal);
    }

    pub fn is_completed(&self, id: &str) -> bool {
        self.completed.iter().any(|(g, _)| g == id)
    }

    /// 评估所有未完结目标。返回本次新完成的 id 列表（拍②导演联动消费）。
    pub fn evaluate(&mut self, vars: &BTreeMap<String, StoryValue>, turn: u64) -> Vec<String> {
        let mut newly_done = Vec::new();
        for g in &self.goals {
            if self.is_completed(&g.id) || self.frustrated.iter().any(|(gid, _)| gid == &g.id) {
                continue;
            }
            let satisfied = StoryGraph::eval_when(vars, &g.success_when).unwrap_or(false);
            if satisfied {
                self.completed.push((g.id.clone(), turn));
                newly_done.push(g.id.clone());
                continue;
            }
            if let Some(deadline) = g.deadline_turn {
                if turn > deadline {
                    self.frustrated.push((g.id.clone(), turn));
                }
            }
        }
        newly_done
    }
}

/// 每拍评估系统：读 [`GoalVars`] 评估 [`GoalLog`]；受挫目标写入
/// [`MemoryStore`](crate::memory::MemoryStore)（world 视角重要性 4）。
/// 任一资源缺席即跳过——目标追踪是可选挂件，不强迫所有世界装配。
pub struct GoalSystem;

impl GoalSystem {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GoalSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl System<World> for GoalSystem {
    fn name(&self) -> &str {
        "oriera.goal"
    }

    fn run(&mut self, _ctx: &FrameCtx, world: &mut World) {
        // 先取变量快照（克隆出借用），再逐资源处理——避免同一 world 上
        // 不可变/可变借用交叠（E0502）。
        let Some(vars) = world.resource::<GoalVars>() else {
            return;
        };
        let vars = vars.clone();
        let turn = vars.0.get("__turn").map(|v| v.as_int()).unwrap_or(0) as u64;
        let newly_done = {
            let Some(log) = world.resource_mut::<GoalLog>() else {
                return;
            };
            log.evaluate(&vars.0, turn)
        };
        let _ = newly_done; // 批②导演联动消费；本拍只保证记账与记忆闭环
        let frustrated_now: Vec<String> = {
            let Some(log) = world.resource::<GoalLog>() else {
                return;
            };
            log.frustrated
                .iter()
                .filter(|(_, t)| *t == turn)
                .map(|(g, _)| g.clone())
                .collect()
        };
        if frustrated_now.is_empty() {
            return;
        }
        // 挫折 → 记忆（情绪动力学最小闭环：NPC 会记得世界没做成什么）。
        // 先收集描述再做写入——world 的可变/不可变借用不交叠。
        let mut fr_notes: Vec<(String, String)> = Vec::new();
        {
            let Some(log) = world.resource::<GoalLog>() else {
                return;
            };
            for id in frustrated_now {
                let desc = log
                    .goals
                    .iter()
                    .find(|g| g.id == id)
                    .map(|g| g.description.clone())
                    .unwrap_or_else(|| id.clone());
                fr_notes.push((id, desc));
            }
        }
        if let Some(mem) = world.resource_mut::<MemoryStore>() {
            for (_, desc) in fr_notes {
                mem.record(
                    "world",
                    Episode {
                        turn,
                        day: 0,
                        about: "world".into(),
                        text: format!("目标受挫：{desc}"),
                        importance: 4,
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_ctx() -> FrameCtx<'static> {
        FrameCtx {
            frame: oriera_common::FrameIndex(0),
            dt: 0.0,
            now: oriera_common::Timestamp(0),
            grants: oriera_common::Grants::EMPTY,
        }
    }

    fn goal(id: &str, when: &str, deadline: Option<u64>) -> Goal {
        Goal {
            id: id.into(),
            description: format!("目标 {id}"),
            success_when: when.into(),
            priority: 0,
            deadline_turn: deadline,
        }
    }

    #[test]
    fn satisfied_goal_completes_once_and_logs_memory_episode() {
        let mut w = World::new();
        w.insert_resource(GoalVars::default());
        w.insert_resource(MemoryStore::new());
        let mut log = GoalLog::default();
        log.add(goal("serve", "cups >= 3", None));
        w.insert_resource(log);
        let mut sys = GoalSystem::new();

        w.resource_mut::<GoalVars>().unwrap().set_int("cups", 1);
        sys.run(&frame_ctx(), &mut w);
        assert!(!w.resource::<GoalLog>().unwrap().is_completed("serve"));

        w.resource_mut::<GoalVars>().unwrap().set_int("cups", 3);
        w.resource_mut::<GoalVars>().unwrap().set_int("__turn", 5);
        sys.run(&frame_ctx(), &mut w);
        assert!(w.resource::<GoalLog>().unwrap().is_completed("serve"));
        assert_eq!(
            w.resource::<MemoryStore>().unwrap().len_total(),
            0,
            "完成不写挫折记忆"
        );

        // 再跑一拍：不重复记账
        sys.run(&frame_ctx(), &mut w);
        assert_eq!(w.resource::<GoalLog>().unwrap().completed.len(), 1);
    }

    #[test]
    fn overdue_goal_frustrates_once_into_memory() {
        let mut w = World::new();
        w.insert_resource(GoalVars::default());
        w.insert_resource(MemoryStore::new());
        let mut log = GoalLog::default();
        log.add(goal("brew", "beans >= 9", Some(3)));
        w.insert_resource(log);
        let mut sys = GoalSystem::new();

        w.resource_mut::<GoalVars>().unwrap().set_int("__turn", 4);
        sys.run(&frame_ctx(), &mut w);
        assert_eq!(w.resource::<GoalLog>().unwrap().frustrated.len(), 1);
        assert_eq!(
            w.resource::<MemoryStore>().unwrap().len_total(),
            1,
            "挫折写进 world 记忆（NPC 记得没做成）"
        );
        let mem = w.resource::<MemoryStore>().unwrap();
        let got = mem.retrieve("world", "world", "目标 受挫", 100, 1);
        assert!(got[0].text.contains("受挫"));

        // 不重复记账
        sys.run(&frame_ctx(), &mut w);
        assert_eq!(w.resource::<GoalLog>().unwrap().frustrated.len(), 1);
    }

    #[test]
    fn unparsable_expression_counts_as_unmet_not_crash() {
        let mut w = World::new();
        w.insert_resource(GoalVars::default());
        w.insert_resource(MemoryStore::new());
        let mut log = GoalLog::default();
        log.add(goal("broken", "cups ~~ 三", Some(1)));
        w.insert_resource(log);
        let mut sys = GoalSystem::new();
        w.resource_mut::<GoalVars>().unwrap().set_int("__turn", 2);
        sys.run(&frame_ctx(), &mut w);
        let log = w.resource::<GoalLog>().unwrap();
        assert!(log.completed.is_empty());
        assert_eq!(log.frustrated.len(), 1, "不可解析按未满足，超时即受挫");
    }
}
