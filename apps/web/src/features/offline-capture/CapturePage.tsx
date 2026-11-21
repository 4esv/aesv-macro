import {
  listOfflineNotes,
  type OfflineNote,
  saveOfflineNote,
} from '@app/lib/persistence/offline-notes';
import { Telemetry } from '@macro-inc/observability';
import { onDocumentSessionChange } from '@queries/storage/documentLoad/offline-context-runtime';
import { Button } from '@ui';
import {
  createResource,
  createSignal,
  For,
  type JSX,
  onCleanup,
  Show,
} from 'solid-js';
import { offlineNoteUser, syncOfflineNotes } from './sync';

/**
 * Write a note with or without a network (W-2096). It is saved on this device
 * at once and sent as a note the next time the network is up.
 */
export function CapturePage(): JSX.Element {
  const [userId, setUserId] = createSignal(offlineNoteUser());
  const unsubscribe = onDocumentSessionChange(() =>
    setUserId(offlineNoteUser())
  );
  const [waiting, { refetch }] = createResource(
    userId,
    (id): Promise<OfflineNote[]> => listOfflineNotes(id)
  );
  const [title, setTitle] = createSignal('');
  const [text, setText] = createSignal('');
  const [saving, setSaving] = createSignal(false);

  const refresh = () => {
    syncOfflineNotes()
      .catch((error) => Telemetry.error(error, { surface: 'offline-notes' }))
      .finally(() => refetch());
  };
  window.addEventListener('online', refresh);
  onCleanup(() => {
    unsubscribe();
    window.removeEventListener('online', refresh);
  });

  const save = async (event: SubmitEvent) => {
    event.preventDefault();
    const id = userId();
    if (!id || !text().trim() || saving()) return;
    setSaving(true);
    try {
      await saveOfflineNote(id, title().trim(), text());
      setTitle('');
      setText('');
      refresh();
    } finally {
      setSaving(false);
    }
  };

  return (
    <main class="mx-auto flex w-full max-w-xl flex-col gap-3 p-4 text-ink">
      <Show
        when={userId()}
        fallback={<p>Sign in once with a network, then notes save here.</p>}
      >
        <form class="flex flex-col gap-3" onSubmit={save}>
          <input
            class="rounded border border-edge bg-page px-3 py-2"
            aria-label="Title"
            placeholder="Title"
            value={title()}
            onInput={(event) => setTitle(event.currentTarget.value)}
          />
          <textarea
            class="min-h-48 rounded border border-edge bg-page px-3 py-2"
            aria-label="Note"
            placeholder="Write it down"
            value={text()}
            onInput={(event) => setText(event.currentTarget.value)}
          />
          <Button type="submit" disabled={saving() || !text().trim()}>
            Save
          </Button>
        </form>
        <Show when={waiting()?.length}>
          <section aria-label="Waiting to send">
            <p>{waiting()?.length} waiting to send</p>
            <ul>
              <For each={waiting()}>
                {(note) => <li>{note.title || note.text.slice(0, 60)}</li>}
              </For>
            </ul>
          </section>
        </Show>
      </Show>
    </main>
  );
}
