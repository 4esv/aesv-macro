import { DEFAULT_ROUTE } from '@app/constants/defaultRoute';

/**
 * Same-app relative paths only. A `next` at the default route is not a real
 * deep link, so it falls through to the default landing.
 */
export function sanitizeNext(value: unknown): string | undefined {
  return typeof value === 'string' &&
    value.startsWith('/') &&
    !value.startsWith('//') &&
    !value.includes('\\') &&
    !value.startsWith(DEFAULT_ROUTE)
    ? value
    : undefined;
}

export const AGENT_SETUP_PATH = '/agent-setup';

/**
 * Resolves the `/login?redirect=` deep link after sign-in. Pages that need an
 * account (team and channel invites, meetings) send people here with the page
 * to come back to; without one, everyone lands on Home.
 */
export function postAuthTarget(input: {
  tutorialComplete: boolean;
  isAgent: boolean;
  redirect: unknown;
}): string {
  if (input.isAgent && !input.tutorialComplete) return AGENT_SETUP_PATH;
  return sanitizeNext(input.redirect) ?? '/';
}

/**
 * The desktop sign-in callback, carrying the page's `redirect`. Desktop SSO
 * returns through a pinned `macro:///login` deep link rather than the page
 * sign-in started on, so the deep link would otherwise be lost; the auth
 * service keeps the query and appends the session code.
 *
 * The auth service URL-decodes `original_url` once more after reading it from
 * the query (`UrlEncoded<Url>`), so the redirect is encoded twice here: a
 * redirect with its own `&` would otherwise split into separate parameters.
 * {@link readRedirectParam} accepts it whether or not that extra decode runs.
 */
export function desktopAuthCallbackUrl(
  callbackUrl: string,
  redirect: unknown
): string {
  const next = sanitizeNext(redirect);
  if (!next) return callbackUrl;
  return `${callbackUrl}?redirect=${encodeURIComponent(encodeURIComponent(next))}`;
}

/**
 * The login page's `redirect` parameter, decoded once more when it arrives
 * still encoded (a desktop callback the auth service did not decode twice).
 */
export function readRedirectParam(value: unknown): unknown {
  if (typeof value !== 'string' || !value.startsWith('%2F')) return value;
  try {
    return decodeURIComponent(value);
  } catch {
    return undefined;
  }
}
