//! L9 narrative primitives (v0.1) — the interactive-film graph as a
//! first-class engine type. Everything here is pure data + pure logic:
//! no `World` coupling, so the browser kernel (demo 14) mirrors it
//! statement-for-statement and Studio's editor/validator consume the
//! same `validate()` findings.
//!
//! Field-tested against 《末班站台》 (interfilm/, M0→M2.1): the schema
//! below is that story.json, minus presentation-only fields.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// A whole interactive story: nodes, edges and the variable namespace.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StoryGraph {
    #[serde(default)]
    pub vars: BTreeMap<String, StoryValue>,
    #[serde(default)]
    pub nodes: Vec<StoryNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoryNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: NodeKind,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub goto: Option<String>,
    #[serde(default)]
    pub options: Vec<StoryOption>,
    #[serde(default)]
    pub routes: Vec<StoryRoute>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Scene,
    Choice,
    Hub,
    Ending,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoryOption {
    pub text: String,
    /// `"+1"`, `"-1"`, `"true"`, `"false"` or a bare literal.
    #[serde(default)]
    pub set: BTreeMap<String, String>,
    pub goto: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoryRoute {
    /// Restricted expression `var op integer` (field-tested subset).
    #[serde(default)]
    pub when: Option<String>,
    pub goto: String,
}

/// A story variable. BTree keeps the fingerprint order-stable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum StoryValue {
    Int(i64),
    Bool(bool),
    Str(String),
}

impl StoryValue {
    fn as_int(&self) -> i64 {
        match self {
            StoryValue::Int(i) => *i,
            StoryValue::Bool(b) => *b as i64,
            StoryValue::Str(_) => 0,
        }
    }
}

// ─────────────────────────── 变量结算 ───────────────────────────

/// The one true settlement semantics (B2): the player kernel, the Rust
/// validator and Studio all evaluate sets/whens through these.
impl StoryGraph {
    /// Apply a `set` map (`"+1"` / `"-2"` / `"true"` / `"-1"`…).
    /// Unknown variables are reported by `validate`, ignored here.
    pub fn apply_sets(
        &self,
        vars: &mut BTreeMap<String, StoryValue>,
        sets: &BTreeMap<String, String>,
    ) {
        for (k, expr) in sets {
            if let Some(cur) = vars.get_mut(k) {
                *cur = apply_one(cur, expr);
            }
        }
    }

    /// Restricted `var op integer` evaluation; `None` = unparsable
    /// (validate flags it, runtime treats it as false).
    pub fn eval_when(vars: &BTreeMap<String, StoryValue>, expr: &str) -> Option<bool> {
        let expr = expr.trim();
        let (name, op, rhs) = split_when(expr)?;
        let a = vars.get(name)?.as_int();
        let b: i64 = rhs.parse().ok()?;
        Some(match op {
            ">=" => a >= b,
            "<=" => a <= b,
            ">" => a > b,
            "<" => a < b,
            "==" => a == b,
            "!=" => a != b,
            _ => return None,
        })
    }

    /// Resolve a hub: first route whose `when` holds, else the last
    /// route (the default arm — the field-tested player convention).
    pub fn resolve_hub<'a>(
        vars: &BTreeMap<String, StoryValue>,
        node: &'a StoryNode,
    ) -> Option<&'a StoryRoute> {
        let mut hit = None;
        let mut fallback = None;
        for r in &node.routes {
            if fallback.is_none() && r.when.is_none() {
                fallback = Some(r);
            }
            if r.when
                .as_deref()
                .is_none_or(|w| Self::eval_when(vars, w).unwrap_or(false))
                && hit.is_none()
            {
                hit = Some(r);
            }
        }
        hit.or(fallback).or_else(|| node.routes.last())
    }
}

fn apply_one(cur: &StoryValue, expr: &str) -> StoryValue {
    let e = expr.trim();
    if let Some(num) = e.strip_prefix('+') {
        if let Ok(d) = num.trim().parse::<i64>() {
            return StoryValue::Int(cur.as_int() + d);
        }
    }
    if let Some(num) = e.strip_prefix('-') {
        if let Ok(d) = num.trim().parse::<i64>() {
            return StoryValue::Int(cur.as_int() - d);
        }
    }
    match e {
        "true" => StoryValue::Bool(true),
        "false" => StoryValue::Bool(false),
        other => other
            .parse::<i64>()
            .map(StoryValue::Int)
            .unwrap_or(StoryValue::Str(other.to_string())),
    }
}

