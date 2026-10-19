/* ============================================================
 * OriEra 引擎内核（浏览器/wasm 侧镜像）
 * 与 Rust crate 一一对照：
 *   MemoryStore      ↔ crates/oriera-agent/src/memory.rs   (L9)
 *   CognitionSystem  ↔ crates/oriera-agent/src/cognition.rs (L9)
 *   DirectorSystem   ↔ 同上（AI 导演）
 *   ZhipuProvider    ↔ crates/oriera-infer/src/provider.rs  (L7 ModelProvider)
 *   parseIntent      ↔ crates/oriera-agent/src/lib.rs::parse (L7 命令通道)
 *   CommandQueue     ↔ crates/oriera-infer/src/lib.rs::InferSystem (L7 队列)
 * v2（2026-10-19，八赛道实战反馈）：命令通道 / 认知参数化（场景·台词库·
 * RNG·max 可注入）/ 记忆容量参数化 / seed·thinking 透传 / 预设表 13 行。
 * 全部加法式：不传新参数时行为与 v1 逐位一致。
 * 应用（index.html）只 import 这些服务——应用代码里不允许出现
 * 任何模型调用、Key、prompt 拼装。
 * ============================================================ */

/* ---------- L9 · 长期记忆（与 Rust MemoryStore 同构） ---------- */
export class MemoryStore {
  /** capacity 可注入（v2）：默认 60 保持 v1 语义。Node 测试环境无
   * localStorage 时静默降级为纯内存（kernel 可被 node --test 直测）。 */
  constructor(persistKey = "aicafe.mem.v1", capacity = 60) {
    this.persistKey = persistKey;
    this.capacity = capacity;
    this.mem = {};
    try {
      if (typeof localStorage !== "undefined") {
        const saved = JSON.parse(localStorage.getItem(persistKey) || "null");
        if (saved) this.mem = saved;
      }
    } catch { /* 损坏的存储当作空白世界 */ }
  }
  record(agent, ep) {
    const v = (this.mem[agent] ||= []);
    v.push(ep);
    if (v.length > this.capacity) v.splice(0, v.length - this.capacity);
    this._save();
  }
  retrieve(agent, about, query, now, k = 4) {
    const bigrams = s => {
      const out = new Set();
      for (const seg of s.match(/[\u4e00-\u9fa5]+/g) || [])
        for (let i = 0; i < seg.length - 1; i++) out.add(seg.substr(i, 2));
      for (const w of (s.toLowerCase().match(/[a-z]{3,}/g) || [])) out.add(w);
      return out;
    };
    const q = bigrams(query + about);
    return (this.mem[agent] || [])
      .map(e => {
        const text = bigrams(e.text + e.about);
        let hits = 0; for (const g of q) if (text.has(g)) hits++;
        const recency = 1 / (1 + (now - e.turn));
        const aboutBonus = e.about === about ? 1.2 : 0;
        return { e, score: hits * 1.6 + e.imp * 0.45 + recency * 2 + aboutBonus };
      })
      .filter(x => x.score > 0.8)
      .sort((a, b) => b.score - a.score).slice(0, k).map(x => x.e);
  }
  agents() { return Object.keys(this.mem); }
  lenTotal() { return Object.values(this.mem).reduce((a, v) => a + v.length, 0); }
  clear() { this.mem = {}; if (typeof localStorage !== "undefined") localStorage.removeItem(this.persistKey); }
  export() { return JSON.stringify(this.mem, null, 2); }
  _save() { if (typeof localStorage !== "undefined") localStorage.setItem(this.persistKey, JSON.stringify(this.mem)); }
}

/* ---------- L7 · ModelProvider：创作者自选大模型 ----------
 * 与 Rust oriera-infer::create_provider 工厂一一对应。
 * 任何 OpenAI 兼容端点（智谱/DeepSeek/Moonshot/Groq/OpenRouter/vLLM）
 * 走 HttpChatProvider；Anthropic 走专用头；rule 为离线后端。
 * ----------------------------------------------------------- */
