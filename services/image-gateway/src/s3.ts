import { S3Client } from "@aws-sdk/client-s3";

import type { Config } from "./config";
import { imageBasePathToPrefix } from "./config";

export function createS3Client(config: Config): S3Client {
  return new S3Client({
    region: config.awsRegion,
    endpoint: config.s3Endpoint,
    forcePathStyle: config.s3ForcePathStyle,
  });
}

export function buildImageObjectKey(params: {
  imageBasePath: string;
  ownerId: string;
  imageId: string;
  version: string;
  filename?: string;
}): string {
  const filename = params.filename ?? "disk.img";
  const prefix = imageBasePathToPrefix(params.imageBasePath);
  return `${prefix}/${params.ownerId}/${params.imageId}/${params.version}/${filename}`;
}

