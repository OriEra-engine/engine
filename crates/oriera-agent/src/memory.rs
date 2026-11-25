//! L9 long-term memory — episodic store with retrieval (book T12).
//!
//! An engine service, not application state: cognition systems call
//! `record`/`retrieve` and persistence is a runtime concern (localStorage
//! in the wasm kernel, `Serialize` on L3 in native builds).

use std::collections::HashMap;
use std::sync::Mutex;

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

/// One audited recall decision — what was asked, with which coefficients,
/// and what came back. A report can re-derive the ranking from this
/// without re-running the store.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallEvent {
    /// Monotonic 1-based index inside the ledger.
    pub seq: u64,
    /// World turn the recall happened on.
    pub turn: u64,
    pub agent: String,
    pub about: String,
    pub query: String,
    pub k: usize,
    /// Hits that survived the gate (before the `k` cap).
    pub returned: usize,
    /// Coefficients in force for this decision.
    pub scoring: MemoryScoring,
    /// Top hits with their per-term breakdown.
    pub top: Vec<RecallHit>,
}

/// One returned episode inside a [`RecallEvent`].
#[derive(Debug, Clone, PartialEq)]
pub struct RecallHit {
    pub about: String,
    pub text: String,
    pub total: f32,
    pub trace: RecallTrace,
}

impl RecallEvent {
    /// Build an event from a traced recall. `top` keeps the same order the
    /// caller received.
    pub fn from_traced(
        turn: u64,
        agent: &str,
        about: &str,
        query: &str,
        k: usize,
        scoring: MemoryScoring,
        hits: &[ScoredEpisode],
    ) -> Self {
        Self {
            seq: 0, // assigned by the ledger on push
            turn,
            agent: agent.to_string(),
            about: about.to_string(),
            query: query.to_string(),
            k,
            returned: hits.len(),
            scoring,
            top: hits
                .iter()
                .map(|h| RecallHit {
                    about: h.episode.about.clone(),
                    text: h.episode.text.clone(),
                    total: h.trace.total,
                    trace: h.trace,
                })
                .collect(),
        }
    }
}

/// 召回台账容量（环形：满了丢最旧）。观察记录，不是世界状态。
pub const RECALL_LEDGER_CAPACITY: usize = 256;

#[derive(Debug, Clone, Default)]
struct LedgerInner {
    seq: u64,
    events: Vec<RecallEvent>,
}

/// Opt-in recall ledger: an **observation** record, never world state
/// (so it stays out of `state_hash` and out of金样指纹). Interior
/// mutability is deliberate — a system that only holds `&World` (the
/// cognition pass) can still append, and an app that never registers one
/// pays exactly nothing.
///
/// 并发说明：多槽并发下事件落库顺序取决于取锁顺序——与
/// `DialogueLog` 落地序同一类不确定性，报告若需复现请配合
/// `DeterminismLevel::Recorded`。
#[derive(Debug, Default)]
pub struct RecallLedger {
    inner: Mutex<LedgerInner>,
}

impl Clone for RecallLedger {
    fn clone(&self) -> Self {
        Self {
            inner: Mutex::new(self.lock().clone()),
        }
    }
}

impl RecallLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Poisoned lock still yields the data (self-healing: 台账坏了不该
    /// 让整个世界停摆)。
    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Append one decision; `seq` is assigned here so callers can't
    /// forge ordering.
    pub fn push(&self, mut event: RecallEvent) {
        let mut g = self.lock();
        g.seq += 1;
        event.seq = g.seq;
        g.events.push(event);
        let len = g.events.len();
        if len > RECALL_LEDGER_CAPACITY {
            g.events.drain(0..len - RECALL_LEDGER_CAPACITY);
        }
    }

    pub fn len(&self) -> usize {
        self.lock().events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().events.is_empty()
    }

    /// Snapshot of all events (newest last).
    pub fn events(&self) -> Vec<RecallEvent> {
        self.lock().events.clone()
    }
}

/// Recall-ledger JSON, behind the optional `json` feature — the artifact
/// an evaluation report attaches so "why this memory surfaced" is
/// reproducible instead of taken on trust.
#[cfg(feature = "json")]
impl RecallLedger {
    pub fn to_json(&self) -> String {
        self.to_value().to_string()
    }

