import {
  claimOfflineCopies,
  type NoteTransport,
  sendOfflineNotes,
} from '@app/lib/persistence/offline-notes';
import { deviceLooksOffline } from '@core/util/connectivity';
import { Telemetry } from '@macro-inc/observability';
import { refetchSoupEntity } from '@queries/soup/cache';
import {
  offlineDocumentContextCache,
  onDocumentSessionChange,
} from '@queries/storage/documentLoad/offline-context-runtime';
import { storageServiceClient } from '@service-storage/client';
import { onCleanup } from 'solid-js';

/** Sends a note as a markdown document under its own id (W-2096). */
export const dssNoteTransport: NoteTransport = {
  async create(note) {
    const created = await storageServiceClient.createMarkdownDocument({
      id: note.id,
      documentName: note.title,
      markdown: note.text,
    });
    return created.isOk();
  },
  // A row alone isn't delivery: a create that failed in finalize leaves one
  // briefly, or for good if its cleanup fails. Only ready content counts.
  async exists(id) {
    storageServiceClient.getDocumentLocation.invalidate({ documentId: id });
    const location = await storageServiceClient.getDocumentLocation({
      documentId: id,
    });
    return location.isOk() && location.value.data.content.state === 'ready';
  },
};

/** The signed-in user, known offline from the last session on this device. */
export function offlineNoteUser(): string | undefined {
  return offlineDocumentContextCache.capture()?.userId;
}

/** Sends this user's waiting notes and shows each in the notes list. */
export async function syncOfflineNotes(): Promise<string[]> {
  const userId = offlineNoteUser();
  if (!userId) return [];
  await claimOfflineCopies(userId);
  if (deviceLooksOffline()) return [];
  const sent = await sendOfflineNotes(userId, dssNoteTransport);
  for (const id of sent) {
    refetchSoupEntity(id, 'document', {
      ownTouch: true,
      refreshGraphql: true,
    }).catch((error) => Telemetry.error(error, { surface: 'offline-notes' }));
  }
  return sent;
}

/** Claims this device's copies, then sends waiting notes: at start, on sign-in and whenever the network returns. */
export function watchOfflineNotes(): void {
  const run = () => {
    syncOfflineNotes().catch((error) =>
      Telemetry.error(error, { surface: 'offline-notes' })
    );
  };
  window.addEventListener('online', run);
  const unsubscribe = onDocumentSessionChange(run);
  run();
  onCleanup(() => {
    window.removeEventListener('online', run);
    unsubscribe();
  });
}
