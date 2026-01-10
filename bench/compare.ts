import fs from "node:fs/promises";
import process from "node:process";
import { defaultThresholds } from "./thresholds";

function parseArgs(argv: string[]): {
  input?: string;
  enforce: boolean;
} {
  let input: string | undefined;
  let enforce = false;

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--input" && argv[i + 1]) {
      input = argv[++i]!;
    } else if (arg === "--enforce") {
      enforce = true;
    }
  }

  return { input, enforce };
}

function evaluateStorage(perf: any): { pass: boolean; violations: string[] } {
  const violations: string[] = [];
  const storage = perf?.benchmarks?.storage;

  const seqWrite = storage?.sequential_write?.mean_mb_per_s;
  const seqRead = storage?.sequential_read?.mean_mb_per_s;
  const p95 = storage?.random_read_4k?.mean_p95_ms;

  if (typeof seqWrite === "number" && seqWrite < defaultThresholds.storage.seq_write_mean_mb_per_s) {
    violations.push(
      `sequential_write.mean_mb_per_s ${seqWrite.toFixed(2)} < ${defaultThresholds.storage.seq_write_mean_mb_per_s}`,
    );
  }

  if (typeof seqRead === "number" && seqRead < defaultThresholds.storage.seq_read_mean_mb_per_s) {
    violations.push(
      `sequential_read.mean_mb_per_s ${seqRead.toFixed(2)} < ${defaultThresholds.storage.seq_read_mean_mb_per_s}`,
    );
  }

  if (typeof p95 === "number" && p95 > defaultThresholds.storage.random_read_p95_ms) {
    violations.push(
      `random_read_4k.mean_p95_ms ${p95.toFixed(2)} > ${defaultThresholds.storage.random_read_p95_ms}`,
    );
  }

  return { pass: violations.length === 0, violations };
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (!args.input) {
    throw new Error("Usage: npm run compare:storage -- --input <perf_export.json> [--enforce]");
  }

  const raw = await fs.readFile(args.input, "utf8");
  const parsed = JSON.parse(raw) as any;
  const evaluation = evaluateStorage(parsed);
  if (!evaluation.pass) {
    for (const v of evaluation.violations) {
      console.warn(`[threshold] ${v}`);
    }
  } else {
    console.log("[threshold] storage metrics within default targets");
  }

  if (args.enforce && !evaluation.pass) process.exit(1);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((err) => {
    console.error(err?.stack ?? String(err));
    process.exitCode = 1;
  });
}
