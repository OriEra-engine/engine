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

/// Retrieval scoring coefficients — the ONE formula both tiers blend
/// around, lifted out of the retrieval code so it can be configured,
/// ablated and audited instead of living as magic numbers inside
/// `retrieve` (AI 原生评审 R3：结构—算法分离的病灶不在分层，而在
/// 算法假设被写死在结构层里——写死的系数既不可消融也不可替换）。
///
/// `Default` reproduces the long-standing formula exactly
/// (`hits×1.6 + importance×0.45 + recency×2.0 + about-bonus`, semantic
/// term `2.0×cos().max(0)`, `> 0.8` gate), so every existing caller —
/// including the wasm mirror in `examples/11-ai-cafe/engine/kernel.js`,
/// which carries the same default — keeps bit-identical results.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemoryScoring {
    /// Weight of a keyword bigram/word hit.
    pub keyword_weight: f32,
    /// Weight of episode salience (1..=5 importance).
    pub importance_weight: f32,
    /// Weight of the recency term (`1/(1+age)`).
    pub recency_weight: f32,
    /// Flat bonus when `episode.about == query.about`.
    pub about_bonus: f32,
    /// Weight of the non-negative cosine term (embedded tier only;
    /// the offline tier always feeds 0, keeping its historic score).
    pub semantic_weight: f32,
    /// Retrieval gate: scores at or below this are dropped.
    pub min_score: f32,
}

impl Default for MemoryScoring {
    fn default() -> Self {
        Self {
            keyword_weight: 1.6,
            importance_weight: 0.45,
            recency_weight: 2.0,
            about_bonus: 1.2,
            semantic_weight: 2.0,
            min_score: 0.8,
        }
    }
}

impl MemoryScoring {
    /// Non-finite coefficients fall back to their default (scoring
    /// config is caller input, not physical truth — one NaN must not
    /// poison a whole store's ranking).
    pub fn sanitized(self) -> Self {
        let d = Self::default();
        let f = |v: f32, fallback: f32| if v.is_finite() { v } else { fallback };
        Self {
            keyword_weight: f(self.keyword_weight, d.keyword_weight),
            importance_weight: f(self.importance_weight, d.importance_weight),
            recency_weight: f(self.recency_weight, d.recency_weight),
            about_bonus: f(self.about_bonus, d.about_bonus),
            semantic_weight: f(self.semantic_weight, d.semantic_weight),
            min_score: f(self.min_score, d.min_score),
        }
    }
}

/// Why one episode ranked where it did — the audit trail behind a
/// retrieval decision. Every term is the exact value that fed `total`,
/// so a report or ledger can re-derive the ranking without re-running
/// the store (L1 台账的最小落点：episode + 分数 + 分项，近零成本)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecallTrace {
    /// Bigram/word hits between `query+about` and `text+about`.
    pub keyword_hits: usize,
    /// `hits × keyword_weight`.
    pub keyword_term: f32,
    /// `importance × importance_weight`.
    pub importance_term: f32,
    /// `1/(1+age) × recency_weight`.
    pub recency_term: f32,
    /// Flat `about_bonus` when the episode is about the queried entity.
    pub about_bonus: f32,
    /// `semantic_weight × cos().max(0)`; always 0 on the offline tier.
    pub semantic_term: f32,
    /// Sum of the terms above — the ranked score.
    pub total: f32,
}

/// One ranked episode with its trace.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredEpisode {
    pub episode: Episode,
    pub trace: RecallTrace,
}

