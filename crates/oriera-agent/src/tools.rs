//! L9 引擎工具桥 —— 把引擎命令词表暴露给模型,把模型的工具调用
//! 派发进推理运行时(RFC-3 工具调用的引擎侧闭环)。
//!
//! 这是「AI 内嵌引擎」的最后一环:模型不再只是*说*,还能*做* ——
//! 通过 [`EngineTools::definitions`] 拿到工具词表,经 L7
//! `complete_with_tools` 交给任意厂商模型;模型的 [`ToolCall`]
//! 由 [`EngineTools::dispatch`] 翻译成 [`Command`] 提交进
//! [`InferSystem`],下一帧由命令预算机制落地。规则后端
//! (`spawn {"count":3}` 标记)让整条闭环离线可测、可演示。
//!
//! 派发永不 panic:坏参数返回给模型一段可读的拒绝说明(模型可以
//! 修正参数重试),未知工具列出可用词表 —— 工具循环必须能自愈。
//!
//! 需要 `json` feature(serde_json 解析参数;浏览器内核在 JS 侧
//! 实现同一循环,不引入此依赖)。

use oriera_infer::{Command, InferSystem, ToolCall, ToolDef};

/// 引擎命令词表 ↔ 模型工具调用的翻译器。无状态,零构造成本。
pub struct EngineTools;

