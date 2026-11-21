import { expect, test } from '@playwright/test';
import {
  expectLanded,
  fakeAuth,
  fakeCalls,
  openFixture,
  signInWithCode,
} from './fixture-page';

test('a returning user signs in with an emailed code', async ({ page }) => {
  await openFixture(page, '/?page=login');
  await page.getByRole('button', { name: 'Continue with email' }).click();
  await signInWithCode(page, 'returning@acme.com', '424242');
  await expectLanded(page, '/');
});

test('email verification retries cleanup without reusing a consumed code', async ({
  page,
}) => {
  await openFixture(page, '/?page=login&cleanupFailure=once');
  await page.getByRole('button', { name: 'Continue with email' }).click();
  await signInWithCode(page, 'returning@example.com', '424242');
  await expect(
    page.getByRole('status', { name: 'Notifications' })
  ).toContainText('Local cleanup failed');
  const verify = page.getByRole('button', { name: 'Verify', exact: true });
  await expect(verify).toBeEnabled();
  await expect(page.getByLabel('One-time code')).toHaveValue('424242');
  await verify.click();
  await expect(
    page.getByRole('heading', { level: 1, name: 'Create your workspace' })
  ).toBeVisible();
  expect(
    (await fakeCalls(page)).filter((call) => call.startsWith('verify:'))
  ).toHaveLength(1);
});

test('changing the email after cleanup failure starts fresh verification', async ({
  page,
}) => {
  await openFixture(page, '/?page=login&cleanupFailure=once');
  await page.getByRole('button', { name: 'Continue with email' }).click();
  await signInWithCode(page, 'first@example.com', '424242');
  await expect(
    page.getByRole('status', { name: 'Notifications' })
  ).toContainText('Local cleanup failed');
  await page.getByRole('button', { name: 'Change email', exact: true }).click();
  await signInWithCode(page, 'second@example.com', '424242');
  await expect(
    page.getByRole('heading', { level: 1, name: 'Create your workspace' })
  ).toBeVisible();
  expect(
    (await fakeCalls(page)).filter((call) => call.startsWith('verify:'))
  ).toEqual([
    'verify:first@example.com:424242',
    'verify:second@example.com:424242',
  ]);
  expect((await fakeAuth(page)).user?.email).toBe('second@example.com');
});

test('a wrong code can be corrected without starting over', async ({
  page,
}) => {
  await openFixture(page, '/?page=login&verifyLatency=300');
  await page.getByRole('button', { name: 'Continue with email' }).click();
  await signInWithCode(page, 'returning@acme.com', '111111');
  await expect(page.getByRole('alert')).toHaveText('Invalid code.');
  const code = page.getByLabel('One-time code');
  await expect(code).toHaveValue('');
  await expect(code).toBeFocused();
  await expect(page.getByRole('button', { name: 'Verify' })).toBeDisabled();
  await page.keyboard.press('Enter');
  expect(
    (await fakeCalls(page)).filter((call) => call.startsWith('verify:'))
  ).toEqual(['verify:returning@acme.com:111111']);
  await page.keyboard.type('424242');
  await expectLanded(page, '/');
});

test('a rate-limited code check says to wait instead of calling the code invalid', async ({
  page,
}) => {
  await openFixture(page, '/?page=login', {
    codeAttempts: { 'returning@acme.com': 5 },
  });
  await page.getByRole('button', { name: 'Continue with email' }).click();
  await signInWithCode(page, 'returning@acme.com', '424242');
  await expect(page.getByRole('alert')).toHaveText(
    'Too many attempts. Wait a minute, then try again.'
  );
  await expect(page.getByLabel('One-time code')).toHaveValue('424242');
  await expect(page.getByRole('button', { name: 'Verify' })).toBeEnabled();
});

test('Google sign-in comes back with a session code the page redeems', async ({
  page,
}) => {
  await openFixture(page, '/?page=login', { ssoEmail: 'returning@acme.com' });
  await page.getByRole('button', { name: 'Continue with Google' }).click();
  await expectLanded(page, '/');
  const world = await fakeAuth(page);
  expect(Object.keys(world.sessionTokens)).toEqual([
    expect.stringMatching(/^google-/),
  ]);
  expect(world.user?.email).toBe('returning@acme.com');
});

test('an expired session code leaves the visitor on sign-in', async ({
  page,
}) => {
  await openFixture(page, '/?page=login&token=expired');
  await expect(page.getByRole('status', { name: 'Notifications' })).toHaveText(
    'Sign-in failed. Please try again.'
  );
  await expect(
    page.getByRole('button', { name: 'Continue with Google' })
  ).toBeVisible();
});

test('a password account signs in with its password', async ({ page }) => {
  await openFixture(page, '/?page=login', {
    passwordAccounts: { 'demo@acme.com': 'hunter2' },
    accounts: { 'demo@acme.com': { tutorialComplete: true } },
  });
  await page.getByRole('button', { name: 'Continue with email' }).click();
  await page.getByPlaceholder('you@company.com').fill('demo@acme.com');
  await page.getByRole('button', { name: 'Continue', exact: true }).click();
  await page.getByPlaceholder('Password').fill('hunter2');
  await page.getByRole('button', { name: 'Continue', exact: true }).click();
  await expectLanded(page, '/');
});

test('a new user who signs in by email lands on Home', async ({ page }) => {
  await openFixture(page, '/?page=login');
  await page.getByRole('button', { name: 'Continue with email' }).click();
  await signInWithCode(page, 'fresh@acme.com', '424242');
  await expectLanded(page, '/');
});

test('desktop sign-up with Google lands a new user on Home', async ({
  page,
}) => {
  await openFixture(page, '/?page=signup', { ssoEmail: 'founder@acme.com' });
  await page.getByRole('button', { name: 'Continue with Google' }).click();
  await expectLanded(page, '/');
  expect((await fakeAuth(page)).user).toMatchObject({
    email: 'founder@acme.com',
  });
});

test('a mobile-web visitor gets a desktop link instead of signing up', async ({
  page,
}) => {
  await openFixture(page, '/?page=mobile-signup');
  await page.getByLabel('Email address').fill('visitor@acme.com');
  await page.getByRole('button', { name: 'Sign Up' }).click();
  await expect(
    page.getByRole('heading', { name: 'aesv is better on desktop.' })
  ).toBeVisible();
  expect(await fakeCalls(page)).toContain('lead:visitor@acme.com');
});

test('a failed provider sign-up falls back to the email form with the reason', async ({
  page,
}) => {
  await openFixture(page, '/?page=signup&sso_error=admin_consent_required', {
    ssoEmail: 'founder@example.com',
  });
  await expect(
    page.getByText(
      'Your organization needs an admin to approve aesv. Continue with your email instead.'
    )
  ).toBeVisible();
  await expect(page.getByPlaceholder(/you@company\.com/)).toBeVisible();
  await expect(
    page.getByRole('button', { name: 'Continue with Microsoft' })
  ).toHaveCount(0);
});

test('with Microsoft sign-up off, sign-up offers no Microsoft button', async ({
  page,
}) => {
  await openFixture(page, '/?page=signup&microsoft=off', {
    ssoEmail: 'founder@example.com',
  });
  await expect(
    page.getByRole('button', { name: 'Continue with Google' })
  ).toBeVisible();
  await expect(
    page.getByRole('button', { name: 'Continue with Microsoft' })
  ).toHaveCount(0);
});
