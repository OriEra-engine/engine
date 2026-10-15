//! L7 Inference — AI runtime stub.
//!
//! v0.1: rule-based intent parser (no real LLM). When the user says
//! "spawn 100 particles", we parse it into an engine command.

use oriera_core::{FrameCtx, System};

#[derive(Debug, Clone)]
pub enum Command {
    Spawn(usize),
    Clear,
    SetGravity(f32),
    Unknown(String),
}

/// Parse a natural-language string into a Command.
pub fn parse(input: &str) -> Command {
    let lower = input.to_lowercase();
    if lower.contains("spawn") || lower.contains("放") || lower.contains("生成") {
        if let Some(n) = extract_number(&lower) {
            return Command::Spawn(n);
        }
    }
    if lower.contains("clear") || lower.contains("清空") || lower.contains("清屏") {
        return Command::Clear;
    }
    if lower.contains("gravity") || lower.contains("重力") {
        if let Some(g) = extract_number(&lower) {
            return Command::SetGravity(g as f32);
        }
    }
    Command::Unknown(input.to_string())
}

fn extract_number(s: &str) -> Option<usize> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() { None } else { digits.parse().ok() }
}

/// Inference system: processes a queue of commands each frame.
pub struct InferSystem {
    pub pending: Vec<Command>,
    pub processed: u64,
}

impl InferSystem {
    pub fn new() -> Self {
        Self { pending: Vec::new(), processed: 0 }
    }
    pub fn submit(&mut self, cmd: Command) {
        self.pending.push(cmd);
    }
}

impl System for InferSystem {
    fn name(&self) -> &str { "infer" }
    fn run(&mut self, _ctx: &FrameCtx) {
        self.processed += self.pending.len() as u64;
        self.pending.clear();
    }
}

impl Default for InferSystem {
    fn default() -> Self { Self::new() }
}
