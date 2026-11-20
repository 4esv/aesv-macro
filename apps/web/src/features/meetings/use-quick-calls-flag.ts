import { useFeatureFlag } from '@app/lib/analytics/flags';
import { ENABLE_CALLS, enableQuickCalls } from '@core/constant/featureFlags';

export function useQuickCallsFlag() {
  const flag = useFeatureFlag(enableQuickCalls);
  return () => ({ ...flag(), enabled: ENABLE_CALLS && flag().enabled });
}
