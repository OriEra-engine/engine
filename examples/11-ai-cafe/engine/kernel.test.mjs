// kernel v2 测试（node --test 直测浏览器内核；kernel 已对无 localStorage
// 环境静默降级）。运行：node --test examples/11-ai-cafe/engine/kernel.test.mjs
import test from "node:test";
import { readFileSync } from "node:fs";
import assert from "node:assert/strict";
import {
  MemoryStore, HttpChatProvider, parseIntent, CommandQueue, GoalLog,
  CognitionSystem, DirectorSystem, PROVIDER_PRESETS, createProvider, fitBudget,
  resolveSlotProvider, SLOT_ROUTING_KEY,
} from "./kernel.js";

const fitBudgetJS = fitBudget; // 显式别名：断言里读得出这是对镜腿

/* ---- golden fixture 单一来源（#98）：与 Rust 双端共读 contracts/golden/*.json。
   改任一侧解析/预设而不更新 fixture，双端 CI 同红——漂移从此有闸门。 ---- */
const INTENT_FIXTURE = new URL("../../../contracts/golden/intents.json", import.meta.url);
const PRESET_FIXTURE = new URL("../../../contracts/golden/presets.json", import.meta.url);

test("parse 镜像：golden fixture 共读（intents.json 单一来源）", () => {
  const { cases } = JSON.parse(readFileSync(INTENT_FIXTURE, "utf8"));
  assert.ok(cases.length >= 20, "fixture must not be emptied");
  for (const { input, expect } of cases) {
    const got = parseIntent(input);
    assert.equal(got.kind, expect.kind, `input: ${input}`);
    if (expect.kind === "spawn") assert.equal(got.n, expect.n, `input: ${input}`);
    if (expect.kind === "gravity") assert.equal(got.g, expect.g, `input: ${input}`);
    if (expect.kind === "teleport") {
      assert.equal(got.dx, expect.dx, `input: ${input}`);
      assert.equal(got.dy, expect.dy, `input: ${input}`);
    }
    if (expect.kind === "speed") assert.equal(got.k, expect.k, `input: ${input}`);
    if (expect.kind === "unknown") assert.equal(got.text, expect.text, `input: ${input}`);
  }
});

test("预设表镜像：golden fixture 共读（presets.json 单一来源）", () => {
  const { rows } = JSON.parse(readFileSync(PRESET_FIXTURE, "utf8"));
  assert.equal(rows.length, Object.keys(PROVIDER_PRESETS).length, "row count out of sync");
  /* kind 推导是 JS 运行时事实（HttpChatProvider 仅对 anthropic 分方言，
     zhipu/rule 语义归位对齐 Rust 词汇表）；这是刻意保留的第二映射点——
     若 Rust 侧行的 ProviderKind 变更，fixture 更新后这里的 deepEqual 会红，
     提示同步本表。评审 P2 裁定：不读 fixture kind（那会使对照变成同义反复）。 */
  const SPECIAL_KINDS = { rule: "rule", zhipu: "zhipu", anthropic: "anthropic" };
  const norm = (id, r) => ({
    id,
    label: r.label,
    kind: SPECIAL_KINDS[id] ?? "openai-compat",
    model: r.model,
    base: r.base,
    local: !!r.local,
    caps: {
      contextWindow: r.caps && r.caps.contextWindow != null ? r.caps.contextWindow : null,
      structuredOutput: !!(r.caps && r.caps.structuredOutput),
      tools: !!(r.caps && r.caps.tools),
      imageInput: !!(r.caps && r.caps.imageInput),
      promptCache: !!(r.caps && r.caps.promptCache),
    },
    price: r.price
      ? { currency: r.price.currency, promptMicroPerM: r.price.promptMicroPerM,
          completionMicroPerM: r.price.completionMicroPerM, cachedMicroPerM: r.price.cachedMicroPerM }
      : null,
  });
  for (const want of rows) {
    assert.ok(PROVIDER_PRESETS[want.id], `kernel missing preset: ${want.id}`);
    assert.deepEqual(norm(want.id, PROVIDER_PRESETS[want.id]), want, `preset row mismatch: ${want.id}`);
  }
});

/* ---- parseIntent：与 Rust L9 parse 同词汇表（镜像断言） ---- */
test("parse 镜像：spawn/clear/gravity/teleport/count/speed", () => {
  assert.deepEqual(parseIntent("spawn 40 particles"), { kind: "spawn", n: 40 });
  assert.deepEqual(parseIntent("放 20 个"), { kind: "spawn", n: 20 });
  assert.deepEqual(parseIntent("生成3个"), { kind: "spawn", n: 3 });
  assert.deepEqual(parseIntent("clear"), { kind: "clear" });
  assert.deepEqual(parseIntent("清空"), { kind: "clear" });
  assert.deepEqual(parseIntent("set gravity to 1.6"), { kind: "gravity", g: 1.6 });
  assert.deepEqual(parseIntent("重力 9.8"), { kind: "gravity", g: 9.8 });
  assert.deepEqual(parseIntent("teleport 100 -25"), { kind: "teleport", dx: 100, dy: -25 });
  assert.deepEqual(parseIntent("how many"), { kind: "count" });
  assert.deepEqual(parseIntent("有多少个"), { kind: "count" });
  assert.deepEqual(parseIntent("speed 2x"), { kind: "speed", k: 2 });
  assert.deepEqual(parseIntent("倍速 0.5"), { kind: "speed", k: 0.5 });
});