export class HttpChatProvider {
  /** cfg: {kind, key, model, base} —— Key 只存在浏览器 localStorage */
  constructor(cfg) { this.cfg = cfg; this.stats = { calls: 0, errors: 0 }; }
  get id() { return (this.cfg.kind || "openai-compat") + ":" + (this.cfg.model || "?"); }
  get ready() { return !!this.cfg.key; }
  async complete(messages, opts = {}) {
    if (!this.ready) throw new Error("no key");
    this.stats.calls++;
    const anthropic = this.cfg.kind === "anthropic";
    const url = ((this.cfg.base || "") + (anthropic ? "/messages" : "/chat/completions"));
    const headers = { "Content-Type": "application/json" };
    if (anthropic) {
      headers["x-api-key"] = this.cfg.key;
      headers["anthropic-version"] = "2023-06-01";
      headers["anthropic-dangerous-direct-browser-access"] = "true";
    } else headers["Authorization"] = "Bearer " + this.cfg.key;
    const body = anthropic
      ? JSON.stringify({ model: this.cfg.model, max_tokens: opts.max ?? 260,
          system: messages.filter(m => m.role === "system").map(m => m.content).join("\n"),
          messages: messages.filter(m => m.role !== "system").map(m => ({ role: m.role, content: m.content })) })
      : JSON.stringify({ model: this.cfg.model, messages,
          temperature: opts.temp ?? 0.85, max_tokens: opts.max ?? 260,
          ...(opts.json ? { response_format: { type: "json_object" } } : {}),
          // v2 透传：seed（复现性，OpenAI 兼容方言）与 thinking（推理型
          // 模型如 DeepSeek-V4 默认 high 档会吃掉 max_tokens 预算，NPC
          // 对话显式关）。anthropic 分支不动——其协议无对应参数。
          ...(opts.seed !== undefined ? { seed: opts.seed } : {}),
          ...(opts.thinking ? { thinking: opts.thinking } : {}) });
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        const r = await fetch(url, { method: "POST", headers, body, signal: AbortSignal.timeout(20000) });
        if (!r.ok) throw new Error("HTTP " + r.status);
        const data = await r.json();
        this.stats.tokens = (this.stats.tokens || 0)
          + ((data.usage && (data.usage.total_tokens ?? data.usage.input_tokens)) || 0);
        return anthropic ? data.content[0].text : data.choices[0].message.content;
      } catch (e) {
        if (attempt) { this.stats.errors++; throw e; }
        await new Promise(z => setTimeout(z, 900));
      }
    }
  }
}
/** 兼容旧名 */
export class ZhipuProvider {
  /** cfg: {key, model, base} —— Key 只存在浏览器 localStorage */
  constructor(cfg) { this.cfg = cfg; this.stats = { calls: 0, errors: 0 }; }
  get id() { return this.cfg.model || "zhipu"; }
  get ready() { return !!this.cfg.key; }
  async complete(messages, opts = {}) {
    if (!this.ready) throw new Error("no key");
    this.stats.calls++;
    const body = JSON.stringify({
      model: this.cfg.model || "glm-4-flash", messages,
      temperature: opts.temp ?? 0.85, max_tokens: opts.max ?? 260,
      ...(opts.json ? { response_format: { type: "json_object" } } : {}),
    });
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        const r = await fetch((this.cfg.base || "https://open.bigmodel.cn/api/paas/v4") + "/chat/completions", {
          method: "POST",
          headers: { "Content-Type": "application/json", Authorization: "Bearer " + this.cfg.key },
          body,
        });
        if (!r.ok) throw new Error("HTTP " + r.status);
        const data = await r.json();
        return data.choices[0].message.content;
      } catch (e) {
        if (attempt) { this.stats.errors++; throw e; }
        await new Promise(z => setTimeout(z, 900));
      }
    }
  }
}

/* ---------- L9 · 认知系统（每 interval tick 一位 NPC 说话） ---------- */
const MOODS = ["happy", "neutral", "sad", "excited", "thoughtful"];
const ACTIONS = ["idle", "wave", "dance", "sitting", "walk_to_counter", "yes", "thumbs_up"];
const extractJSON = s => { const m = (s || "").match(/\{[\s\S]*\}/);
  try { return m ? JSON.parse(m[0]) : null; } catch { return null; } };

