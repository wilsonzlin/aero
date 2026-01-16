import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { formatOneLineUtf8 } from "../../src/text.js";

const MAX_ERROR_MESSAGE_BYTES = 512;
const MAX_ERROR_POINTER_BYTES = 256;

/**
 * `tools/perf/validate_perf_export.mjs` is used by CI and also by the repo's
 * Node-based unit tests.
 *
 * The agent execution environment for unit tests is offline (no `npm install`),
 * so `node_modules/` is unavailable. Prefer Ajv when it is installed, but fall
 * back to a tiny in-repo JSON Schema validator for the subset of draft-07 used
 * by `bench/schema/perf-output.schema.json`.
 */

async function tryImportAjv() {
  try {
    const mod = await import("ajv");
    return mod?.default ?? null;
  } catch (err) {
    if (err && typeof err === "object" && "code" in err && err.code === "ERR_MODULE_NOT_FOUND") {
      return null;
    }
    throw err;
  }
}

function resolveJsonPointer(root, pointer) {
  if (pointer === "#") return root;
  if (!pointer.startsWith("#/")) return null;
  const parts = pointer
    .slice(2)
    .split("/")
    .map((p) => p.replaceAll("~1", "/").replaceAll("~0", "~"));
  let cur = root;
  for (const part of parts) {
    if (!cur || typeof cur !== "object") return null;
    cur = cur[part];
  }
  return cur;
}

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function schemaTypeMatches(type, value) {
  switch (type) {
    case "null":
      return value === null;
    case "boolean":
      return typeof value === "boolean";
    case "string":
      return typeof value === "string";
    case "number":
      return typeof value === "number" && Number.isFinite(value);
    case "integer":
      return typeof value === "number" && Number.isFinite(value) && Number.isInteger(value);
    case "array":
      return Array.isArray(value);
    case "object":
      return isPlainObject(value);
    default:
      return false;
  }
}

function pushError(errors, instancePath, keyword, message, params = {}) {
  errors.push({ instancePath, keyword, message, params });
}

