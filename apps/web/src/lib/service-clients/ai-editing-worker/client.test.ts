import { toastAiEditResult } from '@app/features/block-md/queries/ai-edit';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { hasActiveAiEdit, requestAiEdit } from './client';

const { failure, permissionToken, span } = vi.hoisted(() => ({
  failure: vi.fn(),
  permissionToken: vi.fn(),
  span: {
    setAttr: vi.fn(),
    injectTraceHeaders: vi.fn(),
    error: vi.fn(),
    end: vi.fn(),
  },
}));

vi.mock('@core/component/Toast/Toast', () => ({ toast: { failure } }));
vi.mock('@service-storage/client', () => ({
  getDocumentPermissionToken: permissionToken,
}));
vi.mock('@macro-inc/observability', () => ({
  Telemetry: { span: () => span },
}));

const fetch = vi.fn<typeof window.fetch>();

beforeEach(() => {
  vi.clearAllMocks();
  permissionToken.mockResolvedValue('test-document-permission');
  vi.stubGlobal('fetch', fetch);
  vi.spyOn(console, 'error').mockImplementation(() => {});
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

test.each([
  'ai_allowance_exhausted',
  'ai_free_allowance_exhausted',
  'ai_overage_limit_reached',
  'ai_overage_payment_failed',
])(
  'reports a refused edit (%s) as a failure without applying ops',
  async (code) => {
    fetch.mockResolvedValueOnce(
      Response.json({ code, error: 'Usage blocked' }, { status: 402 })
    );
    const onOps = vi.fn();

    const result = await requestAiEdit({
      documentId: 'document',
      prompt: 'Improve this',
      onOps,
    });
    toastAiEditResult(result);

    expect(result).toEqual({
      kind: 'usage-limit',
      error: { code: 'AI_USAGE_LIMIT', reason: code, message: 'Usage blocked' },
    });
    expect(failure).toHaveBeenCalledWith('AI edit failed');
    expect(onOps).not.toHaveBeenCalled();
    expect(hasActiveAiEdit('document')).toBe(false);
    expect(span.end).toHaveBeenCalledOnce();
  }
);

test.each([403, 500, 503])(
  'keeps non-quota HTTP %i edit failures as generic errors',
  async (status) => {
    fetch.mockResolvedValueOnce(
      Response.json({ code: 'ai_billing_unavailable' }, { status })
    );

    const result = await requestAiEdit({
      documentId: 'document',
      prompt: 'Improve this',
    });
    toastAiEditResult(result);

    expect(result).toEqual({ kind: 'failed' });
    expect(failure).toHaveBeenCalledWith('AI edit failed');
    expect(hasActiveAiEdit('document')).toBe(false);
  }
);

test('keeps successful edits and clarification requests distinct from quota failures', async () => {
  const onOps = vi.fn();
  fetch.mockResolvedValueOnce(Response.json({ ops: [] }));
  expect(
    await requestAiEdit({
      documentId: 'document',
      prompt: 'Improve this',
      onOps,
    })
  ).toEqual({ kind: 'ok' });
  expect(onOps).toHaveBeenCalledWith([]);

  fetch.mockResolvedValueOnce(
    Response.json({ ops: [], clarification: 'Which paragraph?' })
  );
  const result = await requestAiEdit({
    documentId: 'document',
    prompt: 'Improve this',
  });
  toastAiEditResult(result);

  expect(failure).toHaveBeenCalledWith('Which paragraph?');
});

test('runs the edit request under its explicitly supplied parent context', async () => {
  const { NOOP_SPAN } = await vi.importActual<
    typeof import('@macro-inc/observability')
  >('@macro-inc/observability');
  const run = vi.spyOn(NOOP_SPAN, 'run');
  const parent = NOOP_SPAN;
  fetch.mockResolvedValue(new Response('{}', { status: 200 }));
  await requestAiEdit({
    documentId: 'document',
    prompt: 'test',
    parentSpan: parent,
  });
  expect(run).toHaveBeenCalledOnce();
  expect(span.injectTraceHeaders).toHaveBeenCalledOnce();
});
