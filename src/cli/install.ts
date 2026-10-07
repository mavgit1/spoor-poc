// Registers the native messaging host with Firefox, so the extension can
// start it with runtime.connectNative("spoor_bridge").
//
//   macOS    ~/Library/Application Support/Mozilla/NativeMessagingHosts/spoor_bridge.json
//   Linux    ~/.mozilla/native-messaging-hosts/spoor_bridge.json
//   Windows  HKCU\Software\Mozilla\NativeMessagingHosts\spoor_bridge → manifest path
//
// The manifest points at a small launcher in $SPOOR_HOME/bin that runs the
// host with this Node and this checkout. SPOOR_HOME is baked into the
// launcher, because Firefox starts the host with Firefox's environment.

import { spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

import { spoorHome } from "../service/paths.ts";
import { EXTENSION_ID, REPO } from "./firefox.ts";

export const HOST_NAME = "spoor_bridge";

export function manifestDir(): string | null {
  if (process.platform === "darwin") return join(homedir(), "Library", "Application Support", "Mozilla", "NativeMessagingHosts");
  if (process.platform === "linux") return join(homedir(), ".mozilla", "native-messaging-hosts");
  return null; // Windows: registry points at a manifest anywhere
}

export function install(): { manifest: string; launcher: string; registry?: string } {
  const home = spoorHome();
  const binDir = join(home, "bin");
  mkdirSync(binDir, { recursive: true });
  const entry = join(REPO, "src", "host", "main.ts");
  let launcher: string;
  if (process.platform === "win32") {
    launcher = join(binDir, "spoor-host.cmd");
    writeFileSync(launcher, `@echo off\r\nset "SPOOR_HOME=${home}"\r\n"${process.execPath}" --no-warnings=ExperimentalWarning "${entry}" %*\r\n`);
  } else {
    launcher = join(binDir, "spoor-host");
    const q = (s: string) => `'${s.replace(/'/g, `'\\''`)}'`;
    writeFileSync(launcher, `#!/bin/sh\nSPOOR_HOME=${q(home)} exec ${q(process.execPath)} --no-warnings=ExperimentalWarning ${q(entry)} "$@"\n`);
    chmodSync(launcher, 0o755);
  }
  const manifest = {
    name: HOST_NAME,
    description: "Spoor local service bridge",
    path: launcher,
    type: "stdio",
    allowed_extensions: [EXTENSION_ID],
  };
  const dir = manifestDir() ?? home;
  mkdirSync(dir, { recursive: true });
  const manifestPath = join(dir, `${HOST_NAME}.json`);
  writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + "\n");
  if (process.platform === "win32") {
    const key = `HKCU\\Software\\Mozilla\\NativeMessagingHosts\\${HOST_NAME}`;
    const r = spawnSync("reg", ["add", key, "/ve", "/t", "REG_SZ", "/d", manifestPath, "/f"], { encoding: "utf8" });
    if (r.status !== 0) throw new Error(`reg add failed: ${r.stderr || r.stdout}`);
    return { manifest: manifestPath, launcher, registry: key };
  }
  return { manifest: manifestPath, launcher };
}