function validateSchemaInternal(schema, value, instancePath, rootSchema, errors) {
  // Resolve `$ref` chains.
  while (schema && typeof schema === "object" && typeof schema.$ref === "string") {
    const resolved = resolveJsonPointer(rootSchema, schema.$ref);
    if (!resolved || typeof resolved !== "object") {
      pushError(errors, instancePath, "$ref", `unable to resolve schema ref ${schema.$ref}`);
      return;
    }
    schema = resolved;
  }

  if (!schema || typeof schema !== "object") return;

  const anyOf = Array.isArray(schema.anyOf) ? schema.anyOf : null;
  const oneOf = Array.isArray(schema.oneOf) ? schema.oneOf : null;
  const variants = anyOf ?? oneOf;
  if (variants) {
    const score = (errs) => {
      let maxDepth = 0;
      let sumDepth = 0;
      for (const err of errs) {
        const path = typeof err.instancePath === "string" ? err.instancePath : "";
        const depth = path.split("/").filter((p) => p.length > 0).length;
        if (depth > maxDepth) maxDepth = depth;
        sumDepth += depth;
      }
      return { count: errs.length, maxDepth, sumDepth };
    };

    let best = null;
    let bestScore = null;
    for (const variant of variants) {
      const variantErrors = [];
      validateSchemaInternal(variant, value, instancePath, rootSchema, variantErrors);
      if (variantErrors.length === 0) {
        return;
      }
      const candidateScore = score(variantErrors);
      if (
        !best ||
        candidateScore.count < bestScore.count ||
        (candidateScore.count === bestScore.count && candidateScore.maxDepth > bestScore.maxDepth) ||
        (candidateScore.count === bestScore.count &&
          candidateScore.maxDepth === bestScore.maxDepth &&
          candidateScore.sumDepth > bestScore.sumDepth)
      ) {
        best = variantErrors;
        bestScore = candidateScore;
      }
    }
    if (best) errors.push(...best);
    return;
  }

  if ("const" in schema) {
    if (value !== schema.const) {
      pushError(errors, instancePath, "const", "must be equal to constant", { allowedValue: schema.const });
      return;
    }
  }

  const schemaType = schema.type;
  if (typeof schemaType === "string") {
    if (!schemaTypeMatches(schemaType, value)) {
      pushError(errors, instancePath, "type", `must be ${schemaType}`);
      return;
    }
  } else if (Array.isArray(schemaType)) {
    const ok = schemaType.some((t) => typeof t === "string" && schemaTypeMatches(t, value));
    if (!ok) {
      pushError(errors, instancePath, "type", `must be ${schemaType.join(",")}`);
      return;
    }
  }

  if (typeof value === "number" && Number.isFinite(value)) {
    if (typeof schema.minimum === "number" && value < schema.minimum) {
      pushError(errors, instancePath, "minimum", `must be >= ${schema.minimum}`, { comparison: ">=", limit: schema.minimum });
    }
  }

  if (typeof value === "string") {
    if (typeof schema.pattern === "string") {
      const re = new RegExp(schema.pattern);
      if (!re.test(value)) {
        pushError(errors, instancePath, "pattern", `must match pattern ${schema.pattern}`, { pattern: schema.pattern });
      }
    }
  }

  if (Array.isArray(value) && schema.items && typeof schema.items === "object") {
    for (let i = 0; i < value.length; i += 1) {
      validateSchemaInternal(schema.items, value[i], `${instancePath}/${i}`, rootSchema, errors);
    }
  }

  if (isPlainObject(value)) {
    const properties = isPlainObject(schema.properties) ? schema.properties : {};
    const required = Array.isArray(schema.required) ? schema.required : [];

    for (const prop of required) {
      if (typeof prop !== "string") continue;
      if (!(prop in value)) {
        pushError(errors, instancePath, "required", "must have required property", { missingProperty: prop });
      }
    }

    for (const [prop, propSchema] of Object.entries(properties)) {
      if (!(prop in value)) continue;
      if (!propSchema || typeof propSchema !== "object") continue;
      validateSchemaInternal(propSchema, value[prop], `${instancePath}/${prop}`, rootSchema, errors);
    }

    const additional = schema.additionalProperties;
    if (additional === false) {
      for (const prop of Object.keys(value)) {
        if (prop in properties) continue;
        pushError(errors, instancePath, "additionalProperties", "must NOT have additional properties", {
          additionalProperty: prop,
        });
      }
    } else if (additional && typeof additional === "object") {
      for (const prop of Object.keys(value)) {
        if (prop in properties) continue;
        validateSchemaInternal(additional, value[prop], `${instancePath}/${prop}`, rootSchema, errors);
      }
    }
  }
}

function validateWithoutAjv(schemaJson, inputJson) {
  const errors = [];
  validateSchemaInternal(schemaJson, inputJson, "", schemaJson, errors);
  return errors;
}

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
    const msg = formatOneLineUtf8(err?.message ?? err, MAX_ERROR_MESSAGE_BYTES) || "Error";
    throw new Error(`${label}: failed to read ${file}: ${msg}`);
  }

  try {
    return JSON.parse(raw);
  } catch (err) {
    const msg = formatOneLineUtf8(err?.message ?? err, MAX_ERROR_MESSAGE_BYTES) || "Error";
    throw new Error(`${label}: failed to parse JSON from ${file}: ${msg}`);
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
  const safePtr = formatOneLineUtf8(ptr, MAX_ERROR_POINTER_BYTES) || "/";
  const rawMsg = typeof err.message === "string" ? err.message : "schema validation failed";
  const safeMsg = formatOneLineUtf8(rawMsg, MAX_ERROR_MESSAGE_BYTES) || "schema validation failed";
  return `${safePtr}: ${safeMsg}`;
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
  const Ajv = await tryImportAjv();

  let errors = [];
  if (Ajv) {
    const ajv = new Ajv({
      allErrors: true,
      strict: false,
    });
    const validate = ajv.compile(schemaJson);
    const ok = validate(inputJson);
    if (ok) return;
    errors = Array.isArray(validate.errors) ? validate.errors : [];
  } else {
    errors = validateWithoutAjv(schemaJson, inputJson);
    if (errors.length === 0) return;
  }

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
  const msg = formatOneLineUtf8(err?.message ?? err, MAX_ERROR_MESSAGE_BYTES) || "Error";
  console.error(msg);
  process.exit(1);
}
