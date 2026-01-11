export {};

declare global {
  // `globalThis.aero` is used as a long-lived namespace for debug/automation APIs.
  // Keep it `any`-typed so UI + worker code can attach helpers without fighting TS.
  // eslint-disable-next-line no-var
  var aero: any;

  // Most browser callers access this namespace via `window.aero`.
  interface Window {
    aero?: any;
  }
}
