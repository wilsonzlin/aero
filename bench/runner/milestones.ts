import { createHash } from 'node:crypto';
import { setTimeout as sleep } from 'node:timers/promises';

import type { AeroPhase } from '../../shared/aero_status.ts';
import type { ArtifactWriter, EmulatorDriver, MilestoneClient } from '../scenarios/types.ts';

function sanitizePathFragment(fragment: string): string {
  return fragment.replaceAll(/[^a-zA-Z0-9_.-]+/g, '_');
}

function sha256Hex(payload: Uint8Array): string {
  return createHash('sha256').update(payload).digest('hex');
}

export class DefaultMilestoneClient implements MilestoneClient {
  readonly #emulator: EmulatorDriver;
  readonly #artifacts: ArtifactWriter;

  constructor(emulator: EmulatorDriver, artifacts: ArtifactWriter) {
    this.#emulator = emulator;
    this.#artifacts = artifacts;
  }

  async waitForPhase(phase: AeroPhase, options?: { timeoutMs?: number }): Promise<void> {
    const timeoutMs = options?.timeoutMs ?? 60_000;
    const deadlineMs = performance.now() + timeoutMs;

    while (performance.now() < deadlineMs) {
      const current = await this.#emulator.eval<AeroPhase | undefined>('globalThis.aero?.status?.phase');
      if (current === phase) return;
      await sleep(100);
    }

    throw new Error(`Timed out waiting for aero phase ${JSON.stringify(phase)}`);
  }

  async waitForEvent(name: string, options?: { timeoutMs?: number }): Promise<void> {
    if (!this.#emulator.capabilities.statusApi) {
      throw new Error(`Emulator does not expose window.aero status/events; cannot wait for ${name}`);
    }

    const args = options?.timeoutMs === undefined ? 'undefined' : JSON.stringify({ timeoutMs: options.timeoutMs });
    await this.#emulator.eval(`globalThis.aero.waitForEvent(${JSON.stringify(name)}, ${args})`);
  }

  async waitForStableScreen(options?: {
    timeoutMs?: number;
    intervalMs?: number;
    stableCount?: number;
  }): Promise<void> {
    const timeoutMs = options?.timeoutMs ?? 60_000;
    const intervalMs = options?.intervalMs ?? 500;
    const stableCount = options?.stableCount ?? 3;
    const deadlineMs = performance.now() + timeoutMs;

    let lastHash: string | undefined;
    let stableSoFar = 0;

    while (performance.now() < deadlineMs) {
      const png = await this.#emulator.screenshotPng();
      const hash = sha256Hex(png);

      if (hash === lastHash) {
        stableSoFar += 1;
      } else {
        lastHash = hash;
        stableSoFar = 1;
      }

      if (stableSoFar >= stableCount) return;
      await sleep(intervalMs);
    }

    throw new Error(`Timed out waiting for stable screen after ${timeoutMs}ms`);
  }

  async captureScreenshot(name: string): Promise<void> {
    if (!this.#emulator.capabilities.screenshots) return;

    const payload = await this.#emulator.screenshotPng();
    const filename = `${sanitizePathFragment(name)}.png`;
    await this.#artifacts.writeBinary(`screenshots/${filename}`, payload, 'screenshot');
  }
}

