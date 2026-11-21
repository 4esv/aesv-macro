import { NativeAppInfo } from './NativeAppInfo';

/** The tab is offered only inside the mobile app, where native APIs exist. */
export function MobileApp() {
  return (
    <NativeAppInfo
      title="Mobile app"
      description="Your installed version of aesv."
    />
  );
}
