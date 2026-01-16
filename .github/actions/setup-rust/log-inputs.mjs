function getInput(name, fallback = "") {
  const v = (process.env[name] ?? "").trim();
  return v || fallback;
}

const toolchain = getInput("INPUT_TOOLCHAIN", "stable");
const targets = getInput("INPUT_TARGETS", "");
const components = getInput("INPUT_COMPONENTS", "");
const cache = getInput("INPUT_CACHE", "true");
const locked = getInput("INPUT_LOCKED", "always");

console.log("::group::Rust setup");
console.log(`toolchain: ${toolchain}`);
console.log(`targets: ${targets}`);
console.log(`components: ${components}`);
console.log(`cache: ${cache}`);
console.log(`locked: ${locked}`);
console.log("::endgroup::");

