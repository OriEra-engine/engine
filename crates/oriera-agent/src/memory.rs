//! L9 long-term memory — episodic store with retrieval (book T12).
//!
//! An engine service, not application state: cognition systems call
//! `record`/`retrieve` and persistence is a runtime concern (localStorage
//! in the wasm kernel, `Serialize` on L3 in native builds).

use std::collections::HashMap;

/// One remembered episode.
#[derive(Debug, Clone, PartialEq)]
pub struct Episode {
    /// Monotonic turn index when it happened.
    pub turn: u64,
    /// World day (for display).
    pub day: u32,
    /// Who/what it is about (npc name or "world").
    pub about: String,
    /// The remembered fact, in the rememberer's own words.
    pub text: String,
    /// 1..=5 salience; high-importance episodes surface more easily.
    pub importance: u8,
}

/// Per-agent episodic memory with keyword-recency-importance retrieval.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    memories: HashMap<String, Vec<Episode>>,
    /// Sliding-window cap per agent; `Default` = 60 (the long-standing
    /// semantic, kept for every existing caller).
    capacity: usize,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self {
            memories: HashMap::new(),
            capacity: 60,
        }
    }
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Per-agent capacity override (#81 kernel-v2 mirror): the browser
    /// kernel's `new MemoryStore(key, capacity)` needs the same knob
    /// here — the 60 hardcode was flagged by every vertical demo as the
    /// first thing an embodied/companion product raises.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            ..Self::default()
        }
    }

    /// Record an episode for `agent`.
    pub fn record(&mut self, agent: &str, episode: Episode) {
        let v = self.memories.entry(agent.to_string()).or_default();
        v.push(episode);
        let len = v.len();
        if len > self.capacity {
            v.drain(0..len - self.capacity);
        }
    }

    /// Retrieve the `k` most relevant episodes about `query` (+`about`
    /// bonus) for `agent`, scored by keyword overlap, importance and
    /// recency relative to `now`.
    pub fn retrieve(
        &self,
        agent: &str,
        about: &str,
        query: &str,
        now: u64,
        k: usize,
    ) -> Vec<Episode> {
        let Some(v) = self.memories.get(agent) else {
            return Vec::new();
        };
        let q = bigrams(&format!("{query}{about}"));
        let mut scored: Vec<(f32, &Episode)> = v
            .iter()
            .map(|e| {
                let text = bigrams(&format!("{}{}", e.text, e.about));
                let hits = q.iter().filter(|g| text.contains(*g)).count();
                let recency = 1.0 / (1.0 + (now.saturating_sub(e.turn)) as f32);
                let about_bonus = if e.about == about { 1.2 } else { 0.0 };
                (
                    hits as f32 * 1.6 + e.importance as f32 * 0.45 + recency * 2.0 + about_bonus,
                    e,
                )
            })
            .filter(|(s, _)| *s > 0.8)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        scored.into_iter().take(k).map(|(_, e)| e.clone()).collect()
    }

    /// Total episodes across agents (metrics).
    pub fn len_total(&self) -> usize {
        self.memories.values().map(|v| v.len()).sum()
    }

    /// All agents that hold at least one episode.
    pub fn agents(&self) -> impl Iterator<Item = &str> {
        self.memories.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.memories.values().all(|v| v.is_empty())
    }
}

/// Cross-session persistence: `to_json`/`from_json` behind the optional
/// `json` feature (#70). The shell owns where the string lives (file,
/// localStorage, L1 storage later) — the engine only promises the pair
/// round-trips and never panics on bad input.
#[cfg(feature = "json")]
impl MemoryStore {
    /// Compact JSON snapshot. Agents are emitted in sorted order, so the
    /// same store always serializes to the same string — friendly to
    /// hashing, diffing and replay assertions.
    pub fn to_json(&self) -> String {
        use serde_json::{Map, Value};
        let mut root = Map::new();
        root.insert("version".into(), Value::from(1u64));
        let mut agents: Vec<&String> = self.memories.keys().collect();
        agents.sort();
        for agent in agents {
            let episodes: Vec<Value> = self.memories[agent]
                .iter()
                .map(|e| {
                    let mut m = Map::new();
                    m.insert("turn".into(), Value::from(e.turn));
                    m.insert("day".into(), Value::from(e.day));
                    m.insert("about".into(), Value::from(e.about.clone()));
                    m.insert("text".into(), Value::from(e.text.clone()));
                    m.insert("importance".into(), Value::from(e.importance));
                    Value::Object(m)
                })
                .collect();
            root.insert(agent.clone(), Value::Array(episodes));
        }
        Value::Object(root).to_string()
    }

