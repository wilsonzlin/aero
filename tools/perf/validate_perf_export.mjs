import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import Ajv from "ajv";

function usage(exitCode) {
  const msg = `
Usage:
  node tools/perf/validate_perf_export.mjs --schema <schema.json> --input <perf_export.json>

Options:
  --schema <path>    JSON schema file (required)
  --input <path>     perf_export.json file (required)
  --allow-null       Treat a literal \`null\` perf export as success
  --help             Show this help
`;
  console.log(msg.trim());
  process.exit(exitCode);
}

function parseArgs(argv) {
  const out = {
    schema: undefined,
    input: undefined,
    allowNull: false,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    switch (arg) {
      case "--schema":
        out.schema = argv[++i];
        break;
      case "--input":
        out.input = argv[++i];
        break;
      case "--allow-null":
        out.allowNull = true;
        break;
      case "--help":
        usage(0);
        break;
      default:
        if (arg.startsWith("-")) {
          console.error(`Unknown option: ${arg}`);
          usage(1);
        }
        break;
    }
  }

  if (!out.schema || !out.input) {
    console.error("--schema and --input are required");
    usage(1);
  }

  return out;
}

async function readJsonFile(file, label) {
  let raw;
  try {
    raw = await fs.readFile(file, "utf8");
  } catch (err) {
    throw new Error(`${label}: failed to read ${file}: ${err?.message ?? String(err)}`);
  }

  try {
    return JSON.parse(raw);
  } catch (err) {
    throw new Error(`${label}: failed to parse JSON from ${file}: ${err?.message ?? String(err)}`);
  }
}

function formatAjvError(err) {
  let instancePath = typeof err.instancePath === "string" ? err.instancePath : "";

  if (err.keyword === "required" && err.params && typeof err.params.missingProperty === "string") {
    instancePath = `${instancePath}/${err.params.missingProperty}`;
  }
  if (err.keyword === "additionalProperties" && err.params && typeof err.params.additionalProperty === "string") {
    instancePath = `${instancePath}/${err.params.additionalProperty}`;
  }

  const ptr = instancePath === "" ? "/" : instancePath;
  const msg = typeof err.message === "string" ? err.message : "schema validation failed";
  return `${ptr}: ${msg}`;
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const schemaPath = path.resolve(process.cwd(), opts.schema);
  const inputPath = path.resolve(process.cwd(), opts.input);

  const inputJson = await readJsonFile(inputPath, "input");
  if (inputJson === null) {
    if (opts.allowNull) return;
    throw new Error(
      `input: ${path.relative(process.cwd(), inputPath)} is null (perf export missing). ` +
        "Pass --allow-null to accept null exports.",
    );
  }

  const schemaJson = await readJsonFile(schemaPath, "schema");
  const ajv = new Ajv({
    allErrors: true,
    strict: false,
  });

  const validate = ajv.compile(schemaJson);
  const ok = validate(inputJson);
  if (ok) return;

  const errors = Array.isArray(validate.errors) ? validate.errors : [];
  const header = `[perf] schema validation failed for ${path.relative(process.cwd(), inputPath)} (${errors.length} error${
    errors.length === 1 ? "" : "s"
  })`;
  console.error(header);
  for (const err of errors) {
    console.error(`- ${formatAjvError(err)}`);
  }
  process.exit(1);
}

try {
  await main();
} catch (err) {
  console.error(err?.message ?? String(err));
  process.exit(1);
}

