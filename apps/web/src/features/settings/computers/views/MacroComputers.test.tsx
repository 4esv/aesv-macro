/**
 * @vitest-environment jsdom
 */

import type { ComputerStatus } from '@service-auth/computers';
import { fireEvent, render, screen, waitFor } from '@solidjs/testing-library';
import { For } from 'solid-js';
import { beforeAll, beforeEach, expect, it, vi } from 'vitest';
import {
  computerSetupRequest,
  requestComputerSetup,
} from '../primitives/setup-request';
import { MacroComputers } from './MacroComputers';

/** Machines as Harness lays it out. */
function Machines() {
  return <MacroComputers harnesses={[]} />;
}

const mocks = vi.hoisted(() => ({
  rollout: true,
  computers: [] as ComputerStatus[],
  release: vi.fn(),
  retry: vi.fn(),
  confirm: vi.fn(),
  toastFailure: vi.fn(),
  toastSuccess: vi.fn(),
}));

vi.mock('@queries/computers/computers', () => ({
  useSetComputerPowerMutation: () => ({ isPending: false }),
  useReleaseComputerMutation: () => ({
    isPending: false,
    mutateAsync: mocks.release,
  }),
  useRetryComputerMutation: () => ({
    isPending: false,
    mutateAsync: mocks.retry,
  }),
}));

vi.mock('@queries/agents/agents', () => ({
  useAgentsQuery: () => ({ isSuccess: true, data: [] }),
}));

vi.mock('@queries/computers/claude', () => ({
  useClaudeModeQuery: () => ({ isSuccess: false }),
  useClaudeSignOutMutation: () => ({ isPending: false }),
}));

vi.mock('./computer-setup-dialog', () => ({
  ComputerSetupSteps: (props: { step: string; computer: { id: string } }) => (
    <p data-testid="setup-step">{`${props.computer.id}:${props.step}`}</p>
  ),
  StepHeader: () => null,
  computerConnected: () => false,
}));

vi.mock('@queries/harnesses/harnesses', () => ({
  useHarnessesQuery: () => ({ isSuccess: true, data: [] }),
}));

vi.mock('./claude-sign-in-dialog', () => ({
  ClaudeSignInDialog: () => null,
}));

// The menu's own rendering is the design system's; these tests cover which
// items a computer offers.
vi.mock('../components/computer-actions-menu', () => ({
  ComputerActionsMenu: (props: {
    items: { label: string; onSelect: () => void }[];
  }) => (
    <For each={props.items}>
      {(item) => (
        <button type="button" role="menuitem" onClick={item.onSelect}>
          {item.label}
        </button>
      )}
    </For>
  ),
}));

vi.mock('../primitives/team-computers', () => ({
  useTeamComputers: () => ({
    enabled: () => true,
    computers: () => mocks.computers,
    failed: () => false,
  }),
}));

vi.mock('@app/lib/analytics/posthog', () => ({
  useFeatureFlag: () => () => ({ enabled: mocks.rollout, loading: false }),
}));

vi.mock('@core/context/user', () => ({ useUserId: () => () => 'viewer' }));

vi.mock('@core/user', () => ({
  getDisplayNameParts: () => ({
    firstName: 'Maya',
    lastName: '',
    fullName: '',
  }),
  tryMacroId: (id: string) => id,
}));

vi.mock('@queries/team/teams', () => ({
  useIsTeamAdmin: () => () => true,
  useIsTeamOwner: () => () => true,
}));

vi.mock('@ui', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@ui')>()),
  confirmDialog: mocks.confirm,
}));

vi.mock('@core/component/Toast/Toast', () => ({
  toast: { success: mocks.toastSuccess, failure: mocks.toastFailure },
}));

beforeAll(() => {
  vi.stubGlobal('scrollTo', vi.fn());
});

beforeEach(() => {
  vi.clearAllMocks();
  mocks.rollout = true;
  mocks.computers = [];
});

it('says why a computer failed and offers Retry setup on its row, and only there', async () => {
  const failed: ComputerStatus = {
    id: 'computer-1',
    name: 'Build machine',
    size: 'standard',
    vcpus: 4,
    memoryGib: 8,
    diskGib: 100,
    catalogEntry: 'standard@2026-11',
    status: 'failed',
    billing: 'active',
    poweredOn: true,
    agentCount: 0,
    canUndoCancel: false,
    canRestore: false,
    purchasedBy: 'viewer',
    createdAt: '2026-01-01T00:00:00Z',
  };
  // The fail-fast state's reason, as the API sends it.
  Reflect.set(failed, 'failureReason', 'The host ran out of disk.');
  mocks.computers = [failed, { ...failed, id: 'computer-2', status: 'ready' }];
  mocks.retry.mockResolvedValueOnce({ ...failed, status: 'setting_up' });

  render(() => <Machines />);

  expect(
    screen.getByText(/Setup failed: The host ran out of disk\./)
  ).toBeTruthy();
  // Only the ready computer, whose guided setup is not done, offers it.
  expect(screen.getAllByRole('button', { name: 'Finish setup' })).toHaveLength(
    1
  );
  // The row's action, not a second Retry in its menu.
  expect(screen.queryByRole('menuitem', { name: 'Retry' })).toBeNull();
  const retry = screen.getAllByRole('button', { name: 'Retry setup' });
  expect(retry).toHaveLength(1);
  fireEvent.click(retry[0]);
  await waitFor(() => expect(mocks.retry).toHaveBeenCalledWith('computer-1'));
  expect(mocks.toastFailure).not.toHaveBeenCalled();
});

