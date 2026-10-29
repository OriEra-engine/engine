/* ============================================================
 * OriEra L8 生成式 AI 内核（浏览器侧镜像）
 * 与 Rust crate 一一对照：
 *   词汇/校验     ↔ crates/oriera-genai/src/lib.rs
 *   CogView 适配器 ↔ crates/oriera-genai/src/cogview.rs（OpenAI images 方言）
 *   Tripo 适配器   ↔ crates/oriera-genai/src/tripo.rs
 *   离线后端      ↔ crates/oriera-genai/src/mock.rs（此处为程序化贴图，更强）
 *   GenAiSystem   ↔ crates/oriera-genai/src/system.rs
 *   L4 热替换     ↔ crates/oriera-genai/src/hot_swap.rs + oriera-asset
 *   美术导演      ↔ L8 消费端引擎系统（prompt 拼装归引擎，不归应用）
 * 原则（ADR 0003 / docs/AI-NATIVE.md）：生成是引擎系统。
 * 应用只选提供商、说主题；不接触 HTTP、Key、prompt 拼装。
 * Key 只存在浏览器 localStorage（与 demo 11 的 ⚙ 约定同构）。
 *
 * 与 Rust 的差异仅一处（有注释对应）：fetch 的 Promise settling
 * 取代 request/poll 轮询节拍——边界词汇（kind/caps/validate/
 * meter/placeholder 降级）严格一一对照。
 * ============================================================ */

/* ---------- L8 · 生成种类与能力位（↔ lib.rs GenerationKind） ---------- */
export const GenerationKind = {
  TEXT_TO_3D: "text_to_3d",
  IMAGE_TO_3D: "image_to_3d",
  TEXT_TO_IMAGE: "text_to_image", // RFC 0001：小 3D 游戏表达的主力种类
  TEXT_TO_VIDEO: "text_to_video",
  IMAGE_TO_VIDEO: "image_to_video",
};
export const AssetFormat = { GLB: "glb", USDZ: "usdz", VIDEO: "video", IMAGE: "image" };

export const CAPS_ALL = { text_to_3d: true, image_to_3d: true, text_to_image: true, text_to_video: true, image_to_video: true };
export const capsSupport = (caps, kind) => !!caps[kind];

/* ---------- L8 · 请求词汇（↔ lib.rs GenerationRequest） ---------- */
export function textToImage(name, prompt, opts = {}) {
  return { kind: GenerationKind.TEXT_TO_IMAGE, prompt, negative_prompt: "", name, max_bytes: opts.maxBytes ?? 64 * 1024 * 1024, seed: opts.seed ?? null };
}
export function textTo3d(name, prompt, opts = {}) {
  return { kind: GenerationKind.TEXT_TO_3D, prompt, negative_prompt: "", name, max_bytes: opts.maxBytes ?? 64 * 1024 * 1024, seed: opts.seed ?? null };
}

/* ---------- L8 · 校验（↔ lib.rs validate_asset） ----------
 * 字节进 L4 之前必须过：大小上限 + 格式魔数（PNG/JPEG/GLB 头自洽）。
 * ------------------------------------------------------------ */
const HARD_MAX_ASSET_BYTES = 1024 * 1024 * 1024;
const effectiveLimit = n => (!n || n > HARD_MAX_ASSET_BYTES ? 64 * 1024 * 1024 : n);
export function validateAsset(asset) {
  const b = asset.payload, max = effectiveLimit(asset.max_bytes);
  if (b.length > max) throw new Error(`${b.length} bytes exceeds the ${max} byte ceiling`);
  const u32 = i => (b[i] | (b[i + 1] << 8) | (b[i + 2] << 16) | (b[i + 3] << 24)) >>> 0; // LE
  let ok;
  switch (asset.format) {
    case AssetFormat.IMAGE:
      ok = (b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47)
        || (b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff);
      break;
    case AssetFormat.GLB: // magic glTF + version 2 + 声明总长 == 实际长度 + JSON 块界内
      ok = b.length >= 20 && b[0] === 0x67 && b[1] === 0x6c && b[2] === 0x54 && b[3] === 0x46
        && u32(4) === 2 && u32(8) === b.length
        && b[16] === 0x4a && b[17] === 0x53 && b[18] === 0x4f && b[19] === 0x4e
        && 20 + u32(12) <= b.length;
      break;
    default: ok = false;
  }
  if (!ok) throw new Error(`payload does not match the ${asset.format} format signature`);
}

