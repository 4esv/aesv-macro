import { LORO_SNAPSHOT_DB_NAME } from './snapshot-store';
import { LORO_WAL_DB_NAME } from './wal';

// The document copies kept on this device: Loro snapshots and the edits not
// yet acknowledged by sync-service. Both are per device, not per user, so
// signing out clears them (W-2096).
const STORES = [
  { dbName: LORO_SNAPSHOT_DB_NAME, store: 'snapshots' },
  { dbName: LORO_WAL_DB_NAME, store: 'updates' },
] as const;

// NOTE: opening a missing database would create it empty at its version, and
// the real store's upgrade would then never run, so a missing one stays missing.
function openExisting(dbName: string): Promise<IDBDatabase | undefined> {
  if (typeof indexedDB === 'undefined') return Promise.resolve(undefined);
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(dbName);
    let created = false;
    request.onupgradeneeded = () => {
      created = true;
      request.transaction?.abort();
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () =>
      created ? resolve(undefined) : reject(request.error);
  });
}

function done(request: IDBRequest | IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    if ('oncomplete' in request) {
      request.oncomplete = () => resolve();
      request.onabort = () => reject(request.error);
    } else {
      request.onsuccess = () => resolve();
    }
    request.onerror = () => reject(request.error);
  });
}

/** Edits made on this device that sync-service has not acknowledged. */
export async function countUnsentEdits(): Promise<number> {
  const db = await openExisting(LORO_WAL_DB_NAME);
  if (!db) return 0;
  try {
    if (!db.objectStoreNames.contains('updates')) return 0;
    const all = db.transaction('updates').objectStore('updates').getAll();
    await done(all);
    return (all.result as Array<{ delivered: boolean }>).filter(
      (entry) => !entry.delivered
    ).length;
  } finally {
    db.close();
  }
}

/** Clears every document copy on this device. Open editors keep working. */
export async function clearLocalDocumentCopies(): Promise<void> {
  await Promise.all(
    STORES.map(async ({ dbName, store }) => {
      const db = await openExisting(dbName);
      if (!db) return;
      try {
        if (!db.objectStoreNames.contains(store)) return;
        const tx = db.transaction(store, 'readwrite');
        tx.objectStore(store).clear();
        await done(tx);
      } finally {
        db.close();
      }
    })
  );
}
