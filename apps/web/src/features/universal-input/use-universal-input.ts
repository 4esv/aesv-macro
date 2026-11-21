import { useFeatureFlag } from '@app/lib/analytics/flags';
import { enableUniversalInput } from '@core/constant/featureFlags';

/** The rollout flag is the only control for enabling Universal Input. */
export function useUniversalInputEnabled() {
  const flag = useFeatureFlag(enableUniversalInput);
  return () => !flag().loading && flag().enabled;
}