export class CognitionSystem {
  /** v2 可注入：scene 世界前缀（默认隅角咖啡馆保持 v1 兼容）、
   * maxTokens（默认 260=provider 旧默认，线协议不变）、rng（种子化
   * 复现，默认 Math.random）、ruleBank（离线台词库，默认咖啡馆三人）。 */
  constructor(provider, memory, { interval = 1, scene, maxTokens, rng, ruleBank } = {}) {
    this.provider = provider; this.memory = memory;
    this.interval = interval; this.cursor = 0;
    this.scene = scene ?? "隅角咖啡馆";
    this.maxTokens = maxTokens ?? 260;
    this.rng = rng ?? Math.random;
    this.ruleBank = ruleBank ?? null;
  }
  async tick(world) {
    world.turn++;
    if (world.turn % this.interval !== 0) return null;
    const npcs = world.npcs;
    if (!npcs.length) return null;
    const speaker = npcs[this.cursor % npcs.length];
    const partner = npcs.length > 1
      ? npcs[(this.cursor + 1 + Math.floor(this.rng() * (npcs.length - 1))) % npcs.length]
      : { name: "客人", role: "刚推门的旅行者", description: "" };
    this.cursor++;
    const guest = world.guestQueue.shift() || null;

    const mems = this.memory.retrieve(speaker.name, partner.name,
      world.dialogue.slice(-6).map(t => t.text).join("") + (guest || ""), world.turn, 4);
    const recent = world.dialogue.slice(-6).map(t => `${t.speaker}：${t.text}`);
    const beat = world.story.beats.at(-1) || "平静的日常";

    const sys = `你是「${speaker.name}」，${speaker.role}，活在 OriEra 世界引擎的${this.scene}里。
性格：${speaker.description}
当前心情：${speaker.mood}。世界进度：第 ${world.story.day} 世界日，${beat}
${guest ? "刚才一位客人说：" + guest + "\n" : ""}
对话对象：「${partner.name}」（${partner.role}）。
你的相关记忆：
${mems.length ? mems.map(m => `- (第${m.day}日) ${m.text}`).join("\n") : "-（暂无，可能是第一次聊这个）"}
最近对话：
${recent.join("\n") || "（对话刚开始）"}
以 ${speaker.name} 的口吻说 1~3 句口语化的话（可引用记忆细节），严格只输出 JSON：
{"say":"...","action":"${ACTIONS.join("|")}","mood":"${MOODS.join("|")}","remember":"≤30字本轮要点，没有则空串","to":"${partner.name}"}`;

    let res;
    if (this.provider.ready) {
      try {
        const out = await this.provider.complete([
          { role: "system", content: sys },
          { role: "user", content: guest ? "（客人刚说完，请回应客人）" : `（请对${partner.name}说下一句话）` },
        ], { json: true, temp: 0.9, max: this.maxTokens });
        const j = extractJSON(out) || {};
        let say = String(j.say || "……").slice(0, 140);
        // 模型常把称呼拼进台词开头，去掉一次重复
        say = say.replace(new RegExp("^" + (j.to || partner.name) + "[，,、:\s]*"), "");
        res = {
          say,
          action: ACTIONS.includes(j.action) ? j.action : "idle",
          mood: MOODS.includes(j.mood) ? j.mood : speaker.mood,
          remember: String(j.remember || "").slice(0, 40),
          to: j.to || partner.name,
        };
      } catch (e) {
        world.onEvent && world.onEvent({ type: "l7-error", error: String(e) });
        res = null;
      }
    }
    if (!res) res = this._rule(speaker, partner, world);   // L7 降级
    speaker.mood = res.mood;

    const rel = (world.rel = world.rel || {});
    const rk = [speaker.name, res.to].sort().join('~');
    rel[rk] = Math.min(100, (rel[rk] || 40) + ((res.mood === 'happy' || res.mood === 'excited') ? 2 : 0.5));
    world.dialogue.push({ speaker: speaker.name, to: res.to, text: res.say });
    if (world.dialogue.length > 24) world.dialogue.shift();
    if (res.remember)
      this.memory.record(speaker.name, { turn: world.turn, day: world.story.day, about: res.to, text: res.remember, imp: 3 });
    this.memory.record(res.to, {
      turn: world.turn, day: world.story.day, about: speaker.name,
      text: `${speaker.name}说了：${res.say.slice(0, 18)}`, imp: 2,
    });
    return { speaker, ...res, guest };
  }
  _rule(speaker, partner, world) {
    const beat = world.story.beats.at(-1) || "";
    const bank = this.ruleBank || {
      zhou: [`${partner.name}，还是老样子？这豆子果酸重，等它凉半分钟。`,
             "开店十二年，我记得每个人第一次点的东西。",
             `（看着窗外）${beat ? "这阵子——" + beat.slice(0, 10) + "，唉。" : "这条街变了很多，好在咖啡没变。"}`],
      aria: [`${partner.name} 你刚那句话能进我的绘本！`,
             "截稿日像窗外的云追着我跑，但今天的光是蜂蜜色的。",
             "我把心情调成一半灰紫一半橘……你今天是什么颜色？"],
      zhe: [`${partner.name}！引擎 demo 又快了一点！`,
            "我在想要不要休学 all-in 一年，像老周当年盘下这家店。",
            "老周说好咖啡要等，代码也一样——等待算不算一种技术？"],
    };
    const lines = bank[speaker.id] || bank.zhou;
    const moods = ["happy", "thoughtful", "excited", "neutral"];
    return {
      say: lines[Math.floor(this.rng() * lines.length)],
      action: this.rng() < 0.25 ? "wave" : "idle",
      mood: moods[Math.floor(this.rng() * moods.length)],
      remember: `与${partner.name}聊了${beat ? "关于" + beat.slice(0, 10) : "日常"}`,
      to: partner.name,
    };
  }
}