/* ---------- L7 同款 Key 约定（⚙ 面板读写，绝不入仓） ---------- */
const LS_KEY = "oriera.genai.v1";
/** 读取生成提供商配置：本 demo 优先，未配置则复用 demo 11 的智谱 Key（同浏览器）。 */
export function loadGenaiConfig() {
  const parse = k => { try { return JSON.parse(localStorage.getItem(k) || "null"); } catch { return null; } };
  return parse(LS_KEY) || parse("aicafe.llm.v1") || { kind: "zhipu", key: "", model: "", base: "" };
}
export function saveGenaiConfig(cfg) { localStorage.setItem(LS_KEY, JSON.stringify(cfg)); }

/* ---------- L8 · CogView 适配器（↔ cogview.rs，OpenAI images 方言） ---------- */
export class CogViewProvider {
  /** cfg: {key, model, base} —— 默认智谱开放平台，兼容端点改 base/model */
  constructor(cfg) { this.cfg = cfg; this.stats = { calls: 0, errors: 0, images: 0 }; }
  get id() { return "cogview-images-v1"; }
  get ready() { return !!this.cfg.key; }
  get capabilities() { return { text_to_3d: false, image_to_3d: false, text_to_image: true, text_to_video: false, image_to_video: false }; }
  get model() { return this.cfg.model || "cogview-3-flash"; }
  async execute(req) { // Rust: request_generation + poll 合并为一次承诺（见文件头注释）
    if (req.kind !== GenerationKind.TEXT_TO_IMAGE)
      throw Object.assign(new Error(`cogview serves text_to_image only; ${req.kind} requested`), { unsupported: true });
    if (!this.ready) throw new Error("cogview selected without a key");
    this.stats.calls++;
    const r = await fetch((this.cfg.base || "https://open.bigmodel.cn/api/paas/v4") + "/images/generations", {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: "Bearer " + this.cfg.key },
      body: JSON.stringify({ model: this.model, prompt: req.prompt }),
      signal: AbortSignal.timeout(90000), // 图像生成服务端阻塞 5–30s
    });
    if (!r.ok) { this.stats.errors++; throw new Error("cogview HTTP " + r.status); }
    const data = await r.json();
    const entry = data.data && data.data[0];
    let bytes;
    if (entry && entry.b64_json) {
      const bin = atob(entry.b64_json); bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    } else if (entry && entry.url) {
      const img = await fetch(entry.url); // 签名 CDN：不带 Authorization
      if (!img.ok) { this.stats.errors++; throw new Error("cogview download HTTP " + img.status); }
      bytes = new Uint8Array(await img.arrayBuffer());
    } else { this.stats.errors++; throw new Error("cogview reply has neither url nor b64_json"); }
    this.stats.images++;
    return { format: AssetFormat.IMAGE, payload: bytes, provider_id: this.id, model: this.model };
  }
}

