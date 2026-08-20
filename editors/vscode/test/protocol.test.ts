import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  ChunkKind,
  SessionEvents,
  capabilitiesOf,
  deliver,
  locationsOf,
  mentionsIn,
  parseCommands,
  parseConfigOptions,
  parsePlan,
  toolCallOf,
} from '../src/protocol';

// These read whatever the agent put on the wire. The cases that matter are the
// ones where a field is missing or the wrong shape: the panel has to draw
// something either way, and drawing a blank is how a dropped field hides.

test('a tool call keeps its diff, its output and its terminal', () => {
  const call = toolCallOf({
    toolCallId: 'call-1',
    title: 'Write src/main.rs',
    status: 'completed',
    kind: 'edit',
    content: [
      { type: 'diff', path: '/src/main.rs', oldText: 'a\n', newText: 'b\n' },
      { type: 'content', content: { type: 'text', text: 'wrote 1 line' } },
      { type: 'terminal', terminalId: 'term-1' },
    ],
  });

  assert.equal(call.toolCallId, 'call-1');
  assert.deepEqual(call.diffs, [{ path: '/src/main.rs', oldText: 'a\n', newText: 'b\n' }]);
  assert.equal(call.output, 'wrote 1 line');
  assert.equal(call.terminalId, 'term-1');
});

test('a tool call with nothing attached does not invent anything', () => {
  const call = toolCallOf({ toolCallId: 'call-1' });

  assert.deepEqual(call.diffs, []);
  assert.deepEqual(call.locations, []);
  assert.equal(call.output, undefined);
  assert.equal(call.terminalId, undefined);
});

test('a permission request is read with the same reader as a report', () => {
  // The agent sends the preview in the shape it sends a result, which is why
  // there is one reader. A second one would drift.
  const call = toolCallOf({
    toolCallId: 'call-2',
    title: 'Approve Write',
    status: 'pending',
    content: [{ type: 'diff', path: '/notes.txt', newText: 'after\n' }],
    locations: [{ path: '/notes.txt' }],
  });

  assert.equal(call.diffs.length, 1);
  assert.equal(call.diffs[0].oldText, undefined, 'a new file has no previous contents');
  assert.deepEqual(call.locations, [{ path: '/notes.txt', line: undefined }]);
});

test('a location without a path is dropped rather than drawn blank', () => {
  const locations = locationsOf([{ path: '/a.rs', line: 12 }, { line: 3 }, { path: '' }, null]);

  assert.deepEqual(locations, [{ path: '/a.rs', line: 12 }]);
});

test('a location line that is not a number is left unset', () => {
  assert.deepEqual(locationsOf([{ path: '/a.rs', line: '12' }]), [
    { path: '/a.rs', line: undefined },
  ]);
});

test('every content block reaches an event of its own', () => {
  const seen: Array<[string, string]> = [];
  const handler: SessionEvents = {
    onTextChunk: (text: string, kind: ChunkKind) => seen.push([kind, text]),
    onImage: (mimeType: string, _data: string, kind: ChunkKind) => seen.push([kind, mimeType]),
  };

  deliver(handler, { type: 'text', text: 'hello' }, 'agent');
  deliver(handler, { type: 'image', mimeType: 'image/png', data: 'AAAA' }, 'user');
  deliver(handler, { type: 'resource_link', uri: 'file:///a.rs', name: 'a.rs' }, 'user');
  deliver(handler, { type: 'resource', resource: { uri: 'file:///b.rs', text: 'x' } }, 'user');

  assert.deepEqual(seen, [
    ['agent', 'hello'],
    ['user', 'image/png'],
    ['user', '@a.rs'],
    ['user', '@file:///b.rs'],
  ]);
});

test('an image with no data is not announced', () => {
  // An empty data URL renders as a broken image, which reads as a bug in the
  // panel rather than as an empty block.
  let announced = false;
  deliver({ onImage: () => (announced = true) }, { type: 'image', data: '' }, 'agent');

  assert.equal(announced, false);
});

