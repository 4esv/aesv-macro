import { useFeatureFlag } from '@app/lib/analytics/posthog';
import { t } from '@app/lib/i18n';
import { toast } from '@core/component/Toast/Toast';
import { enableMacroComputers } from '@core/constant/featureFlags';
import { fireAndForget } from '@core/util/fireAndForget';
import { useAgentsQuery } from '@queries/agents/agents';
import {
  useClaudeModeQuery,
  useClaudeSignOutMutation,
} from '@queries/computers/claude';
import {
  useReleaseComputerMutation,
  useRetryComputerMutation,
  useSetComputerPowerMutation,
} from '@queries/computers/computers';
import { useIsTeamAdmin } from '@queries/team/teams';
import type { ComputerStatus } from '@service-auth/computers';
import type { Harness as RegisteredHarness } from '@service-storage/client';
import { confirmDialog } from '@ui';
import { createEffect, createSignal, For, on, Show } from 'solid-js';
import { MachineCardError } from '../../components/connected-runtimes-section';
import { SettingsCard } from '../../primitives';
import type { ComputerMenuItem } from '../components/computer-actions-menu';
import {
  type ComputerClaudeState,
  type ComputerPrimaryAction,
  ComputerRow,
} from '../components/computer-row';
import { ReleaseComputerDialog } from '../components/release-computer-dialog';
import {
  canTogglePower,
  computerFailureMessage,
} from '../core/computer-display';
import {
  computerSetupRequest,
  consumeComputerSetupRequest,
} from '../primitives/setup-request';
import { useTeamComputers } from '../primitives/team-computers';
import { ClaudeSignInDialog } from './claude-sign-in-dialog';
import { ComputerBuyFlow } from './computer-buy-flow';
import type { SetupStepId } from './computer-setup-dialog';

