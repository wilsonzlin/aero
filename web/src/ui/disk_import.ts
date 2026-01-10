import { importFileToOpfs } from "../storage/opfs_raw";

export async function importDiskFromFileInput(
  input: HTMLInputElement,
  opts: {
    destName?: string;
    onProgress?: (percent: number) => void;
  } = {},
): Promise<string> {
  const file = input.files?.[0];
  if (!file) {
    throw new Error("no file selected");
  }
  return await importFileToOpfs(file, {
    destName: opts.destName,
    onProgress: (written, total) => {
      if (total > 0) {
        opts.onProgress?.((written / total) * 100);
      }
    },
  });
}

