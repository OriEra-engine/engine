/* zcode-workflow
description: OriEra 世界引擎公众号「迭代战报」自动写稿：从 engine 仓库 git 记录与 release notes
  收集素材并核对数据锚点，按潜元无限风格库（oriera-mp-style）写初稿，独立复查编辑逐条溯源+过红线，自动校验（结构/CTA/密钥泄露）并修复，最终交付
  _wechat_work/article-MMDD.md 成稿与素材单。
whenToUse: 每期公众号迭代战报：OriEra 引擎发版或积累了一批 PR 后要写推文时。传入 since=上篇发布日即可端到端出稿；主编最后人工终审一遍再走排版发表。
args:
  focus:
    type: string
    description: 主编本期提示（可选）：重点主题、叙事侧重等，会传给素材收集员与主笔
    default: ""
  since:
    type: string
    description: 上篇推文发布日（YYYY-MM-DD）。工作流只收集此日期之后的引擎迭代作为本期素材
    required: true
*/
// 公众号「迭代战报」自动写稿工作流（潜元无限 / OriEra 世界引擎）
// 流程：收集素材 → 按风格库写初稿 → 独立复查（数据溯源+红线）→ 自动校验修复 → 交付

interface ReleaseNote {
  /** 版本号，如 v0.10.0 */
  version: string;
  /** 发布日期 YYYY-MM-DD（取版本发布提交的日期） */
  date: string;
  /** 本版要点，每条一句话并保留 PR 编号 */
  highlights: string[];
  /** release note 源文件路径 */
  source: string;
}

interface SeriesContext {
  /** 上一篇成稿的绝对路径（正斜杠） */
  lastFile: string;
  /** 本篇篇号（上篇为第 N 篇则本篇为 N+1） */
  nextIndex: number;
  /** 上篇结尾的预告或金句 */
  lastPromise: string;
}

interface DataAnchor {
  /** 一条可写进文章的事实（尽量带数字） */
  claim: string;
  /** 溯源：文件路径或 git 命令 */
  source: string;
}

interface Material {
  releases: ReleaseNote[];
  /** 素材窗口内的功能提交/PR（ref=PR号或commit、subject、date） */
  commits: { ref: string; subject: string; date: string }[];
  series: SeriesContext;
  anchors: DataAnchor[];
  /** 成稿绝对路径（正斜杠），如 D:/OriEra/_wechat_work/article-1027.md */
  articleFile: string;
  /** 查不到或拿不准、需主编知道的空白 */
  gaps: string[];
}

interface DraftInfo {
  /** 选定标题（≤30 字） */
  title: string;
  /** 落选候选，每条格式「标题——弃选理由」 */
  rejected: string[];
}

interface ReviewOutcome {
  /** 逐条修复说明（改了什么、为什么） */
  fixes: string[];
  /** 逐条核对过的数据锚点数 */
  anchorsChecked: number;
  /** 不可溯源、已删改的表述 */
  unsupportedClaims: string[];
  /** 仍需主编人工拍板的事项 */
  openQuestions: string[];
}

interface ArticleCheck {
  ok: boolean;
  issues: string[];
  text: string;
}

interface Finding {
  /** 问题所在文件（含行号可选） */
  where: string;
  /** 一句话：问题是什么 */
  what: string;
  /** 依据 */
  evidence: string;
  /** verified=已独立确认；unconfirmed=待人工确认 */
  status: "verified" | "unconfirmed";
  severity: "low" | "medium" | "high";
}

interface WorkflowReport {
  conclusion: string;
  findings: Finding[];
  verified: string[];
  notCovered: string[];
}

const since = typeof args.since === "string" && args.since ? args.since : "2026-10-25";
const focus = typeof args.focus === "string" ? args.focus : "";
const STYLE = "C:/Users/Lenovo/.agents/skills/oriera-mp-style/SKILL.md";

async function checkArticle(absPath: string): Promise<ArticleCheck> {
  const code = [
    "const fs = require('fs');",
    `const p = ${JSON.stringify(absPath)};`,
    "let text = '';",
    "try { text = fs.readFileSync(p, 'utf8'); }",
    "catch (e) { console.log(JSON.stringify({ ok: false, issues: ['成稿读取失败: ' + String(e)], text: '' })); process.exit(0); }",
    "const issues = [];",
    "if (text.trim().length < 600) issues.push('正文过短（不足 600 字）');",
    "if (!text.includes('加群')) issues.push('结尾缺少「回复加群」CTA');",
    "if (!text.includes('技术拆解')) issues.push('缺少系列篇头（技术拆解 · 第 N 篇）');",
    "if (/sk-[A-Za-z0-9]{8,}/.test(text)) issues.push('疑似 API key 泄露（sk- 开头长串）');",
    "const title = text.split('\\n')[0] || '';",
    "if (!title.startsWith('# ')) issues.push('首行不是一级标题');",
    "else if (title.length > 45) issues.push('标题过长（>45 字符）');",
    "console.log(JSON.stringify({ ok: issues.length === 0, issues, text }));",
  ].join("\n");
  const res = await world.run("node", ["-e", code]);
  const parsed = JSON.parse(res.stdout.trim()) as { ok: boolean; issues: string[]; text: string };
  return { ok: parsed.ok, issues: parsed.issues, text: parsed.text };
}