test("否定语义先于意图 → unknown（别清空不清空）", () => {
  assert.equal(parseIntent("别清空").kind, "unknown");
  assert.equal(parseIntent("不要生成新的").kind, "unknown");
  assert.equal(parseIntent("do not spawn").kind, "unknown");
  // spawn 无数字 → 落空为 unknown（与 Rust 一致：意图词在但参数缺失不猜测）
  assert.equal(parseIntent("spawn it, 别 cancel").kind, "unknown");
});

test("无法识别 → unknown 且携带原文", () => {
  const c = parseIntent("给我讲个故事");
  assert.equal(c.kind, "unknown");
  assert.equal(c.text, "给我讲个故事");
});

/* ---- CommandQueue：预算/合并/拒绝计数 ---- */
test("队列：相邻 spawn 合并、count 重复合一、unknown 计拒绝", () => {
  const seen = [];
  const q = new CommandQueue((cmd) => seen.push(cmd), { frameBudget: 8 });
  q.submit("spawn 40");
  q.submit("spawn 20");   // 合并 → 60
  q.submit("多少");        // count
  q.submit("有多少个");    // 与上一条合并
  q.submit("别清空");      // unknown → rejected
  q.submit("clear");
  assert.equal(q.coalesced, 2);
  q.tick({});
  assert.equal(q.processed, 3); // spawn(60) + count + clear
  assert.equal(q.rejected, 1);  // unknown 在执行期计数
  assert.deepEqual(seen, [{ kind: "spawn", n: 60 }, { kind: "count" }, { kind: "clear" }]);
});

test("帧预算：一次 tick 只消化预算条数，不饿死", () => {
  const q = new CommandQueue(() => {}, { frameBudget: 2 });
  q.submit("clear"); q.submit("多少"); q.submit("clear"); q.submit("多少");
  q.tick({});
  assert.equal(q.pending, 2);
  q.tick({});
  assert.equal(q.pending, 0);
});


/* ---- #104 批③：GoalLog（goal.rs 对镜：完成一次记账/受挫一次+记忆/坏表达式不崩） ---- */
test("GoalLog：目标完成一次性记账，不写挫折记忆", () => {
  const mem = new MemoryStore("test.goals1");
  const g = new GoalLog("test.goals1");
  g.add({ id: "serve", description: "上咖啡", success_when: "cups >= 3" });
  g.evaluate({ cups: 1 }, 2, mem);
  assert.ok(!g.isCompleted("serve"));
  g.evaluate({ cups: 3 }, 5, mem);
  assert.ok(g.isCompleted("serve"));
  assert.equal(g.completed.length, 1);
  g.evaluate({ cups: 3 }, 6, mem);
  assert.equal(g.completed.length, 1, "不重复记账");
  assert.equal(mem.lenTotal(), 0, "完成不写挫折记忆");
});

test("GoalLog：超时受挫一次性记账并写 world 记忆（可检索）", () => {
  const mem = new MemoryStore("test.goals2");
  const g = new GoalLog("test.goals2");
  g.add({ id: "brew", description: "冲一壶好咖啡", success_when: "beans >= 9", deadline_turn: 3 });
  g.evaluate({ beans: 1 }, 4, mem);
  assert.equal(g.frustrated.length, 1);
  assert.equal(mem.lenTotal(), 1, "挫折写进 world 记忆");
  const hit = mem.retrieve("world", "world", "目标 受挫", 100, 1);
  assert.ok(hit.length > 0 && hit[0].text.includes("受挫"));
  g.evaluate({ beans: 1 }, 5, mem);
  assert.equal(g.frustrated.length, 1, "不重复记账");
});

test("GoalLog：坏表达式按未满足不崩，超时照走受挫", () => {
  const mem = new MemoryStore("test.goals3");
  const g = new GoalLog("test.goals3");
  g.add({ id: "broken", description: "坏表达式", success_when: "cups ~~ 三", deadline_turn: 1 });
  g.evaluate({ cups: 3 }, 2, mem);
  assert.ok(!g.isCompleted("broken"));
  assert.equal(g.frustrated.length, 1);
  assert.equal(mem.lenTotal(), 1);
});

