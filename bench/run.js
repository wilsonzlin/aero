#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";

function parseArgs(argv) {
  const options = {};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (!arg.startsWith("--")) throw new Error(`Unexpected argument: ${arg}`);
    const key = arg.slice(2);
    const value = argv[i + 1];
    if (!value || value.startsWith("--")) throw new Error(`Missing value for --${key}`);
    options[key] = value;
    i++;
  }
  return options;
}

function measureMs(fn) {
  const start = performance.now();
  fn();
  return performance.now() - start;
}

function benchOpsPerSecond(fn, durationMs) {
  const start = performance.now();
  let ops = 0;
  while (performance.now() - start < durationMs) {
    fn();
    ops++;
  }
  const elapsedSec = (performance.now() - start) / 1000;
  return ops / elapsedSec;
}

function startupWork() {
  const buf = new Uint8Array(4_000_000);
  for (let i = 0; i < buf.length; i += 97) buf[i] = i & 0xff;
  let acc = 0;
  for (let i = 0; i < 100_000; i++) acc = (acc + i) ^ (acc >>> 1);
  if (acc === 42) buf[0] = acc;
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const outPath = options.out;
  if (!outPath) throw new Error("run.js requires --out <path>");

  const warmup = 1;
  for (let i = 0; i < warmup; i++) startupWork();

  const startupSamples = [];
  for (let i = 0; i < 7; i++) startupSamples.push(measureMs(startupWork));

  const jsonText = JSON.stringify({
    items: Array.from({ length: 50 }, (_, i) => ({
      id: i,
      name: `item-${i}`,
      tags: ["alpha", "beta", "gamma"],
      value: Math.sin(i) * 1000,
    })),
  });

  const warmupMicro = () => JSON.parse(jsonText);
  for (let i = 0; i < 10; i++) warmupMicro();

  const jsonParseSamples = [];
  const arithSamples = [];
  const durationMs = 200;

  for (let i = 0; i < 5; i++) {
    jsonParseSamples.push(benchOpsPerSecond(() => JSON.parse(jsonText), durationMs));
    arithSamples.push(
      benchOpsPerSecond(() => {
        let x = 1;
        for (let j = 0; j < 100; j++) x = (x * 1664525 + 1013904223) >>> 0;
        if (x === 0xdeadbeef) throw new Error("unreachable");
      }, durationMs),
    );
  }

  const payload = {
    schemaVersion: 1,
    meta: {
      node: process.version,
      platform: process.platform,
      arch: process.arch,
    },
    scenarios: {
      startup: {
        metrics: {
          startup_ms: {
            unit: "ms",
            better: "lower",
            samples: startupSamples,
          },
        },
      },
      microbench: {
        metrics: {
          json_parse_ops_s: {
            unit: "ops/s",
            better: "higher",
            samples: jsonParseSamples,
          },
          arith_ops_s: {
            unit: "ops/s",
            better: "higher",
            samples: arithSamples,
          },
        },
      },
    },
  };

  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  fs.writeFileSync(outPath, `${JSON.stringify(payload, null, 2)}\n`, "utf8");
}

if (fileURLToPath(import.meta.url) === path.resolve(process.argv[1] ?? "")) {
  try {
    main();
  } catch (err) {
    console.error(err instanceof Error ? err.message : err);
    process.exitCode = 1;
  }
}
