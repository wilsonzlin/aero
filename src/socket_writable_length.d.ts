export function socketWritableLengthOrOverflow(
  socket: { readonly writableLength: number } | null | undefined,
  max: number,
): number;

export function socketWritableLengthExceedsCap(
  socket: { readonly writableLength: number } | null | undefined,
  max: number,
): boolean;

