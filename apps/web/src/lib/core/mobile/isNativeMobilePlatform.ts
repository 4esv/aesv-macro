import { getNativeMobilePlatform } from '@core/util/platform';

export function isNativeMobilePlatform() {
  return !!getNativeMobilePlatform();
}

/**
 * Whether this app keeps copies for offline use: the native app, or any
 * browser with a service worker, which opens the app with no network
 * (W-2096). That is every tab, not only an installed app: a note read in a
 * phone's browser tab opens offline too. Each copy is per signed-in user and
 * cleared at sign-out.
 */
export function keepsOfflineCopies() {
  return isNativeMobilePlatform() || 'serviceWorker' in navigator;
}
