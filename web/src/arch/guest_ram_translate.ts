import { HIGH_RAM_START, LOW_RAM_END } from "./guest_phys.ts";

export { HIGH_RAM_START, LOW_RAM_END };

function toU64(value: number, label: string): number {
  // JS numbers are IEEE754 doubles; we can exactly represent up to 2^53-1.
  if (!Number.isFinite(value)) {
    throw new RangeError(`Expected a finite integer ${label}, got ${String(value)}`);
  }
  const int = Math.trunc(value);
  if (int < 0 || int > Number.MAX_SAFE_INTEGER) {
    throw new RangeError(`Expected an integer ${label} in [0, 2^53), got ${String(value)}`);
  }
  return int;
}

/**
 * Translate a guest physical address into a backing-RAM offset (0..ramBytes) when (and only when)
 * the address is backed by RAM.
 *
 * Returns `null` for ECAM/PCI holes or out-of-range addresses.
 *
 * This mirrors the PC/Q35 address translation in `crates/aero-wasm/src/guest_phys.rs`.
 */
export function guestPaddrToRamOffset(ramBytes: number, paddr: number): number | null {
  const addr = toU64(paddr, "guest physical address");
  const ram = toU64(ramBytes, "guest RAM size");

  if (ram <= LOW_RAM_END) {
    return addr < ram ? addr : null;
  }

  // Low RAM.
  if (addr < LOW_RAM_END) return addr;

  // High RAM remap.
  const highLen = ram - LOW_RAM_END;
  const highEnd = HIGH_RAM_START + highLen;
  if (addr >= HIGH_RAM_START && addr < highEnd) {
    return LOW_RAM_END + (addr - HIGH_RAM_START);
  }

  // Hole or out-of-range.
  return null;
}

export function guestRangeInBounds(ramBytes: number, paddr: number, len: number): boolean {
  const addr = toU64(paddr, "guest physical address");
  const length = toU64(len, "byte length");
  const ram = toU64(ramBytes, "guest RAM size");

  if (ram <= LOW_RAM_END) {
    if (length === 0) return addr <= ram;
    return addr < ram && length <= ram - addr;
  }

  // Low RAM region: [0..LOW_RAM_END).
  if (addr <= LOW_RAM_END) {
    if (length === 0) return true;
    return addr < LOW_RAM_END && length <= LOW_RAM_END - addr;
  }

  // High RAM remap: [HIGH_RAM_START..HIGH_RAM_START + (ram-LOW_RAM_END)).
  const highLen = ram - LOW_RAM_END;
  const highEnd = HIGH_RAM_START + highLen;
  if (addr >= HIGH_RAM_START && addr <= highEnd) {
    if (length === 0) return true;
    return addr < highEnd && length <= highEnd - addr;
  }

  // Hole or out-of-range.
  return false;
}

