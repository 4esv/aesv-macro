import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  isTauri: vi.fn(() => false),
  graphqlEnabled: vi.fn(() => true),
  killOverride: undefined as boolean | undefined,
  unsupportedBrowserReason: vi.fn<() => string | undefined>(),
}));

vi.mock('@core/util/platform', () => ({ isTauri: mocks.isTauri }));
vi.mock('./host/browser-support', () => ({
  unsupportedBrowserReason: mocks.unsupportedBrowserReason,
}));
vi.mock('@core/constant/featureFlags', () => ({
  enableGraphqlSoup: { key: 'enable-graphql-soup' },
  disableBrowserTursoCache: {
    key: 'disable-browser-turso-cache',
    get override() {
      return mocks.killOverride;
    },
  },
  isFeatureEnabled: mocks.graphqlEnabled,
}));

import { getBrowserTursoCacheRolloutDecision } from './rollout';

describe('browser Turso cache production gate', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.isTauri.mockReturnValue(false);
    mocks.graphqlEnabled.mockReturnValue(true);
    mocks.killOverride = undefined;
    mocks.unsupportedBrowserReason.mockReturnValue(undefined);
  });

  it('enables browser cache with the GraphQL soup gate', () => {
    expect(getBrowserTursoCacheRolloutDecision()).toMatchObject({
      enabled: true,
      cohort: 'treatment',
      reason: 'graphql-transport-enabled',
    });
  });

  it('keeps browsers without the cache APIs uncached', () => {
    mocks.unsupportedBrowserReason.mockReturnValue(
      'OPFS is not supported by this browser'
    );
    expect(getBrowserTursoCacheRolloutDecision()).toMatchObject({
      enabled: false,
      reason: 'browser-unsupported',
    });
  });

  it('lets the independent emergency kill win', () => {
    mocks.killOverride = true;
    expect(getBrowserTursoCacheRolloutDecision()).toMatchObject({
      enabled: false,
      reason: 'emergency-disabled',
    });
  });

  it('disables browser cache with the GraphQL soup gate', () => {
    mocks.graphqlEnabled.mockReturnValue(false);
    expect(getBrowserTursoCacheRolloutDecision()).toMatchObject({
      enabled: false,
      reason: 'graphql-transport-disabled',
    });
  });

  it('leaves the native cache unchanged on Tauri', () => {
    mocks.isTauri.mockReturnValue(true);
    mocks.killOverride = true;

    expect(getBrowserTursoCacheRolloutDecision()).toMatchObject({
      enabled: true,
      nativeCacheUnchanged: true,
    });
  });

  it('imports without constructing browser workers', async () => {
    const WorkerConstructor = vi.fn(() => {
      throw new Error('worker must stay lazy');
    });
    vi.stubGlobal('Worker', WorkerConstructor);
    vi.stubGlobal('SharedWorker', WorkerConstructor);

    await import('./worker/coordinator-page-adapter');

    expect(WorkerConstructor).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });
});
