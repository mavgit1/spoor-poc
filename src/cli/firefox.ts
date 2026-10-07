// Spoor's Firefox: a regular release Firefox with its own profile and the
// Spoor extension. No remote protocol, no automation flags; one container per
// site inside it.
//
// - signed mode (default): the signed spoor.xpi is placed in the profile's
//   extensions folder and Firefox starts normally.
// - dev mode (--dev): web-ext loads extension/dist as a temporary add-on.
//   That uses Firefox's remote debugging to install it, so use it for
//   developing Spoor, not for daily use on sites.

import { spawn, spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { paths } from "../service/paths.ts";

export const EXTENSION_ID = "spoor@mavgit1.local";
export const REPO = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
export const DIST = join(REPO, "extension", "dist");

/** Prefs that only affect Firefox's own UI and startup, nothing a site sees. */
export const PREFS: Record<string, boolean | number | string> = {
  "privacy.userContext.enabled": true,
  "privacy.userContext.ui.enabled": true,
  "extensions.autoDisableScopes": 0,
  "browser.shell.checkDefaultBrowser": false,
  "browser.startup.homepage_override.mstone": "ignore",
  "browser.aboutwelcome.enabled": false,
  "datareporting.policy.dataSubmissionPolicyBypassNotification": true,
  "browser.tabs.warnOnClose": false,
};

export function findFirefox(): string {
  const env = process.env.SPOOR_FIREFOX;
  if (env) return env;
  const candidates =
    process.platform === "darwin"
      ? ["/Applications/Firefox.app/Contents/MacOS/firefox", join(process.env.HOME ?? "", "Applications/Firefox.app/Contents/MacOS/firefox")]
      : process.platform === "win32"
        ? [
            join(process.env.ProgramFiles ?? "C:\\Program Files", "Mozilla Firefox", "firefox.exe"),
            join(process.env["ProgramFiles(x86)"] ?? "C:\\Program Files (x86)", "Mozilla Firefox", "firefox.exe"),
            join(process.env.LOCALAPPDATA ?? "", "Mozilla Firefox", "firefox.exe"),
          ]
        : ["/usr/bin/firefox", "/usr/local/bin/firefox", "/snap/bin/firefox"];
  const hit = candidates.find(existsSync);
  if (!hit) throw new Error(`Firefox not found (looked in ${candidates.join(", ")}); set SPOOR_FIREFOX`);
  return hit;
}

function writeUserJs(profile: string): void {
  mkdirSync(profile, { recursive: true });
  const lines = Object.entries(PREFS).map(([k, v]) => `user_pref(${JSON.stringify(k)}, ${JSON.stringify(v)});`);
  writeFileSync(join(profile, "user.js"), `// written by spoor browser\n${lines.join("\n")}\n`);
}

export function ensureBuilt(): void {
  if (existsSync(join(DIST, "manifest.json"))) return;
  const r = spawnSync(process.execPath, ["--no-warnings=ExperimentalWarning", join(REPO, "scripts", "build-extension.ts")], { stdio: "inherit" });
  if (r.status !== 0) throw new Error("building the extension failed");
}

export interface LaunchOptions {
  dev?: boolean;
  headless?: boolean;
  /** Pages to open besides the home panel. */
  urls?: string[];
}

/** Start Spoor's Firefox. Signed mode returns once it is spawned; dev mode runs web-ext in the foreground. */
export async function launch(opts: LaunchOptions = {}): Promise<number | null> {
  const firefox = findFirefox();
  const profile = paths.profile();
  writeUserJs(profile);
  if (opts.dev) {
    ensureBuilt();
    const webExt = join(REPO, "node_modules", ".bin", process.platform === "win32" ? "web-ext.cmd" : "web-ext");
    const args = [
      "run",
      "--source-dir", DIST,
      "--firefox", firefox,
      "--firefox-profile", profile,
      "--profile-create-if-missing",
      "--keep-profile-changes",
      "--no-reload",
      "--no-input",
      ...Object.entries(PREFS).map(([k, v]) => `--pref=${k}=${v}`),
      ...(opts.headless ? ["--arg=-headless"] : []),
      ...(opts.urls ?? []).flatMap(u => ["--start-url", u]),
    ];
    const child = spawn(webExt, args, { stdio: "inherit", shell: process.platform === "win32" });
    return new Promise(resolve => child.on("exit", code => resolve(code)));
  }
  if (!existsSync(paths.xpi())) {
    throw new Error(`no signed extension at ${paths.xpi()} — run \`spoor sign\` once (needs AMO API keys), or use \`spoor browser --dev\``);
  }
  const extDir = join(profile, "extensions");
  mkdirSync(extDir, { recursive: true });
  copyFileSync(paths.xpi(), join(extDir, `${EXTENSION_ID}.xpi`));
  const child = spawn(firefox, ["-profile", profile, "-no-remote", ...(opts.headless ? ["-headless"] : []), ...(opts.urls ?? [])], {
    detached: true,
    stdio: "ignore",
  });
  child.unref();
  return null;
}

/**
 * Sign the extension as an unlisted add-on through addons.mozilla.org. Needs
 * AMO API credentials in AMO_JWT_ISSUER / AMO_JWT_SECRET
 * (https://addons.mozilla.org/developers/addon/api/key/).
 */
export function sign(): string {
  const issuer = process.env.AMO_JWT_ISSUER;
  const secret = process.env.AMO_JWT_SECRET;
  if (!issuer || !secret) throw new Error("set AMO_JWT_ISSUER and AMO_JWT_SECRET (https://addons.mozilla.org/developers/addon/api/key/)");
  ensureBuilt();
  const out = join(REPO, "extension", "signed");
  const webExt = join(REPO, "node_modules", ".bin", process.platform === "win32" ? "web-ext.cmd" : "web-ext");
  const r = spawnSync(webExt, ["sign", "--channel", "unlisted", "--source-dir", DIST, "--artifacts-dir", out, "--api-key", issuer, "--api-secret", secret], {
    stdio: "inherit",
    shell: process.platform === "win32",
  });
  if (r.status !== 0) throw new Error("web-ext sign failed");
  const ls = spawnSync(process.platform === "win32" ? "cmd" : "ls", process.platform === "win32" ? ["/c", "dir", "/b", "/o-d", out] : ["-t", out], { encoding: "utf8" });
  const newest = ls.stdout.split(/\r?\n/).find(f => f.endsWith(".xpi"));
  if (!newest) throw new Error(`no .xpi in ${out}`);
  mkdirSync(dirname(paths.xpi()), { recursive: true });
  copyFileSync(join(out, newest), paths.xpi());
  return paths.xpi();
}