phase("收集本次迭代素材");
const material = await agent(
  "素材收集员",
  "你是科技媒体资料员：只陈述查证过的事实，查不到就写进 gaps，绝不推测编造。若素材窗口内没有任何版本发布，如实返回空 releases 而不是硬凑。",
).ask<Material>(
  `为 OriEra 世界引擎的公众号「迭代战报」收集素材。你的当前目录就是引擎仓库（D:/OriEra/engine，独立 git 仓库）；公众号工作区在 D:/OriEra/_wechat_work。素材窗口：${since}（上篇发布日）之后至今。步骤：\n` +
    `1. 跑 git log --since="${since} 00:00" --format="%ci|%s"，列出窗口内全部提交；识别版本发布提交（标题含 vX.Y.Z 版本号），记录每个版本的发布日期与时间。\n` +
    `2. 对窗口内每个新版本，读 docs/release-notes/ 下对应文件（按文件名版本号匹配），提炼要点，保留 PR 编号与全部可验证数字。\n` +
    `3. 列 D:/OriEra/_wechat_work 下所有 article-*.md，读最近一篇：开头确认系列篇号「第 N 篇」，结尾摘出预告/金句。\n` +
    `4. 把文章可引用的每个硬数字整理成数据锚点（claim 一句话 + source）；README.md 可核对 demo 数量等公开事实。\n` +
    `5. articleFile 用最新版本发布日期命名：D:/OriEra/_wechat_work/article-MMDD.md。\n` +
    `只返回 Material，不写文章。红线：数字必须有来源；内部人名一律匿名（外部贡献者的公开 GitHub handle 可保留）；查不到的进 gaps。` +
    (focus ? `\n主编本期提示：${focus}` : ""),
);
log(
  `素材就绪：${material.releases.length} 个版本、${material.commits.length} 个提交、${material.anchors.length} 个数据锚点；本篇为第 ${material.series.nextIndex} 篇`,
);

phase("按潜元无限风格写初稿");
const draftInfo = await agent(
  "主笔",
  "你是潜元无限公众号的主笔，风格=量子位的口语钩子 + DeepTech 的技术纵深。铁律：时间客观、数据只写实测过的、查不到的不编；若素材撑不起某个说法，就不写那个说法。",
).ask<DraftInfo>(
  `为潜元无限写本期公众号迭代战报。先做三件事：完整读风格库 ${STYLE}；读上一篇成稿 ${material.series.lastFile} 对齐语气与系列脉络（本篇是「技术拆解 · 第 ${material.series.nextIndex} 篇（迭代战报）」，篇头引用格式照上一篇）；消化素材：\n${JSON.stringify(material)}\n` +
    `然后按风格库流程成稿：先按标题公式库写 5 个候选标题再选 1（≤30 字、有数字、有承诺、不放读者没概念的术语）；开头三选一（前 3 段自查：无套话、有承诺、够具体）；正文骨架「钩子→痛点→方案→数据锚→demo 证据→CTA」；每屏至少一处加粗金句；小标题用金句感写法（忌编号体）；结尾三条路 CTA（看 demo / 自己做一个 / 回复「加群」）+ 一句金句收尾，并自然预告下一篇。\n` +
    `硬性红线（风格库第五节全条适用）：只用素材 anchors 里的事实与数字，一个都不许编；时间写具体日期不写「近期/前几天」；人名匿名；上篇结尾是「${material.series.lastPromise}」，本篇开头要接住这句话。\n` +
    `成稿写入 ${material.articleFile}（整文件覆盖写入），文末附「配图建议」清单（深色数据卡/实拍截图，不进正文）。返回 DraftInfo。`,
);

