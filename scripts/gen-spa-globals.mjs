#!/usr/bin/env node
// Generates the eslint globals allowlist for the dashboard SPA.
//
// The SPA is classic-script architecture: app.js defines hundreds of top-level
// functions/vars that index.html's inline scripts and onclick= attributes call,
// and index.html's inline scripts define names (amuxTrack, posthog bootstrap)
// that app.js calls back. Hand-listing those globals would rot on the first
// concurrent edit, so this script derives the list from the actual code:
//   - top-level function/var/class declarations in every static/*.js file
//   - top-level declarations in every inline <script> block of index.html
//   - every `window.NAME =` assignment anywhere (explicit global exports)
// Output: crates/amux-dashboard/eslint.globals.generated.json, read by
// eslint.config.mjs. scripts/spa-lint.sh regenerates it on every run, so the
// allowlist can never go stale relative to the code being linted.
//
// IMPORTANT: this allowlist cannot hide a real bug inside a function body —
// no-undef only consults it for names that are not declared in any enclosing
// scope, and a typo'd local (the worker-delete bug class) is never a top-level
// declaration, so it stays an error.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as espree from 'espree';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const staticDir = path.join(root, 'crates/amux-dashboard/static');
const outFile = path.join(root, 'crates/amux-dashboard/eslint.globals.generated.json');

const globalsFound = new Set();

function idsFromPattern(node, out) {
  if (!node) return;
  switch (node.type) {
    case 'Identifier': out.add(node.name); break;
    case 'ObjectPattern': for (const p of node.properties) idsFromPattern(p.value || p.argument, out); break;
    case 'ArrayPattern': for (const el of node.elements) idsFromPattern(el, out); break;
    case 'AssignmentPattern': idsFromPattern(node.left, out); break;
    case 'RestElement': idsFromPattern(node.argument, out); break;
  }
}

function collectTopLevel(code, label) {
  let ast;
  try {
    ast = espree.parse(code, { ecmaVersion: 'latest', sourceType: 'script' });
  } catch (e) {
    // Exit-code honest: a source we cannot parse is a gate failure, not a skip.
    console.error(`gen-spa-globals: parse error in ${label}: ${e.message}`);
    process.exit(2);
  }
  for (const node of ast.body) {
    if (node.type === 'FunctionDeclaration' || node.type === 'ClassDeclaration') {
      if (node.id) globalsFound.add(node.id.name);
    } else if (node.type === 'VariableDeclaration') {
      for (const d of node.declarations) idsFromPattern(d.id, globalsFound);
    }
  }
  // Explicit global exports at any depth: window.NAME = ... (not == / ===)
  for (const m of code.matchAll(/\bwindow\.([A-Za-z_$][A-Za-z0-9_$]*)\s*=(?![=])/g)) {
    globalsFound.add(m[1]);
  }
}

// 1. Every plain .js in static/ (app.js, sw.js, future splits)
for (const f of fs.readdirSync(staticDir).filter(f => f.endsWith('.js')).sort()) {
  collectTopLevel(fs.readFileSync(path.join(staticDir, f), 'utf8'), f);
}

// 2. Inline <script> blocks in index.html (skip src= tags — those are vendors,
//    declared by hand in eslint.config.mjs where each name is documented)
const html = fs.readFileSync(path.join(staticDir, 'index.html'), 'utf8');
let i = 0;
for (const m of html.matchAll(/<script(\s[^>]*)?>([\s\S]*?)<\/script>/gi)) {
  i++;
  if (m[1] && /\bsrc\s*=/i.test(m[1])) continue;
  if (!m[2].trim()) continue;
  collectTopLevel(m[2], `index.html inline script #${i}`);
}

const out = {};
for (const name of [...globalsFound].sort()) out[name] = 'writable';
fs.writeFileSync(outFile, JSON.stringify(out, null, 1) + '\n');
console.log(`gen-spa-globals: ${Object.keys(out).length} globals -> ${path.relative(root, outFile)}`);
