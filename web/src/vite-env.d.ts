/// <reference types="vite/client" />
/// <reference types="@webgpu/types" />
/// <reference types="wicg-file-system-access" />

declare global {
  interface Window {
    aero?: {
      perf?: unknown;
    };
  }
}

export {};
