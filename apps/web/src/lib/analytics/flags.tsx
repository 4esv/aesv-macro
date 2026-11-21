import type { RemoteFlag } from '@core/constant/featureFlags';
import { createAssertedContextProvider } from '@core/context/createContext';
import { type Accessor, type JSX, Show } from 'solid-js';

type JsonType =
  | string
  | number
  | boolean
  | null
  | { [key: string]: JsonType }
  | JsonType[];

// This build has no remote flag service: a flag is its env override or
// `default`, and the answer is known at startup.
export const [FlagsProvider, useFlags] = createAssertedContextProvider(
  'FlagsProvider',
  () => ({ flagsLoaded: () => true })
);

type FeatureFlagResult<T> = {
  enabled: boolean;
  payload: T;
  loading: boolean;
};

type FeatureFlagOpts<T> = {
  fallbackPayload?: T;
  enabledOverride?: boolean;
  /** Treat the flag as on when nothing decides it. */
  enabledWhenUnreachable?: boolean;
};

function readFeatureFlag<T extends JsonType>(
  flagOrKey: RemoteFlag | string,
  opts?: FeatureFlagOpts<T>
): Accessor<FeatureFlagResult<T | undefined>> {
  const remote = typeof flagOrKey === 'string' ? undefined : flagOrKey;
  const result = {
    enabled:
      remote?.override ??
      opts?.enabledOverride ??
      opts?.enabledWhenUnreachable === true,
    payload: opts?.fallbackPayload,
    loading: false,
  };
  return () => result;
}

export function useFeatureFlag<T extends JsonType>(
  flag: RemoteFlag,
  opts?: Pick<FeatureFlagOpts<T>, 'fallbackPayload' | 'enabledWhenUnreachable'>
): Accessor<FeatureFlagResult<T | undefined>>;
export function useFeatureFlag<T extends JsonType>(
  key: string,
  opts?: FeatureFlagOpts<T>
): Accessor<FeatureFlagResult<T | undefined>>;
export function useFeatureFlag<T extends JsonType>(
  flagOrKey: RemoteFlag | string,
  opts?: FeatureFlagOpts<T>
): Accessor<FeatureFlagResult<T | undefined>> {
  return readFeatureFlag(flagOrKey, opts);
}

type ShowFeatureFlagProps<T> = {
  fallback?: JSX.Element;
  fallbackPayload?: T;
  children: JSX.Element;
} & ({ flag: RemoteFlag } | { key: string; enabledOverride?: boolean });

export const ShowFeatureFlag = <T extends JsonType>(
  props: ShowFeatureFlagProps<T>
) => {
  const flag = readFeatureFlag('flag' in props ? props.flag : props.key, {
    fallbackPayload: props.fallbackPayload,
    enabledOverride: 'flag' in props ? undefined : props.enabledOverride,
  });

  return (
    <Show when={flag().enabled} fallback={props.fallback}>
      {props.children}
    </Show>
  );
};
