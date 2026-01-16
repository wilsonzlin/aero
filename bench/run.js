#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";

import { formatOneLineError } from "../src/text.js";

function parseArgs(argv) {
  const options = {
    out: undefined,
    scenario: "all",
    iterations: undefined,
    help: false,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const requireValue = (flag) => {
      const value = argv[i + 1];
      if (!value || value.startsWith("--")) throw new Error(`${flag} requires a value`);
      i += 1;
      return value;
    };
    switch (arg) {
      case "--help":
      case "-h":
        options.help = true;
        break;
      case "--out":
        options.out = requireValue("--out");
        break;
      case "--scenario":
        options.scenario = requireValue("--scenario");
        break;
      case "--iterations":
        options.iterations = requireValue("--iterations");
        break;
      default:
        throw new Error(`Unknown argument: ${arg}`);
    }
  }

  return options;
}

function printHelp() {
  process.stdout.write(`bench/run.js

Lightweight Node microbench runner (PF-009).

Usage:
  node bench/run.js --out <path> [--scenario <startup|microbench|all>] [--iterations <n>]

Options:
  --out <path>           Write results JSON to <path> (required)
  --scenario <name>      Scenario subset to run (default: all)
  --iterations <n>       Samples per scenario (default: startup=7, microbench=5)
`);
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
  if (options.help) {
    printHelp();
    return;
  }

  const outPath = options.out;
  if (!outPath) throw new Error("run.js requires --out <path>");

  const scenario = options.scenario ?? "all";
  if (!["startup", "microbench", "all"].includes(scenario)) {
    throw new Error(`Invalid --scenario: ${scenario}`);
  }

  const iterationsRaw = options.iterations;
  const iterations =
    iterationsRaw === undefined ? undefined : Number.parseInt(String(iterationsRaw), 10);
  if (iterationsRaw !== undefined && (!Number.isFinite(iterations) || iterations <= 0)) {
    throw new Error(`Invalid --iterations: ${iterationsRaw}`);
  }

  const warmup = 1;
  const scenarioData = {};

  if (scenario === "startup" || scenario === "all") {
    for (let i = 0; i < warmup; i++) startupWork();

    const startupSamples = [];
    const startupIterations = iterations ?? 7;
    for (let i = 0; i < startupIterations; i++) startupSamples.push(measureMs(startupWork));

    scenarioData.startup = {
      metrics: {
        startup_ms: {
          unit: "ms",
          better: "lower",
          samples: startupSamples,
        },
      },
    };
  }

  if (scenario === "microbench" || scenario === "all") {
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
    const microIterations = iterations ?? 5;

    for (let i = 0; i < microIterations; i++) {
      jsonParseSamples.push(benchOpsPerSecond(() => JSON.parse(jsonText), durationMs));
      arithSamples.push(
        benchOpsPerSecond(() => {
          let x = 1;
          for (let j = 0; j < 100; j++) x = (x * 1664525 + 1013904223) >>> 0;
          if (x === 0xdeadbeef) throw new Error("unreachable");
        }, durationMs),
      );
    }

    scenarioData.microbench = {
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
    };
  }

  const payload = {
    schemaVersion: 1,
    meta: {
      node: process.version,
      platform: process.platform,
      arch: process.arch,
    },
    scenarios: scenarioData,
  };

  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  fs.writeFileSync(outPath, `${JSON.stringify(payload, null, 2)}\n`, "utf8");
}

if (fileURLToPath(import.meta.url) === path.resolve(process.argv[1] ?? "")) {
  try {
    main();
  } catch (err) {
    console.error(formatOneLineError(err, 512));
    process.exitCode = 1;
  }
}
