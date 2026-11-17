import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { expect, it } from 'vitest';

const root = fileURLToPath(new URL('..', import.meta.url));

// NOTE: the hashed web-hosted/ naming test went with the tutorial videos, the only imported videos.
it('leaves web-hosted/ out of every native bundle', () => {
  const read = (path: string) => readFileSync(resolve(root, path), 'utf8');
  const justfile = read('justfile');
  for (const recipe of ['build-tauri', 'build-tauri-dev']) {
    const body = justfile.split(`\n${recipe}:`)[1]?.split('\n\n')[0];
    expect(body, recipe).toContain('rm -rf dist/web-hosted');
  }
  expect(justfile).toMatch(/zip -qr "\$TARGET" \. -x [^\n]*'web-hosted\/\*'/);
  expect(read('../../infra/stacks/web-app/index.ts')).toContain(
    'rm -rf ${shellQuote(`${appArchiveOutputPath}/web-hosted`)}'
  );
  expect(read('../../nix/tauri-desktop.nix')).toContain(
    'rm -rf "$out/web-hosted"'
  );
});