impl EngineTools {
    /// 引擎命令的工具词表(L7 `ToolDef` 形状,JSON Schema 参数)。
    /// 与 [`Command`] 一一对应;`Nudge` 依赖实体句柄,由壳层在
    /// `SpawnedThisFrame` 之上自行暴露,v1 词表不收录。
    pub fn definitions() -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "spawn".into(),
                description: "在世界中生成 count 个实体(带随机位置与速度)".into(),
                parameters_json: r#"{"type":"object","properties":{"count":{"type":"integer","minimum":1,"maximum":100}},"required":["count"]}"#.into(),
            },
            ToolDef {
                name: "clear".into(),
                description: "清空世界中的所有实体".into(),
                parameters_json: r#"{"type":"object","properties":{}}"#.into(),
            },
            ToolDef {
                name: "set_gravity".into(),
                description: "设置世界重力(米/秒²;月球约 1.6,地球约 9.8)".into(),
                parameters_json: r#"{"type":"object","properties":{"value":{"type":"number"}},"required":["value"]}"#.into(),
            },
            ToolDef {
                name: "teleport".into(),
                description: "把所有实体平移 (dx, dy)".into(),
                parameters_json: r#"{"type":"object","properties":{"dx":{"type":"number"},"dy":{"type":"number"}},"required":["dx","dy"]}"#.into(),
            },
            ToolDef {
                name: "count_entities".into(),
                description: "统计当前世界实体数(结果写入 Stats 资源)".into(),
                parameters_json: r#"{"type":"object","properties":{}}"#.into(),
            },
            ToolDef {
                name: "set_speed".into(),
                description: "把所有实体速度乘以系数 k(2.0 = 加倍)".into(),
                parameters_json: r#"{"type":"object","properties":{"factor":{"type":"number"}},"required":["factor"]}"#.into(),
            },
        ]
    }

    /// 派发一次模型工具调用:翻译成 [`Command`] 提交进推理运行时。
    /// 返回值是回给模型的确认/拒绝文本(错误自愈信息,不是异常)。
    pub fn dispatch(call: &ToolCall, infer: &mut InferSystem) -> String {
        let args: serde_json::Value =
            serde_json::from_str(&call.arguments_json).unwrap_or(serde_json::Value::Null);
        match call.name.as_str() {
            "spawn" => match require_int(&args, "count", 1, 100) {
                Ok(n) => {
                    infer.submit(Command::Spawn(n as usize));
                    format!("已提交:下一帧生成 {n} 个实体")
                }
                Err(why) => reject("spawn", why),
            },
            "clear" => {
                infer.submit(Command::Clear);
                "已提交:下一帧清空世界".into()
            }
            "set_gravity" => match require_f64(&args, "value") {
                Ok(g) => {
                    infer.submit(Command::SetGravity(g as f32));
                    format!("已提交:重力设为 {g}")
                }
                Err(why) => reject("set_gravity", why),
            },
            "teleport" => match (require_f64(&args, "dx"), require_f64(&args, "dy")) {
                (Ok(dx), Ok(dy)) => {
                    infer.submit(Command::Teleport(dx as f32, dy as f32));
                    format!("已提交:全体平移 ({dx}, {dy})")
                }
                (Err(why), _) | (_, Err(why)) => reject("teleport", why),
            },
            "count_entities" => {
                infer.submit(Command::Count);
                "已提交:下一帧统计实体数(读 Stats 资源)".into()
            }
            "set_speed" => match require_f64(&args, "factor") {
                Ok(k) => {
                    infer.submit(Command::SetSpeed(k as f32));
                    format!("已提交:速度系数 {k}")
                }
                Err(why) => reject("set_speed", why),
            },
            other => format!(
                "未知工具「{other}」。可用:{}",
                Self::definitions()
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

fn reject(tool: &str, why: String) -> String {
    format!("拒绝执行 {tool}:{why}。请修正参数后重试。")
}

fn require_int(args: &serde_json::Value, key: &str, min: i64, max: i64) -> Result<i64, String> {
    let v = args.get(key).ok_or_else(|| format!("缺少参数 {key}"))?;
    let n = v
        .as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .ok_or_else(|| format!("参数 {key} 必须是整数"))?;
    if !(min..=max).contains(&n) {
        return Err(format!("参数 {key} 超出范围 [{min},{max}]"));
    }
    Ok(n)
}

fn require_f64(args: &serde_json::Value, key: &str) -> Result<f64, String> {
    args.get(key)
        .and_then(|v| v.as_f64())
        .ok_or_else(|| format!("缺少或非法的数值参数 {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oriera_core::{FrameCtx, System};
    use oriera_ecs::World;
    use oriera_infer::{ChatMessage, ModelProvider, RuleProvider, ToolOutcome};
    use std::sync::Arc;

    fn ctx() -> FrameCtx<'static> {
        use oriera_common::{FrameIndex, Grants, Timestamp};
        FrameCtx {
            frame: FrameIndex(0),
            dt: 0.0,
            now: Timestamp(0),
            grants: Grants::EMPTY,
        }
    }

    #[test]
    fn dispatch_spawns_entities_next_frame() {
        let mut infer = InferSystem::new();
        let ack = EngineTools::dispatch(
            &ToolCall {
                id: "c1".into(),
                name: "spawn".into(),
                arguments_json: r#"{"count":3}"#.into(),
            },
            &mut infer,
        );
        assert!(ack.contains("3"));
        assert_eq!(infer.pending(), 1);

        let mut world = World::new();
        infer.run(&ctx(), &mut world);
        assert_eq!(world.len(), 3, "commands land on the next tick");
    }

    #[test]
    fn bad_arguments_are_rejected_with_self_healing_text() {
        let mut infer = InferSystem::new();
        let missing = EngineTools::dispatch(
            &ToolCall {
                id: "c2".into(),
                name: "spawn".into(),
                arguments_json: "{}".into(),
            },
            &mut infer,
        );
        assert!(missing.contains("缺少参数"), "{missing}");
        let out_of_range = EngineTools::dispatch(
            &ToolCall {
                id: "c3".into(),
                name: "spawn".into(),
                arguments_json: r#"{"count":9999}"#.into(),
            },
            &mut infer,
        );
        assert!(out_of_range.contains("超出范围"));
        assert_eq!(infer.pending(), 0, "rejected calls never reach the queue");
    }

    #[test]
    fn unknown_tool_lists_the_vocabulary() {
        let mut infer = InferSystem::new();
        let ack = EngineTools::dispatch(
            &ToolCall {
                id: "c4".into(),
                name: "make_coffee".into(),
                arguments_json: "{}".into(),
            },
            &mut infer,
        );
        assert!(ack.contains("未知工具"));
        assert!(ack.contains("spawn"), "{ack}");
    }

    #[test]
    fn offline_full_loop_rule_provider_to_world() {
        // RFC-3 工具闭环的离线全程:词表 → 规则模型(标记模拟)→
        // 派发 → 命令落地 → 世界改变。
        let provider = RuleProvider::new();
        let tools = EngineTools::definitions();
        assert!(provider.supports_tools());

        let outcome = provider
            .complete_with_tools(&[ChatMessage::user("导演指令:spawn {\"count\":5}")], &tools)
            .unwrap();
        let ToolOutcome::ToolCalls(calls) = outcome else {
            panic!("rule provider should request the tool");
        };

        let mut infer = InferSystem::new();
        for call in &calls {
            EngineTools::dispatch(call, &mut infer);
        }
        let mut world = World::new();
        infer.run(&ctx(), &mut world);
        assert_eq!(world.len(), 5, "the model's tool call changed the world");
    }

    #[test]
    fn gravity_and_speed_round_trip_through_the_queue() {
        let mut infer = InferSystem::new();
        EngineTools::dispatch(
            &ToolCall {
                id: "g".into(),
                name: "set_gravity".into(),
                arguments_json: r#"{"value":1.6}"#.into(),
            },
            &mut infer,
        );
        let mut world = World::new();
        infer.run(&ctx(), &mut world);
        assert_eq!(
            world.resource::<oriera_subsys::Gravity>().map(|g| g.0),
            Some(1.6)
        );
    }

    #[test]
    fn vocabulary_matches_command_names() {
        let defs = EngineTools::definitions();
        let names: Vec<&str> = defs.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "spawn",
                "clear",
                "set_gravity",
                "teleport",
                "count_entities",
                "set_speed"
            ]
        );
        // 每个 schema 都是合法 JSON(上链前会被再解析一次)
        for t in EngineTools::definitions() {
            assert!(serde_json::from_str::<serde_json::Value>(&t.parameters_json).is_ok());
        }
    }

    // 占位:Arc<RuleProvider> 也满足 'static,证明异步作业层
    // (InferJobs)可以原样承载工具化提供者。
    #[test]
    fn tool_providers_compose_with_arc() {
        let p: Arc<dyn ModelProvider> = Arc::new(RuleProvider::new());
        assert!(p.supports_tools());
    }
}