/* ---- MemoryStore：容量参数化 + 无 localStorage 可用 ---- */
test("记忆容量可注入且默认 60", () => {
  const m60 = new MemoryStore("test.mem60");
  for (let i = 0; i < 70; i++) m60.record("a", { turn: i, day: 1, about: "w", text: "t" + i, imp: 1 });
  assert.equal(m60.lenTotal(), 60);
  const m5 = new MemoryStore("test.mem5", 5);
  for (let i = 0; i < 9; i++) m5.record("a", { turn: i, day: 1, about: "w", text: "t" + i, imp: 1 });
  assert.equal(m5.lenTotal(), 5);
  assert.equal(m5.retrieve("a", "w", "t8", 100, 1)[0].text, "t8"); // 最新保留
});

/* ---- HttpChatProvider：seed/thinking 透传（stub fetch 抓请求体） ---- */
test("provider 透传 seed 与 thinking（仅 openai 兼容方言）", async () => {
  let captured = null;
  const origFetch = globalThis.fetch;
  globalThis.fetch = async (_u, o) => {
    captured = JSON.parse(o.body);
    return { ok: true, json: async () => ({ choices: [{ message: { content: "ok" } }], usage: { total_tokens: 5 } }) };
  };
  try {
    const p = new HttpChatProvider({ kind: "deepseek", key: "test", model: "deepseek-flash", base: "https://x" });
    await p.complete([{ role: "user", content: "hi" }], { seed: 42, thinking: { type: "disabled" } });
    assert.equal(captured.seed, 42);
    assert.deepEqual(captured.thinking, { type: "disabled" });
    assert.equal(p.stats.errors, 0);
  } finally { globalThis.fetch = origFetch; }
});

/* ---- v0.7（PLAN-deepseek-upgrade）：B6 schema 透传 / B5 缓存计量 ---- */
test("provider 透传 schema 为 json_schema，且优先于 json_object（B6）", async () => {
  const bodies = [];
  const origFetch = globalThis.fetch;
  globalThis.fetch = async (_u, o) => {
    bodies.push(JSON.parse(o.body));
    return { ok: true, json: async () => ({ choices: [{ message: { content: "{}" } }], usage: {} }) };
  };
  try {
    const p = new HttpChatProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x" });
    const schema = { type: "object", properties: { say: { type: "string" } }, required: ["say"] };
    await p.complete([{ role: "user", content: "hi" }], { json: true, schema });
    assert.equal(bodies[0].response_format.type, "json_schema");
    assert.equal(bodies[0].response_format.json_schema.name, "reply");
    assert.deepEqual(bodies[0].response_format.json_schema.schema, schema);
    // 单独 json 仍是旧对象格式
    await p.complete([{ role: "user", content: "hi" }], { json: true });
    assert.deepEqual(bodies[1].response_format, { type: "json_object" });
    // 都不传：线上干净（与 Rust default 分支对镜）
    await p.complete([{ role: "user", content: "hi" }], {});
    assert.equal(bodies[2].response_format, undefined);
  } finally { globalThis.fetch = origFetch; }
});

test("provider 缓存命中 tokens 进 stats.cached（DeepSeek/OpenAI + 谎报钳制，B5）", async () => {
  const replies = [
    { choices: [{ message: { content: "a" } }], usage: { total_tokens: 108, prompt_tokens: 100, completion_tokens: 8, prompt_cache_hit_tokens: 92 } },
    { choices: [{ message: { content: "b" } }], usage: { total_tokens: 54, prompt_tokens: 50, completion_tokens: 4, prompt_tokens_details: { cached_tokens: 30 } } },
    // 谎报厂商：cached 999 > prompt 10 → 按 prompt 钳制（与 Rust 对镜）
    { choices: [{ message: { content: "c" } }], usage: { total_tokens: 11, prompt_tokens: 10, completion_tokens: 1, prompt_cache_hit_tokens: 999 } },
  ];
  let i = 0;
  const origFetch = globalThis.fetch;
  globalThis.fetch = async () => ({ ok: true, json: async () => replies[i++] });
  try {
    const p = new HttpChatProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x" });
    await p.complete([{ role: "user", content: "x" }]);
    assert.equal(p.stats.cached, 92);
    await p.complete([{ role: "user", content: "x" }]);
    assert.equal(p.stats.cached, 122, "两次调用累加（92 + 30）");
    await p.complete([{ role: "user", content: "x" }]);
    assert.equal(p.stats.cached, 132, "谎报被钳到 prompt（122 + 10）");
  } finally { globalThis.fetch = origFetch; }
});