fn split_when(expr: &str) -> Option<(&str, &str, &str)> {
    for op in ["<=", ">=", "==", "!=", "<", ">"] {
        if let Some((l, r)) = expr.split_once(op) {
            let name = l.trim();
            if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !name.is_empty() {
                return Some((name, op, r.trim()));
            }
        }
    }
    None
}

// ─────────────────────────── 校验器（B1） ───────────────────────────

/// One validator finding. Studio renders these directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryFinding {
    pub severity: Severity,
    pub node: String,
    pub kind: FindingKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingType {
    DeadEnd,
    Unreachable,
    MissingGoto,
    UnknownVariable,
    HubWithoutDefault,
    ChoiceWithoutOptions,
    Empty,
}

pub use FindingType as FindingKind;

impl StoryGraph {
    /// The narrative twin of the physics determinism guarantees (#66/#67):
    /// every node reachable, every non-ending node has an exit, every
    /// referenced variable declared, every hub has a default arm.
    pub fn validate(&self) -> Vec<StoryFinding> {
        let mut out = Vec::new();
        if self.nodes.is_empty() {
            out.push(StoryFinding {
                severity: Severity::Error,
                node: "-".into(),
                kind: FindingType::Empty,
                detail: "story graph has no nodes".into(),
            });
            return out;
        }
        let ids: HashSet<&str> = self.nodes.iter().map(|n| n.id.as_str()).collect();
        let declared: HashSet<&str> = self.vars.keys().map(|s| s.as_str()).collect();

        let outs = |n: &StoryNode| -> Vec<String> {
            let mut v: Vec<String> = n.options.iter().map(|o| o.goto.clone()).collect();
            v.extend(n.routes.iter().map(|r| r.goto.clone()));
            if let Some(g) = &n.goto {
                v.push(g.clone());
            }
            v
        };

        for n in &self.nodes {
            // exits
            match n.kind {
                NodeKind::Ending => {}
                NodeKind::Choice if n.options.is_empty() => out.push(StoryFinding {
                    severity: Severity::Error,
                    node: n.id.clone(),
                    kind: FindingType::ChoiceWithoutOptions,
                    detail: "choice node without options".into(),
                }),
                NodeKind::Scene if n.goto.is_none() => out.push(StoryFinding {
                    severity: Severity::Error,
                    node: n.id.clone(),
                    kind: FindingType::DeadEnd,
                    detail: "scene without goto (dead end)".into(),
                }),
                NodeKind::Hub if n.routes.is_empty() => out.push(StoryFinding {
                    severity: Severity::Error,
                    node: n.id.clone(),
                    kind: FindingType::DeadEnd,
                    detail: "hub without routes (dead end)".into(),
                }),
                _ => {}
            }
            if n.kind == NodeKind::Hub
                && !n.routes.is_empty()
                && n.routes.iter().all(|r| r.when.is_some())
            {
                out.push(StoryFinding {
                    severity: Severity::Warning,
                    node: n.id.clone(),
                    kind: FindingType::HubWithoutDefault,
                    detail: "hub without a default arm stalls when all whens fail".into(),
                });
            }
            // goto targets + variables
            for g in outs(n) {
                if !ids.contains(g.as_str()) {
                    out.push(StoryFinding {
                        severity: Severity::Error,
                        node: n.id.clone(),
                        kind: FindingType::MissingGoto,
                        detail: format!("goto target does not exist: {g}"),
                    });
                }
            }
            for o in &n.options {
                for var in o.set.keys() {
                    if !declared.contains(var.as_str()) {
                        out.push(StoryFinding {
                            severity: Severity::Error,
                            node: n.id.clone(),
                            kind: FindingType::UnknownVariable,
                            detail: format!("set references undeclared variable: {var}"),
                        });
                    }
                }
            }
            for r in &n.routes {
                if let Some(w) = &r.when {
                    match split_when(w) {
                        Some((name, _, _)) if declared.contains(name) => {}
                        Some((name, _, _)) => out.push(StoryFinding {
                            severity: Severity::Error,
                            node: n.id.clone(),
                            kind: FindingType::UnknownVariable,
                            detail: format!("when references undeclared variable: {name}"),
                        }),
                        None => out.push(StoryFinding {
                            severity: Severity::Warning,
                            node: n.id.clone(),
                            kind: FindingType::UnknownVariable,
                            detail: format!("unparsable when (restricted `var op int` only): {w}"),
                        }),
                    }
                }
            }
        }

        // reachability (BFS from the first node — the field-tested entry)
        let by_id: HashMap<&str, &StoryNode> =
            self.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut q = VecDeque::new();
        if let Some(first) = self.nodes.first() {
            q.push_back(first.id.as_str());
        }
        while let Some(id) = q.pop_front() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(n) = by_id.get(id) {
                for g in outs(n) {
                    if let Some(t) = g.as_str().strip_prefix("") {
                        // keep lifetimes simple: re-lookup by value
                        let owned = t.to_string();
                        if let Some(node) = by_id.get(owned.as_str()) {
                            q.push_back(node.id.as_str());
                        }
                    }
                }
            }
        }
        for n in &self.nodes {
            if !seen.contains(n.id.as_str()) {
                out.push(StoryFinding {
                    severity: Severity::Error,
                    node: n.id.clone(),
                    kind: FindingType::Unreachable,
                    detail: "node is unreachable from the entry".into(),
                });
            }
        }
        let reachable_endings = self
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Ending && seen.contains(n.id.as_str()))
            .count();
        if reachable_endings == 0 {
            out.push(StoryFinding {
                severity: Severity::Error,
                node: "-".into(),
                kind: FindingType::Unreachable,
                detail: "no ending is reachable (cycle or broken edges)".into(),
            });
        }
        out
    }
}