/* ---------- L9 · AI 导演（推进世界进程） ---------- */
const RULE_BEATS = [
  "雨季来了，店里放起了爵士", "Aria 的绘本画到'会说话的咖啡机'这一章",
  "阿哲的引擎 demo 第一次跑通", "街对面开了连锁咖啡，老周嘴上不在意",
  "Aria 发现绘本角色开始像店里常客", "阿哲收到大厂 offer，反而更犹豫",
  "停电之夜：三人点着蜡烛聊到打烊", "老周翻出十二年前开店的旧照片",
  "绘本完稿，首发式定在这家店", "阿哲决定休学一年做世界引擎",
  "咖啡馆一周年展：墙上挂满客人的故事", "新的常客推门进来——故事继续",
];
export class DirectorSystem {
  /** v2：ruleBeats 可注入（默认咖啡馆十二拍，保持 v1 兼容）。 */
  constructor(provider, memory, { interval = 8, ruleBeats } = {}) {
    this.provider = provider; this.memory = memory; this.interval = interval; this.beatIdx = -1;
    this.ruleBeats = ruleBeats || RULE_BEATS;
  }
  async tick(world) {
    if (world.turn % this.interval !== 0) return null;
    let beat = null;
    if (this.provider.ready) {
      try {
        const out = await this.provider.complete([
          { role: "system", content: "你是 OriEra 世界引擎的 AI 导演。根据咖啡馆最近发生的事，推进一个小而具体的世界事件（一句话中文，不改变人物命运但让角色有事可聊）。只输出 JSON：{\"beat\":\"一句话事件\"}" },
          { role: "user", content: `第${world.story.day}世界日。已发生：${world.story.beats.slice(-3).join("；") || "无"}\n最近对话：\n${world.dialogue.slice(-8).map(t => `${t.speaker}：${t.text}`).join("\n")}` },
        ], { json: true, temp: 1.0, max: 120 });
        const j = extractJSON(out);
        if (j && j.beat && String(j.beat).length < 60) beat = String(j.beat);
      } catch { /* 降级规则 */ }
    }
    if (!beat) { this.beatIdx = (this.beatIdx + 1) % this.ruleBeats.length; beat = this.ruleBeats[this.beatIdx]; }
    world.story.beats.push(beat); if (world.story.beats.length > 30) world.story.beats.shift();
    if (world.turn % (this.interval * 3) === 0) world.story.day++;
    for (const a of this.memory.agents())
      this.memory.record(a, { turn: world.turn, day: world.story.day, about: "world", text: `世界事件：${beat}`, imp: 4 });
    return beat;
  }
}

/* ---------- L2 · 调度器（应用只 pump，不知道里面有什么系统） ---------- */
export class Scheduler {
  constructor() { this.systems = []; }
  add(sys) { this.systems.push(sys); }
  async tick(world) { for (const s of this.systems) await s.tick(world); }
}

/* ---------- L7 · 命令通道（v2 · 镜像 Rust parse + InferSystem） ----------
 * 八赛道实战（out/verticals 七案）齐声要的缺口：自然语言 → 结构化命令。
 * parseIntent 只做确定性解析（中英+否定语义，与 Rust L9 parse 同词汇表）；
 * 执行器由应用注入（浏览器世界形状归应用，引擎管解析与队列纪律：
 * 帧预算 + 可证安全合并 + 拒绝计数，语义与 Rust InferSystem 一致）。
 * ------------------------------------------------------------------ */
const CMD_NEGATIONS = ["不要", "别", "勿", "don't", "dont", "do not", "never"];
const CMD_INTENTS = ["spawn", "放", "生成", "clear", "清空", "清屏", "gravity", "重力",
  "teleport", "传送", "how many", "多少", "speed", "速度", "倍速"];

