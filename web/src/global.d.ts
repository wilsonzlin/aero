export {};

declare global {
  interface Window {
    aero?: {
      perf?: {
        export: () => unknown;
        getStats?: () => unknown;
        setEnabled?: (enabled: boolean) => void;
      };
    };
  }
}

