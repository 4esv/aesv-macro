// Fails when the built frontend bundle still carries PostHog or the Macro
// brand where a user can read it (W-2087).
//
// Usage: node deploy/aesv/bundle-brand-check.mjs <bundle-dir>
//
// - "posthog" in any case fails anywhere in a shipped file.
// - The word "Macro" fails inside a JavaScript string, template or the text of
//   an HTML, JSON, CSS or SVG file, inflected too ("Macros", "Macron",
//   "Macro-Gruvbox"). Identifiers such as MacroLogo, headers such
//   as X-Macro-Passwords, emails, paths and `new Macro(` (the SDK class) are
//   not text a user reads, and neither are the positions below.
// - index.html fails on any macro.com origin, since the page loads it.
//
// Source maps (*.map) are skipped: they carry the source and its comments.
// The pdf.js worker skips the word check: its glyph tables name the macron
// accent "Macron", which no pattern can tell from Finnish "Macron".
import { readdirSync, readFileSync, realpathSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const ts = createRequire(import.meta.url)('typescript');

// A `|` beside the word is a regex alternation in a syntax grammar (Apache's
// <Macro>, Vim's Macro group), not text. A capital or digit after it is an
// identifier (MacroLogo); a lowercase letter or `-` is the brand, inflected.
export const WORD = /(?<![A-Za-z0-9_$@/|-])(?<!new )Macro(?![A-Z0-9_$|])/;
const POSTHOG = /posthog/i;
// A value under these keys is data (a stored id, an enum tag, a Lezer
// grammar's node names), not prose.
const KEYS = new Set(['id', 'key', 'slug', 'tag', 'kind', 'type', 'nodeNames']);
const TEXT_FILE = /\.(html|json|webmanifest|css|svg|txt)$/;
const JS_FILE = /\.m?js$/;
const PDFJS_WORKER = /^pdfjs-worker-.*\.m?js$/;
const TEXT_NODES = new Set([
  ts.SyntaxKind.StringLiteral,
  ts.SyntaxKind.NoSubstitutionTemplateLiteral,
  ts.SyntaxKind.TemplateHead,
  ts.SyntaxKind.TemplateMiddle,
  ts.SyntaxKind.TemplateTail,
]);

function around(text, index) {
  return text.slice(Math.max(0, index - 50), index + 60).replace(/\s+/g, ' ');
}

// A literal nobody reads: data compared or keyed on, a schema description
// (`.describe(...)`, generated from the backend's OpenAPI), or the GraphQL
// schema the cache reads, generated from the backend's doc comments.
function unread(node, sf) {
  const p = node.parent;
  if (node.text.trimStart().startsWith('"""')) return true;
  if (ts.isCaseClause(p) || ts.isElementAccessExpression(p)) return true;
  if (ts.isPropertyAssignment(p)) {
    return p.name === node || KEYS.has(p.name.getText(sf));
  }
  if (ts.isBinaryExpression(p)) return /^[=!]==?$/.test(p.operatorToken.getText(sf));
  return (
    ts.isCallExpression(p) &&
    ts.isPropertyAccessExpression(p.expression) &&
    p.expression.name.text === 'describe'
  );
}

function jsHits(file, src) {
  const sf = ts.createSourceFile(file, src, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
  const hits = [];
  const visit = (node) => {
    if (TEXT_NODES.has(node.kind) && WORD.test(node.text) && !unread(node, sf)) {
      const at = node.getStart(sf);
      hits.push(around(src, at + src.slice(at, node.end).search(WORD)));
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return hits;
}

export function check(dir) {
  const hits = [];
  for (const entry of readdirSync(dir, { recursive: true, withFileTypes: true })) {
    if (!entry.isFile() || entry.name.endsWith('.map')) continue;
    const file = path.join(entry.parentPath, entry.name);
    const rel = path.relative(dir, file);
    if (!JS_FILE.test(rel) && !TEXT_FILE.test(rel)) continue;
    const src = readFileSync(file, 'utf8');
    const say = (what, text) => hits.push(`${rel}: ${what}: ${text}`);
    const posthog = src.search(POSTHOG);
    if (posthog >= 0) say('posthog', around(src, posthog));
    if (PDFJS_WORKER.test(entry.name)) {
      // Glyph names only; see the header.
    } else if (JS_FILE.test(rel)) {
      for (const text of jsHits(file, src)) say('Macro', text);
    } else {
      const macro = src.search(WORD);
      if (macro >= 0) say('Macro', around(src, macro));
    }
    if (rel === 'index.html') {
      const origin = src.search(/[a-z.-]*macro\.com/i);
      if (origin >= 0) say('macro.com', around(src, origin));
    }
  }
  return hits;
}

// NOTE: resolve the path, or a symlinked checkout skips the check and exits 0.
if (process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) {
  const dir = process.argv[2];
  if (!dir) {
    console.error('usage: node deploy/aesv/bundle-brand-check.mjs <bundle-dir>');
    process.exit(2);
  }
  const hits = check(dir);
  for (const hit of hits) console.log(hit);
  if (hits.length > 0) {
    console.error(`${hits.length} branding or tracking hits in ${dir}`);
    process.exit(1);
  }
  console.log(`no PostHog or Macro branding in ${dir}`);
}
