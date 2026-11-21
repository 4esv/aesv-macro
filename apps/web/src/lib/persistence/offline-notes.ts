import { clearLocalDocumentCopies } from '@macro-inc/collaboration/collab/local-copies';
import { RecoverableIDB } from './recoverable-idb';

// Notes written with no network (W-2096). Each is saved here whole, under the
// id it will keep on the server, and is never edited after it is saved, so a
// send that lands late can't drop newer text. It leaves only once the server
// holds that id: the id makes a resend collide instead of making a copy.

/** A note written on this device that the server has not confirmed. */
export type OfflineNote = Readonly<{
  id: string;
  userId: string;
  title: string;
  text: string;
  createdAt: number;
}>;

/** How a note reaches the server; injected so tests need no network. */
export type NoteTransport = Readonly<{
  /** True when the server created the note. */
  create: (note: OfflineNote) => Promise<boolean>;
  /** True when the server already holds a note with this id. */
  exists: (id: string) => Promise<boolean>;
}>;

const STORE = 'notes';
let database: RecoverableIDB | undefined;

function db() {
  database ??= new RecoverableIDB('aesv-offline-notes-v1', 1, (idb) => {
    idb
      .createObjectStore(STORE, { keyPath: 'id' })
      .createIndex('userId', 'userId');
  });
  return database;
}

async function run<T>(
  mode: IDBTransactionMode,
  fn: (store: IDBObjectStore) => IDBRequest<T>
): Promise<T> {
  const tx = await db().transaction(STORE, mode);
  return new Promise<T>((resolve, reject) => {
    const request = fn(tx.objectStore(STORE));
    tx.oncomplete = () => resolve(request.result);
    tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error);
  });
}

/** Saves a new note for this user; it waits here until the server has it. */
export async function saveOfflineNote(
  userId: string,
  title: string,
  text: string
): Promise<OfflineNote> {
  const note: OfflineNote = {
    id: crypto.randomUUID(),
    userId,
    title,
    text,
    createdAt: Date.now(),
  };
  await run('readwrite', (store) => store.add(note));
  return note;
}

/** This user's notes not yet on the server, oldest first. */
export async function listOfflineNotes(userId: string): Promise<OfflineNote[]> {
  const notes = await run<OfflineNote[]>('readonly', (store) =>
    store.index('userId').getAll(userId)
  );
  return notes.sort((a, b) => a.createdAt - b.createdAt);
}

/** Every unsent note on this device, for the sign-out warning. */
export async function countOfflineNotes(): Promise<number> {
  if (typeof indexedDB === 'undefined') return 0;
  return run('readonly', (store) => store.count());
}

/** Removes every unsent note on this device, at sign-out. */
export async function clearOfflineNotes(): Promise<void> {
  setOwner(undefined);
  if (typeof indexedDB === 'undefined') return;
  await run('readwrite', (store) => store.clear());
}

// The user this device's notes and unsent edits belong to. An expired session
// keeps them, since its user sends them after signing back in; a different
// user signing in clears them first.
const OWNER_KEY = 'aesv:offline-copies-owner';

function owner(): string | undefined {
  try {
    return localStorage.getItem(OWNER_KEY) ?? undefined;
  } catch {
    return undefined;
  }
}

function setOwner(userId: string | undefined) {
  try {
    if (userId) localStorage.setItem(OWNER_KEY, userId);
    else localStorage.removeItem(OWNER_KEY);
  } catch {
    // Without storage the notes stay keyed by user, and sends filter by user.
  }
}

/** Marks this device's copies as this user's, clearing another user's first. */
export async function claimOfflineCopies(userId: string): Promise<void> {
  const previous = owner();
  if (previous === userId) return;
  if (typeof indexedDB !== 'undefined') {
    const notes = await run('readonly', (store) => store.getAll());
    for (const note of notes as OfflineNote[])
      if (note.userId !== userId)
        await run('readwrite', (store) => store.delete(note.id));
  }
  // NOTE: with no owner on record (before this shipped) the edits can't be
  // attributed, so they stay; their documents still need this user's access.
  if (previous !== undefined) await clearLocalDocumentCopies();
  setOwner(userId);
}

let sending: Promise<string[]> | undefined;
let queued: Promise<string[]> | undefined;

/**
 * Sends this user's waiting notes, one at a time, and returns the ids the
 * server now holds. A note stays when neither the create nor the check
 * confirms it, and goes again next time. Passes never overlap: a call made
 * during a pass runs one more pass after it, so a note saved meanwhile goes too.
 */
export function sendOfflineNotes(
  userId: string,
  transport: NoteTransport
): Promise<string[]> {
  if (sending) {
    queued ??= sending.then(() => {
      queued = undefined;
      return sendOfflineNotes(userId, transport);
    });
    return queued;
  }
  sending = (async () => {
    const sent: string[] = [];
    for (const note of await listOfflineNotes(userId)) {
      const created = await transport.create(note).catch(() => false);
      if (created || (await transport.exists(note.id).catch(() => false))) {
        await run('readwrite', (store) => store.delete(note.id));
        sent.push(note.id);
      }
    }
    return sent;
  })().finally(() => {
    sending = undefined;
  });
  return sending;
}
