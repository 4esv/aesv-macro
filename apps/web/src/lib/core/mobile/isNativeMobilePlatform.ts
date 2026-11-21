import { getNativeMobilePlatform } from '@core/util/platform';

export function isNativeMobilePlatform() {
  return !!getNativeMobilePlatform();
}

/**
 * Whether this app keeps copies for offline use: the native app, or a web app
 * whose service worker opens it with no network (W-2096). Each copy is per
 * signed-in user and cleared at sign-out.
 */
export function keepsOfflineCopies() {
  return isNativeMobilePlatform() || 'serviceWorker' in navigator;
}
