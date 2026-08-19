// Turning agent text into markup is the one place this panel produces HTML it
// did not build node by node, so every setting that keeps that safe is here
// and commented, rather than spread through the renderer.
import MarkdownIt from 'markdown-it';
import hljs from 'highlight.js/lib/core';

import bash from 'highlight.js/lib/languages/bash';
import css from 'highlight.js/lib/languages/css';
import diff from 'highlight.js/lib/languages/diff';
import go from 'highlight.js/lib/languages/go';
import javascript from 'highlight.js/lib/languages/javascript';
import json from 'highlight.js/lib/languages/json';
import markdown from 'highlight.js/lib/languages/markdown';
import python from 'highlight.js/lib/languages/python';
import rust from 'highlight.js/lib/languages/rust';
import sql from 'highlight.js/lib/languages/sql';
import typescript from 'highlight.js/lib/languages/typescript';
import xml from 'highlight.js/lib/languages/xml';
import yaml from 'highlight.js/lib/languages/yaml';

// Registering the whole library would put every grammar it ships into the
// bundle. These are what a coding agent actually emits here.
const LANGUAGES: Record<string, unknown> = {
  bash,
  css,
  diff,
  go,
  javascript,
  json,
  markdown,
  python,
  rust,
  sql,
  typescript,
  xml,
  yaml,
};

for (const [name, language] of Object.entries(LANGUAGES)) {
  hljs.registerLanguage(name, language as never);
}
hljs.registerAliases(['js', 'jsx'], { languageName: 'javascript' });
hljs.registerAliases(['ts', 'tsx'], { languageName: 'typescript' });
hljs.registerAliases(['sh', 'shell', 'zsh'], { languageName: 'bash' });
hljs.registerAliases(['py'], { languageName: 'python' });
hljs.registerAliases(['rs'], { languageName: 'rust' });
hljs.registerAliases(['yml'], { languageName: 'yaml' });
hljs.registerAliases(['html'], { languageName: 'xml' });
hljs.registerAliases(['md'], { languageName: 'markdown' });

const md = MarkdownIt({
  // The agent's output is text, not markup. Leaving this off means a `<script>`
  // it writes is escaped and shown, which is what the user asked to read.
  html: false,
  // Only what the author marked as a link becomes one. Autolinking would turn
  // a URL quoted inside prose into something clickable that was never meant to
  // be followed.
  linkify: false,
  breaks: true,
  langPrefix: 'language-',
  highlight: highlightBlock,
});

// markdown-it's own rule allows data:image URLs. Nothing in a chat transcript
// needs one, and narrowing to the two schemes that make sense here removes a
// class of link the panel would otherwise have to reason about.
md.validateLink = (url: string) => /^https?:\/\//i.test(url.trim());

/** Colour one fenced block, or leave it to markdown-it to escape.
 *
 * `hljs.highlight` escapes the source it is given, so what comes back is
 * `<span class="hljs-...">` around escaped text and nothing else. Returning ''
 * hands an unknown language back to markdown-it, which escapes it itself: a
 * block tagged with a language nobody registered is drawn plain rather than
 * guessed at. `highlightAuto` is deliberately not used, because a wrong guess
 * looks like a result rather than like a failure.
 */
function highlightBlock(source: string, language: string): string {
  if (!language || !hljs.getLanguage(language)) {
    return '';
  }
  try {
    return hljs.highlight(source, { language, ignoreIllegals: true }).value;
  } catch {
    // A grammar that threw is a grammar that produced nothing trustworthy.
    // Falling through escapes the source instead of shipping half a parse.
    return '';
  }
}

/** Render agent text as markup.
 *
 * The result is assigned with innerHTML, which is only safe because `html` is
 * off, `validateLink` is narrowed, and the highlighter escapes its own input.
 * Changing any one of those changes what this function returns. */
export function renderMarkdown(text: string): string {
  return md.render(text);
}
