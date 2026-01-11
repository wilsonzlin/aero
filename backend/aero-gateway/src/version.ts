export type VersionInfo = Readonly<{
  version: string;
  gitSha: string;
  builtAt: string;
}>;

export function getVersionInfo(): VersionInfo {
  const version = process.env.AERO_GATEWAY_VERSION || 'dev';
  const gitSha = process.env.AERO_GATEWAY_GIT_SHA || process.env.GIT_SHA || version;
  const builtAt = process.env.AERO_GATEWAY_BUILD_TIMESTAMP || process.env.BUILD_TIMESTAMP || '';
  return { version, gitSha, builtAt };
}
