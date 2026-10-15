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
#[derive(Debug, Default, Clone)]
pub struct MemoryStore {
    memories: HashMap<String, Vec<Episode>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an episode for `agent`.
    pub fn record(&mut self, agent: &str, episode: Episode) {
        let v = self.memories.entry(agent.to_string()).or_default();
        v.push(episode);
        let len = v.len();
        if len > 60 {
            v.drain(0..len - 60);
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
    fn unknown_agent_is_empty() {
        assert!(MemoryStore::new()
            .retrieve("nobody", "", "", 0, 3)
            .is_empty());
    }
}