    /// The ledger as a JSON value — an evaluation report embeds this
    /// verbatim (`HeadlessRunAccount::attach_recall`).
    pub fn to_value(&self) -> serde_json::Value {
        use serde_json::{Map, Value};
        let g = self.lock();
        let mut root = Map::new();
        root.insert("version".into(), Value::from(1u64));
        let scoring_json = |s: &MemoryScoring| -> Value {
            let mut m = Map::new();
            m.insert(
                "keyword_weight".into(),
                Value::from(f64::from(s.keyword_weight)),
            );
            m.insert(
                "importance_weight".into(),
                Value::from(f64::from(s.importance_weight)),
            );
            m.insert(
                "recency_weight".into(),
                Value::from(f64::from(s.recency_weight)),
            );
            m.insert("about_bonus".into(), Value::from(f64::from(s.about_bonus)));
            m.insert(
                "semantic_weight".into(),
                Value::from(f64::from(s.semantic_weight)),
            );
            m.insert("min_score".into(), Value::from(f64::from(s.min_score)));
            Value::Object(m)
        };
        let events: Vec<Value> = g
            .events
            .iter()
            .map(|e| {
                let mut m = Map::new();
                m.insert("seq".into(), Value::from(e.seq));
                m.insert("turn".into(), Value::from(e.turn));
                m.insert("agent".into(), Value::from(e.agent.clone()));
                m.insert("about".into(), Value::from(e.about.clone()));
                m.insert("query".into(), Value::from(e.query.clone()));
                m.insert("k".into(), Value::from(e.k as u64));
                m.insert("returned".into(), Value::from(e.returned as u64));
                m.insert("scoring".into(), scoring_json(&e.scoring));
                let top: Vec<Value> = e
                    .top
                    .iter()
                    .map(|h| {
                        let mut hm = Map::new();
                        hm.insert("about".into(), Value::from(h.about.clone()));
                        hm.insert("text".into(), Value::from(h.text.clone()));
                        hm.insert("total".into(), Value::from(f64::from(h.total)));
                        hm.insert(
                            "keyword_hits".into(),
                            Value::from(h.trace.keyword_hits as u64),
                        );
                        hm.insert(
                            "keyword_term".into(),
                            Value::from(f64::from(h.trace.keyword_term)),
                        );
                        hm.insert(
                            "importance_term".into(),
                            Value::from(f64::from(h.trace.importance_term)),
                        );
                        hm.insert(
                            "recency_term".into(),
                            Value::from(f64::from(h.trace.recency_term)),
                        );
                        hm.insert(
                            "about_bonus".into(),
                            Value::from(f64::from(h.trace.about_bonus)),
                        );
                        hm.insert(
                            "semantic_term".into(),
                            Value::from(f64::from(h.trace.semantic_term)),
                        );
                        Value::Object(hm)
                    })
                    .collect();
                m.insert("top".into(), Value::Array(top));
                Value::Object(m)
            })
            .collect();
        root.insert("events".into(), Value::Array(events));
        Value::Object(root)
    }
}

/// What a backend needs to answer one recall — all borrowed, so a
/// retrieve costs no allocation for agent/about/query.
#[derive(Debug, Clone, Copy)]
pub struct RetrievalRequest<'a> {
    pub agent: &'a str,
    pub about: &'a str,
    pub query: &'a str,
    pub now: u64,
    pub k: usize,
    /// Ranking policy travels WITH the request: a backend that ranks its
    /// own way (a remote graph store) may ignore it; the facade still
    /// reports it back through `scoring()`.
    pub scoring: MemoryScoring,
}

