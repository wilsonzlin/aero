import { join } from 'node:path';

import type { DiskImageSource, RunnerConfig } from '../scenarios/types.ts';

export type CliCommand =
  | { kind: 'help'; error?: string }
  | { kind: 'list' }
  | { kind: 'run'; config: RunnerConfig };

export function parseDiskImageSource(raw: string): DiskImageSource {
  if (/^https?:\/\//.test(raw) || raw.startsWith('file://')) {
    return { kind: 'url', url: raw };
  }
  return { kind: 'path', path: raw };
}

function defaultOutDir(now: Date, scenarioId: string): string {
  const ts = now.toISOString().replaceAll(/[:.]/g, '-');
  // `bench/results/` is ignored by default, so local runs don't create noisy git status.
  return join('bench', 'results', `${ts}-${scenarioId}`);
}

export function formatBenchUsage(): string {
  return [
    'Usage:',
    '  node --experimental-strip-types --import ./scripts/register-ts-strip-loader.mjs bench/runner.ts --list',
    '  node --experimental-strip-types --import ./scripts/register-ts-strip-loader.mjs bench/runner.ts <scenarioId> [options]',
    '',
    'Options:',
    '  --disk-image <pathOrUrl>  Disk image path/URL (or env AERO_DISK_IMAGE_PATH)',
    '  --out-dir <dir>           Output directory (default: bench/results/<timestamp>-<scenarioId>)',
    '  --trace                   Save optional trace (if supported by emulator)',
    '  --list                    List available scenarios',
    '  --help                    Show this help',
    '',
    'Examples:',
    '  AERO_DISK_IMAGE_PATH=/path/to/win7.img node --experimental-strip-types --import ./scripts/register-ts-strip-loader.mjs bench/runner.ts system_boot',
    '  node --experimental-strip-types --import ./scripts/register-ts-strip-loader.mjs bench/runner.ts noop',
    '',
  ].join('\n');
}

export function parseBenchCli(
  argv: readonly string[],
  env: Record<string, string | undefined>,
  now: Date = new Date(),
): CliCommand {
  let scenarioId: string | undefined;
  let diskImageRaw: string | undefined;
  let outDirRaw: string | undefined;
  let trace = false;
  let list = false;

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (!arg) continue;

    if (arg === '--help' || arg === '-h') return { kind: 'help' };
    if (arg === '--list') {
      list = true;
      continue;
    }

    if (arg === '--disk-image') {
      const next = argv[i + 1];
      if (!next) return { kind: 'help', error: '--disk-image requires a value' };
      diskImageRaw = next;
      i += 1;
      continue;
    }

    if (arg === '--out-dir') {
      const next = argv[i + 1];
      if (!next) return { kind: 'help', error: '--out-dir requires a value' };
      outDirRaw = next;
      i += 1;
      continue;
    }

    if (arg === '--trace') {
      trace = true;
      continue;
    }

    if (arg.startsWith('-')) return { kind: 'help', error: `Unknown option: ${arg}` };

    if (!scenarioId) {
      scenarioId = arg;
      continue;
    }

    return { kind: 'help', error: `Unexpected argument: ${arg}` };
  }

  if (list) return { kind: 'list' };
  if (!scenarioId) return { kind: 'help', error: 'Missing scenarioId' };

  const diskImage = diskImageRaw ?? env.AERO_DISK_IMAGE_PATH;
  const outDir = outDirRaw ?? defaultOutDir(now, scenarioId);

  return {
    kind: 'run',
    config: {
      scenarioId,
      outDir,
      diskImage: diskImage ? parseDiskImageSource(diskImage) : undefined,
      trace,
    },
  };
}