// ─────────────────────────── 存档指纹（D2 前置） ───────────────────────────

/// FNV-1a over sorted vars + node id — the narrative sibling of
/// `World::state_hash` (#67). Same save ⇒ same fingerprint, so runs are
/// comparable and shareable (speedrun ladders, ending proofs).
pub fn save_fingerprint(vars: &BTreeMap<String, StoryValue>, node_id: &str) -> u64 {
    // Seed mirrors the #67 convention: ASCII tag as the FNV offset basis.
    let mut h: u64 = 0xcbf29ce484222325 ^ u64::from_le_bytes(*b"oriera.s");
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    for (k, v) in vars {
        feed(k.as_bytes());
        feed(&[0x1f]);
        match v {
            StoryValue::Int(i) => feed(&i.to_le_bytes()),
            StoryValue::Bool(b) => feed(&[*b as u8]),
            StoryValue::Str(s) => feed(s.as_bytes()),
        }
        feed(&[0x1e]);
    }
    feed(node_id.as_bytes());
    h
}

// ─────────────────────────── 角色记忆桥（B3） ───────────────────────────

/// A distilled player choice, ready to become a memory episode so an
/// LLM character (the phone-call easter egg) *remembers* what the
/// player did — field-tested as the highest-impact immersion lever.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceMemory {
    pub node_id: String,
    pub option_text: String,
    /// Choices that touched these variables are worth remembering.
    pub touched_vars: Vec<String>,
}

impl StoryGraph {
    /// All choice points as memory candidates (pre-play; filter at
    /// runtime by the options actually taken).
    pub fn choice_memories(&self) -> Vec<ChoiceMemory> {
        self.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Choice)
            .flat_map(|n| {
                n.options.iter().map(move |o| ChoiceMemory {
                    node_id: n.id.clone(),
                    option_text: o.text.clone(),
                    touched_vars: o.set.keys().cloned().collect(),
                })
            })
            .collect()
    }
}

// ─────────────────────────── 叙事导演（B4, v0.1 事件版） ───────────────────────────

/// Atmosphere directives derived from story vars — pure function, no
/// `World` coupling; the kernel renders them (rain density, siren
/// strobing), demo 13's `StoryboardArtSystem` is the visual sibling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtmosphereDirective {
    Calm,
    RainHeavier,
    SirensOn,
    TensionPeak,
}

