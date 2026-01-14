/**
 * Encode interleaved Float32 audio samples as a 16-bit PCM WAV file.
 *
 * - `samples.length` must be a multiple of `channelCount`.
 * - Samples are expected in the Web Audio convention: [-1, 1] float range.
 */
export function encodeWavPcm16(samples: Float32Array, sampleRate: number, channelCount: number): Uint8Array {
  const sr = Math.trunc(Number.isFinite(sampleRate) ? sampleRate : 0);
  if (!Number.isFinite(sr) || sr <= 0) {
    throw new Error(`Invalid sampleRate: ${sampleRate}`);
  }
  const cc = Math.trunc(Number.isFinite(channelCount) ? channelCount : 0);
  if (!Number.isFinite(cc) || cc <= 0 || cc > 255) {
    throw new Error(`Invalid channelCount: ${channelCount}`);
  }
  if (samples.length % cc !== 0) {
    throw new Error(`Invalid sample buffer length: ${samples.length} (not divisible by channelCount=${cc})`);
  }

  const bytesPerSample = 2;
  const dataBytes = samples.length * bytesPerSample;
  const headerBytes = 44;
  const totalBytes = headerBytes + dataBytes;
  if (!Number.isSafeInteger(totalBytes) || totalBytes <= headerBytes) {
    throw new Error(`Invalid WAV size: ${totalBytes}`);
  }

  const buf = new ArrayBuffer(totalBytes);
  const view = new DataView(buf);
  const out = new Uint8Array(buf);

  const writeAscii = (offset: number, text: string) => {
    for (let i = 0; i < text.length; i += 1) {
      view.setUint8(offset + i, text.charCodeAt(i) & 0xff);
    }
  };

  // RIFF header.
  writeAscii(0, "RIFF");
  view.setUint32(4, totalBytes - 8, true);
  writeAscii(8, "WAVE");

  // fmt chunk.
  writeAscii(12, "fmt ");
  view.setUint32(16, 16, true); // PCM fmt chunk size
  view.setUint16(20, 1, true); // audio format = PCM
  view.setUint16(22, cc, true);
  view.setUint32(24, sr >>> 0, true);
  const byteRate = sr * cc * bytesPerSample;
  view.setUint32(28, byteRate >>> 0, true);
  view.setUint16(32, cc * bytesPerSample, true); // blockAlign
  view.setUint16(34, 16, true); // bitsPerSample

  // data chunk.
  writeAscii(36, "data");
  view.setUint32(40, dataBytes >>> 0, true);

  // PCM samples.
  let off = headerBytes;
  for (let i = 0; i < samples.length; i += 1) {
    let s = samples[i] ?? 0;
    if (!Number.isFinite(s)) s = 0;
    let v: number;
    if (s <= -1) v = -32768;
    else if (s >= 1) v = 32767;
    else v = s < 0 ? Math.round(s * 32768) : Math.round(s * 32767);
    view.setInt16(off, v, true);
    off += 2;
  }

  return out;
}