/// Pluggable storage + recall behind [`MemoryStore`].
///
/// 认知 / 目标系统只认 `MemoryStore` 这个门面；换后端是装配期的一次
/// `with_backend`，不是调用方逐处改类型——这是 R3（记忆后端解耦）的
/// 落点，也是"接远程/图后端之前先有接口"的前提（否则接进来就是第二个
/// 黑箱）。
///
/// 契约：
/// * `agents()` 必须**零分配**地报名（门面的 `agents()` 迭代器借用它），
///   所以后端自己维护一张名字索引；
/// * `episodes()` 对读不出内容的存储可返回 `None`，`to_json` 随之跳过
///   该 agent，绝不编造内容；
/// * `retrieve_embedded` 默认退回离线档——"不支持向量"要**可见地降级**，
///   不允许看起来健康的座位返回空结果（同 #101 契约）。
pub trait MemoryBackend: std::fmt::Debug + Send + Sync {
    fn record(&mut self, agent: &str, episode: Episode);
    fn retrieve(&self, req: &RetrievalRequest<'_>) -> Vec<ScoredEpisode>;
    fn retrieve_embedded(
        &self,
        _embedder: &dyn oriera_infer::EmbeddingProvider,
        req: &RetrievalRequest<'_>,
    ) -> Vec<ScoredEpisode> {
        self.retrieve(req)
    }
    /// Keep at most `capacity` newest episodes for `agent`.
    fn prune(&mut self, agent: &str, capacity: usize);
    fn episodes(&self, agent: &str) -> Option<&[Episode]> {
        let _ = agent;
        None
    }
    fn agents(&self) -> &[String];
    fn len_total(&self) -> usize;
    fn is_empty(&self) -> bool;
    /// Future consolidation (summary / forget) hook; no-op today so a
    /// backend can opt in without touching the facade.
    fn consolidate(&mut self) {}
    /// Keeps `MemoryStore: Clone`（克隆旧 store = 回滚路径）。
    fn clone_backend(&self) -> Box<dyn MemoryBackend>;
}

impl Clone for Box<dyn MemoryBackend> {
    fn clone(&self) -> Self {
        self.clone_backend()
    }
}

/// The default backend: per-agent episodic vectors in memory, the sliding
/// window enforced by [`MemoryBackend::prune`], and scoring exactly the
/// historic formula (through [`score_of`]).
#[derive(Debug, Clone, Default)]
pub struct LocalMemoryBackend {
    memories: HashMap<String, Vec<Episode>>,
    /// First-record order, so `agents()` can borrow without allocating.
    names: Vec<String>,
}

impl MemoryBackend for LocalMemoryBackend {
    fn record(&mut self, agent: &str, episode: Episode) {
        if !self.memories.contains_key(agent) {
            self.names.push(agent.to_string());
        }
        self.memories
            .entry(agent.to_string())
            .or_default()
            .push(episode);
    }

    fn prune(&mut self, agent: &str, capacity: usize) {
        if let Some(v) = self.memories.get_mut(agent) {
            let len = v.len();
            if len > capacity {
                v.drain(0..len - capacity);
            }
        }
    }

    fn retrieve(&self, req: &RetrievalRequest<'_>) -> Vec<ScoredEpisode> {
        let Some(v) = self.memories.get(req.agent) else {
            return Vec::new();
        };
        let q = bigrams(&format!("{}{}", req.query, req.about));
        let mut scored: Vec<(f32, RecallTrace, &Episode)> = v
            .iter()
            .map(|e| {
                let text = bigrams(&format!("{}{}", e.text, e.about));
                let hits = q.iter().filter(|g| text.contains(*g)).count();
                let trace = score_of(e, hits, req.now, req.about, 0.0, req.scoring);
                (trace.total, trace, e)
            })
            .filter(|(s, _, _)| *s > req.scoring.min_score)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored
            .into_iter()
            .take(req.k)
            .map(|(_, trace, e)| ScoredEpisode {
                episode: e.clone(),
                trace,
            })
            .collect()
    }