/** One computer's row, with its Claude state and the actions the viewer has. */
function ManagedComputerRow(props: {
  computer: ComputerStatus;
  harness?: RegisteredHarness;
  canManage: boolean;
  canManageClaude: boolean;
  /** A team owner or admin, whom the server lets finish setup. */
  canFinishSetup: boolean;
  onFinishSetup: () => void;
  onSignIn: () => void;
  onRelease: () => void;
}) {
  const rollout = useFeatureFlag(enableMacroComputers);
  const mode = useClaudeModeQuery(() =>
    rollout().enabled && props.canManageClaude
      ? (props.computer.harnessId ?? undefined)
      : undefined
  );
  const power = useSetComputerPowerMutation();
  const retry = useRetryComputerMutation();
  const signOut = useClaudeSignOutMutation();

  // Claude's long-lived credential names no account; it only expires.
  const expired = () => {
    const expiresAt = mode.isSuccess ? mode.data.account?.expiresAt : undefined;
    return expiresAt ? new Date(expiresAt).getTime() <= Date.now() : false;
  };
  const currentMode = () =>
    mode.isSuccess ? mode.data.mode : props.harness?.claude_mode;
  const claude = (): ComputerClaudeState => {
    // Guided setup picks Claude; until then there is nothing to show.
    if (!props.computer.setupCompletedAt) return { kind: 'unset' };
    const value = currentMode();
    if (value === 'macro_credits') return { kind: 'credits' };
    if (value === 'own_account') return { kind: 'account', expired: expired() };
    return { kind: 'loading' };
  };
  const setupIncomplete = () => !props.computer.setupCompletedAt;
  const claudeMissing = () => {
    const state = claude();
    return state.kind === 'account' && state.expired;
  };

  const failed = () => props.computer.status === 'failed';
  const primary = (): ComputerPrimaryAction | undefined => {
    // A failed setup only moves again once it is retried.
    if (failed() && props.canManage)
      return {
        label: t('computers.retrySetup'),
        disabled: retry.isPending,
        onClick: () => run(retrySetup()),
      };
    if (setupIncomplete()) {
      if (!props.canFinishSetup) return undefined;
      return {
        label: t('computers.finishSetup'),
        onClick: props.onFinishSetup,
      };
    }
    if (props.canManageClaude && claudeMissing())
      return { label: t('computers.claudeSignIn'), onClick: props.onSignIn };
    return undefined;
  };

  const turnOff = async () => {
    const confirmed = await confirmDialog({
      title: t('computers.turnOffTitle', { name: props.computer.name }),
      body: t('computers.turnOffBody'),
      confirmLabel: t('computers.turnOff'),
      tone: 'danger',
    });
    if (confirmed) await setPower(false);
  };
  const setPower = async (on: boolean) => {
    try {
      await power.mutateAsync({ computerId: props.computer.id, on });
    } catch (error) {
      toast.failure(
        computerFailureMessage(
          error,
          on ? t('computers.powerOnFailed') : t('computers.powerOffFailed')
        )
      );
    }
  };
  const switchToCredits = async () => {
    const harnessId = props.computer.harnessId;
    if (!harnessId) return;
    const confirmed = await confirmDialog({
      title: t('computers.switchToCreditsTitle', { name: props.computer.name }),
      body: t('computers.switchToCreditsBody'),
      confirmLabel: t('computers.switchToCredits'),
    });
    if (!confirmed) return;
    try {
      await signOut.mutateAsync(harnessId);
    } catch {
      toast.failure(t('computers.claudeCreditsFailed'));
    }
  };
  const switchToAccount = async () => {
    const confirmed = await confirmDialog({
      title: t('computers.switchToAccountTitle', { name: props.computer.name }),
      body: t('computers.switchToAccountBody'),
      confirmLabel: t('computers.continueToSignIn'),
    });
    if (confirmed) props.onSignIn();
  };
  const retrySetup = async () => {
    try {
      await retry.mutateAsync(props.computer.id);
    } catch (error) {
      toast.failure(computerFailureMessage(error, t('computers.retryFailed')));
    }
  };

  const run = (work: Promise<void>) => fireAndForget(work, 'macro-computers');

  const menu = (): ComputerMenuItem[] => {
    const items: ComputerMenuItem[] = [];
    const computer = props.computer;
    // Once billing has ended it only turns back on after a Restore.
    const canPower =
      canTogglePower(computer) &&
      (computer.poweredOn || computer.billing !== 'ended');
    // Retry setup is the row's action; the menu keeps the rest.
    if (props.canManage && computer.status === 'failed' && !primary()) {
      items.push({
        label: t('computers.retry'),
        icon: 'retry',
        disabled: retry.isPending,
        onSelect: () => run(retrySetup()),
      });
    }
    if (props.canManage && canPower) {
      items.push(
        computer.poweredOn
          ? {
              label: t('computers.turnOff'),
              icon: 'power',
              disabled: power.isPending,
              onSelect: () => run(turnOff()),
            }
          : {
              label: t('computers.turnOn'),
              icon: 'power',
              disabled: power.isPending,
              onSelect: () => run(setPower(true)),
            }
      );
    }
    if (props.canManageClaude && !setupIncomplete()) {
      const state = claude();
      if (state.kind === 'account')
        items.push({
          label: t('computers.switchToCredits'),
          icon: 'credits',
          onSelect: () => run(switchToCredits()),
        });
      if (state.kind === 'credits')
        items.push({
          label: t('computers.claudeSignIn'),
          icon: 'sign-in',
          onSelect: () => run(switchToAccount()),
        });
    }
    if (props.canManage && computer.status !== 'purchasing') {
      items.push({
        label: t('computers.releaseNow'),
        icon: 'release',
        danger: true,
        onSelect: props.onRelease,
      });
    }
    return items;
  };

  return (
    <ComputerRow
      computer={props.computer}
      claude={claude()}
      primary={primary()}
      menu={menu()}
      harness={props.harness?.access === 'owner' ? props.harness : undefined}
    />
  );
}

/**
 * The team's Macro computers on Settings → Machines: every member sees the
 * list; team admins also get the power controls; whoever may manage a
 * computer's Claude gets those.
 */