export function parseIntent(input) {
  const lower = String(input).toLowerCase();
  const firstNeg = CMD_NEGATIONS.map(n => lower.indexOf(n)).filter(i => i >= 0);
  const firstKw = CMD_INTENTS.map(k => lower.indexOf(k)).filter(i => i >= 0);
  if (firstNeg.length && firstKw.length && Math.min(...firstNeg) < Math.min(...firstKw))
    return { kind: "unknown", text: input };
  const nums = (lower.match(/-?\d+(?:\.\d+)?/g) || []).map(Number);
  if ((lower.includes("spawn") || lower.includes("放") || lower.includes("生成")) && nums.length)
    return { kind: "spawn", n: Math.max(0, Math.round(nums[0])) };
  if (lower.includes("clear") || lower.includes("清空") || lower.includes("清屏"))
    return { kind: "clear" };
  if ((lower.includes("gravity") || lower.includes("重力")) && nums.length)
    return { kind: "gravity", g: nums[0] };
  if ((lower.includes("teleport") || lower.includes("传送")) && nums.length >= 2)
    return { kind: "teleport", dx: nums[0], dy: nums[1] };
  if (lower.includes("how many") || lower.includes("多少"))
    return { kind: "count" };
  if ((lower.includes("speed") || lower.includes("速度") || lower.includes("倍速")) && nums.length)
    return { kind: "speed", k: nums[0] };
  return { kind: "unknown", text: input };
}

export class CommandQueue {
  /** executor: (cmd, world) => void，应用注入；预算/合并/计数归引擎。 */
  constructor(executor, { frameBudget = 32 } = {}) {
    this.executor = executor; this.frameBudget = frameBudget;
    this.queue = [];
    this.processed = 0; this.rejected = 0; this.coalesced = 0;
  }
  submit(text) { this.submitIntent(parseIntent(text)); }
  /** 合并在提交时发生（镜像 Rust InferSystem.submit）：相邻 spawn 求和、
   * 同型 count 重复合一——都是可证安全的世界等价变换。 */
  submitIntent(cmd) {
    const tail = this.queue[this.queue.length - 1];
    if (tail && cmd.kind === "spawn" && tail.kind === "spawn") { tail.n += cmd.n; this.coalesced++; return; }
    if (tail && cmd.kind === "count" && tail.kind === "count") { this.coalesced++; return; }
    this.queue.push(cmd);
  }
  get pending() { return this.queue.length; }
  tick(world) {
    let budget = this.frameBudget;
    while (this.queue.length && budget > 0) {
      const cmd = this.queue.shift();
      budget--; // unknown 也占预算位（与 Rust 一致：出队即耗预算）
      if (cmd.kind === "unknown") { this.rejected++; continue; }
      this.executor(cmd, world);
      this.processed++;
    }
  }
}

/* ---------- L7 · 提供商工厂（镜像 Rust create_provider） ---------- */
export const PROVIDER_PRESETS = {
  rule:        { label: "离线规则引擎",    base: "",                                        model: "rule-v1", local: true },
  zhipu:       { label: "智谱 GLM",       base: "https://open.bigmodel.cn/api/paas/v4", model: "glm-4-flash" },
  openai:      { label: "OpenAI",         base: "https://api.openai.com/v1",             model: "gpt-4o-mini" },
  qwen:        { label: "Qwen 通义",      base: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "qwen-plus" },
  deepseek:    { label: "DeepSeek",       base: "https://api.deepseek.com/v1",           model: "deepseek-chat" },
  moonshot:    { label: "Moonshot Kimi",  base: "https://api.moonshot.cn/v1",            model: "moonshot-v1-8k" },
  groq:        { label: "Groq",           base: "https://api.groq.com/openai/v1",        model: "llama-3.1-8b-instant" },
  openrouter:  { label: "OpenRouter",     base: "https://openrouter.ai/api/v1",          model: "openrouter/auto" },
  anthropic:   { label: "Anthropic",      base: "https://api.anthropic.com/v1",          model: "claude-3-5-haiku-latest" },
  ollama:      { label: "Ollama 本地",     base: "http://127.0.0.1:11434/v1",             model: "qwen3:8b", local: true },
  vllm:        { label: "vLLM 本地",       base: "http://127.0.0.1:8000/v1",              model: "Qwen/Qwen3-8B", local: true },
  llamacpp:    { label: "llama.cpp 本地",  base: "http://127.0.0.1:8080/v1",              model: "glm-4-9b-chat", local: true },
  lmstudio:    { label: "LM Studio 本地",  base: "http://127.0.0.1:1234/v1",              model: "minicpm-v", local: true },
};
export function createProvider(cfg) {
  if (!cfg.kind || cfg.kind === "rule" || !cfg.key) return null; // 认知系统自动降级
  return new HttpChatProvider(cfg);
}
