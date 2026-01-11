import {
  createMicRingBuffer,
  DROPPED_SAMPLES_INDEX,
  micRingBufferWrite,
  READ_POS_INDEX,
  type MicRingBuffer,
  WRITE_POS_INDEX,
} from "./mic_ring.js";

export { createMicRingBuffer, micRingBufferReadInto, type MicRingBuffer } from "./mic_ring.js";

export type MicCaptureState =
  | "inactive"
  | "starting"
  | "active"
  | "muted"
  | "denied"
  | "error";

export interface MicCaptureOptions {
  /** Desired sample rate for the capture graph. Browser may ignore. */
  sampleRate?: number;
  /** Ring buffer capacity in milliseconds. */
  bufferMs?: number;
  /** Prefer AudioWorklet (low latency). Falls back to ScriptProcessorNode. */
  preferWorklet?: boolean;

  deviceId?: string;
  echoCancellation?: boolean;
  noiseSuppression?: boolean;
  autoGainControl?: boolean;
}

type ResolvedMicCaptureOptions = {
  sampleRate: number;
  bufferMs: number;
  preferWorklet: boolean;
  deviceId?: string;
  echoCancellation: boolean;
  noiseSuppression: boolean;
  autoGainControl: boolean;
};

function isAudioWorkletSupported(): boolean {
  return typeof AudioWorkletNode !== "undefined";
}

export class MicCapture extends EventTarget {
  readonly options: ResolvedMicCaptureOptions;
  ringBuffer: MicRingBuffer;
  /** Actual sample rate of the capture graph (AudioContext.sampleRate). */
  actualSampleRate: number;

  state: MicCaptureState = "inactive";

  private audioContext: AudioContext | null = null;
  private stream: MediaStream | null = null;
  private sourceNode: MediaStreamAudioSourceNode | null = null;
  private workletNode: AudioWorkletNode | null = null;
  private scriptNode: ScriptProcessorNode | null = null;
  private muteRequested = false;

  private permissionStatus: PermissionStatus | null = null;
  private deviceChangeListener: (() => void) | null = null;

  constructor(opts: MicCaptureOptions = {}) {
    super();
    this.options = {
      sampleRate: opts.sampleRate ?? 48000,
      bufferMs: opts.bufferMs ?? 100,
      preferWorklet: opts.preferWorklet ?? true,
      deviceId: opts.deviceId,
      echoCancellation: opts.echoCancellation ?? true,
      noiseSuppression: opts.noiseSuppression ?? true,
      autoGainControl: opts.autoGainControl ?? true,
    };

    // The browser may ignore our requested sample rate when constructing the
    // AudioContext. We'll resize the ring buffer during `start()` once we know
    // the actual capture rate.
    this.actualSampleRate = this.options.sampleRate;
    const capacitySamples = Math.max(1, Math.floor((this.actualSampleRate * this.options.bufferMs) / 1000));
    this.ringBuffer = createMicRingBuffer(capacitySamples);
  }