/* ---------- L8 · Tripo 适配器（↔ tripo.rs：create → poll → 下载） ---------- */
export class TripoProvider {
  /** cfg: {key, model, base} */
  constructor(cfg) { this.cfg = cfg; this.stats = { calls: 0, errors: 0, meshes: 0 }; }
  get id() { return "tripo-v2"; }
  get ready() { return !!this.cfg.key; }
  get capabilities() { return { text_to_3d: true, image_to_3d: false, text_to_image: false, text_to_video: false, image_to_video: false }; }
  async execute(req) {
    if (req.kind !== GenerationKind.TEXT_TO_3D)
      throw Object.assign(new Error(`tripo serves text_to_3d only; ${req.kind} requested`), { unsupported: true });
    if (!this.ready) throw new Error("tripo selected without a key");
    this.stats.calls++;
    const base = this.cfg.base || "https://api.tripo3d.ai";
    const hdr = { "Content-Type": "application/json", Authorization: "Bearer " + this.cfg.key };
    const create = await fetch(base + "/v2/openapi/task", {
      method: "POST", headers: hdr,
      body: JSON.stringify({ type: "text_to_model", prompt: req.prompt }),
      signal: AbortSignal.timeout(30000),
    });
    if (!create.ok) { this.stats.errors++; throw new Error("tripo create HTTP " + create.status); }
    const cj = await create.json();
    if (cj.code !== 0 || !cj.data || !cj.data.task_id) { this.stats.errors++; throw new Error("tripo create: " + (cj.message || "no task_id")); }
    // poll —— 引擎侧的轮询节拍在此适配器内部消化（3s 间隔，封顶 5 分钟）
    for (let i = 0; i < 100; i++) {
      await new Promise(z => setTimeout(z, 3000));
      const p = await fetch(`${base}/v2/openapi/task/${cj.data.task_id}`, { headers: hdr, signal: AbortSignal.timeout(20000) });
      if (!p.ok) { this.stats.errors++; throw new Error("tripo poll HTTP " + p.status); }
      const j = await p.json();
      if (j.code !== 0) { this.stats.errors++; throw new Error("tripo poll: " + (j.message || j.code)); }
      const st = j.data && j.data.status;
      if (st === "queued" || st === "running") continue;
      if (st === "success") {
        const url = j.data.output && j.data.output.pbr_model;
        if (!url) { this.stats.errors++; throw new Error("tripo success without pbr_model url"); }
        const g = await fetch(url); // 签名 CDN：不带 Authorization
        if (!g.ok) { this.stats.errors++; throw new Error("tripo download HTTP " + g.status); }
        this.stats.meshes++;
        return { format: AssetFormat.GLB, payload: new Uint8Array(await g.arrayBuffer()), provider_id: this.id, model: this.cfg.model || "tripo-v2.5" };
      }
      this.stats.errors++; throw new Error("vendor status: " + st);
    }
    this.stats.errors++; throw new Error("tripo poll timeout");
  }
}

/* ---------- L8 · 程序化离线后端（↔ mock.rs 的强化版） ----------
 * 同一 prompt → 同一张贴图（确定性 hash 取色），无 Key 也出画面：
 * 这就是 ADR 0003 判据 3 的"降级也是表达"在浏览器侧的形态。
 * ------------------------------------------------------------ */
export class ProceduralProvider {
  constructor() { this.stats = { calls: 0, errors: 0, images: 0 }; }
  get id() { return "procedural-v1"; }
  get ready() { return true; }
  get capabilities() { return { text_to_3d: false, image_to_3d: false, text_to_image: true, text_to_video: false, image_to_video: false }; }
  async execute(req) {
    if (req.kind !== GenerationKind.TEXT_TO_IMAGE)
      throw Object.assign(new Error(`procedural serves text_to_image only; ${req.kind} requested`), { unsupported: true });
    this.stats.calls++;
    const seed = [...(req.prompt + "|" + req.name)].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 7);
    const rnd = (() => { let s = seed || 1; return () => (s = (s * 1664525 + 1013904223) >>> 0) / 4294967296; })();
    const cv = document.createElement("canvas"); cv.width = cv.height = 512;
    const g = cv.getContext("2d");
    const hue = seed % 360, hue2 = (hue + 40 + Math.floor(rnd() * 80)) % 360;
    const slot = req.name.split(".")[0];
    if (slot === "sky") { // 天空：双色纵向渐变 + 星点/云带
      const grad = g.createLinearGradient(0, 0, 0, 512);
      grad.addColorStop(0, `hsl(${hue} 62% 16%)`); grad.addColorStop(.55, `hsl(${hue2} 58% 34%)`); grad.addColorStop(1, `hsl(${hue2} 48% 58%)`);
      g.fillStyle = grad; g.fillRect(0, 0, 512, 512);
      for (let i = 0; i < 180; i++) { g.fillStyle = `rgba(255,255,255,${.2 + rnd() * .7})`; const r = rnd() * 1.6; g.beginPath(); g.arc(rnd() * 512, rnd() * 300, r, 0, 7); g.fill(); }
      for (let i = 0; i < 5; i++) { g.fillStyle = `hsla(${(hue + rnd() * 60) % 360} 60% 70% / .08)`; g.beginPath(); g.ellipse(rnd() * 512, 120 + rnd() * 200, 90 + rnd() * 120, 16 + rnd() * 22, rnd() * 3, 0, 7); g.fill(); }
    } else if (slot === "ground") { // 地面：基色 + 噪点 + 无缝格纹
      g.fillStyle = `hsl(${hue} 30% ${18 + rnd() * 8}%)`; g.fillRect(0, 0, 512, 512);
      for (let i = 0; i < 2600; i++) { const v = 12 + rnd() * 26; g.fillStyle = `hsla(${hue + rnd() * 30 - 15} ${24 + rnd() * 20}% ${v}% / .5)`; g.fillRect(rnd() * 512, rnd() * 512, 1 + rnd() * 2.5, 1 + rnd() * 2.5); }
      g.strokeStyle = `hsla(${hue} 20% 40% / .12)`; g.lineWidth = 1;
      for (let i = 0; i <= 8; i++) { g.beginPath(); g.moveTo(i * 64, 0); g.lineTo(i * 64, 512); g.stroke(); g.beginPath(); g.moveTo(0, i * 64); g.lineTo(512, i * 64); g.stroke(); }
    } else { // 海报：径向双色 + 抽象几何（圆/弧/条带），观感稳定不吓人
      const bg = g.createRadialGradient(256, 200, 30, 256, 256, 380);
      bg.addColorStop(0, `hsl(${hue} 55% 30%)`); bg.addColorStop(1, `hsl(${hue2} 60% 12%)`);
      g.fillStyle = bg; g.fillRect(0, 0, 512, 512);
      for (let i = 0; i < 7; i++) {
        g.strokeStyle = `hsla(${(hue + i * 24) % 360} 70% ${55 + rnd() * 20}% / .8)`; g.lineWidth = 2 + rnd() * 7;
        g.beginPath();
        if (rnd() < .5) g.arc(256, 256, 40 + i * 26 + rnd() * 18, rnd() * 6, rnd() * 6 + 1.5 + rnd() * 3);
        else { const y = 60 + rnd() * 400; g.moveTo(40 + rnd() * 60, y); g.bezierCurveTo(180, y - 120 * rnd(), 340, y + 120 * rnd(), 470 - rnd() * 50, y); }
        g.stroke();
      }
      g.fillStyle = "rgba(255,255,255,.85)"; g.beginPath(); g.arc(256 + rnd() * 60 - 30, 190, 26 + rnd() * 16, 0, 7); g.fill();
    }
    const blob = await new Promise(res => cv.toBlob(res, "image/png"));
    const bytes = new Uint8Array(await blob.arrayBuffer());
    this.stats.images++;
    return { format: AssetFormat.IMAGE, payload: bytes, provider_id: this.id, model: "procedural-canvas" };
  }
}