/* ---- #105：SSE 流式（completeStream：增量回调 + usage 记账 + 流式请求 wire） ---- */
function sseBody(text) {
  const enc = new TextEncoder();
  return { ok: true, body: new ReadableStream({
    start(c) { c.enqueue(enc.encode(text)); c.close(); },
  }) };
}
test("completeStream：增量按到达序回调，usage 记账与 wire 同 complete 对镜", async () => {
  const sse = 'data: {"choices":[{"delta":{"content":"雨"}}]}\n\n' +
    'data: {"choices":[{"delta":{"content":"停了"}}]}\n\n' +
    'data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":9,"completion_tokens":3,"prompt_cache_hit_tokens":7}}\n\n' +
    "data: [DONE]\n\n";
  let captured = null;
  const seen = [];
  const origFetch = globalThis.fetch;
  globalThis.fetch = async (_u, o) => {
    captured = JSON.parse(o.body);
    return sseBody(sse);
  };
  try {
    const p = new HttpChatProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x" });
    const chunks = await p.completeStream([{ role: "user", content: "写三个字" }], {
      onChunk: t => seen.push(t),
    });
    assert.deepEqual(chunks, ["雨", "停了"]);
    assert.deepEqual(seen, ["雨", "停了"], "onChunk 逐拍同序");
    assert.equal(captured.stream, true);
    assert.deepEqual(captured.stream_options, { include_usage: true });
    assert.equal(p.stats.tokens, 12, "in+out 记账");
    assert.equal(p.stats.cached, 7);
    assert.equal(p.stats.errors, 0);
  } finally { globalThis.fetch = origFetch; }
});
test("completeStream：Anthropic 事件流（message_start/delta 记账）", async () => {
  const sse = 'event: message_start\ndata: {"type":"message_start","message":{"usage":{"input_tokens":11,"cache_read_input_tokens":4}}}\n\n' +
    'event: content_block_delta\ndata: {"type":"content_block_delta","delta":{"type":"text_delta","text":"灯"}}\n\n' +
    'event: content_block_delta\ndata: {"type":"content_block_delta","delta":{"type":"text_delta","text":"亮着"}}\n\n' +
    'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}\n\n' +
    'event: message_stop\ndata: {"type":"message_stop"}\n\n';
  const origFetch = globalThis.fetch;
  globalThis.fetch = async () => sseBody(sse);
  try {
    const p = new HttpChatProvider({ kind: "anthropic", key: "k", model: "claude", base: "https://x" });
    const chunks = await p.completeStream([{ role: "user", content: "两个字" }]);
    assert.deepEqual(chunks, ["灯", "亮着"]);
    assert.equal(p.stats.tokens, 15, "in(11)+out(4)");
    assert.equal(p.stats.cached, 4);
  } finally { globalThis.fetch = origFetch; }
});
test("completeStream：空流报错且计 breaker 失败（诚实不假流）", async () => {
  const origFetch = globalThis.fetch;
  globalThis.fetch = async () => sseBody('data: {"choices":[{"delta":{}}]}\n\ndata: [DONE]\n\n');
  try {
    const p = new HttpChatProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x" });
    await assert.rejects(() => p.completeStream([{ role: "user", content: "x" }]), /no text deltas/);
    assert.equal(p.stats.errors, 1);
    assert.equal(p.breaker.consecutiveFailures, 1);
  } finally { globalThis.fetch = origFetch; }
});

test("Anthropic 方言：cache_read_input_tokens 进 stats.cached", async () => {
  const origFetch = globalThis.fetch;
  globalThis.fetch = async () => ({
    ok: true,
    json: async () => ({ content: [{ text: "在场" }], usage: { input_tokens: 40, output_tokens: 5, cache_read_input_tokens: 25 } }),
  });
  try {
    const p = new HttpChatProvider({ kind: "anthropic", key: "k", model: "claude", base: "https://x" });
    const out = await p.complete([{ role: "user", content: "x" }]);
    assert.equal(out, "在场");
    assert.equal(p.stats.cached, 25);
  } finally { globalThis.fetch = origFetch; }
});

test("预设表带已核实的 caps/price（deepseek/openai），未核实行保持干净", () => {
  assert.equal(PROVIDER_PRESETS.deepseek.caps.contextWindow, 1000000);
  assert.ok(PROVIDER_PRESETS.deepseek.caps.promptCache);
  assert.ok(PROVIDER_PRESETS.deepseek.price.cachedMicroPerM < PROVIDER_PRESETS.deepseek.price.promptMicroPerM,
    "缓存命中必须比 miss 便宜——这是 B5 的全部意义");
  assert.ok(PROVIDER_PRESETS.openai.caps.structuredOutput);
  // 未核实行：不带 caps 也不带 price（镜像 Rust ModelCaps::UNREGISTERED）
  for (const id of ["rule", "qwen", "stepfun", "moonshot", "groq", "openrouter"]) {
    assert.equal(PROVIDER_PRESETS[id].caps, undefined, id);
    assert.equal(PROVIDER_PRESETS[id].price, undefined, id);
  }
  // 本地行永不带价（电费口径）
  for (const id of ["ollama", "vllm", "llamacpp", "lmstudio"]) {
    assert.equal(PROVIDER_PRESETS[id].price, undefined, id);
  }
});

