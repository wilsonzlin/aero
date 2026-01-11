import url from "./module.js?worker&url";

if (typeof url !== "string") {
  throw new Error(`expected a string default export, got: ${typeof url}`);
}

// Print a stable marker so the parent test can parse stdout.
console.log(url);