test('a block shape nobody knows is passed over in silence', () => {
  let calls = 0;
  const handler: SessionEvents = { onTextChunk: () => calls++, onImage: () => calls++ };
  deliver(handler, { type: 'audio', data: 'AAAA' }, 'user');

  assert.equal(calls, 0);
});

test('a capability the agent did not claim reads as false', () => {
  // There is nothing in the answer that tells "no" apart from "an older
  // agent", and the safe reading is the one that does not send a block the
  // agent would drop.
  assert.deepEqual(capabilitiesOf({}), {
    name: undefined,
    version: undefined,
    image: false,
    embeddedContext: false,
    loadSession: false,
  });
});

test('the agent capabilities and name are read when they are there', () => {
  const caps = capabilitiesOf({
    agentInfo: { name: 'mikmik', version: '0.1.7' },
    agentCapabilities: {
      loadSession: true,
      promptCapabilities: { image: true, embeddedContext: true },
    },
  });

  assert.deepEqual(caps, {
    name: 'mikmik',
    version: '0.1.7',
    image: true,
    embeddedContext: true,
    loadSession: true,
  });
});

test('a capability sent as a truthy non-boolean is not taken as yes', () => {
  assert.equal(capabilitiesOf({ agentCapabilities: { promptCapabilities: { image: 1 } } }).image, false);
});

test('a command carries its argument hint', () => {
  const commands = parseCommands([
    { name: 'rewind', description: 'go back', input: { hint: '[n]' } },
    { name: 'help', description: 'list commands' },
    { description: 'nameless' },
  ]);

  assert.deepEqual(commands, [
    { name: 'rewind', description: 'go back', hint: '[n]' },
    { name: 'help', description: 'list commands', hint: undefined },
  ]);
});

test('a plan entry without a status is pending', () => {
  assert.deepEqual(parsePlan([{ content: 'do the thing' }]), [
    { content: 'do the thing', status: 'pending', priority: undefined },
  ]);
});

test('a config option that is not a select is left out rather than drawn empty', () => {
  const options = parseConfigOptions([
    { type: 'text', id: 'note' },
    {
      type: 'select',
      id: 'model',
      name: 'Model',
      currentValue: 'a',
      options: [{ value: 'a', name: 'A' }, { value: 'b' }],
    },
  ]);

  assert.equal(options.length, 1);
  assert.equal(options[0].id, 'model');
  assert.deepEqual(options[0].values, [
    { value: 'a', name: 'A' },
    { value: 'b', name: 'b' },
  ]);
});

test('a grouped config option is flattened rather than ignored', () => {
  const options = parseConfigOptions([
    {
      type: 'select',
      id: 'model',
      currentValue: 'a',
      options: [{ options: [{ value: 'a' }, { value: 'b' }] }, { value: 'c' }],
    },
  ]);

  assert.deepEqual(
    options[0].values.map((v) => v.value),
    ['a', 'b', 'c'],
  );
});

test('anything that is not a list parses to nothing', () => {
  assert.deepEqual(parseCommands(undefined), []);
  assert.deepEqual(parsePlan('nope'), []);
  assert.deepEqual(parseConfigOptions(null), []);
  assert.deepEqual(locationsOf({}), []);
});

test('a mention runs to the next space', () => {
  assert.deepEqual(mentionsIn('look at @src/main.rs and @Cargo.toml please'), [
    'src/main.rs',
    'Cargo.toml',
  ]);
});

test('an address is not a mention', () => {
  // Only a token at the start of a word counts, or every email in a prompt
  // would be looked up as a file.
  assert.deepEqual(mentionsIn('mail someone@example.com'), []);
});

test('the same file mentioned twice is resolved once', () => {
  assert.deepEqual(mentionsIn('@a.rs and @a.rs'), ['a.rs']);
});
