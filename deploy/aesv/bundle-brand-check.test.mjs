import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { check } from './bundle-brand-check.mjs';

function bundle(files) {
  const dir = mkdtempSync(path.join(tmpdir(), 'bundle-'));
  for (const [name, text] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    writeFileSync(path.join(dir, name), text);
  }
  return dir;
}

test('a clean bundle passes', () => {
  const dir = bundle({
    'index.html': '<title>aesv</title><link rel="preconnect" href="/">',
    'a.js': 'const MacroLogo=1;h({"X-Macro-Passwords":`1`});let t=`Ask aesv`;',
  });
  assert.deepEqual(check(dir), []);
});

test('the brand in a string a user reads fails', () => {
  const dir = bundle({ 'a.js': 'x({label:`Ask Macro`});y("Sign out of Macro.")' });
  assert.equal(check(dir).length, 2);
});

test('the brand in an HTML page fails', () => {
  const dir = bundle({ 'pipedream-connect.html': '<title>Connect to Macro</title>' });
  assert.equal(check(dir).length, 1);
});

test('posthog fails in any case and any shipped file', () => {
  const dir = bundle({ 'a.js': 'let p="PostHog"', 'b.css': '.posthog{}' });
  assert.equal(check(dir).length, 2);
});

test('index.html may not load a macro.com origin', () => {
  const dir = bundle({ 'index.html': '<link rel="preconnect" href="https://dev-gateway.macro.com">' });
  assert.match(check(dir)[0], /macro\.com/);
});

test('data, schema text and source maps are not read by a user', () => {
  const dir = bundle({
    'a.js': [
      'let t={id:`Macro Dark`,name:`aesv Dark`};',
      'if(x===`Join Macro call`)y();',
      'z.describe(`The type of an entity in Macro`);',
      'let s=`"""\nRow of a Macro database table.\n"""`;',
      'let m=new Macro({});let e="a@Macro.com";let p="/home/me/Macro";',
    ].join('\n'),
    'a.js.map': '{"sourcesContent":["// Macro posthog"]}',
  });
  assert.deepEqual(check(dir), []);
});

test("a syntax grammar's Macro is not the brand", () => {
  const dir = bundle({
    'apache.js': 'var g=JSON.parse(\'{"match":"(<)(Location|VirtualHost|Macro|If)(>)"}\');',
    'cpp.js': 'let p=L.deserialize({nodeNames:`#ifndef PreprocDirectiveName Macro Program`,maxTerm:433});',
  });
  assert.deepEqual(check(dir), []);
});

test('the brand fails inflected or hyphenated', () => {
  const dir = bundle({
    'a.js': [
      'let t={id:`Macro-Gruvbox`,name:`Macro-Gruvbox`};',
      'let sv=`En ny version av Macros skrivbordsapp har laddats ned.`;',
      'let fi=[`Macron uusi versio on valmis.`,`Osaa Macrosta ei voitu ladata.`];',
      'let d=`Codex, or other harness, via Macrod.`;',
    ].join('\n'),
  });
  assert.equal(check(dir).length, 5);
});

test("pdf.js's macron glyph is not the brand, but other Macro in it is", () => {
  const glyphs = 'let g=[`Macron`,63184,`Macronsmall`,63407];let t=`Brevesmall,Macronsmall,figuredash`;';
  assert.deepEqual(check(bundle({ 'pdfjs-worker-a1.js': glyphs })), []);
  const planted = bundle({ 'pdfjs-worker-a1.js': `${glyphs}h({label:\`Ask Macro\`});` });
  assert.equal(check(planted).length, 1);
});
