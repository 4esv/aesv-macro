import { useFeatureFlag } from '@app/lib/analytics/posthog';
import { LoadingBlock } from '@core/component/LoadingBlock';
import { enableMicrosoftSignup } from '@core/constant/featureFlags';
import { virtualKeyboardVisible } from '@core/mobile/virtualKeyboard';
import { getNativeMobilePlatform } from '@core/util/platform';
import { getWebOrigin } from '@core/util/webOrigin';
import { useNavigate, useSearchParams } from '@solidjs/router';
import { onMount } from 'solid-js';
import { AuthProvider, type AuthUser } from './context/auth-context';
import { sessionTokenParam } from './core/email-code';
import {
  AGENT_SETUP_PATH,
  postAuthTarget,
  readRedirectParam,
} from './core/post-auth-target';
import { ssoFallbackNotice } from './core/sso-fallback';
import { createAppAuthContext } from './create-app-auth-context';
import { AuthView } from './views/auth-view';
import { MobileWebSignupView } from './views/mobile-web-signup-view';

/** /login and /signup. Once authenticated, everyone proceeds into the app. */
export function Login(props: { signupMode?: boolean }) {
  const [params] = useSearchParams();
  const email = typeof params.email === 'string' ? params.email : undefined;
  const microsoftSignup = useFeatureFlag(enableMicrosoftSignup);
  return (
    <AuthProvider value={createAppAuthContext()}>
      <AuthView
        intent={props.signupMode ? 'signup' : 'login'}
        showApple={getNativeMobilePlatform() === 'ios'}
        showMicrosoft={microsoftSignup().enabled}
        compact={virtualKeyboardVisible()}
        initialEmail={email}
        // Dev persona links (`?email=`) sign straight in against a local backend.
        autoStart={import.meta.env.DEV && email !== undefined}
        token={sessionTokenParam(params.token)}
        ssoFallbackNotice={ssoFallbackNotice(
          typeof params.sso_error === 'string' ? params.sso_error : undefined
        )}
        onVerified={() => {
          const referral = new URLSearchParams(window.location.search).get(
            'referral'
          );
          if (referral) window.location.href = `/app?referral=${referral}`;
        }}
        signedIn={(user) => (
          <PostLoginRedirect
            target={postAuthTarget({
              tutorialComplete: user().tutorialComplete,
              isAgent: user().isAgent,
              redirect: readRedirectParam(params.redirect),
            })}
          />
        )}
      />
    </AuthProvider>
  );
}

/**
 * /signup/agent: Google or an emailed code, without the opening slides. An
 * agent invite links here with `team_invite`, which an existing account still
 * has to accept; a new agent joins the team during setup.
 */
export function AgentSignup() {
  const [params] = useSearchParams();
  const teamInvite = () =>
    typeof params.team_invite === 'string' ? params.team_invite : undefined;
  return (
    <AuthProvider value={createAppAuthContext()}>
      <AuthView
        intent="agent-signup"
        showApple={false}
        compact={virtualKeyboardVisible()}
        token={sessionTokenParam(params.token)}
        signedIn={(user) => (
          <PostLoginRedirect
            target={agentSignupDestination(user(), teamInvite())}
          />
        )}
      />
    </AuthProvider>
  );
}

function agentSignupDestination(user: AuthUser, teamInvite?: string) {
  if (needsAgentSetup(user)) return AGENT_SETUP_PATH;
  if (teamInvite) return `/team-invite?id=${encodeURIComponent(teamInvite)}`;
  return '/';
}

function needsAgentSetup(user: AuthUser) {
  return user.isAgent && !user.tutorialComplete;
}

/** Login init runs in the per-method handlers; this only navigates. */
function PostLoginRedirect(props: { target: string }) {
  const navigate = useNavigate();
  onMount(() => navigate(props.target, { replace: true }));
  return <LoadingBlock />;
}

/** Mobile-web visitors get a desktop link instead of signing up on a phone. */
export function MobileWebSignup() {
  const navigate = useNavigate();
  return (
    <AuthProvider value={createAppAuthContext()}>
      <MobileWebSignupView
        onLogin={() => navigate('/login')}
        onBackHome={() => {
          window.location.href = getWebOrigin();
        }}
      />
    </AuthProvider>
  );
}
