import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';

import type { ArtifactKind, ArtifactManifestEntry, ArtifactWriter } from '../scenarios/types.ts';

export class FileArtifactWriter implements ArtifactWriter {
  readonly rootDir: string;
  readonly #entries: ArtifactManifestEntry[] = [];

  constructor(rootDir: string) {
    this.rootDir = rootDir;
  }

  async writeJson(path: string, data: unknown, kind: ArtifactKind = 'other'): Promise<void> {
    const payload = new TextEncoder().encode(`${JSON.stringify(data, null, 2)}\n`);
    await this.writeBinary(path, payload, kind);
  }

  async writeBinary(path: string, data: Uint8Array, kind: ArtifactKind = 'other'): Promise<void> {
    const target = join(this.rootDir, path);
    await mkdir(dirname(target), { recursive: true });
    await writeFile(target, data);
    this.#entries.push({ kind, path });
  }

  manifest(): ArtifactManifestEntry[] {
    return [...this.#entries];
  }
}