/* ---- CognitionSystem：场景/台词库/RNG/max 注入 ---- */
test("认知系统：注入场景与 max 后 prompt 走新世界，默认仍是咖啡馆", async () => {
  let capturedSys = null, capturedOpts = null;
  const origFetch = globalThis.fetch;
  globalThis.fetch = async (_u, o) => {
    const body = JSON.parse(o.body);
    capturedSys = body.messages[0].content;
    capturedOpts = body;
    return { ok: true, json: async () => ({ choices: [{ message: { content: '{"say":"收到","action":"idle","mood":"neutral","remember":"","to":"客人"}' } }] }) };
  };
  try {
    const world = { turn: 0, npcs: [{ id: "zhou", name: "老周", role: "老板", description: "手冲派", mood: "neutral" }], dialogue: [], story: { day: 1, beats: [] }, guestQueue: [] };
    const mem = new MemoryStore("test.cog");
    const space = new CognitionSystem(
      new HttpChatProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x" }),
      mem, { scene: "环形太空站", maxTokens: 120 },
    );
    await space.tick(world);
    assert.ok(capturedSys.includes("环形太空站"), "注入场景生效");
    assert.ok(!capturedSys.includes("隅角咖啡馆"), "旧场景不残留");
    assert.equal(capturedOpts.max_tokens, 120);
    assert.ok(capturedSys.includes("隅角咖啡馆") === false);

    const cafe = new CognitionSystem(
      new HttpChatProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x" }),
      new MemoryStore("test.cog2"), {},
    );
    capturedSys = null;
    await cafe.tick({ ...world, turn: 0 });
    assert.ok(capturedSys.includes("隅角咖啡馆"), "默认场景保持 v1 兼容");
    assert.equal(capturedOpts.max_tokens, 260);
  } finally { globalThis.fetch = origFetch; }
});

