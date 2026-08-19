// Builds the two bundles this extension ships. They cannot share one config:
// the host runs in Node with `vscode` injected at runtime, and the webview runs
// in a browser sandbox where neither exists.
import * as esbuild from 'esbuild';

import { readdir } from 'node:fs/promises';

const production = process.argv.includes('--production');
const watch = process.argv.includes('--watch');
const tests = process.argv.includes('--tests');

/** @type {import('esbuild').BuildOptions} */
const shared = {
  bundle: true,
  logLevel: 'info',
  minify: production,
  sourcemap: !production,
  target: 'es2022',
};

const targets = [
  {
    ...shared,
    entryPoints: ['src/extension.ts'],
    outfile: 'out/extension.js',
    platform: 'node',
    format: 'cjs',
    // Supplied by VS Code at load time; bundling it would shadow the real one.
    external: ['vscode'],
  },
  {
    ...shared,
    entryPoints: ['src/webview/main.ts'],
    outfile: 'out/webview.js',
    platform: 'browser',
    format: 'iife',
  },
];

// The tests exercise webview modules, which are ESM and TypeScript. Bundling
// them for Node is what lets `node --test` run them without a loader.
if (tests) {
  const entries = await readdir('test', { recursive: true });
  await esbuild.build({
    ...shared,
    minify: false,
    entryPoints: entries.filter((name) => name.endsWith('.test.ts')).map((name) => `test/${name}`),
    outdir: 'out/test',
    platform: 'node',
    format: 'cjs',
  });
} else if (watch) {
  const contexts = await Promise.all(targets.map((target) => esbuild.context(target)));
  await Promise.all(contexts.map((context) => context.watch()));
} else {
  await Promise.all(targets.map((target) => esbuild.build(target)));
}