phase("独立复查：数据溯源与红线");
const review = await agent(
  "复查编辑",
  "你是没参与写作的外部审稿编辑，任务是挑毛病不是夸奖：每条数字都要溯源，每条红线都要过堂。可读任何文件、跑只读 git 命令，但只允许编辑成稿文件本身；若某说法无法溯源又拿不准，列入 openQuestions 而不是替它编一个来源。",
).ask<ReviewOutcome>(
  `独立复查成稿 ${material.articleFile}（标题：「${draftInfo.title}」）。先读风格库 ${STYLE}（重点第五节红线、第七节发表前 checklist），再逐项过堂：\n` +
    `1. 数据溯源：文中每个数字/事实逐一核对。锚点清单：${JSON.stringify(material.anchors)}；不够就自己读 D:/OriEra/engine/docs/release-notes/ 或跑只读 git log 验证。不可溯源的表述：改写成可溯源的或删除，记入 unsupportedClaims。\n` +
    `2. 红线：时间表述具体客观；无内部人名；无任何 API key/密钥样串；无编造数据；「零成本/零云费」类表述必须注明前提（如电费口径、本地离线）。\n` +
    `3. 结构与风格：标题 ≤30 字有数字有承诺；开头前 3 段无套话；每屏有加粗金句；无超过 3 行的段落；术语先比喻后术语且全文术语名只出现一次；结尾三条路 CTA + 金句收尾。\n` +
    `发现的问题直接编辑 ${material.articleFile} 修复（小修小补，不整篇重写；风格性改写要克制，保留主笔语气）。返回 ReviewOutcome。`,
);
log(`复查完成：修复 ${review.fixes.length} 处，核对 ${review.anchorsChecked} 个锚点，存疑 ${review.openQuestions.length} 条`);

phase("定稿校验并交付");
const stamp = await world.run("git", ["log", "-1", "--format=%h %ci %s"]);
let check = await checkArticle(material.articleFile);
for (let round = 1; round <= 2 && !check.ok; round++) {
  log(`自动校验未过（${check.issues.join("；")}），第 ${round} 轮修复`);
  await agent(
    `修复编辑${round}`,
    "你是排版收尾编辑：只修明确列出的校验问题，绝不重写全文、不改内容观点。",
  ).ask(`成稿 ${material.articleFile} 未过自动校验：${check.issues.join("；")}。直接编辑该文件，只修复这些列出的问题。`);
  check = await checkArticle(material.articleFile);
}

const materialMd = [
  `# 本期素材单（${since} 之后）`,
  "",
  "## 版本",
  ...material.releases.map((r) => `- **${r.version}**（${r.date}）：${r.highlights.join("；")}（源：${r.source}）`),
  "",
  "## 数据锚点",
  ...material.anchors.map((a) => `- ${a.claim} —— ${a.source}`),
  "",
  "## 素材空白",
  ...(material.gaps.length > 0 ? material.gaps.map((g) => `- ${g}`) : ["- 无"]),
].join("\n");
await artifact.markdown("material", materialMd, { title: "本期素材与数据锚点单" });

if (check.ok) {
  await artifact.markdown("article", check.text, {
    title: `成稿：${draftInfo.title}`,
    description: `技术拆解 · 第 ${material.series.nextIndex} 篇（迭代战报）。已过独立数据溯源复查与自动校验（结构/CTA/密钥泄露）。`,
    primary: true,
  });
}

const findings: Finding[] = review.openQuestions.map((q) => ({
  where: material.articleFile,
  what: q,
  evidence: "复查编辑标记，待主编人工确认",
  status: "unconfirmed" as const,
  severity: "medium" as const,
}));

const report: WorkflowReport = {
  conclusion: check.ok
    ? `成稿已交付：${material.articleFile}（标题「${draftInfo.title}」）。覆盖 ${material.releases.map((r) => r.version).join("、")}；复查修复 ${review.fixes.length} 处、核对 ${review.anchorsChecked} 个数据锚点、删改不可溯源表述 ${review.unsupportedClaims.length} 处。`
    : `成稿 ${material.articleFile} 经 2 轮修复仍未过自动校验：${check.issues.join("；")}。文件在原处，需主编人工处理。`,
  findings,
  verified: [
    `复查编辑逐条溯源核对了 ${review.anchorsChecked} 个数据锚点（源：engine/docs/release-notes 与 git log）`,
    `自动校验：标题/篇幅/系列篇头/CTA/疑似密钥泄露${check.ok ? "全部通过" : "未通过"}`,
    `素材截止引擎提交 ${stamp.stdout.trim()}`,
  ],
  notCovered: [
    ...material.gaps,
    "配图仅为文末建议清单；制图、公众号排版与定时发表走 oriera-wechat-mp-pipeline，不在本工作流范围",
  ],
};
return report;