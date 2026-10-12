// Spoor's Firefox: Firefox Developer Edition (or Nightly) with its own profile
// and the Spoor extension. No remote protocol, no automation flags; one
// container per site inside it.
//
// Developer Edition and Nightly honour xpinstall.signatures.required=false, so
// the unsigned build in extension/dist is packed into the profile's
// extensions folder and loads as a normal, permanent add-on on every start.
// Release Firefox would only take it as a temporary add-on over remote
// debugging, which Spoor doesn't do.

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { crc32, deflateRawSync } from "node:zlib";

import { paths } from "../service/paths.ts";

export const EXTENSION_ID = "spoor@mavgit1.local";
export const REPO = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
export const DIST = join(REPO, "extension", "dist");

/** Prefs for Firefox's own behaviour; nothing here is visible to a site. */
export function prefs(): Record<string, boolean | number | string> {
  return {
    "xpinstall.signatures.required": false,
    "extensions.autoDisableScopes": 0,
    // Pick up a rebuilt add-on in the profile while starting, not by
    // reloading it (and dropping the service connection) just after.
    "extensions.startupScanScopes": 1,
    "privacy.userContext.enabled": true,
    "privacy.userContext.ui.enabled": true,
    // Restore the last session on start. That keeps session cookies (most
    // logins are session cookies), so a restart doesn't log every site out.
    "browser.startup.page": 3,
    "browser.sessionstore.resume_from_crash": true,
    "browser.sessionstore.interval": 10000,
    // Developer Edition's dark theme would otherwise tell pages "prefers dark".
    "layout.css.prefers-color-scheme.content-override": 2,
    // spoor.download() saves here; the service moves files into run folders.
    "browser.download.dir": paths.downloads(),
    "browser.download.folderList": 2,
    "browser.download.useDownloadDir": true,
    "browser.download.alwaysOpenPanel": false,
    "browser.download.start_downloads_in_tmp_dir": false,
    "browser.download.manager.addToRecentDocs": false,
    "browser.shell.checkDefaultBrowser": false,
    "browser.startup.homepage_override.mstone": "ignore",
    "browser.aboutwelcome.enabled": false,
    "datareporting.policy.dataSubmissionPolicyBypassNotification": true,
    "browser.tabs.warnOnClose": false,
    // JSON opens as text, not in the viewer (an extension can't run scripts there).
    "devtools.jsonview.enabled": false,
    "devtools.debugger.remote-enabled": false,
    "devtools.chrome.enabled": false,
  };
}

export function findFirefox(): string {
  const env = process.env.SPOOR_FIREFOX;
  if (env) return env;
  const home = process.env.HOME ?? "";
  const candidates =
    process.platform === "darwin"
      ? ["/Applications/Firefox Developer Edition.app", join(home, "Applications/Firefox Developer Edition.app"), "/Applications/Firefox Nightly.app", join(home, "Applications/Firefox Nightly.app")].map(a => join(a, "Contents/MacOS/firefox"))
      : process.platform === "win32"
        ? [
            join(process.env.ProgramFiles ?? "C:\\Program Files", "Firefox Developer Edition", "firefox.exe"),
            join(process.env.LOCALAPPDATA ?? "", "Firefox Developer Edition", "firefox.exe"),
            join(process.env.ProgramFiles ?? "C:\\Program Files", "Firefox Nightly", "firefox.exe"),
          ]
        : ["/opt/firefox-developer-edition/firefox", "/usr/lib/firefox-developer-edition/firefox", "/usr/bin/firefox-developer-edition", "/opt/firefox-nightly/firefox"];
  const hit = candidates.find(existsSync);
  if (!hit) {
    throw new Error(
      `Firefox Developer Edition not found (looked in ${candidates.join(", ")}). ` +
        "Install it from https://www.mozilla.org/firefox/developer/ or set SPOOR_FIREFOX to a Developer Edition or Nightly binary.",
    );
  }
  return hit;
}

function writeUserJs(profile: string): void {
  mkdirSync(profile, { recursive: true });
  const lines = Object.entries(prefs()).map(([k, v]) => `user_pref(${JSON.stringify(k)}, ${JSON.stringify(v)});`);
  writeFileSync(join(profile, "user.js"), `// written by spoor browser\n${lines.join("\n")}\n`);
}

export function ensureBuilt(): void {
  if (existsSync(join(DIST, "manifest.json"))) return;
  const r = spawnSync(process.execPath, ["--no-warnings=ExperimentalWarning", join(REPO, "scripts", "build-extension.ts")], { stdio: "inherit" });
  if (r.status !== 0) throw new Error("building the extension failed");
}

function filesUnder(dir: string): string[] {
  return readdirSync(dir).flatMap(name => {
    const p = join(dir, name);
    return statSync(p).isDirectory() ? filesUnder(p) : [p];
  });
}

/** A minimal zip (deflate, no extras): all an .xpi needs. */
export function zip(dir: string): Buffer {
  const locals: Buffer[] = [];
  const central: Buffer[] = [];
  let offset = 0;
  const files = filesUnder(dir).sort();
  for (const file of files) {
    const name = Buffer.from(relative(dir, file).split("\\").join("/"));
    const data = readFileSync(file);
    const packed = deflateRawSync(data);
    const crc = crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0x0800, 6); // UTF-8 names
    local.writeUInt16LE(8, 8); // deflate
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(packed.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(name.length, 26);
    const entry = Buffer.alloc(46);
    entry.writeUInt32LE(0x02014b50, 0);
    entry.writeUInt16LE(20, 4);
    entry.writeUInt16LE(20, 6);
    entry.writeUInt16LE(0x0800, 8);
    entry.writeUInt16LE(8, 10);
    entry.writeUInt32LE(crc, 16);
    entry.writeUInt32LE(packed.length, 20);
    entry.writeUInt32LE(data.length, 24);
    entry.writeUInt16LE(name.length, 28);
    entry.writeUInt32LE(offset, 42);
    locals.push(local, name, packed);
    central.push(entry, name);
    offset += local.length + name.length + packed.length;
  }
  const cd = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(files.length, 8);
  end.writeUInt16LE(files.length, 10);
  end.writeUInt32LE(cd.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, cd, end]);
}

export interface LaunchOptions {
  headless?: boolean;
  /** Pages to open besides the home panel. */
  urls?: string[];
}

/** Start Spoor's Firefox with the current build of the extension. Returns once it is spawned. */
export function launch(opts: LaunchOptions = {}): void {
  const firefox = findFirefox();
  const profile = paths.profile();
  ensureBuilt();
  writeUserJs(profile);
  mkdirSync(paths.downloads(), { recursive: true });
  const extDir = join(profile, "extensions");
  mkdirSync(extDir, { recursive: true });
  // Only when it changed: a new file makes Firefox reinstall the add-on.
  const xpi = join(extDir, `${EXTENSION_ID}.xpi`);
  const packed = zip(DIST);
  if (!existsSync(xpi) || !readFileSync(xpi).equals(packed)) writeFileSync(xpi, packed);
  const headless = opts.headless ?? !!process.env.SPOOR_HEADLESS;
  const child = spawn(firefox, ["-profile", profile, "-no-remote", ...(headless ? ["-headless"] : []), ...(opts.urls ?? [])], {
    detached: true,
    stdio: "ignore",
  });
  child.unref();
}
