import type { RemoteFlag } from '@core/constant/featureFlags';
import { describe, expect, it } from 'vitest';
import { useFeatureFlag } from './flags';

const flag = (override: boolean | undefined): RemoteFlag => ({
  key: 'enable-channel-tags',
  override,
});

describe('useFeatureFlag without a remote flag service', () => {
  it('follows the env override or default', () => {
    expect(useFeatureFlag(flag(true))()).toEqual({
      enabled: true,
      payload: undefined,
      loading: false,
    });
    expect(useFeatureFlag(flag(false))().enabled).toBe(false);
  });

  it('is off when nothing decides it, unless the caller asks for on', () => {
    expect(useFeatureFlag(flag(undefined))().enabled).toBe(false);
    expect(
      useFeatureFlag(flag(undefined), { enabledWhenUnreachable: true })()
        .enabled
    ).toBe(true);
    expect(
      useFeatureFlag(flag(false), { enabledWhenUnreachable: true })().enabled
    ).toBe(false);
  });

  it('returns the fallback payload', () => {
    expect(
      useFeatureFlag<string>(flag(true), { fallbackPayload: 'x' })().payload
    ).toBe('x');
  });
});
