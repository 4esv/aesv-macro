import { toast } from '@core/component/Toast/Toast';
import type { IUser } from '@core/user';
import { idToDisplayName, idToEmail } from '@core/user/util';
import { fireAndForget } from '@core/util/fireAndForget';
import { throwOnErr } from '@core/util/result';
import { authServiceClient } from '@service-auth/client';
import type { TeamWithMembers } from '@service-auth/generated/schemas/teamWithMembers';
import { useMutation } from '@tanstack/solid-query';
import { type Accessor, createMemo } from 'solid-js';
import { queryClient } from '../client';
import { queryReadyGate } from '../gate';
import { type MutationCallbacks, withCallbacks } from '../utils';
import { teamKeys } from './keys';
import { invalidateTeam, useCurrentTeamQuery } from './teams';

type RemoveUserFromTeamArgs = { teamId: string; userId: string };
type RemoveUserFromTeamContext = { previousTeam: TeamWithMembers | undefined };
type RemoveUserFromTeamCallbacks = MutationCallbacks<
  void,
  Error,
  RemoveUserFromTeamArgs,
  RemoveUserFromTeamContext
>;

/** The cached current team roster, independent of the viewer's contacts. */
export function useTeamMembers(enabled: Accessor<boolean>) {
  const query = useCurrentTeamQuery(enabled);
  const users = createMemo((): IUser[] => {
    if (!enabled()) return [];
    if (!queryReadyGate(query)) return [];

    return (query.data?.members ?? []).map(({ user_id }) => ({
      id: user_id,
      email: idToEmail(user_id),
      name: idToDisplayName(user_id),
    }));
  });

  return {
    users,
    isLoading: () => enabled() && query.isPending,
    isError: () => enabled() && query.isError,
  };
}

export function useRemoveUserFromTeamMutation(
  callbacks?: RemoveUserFromTeamCallbacks
) {
  return useMutation(() => ({
    mutationFn: async ({ userId }: RemoveUserFromTeamArgs) => {
      await throwOnErr(() => authServiceClient.removeUserFromTeam(userId));
    },

    ...withCallbacks<
      void,
      Error,
      RemoveUserFromTeamArgs,
      RemoveUserFromTeamContext
    >(
      {
        onMutate: async ({ teamId, userId }) => {
          const queryKey = teamKeys.detail(teamId).queryKey;
          await queryClient.cancelQueries({ queryKey });

          const previousTeam =
            queryClient.getQueryData<TeamWithMembers>(queryKey);

          queryClient.setQueryData<TeamWithMembers>(queryKey, (old) =>
            old
              ? {
                  ...old,
                  members: old.members.filter(
                    (member) => member.user_id !== userId
                  ),
                }
              : undefined
          );

          return { previousTeam };
        },

        onSuccess: (_data, { teamId }) => {
          fireAndForget(invalidateTeam(teamId), 'team-settings');
          toast.success('Member removed');
        },

        onError: (error, { teamId }, context) => {
          console.error('Failed to remove user from team', error);
          toast.failure('Failed to remove team member');

          if (context?.previousTeam) {
            queryClient.setQueryData(
              teamKeys.detail(teamId).queryKey,
              context.previousTeam
            );
          }
        },
      },
      callbacks
    ),
  }));
}