    fn retrieve_embedded(
        &self,
        embedder: &dyn oriera_infer::EmbeddingProvider,
        req: &RetrievalRequest<'_>,
    ) -> Vec<ScoredEpisode> {
        let Some(v) = self.memories.get(req.agent) else {
            return Vec::new();
        };
        let mut inputs: Vec<String> = Vec::with_capacity(v.len() + 1);
        inputs.push(format!("{}{}", req.query, req.about));
        inputs.extend(v.iter().map(|e| format!("{}{}", e.text, e.about)));
        let vecs = match embedder.embed(&inputs) {
            Ok(vs) if vs.len() == inputs.len() => vs,
            _ => return self.retrieve(req),
        };
        let (q, rows) = vecs.split_first().expect("query vector present");
        let q_bigrams = bigrams(&format!("{}{}", req.query, req.about));
        let mut scored: Vec<(f32, RecallTrace, &Episode)> = v
            .iter()
            .zip(rows)
            .map(|(e, emb)| {
                let text = bigrams(&format!("{}{}", e.text, e.about));
                let hits = q_bigrams.iter().filter(|g| text.contains(*g)).count();
                // 语义项无 +1 基线（AI 原生三轮评审 P1）：曾用 2×(cos+1)/2
                // 使无关文本也白得 ~1.0 分，0.8 过滤被整体中和、陈旧条目
                // 全放行——负/零相关不得白得分。
                let trace = score_of(e, hits, req.now, req.about, cosine(q, emb), req.scoring);
                (trace.total, trace, e)
            })
            .filter(|(s, _, _)| *s > req.scoring.min_score)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored
            .into_iter()
            .take(req.k)
            .map(|(_, trace, e)| ScoredEpisode {
                episode: e.clone(),
                trace,
            })
            .collect()
    }

    fn episodes(&self, agent: &str) -> Option<&[Episode]> {
        self.memories.get(agent).map(Vec::as_slice)
    }

    fn agents(&self) -> &[String] {
        &self.names
    }

    fn len_total(&self) -> usize {
        self.memories.values().map(|v| v.len()).sum()
    }

    fn is_empty(&self) -> bool {
        self.memories.values().all(|v| v.is_empty())
    }

