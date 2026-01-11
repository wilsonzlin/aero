const fs = require("node:fs");
const path = require("node:path");

const distDir = path.resolve(__dirname, "..", "dist");
fs.rmSync(distDir, { recursive: true, force: true });