  async start(): Promise<void> {
    if (this.state === "starting" || this.state === "active" || this.state === "muted") return;

    this.setState("starting");

    try {
      await this.attachPermissionListener();

      const constraints: MediaStreamConstraints = {
        audio: {
          deviceId: this.options.deviceId ? { exact: this.options.deviceId } : undefined,
          channelCount: 1,
          echoCancellation: this.options.echoCancellation,
          noiseSuppression: this.options.noiseSuppression,
          autoGainControl: this.options.autoGainControl,
        },
        video: false,
      };

      // Permission must be requested from an explicit user action, which is the
      // responsibility of the caller (they call start() on click).
      const stream = await navigator.mediaDevices.getUserMedia(constraints);
      this.stream = stream;

      const audioContext = new AudioContext({
        sampleRate: this.options.sampleRate,
        latencyHint: "interactive",
      });
      this.audioContext = audioContext;
      this.actualSampleRate = audioContext.sampleRate;

      // Ensure the ring buffer duration matches the actual capture rate.
      const capacitySamples = Math.max(1, Math.floor((this.actualSampleRate * this.options.bufferMs) / 1000));
      if (this.ringBuffer.capacity !== capacitySamples) {
        this.ringBuffer = createMicRingBuffer(capacitySamples);
      }

      // Clear indices/counters from any previous run.
      Atomics.store(this.ringBuffer.header, WRITE_POS_INDEX, 0);
      Atomics.store(this.ringBuffer.header, READ_POS_INDEX, 0);
      Atomics.store(this.ringBuffer.header, DROPPED_SAMPLES_INDEX, 0);

      // Ensure the graph runs (required by some browsers) and that we never
      // leak mic audio to speakers.
      const sinkGain = audioContext.createGain();
      sinkGain.gain.value = 0;
      sinkGain.connect(audioContext.destination);

      const source = audioContext.createMediaStreamSource(stream);
      this.sourceNode = source;

      const useWorklet = this.options.preferWorklet && isAudioWorkletSupported();
      if (useWorklet) {
        await audioContext.audioWorklet.addModule(
          new URL("./mic-worklet-processor.js", import.meta.url).toString(),
        );
        const node = new AudioWorkletNode(audioContext, "aero-mic-capture", {
          numberOfInputs: 1,
          numberOfOutputs: 1,
          outputChannelCount: [1],
          processorOptions: { ringBuffer: this.ringBuffer.sab },
        });
        node.port.onmessage = (ev) => {
          this.dispatchEvent(new MessageEvent("message", { data: ev.data }));
        };
        source.connect(node);
        node.connect(sinkGain);
        this.workletNode = node;
      } else {
        const node = audioContext.createScriptProcessor(2048, 1, 1);
        node.onaudioprocess = (ev) => {
          if (this.muteRequested) return;
          const input = ev.inputBuffer.getChannelData(0);
          micRingBufferWrite(this.ringBuffer, input);
        };
        source.connect(node);
        node.connect(sinkGain);
        this.scriptNode = node;
      }

      // Track end covers device removal, permission revocation, etc.
      stream.getAudioTracks()[0]?.addEventListener("ended", () => {
        // When this happens mid-session, surface a clear state to the UI.
        // Consumers can call start() again to re-request permission.
        void this.stop();
      });

      // Monitor device changes so the UI can prompt the user to reselect.
      this.deviceChangeListener = () => {
        this.dispatchEvent(new Event("devicechange"));
      };
      navigator.mediaDevices.addEventListener("devicechange", this.deviceChangeListener);

      await audioContext.resume();

      this.setState(this.muteRequested ? "muted" : "active");
    } catch (err: any) {
      const name = err?.name ?? "";
      if (name === "NotAllowedError" || name === "SecurityError") {
        this.setState("denied");
      } else {
        this.setState("error");
      }
      await this.stop();
      throw err;
    }
  }

  async stop(): Promise<void> {
    if (this.deviceChangeListener) {
      navigator.mediaDevices.removeEventListener("devicechange", this.deviceChangeListener);
      this.deviceChangeListener = null;
    }

    if (this.permissionStatus) {
      this.permissionStatus.onchange = null;
      this.permissionStatus = null;
    }

    this.workletNode?.disconnect();
    this.workletNode = null;
    this.scriptNode?.disconnect();
    this.scriptNode = null;
    this.sourceNode?.disconnect();
    this.sourceNode = null;

    this.stream?.getTracks().forEach((t) => t.stop());
    this.stream = null;

    if (this.audioContext) {
      try {
        await this.audioContext.close();
      } catch {
        // ignore
      }
      this.audioContext = null;
    }

    if (this.state !== "denied" && this.state !== "error") {
      this.setState("inactive");
    }
  }

  setMuted(muted: boolean): void {
    this.muteRequested = muted;
    if (this.workletNode) {
      this.workletNode.port.postMessage({ type: "set_muted", muted });
    }
    if (this.state === "active" || this.state === "muted") {
      this.setState(muted ? "muted" : "active");
    }
  }

  private async attachPermissionListener(): Promise<void> {
    // Permissions API is not available in all browsers. Best-effort only.
    const navAny: any = navigator;
    if (!navAny.permissions?.query) return;
    try {
      const status = (await navAny.permissions.query({ name: "microphone" })) as PermissionStatus;
      this.permissionStatus = status;
      status.onchange = () => {
        if (status.state === "denied") {
          this.setState("denied");
          void this.stop();
        }
      };
    } catch {
      // ignore
    }
  }

  private setState(state: MicCaptureState): void {
    if (this.state === state) return;
    this.state = state;
    this.dispatchEvent(new Event("statechange"));
  }
}
