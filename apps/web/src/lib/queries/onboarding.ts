/** Closes the server's onboarding row; there is no onboarding flow to show. */

import { throwOnErr } from '@core/util/result';
import { onboardingClient } from '@service-cognition/onboarding';
import { useMutation } from '@tanstack/solid-query';

export function useCompleteOnboardingMutation() {
  return useMutation(() => ({
    mutationFn: async (args: { skipped: boolean }) =>
      throwOnErr(() => onboardingClient.complete(args)),
  }));
}