impl StoryGraph {
    /// Field-tested mapping from 《末班站台》: trust sags → rain picks
    /// up; the finale lights the sirens; hostility peaks the tension.
    pub fn atmosphere(&self, vars: &BTreeMap<String, StoryValue>) -> AtmosphereDirective {
        let trust = vars.get("trust").map(|v| v.as_int()).unwrap_or(0);
        let affinity = vars.get("affinity_lin").map(|v| v.as_int()).unwrap_or(0);
        if trust <= -2 || affinity <= -2 {
            AtmosphereDirective::TensionPeak
        } else if trust < 0 {
            AtmosphereDirective::RainHeavier
        } else if trust >= 2 {
            AtmosphereDirective::SirensOn
        } else {
            AtmosphereDirective::Calm
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn park_platform() -> StoryGraph {
        // 《末班站台》 distilled: 5 nodes, one choice, one hub, two endings.
        serde_json::from_str(
            r#"{
              "vars": { "affinity_lin": 0, "trust": 0 },
              "nodes": [
                {"id":"s001","type":"scene","title":"末班地铁","goto":"c001"},
                {"id":"c001","type":"choice","title":"站台",
                 "options":[
                   {"text":"递伞","set":{"affinity_lin":"+1"},"goto":"s002a"},
                   {"text":"无视","set":{"affinity_lin":"-1"},"goto":"s002b"}]},
                {"id":"s002a","type":"scene","title":"道谢","goto":"hub"},
                {"id":"s002b","type":"scene","title":"目光","goto":"hub"},
                {"id":"hub","type":"hub","title":"结算",
                 "routes":[{"when":"affinity_lin >= 1","goto":"e_good"},{"goto":"e_bad"}]},
                {"id":"e_good","type":"ending","title":"雨夜真相"},
                {"id":"e_bad","type":"ending","title":"消失的站台"}
              ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn field_tested_graph_validates_clean() {
        assert!(
            park_platform().validate().is_empty(),
            "the shipped graph must be clean"
        );
    }

    #[test]
    fn validator_catches_the_real_failure_modes() {
        let mut g = park_platform();
        // dead end: strip a scene's goto
        g.nodes[0].goto = None;
        // unknown var: set a typo
        g.nodes[1].options[0]
            .set
            .insert("affiniti_lin".into(), "+1".into());
        let f = g.validate();
        assert!(f.iter().any(|x| x.kind == FindingType::DeadEnd));
        assert!(f.iter().any(|x| x.kind == FindingType::UnknownVariable));
    }

    #[test]
    fn hub_settlement_matches_the_player_convention() {
        let g = park_platform();
        let hub = g.nodes.iter().find(|n| n.id == "hub").unwrap();
        let mut warm = g.vars.clone();
        g.apply_sets(&mut warm, &g.nodes[1].options[0].set);
        assert_eq!(
            StoryGraph::resolve_hub(&warm, hub).map(|r| r.goto.as_str()),
            Some("e_good")
        );
        let mut cold = g.vars.clone();
        g.apply_sets(&mut cold, &g.nodes[1].options[1].set);
        assert_eq!(
            StoryGraph::resolve_hub(&cold, hub).map(|r| r.goto.as_str()),
            Some("e_bad")
        );
    }

    #[test]
    fn fingerprint_is_order_stable_and_save_sensitive() {
        let g = park_platform();
        let mut vars = g.vars.clone();
        let a = save_fingerprint(&vars, "c001");
        vars.insert("affinity_lin".into(), StoryValue::Int(1));
        let b = save_fingerprint(&vars, "c001");
        let c = save_fingerprint(&vars, "s001");
        assert_ne!(a, b, "variable change must change the fingerprint");
        assert_ne!(a, c, "node change must change the fingerprint");
        assert_eq!(a, save_fingerprint(&g.vars, "c001"));
    }

    #[test]
    fn choices_become_memories_and_direct_the_atmosphere() {
        let g = park_platform();
        let mems = g.choice_memories();
        assert_eq!(mems.len(), 2);
        assert!(mems.iter().any(|m| m.option_text == "递伞"));

        let mut vars = g.vars.clone();
        vars.insert("trust".into(), StoryValue::Int(-2));
        assert_eq!(g.atmosphere(&vars), AtmosphereDirective::TensionPeak);
        assert_eq!(g.atmosphere(&g.vars), AtmosphereDirective::Calm);
    }

    #[test]
    fn when_expressions_stay_restricted() {
        let mut v = BTreeMap::new();
        v.insert("trust".to_string(), StoryValue::Int(3));
        assert_eq!(StoryGraph::eval_when(&v, "trust >= 1"), Some(true));
        assert_eq!(StoryGraph::eval_when(&v, "trust == 3"), Some(true));
        assert_eq!(StoryGraph::eval_when(&v, "trust < 3"), Some(false));
        assert_eq!(StoryGraph::eval_when(&v, "drop table users;"), None);
    }
}
