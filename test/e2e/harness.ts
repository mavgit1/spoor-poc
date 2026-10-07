// The login harness against the fake site: silent login (the fake IdP signs
// in by itself, like a persistent Microsoft session would), capture, then
// plain HTTP with the captured session.
//
//   npm run e2e:harness     (needs `npx playwright install firefox` once)

import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { loginHarness } from "../../harness/src/index.ts";
import { startFakeSite } from "./site.ts";

const site = await startFakeSite(7610, 7611);
let failures = 0;
const check = (name: string, ok: unknown, detail?: unknown) => {
  console.log(`  ${ok ? "✔" : "✘"} ${name}${ok || detail === undefined ? "" : `\n      ${JSON.stringify(detail).slice(0, 800)}`}`);
  if (!ok) failures++;
};

try {
  const profile = join(mkdtempSync(join(tmpdir(), "spoor-harness-")), "fake");
  const session = await loginHarness({
    start: `${site.app}/`,
    profile,
    done: s => s.url.startsWith(site.app) && s.cookies.has("APPSESSION") && !!s.storage.get("authToken"),
    capture: { cookies: ["127.0.0.1"], storage: ["authToken"] },
    silentTimeoutMs: 20_000,
  });
  check("login completed without showing a window", session.silent, session);
  check("captured only the app's cookies", session.cookies.length > 0 && session.cookies.every(c => c.domain.includes("127.0.0.1")), session.cookies.map(c => c.name));
  check("captured the HttpOnly session cookie", session.cookies.some(c => c.name === "APPSESSION" && c.httpOnly));
  check("captured the storage token", session.storage.authToken?.startsWith("ey"), session.storage);
  check("expiry known", session.expiresAt instanceof Date);
  const r = await session.fetch(`${site.app}/api/items`);
  check("plain HTTP with the session works", r.status === 200 && (await r.json()).items.length >= 2);
  check("cookie header excludes other hosts", !session.cookieHeader(`${site.app}/`).includes("IDPSESSION"));
} catch (e) {
  failures++;
  console.log(`  ✘ ${(e as Error).stack}`);
} finally {
  await site.close();
}
console.log(failures ? `\n${failures} failed` : "\nall passed");
process.exit(failures ? 1 : 0);