it('offers no power, cancel or release while a purchase is being confirmed', () => {
  mocks.computers = [
    {
      id: 'computer-1',
      name: 'Build machine',
      size: 'standard',
      vcpus: 4,
      memoryGib: 8,
      diskGib: 100,
      catalogEntry: 'standard@2026-11',
      status: 'purchasing',
      billing: 'purchasing',
      setup: { step: 'creating', position: 1, total: 4 },
      poweredOn: false,
      agentCount: 0,
      canUndoCancel: false,
      canRestore: false,
      purchasedBy: 'viewer',
      createdAt: '2026-01-01T00:00:00Z',
    },
  ];

  render(() => <Machines />);

  expect(screen.queryAllByRole('menuitem')).toEqual([]);
});

it('keeps existing computers and release available with rollout off', async () => {
  mocks.rollout = false;
  mocks.computers = [
    {
      id: 'computer-1',
      name: 'Build machine',
      size: 'standard',
      vcpus: 4,
      memoryGib: 8,
      diskGib: 100,
      catalogEntry: 'standard@2026-11',
      status: 'ready',
      billing: 'active',
      poweredOn: true,
      agentCount: 0,
      canUndoCancel: false,
      canRestore: false,
      purchasedBy: 'viewer',
      createdAt: '2026-01-01T00:00:00Z',
    },
  ];
  render(() => <Machines />);
  expect(screen.getByText('Build machine')).toBeTruthy();
  expect(
    screen.queryByRole('menuitem', { name: 'Cancel at period end' })
  ).toBeNull();
  fireEvent.click(screen.getByRole('menuitem', { name: 'Release now' }));
  expect(await screen.findByLabelText(/Type/)).toBeTruthy();
});

it('offers Finish setup until setup is done, and nothing once Claude is signed in', () => {
  const harness = {
    id: 'harness-build',
    name: 'Build machine',
    kind: 'macrod' as const,
    owner: { type: 'user' as const, user_id: 'viewer' },
    access: 'owner' as const,
    shared_team_ids: [],
    created_by: 'viewer',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    connected: true,
    last_connected_at: null,
    allow_permission_bypass: true,
    computer_id: 'computer-1',
    claude_mode: 'own_account' as const,
  };
  const computer: ComputerStatus = {
    id: 'computer-1',
    name: 'Build machine',
    size: 'standard',
    vcpus: 4,
    memoryGib: 8,
    diskGib: 100,
    catalogEntry: 'standard@2026-11',
    status: 'ready',
    billing: 'active',
    poweredOn: true,
    agentCount: 0,
    canUndoCancel: false,
    canRestore: false,
    purchasedBy: 'viewer',
    createdAt: '2026-01-01T00:00:00Z',
    harnessId: 'harness-build',
    setupCompletedAt: null,
  };
  mocks.computers = [computer];
  const first = render(() => <MacroComputers harnesses={[harness]} />);
  expect(screen.getByRole('button', { name: 'Finish setup' })).toBeTruthy();
  expect(
    screen.queryByRole('button', { name: 'Sign in with Claude' })
  ).toBeNull();
  first.unmount();

  mocks.computers = [{ ...computer, setupCompletedAt: '2026-01-02T00:00:00Z' }];
  const second = render(() => <MacroComputers harnesses={[harness]} />);
  // Signed in with Claude and not expired: nothing to do.
  expect(
    screen.queryByRole('button', { name: 'Sign in with Claude' })
  ).toBeNull();
  expect(screen.getByText(/Signed in with Claude/)).toBeTruthy();
  second.unmount();

  render(() => (
    <MacroComputers
      harnesses={[{ ...harness, claude_mode: 'macro_credits' }]}
    />
  ));
  expect(
    screen.queryByRole('button', { name: 'Sign in with Claude' })
  ).toBeNull();
  expect(screen.queryByRole('button', { name: 'Finish setup' })).toBeNull();
  expect(screen.getByText(/Claude: aesv credits/)).toBeTruthy();
});

it("reopens a computer's setup at Bring your AI when a notification asks", async () => {
  mocks.computers = [
    {
      id: 'computer-1',
      name: 'Build machine',
      size: 'standard',
      vcpus: 4,
      memoryGib: 8,
      diskGib: 100,
      catalogEntry: 'standard@2026-11',
      status: 'ready',
      billing: 'active',
      poweredOn: true,
      agentCount: 1,
      canUndoCancel: false,
      canRestore: false,
      purchasedBy: 'viewer',
      createdAt: '2026-01-01T00:00:00Z',
      harnessId: 'harness-build',
      setupCompletedAt: null,
    },
  ];
  render(() => <Machines />);
  expect(screen.queryByTestId('setup-step')).toBeNull();

  requestComputerSetup('computer-1');

  expect((await screen.findByTestId('setup-step')).textContent).toBe(
    'computer-1:claude'
  );
  expect(computerSetupRequest()).toBeUndefined();
});
