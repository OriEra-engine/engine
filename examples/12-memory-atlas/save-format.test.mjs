import { test } from 'node:test';
import assert from 'node:assert/strict';
import { AGENTS, MAX_SAVE_BYTES, parseSave, serializeSave } from './save-format.mjs';
const episode = () => ({ text: '灯塔日志 <img src=x>', about: '灯塔', turn: 8, day: 8, imp: 5 });
const fixture = () => ({ format: 'oriera-memory-atlas', version: 1, turn: 22,
  memories: Object.fromEntries(AGENTS.map(name => [name, [episode()]])) });
const parse = data => parseSave(JSON.stringify(data));

test('round trip preserves idle days, Unicode, literal HTML and independent agents', () => {
  const data = fixture();
  data.memories['阿澜'] = [];
  assert.deepEqual(parseSave(serializeSave(data.memories, data.turn)), data);
});
test('reject malformed JSON, wrong format/version and invalid world dates', () => {
  for (const text of ['{', 'null', '[]', '"save"']) assert.throws(() => parseSave(text));
  for (const patch of [{format:'other'}, {version:2}, {turn:0}, {turn:1.5}, {turn:1000001}, {turn:'22'}]) {
    assert.throws(() => parse({...fixture(), ...patch}));
  }
});
test('reject missing/unknown agents and non-array or empty memory collections', () => {
  for (const memories of [null, [], {}, {...fixture().memories, stranger:[]},
    {...fixture().memories, '林舟':{}}, Object.fromEntries(AGENTS.map(n=>[n,[]]))]) {
    assert.throws(() => parse({...fixture(), memories}));
  }
});
test('reject any invalid episode without modifying source data', () => {
  for (const patch of [{text:3}, {text:'x'.repeat(181)}, {about:'other'}, {turn:23,day:23},
    {turn:0,day:0}, {day:7}, {imp:0}, {imp:6}, {imp:1.5}, {turn:NaN}]) {
    const data=fixture(); data.memories['林舟'].push({...episode(), ...patch});
    const before=JSON.stringify(data);
    assert.throws(()=>parse(data)); assert.equal(JSON.stringify(data),before);
  }
});
test('accept 60 entries per agent; reject 61 rather than silently truncate', () => {
  const data=fixture(); data.memories['林舟']=Array.from({length:60},episode);
  assert.equal(parse(data).memories['林舟'].length,60);
  data.memories['林舟'].push(episode()); assert.throws(()=>parse(data));
});
test('enforce byte limit, including multibyte input', () => {
  assert.throws(()=>parseSave(' '.repeat(MAX_SAVE_BYTES+1)), /256 KiB/);
  const data=fixture(); data.extra='海'.repeat(MAX_SAVE_BYTES/3);
  assert.throws(()=>parse(data), /256 KiB/);
});
test('project known fields into fresh objects and discard prototype-shaped extras', () => {
  const data=fixture(); data.extra='ignored'; data.memories['林舟'][0].extra='ignored';
  const parsed=parseSave(JSON.stringify(data).replace('"version":1','"version":1,"__proto__":{"polluted":true}'));
  assert.deepEqual(parsed,fixture()); assert.equal({}.polluted,undefined);
  parsed.memories['林舟'][0].text='changed'; assert.notEqual(data.memories['林舟'][0].text,'changed');
});