export function MacroComputers(props: {
  harnesses: readonly RegisteredHarness[];
}) {
  const isAdmin = useIsTeamAdmin();
  const { enabled, computers, failed } = useTeamComputers();
  const release = useReleaseComputerMutation();
  const agents = useAgentsQuery();
  const [settingUp, setSettingUp] = createSignal<string>();
  // Set when a setup notification reopens the flow: it lands on Bring your AI.
  const [resumeAt, setResumeAt] =
    createSignal<Exclude<SetupStepId, 'computer'>>();
  const [signingIn, setSigningIn] = createSignal<string>();
  const [releasing, setReleasing] = createSignal<ComputerStatus>();
  const [releaseError, setReleaseError] = createSignal<string>();

  // A setup notification, or its link opened cold, names the computer.
  createEffect(
    on(computerSetupRequest, (request) => {
      if (!request) return;
      setResumeAt('claude');
      setSettingUp(request.computerId);
      consumeComputerSetupRequest(request.id);
    })
  );

  const harnessFor = (computer: ComputerStatus) =>
    props.harnesses.find((candidate) => candidate.id === computer.harnessId);

  // The computer's team owner and admins own its harness, not its buyer.
  const canManageClaude = (computer: ComputerStatus) =>
    harnessFor(computer)?.access === 'owner';

  const releaseNow = async (computer: ComputerStatus, typedName: string) => {
    setReleaseError(undefined);
    try {
      await release.mutateAsync({
        computerId: computer.id,
        confirmName: typedName,
      });
      setReleasing(undefined);
      toast.success(t('computers.released', { name: computer.name }));
    } catch (error) {
      setReleaseError(
        computerFailureMessage(error, t('computers.releaseFailed'))
      );
    }
  };

  const closeRelease = () => {
    if (release.isPending) return;
    setReleasing(undefined);
    setReleaseError(undefined);
  };

  // Releasing deletes every agent bound to the computer's runtime.
  const agentsOn = (computer: ComputerStatus) =>
    agents.isSuccess && computer.harnessId
      ? agents.data
          .filter((agent) => agent.harness_id === computer.harnessId)
          .map((agent) => agent.bot.name)
      : [];

  const byId = (id: string | undefined) =>
    computers().find((computer) => computer.id === id);

  return (
    <Show when={enabled()}>
      <Show when={computers().length > 0 || failed()}>
        <div data-settings-target="macro-computer">
          <h2 class="mb-3 px-6 text-sm font-semibold text-ink">
            {t('computers.title')}
          </h2>
          <SettingsCard inset="wide">
            <For each={computers()}>
              {(computer) => (
                <ManagedComputerRow
                  computer={computer}
                  harness={harnessFor(computer)}
                  canManage={isAdmin()}
                  canManageClaude={canManageClaude(computer)}
                  canFinishSetup={isAdmin()}
                  onFinishSetup={() => {
                    setResumeAt(undefined);
                    setSettingUp(computer.id);
                  }}
                  onSignIn={() => setSigningIn(computer.id)}
                  onRelease={() => {
                    setReleaseError(undefined);
                    setReleasing(computer);
                  }}
                />
              )}
            </For>
            <Show when={failed()}>
              <MachineCardError>{t('computers.loadFailed')}</MachineCardError>
            </Show>
          </SettingsCard>
        </div>
      </Show>

      <ComputerBuyFlow
        open={!!settingUp()}
        computerId={settingUp()}
        resumeStep={resumeAt()}
        onClose={() => {
          setSettingUp(undefined);
          setResumeAt(undefined);
        }}
      />
      <Show when={byId(signingIn())?.harnessId}>
        {(harnessId) => (
          <ClaudeSignInDialog
            harnessId={harnessId()}
            entry="computer_row"
            onClose={() => setSigningIn(undefined)}
          />
        )}
      </Show>
      <Show when={releasing()} keyed>
        {(computer) => (
          <ReleaseComputerDialog
            computerName={computer.name}
            agentCount={computer.agentCount}
            agentNames={agentsOn(computer)}
            pending={release.isPending}
            error={releaseError()}
            onClose={closeRelease}
            onRelease={(typedName) =>
              fireAndForget(releaseNow(computer, typedName), 'macro-computers')
            }
          />
        )}
      </Show>
    </Show>
  );
}
