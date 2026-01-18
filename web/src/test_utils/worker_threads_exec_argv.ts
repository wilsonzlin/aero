export const TS_STRIP_LOADER_URL = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);

function toImportSpecifier(value: string | URL): string {
  if (typeof value === "string") return value;
  return value.href;
}

export function makeNodeWorkerExecArgv(extraImports: Array<string | URL> = []): string[] {
  const out = ["--experimental-strip-types", "--import", TS_STRIP_LOADER_URL.href];
  for (const imp of extraImports) {
    out.push("--import", toImportSpecifier(imp));
  }
  return out;
}