/// Per-agent episodic memory with keyword-recency-importance retrieval.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    memories: HashMap<String, Vec<Episode>>,
    /// Sliding-window cap per agent; `Default` = 60 (the long-standing
    /// semantic, kept for every existing caller).
    capacity: usize,
    /// Ranking coefficients; `Default` = the historic formula.
    scoring: MemoryScoring,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self {
            memories: HashMap::new(),
            capacity: 60,
            scoring: MemoryScoring::default(),
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

    /// Capacity + scoring in one shot (remote/graph backends configure
    /// both at wiring time).
    pub fn with_capacity_and_scoring(capacity: usize, scoring: MemoryScoring) -> Self {
        Self {
            capacity: capacity.max(1),
            scoring: scoring.sanitized(),
            ..Self::default()
        }
    }

    /// Swap the ranking coefficients at runtime — the ablation knob for
    /// "which coefficient actually earns its place". Non-finite values
    /// are sanitized to defaults.
    pub fn set_scoring(&mut self, scoring: MemoryScoring) {
        self.scoring = scoring.sanitized();
    }

    /// Current ranking coefficients.
    pub fn scoring(&self) -> MemoryScoring {
        self.scoring
    }

    /// Total + per-term breakdown for one episode; `semantic` is the
    /// non-negative cosine (0 on the offline tier).
    fn trace(
        &self,
        e: &Episode,
        hits: usize,
        now: u64,
        about: &str,
        semantic: f32,
    ) -> RecallTrace {
        let s = self.scoring;
        let recency = 1.0 / (1.0 + (now.saturating_sub(e.turn)) as f32);
        let about_bonus = if e.about == about { s.about_bonus } else { 0.0 };
        let keyword_term = hits as f32 * s.keyword_weight;
        let importance_term = e.importance as f32 * s.importance_weight;
        let recency_term = recency * s.recency_weight;
        let semantic_term = s.semantic_weight * semantic.max(0.0);
        RecallTrace {
            keyword_hits: hits,
            keyword_term,
            importance_term,
            recency_term,
            about_bonus,
            semantic_term,
            total: keyword_term + importance_term + recency_term + about_bonus + semantic_term,
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
        self.retrieve_traced(agent, about, query, now, k)
            .into_iter()
            .map(|s| s.episode)
            .collect()
    }

    /// Offline tier with the full scoring breakdown — same ranking as
    /// [`MemoryStore::retrieve`], but每一条命中都带回分项，供台账与
    /// 审计使用（评测报告要能复算名次，而不是只信任一个总数）。
    pub fn retrieve_traced(
        &self,
        agent: &str,
        about: &str,
        query: &str,
        now: u64,
        k: usize,
    ) -> Vec<ScoredEpisode> {
        let Some(v) = self.memories.get(agent) else {
            return Vec::new();
        };
        let q = bigrams(&format!("{query}{about}"));
        let mut scored: Vec<(f32, RecallTrace, &Episode)> = v
            .iter()
            .map(|e| {
                let text = bigrams(&format!("{}{}", e.text, e.about));
                let hits = q.iter().filter(|g| text.contains(*g)).count();
                let trace = self.trace(e, hits, now, about, 0.0);
                (trace.total, trace, e)
            })
            .filter(|(s, _, _)| *s > self.scoring.min_score)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored
            .into_iter()
            .take(k)
            .map(|(_, trace, e)| ScoredEpisode {
                episode: e.clone(),
                trace,
            })
            .collect()
    }

    /// Semantic tier of the two-tier retrieval (#101 step two): the
    /// same ranking as [`MemoryStore::retrieve`] plus a cosine term
    /// from `embedder` (one BATCH call for query + episodes — a remote
    /// embedder costs one wire round-trip, not one per episode). Blend:
    /// `offline + semantic_weight × cos().max(0)`, same gate. Embedder
    /// failure degrades VISIBLY to the offline tier (same contract as
    /// `retrieve`) — never empty results from a broken seat.
    pub fn retrieve_embedded(
        &self,
        embedder: &dyn oriera_infer::EmbeddingProvider,
        agent: &str,
        about: &str,
        query: &str,
        now: u64,
        k: usize,
    ) -> Vec<Episode> {
        self.retrieve_embedded_traced(embedder, agent, about, query, now, k)
            .into_iter()
            .map(|s| s.episode)
            .collect()
    }

    /// Embedded tier with the full scoring breakdown (semantic_term is
    /// the cosine contribution; 0 everywhere on the degraded path).
    pub fn retrieve_embedded_traced(
        &self,
        embedder: &dyn oriera_infer::EmbeddingProvider,
        agent: &str,
        about: &str,
        query: &str,
        now: u64,
        k: usize,
    ) -> Vec<ScoredEpisode> {
        let Some(v) = self.memories.get(agent) else {
            return Vec::new();
        };
        let mut inputs: Vec<String> = Vec::with_capacity(v.len() + 1);
        inputs.push(format!("{query}{about}"));
        inputs.extend(v.iter().map(|e| format!("{}{}", e.text, e.about)));
        let vecs = match embedder.embed(&inputs) {
            Ok(vs) if vs.len() == inputs.len() => vs,
            _ => return self.retrieve_traced(agent, about, query, now, k),
        };
        let (q, rows) = vecs.split_first().expect("query vector present");
        let q_bigrams = bigrams(&format!("{query}{about}"));
        let mut scored: Vec<(f32, RecallTrace, &Episode)> = v
            .iter()
            .zip(rows)
            .map(|(e, emb)| {
                let text = bigrams(&format!("{}{}", e.text, e.about));
                let hits = q_bigrams.iter().filter(|g| text.contains(*g)).count();
                // 语义项无 +1 基线（AI 原生三轮评审 P1）：曾用 2×(cos+1)/2
                // 使无关文本也白得 ~1.0 分，0.8 过滤被整体中和、陈旧条目
                // 全放行——负/零相关不得白得分。
                let trace = self.trace(e, hits, now, about, cosine(q, emb));
                (trace.total, trace, e)
            })
            .filter(|(s, _, _)| *s > self.scoring.min_score)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored
            .into_iter()
            .take(k)
            .map(|(_, trace, e)| ScoredEpisode {
                episode: e.clone(),
                trace,
            })
            .collect()
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

/// Cosine similarity; mismatched lengths or zero vectors score 0 (an
/// empty embed must not fabricate affinity).
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na <= f32::EPSILON || nb <= f32::EPSILON {
        return 0.0;
    }
    dot / (na * nb)
}

fn bigrams(s: &str) -> Vec<String> {
    // #139 真对齐（架构师三轮评审抓包：此前 commit 声称对齐但实现仍是
    // 字符对+不去重）。与 kernel.js zhBigrams 逐语义一致：
    //   CJK 连续段 → 相邻二元组；latin 连续字母 ≥3 → 整词；全小写；
    //   Set 去重、保首现序。
    let lower = s.to_lowercase();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let push = |g: String, seen: &mut std::collections::HashSet<String>, out: &mut Vec<String>| {
        if seen.insert(g.clone()) {
            out.push(g);
        }
    };
    let chars: Vec<char> = lower.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if ('\u{4E00}'..='\u{9FA5}').contains(&chars[i]) {
            let start = i;
            while i < chars.len() && ('\u{4E00}'..='\u{9FA5}').contains(&chars[i]) {
                i += 1;
            }
            for w in start..i - 1 {
                push(format!("{}{}", chars[w], chars[w + 1]), &mut seen, &mut out);
            }
        } else if chars[i].is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i - start >= 3 {
                push(chars[start..i].iter().collect(), &mut seen, &mut out);
            }
        } else {
            i += 1;
        }
    }
    out
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

    /// #101 step two: the semantic tier ranks the lexically-shared
    /// episode above a same-importance recency-rival, and is
    /// bit-stable across repeated calls (hash embedder = Pinned).
    #[test]
    fn embedded_retrieval_ranks_semantically_and_stays_deterministic() {
        let mut m = MemoryStore::new();
        m.record(
            "a",
            ep(1, "world", "咖啡豆是埃塞的果酸重等它凉半分钟再喝", 3),
        );
        m.record("a", ep(2, "world", "外面下大雨了记得带伞", 3));
        let emb = oriera_infer::HashEmbeddingProvider::new(64);
        let pick = |q: &str| {
            m.retrieve_embedded(&emb, "a", "world", q, 10, 1)[0]
                .text
                .clone()
        };
        assert!(pick("埃塞咖啡果酸重").contains("咖啡"), "semantic pick #1");
        assert!(pick("外面下雨了怎么办").contains("雨"), "semantic pick #2");
        assert_eq!(
            m.retrieve_embedded(&emb, "a", "world", "咖啡", 10, 2),
            m.retrieve_embedded(&emb, "a", "world", "咖啡", 10, 2),
            "hash embedder = Pinned determinism"
        );
    }

    /// #101: a broken embedder degrades VISIBLY to exactly the offline
    /// tier — never empty results from a broken seat.
    #[test]
    fn broken_embedder_degrades_to_offline_tier() {
        struct Down;
        impl oriera_infer::EmbeddingProvider for Down {
            fn id(&self) -> &str {
                "down"
            }
            fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>, oriera_infer::InferError> {
                Err(oriera_infer::InferError::Provider("down".into()))
            }
        }
        let mut m = MemoryStore::new();
        m.record("a", ep(1, "w", "咖啡凉了", 3));
        let offline = m.retrieve("a", "w", "咖啡凉了", 10, 5);
        let degraded = m.retrieve_embedded(&Down, "a", "w", "咖啡凉了", 10, 5);
        assert_eq!(offline, degraded);
    }

    // ---- 记忆打分可配置：结构—算法分离的技术落点 ----
    //
    // 病灶：系数写死在 `retrieve` 里，既不可消融也不可替换。系数搬进
    // `MemoryScoring` 之后，下面三条测试就是这套搬动的验收：
    // 默认逐位等于历史公式 / 分项可复算总分 / 单变量置零是消融而非重写。

    #[test]
    fn scoring_default_reproduces_the_historic_formula() {
        let s = MemoryScoring::default();
        assert_eq!(s.keyword_weight, 1.6);
        assert_eq!(s.importance_weight, 0.45);
        assert_eq!(s.recency_weight, 2.0);
        assert_eq!(s.about_bonus, 1.2);
        assert_eq!(s.semantic_weight, 2.0);
        assert_eq!(s.min_score, 0.8);
        // Default store carries it — no caller has to know the numbers.
        assert_eq!(MemoryStore::new().scoring(), s);
        assert_eq!(MemoryStore::with_capacity(200).scoring(), s);
    }

    #[test]
    fn traced_terms_reconstruct_the_ranked_score() {
        let mut m = MemoryStore::new();
        m.record("a", ep(1, "老周", "老周记得每个人的口味", 3));
        m.record("a", ep(2, "world", "雨季来了", 2));
        let scored = m.retrieve_traced("a", "老周", "口味", 10, 5);
        assert!(!scored.is_empty());
        for s in &scored {
            let t = s.trace;
            let sum = t.keyword_term
                + t.importance_term
                + t.recency_term
                + t.about_bonus
                + t.semantic_term;
            assert!(
                (sum - t.total).abs() < 1e-5,
                "分项必须复算出总分（审计要能复算名次）"
            );
            assert_eq!(t.semantic_term, 0.0, "offline tier carries no cosine");
            assert!(t.total > MemoryScoring::default().min_score);
        }
        // Plain tier = traced tier minus the trace (no drift between them).
        let plain = m.retrieve("a", "老周", "口味", 10, 5);
        let traced: Vec<Episode> = scored.into_iter().map(|s| s.episode).collect();
        assert_eq!(plain, traced);
    }

    #[test]
    fn zeroing_one_coefficient_is_an_ablation_not_a_rewrite() {
        let mut m = MemoryStore::new();
        // Old but lexically dense, vs recent but lexically empty.
        m.record("a", ep(1, "world", "咖啡 咖啡豆 咖啡机", 1));
        m.record("a", ep(9, "world", "今天下雨了", 1));

        let baseline = m.retrieve("a", "world", "咖啡", 10, 5);
        assert!(baseline[0].text.contains("咖啡"), "keyword term dominates");

        // Single-coefficient ablation: turn the lexical term off.
        m.set_scoring(MemoryScoring {
            keyword_weight: 0.0,
            ..MemoryScoring::default()
        });
        let ablated = m.retrieve("a", "world", "咖啡", 10, 5);
        assert!(
            ablated[0].text.contains("下雨"),
            "keyword off → recency decides (系数确实在起作用)"
        );
        let t = m.retrieve_traced("a", "world", "咖啡", 10, 5);
        assert!(
            t.iter().all(|s| s.trace.keyword_term == 0.0),
            "置零后关键词项必须恒等于 0，不能残留隐性权重"
        );

        // Restoring the default restores the historic ranking exactly.
        m.set_scoring(MemoryScoring::default());
        assert_eq!(m.retrieve("a", "world", "咖啡", 10, 5), baseline);
    }

    #[test]
    fn non_finite_and_custom_gates_are_sanitized_not_poisoning() {
        let dirty = MemoryScoring {
            keyword_weight: f32::NAN,
            min_score: f32::INFINITY,
            ..MemoryScoring::default()
        };
        let clean = dirty.sanitized();
        assert_eq!(clean.keyword_weight, 1.6);
        assert_eq!(clean.min_score, 0.8);

        // A raised gate is honored (empty result, not a panic).
        let mut m = MemoryStore::new();
        m.record("a", ep(1, "world", "咖啡凉了", 1));
        m.set_scoring(MemoryScoring {
            min_score: 100.0,
            ..MemoryScoring::default()
        });
        assert!(m.retrieve("a", "world", "咖啡", 1, 5).is_empty());
    }

    /// 语义项置零 = 严格退回离线档（同一排序、同一集合）——这是把
    /// `semantic_weight` 作为可消融旋钮的直接证据。
    #[test]
    fn zero_semantic_weight_equals_the_offline_tier() {
        let mut m = MemoryStore::new();
        m.record("a", ep(1, "world", "咖啡豆是埃塞的果酸", 3));
        m.record("a", ep(2, "world", "外面下大雨了", 3));
        m.set_scoring(MemoryScoring {
            semantic_weight: 0.0,
            ..MemoryScoring::default()
        });
        let emb = oriera_infer::HashEmbeddingProvider::new(64);
        for q in ["咖啡", "下雨"] {
            assert_eq!(
                m.retrieve_embedded(&emb, "a", "world", q, 10, 2),
                m.retrieve("a", "world", q, 10, 2),
                "semantic off must be byte-identical to the offline tier"
            );
        }
    }
}

/// #139 真对齐回归钉（架构师三轮评审抓包：此前 commit 声称对齐但实现
/// 仍字符对+不去重）。与 kernel.js zhBigrams 同规格：CJK 相邻二元组、
/// latin 纯字母 ≥3 整词、全小写、去重保首现序。
#[cfg(test)]
mod bigram_grammar_tests {
    use super::bigrams;

    #[test]
    fn grammar_matches_kernel_zhbigrams() {
        // CJK 段成对；latin 整词（非字符对）；短 latin（<3）丢弃
        assert_eq!(bigrams("咖啡豆"), vec!["咖啡", "啡豆"]);
        assert_eq!(
            bigrams("coffee 茶"),
            vec!["coffee"],
            "单字 CJK 无相邻对（kernel 同规格）"
        );
        assert_eq!(bigrams("ab Coffee"), vec!["coffee"], "小写化+短词丢弃+整词");
        // 去重保首现序
        assert_eq!(bigrams("咖啡咖啡"), vec!["咖啡", "啡咖"], "去重+尾字不成对");
        // 中英混排：CJK 段独立成对、latin 整词独立、互不粘连
        assert_eq!(bigrams("来杯coffee好喝"), vec!["来杯", "coffee", "好喝"]);
    }
}
