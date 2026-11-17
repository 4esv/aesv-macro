import { t } from '@app/lib/i18n';
import { useUserId } from '@core/context/user';
import { getDisplayNameParts, tryMacroId } from '@core/user';
import SpinnerIcon from '@icon/system/regular/spinner.svg';
import { useHarnessesQuery } from '@queries/harnesses/harnesses';
import { isTeamAgentComputer } from '@queries/harnesses/runnable';
import type { ClaudeMode } from '@service-agent-harness/generated/schemas';
import type { ComputerStatus } from '@service-auth/computers';
import { ActionDialogShell, Button, Dialog } from '@ui';
import { createEffect, createSignal, on, Show } from 'solid-js';
import { showComputerSetupNotice } from '../primitives/setup-notice';
import { useTeamComputers } from '../primitives/team-computers';
import {
  ComputerSetupSteps,
  computerConnected,
  type SetupStepId,
  StepHeader,
} from './computer-setup-dialog';

/** The viewer's first name, for "Maya's Agent Box" and "Maya's Agent". */
export function useViewerFirstName() {
  const userId = useUserId();
  return () =>
    getDisplayNameParts(tryMacroId(userId() ?? '')).firstName || undefined;
}

/**
 * Setup resumed from a row's Finish setup, before the team's computer list
 * has the computer. The dialog can be closed meanwhile.
 */
function ResumingStep(props: { step: SetupStepId; onClose: () => void }) {
  return (
    <>
      <ActionDialogShell.Body>
        <StepHeader step={props.step} claude="step" />
        <ActionDialogShell.Header>
          <ActionDialogShell.Title>
            {t('computers.setupAgentTitle')}
          </ActionDialogShell.Title>
        </ActionDialogShell.Header>
        <p role="status" class="flex items-center gap-2 text-sm text-ink-muted">
          <SpinnerIcon class="size-4 animate-spin" />
          {t('common.loading')}
        </p>
      </ActionDialogShell.Body>
      <ActionDialogShell.Footer>
        <Button type="button" variant="ghost" onClick={props.onClose}>
          {t('common.close')}
        </Button>
      </ActionDialogShell.Footer>
    </>
  );
}

/**
 * Guided setup of a team computer in one modal: Configure agent → Bring your
 * AI → Create agent. Opened with `computerId` from a row's Finish setup.
 *
 * It keeps its place when closed. Closed while waiting for the Claude
 * sign-in link, it toasts once the link is ready, and the toast reopens it on
 * that step. `onFinished` gets the computer when it closes, so a caller can
 * select it.
 */
export function ComputerBuyFlow(props: {
  open: boolean;
  /** The computer to set up. */
  computerId?: string;
  /** Where resuming starts; Configure agent unless a link names a step. */
  resumeStep?: Exclude<SetupStepId, 'computer'>;
  onClose: () => void;
  onFinished?: (computer: ComputerStatus) => void;
  /** Opened from an agent's Where it runs, which `onFinished` updates. */
  selectsForAgent?: boolean;
}) {
  const firstName = useViewerFirstName();
  const { computers } = useTeamComputers();
  const harnesses = useHarnessesQuery();
  const [step, setStep] = createSignal<SetupStepId>('computer');
  const [name, setName] = createSignal<string>();
  const [choice, setChoice] = createSignal<ClaudeMode>();
  const [reopened, setReopened] = createSignal(false);
  const [waitingForLink, setWaitingForLink] = createSignal(false);
  // Choosing credits after opening the sign-in completes Bring your AI
  // rather than skipping it.
  const [signInVisited, setSignInVisited] = createSignal(false);
  createEffect(() => {
    if (step() === 'claude') setSignInVisited(true);
  });

  const computerId = () => props.computerId;
  const computer = () => computers().find((item) => item.id === computerId());
  const connected = () => {
    const id = computer()?.harnessId;
    return harnesses.isSuccess
      ? harnesses.data.some((harness) => harness.id === id && harness.connected)
      : false;
  };
  const up = () => computerConnected(computer(), connected());
  // An agent teammate's computer runs only its agent: nothing to try or
  // create on it.
  const teamAgentComputer = () => {
    const id = computer()?.harnessId;
    return harnesses.isSuccess
      ? harnesses.data.some(
          (harness) => harness.id === id && isTeamAgentComputer(harness)
        )
      : false;
  };
  const visible = () => props.open || reopened();
  const setupStep = (): Exclude<SetupStepId, 'computer'> => {
    const current = step();
    return current === 'computer' ? 'agent' : current;
  };

  // Each step replaces the content, and with it the focused control; keep
  // focus in the modal so Escape and Tab still reach it.
  let content: HTMLDivElement | undefined;
  createEffect(
    on(step, () => {
      queueMicrotask(() => {
        if (content && !content.contains(document.activeElement))
          content.focus();
      });
    })
  );

  // Resuming starts on the agent step, or where a setup link points; a
  // link for another computer than the one shown moves there too.
  createEffect(
    on(
      () => [props.open, props.computerId, props.resumeStep] as const,
      ([open, computerId, resumeStep], previous) => {
        if (!open || !computerId) return;
        if (
          resumeStep &&
          (resumeStep !== previous?.[2] || computerId !== previous?.[1])
        )
          setStep(resumeStep);
        else if (step() === 'computer') setStep('agent');
      }
    )
  );

  const close = () => {
    setWaitingForLink(
      step() === 'claude' && choice() === 'own_account' && !up()
    );
    setReopened(false);
    const current = computer();
    props.onClose();
    if (current) props.onFinished?.(current);
  };

  // The link the user was waiting for is ready: say so, once.
  createEffect(() => {
    const current = computer();
    if (!waitingForLink() || visible() || !current || !up()) return;
    setWaitingForLink(false);
    // The same notice the computer's setup notification shows, so the two
    // never stack; this one reopens this dialog in place.
    showComputerSetupNotice(
      {
        computerId: current.id,
        harnessId: current.harnessId ?? null,
        computerName: current.name,
        outcome: { kind: 'ready' },
      },
      () => {
        setStep('claude');
        setReopened(true);
      }
    );
  });

  return (
    <Show when={visible()}>
      <Dialog
        open
        onOpenChange={(open) => !open && close()}
        position="center"
        visibleScrim
        contentRef={(element) => {
          content = element;
        }}
        class="w-[min(600px,calc(100vw-16px))]"
      >
        <ActionDialogShell class="h-auto">
          <Show
            when={step() !== 'computer' && computer()}
            fallback={<ResumingStep step={setupStep()} onClose={close} />}
          >
            {(current) => (
              <ComputerSetupSteps
                computer={current()}
                connected={connected()}
                firstName={firstName()}
                step={setupStep()}
                name={name()}
                choice={choice()}
                onName={setName}
                onChoice={setChoice}
                onStep={setStep}
                onClose={close}
                selectsForAgent={props.selectsForAgent}
                teamAgentComputer={teamAgentComputer()}
                signInVisited={signInVisited()}
              />
            )}
          </Show>
        </ActionDialogShell>
      </Dialog>
    </Show>
  );
}