/* ---------- L8 · 提供商工厂（↔ config.rs create_generation_provider） ---------- */
export const GEN_PROVIDER_PRESETS = {
  procedural: { label: "离线程序化（无需 Key）", kind: "procedural" },
  zhipu:      { label: "智谱 CogView 文生图",   kind: "zhipu",  base: "https://open.bigmodel.cn/api/paas/v4", model: "cogview-3-flash" },
  openai:     { label: "OpenAI images",         kind: "openai", base: "https://api.openai.com/v1",            model: "gpt-image-1" },
  tripo:      { label: "Tripo 文生 3D（道具）",  kind: "tripo",  base: "https://api.tripo3d.ai",              model: "tripo-v2.5" },
};
/** 选型 → 适配器。无 Key 的远端选择自动降级为程序化后端（镜像
 *  Rust「remote kinds with an empty key degrade to offline」）。 */
export function createGenerationProvider(cfg) {
  if (!cfg || !cfg.kind || cfg.kind === "procedural") return new ProceduralProvider();
  if (cfg.kind === "tripo") return cfg.key ? new TripoProvider(cfg) : new ProceduralProvider();
  // zhipu / openai / 任意 OpenAI images 兼容端点
  return cfg.key ? new CogViewProvider(cfg) : new ProceduralProvider();
}

/** L7 聊天选型 → L8 图像提供商的引擎内映射（厂商知识不进应用）：
 *  智谱/OpenAI 的 Key 直接复用为 CogView/images 生成，base/model 引用
 *  GEN_PROVIDER_PRESETS（单一来源）；其余厂商（无 images 方言端点）与
 *  无 Key 情况降级为程序化后端——离线也是表达（ADR 0003 判据 3）。 */
export function imageProviderFromChatConfig(cfg) {
  const preset = cfg && cfg.key && GEN_PROVIDER_PRESETS[cfg.kind];
  return preset && preset.kind !== "tripo" // tripo 是 3D 方言，不能当 images 端点
    ? new CogViewProvider({ key: cfg.key, model: preset.model, base: preset.base })
    : new ProceduralProvider();
}

