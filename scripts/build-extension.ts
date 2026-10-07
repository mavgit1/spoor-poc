// Bundle the extension into extension/dist (what web-ext runs and signs).

import { build } from "esbuild";
import { cpSync, mkdirSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ext = join(dirname(fileURLToPath(import.meta.url)), "..", "extension");
const dist = join(ext, "dist");

rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });
await build({
  entryPoints: ["background", "home", "sidebar"].map(n => join(ext, "src", `${n}.ts`)),
  outdir: dist,
  bundle: true,
  format: "iife",
  target: "firefox140",
  logLevel: "warning",
});
cpSync(join(ext, "manifest.json"), join(dist, "manifest.json"));
cpSync(join(ext, "static"), dist, { recursive: true });
console.log(`built ${dist}`);
