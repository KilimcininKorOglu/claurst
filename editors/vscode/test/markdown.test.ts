import assert from 'node:assert/strict';
import { test } from 'node:test';

import { renderMarkdown } from '../src/webview/markdown';

// This is the only place in the panel that produces HTML it did not build node
// by node, so what it refuses matters as much as what it renders.

test('a script tag in the agent\'s answer is shown, not run', () => {
  const html = renderMarkdown('here is a tag: <script>alert(1)</script>');

  assert.ok(!html.includes('<script'), html);
  assert.ok(html.includes('&lt;script&gt;'), html);
});

test('an event handler attribute cannot be smuggled in through raw html', () => {
  // The word survives as text, which is what the reader asked to see. What
  // must not survive is the tag it would have been an attribute of.
  const html = renderMarkdown('<img src=x onerror="alert(1)">');

  assert.ok(!html.includes('<img'), html);
  assert.ok(html.includes('&lt;img'), html);
});

test('a javascript: link never becomes a link', () => {
  // A refused destination leaves the source as plain text rather than an
  // anchor, so the test is that no anchor came out of it.
  const html = renderMarkdown('[click](javascript:alert(1))');

  assert.ok(!html.includes('<a '), html);
  assert.ok(!html.includes('href='), html);
});

test('a data: link never becomes one either', () => {
  // markdown-it's own rule permits data:image URLs. Nothing in a transcript
  // needs one, and the narrowed rule should refuse it.
  const html = renderMarkdown('[x](data:image/png;base64,AAAA)');

  assert.ok(!html.includes('href='), html);
});

test('an ordinary https link survives', () => {
  const html = renderMarkdown('[docs](https://example.com/a)');

  assert.ok(html.includes('href="https://example.com/a"'), html);
});

test('a fenced block is highlighted for a language that is registered', () => {
  const html = renderMarkdown('```rust\nfn main() {}\n```');

  assert.ok(html.includes('class="language-rust"'), html);
  assert.ok(html.includes('hljs-keyword'), html);
});

test('a fenced block in an unregistered language is drawn plain rather than guessed at', () => {
  const html = renderMarkdown('```foobar\nlet x = 1\n```');

  assert.ok(html.includes('let x = 1'), html);
  assert.ok(!html.includes('hljs-'), html);
});

test('highlighting escapes the code it is given', () => {
  // The highlighter writes markup around the source, so source that looks like
  // markup must come back escaped or it closes the block it is inside.
  const html = renderMarkdown('```rust\nlet s = "</code><img onerror=alert(1)>";\n```');

  assert.ok(!html.includes('<img'), html);
  assert.ok(html.includes('&lt;/code&gt;'), html);
});

test('an unfenced block of text is still escaped', () => {
  const html = renderMarkdown('    </pre><script>alert(1)</script>');

  assert.ok(!html.includes('<script'), html);
});

test('inline code keeps its contents literal', () => {
  const html = renderMarkdown('use `<T>` here');

  assert.ok(html.includes('<code>&lt;T&gt;</code>'), html);
});
