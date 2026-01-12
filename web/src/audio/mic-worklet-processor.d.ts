export class AeroMicCaptureProcessor {
  constructor(options?: {
    processorOptions?: {
      ringBuffer?: SharedArrayBuffer;
    };
  });

  readonly port: {
    postMessage(message: unknown): void;
    onmessage?: ((event: MessageEvent) => void) | null;
  };

  process(
    inputs: Float32Array[][],
    outputs: Float32Array[][],
    _parameters?: Record<string, Float32Array>,
  ): boolean;
}

declare const url: string;
export default url;
