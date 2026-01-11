function toHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

export async function sha256Hex(data: Uint8Array): Promise<string> {
  if (!globalThis.crypto?.subtle) {
    throw new Error("SHA-256 integrity requires WebCrypto (crypto.subtle)");
  }
  const digest = await globalThis.crypto.subtle.digest("SHA-256", data);
  return toHex(new Uint8Array(digest));
}

