import type { AppEventNames, AppEvents } from '@app/lib/analytics/app-events';

type EventName = AppEventNames | (string & {});

type TrackFn = <E extends EventName>(
  event: E,
  data?: E extends keyof AppEvents ? AppEvents[E] : Record<string, unknown>
) => void;

interface PageViewOptions {
  path?: string;
  location?: string;
}

export type AnalyticsInterface = {
  track: TrackFn;
  pageView: (pageTitle: string, opts?: PageViewOptions) => void;
};

// NOTE(shortcut): this build sends no product analytics, so `track` and
// `pageView` drop every event; delete a call site when you next touch its file.
export const analytics: AnalyticsInterface = {
  track: () => {},
  pageView: () => {},
};
