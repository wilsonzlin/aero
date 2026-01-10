import type { AeroGlobalApi } from "../shared/aero_api.ts";

export {};

declare global {
  interface Window {
    aero?: AeroGlobalApi;
  }
}
