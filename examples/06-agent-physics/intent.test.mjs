import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const html = readFileSync(new URL('./index.html', import.meta.url), 'utf8');
const parserSource = html.match(/\/\/ 意图解析\n([\s\S]*?)\/\/ ============================================================\n\/\/  Chat UI/);
assert.ok(parserSource, 'intent parser should be present in the HTML demo');
const parseIntents = runInNewContext(`${parserSource[1]}\nparseIntents`);

const sendSource = html.match(/function send\(\) \{[\s\S]*?\n\}\nsendBtn\.onclick = send;/);
assert.ok(sendSource, 'send function should be present in the HTML demo');
const sendFunction = sendSource[0].replace(/\nsendBtn\.onclick = send;$/, '');

function runCommands(input) {
  const calls = [];
  const messages = [];
  const makeTool = tool => ({ handler: args => {
    calls.push({ tool, args: structuredClone(args) });
    return `${tool} succeeded`;
  } });
  const inputEl = { value: input };
  const context = {
    inputEl,
    parseIntents,
    addMsg: (who, message) => messages.push({ who, message }),
    tools: {
      spawn_boxes: makeTool('spawn_boxes'),
      spawn_balls: makeTool('spawn_balls'),
      set_gravity: makeTool('set_gravity'),
      clear_world: makeTool('clear_world')
    }
  };
  runInNewContext(`${sendFunction}\nsend();`, context);
  return { calls, messages, inputEl };
}

test('multiple box commands execute in order with independent sides', () => {
  const { calls } = runCommands('右边扔2个方块·左边扔1个方块');
  assert.deepEqual(calls, [
    { tool: 'spawn_boxes', args: { count: 2, side: 'right' } },
    { tool: 'spawn_boxes', args: { count: 1, side: 'left' } }
  ]);
});

test('commas keep the direction scoped to each command', () => {
  const { calls } = runCommands('左边放2个方块，右边放3个方块');
  assert.deepEqual(calls.map(c => c.args), [
    { count: 2, side: 'left' },
    { count: 3, side: 'right' }
  ]);
});

test('mixed commands and Chinese quantities both execute', () => {
  const { calls } = runCommands('左边放6个球，右边放一个方块');
  assert.deepEqual(calls, [
    { tool: 'spawn_balls', args: { count: 6 } },
    { tool: 'spawn_boxes', args: { count: 1, side: 'right' } }
  ]);
});

test('reset and gravity changes retain execution order', () => {
  const { calls } = runCommands('清空；重力调到0.3');
  assert.deepEqual(calls, [
    { tool: 'clear_world', args: {} },
    { tool: 'set_gravity', args: { factor: 0.3 } }
  ]);
});

test('unknown fragments are reported without blocking subsequent commands', () => {
  const { calls, messages } = runCommands('扔2个球·飞起来·扔1个球');
  assert.deepEqual(calls.map(x => x.args.count), [2, 1]);
  assert.ok(messages.some(m => m.who === 'agent' && m.message === '没听懂：飞起来'));
});

test('single command remains backward compatible', () => {
  const { calls, inputEl } = runCommands('扔3个球');
  assert.deepEqual(calls, [{ tool: 'spawn_balls', args: { count: 3 } }]);
  assert.equal(inputEl.value, '');
});

test('Chinese numerals up to 99 work in simple commands', () => {
  const { calls } = runCommands('右边放十二个方块、左边放二十一个方块');
  assert.deepEqual(calls.map(c => c.args), [
    { count: 12, side: 'right' },
    { count: 21, side: 'left' }
  ]);
});

test('consecutive delimiters are ignored', () => {
  const { calls } = runCommands('清空·，；然后左边放1个方块');
  assert.deepEqual(calls.map(c => c.tool), ['clear_world', 'spawn_boxes']);
});

test('all supported separators preserve command order', () => {
  for (const separator of ['·', '，', ',', '；', ';', '。', '\n', '、', '然后', '接着']) {
    const { calls } = runCommands(`扔2个球${separator}清空`);
    assert.deepEqual(calls.map(c => c.tool), ['spawn_balls', 'clear_world'], separator);
  }
});

test('malformed or unsupported Chinese quantities do not dispatch or block later commands', () => {
  for (const count of ['二三', '二十十', '一百二十一', '一千', '三零', '壹', '1二', '三点五', '负三', '廿一', '9007199254740992']) {
    for (const object of ['球', '方块']) {
      const command = `扔${count}个${object}`;
      const { calls, messages } = runCommands(`${command}·扔1个球`);
      assert.deepEqual(calls, [{ tool: 'spawn_balls', args: { count: 1 } }], command);
      assert.ok(messages.some(m => m.who === 'agent' && m.message.includes(command)), command);
    }
  }
});

test('Chinese single digits and tens use their complete value', () => {
  for (const [count, expected] of [['一', 1], ['两', 2], ['九', 9], ['十', 10], ['十一', 11], ['二十', 20], ['九十九', 99]]) {
    assert.deepEqual(runCommands(`扔${count}个球`).calls, [
      { tool: 'spawn_balls', args: { count: expected } }
    ]);
  }
});

test('delimiter-only input receives feedback without dispatching tools', () => {
  const { calls, messages } = runCommands('·，；然后');
  assert.deepEqual(calls, []);
  assert.ok(messages.some(m => m.who === 'agent'));
});

test('blank input does not dispatch or create messages', () => {
  const { calls, messages } = runCommands('  ');
  assert.deepEqual(calls, []);
  assert.deepEqual(messages, []);
});
