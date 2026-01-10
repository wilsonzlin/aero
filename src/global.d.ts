export {};

declare global {
  // `globalThis.aero` is used as a long-lived namespace for debug/automation APIs.
  // Keep it `any`-typed so UI + worker code can attach helpers without fighting TS.
  // eslint-disable-next-line no-var
  var aero: any;
}