/* ---------- L4 · 命名资产库（↔ oriera-asset insert_named 热替换） ---------- */
export class AssetStore {
  constructor() { this.named = new Map(); this.version = 0; this.subs = []; }
  insertNamed(name, genAsset) { // 同名原位换芯：句柄不变，负载更新
    this.named.set(name, { ...genAsset, name, v: ++this.version });
    for (const fn of this.subs) fn(name, this.named.get(name));
  }
  getNamed(name) { return this.named.get(name); }
  onInsert(fn) { this.subs.push(fn); }
}

/* ---------- L8 · 引擎系统（↔ system.rs GenAiSystem） ----------
 * 队列 + 在途上限 + 校验 + 计量 + 按格式匹配的占位降级 + 类型化事件。
 * 占位资产：Image 槽落 1×1 PNG、GLB 槽落空场景 GLB（与 Rust 同源语义）。
 * ------------------------------------------------------------------ */
const PLACEHOLDER_PNG = Uint8Array.from(atob(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAEElEQVR4nAEFAPr/AAAAAAAABQABkpOIwAAAAABJRU5ErkJggg=="
), c => c.charCodeAt(0)); // 与 Rust tiny_png 同构：1×1 RGBA，块级 CRC/Adler 经真实解码器验证
export class GenAiSystem {
  constructor(provider, { maxInFlight = 3 } = {}) {
    this.provider = provider; this.maxInFlight = maxInFlight;
    this.queue = []; this.inFlight = 0; this.rejected = 0;
  }
  get name() { return "l8-genai"; }
  submit(req) {
    if (!capsSupport(this.provider.capabilities, req.kind)) {
      this.rejected++; // 能力位不匹配：起步即拒，无网络无计费（同 Rust fail-fast）
      return false;
    }
    this.queue.push(req); return true;
  }
  pending() { return this.queue.length + this.inFlight; }
  /** 由应用壳每帧/每节拍泵一次：提交在途任务并让已落地者入队待换。 */
  tick(world) {
    while (this.inFlight < this.maxInFlight && this.queue.length) {
      const req = this.queue.shift();
      this.inFlight++;
      world.genaiMeter = world.genaiMeter || { submitted: 0, succeeded: 0, failed: 0, degraded: 0, bytes_in: 0 };
      world.genaiMeter.submitted++;
      this.provider.execute(req)
        .then(asset => {
          try { validateAsset(asset); }
          catch (e) { this._fail(world, req, "validation: " + e.message); return; }
          world.genaiMeter.succeeded++; world.genaiMeter.bytes_in += asset.payload.length;
          (world.pendingSwaps = world.pendingSwaps || []).push({ name: req.name, asset, placeholder: false });
          (world.events = world.events || []).push({ type: "genai-landed", name: req.name, provider: this.provider.id, placeholder: false });
        })
        .catch(e => this._fail(world, req, String(e && e.message || e)))
        .finally(() => { this.inFlight--; });
    }
  }
  _fail(world, req, error) {
    world.genaiMeter = world.genaiMeter || { submitted: 0, succeeded: 0, failed: 0, degraded: 0, bytes_in: 0 };
    world.genaiMeter.failed++; world.genaiMeter.degraded++;
    const ph = req.kind === GenerationKind.TEXT_TO_IMAGE
      ? { format: AssetFormat.IMAGE, payload: PLACEHOLDER_PNG, provider_id: "placeholder", model: "" }
      : null; // 3D 槽占位由渲染端保持原模型（浏览器侧与 Rust 空场景 GLB 等价的处置）
    if (ph) (world.pendingSwaps = world.pendingSwaps || []).push({ name: req.name, asset: ph, placeholder: true });
    (world.events = world.events || []).push({ type: "genai-failed", name: req.name, error, degraded: true });
  }
}

/* ---------- L4 · 换芯落地（↔ hot_swap.rs apply_pending） ---------- */
export function applyPending(store, world) {
  const swaps = (world.pendingSwaps || []).splice(0);
  for (const s of swaps) store.insertNamed(s.name, { ...s.asset, placeholder: s.placeholder });
  return swaps.length;
}

/* ---------- L5 · 槽位绑定（渲染消费端，renderer 无关） ---------- */
export class SlotBindings {
  constructor(store) { this.map = new Map(); store.onInsert((name, asset) => { const fn = this.map.get(name); if (fn) fn(asset); }); }
  bind(name, apply) { this.map.set(name, apply); }
}