test("认知系统：注入 rng 后离线台词可复现", () => {
  const world = { turn: 0, npcs: [{ id: "zhou", name: "老周", role: "老板", description: "手冲派", mood: "neutral" }], dialogue: [], story: { day: 1, beats: [] }, guestQueue: [] };
  let seed = 1234;
  const rng = () => (seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648;
  const run = () => {
    seed = 1234;
    const c = new CognitionSystem({ ready: false }, new MemoryStore("test.rng"), { rng });
    return c._rule({ id: "zhou", name: "老周" }, { name: "客人" }, world).say;
  };
  assert.equal(run(), run());
});

test("稳定前缀（B5）：system 跨拍字节不变，易变内容走 user", async () => {
  const calls = [];
  const origFetch = globalThis.fetch;
  globalThis.fetch = async (_u, o) => {
    const b = JSON.parse(o.body);
    calls.push({ sys: b.messages[0].content, user: b.messages[1].content });
    return { ok: true, json: async () => ({ choices: [{ message: { content: '{"say":"好","action":"idle","mood":"neutral","remember":"","to":"客人"}' } }], usage: {} }) };
  };
  try {
    const world = { turn: 0, npcs: [{ id: "zhou", name: "老周", role: "老板", description: "手冲派", mood: "neutral" }], dialogue: [], story: { day: 1, beats: [] }, guestQueue: [] };
    const c = new CognitionSystem(
      new HttpChatProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x" }),
      new MemoryStore("test.stable"), {},
    );
    await c.tick(world);
    world.story.day = 2;                       // 世界在动
    world.dialogue.push({ speaker: "客人", to: "老周", text: "今天豆子如何？" });
    await c.tick(world);
    assert.equal(calls[0].sys, calls[1].sys, "system prompt 必须字节稳定（前缀缓存契约）");
    assert.ok(!calls[1].sys.includes("世界进度"), "易变内容不得进 system");
    assert.ok(calls[1].user.includes("第 2 世界日"), "易变内容在 user");
    assert.ok(calls[1].user.includes("今天豆子如何"), "对话在 user");
  } finally { globalThis.fetch = origFetch; }
});

test("导演系统：注入 ruleBeats 生效", async () => {
  const d = new DirectorSystem({ ready: false }, new MemoryStore("test.dir"), { ruleBeats: ["甲事件", "乙事件"] });
  const world = { turn: 0, dialogue: [], story: { day: 1, beats: [] } };
  world.turn = 8; // interval 8 → tick 生效
  const beat = await d.tick(world);
  assert.equal(beat, "甲事件");
});

/* ---- 预设表：行数与 Rust PROVIDER_PRESETS 对齐（行数单一来源=presets.json，
      评审 P2：硬编码行数会让加行的演进成本多一处） ---- */
test("预设表行数与构成（rule 领头，xlite 第二，云端 + 本地开源）", () => {
  const ids = Object.keys(PROVIDER_PRESETS);
  const { rows } = JSON.parse(readFileSync(PRESET_FIXTURE, "utf8"));
  assert.equal(ids.length, rows.length);
  assert.equal(ids[0], "rule");
  assert.equal(ids[1], "xlite");
  for (const id of ["qwen", "stepfun", "ollama", "vllm", "llamacpp", "lmstudio"]) assert.ok(ids.includes(id), id);
  assert.equal(PROVIDER_PRESETS.ollama.base, "http://127.0.0.1:11434/v1");
  assert.equal(PROVIDER_PRESETS.qwen.base, "https://dashscope.aliyuncs.com/compatible-mode/v1");
  assert.equal(PROVIDER_PRESETS.stepfun.base, "https://api.stepfun.com/v1");
  assert.equal(PROVIDER_PRESETS.xlite.base, "https://xlite.dezai.com/v1");
});

test("createProvider 兼容：rule/无 key 返回 null 走降级", () => {
  assert.equal(createProvider({ kind: "rule" }), null);
  assert.equal(createProvider({ kind: "deepseek" }), null);
  assert.ok(createProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x" }));
});

/* ---- v2.1 极客评测快修回归 ---- */
test("parseIntent 全角数字归一（中文输入法）", () => {
  assert.deepEqual(parseIntent("放５个"), { kind: "spawn", n: 5 });
  assert.deepEqual(parseIntent("重力２.５"), { kind: "gravity", g: 2.5 });
  assert.deepEqual(parseIntent("speed ２"), { kind: "speed", k: 2 });
});

test("createProvider 未知 kind 拒绝（不再静默按 openai-compat）", () => {
  assert.equal(createProvider({ kind: "deepseak", key: "k", model: "m", base: "https://x" }), null);
  for (const kind of ["deepseek", "zhipu", "ollama", "qwen", "openai-compat", "xlite"]) {
    const p = createProvider({ kind, key: "k", model: "m", base: "https://x" });
    assert.ok(p, kind);
  }
});

/* ---- v3 HttpChatProvider 参数自愈（本地 mock 服务器，node:http） ---- */
import http from "node:http";

/** 起 n 步 mock：每步 {status, body} 顺序应答，记录收到的请求体；返回 {server, base, reqs} */
function mockGateway(steps) {
  const reqs = [];
  const server = http.createServer((req, res) => {
    let buf = "";
    req.on("data", c => buf += c);
    req.on("end", () => {
      reqs.push(buf);
      const step = steps[Math.min(reqs.length - 1, steps.length - 1)];
      res.writeHead(step.status, { "Content-Type": "application/json" });
      res.end(step.body);
    });
  });
  return new Promise(resolve => {
    server.listen(0, "127.0.0.1", () => {
      resolve({ server, base: "http://127.0.0.1:" + server.address().port, reqs });
    });
  });
}

test("自愈①：400 点名 temperature → 归一为 1 重试成功", async () => {
  const m = await mockGateway([
    { status: 400, body: '{"error":{"message":"claude-sonnet-5-5 does not support non-default temperature"}}' },
    { status: 200, body: '{"choices":[{"message":{"content":"好了"}}],"usage":{"total_tokens":9}}' },
  ]);
  const p = createProvider({ kind: "xlite", key: "k", model: "m", base: m.base });
  const out = await p.complete([{ role: "user", content: "hi" }], { max: 40 });
  assert.equal(out, "好了");
  assert.equal(p.stats.retries, 1);
  assert.match(m.reqs[1], /"temperature":1(\.0)?/); // 第二次请求温度为 1
  assert.match(m.reqs[0], /"temperature":0\.85/);   // 第一次是默认 0.85
  m.server.close();
});

test("自愈②：length 截断空内容 → 加倍 max_tokens 重试成功", async () => {
  const m = await mockGateway([
    { status: 200, body: '{"choices":[{"message":{"content":""},"finish_reason":"length"}],"usage":{"total_tokens":50}}' },
    { status: 200, body: '{"choices":[{"message":{"content":"这次有内容了"}}],"usage":{"total_tokens":120}}' },
  ]);
  const p = createProvider({ kind: "openai-compat", key: "k", model: "m", base: m.base });
  const out = await p.complete([{ role: "user", content: "hi" }], { max: 60 });
  assert.equal(out, "这次有内容了");
  assert.equal(p.stats.retries, 1);
  assert.match(m.reqs[1], /"max_tokens":120/); // 60 → 120
  m.server.close();
});

test("自愈不误伤：普通 400 直接抛错（不再重试）", async () => {
  const m = await mockGateway([
    { status: 400, body: '{"error":{"message":"insufficient quota"}}' },
    { status: 400, body: '{"error":{"message":"insufficient quota"}}' },
  ]);
  const p = createProvider({ kind: "openai-compat", key: "k", model: "m", base: m.base });
  await assert.rejects(() => p.complete([{ role: "user", content: "hi" }], { max: 40 }), /HTTP 400/);
  assert.equal(m.reqs.length, 2); // 初始 + 传输层重试 1 次（900ms 后），无参数自愈轮
  m.server.close();
});

test("超时可配：opts.timeout 短预算触发超时错误", async () => {
  const slow = http.createServer((req, res) => setTimeout(() => {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end('{"choices":[{"message":{"content":"迟到"}}]}');
  }, 500));
  await new Promise(r => slow.listen(0, "127.0.0.1", r));
  const p = createProvider({ kind: "openai-compat", key: "k", model: "m", base: "http://127.0.0.1:" + slow.address().port });
  await assert.rejects(() => p.complete([{ role: "user", content: "hi" }], { timeout: 80, max: 40 }));
  slow.close();
});

test("MemoryStore 未来 turn 不再 Infinity 霸榜", () => {
  const m = new MemoryStore("test.future");
  m.record("a", { turn: 1000, day: 1, about: "w", text: "来自未来", imp: 1 });
  const hits = m.retrieve("a", "w", "来自未来", 10, 3);
  assert.equal(hits.length, 1); // 分数有界（>0.8 阈值仍可召回）
  assert.ok(Number.isFinite(m.retrieve("a", "w", "来自未来", 10, 3).length));
});

/* ---- v0.8（B7 熔断器 + B4 预算裁剪） ---- */
test("熔断器：连续失败开闸快速失败，半开探测成功闭闸（B7）", async () => {
  let fetches = 0, fail = true;
  const origFetch = globalThis.fetch;
  globalThis.fetch = async () => { fetches++; if (fail) throw new Error("HTTP 503");
    return { ok: true, json: async () => ({ choices: [{ message: { content: "好了" } }], usage: {} }) }; };
  try {
    // 长冷却：开闸后快速失败腿（冷却未过，绝不碰网络）
    const p = new HttpChatProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x", breakerThreshold: 2, breakerCooldownMs: 60000, retryDelayMs: 0 });
    await assert.rejects(p.complete([{ role: "user", content: "x" }]));
    assert.equal(p.breaker.consecutiveFailures, 1);
    await assert.rejects(p.complete([{ role: "user", content: "x" }]));
    assert.equal(p.breaker.state, "open");
    const before = fetches;
    await assert.rejects(p.complete([{ role: "user", content: "x" }]), /breaker open/);
    assert.equal(fetches, before, "开闸后快速失败，绝不碰网络");
    // 零冷却：半开探测成功闭闸腿
    const q = new HttpChatProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x", breakerThreshold: 2, breakerCooldownMs: 0, retryDelayMs: 0 });
    await assert.rejects(q.complete([{ role: "user", content: "x" }]));
    await assert.rejects(q.complete([{ role: "user", content: "x" }]));
    assert.equal(q.breaker.state, "open");
    fail = false; // 冷却 0 → 半开探测成功闭闸
    assert.equal(await q.complete([{ role: "user", content: "x" }]), "好了");
    assert.equal(q.breaker.state, "closed");
  } finally { globalThis.fetch = origFetch; }
});

test("熔断器：4xx 配置类错误不计开闸（与 Rust trips_breaker 对镜）", async () => {
  let fetches = 0;
  const origFetch = globalThis.fetch;
  globalThis.fetch = async () => { fetches++; return { ok: false, status: 401, json: async () => ({}) }; };
  try {
    const p = new HttpChatProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x", breakerThreshold: 2, retryDelayMs: 0 });
    await assert.rejects(p.complete([{ role: "user", content: "x" }]));
    await assert.rejects(p.complete([{ role: "user", content: "x" }]));
    await assert.rejects(p.complete([{ role: "user", content: "x" }]));
    assert.equal(p.breaker.state, "closed", "坏 key 换线没用，不开闸");
  } finally { globalThis.fetch = origFetch; }
});

test("fitBudget：头部必保、最旧对话先删、确定性（B4）", () => {
  let user = "世界进度：第 1 世界日，拍\n对话对象：「客人」。\n你的相关记忆：\n- 记忆甲\n最近对话：\n";
  for (let i = 0; i < 20; i++) user += `老周：第${i}句台词内容比较长一些\n`;
  const out = fitBudgetJS(user, 250);
  assert.ok(out.length <= 251, "预算守住");
  assert.ok(out.startsWith("世界进度"), "头部必保");
  assert.ok(out.includes("第19句"), "最新台词保住");
  assert.ok(!out.includes("第0句"), "最旧台词先删");
  assert.equal(out, fitBudgetJS(user, 250), "确定性");
  assert.equal(fitBudgetJS("世界进度：短\n", 250), "世界进度：短\n", "预算内直通");
});

test("fitBudget：emoji 按码点计（与 Rust chars 对镜，评审 P1-2）", () => {
  const user = "世界进度：第 1 世界日\n你的相关记忆：\n- 🐱记忆\n最近对话：\n老周：🐱🐱🐱🐱🐱🐱🐱🐱🐱🐱\n老周：普通台词\n";
  const out = fitBudget(user, 30);
  assert.ok([...out].length <= 30, "码点预算守住");
  assert.ok(!/[\uD800-\uDFFF]/.test(out.slice(-2))) ; // 末尾不落孤代理
  assert.ok(out.includes("世界进度"), "头部必保");
});

test("熔断器：半开探测失败→重开（与 Rust 对镜腿）", async () => {
  let calls = 0, mode = "fail";
  const origFetch = globalThis.fetch;
  globalThis.fetch = async () => { calls++;
    if (mode === "fail" || mode === "probe-fail") throw new Error("HTTP 503");
    return { ok: true, json: async () => ({ choices: [{ message: { content: "ok" } }], usage: {} }) }; };
  try {
    const p = new HttpChatProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x", breakerThreshold: 1, breakerCooldownMs: 0, retryDelayMs: 0 });
    await assert.rejects(p.complete([{ role: "user", content: "x" }])); // trip (threshold 1)
    assert.equal(p.breaker.state, "open");
    mode = "probe-fail";
    await assert.rejects(p.complete([{ role: "user", content: "x" }])); // 半开探测失败
    assert.equal(p.breaker.state, "open", "探测失败重开");
    mode = "ok";
    assert.equal(await p.complete([{ role: "user", content: "x" }]), "ok"); // 再半开→成功闭闸
    assert.equal(p.breaker.state, "closed");
  } finally { globalThis.fetch = origFetch; }
});


/* ---- v0.9 槽位路由（resolveSlotProvider · Node 里 mock localStorage） ---- */
function withLocalStorage(routing, fn) {
  const bak = globalThis.localStorage;
  const store = { [SLOT_ROUTING_KEY]: JSON.stringify(routing) };
  globalThis.localStorage = {
    getItem: k => (k in store ? store[k] : null),
    setItem: (k, v) => { store[k] = String(v); },
    removeItem: k => { delete store[k]; },
  };
  try { return fn(); } finally { globalThis.localStorage = bak; }
}

test("槽位路由：同厂商无 Key 继承全局（XLite 一 Key 多模型主路径）", () => {
  const globalCfg = { kind: "xlite", key: "sk-global", model: "gpt-4o-mini", base: "https://xlite.dezai.com/v1" };
  const p = withLocalStorage({ npc_dialog: { kind: "xlite", model: "glm-5.3-flash" } }, () =>
    resolveSlotProvider("npc_dialog", globalCfg));
  assert.ok(p, "应产出槽位 provider");
  assert.equal(p.cfg.model, "glm-5.3-flash");
  assert.equal(p.cfg.key, "sk-global", "同厂商继承全局 Key");
  assert.equal(p.cfg.kind, "xlite");
  assert.equal(p.cfg.base, "https://xlite.dezai.com/v1", "同厂商继承全局 base（含已本地化的代理地址）");
});

test("槽位路由：跨厂商无 Key 诚实回退全局", () => {
  const globalCfg = { kind: "xlite", key: "sk-global", model: "gpt-4o-mini", base: "https://xlite.dezai.com/v1" };
  const p = withLocalStorage({ director: { kind: "zhipu", model: "glm-4-flash" } }, () =>
    resolveSlotProvider("director", globalCfg));
  assert.ok(p);
  assert.equal(p.cfg.kind, "xlite", "无凭据不硬撑，回退全局");
  assert.equal(p.cfg.model, "gpt-4o-mini");
});

test("槽位路由：跨厂商带独立 Key 生效", () => {
  const globalCfg = { kind: "xlite", key: "sk-global", model: "gpt-4o-mini", base: "https://xlite.dezai.com/v1" };
  const p = withLocalStorage({ director: { kind: "zhipu", model: "glm-4-flash", key: "zhipu-key" } }, () =>
    resolveSlotProvider("director", globalCfg));
  assert.equal(p.cfg.kind, "zhipu");
  assert.equal(p.cfg.key, "zhipu-key");
  assert.equal(p.cfg.base, PROVIDER_PRESETS.zhipu.base, "base 回退预设");
});

test("槽位路由：旧字符串格式 + 缺省/坏 kind 全部安全回退", () => {
  const globalCfg = { kind: "xlite", key: "sk-global", model: "gpt-4o-mini", base: "https://xlite.dezai.com/v1" };
  // 旧格式 { slot: "kind" }：同厂商继承全局 Key，按预设默认 model 展开
  const p1 = withLocalStorage({ director: "xlite" }, () => resolveSlotProvider("director", globalCfg));
  assert.equal(p1.cfg.model, "gpt-4o-mini", "旧格式按预设默认模型展开");
  assert.equal(p1.cfg.key, "sk-global", "同厂商继承全局 Key");
  // 旧格式跨厂商：无凭据 → 全局兜底（诚实回退）
  const p1b = withLocalStorage({ director: "deepseek" }, () => resolveSlotProvider("director", globalCfg));
  assert.equal(p1b.cfg.kind, "xlite", "旧格式跨厂商无 Key 回退全局");
  // 缺省槽位 → 全局
  const p2 = withLocalStorage({}, () => resolveSlotProvider("npc_dialog", globalCfg));
  assert.equal(p2.cfg.kind, "xlite");
  // 非白名单 kind → 全局（不硬撑）
  const p3 = withLocalStorage({ npc_dialog: { kind: "deepseak", model: "x" } }, () => resolveSlotProvider("npc_dialog", globalCfg));
  assert.equal(p3.cfg.kind, "xlite");
});
