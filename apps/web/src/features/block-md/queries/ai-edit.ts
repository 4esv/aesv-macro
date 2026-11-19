import { toast } from '@core/component/Toast/Toast';
import { type AiEditResult, requestAiEdit } from '@service-ai-editing/client';

export {
  cancelAiEdit,
  hasActiveAiEdit,
  requestAiEdit,
} from '@service-ai-editing/client';

/** Present edit failures at the document action boundary; cancellations stay silent. */
export function toastAiEditResult(result: AiEditResult): void {
  if (result.kind === 'failed' || result.kind === 'usage-limit')
    toast.failure('AI edit failed');
  if (result.kind === 'blocked') toast.failure(result.message);
}

export async function requestAiEditWithToast(
  args: Parameters<typeof requestAiEdit>[0],
  onSettled: () => void
): Promise<void> {
  try {
    toastAiEditResult(await requestAiEdit(args));
  } finally {
    onSettled();
  }
}