/* ---------- L8 · 美术导演（引擎系统：prompt 拼装归引擎） ----------
 * 应用只说主题；天空/地面/海报/道具的提示词形状是引擎知识。
 * ------------------------------------------------------------------ */
export const ART_SLOTS = [
  { name: "sky.dome",     label: "天空穹顶", kind: GenerationKind.TEXT_TO_IMAGE },
  { name: "ground.albedo",label: "地面材质", kind: GenerationKind.TEXT_TO_IMAGE },
  { name: "poster.1",     label: "海报 ①",   kind: GenerationKind.TEXT_TO_IMAGE },
  { name: "poster.2",     label: "海报 ②",   kind: GenerationKind.TEXT_TO_IMAGE },
  { name: "poster.3",     label: "海报 ③",   kind: GenerationKind.TEXT_TO_IMAGE },
  { name: "hero.prop",    label: "主视觉道具", kind: GenerationKind.TEXT_TO_3D },
];
export class ArtDirectorSystem {
  constructor(genai, { posters = 3, props = true } = {}) { this.genai = genai; this.posters = posters; this.props = props; }
  get name() { return "l8-art-director"; }
  /** 换主题：对每个槽位提交生成请求（同名再生成 = 热替换）。 */
  retheme(world, theme) {
    const names = [];
    const submit = (name, prompt) => { if (this.genai.submit(textToImage(name, prompt))) names.push(name); };
    submit("sky.dome", `游戏天空穹顶全景贴图：${theme}。无文字、无水印，水平方向无缝，色调统一，适合贴在球体内壁。`);
    submit("ground.albedo", `游戏地面材质贴图（albedo）：${theme}。正俯视、平铺无缝、无文字，细节均匀不过曝。`);
    for (let i = 1; i <= this.posters; i++)
      submit(`poster.${i}`, `世界主题海报插画 #${i}：${theme}。竖构图，色彩氛围浓，画面抽象或有主体物，无任何文字。`);
    if (this.props && capsSupport(this.genai.provider.capabilities, GenerationKind.TEXT_TO_3D)
        && this.genai.submit(textTo3d("hero.prop", `一个最能代表「${theme}」的小型桌面摆件道具，单体、干净背景、桌摆比例。`)))
      names.push("hero.prop");
    (world.events = world.events || []).push({ type: "art-retheme", theme, names });
    return names;
  }
}

/* ---------- L8 · 分镜画系统（剧情驱动的持续生成，引擎系统） ----------
 * 与 ArtDirectorSystem（一句话主题装帧）互补：这个系统**观察世界**——
 * 盯住 L9 导演推进的剧情拍点（world.story.beats，kernel.js DirectorSystem
 * 的世界词汇），每逢第 N 拍为命名槽位提交一张「此刻的故事」插画。
 * 提示词形状（插画该长什么样）是引擎知识；世界只提供事实（beat 文本）。
 * ------------------------------------------------------------------------ */
export class StoryboardArtSystem {
  constructor(genai, { name = "wall.art", every = 2 } = {}) {
    this.genai = genai; this.name_ = name; this.every = Math.max(1, every | 0);
    this.seen = -1; // 已观察到的 beat 下标
  }
  get name() { return "l8-storyboard-art"; }
  tick(world) {
    const beats = world.story && world.story.beats;
    if (!beats || !beats.length) return;
    const idx = beats.length - 1;
    if (this.seen === idx) return; // 没有新拍点
    const first = this.seen < 0;
    this.seen = idx;
    // 首次观察立即生成（世界装载即有画）；此后每逢第 N 拍换新
    if (!first && idx % this.every !== 0) return;
    const beat = String(beats[idx]).slice(0, 60);
    this.genai.submit(textToImage(this.name_,
      `咖啡馆墙面装饰插画，描绘此刻店里发生的故事瞬间：「${beat}」。暖色咖啡馆氛围，有生活气息，横构图，无文字无水印。`));
    (world.events = world.events || []).push({ type: "art-beat", name: this.name_, beat });
  }
}

/* ---------- L2 · 调度器（与 kernel.js 同构；系统注册制） ---------- */
export class Scheduler {
  constructor() { this.systems = []; }
  add(sys) { this.systems.push(sys); return sys; }
  tick(world) { for (const s of this.systems) s.tick && s.tick(world); }
}
