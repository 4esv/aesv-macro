import 'fake-indexeddb/auto';
import { describe, expect, it } from 'vitest';
import { clearLocalDocumentCopies, countUnsentEdits } from './local-copies';
import { IDBSnapshotStore, LORO_SNAPSHOT_DB_NAME } from './snapshot-store';
import { BrowserWALStore, LORO_WAL_DB_NAME } from './wal';

describe('local document copies', () => {
  it('leaves missing stores missing, so the real ones still upgrade', async () => {
    expect(await countUnsentEdits()).toBe(0);
    await clearLocalDocumentCopies();
    const wal = new BrowserWALStore<string>(LORO_WAL_DB_NAME, 'doc-a');
    await wal.append('edit');
    expect(await wal.getAll()).toHaveLength(1);
  });

  it('counts unsent edits and clears both stores at sign-out', async () => {
    const snapshots = new IDBSnapshotStore<string>(
      LORO_SNAPSHOT_DB_NAME,
      'doc-b'
    );
    await snapshots.save('snapshot');
    const wal = new BrowserWALStore<string>(LORO_WAL_DB_NAME, 'doc-b');
    await wal.append('sent');
    await wal.append('unsent');
    const [sent] = await wal.getAll();
    await wal.markDelivered([sent!.id]);
    expect(await countUnsentEdits()).toBe(2); // doc-a's edit and doc-b's unsent one

    await clearLocalDocumentCopies();

    expect(await countUnsentEdits()).toBe(0);
    expect(await snapshots.load()).toBeNull();
    expect(await wal.getAll()).toHaveLength(0);
  });
});