    /// Rebuild a store from [`MemoryStore::to_json`] output. Malformed
    /// input of any kind is `Err` — never a panic; callers keep their
    /// old store and carry on (self-healing philosophy). Episodes are
    /// re-`record`ed, so the 60-per-agent sliding window invariant
    /// applies to imports exactly as it does to live writes.
    pub fn from_json(s: &str) -> Result<Self, String> {
        use serde_json::Value;
        let v: Value =
            serde_json::from_str(s).map_err(|e| format!("memory json: parse error: {e}"))?;
        let obj = v
            .as_object()
            .ok_or_else(|| "memory json: top level must be an object".to_string())?;
        let mut store = MemoryStore::new();
        for (agent, episodes) in obj {
            if agent == "version" {
                continue; // reserved, forward-compat slot
            }
            let arr = episodes.as_array().ok_or_else(|| {
                format!("memory json: agent '{agent}' must map to an episode array")
            })?;
            for ep in arr {
                let o = ep.as_object().ok_or_else(|| {
                    format!("memory json: agent '{agent}' has a non-object episode")
                })?;
                let field = |k: &str| {
                    o.get(k).ok_or_else(|| {
                        format!("memory json: agent '{agent}' episode missing field '{k}'")
                    })
                };
                let u64_field = |k: &str| -> Result<u64, String> {
                    field(k)?.as_u64().ok_or_else(|| {
                        format!("memory json: field '{k}' must be a non-negative integer")
                    })
                };
                let str_field = |k: &str| -> Result<String, String> {
                    field(k)?
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("memory json: field '{k}' must be a string"))
                };
                let importance = u64_field("importance")?;
                let importance = u8::try_from(importance)
                    .map_err(|_| "memory json: field 'importance' must fit in u8")?;
                store.record(
                    agent,
                    Episode {
                        turn: u64_field("turn")?,
                        day: u32::try_from(u64_field("day")?)
                            .map_err(|_| "memory json: field 'day' must fit in u32")?,
                        about: str_field("about")?,
                        text: str_field("text")?,
                        importance,
                    },
                );
            }
        }
        Ok(store)
    }
}

/// CJK bigrams + latin words — the cheap deterministic tokenizer the
/// wasm kernel mirrors (`zhBigrams`).
fn bigrams(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ('\u{4E00}'..='\u{9FA5}').contains(&ch) {
            cur.push(ch);
        } else {
            flush(&mut cur, &mut out, s);
        }
    }
    flush(&mut cur, &mut out, s);
    out
}

