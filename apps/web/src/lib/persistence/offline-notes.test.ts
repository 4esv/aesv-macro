import 'fake-indexeddb/auto';
import { clearLocalDocumentCopies } from '@macro-inc/collaboration/collab/local-copies';
import { describe, expect, it, vi } from 'vitest';
import {
  claimOfflineCopies,
  clearOfflineNotes,
  countOfflineNotes,
  listOfflineNotes,
  type OfflineNote,
  saveOfflineNote,
  sendOfflineNotes,
} from './offline-notes';

vi.mock('@macro-inc/collaboration/collab/local-copies', () => ({
  clearLocalDocumentCopies: vi.fn(async () => {}),
}));

/** A server that holds notes by id: a second create of one id fails, as the primary key does. */
function server(options: { down?: boolean; loseResponse?: boolean } = {}) {
  const notes = new Map<string, OfflineNote>();
  let creates = 0;
  return {
    notes,
    creates: () => creates,
    transport: {
      async create(note: OfflineNote) {
        creates += 1;
        if (options.down) throw new TypeError('Failed to fetch');
        if (notes.has(note.id)) return false;
        notes.set(note.id, note);
        return !options.loseResponse;
      },
      async exists(id: string) {
        if (options.down) throw new TypeError('Failed to fetch');
        return notes.has(id);
      },
    },
  };
}

describe('offline notes', () => {
  it('reach the server exactly once, even when the response is lost', async () => {
    await clearOfflineNotes();
    const note = await saveOfflineNote('axel', 'Idea', 'on the train');
    const lossy = server({ loseResponse: true });

    expect(await sendOfflineNotes('axel', lossy.transport)).toEqual([note.id]);
    expect(await sendOfflineNotes('axel', lossy.transport)).toEqual([]);
    expect([...lossy.notes.keys()]).toEqual([note.id]);
    expect(await countOfflineNotes()).toBe(0);
  });

  it('stay on the device while the server cannot be reached', async () => {
    await clearOfflineNotes();
    const note = await saveOfflineNote('axel', 'Idea', 'offline');
    expect(
      await sendOfflineNotes('axel', server({ down: true }).transport)
    ).toEqual([]);
    expect((await listOfflineNotes('axel')).map((n) => n.id)).toEqual([
      note.id,
    ]);
  });

  it('are sent once by overlapping passes, and only for their own user', async () => {
    await clearOfflineNotes();
    const mine = await saveOfflineNote('axel', 'Mine', 'a');
    await saveOfflineNote('someone', 'Theirs', 'b');
    const up = server();

    const [first, second] = await Promise.all([
      sendOfflineNotes('axel', up.transport),
      sendOfflineNotes('axel', up.transport),
    ]);

    expect(first).toEqual([mine.id]);
    // The second call runs after the first pass and finds nothing left.
    expect(second).toEqual([]);
    expect(up.creates()).toBe(1);
    expect(await countOfflineNotes()).toBe(1);
  });

  it('send a note saved while a pass is running, when asked during that pass', async () => {
    await clearOfflineNotes();
    const early = await saveOfflineNote('axel', 'Early', 'a');
    const up = server();
    let release = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const slow = {
      ...up.transport,
      async create(note: OfflineNote) {
        if (note.id === early.id) await gate; // a send on a slow network
        return up.transport.create(note);
      },
    };

    const first = sendOfflineNotes('axel', slow);
    const late = await saveOfflineNote('axel', 'Late', 'b');
    const second = sendOfflineNotes('axel', slow);
    release();

    expect(await first).toEqual([early.id]);
    expect(await second).toEqual([late.id]);
    expect(up.creates()).toBe(2);
    expect(await countOfflineNotes()).toBe(0);
  });

  it('belong to the user who wrote them: another user signing in clears them and the unsent edits', async () => {
    await clearOfflineNotes();
    await claimOfflineCopies('axel');
    await saveOfflineNote('axel', 'Mine', 'kept through an expired session');
    vi.mocked(clearLocalDocumentCopies).mockClear();

    await claimOfflineCopies('axel'); // the same user signs back in
    expect(await countOfflineNotes()).toBe(1);
    expect(clearLocalDocumentCopies).not.toHaveBeenCalled();

    await claimOfflineCopies('someone');
    expect(await countOfflineNotes()).toBe(0);
    expect(clearLocalDocumentCopies).toHaveBeenCalledOnce();
  });
});
