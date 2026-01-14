export type TarEntry = {
  /**
   * Path inside the archive.
   *
   * Use forward slashes (`/`). Directory entries are optional; regular file entries with
   * a `dir/file` path are sufficient for most extractors.
   */
  path: string;
  /** Raw file bytes. */
  data: Uint8Array;
  /** POSIX mode bits (default 0o644). */
  mode?: number;
  /** Unix mtime in seconds (default `Date.now()/1000`). */
  mtimeSec?: number;
};

const TAR_BLOCK_BYTES = 512;

function concatU8(chunks: Uint8Array[]): Uint8Array {
  let total = 0;
  for (const c of chunks) total += c.byteLength;
  const out = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) {
    out.set(c, off);
    off += c.byteLength;
  }
  return out;
}

function encodeAscii(src: string): Uint8Array {
  // Tar headers are ASCII; limit to low bytes.
  const out = new Uint8Array(src.length);
  for (let i = 0; i < src.length; i += 1) out[i] = src.charCodeAt(i) & 0xff;
  return out;
}

function writeAsciiField(dst: Uint8Array, offset: number, len: number, value: string): void {
  const bytes = encodeAscii(value);
  dst.set(bytes.subarray(0, len), offset);
}

function writeOctalField(dst: Uint8Array, offset: number, len: number, value: number): void {
  // Fields are NUL-terminated octal with leading zeros.
  const digitsLen = Math.max(0, len - 1);
  const v = typeof value === "number" && Number.isFinite(value) ? Math.max(0, Math.trunc(value)) : 0;
  const oct = v.toString(8);
  const padded = oct.padStart(digitsLen, "0").slice(-digitsLen) + "\0";
  writeAsciiField(dst, offset, len, padded);
}

function splitUstarPath(path: string): { name: string; prefix: string } {
  if (path.length <= 100) return { name: path, prefix: "" };

  // Try to split on the last '/' so name<=100 and prefix<=155.
  for (let i = path.lastIndexOf("/"); i >= 0; i = path.lastIndexOf("/", i - 1)) {
    const prefix = path.slice(0, i);
    const name = path.slice(i + 1);
    if (name.length <= 100 && prefix.length <= 155) return { name, prefix };
  }

  throw new Error(`tar: path too long for ustar header: ${path}`);
}

function computeChecksum(header: Uint8Array): number {
  let sum = 0;
  for (let i = 0; i < header.length; i += 1) sum += header[i] ?? 0;
  return sum;
}

function writeUstarHeader(entry: TarEntry, mtimeSecDefault: number): Uint8Array {
  const header = new Uint8Array(TAR_BLOCK_BYTES);

  const { name, prefix } = splitUstarPath(entry.path);
  writeAsciiField(header, 0, 100, name);

  // mode / uid / gid
  writeOctalField(header, 100, 8, entry.mode ?? 0o644);
  writeOctalField(header, 108, 8, 0);
  writeOctalField(header, 116, 8, 0);

  // size / mtime
  writeOctalField(header, 124, 12, entry.data.byteLength);
  writeOctalField(header, 136, 12, entry.mtimeSec ?? mtimeSecDefault);

  // checksum field is computed with this field treated as spaces.
  for (let i = 148; i < 156; i += 1) header[i] = 0x20;

  // typeflag: '0' = regular file.
  header[156] = 0x30;

  // magic + version
  writeAsciiField(header, 257, 6, "ustar\0");
  writeAsciiField(header, 263, 2, "00");

  // uname/gname (optional; keep stable to aid diffing).
  writeAsciiField(header, 265, 32, "aero");
  writeAsciiField(header, 297, 32, "aero");

  // prefix
  if (prefix) writeAsciiField(header, 345, 155, prefix);

  const checksum = computeChecksum(header);
  const chk = checksum.toString(8).padStart(6, "0").slice(-6);
  // Standard checksum field encoding: 6 digits, NUL, space.
  writeAsciiField(header, 148, 6, chk);
  header[154] = 0;
  header[155] = 0x20;

  return header;
}

export function createTarArchive(entries: TarEntry[], opts: { mtimeSec?: number } = {}): Uint8Array {
  const mtimeSecDefault =
    typeof opts.mtimeSec === "number" && Number.isFinite(opts.mtimeSec) ? Math.trunc(opts.mtimeSec) : Math.trunc(Date.now() / 1000);

  const chunks: Uint8Array[] = [];
  for (const entry of entries) {
    if (!entry || typeof entry.path !== "string") {
      throw new Error("tar: invalid entry (missing path).");
    }
    if (!(entry.data instanceof Uint8Array)) {
      throw new Error(`tar: invalid entry data for ${entry.path} (expected Uint8Array).`);
    }

    const header = writeUstarHeader(entry, mtimeSecDefault);
    chunks.push(header);
    chunks.push(entry.data);

    const pad = (TAR_BLOCK_BYTES - (entry.data.byteLength % TAR_BLOCK_BYTES)) % TAR_BLOCK_BYTES;
    if (pad) chunks.push(new Uint8Array(pad));
  }

  // End of archive: two zero blocks.
  chunks.push(new Uint8Array(TAR_BLOCK_BYTES * 2));

  return concatU8(chunks);
}
