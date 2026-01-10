import type { HostCapabilities, RunnerConfig, Scenario, ScenarioRequirements } from '../scenarios/types.ts';

function checkDiskImage(req: ScenarioRequirements, config: RunnerConfig): string | undefined {
  if ((req.diskImage ?? 'none') !== 'required') return undefined;
  if (config.diskImage) return undefined;
  return 'missing disk image (provide --disk-image or set AERO_DISK_IMAGE_PATH)';
}

function checkHost(req: ScenarioRequirements, host: HostCapabilities): string | undefined {
  if (req.crossOriginIsolated && !host.crossOriginIsolated) {
    return 'requires crossOriginIsolated';
  }
  if (req.webgpu && !host.webgpu) {
    return 'requires WebGPU';
  }
  if (req.opfs && !host.opfs) {
    return 'requires OPFS';
  }
  return undefined;
}

export function checkScenarioRequirements(
  scenario: Scenario,
  config: RunnerConfig,
  host: HostCapabilities,
): string | undefined {
  const req = scenario.requirements;
  if (!req) return undefined;
  return checkDiskImage(req, config) ?? checkHost(req, host);
}