    fn clone_backend(&self) -> Box<dyn MemoryBackend> {
        Box::new(self.clone())
    }
}

/// Per-agent episodic memory facade with keyword-recency-importance
/// retrieval. Storage and recall live behind a [`MemoryBackend`];
/// the public API here is what cognition, goal and every existing caller
/// already uses (`record` / `retrieve` / `retrieve_traced` …), so a
/// backend swap touches no call site.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    backend: Box<dyn MemoryBackend>,
    /// Sliding-window cap per agent; `Default` = 60 (the long-standing
    /// semantic, kept for every existing caller).
    capacity: usize,
    /// Ranking coefficients; `Default` = the historic formula.
    scoring: MemoryScoring,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self {
            backend: Box::new(LocalMemoryBackend::default()),
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

    /// Swap in any backend at wiring time (remote RAG / graph store …).
    /// The facade API — and therefore every cognition / goal call site —
    /// stays exactly as it is.
    pub fn with_backend(
        capacity: usize,
        scoring: MemoryScoring,
        backend: Box<dyn MemoryBackend>,
    ) -> Self {
        Self {
            backend,
            capacity: capacity.max(1),
            scoring: scoring.sanitized(),
        }
    }

    /// Read-only view of the attached backend (diagnostics / tests).
    pub fn backend(&self) -> &dyn MemoryBackend {
        self.backend.as_ref()
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

    /// Record an episode for `agent` (backend write + capacity window).
    pub fn record(&mut self, agent: &str, episode: Episode) {
        self.backend.record(agent, episode);
        self.backend.prune(agent, self.capacity);
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
        self.backend.retrieve(&RetrievalRequest {
            agent,
            about,
            query,
            now,
            k,
            scoring: self.scoring,
        })
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
        self.backend.retrieve_embedded(
            embedder,
            &RetrievalRequest {
                agent,
                about,
                query,
                now,
                k,
                scoring: self.scoring,
            },
        )
    }

    /// Total episodes across agents (metrics).
    pub fn len_total(&self) -> usize {
        self.backend.len_total()
    }

    /// All agents that hold at least one episode.
    pub fn agents(&self) -> impl Iterator<Item = &str> {
        self.backend.agents().iter().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.backend.is_empty()
    }
}

/// The ONE ranking formula both tiers blend around —
/// `hits×kw + importance×iw + recency×rw + about-bonus + semantic×sw`,
/// evaluated into a [`RecallTrace`] so every term stays auditable.
fn score_of(
    e: &Episode,
    hits: usize,
    now: u64,
    about: &str,
    semantic: f32,
    s: MemoryScoring,
) -> RecallTrace {
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
        let mut agents: Vec<&String> = self.backend.agents().iter().collect();
        agents.sort();
        for agent in agents {
            // Backends that cannot export (remote/graph) yield None here:
            // skip them rather than fabricate content.
            let Some(eps) = self.backend.episodes(agent) else {
                continue;
            };
            let episodes: Vec<Value> = eps
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

    // ---- R3：换后端不改调用点 ----
    //
    // 门面（MemoryStore）保留全部公开 API，后端进 `Box<dyn MemoryBackend>`。
    // 下面这条用一个"假远程"后端证明四件事：写入路由到后端 / 召回由后端
    // 作答 / 不支持向量时可见降级 / 读不出内容时不编造 JSON。

    /// A backend that behaves like a remote store: it ranks its own way,
    /// cannot export raw episodes, and has no vector tier.
    #[derive(Debug, Default)]
    struct CannedBackend {
        writes: usize,
        names: Vec<String>,
    }

    impl MemoryBackend for CannedBackend {
        fn record(&mut self, agent: &str, _episode: Episode) {
            self.writes += 1;
            if !self.names.iter().any(|n| n == agent) {
                self.names.push(agent.to_string());
            }
        }

        fn retrieve(&self, req: &RetrievalRequest<'_>) -> Vec<ScoredEpisode> {
            // 远程库按自己的方式排序，`req.scoring` 它可以不认。
            if req.k == 0 {
                return Vec::new();
            }
            vec![ScoredEpisode {
                episode: Episode {
                    turn: 999,
                    day: 9,
                    about: "remote".into(),
                    text: format!("remote:{}", req.query),
                    importance: 5,
                },
                trace: RecallTrace {
                    keyword_hits: 0,
                    keyword_term: 0.0,
                    importance_term: 0.0,
                    recency_term: 0.0,
                    about_bonus: 0.0,
                    semantic_term: 0.0,
                    total: 0.0,
                },
            }]
        }

        fn prune(&mut self, _agent: &str, _capacity: usize) {}

        fn agents(&self) -> &[String] {
            &self.names
        }

        fn len_total(&self) -> usize {
            self.writes
        }

        fn is_empty(&self) -> bool {
            self.writes == 0
        }

        fn clone_backend(&self) -> Box<dyn MemoryBackend> {
            Box::new(Self {
                writes: self.writes,
                names: self.names.clone(),
            })
        }
    }

    #[test]
    fn a_remote_backend_swaps_in_without_touching_any_call_site() {
        let mut store = MemoryStore::with_backend(
            60,
            MemoryScoring::default(),
            Box::new(CannedBackend::default()),
        );

        // 1) 写入路由到后端，而不是本地 HashMap。
        store.record("aria", ep(1, "world", "本地也想记一条", 3));
        assert_eq!(store.len_total(), 1, "record must reach the backend");
        assert_eq!(store.agents().collect::<Vec<_>>(), vec!["aria"]);

        // 2) 召回由后端作答（文本前缀即证据），调用点签名一字未改。
        let got = store.retrieve("aria", "world", "咖啡", 10, 2);
        assert_eq!(got.len(), 1);
        assert!(
            got[0].text.starts_with("remote:"),
            "recall must be answered by the backend, not local scoring"
        );

        // 3) 不支持向量 → 默认实现可见地退回离线档，绝不返回空结果。
        let emb = oriera_infer::HashEmbeddingProvider::new(64);
        let degraded = store.retrieve_embedded(&emb, "aria", "world", "咖啡", 10, 2);
        assert_eq!(degraded.len(), 1, "no-vector backend degrades visibly");

        // 4) 读不出原始条目 → to_json 跳过该 agent，不编造内容。
        #[cfg(feature = "json")]
        {
            let json = store.to_json();
            assert!(json.contains("\"version\":1"));
            assert!(!json.contains("remote:"), "never fabricate exports");
            assert!(!json.contains("aria"), "unexportable agent must be omitted");
        }

        // 5) 克隆 = 回滚路径，且克隆彼此独立。
        let mut rollback = store.clone();
        rollback.record("aria", ep(2, "world", "回滚后再记一条", 1));
        assert_eq!(rollback.len_total(), 2);
        assert_eq!(store.len_total(), 1, "clone must not share the backend");

        // 6) 门面仍如实报告调用方设定的打分系数。
        assert_eq!(store.scoring(), MemoryScoring::default());
    }

    /// 门面发号与本地后端一致：首次出现顺序、零分配借用可迭代。
    #[test]
    fn agent_names_follow_first_record_order() {
        let mut m = MemoryStore::new();
        m.record("z", ep(1, "w", "先来", 1));
        m.record("a", ep(2, "w", "后到", 1));
        assert_eq!(m.agents().collect::<Vec<_>>(), vec!["z", "a"]);
    }

    // ---- 召回台账：观察记录，不是世界状态 ----

    fn bare_event(turn: u64, agent: &str) -> RecallEvent {
        RecallEvent {
            seq: 0,
            turn,
            agent: agent.into(),
            about: "world".into(),
            query: "咖啡".into(),
            k: 4,
            returned: 0,
            scoring: MemoryScoring::default(),
            top: Vec::new(),
        }
    }

    #[test]
    fn ledger_assigns_monotonic_seq_and_keeps_the_newest_window() {
        let led = RecallLedger::new();
        for i in 0..(RECALL_LEDGER_CAPACITY as u64 + 10) {
            led.push(bare_event(i, "aria"));
        }
        let ev = led.events();
        assert_eq!(ev.len(), RECALL_LEDGER_CAPACITY, "ring keeps the window");
        assert_eq!(
            ev.last().unwrap().seq,
            RECALL_LEDGER_CAPACITY as u64 + 10,
            "seq keeps counting past the ring"
        );
        assert_eq!(ev.first().unwrap().seq, 11, "oldest dropped, seq not reset");
        assert_eq!(led.len(), RECALL_LEDGER_CAPACITY);
        assert!(!led.is_empty());
    }

    #[test]
    fn ledger_clone_is_an_independent_snapshot() {
        let led = RecallLedger::new();
        led.push(bare_event(1, "a"));
        let snap = led.clone();
        led.push(bare_event(2, "b"));
        assert_eq!(snap.len(), 1, "clone = 快照（回滚/对照用）");
        assert_eq!(led.len(), 2);
    }

    #[test]
    fn event_from_traced_keeps_order_and_terms() {
        let mut m = MemoryStore::new();
        m.record("a", ep(1, "world", "咖啡豆是埃塞的", 3));
        m.record("a", ep(2, "world", "下雨了", 2));
        let scored = m.retrieve_traced("a", "world", "咖啡", 10, 2);
        let ev = RecallEvent::from_traced(10, "a", "world", "咖啡", 2, m.scoring(), &scored);
        assert_eq!(ev.returned, scored.len());
        assert_eq!(ev.top.len(), scored.len());
        for (hit, s) in ev.top.iter().zip(scored.iter()) {
            assert_eq!(hit.text, s.episode.text, "order preserved");
            assert_eq!(hit.total, s.trace.total, "terms ride along verbatim");
        }
        assert_eq!(ev.seq, 0, "push assigns seq, the builder does not");
    }

    #[cfg(feature = "json")]
    #[test]
    fn ledger_json_carries_scoring_and_per_term_breakdown() {
        let led = RecallLedger::new();
        let mut m = MemoryStore::new();
        m.record("a", ep(1, "world", "咖啡豆是埃塞的", 3));
        let scored = m.retrieve_traced("a", "world", "咖啡", 10, 4);
        led.push(RecallEvent::from_traced(
            10,
            "a",
            "world",
            "咖啡",
            4,
            m.scoring(),
            &scored,
        ));
        let v: serde_json::Value = serde_json::from_str(&led.to_json()).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["events"][0]["seq"], 1);
        assert_eq!(v["events"][0]["agent"], "a");
        assert_eq!(
            v["events"][0]["scoring"]["keyword_weight"]
                .as_f64()
                .unwrap(),
            f64::from(MemoryScoring::default().keyword_weight),
            "系数原样落盘（f32 精度，不四舍五入）"
        );
        assert!(v["events"][0]["top"][0]["total"].as_f64().unwrap() > 0.8);
        assert!(v["events"][0]["top"][0]["keyword_term"].as_f64().unwrap() >= 0.0);
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
