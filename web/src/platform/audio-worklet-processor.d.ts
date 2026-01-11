export function addUnderrunFrames(header: Uint32Array, missingFrames: number): number;

export class AeroAudioProcessor {
  constructor(options?: {
    processorOptions?: {
      ringBuffer?: SharedArrayBuffer;
      channelCount?: number;
      capacityFrames?: number;
    };
  });

  readonly port: {
    postMessage(message: unknown): void;
  };

  process(_inputs: unknown[], outputs: Float32Array[][]): boolean;
}

declare const url: string;
export default url;
