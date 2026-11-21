import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@solidjs/testing-library';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Login } from './auth';
import type { createFakeAuth } from './tests/fake-auth-context';

const world = vi.hoisted(() => ({
  microsoftSignup: false,
  auth: undefined as ReturnType<typeof createFakeAuth> | undefined,
}));

vi.mock('@app/lib/analytics/flags', () => ({
  useFeatureFlag: (flag: { key: string }) => () => ({
    enabled: flag.key === 'enable-microsoft-signup' && world.microsoftSignup,
  }),
}));

// Provider sign-in leaves the page; the fake never comes back signed in.
vi.mock('./create-app-auth-context', async () => {
  const { createFakeAuth } = await import('./tests/fake-auth-context');
  return {
    createAppAuthContext: () => {
      world.auth = createFakeAuth(
        {},
        { ssoRedirect: () => new Promise(() => {}) }
      );
      return world.auth.context;
    },
  };
});

vi.mock('@solidjs/router', async (original) => ({
  ...(await original<typeof import('@solidjs/router')>()),
  useSearchParams: () => [{}, vi.fn()],
  useNavigate: () => vi.fn(),
}));

beforeEach(() => {
  sessionStorage.clear();
  localStorage.clear();
  world.microsoftSignup = false;
  world.auth = undefined;
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const microsoftButton = () =>
  screen.queryByRole('button', { name: 'Continue with Microsoft' });

describe('Continue with Microsoft, behind enable-microsoft-signup', () => {
  it('is not offered on sign-in while the flag is off', async () => {
    render(() => <Login />);
    await screen.findByRole('button', { name: 'Continue with Google' });
    expect(microsoftButton()).toBeNull();
  });

  it('signs in through Microsoft once the flag is on', async () => {
    world.microsoftSignup = true;
    render(() => <Login />);
    await screen.findByRole('button', { name: 'Continue with Google' });
    const microsoft = microsoftButton();
    expect(microsoft).toBeTruthy();
    if (microsoft) fireEvent.click(microsoft);
    await waitFor(() =>
      expect(world.auth?.calls()).toContain('sso:microsoft:login')
    );
  });

  it('keeps sign-up Google-only while the flag is off', async () => {
    render(() => <Login signupMode />);
    await screen.findByRole('button', { name: 'Continue with Google' });
    expect(microsoftButton()).toBeNull();
  });

  it('offers Microsoft sign-up once the flag is on', async () => {
    world.microsoftSignup = true;
    render(() => <Login signupMode />);
    await screen.findByRole('button', { name: 'Continue with Google' });
    const microsoft = microsoftButton();
    expect(microsoft).toBeTruthy();
    if (microsoft) fireEvent.click(microsoft);
    await waitFor(() =>
      expect(world.auth?.calls()).toContain('sso:microsoft:signup')
    );
  });
});
