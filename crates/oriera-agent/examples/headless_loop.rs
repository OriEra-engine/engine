//! P0-5 headless 验证回路 CLI（方案 A 最小竖切——缺口 G5/G6/G7-部分）。
//!
//! 与 `studio_e2e` 同模式的入口形态：固定场景 → 固定步数物理仿真 →
//! **三段分账 JSON**（BENCH-CONTRACT 附 A）打到 stdout——
//!
//! * `build`：引擎构建段毫秒 + 构建完成事件时间戳/回执指纹（G5）；
//! * `sim`：固定步物理仿真段毫秒 + 终态指纹（#66 `with_fixed_dt`
//!   确定性路径）；
//! * `render`：占位 `null`（World→ScenePart 桥是 G1，另行）；
//! * `llm`：接口槽位占位 `null`（真实 provider 计时包装是 G4，另行）；
//! * 附平台标签（`os-arch`）与环境指纹（契约第 2 条同环境对拍审计），
//!   计时口径按契约第 1 条 fallback 随身标注（`timing_scope`）。
//!
//! 入口自带双跑同构建位断言（构建回执/仿真终态逐位一致，物理金样
//! 同款纪律）——断言失败即 panic，测试即 demo（studio_e2e 惯例）。
//!
//! 运行：`cargo run -p oriera-agent --example headless_loop --features json`
//! （serde_json 打印走 `json` feature，与 `tool_loop` 同款 required-features）

use oriera_agent::run_headless_loop;

fn main() {
    // 双跑同构建位断言：同一构建位下两次独立运行，构建回执与仿真
    // 终态必须逐位一致（墙钟字段允许漂移，不参与比对）。
    let a = run_headless_loop();
    let b = run_headless_loop();
    assert_eq!(
        a.build.state_fingerprint, b.build.state_fingerprint,
        "构建完成回执双跑漂移——同一构建位必须逐位一致"
    );
    assert_eq!(
        a.sim.state_fingerprint, b.sim.state_fingerprint,
        "固定步仿真终态双跑漂移——#66 with_fixed_dt 契约被破坏"
    );

    // 人类可读的旁注走 stderr，stdout 只留机器可读的分账 JSON。
    eprintln!(
        "headless 回路 ×2：双跑同构建位一致（build {:016x} / sim {:016x}）",
        a.build.state_fingerprint, a.sim.state_fingerprint
    );
    eprintln!(
        "平台 {} · 环境指纹 {:016x} · 渲染段/LLM 段占位 null（G1/G4 另行）",
        a.platform, a.env_fingerprint
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&a).expect("分账结构可序列化（G6）")
    );
}