fn flush(cur: &mut String, out: &mut Vec<String>, _s: &str) {
    let chars: Vec<char> = cur.chars().collect();
    for w in chars.windows(2) {
        out.push(w.iter().collect());
    }
    cur.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(turn: u64, about: &str, text: &str, importance: u8) -> Episode {
        Episode {
            turn,
            day: 1,
            about: about.into(),
            text: text.into(),
            importance,
        }
    }

    #[test]
    fn retrieval_prefers_keyword_and_partner() {
        let mut m = MemoryStore::new();
        m.record("aria", ep(1, "老周", "老周记得每个人的口味，像魔法", 3));
        m.record("aria", ep(2, "world", "雨季来了，店里放起了爵士", 2));
        m.record("aria", ep(3, "阿哲", "阿哲的引擎 demo 跑通了", 3));

        let hits = m.retrieve("aria", "老周", "口味 手冲", 10, 2);
        assert!(!hits.is_empty());
        assert!(hits[0].about == "老周");
    }

    #[test]
    fn caps_at_sixty_per_agent() {
        let mut m = MemoryStore::new();
        for i in 0..80 {
            m.record("z", ep(i, "world", &format!("事件{i}"), 1));
        }
        assert_eq!(m.len_total(), 60);
        // newest kept
        assert!(m.retrieve("z", "world", "事件79", 100, 1)[0]
            .text
            .contains("79"));
    }

    #[test]
    fn capacity_is_parameterizable_with_sane_default() {
        // Default stays 60 (every existing caller's semantic).
        let mut d = MemoryStore::new();
        for i in 0..70 {
            d.record("z", ep(i, "world", &format!("e{i}"), 1));
        }
        assert_eq!(d.len_total(), 60);
        // Raised cap honored (kernel-v2 mirror).
        let mut big = MemoryStore::with_capacity(200);
        for i in 0..70 {
            big.record("z", ep(i, "world", &format!("e{i}"), 1));
        }
        assert_eq!(big.len_total(), 70);
        // json round trip: from_json re-records into a DEFAULT-capacity
        // store, so the #70 sliding window caps the import at 60 — the
        // documented import semantic (a capacity-carrying import is a
        // future extension, not an accident).
        #[cfg(feature = "json")]
        {
            let restored = MemoryStore::from_json(&big.to_json()).unwrap();
            assert_eq!(restored.len_total(), 60);
        }
    }

    #[test]
    fn unknown_agent_is_empty() {
        assert!(MemoryStore::new()
            .retrieve("nobody", "", "", 0, 3)
            .is_empty());
    }

    // ---- #70: MemoryStore ↔ JSON (feature `json`) ----

    #[cfg(feature = "json")]
    fn seeded_store() -> MemoryStore {
        let mut m = MemoryStore::new();
        m.record("aria", ep(1, "老周", "老周记得每个人的口味", 4));
        m.record("aria", ep(9, "world", "雨季来了，店里放起了爵士", 2));
        m.record("老周", ep(3, "Aria", "Aria 总在窗边画到打烊", 5));
        m.record("老周", ep(7, "aria", "双份浓缩是她的老样子", 3));
        m
    }

    #[cfg(feature = "json")]
    #[test]
    fn json_round_trip_preserves_retrieval() {
        let m = seeded_store();
        let restored = MemoryStore::from_json(&m.to_json()).unwrap();
        assert_eq!(restored.len_total(), m.len_total());
        // Same questions → same answers, same order.
        assert_eq!(
            restored.retrieve("aria", "老周", "口味", 100, 2),
            m.retrieve("aria", "老周", "口味", 100, 2)
        );
        assert_eq!(
            restored.retrieve("老周", "Aria", "窗边 画", 100, 1),
            m.retrieve("老周", "Aria", "窗边 画", 100, 1)
        );
    }

    #[cfg(feature = "json")]
    #[test]
    fn bad_json_is_err_never_panic() {
        assert!(MemoryStore::from_json("").is_err());
        assert!(MemoryStore::from_json("{not json").is_err());
        assert!(MemoryStore::from_json("[]").is_err()); // top level must be object
        assert!(MemoryStore::from_json(r#"{"version":1,"a":"nope"}"#).is_err()); // episodes must be array
        assert!(MemoryStore::from_json(r#"{"a":[{"turn":"x"}]}"#).is_err()); // wrong type
        assert!(MemoryStore::from_json(r#"{"a":[{"turn":1}]}"#).is_err()); // missing fields
        assert!(MemoryStore::from_json(
            r#"{"a":[{"turn":1,"day":1,"about":"","text":"","importance":999}]}"#
        )
        .is_err()); // u8 overflow
                    // Empty store round-trips too.
        assert!(MemoryStore::from_json(&MemoryStore::new().to_json())
            .unwrap()
            .is_empty());
    }

    #[cfg(feature = "json")]
    #[test]
    fn to_json_is_stable_regardless_of_agent_insertion_order() {
        let a = seeded_store();
        let mut b = MemoryStore::new();
        // Same episodes, same per-agent history order — only the order
        // the two AGENTS were first recorded differs. Episode order is
        // the store's history and is deliberately preserved, not sorted.
        b.record("老周", ep(3, "Aria", "Aria 总在窗边画到打烊", 5));
        b.record("老周", ep(7, "aria", "双份浓缩是她的老样子", 3));
        b.record("aria", ep(1, "老周", "老周记得每个人的口味", 4));
        b.record("aria", ep(9, "world", "雨季来了，店里放起了爵士", 2));
        assert_eq!(a.to_json(), b.to_json());
        assert_eq!(a.to_json(), a.to_json()); // idempotent string
    }

    #[cfg(feature = "json")]
    #[test]
    fn import_honors_the_sliding_window() {
        let mut m = MemoryStore::new();
        for i in 0..80 {
            m.record("z", ep(i, "world", &format!("事件{i}"), 1));
        }
        let restored = MemoryStore::from_json(&m.to_json()).unwrap();
        assert_eq!(restored.len_total(), 60); // cap survives the round trip
        assert!(restored.retrieve("z", "world", "事件79", 100, 1)[0]
            .text
            .contains("79"));
    }
}
