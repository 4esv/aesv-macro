import { fireAndForget } from '@core/util/fireAndForget';
import { Stepper } from '@ui/components/Stepper';
import { type Accessor, type JSX, onMount, Show } from 'solid-js';
import { EmailForm } from '../components/email-form';
import { LoginCard } from '../components/login-card';
import { LoginPicker } from '../components/login-picker';
import { VerifyForm } from '../components/verify-form';
import {
  type AuthIntent,
  type AuthUser,
  useAuthContext,
} from '../context/auth-context';
import { createEmailLogin, type EmailLogin } from '../primitives/email-login';
import { redeemSessionToken } from '../primitives/session';

/** Sign-in and sign-up: SSO or an emailed code, then whatever comes next. */
export function AuthView(props: {
  intent: AuthIntent;
  showApple: boolean;
  /** Continue with Microsoft, behind `enable-microsoft-signup`. */
  showMicrosoft?: boolean;
  /** The header yields to the on-screen keyboard on phones. */
  compact: boolean;
  initialEmail?: string;
  autoStart?: boolean;
  /** Why a provider sign-in just failed; the email step opens instead. */
  ssoFallbackNotice?: string;
  /** The session code an SSO return put on the URL. */
  token?: string;
  onVerified?: () => void;
  /** What a signed-in visitor sees instead. */
  signedIn: (user: Accessor<AuthUser>) => JSX.Element;
}) {
  const context = useAuthContext();
  const login = createEmailLogin(context, {
    intent: props.intent,
    initialEmail: props.initialEmail,
    autoStart: props.autoStart,
    onVerified: props.onVerified,
    fallbackNotice: props.ssoFallbackNotice,
  });
  redeemSessionToken(context, () => props.token);
  onMount(() => context.pageView(props.intent));

  const user = () => {
    const session = context.session();
    return session.t === 'signed-in' ? session.user : undefined;
  };

  const signedOut = () => (
    <LoginCard
      compact={props.compact}
      agentSignup={props.intent === 'agent-signup'}
    >
      <Stepper
        step={STEP_INDEX[login.step()]}
        transition={Stepper.transitions.scale}
      >
        <Stepper.Step>
          <LoginPicker
            showApple={props.showApple}
            showMicrosoft={props.showMicrosoft === true}
            onGoogle={() =>
              fireAndForget(context.startSso('google', props.intent), 'auth')
            }
            onMicrosoft={() =>
              fireAndForget(context.startSso('microsoft', props.intent), 'auth')
            }
            onApple={() =>
              fireAndForget(context.startSso('apple', props.intent), 'auth')
            }
            onEmail={login.chooseEmail}
          />
        </Stepper.Step>
        <Stepper.Step>
          <EmailStep login={login} />
        </Stepper.Step>
        <Stepper.Step>
          <VerifyStep login={login} />
        </Stepper.Step>
      </Stepper>
    </LoginCard>
  );

  return (
    <Show when={context.session().t !== 'loading'}>
      <Show when={user()} fallback={signedOut()}>
        {(signedIn) => (
          // Refreshes update the existing workspace; only changing accounts
          // should remount it and restart its pending work.
          <Show when={signedIn().id} keyed>
            {(_id) => props.signedIn(signedIn)}
          </Show>
        )}
      </Show>
    </Show>
  );
}

const STEP_INDEX = { choose: 0, email: 1, verify: 2 } as const;

function EmailStep(props: { login: EmailLogin }) {
  return (
    <EmailForm
      email={props.login.email()}
      password={props.login.password()}
      passwordRequired={props.login.passwordRequired()}
      sending={props.login.sending()}
      error={props.login.sendError()}
      onEmailInput={props.login.setEmail}
      onPasswordInput={props.login.setPassword}
      onSubmit={props.login.submitEmail}
      onBack={props.login.back}
    />
  );
}

function VerifyStep(props: { login: EmailLogin }) {
  return (
    <VerifyForm
      email={props.login.email()}
      code={props.login.code()}
      verifying={props.login.verifying()}
      canVerify={props.login.canVerify()}
      resendIn={props.login.resendIn()}
      resending={props.login.sending()}
      error={props.login.verifyError() ?? props.login.sendError()}
      onCodeInput={props.login.setCode}
      onVerify={props.login.verify}
      onResend={() => fireAndForget(props.login.resend(), 'auth')}
      onBack={props.login.back}
    />
  );
}
