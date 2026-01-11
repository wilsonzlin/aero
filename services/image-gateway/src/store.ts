export type ImageStatus = "uploading" | "complete";

export interface ImageRecord {
  id: string;
  ownerId: string;
  createdAt: string;
  version: string;

  s3Key: string;
  chunkedPrefix?: string;
  chunkedManifestKey?: string;
  uploadId: string;
  status: ImageStatus;

  size?: number;
  etag?: string;
  lastModified?: string;
}

export interface ImageStore {
  create(record: ImageRecord): void;
  get(id: string): ImageRecord | undefined;
  update(id: string, patch: Partial<ImageRecord>): ImageRecord;
}

export class MemoryImageStore implements ImageStore {
  private readonly records = new Map<string, ImageRecord>();

  create(record: ImageRecord): void {
    this.records.set(record.id, record);
  }

  get(id: string): ImageRecord | undefined {
    return this.records.get(id);
  }

  update(id: string, patch: Partial<ImageRecord>): ImageRecord {
    const existing = this.records.get(id);
    if (!existing) {
      throw new Error(`Image not found: ${id}`);
    }
    const updated = { ...existing, ...patch };
    this.records.set(id, updated);
    return updated;
  }
}
