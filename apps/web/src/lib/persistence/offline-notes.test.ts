import 'fake-indexeddb/auto';
import { describe, expect, it } from 'vitest';
import {
  clearOfflineNotes,
  countOfflineNotes,
  listOfflineNotes,
  type OfflineNote,
  saveOfflineNote,
  sendOfflineNotes,
} from './offline-notes';

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
    expect(second).toEqual([mine.id]);
    expect(up.creates()).toBe(1);
    expect(await countOfflineNotes()).toBe(1);
  });
});
