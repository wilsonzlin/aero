export function isTruthyEnvValue(value: string | undefined): boolean {
  if (!value) return false;
  const normalized = value.trim().toLowerCase();
  return normalized === '1' || normalized === 'true' || normalized === 'yes' || normalized === 'on';
}

export function isWebGPURequired(): boolean {
  return isTruthyEnvValue(process.env.AERO_REQUIRE_WEBGPU);
}
