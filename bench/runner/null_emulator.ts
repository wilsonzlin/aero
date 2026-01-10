import type { DiskImageSource, EmulatorCapabilities, EmulatorDriver } from '../scenarios/types.ts';

export class NullEmulatorDriver implements EmulatorDriver {
  readonly capabilities: EmulatorCapabilities = {
    systemBoot: false,
    perfExport: false,
    screenshots: false,
    trace: false,
    statusApi: false,
  };

  async configure(_options: Record<string, unknown>): Promise<void> {
    throw new Error('NullEmulatorDriver does not support configure()');
  }

  async attachDiskImage(_source: DiskImageSource): Promise<void> {
    throw new Error('NullEmulatorDriver does not support attachDiskImage()');
  }

  async start(): Promise<void> {
    throw new Error('NullEmulatorDriver does not support start()');
  }

  async stop(): Promise<void> {
    throw new Error('NullEmulatorDriver does not support stop()');
  }

  async eval<T>(_expression: string): Promise<T> {
    throw new Error('NullEmulatorDriver does not support eval()');
  }

  async screenshotPng(): Promise<Uint8Array> {
    throw new Error('NullEmulatorDriver does not support screenshotPng()');
  }
}

