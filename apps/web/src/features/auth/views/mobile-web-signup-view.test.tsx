import { cleanup, fireEvent, screen, waitFor } from '@solidjs/testing-library';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { FakeAuthWorld } from '../tests/fake-auth-context';
import { renderWithFakeAuth } from '../tests/render';
import { MobileWebSignupView } from './mobile-web-signup-view';

afterEach(cleanup);

function setup(world: Partial<FakeAuthWorld> = {}) {
  const login = vi.fn();
  const { fake } = renderWithFakeAuth(
    () => <MobileWebSignupView onLogin={login} onBackHome={() => {}} />,
    world
  );
  return { fake, login };
}

const submit = (email: string) => {
  fireEvent.input(screen.getByLabelText('Email address'), {
    target: { value: email },
  });
  fireEvent.click(screen.getByRole('button', { name: 'Sign Up' }));
};

describe('mobile-web sign-up', () => {
  it('emails a desktop link to the normalized email', async () => {
    const { fake } = setup();
    submit(' Visitor@Example.com ');
    await screen.findByText('aesv is better on desktop.');
    expect(fake.calls()).toContain('mobileWelcome:visitor@example.com');
  });

  it('stays on the capture step for an empty or rejected address', async () => {
    const { fake } = setup({ mobileWelcome: 'invalid-email' });
    submit('   ');
    expect(fake.calls()).not.toContain('mobileWelcome:');
    submit('typo@acme.con');
    await waitFor(() =>
      expect(fake.notifications()).toEqual(['Invalid email address.'])
    );
    expect(screen.getByRole('button', { name: 'Sign Up' })).toBeTruthy();
  });

  it('sends existing users to sign in', () => {
    const { fake, login } = setup();
    fireEvent.click(screen.getByRole('link', { name: 'Login' }));
    expect(login).toHaveBeenCalledOnce();
    expect(fake.events()).toContainEqual({
      event: 'login_from_onboarding',
      data: undefined,
    });
  });
});
