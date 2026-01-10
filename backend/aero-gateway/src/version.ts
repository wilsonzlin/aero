export type VersionInfo = Readonly<{
  version: string;
}>;

export function getVersionInfo(): VersionInfo {
  const version = process.env.AERO_GATEWAY_VERSION || process.env.GIT_SHA || 'dev';
  return { version };
}

