// Portable demo saves; independent of the browser and the engine's storage key.
export const MAX_SAVE_BYTES = 256 * 1024;
export const AGENTS = ['林舟', '阿澜', '小满'];
const TOPICS = ['灯塔', '花园', '港口'];
const validTurn = n => Number.isSafeInteger(n) && n >= 1 && n <= 1000000;
export const validEpisode = e => e && typeof e.text === 'string' && e.text.length <= 180
  && TOPICS.includes(e.about) && validTurn(e.turn) && e.day === e.turn
  && Number.isInteger(e.imp) && e.imp >= 1 && e.imp <= 5;

export function parseSave(text) {
  if (new TextEncoder().encode(text).length > MAX_SAVE_BYTES) throw new Error('存档不能超过 256 KiB');
  let data;
  try { data = JSON.parse(text); } catch { throw new Error('文件不是有效的 JSON'); }
  if (!data || data.format !== 'oriera-memory-atlas' || data.version !== 1) {
    throw new Error('不支持的存档格式或版本');
  }
  if (!validTurn(data.turn)) throw new Error('世界日期无效');
  if (!data.memories || typeof data.memories !== 'object' || Array.isArray(data.memories)
    || Object.keys(data.memories).length !== AGENTS.length
    || !AGENTS.every(name => Object.hasOwn(data.memories, name))) {
    throw new Error('存档必须包含三个已知角色');
  }
  const memories = {};
  let count = 0;
  for (const name of AGENTS) {
    const episodes = data.memories[name];
    if (!Array.isArray(episodes) || episodes.length > 60
      || !episodes.every(e => validEpisode(e) && e.turn <= data.turn)) {
      throw new Error(`${name}的记忆无效（最多 60 条，日期不能晚于世界日期）`);
    }
    // Rebuild only known fields; never spread untrusted objects into live state.
    memories[name] = episodes.map(({ text, about, turn, day, imp }) => ({ text, about, turn, day, imp }));
    count += episodes.length;
  }
  if (!count) throw new Error('存档至少需要一段记忆');
  return { format: 'oriera-memory-atlas', version: 1, turn: data.turn, memories };
}

export function serializeSave(memories, turn) {
  const data = parseSave(JSON.stringify({ format: 'oriera-memory-atlas', version: 1, turn, memories }));
  return JSON.stringify(data, null, 2);
}
