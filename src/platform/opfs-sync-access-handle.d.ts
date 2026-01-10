export {};

declare global {
  /**
   * OPFS synchronous access handle.
   *
   * This type is not currently included in TypeScript's built-in `lib.dom.d.ts`
   * (and is also missing from `@types/wicg-file-system-access`), but Aero needs
   * to reference it for worker-side OPFS I/O.
   *
   * The interface here is intentionally minimal and matches the subset used by
   * our wrappers.
   */
  interface FileSystemSyncAccessHandle {
    read(buffer: ArrayBufferView, options?: { at?: number }): number;
    write(buffer: ArrayBufferView, options?: { at?: number }): number;
    flush(): void;
    close(): void;
    getSize(): number;
    truncate(newSize: number): void;
  }
}

