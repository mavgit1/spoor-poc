// End-to-end test: real Firefox (headless, dev mode via web-ext), the real
// extension, native messaging host and service, against the fake site.
//
//   npm run e2e            (needs Firefox installed; registers the native host)
//
// Uses a throwaway SPOOR_HOME. Note: the native messaging manifest is global
// per user, so this re-points it at the test home; `spoor install` afterwards
// points it back (the script does that at the end).

import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import type { CookieInfo, Flow, RunEvent, StorageSnapshot } from "../../src/shared/types.ts";
import { startFakeSite } from "./site.ts";

const HOME = mkdtempSync(join(tmpdir(), "spoor-e2e-"));
const ORIGINAL_HOME = process.env.SPOOR_HOME;
process.env.SPOOR_HOME = HOME;
const { api } = await import("../../src/service/client.ts");
const { paths } = await import("../../src/service/paths.ts");
const store = await import("../../src/service/store.ts");

const REPO = join(import.meta.dirname, "..", "..");
const CLI = join(REPO, "src", "cli", "main.ts");
const PORT = 7518;

let failures = 0;
function check(name: string, ok: unknown, detail?: unknown): void {
  if (ok) console.log(`  ✔ ${name}`);
  else {
    failures++;
    console.log(`  ✘ ${name}${detail !== undefined ? `\n      ${typeof detail === "string" ? detail : JSON.stringify(detail).slice(0, 1500)}` : ""}`);
  }
}

/** Run the CLI without blocking this process (the fake site lives here). */
function cli(...args: string[]): Promise<{ status: number | null; stdout: string; stderr: string }> {
  return new Promise(resolve => {
    const child = spawn(process.execPath, ["--no-warnings=ExperimentalWarning", CLI, ...args], { env: { ...process.env, SPOOR_HOME: HOME } });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", d => (stdout += d));
    child.stderr.on("data", d => (stderr += d));
    child.on("exit", status => resolve({ status, stdout, stderr }));
  });
}

async function until<T>(what: string, f: () => Promise<T | undefined | false>, timeoutMs = 30_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  let last: unknown;
  while (Date.now() < deadline) {
    try {
      const v = await f();
      if (v) return v;
    } catch (e) {
      last = e;
    }
    await new Promise(r => setTimeout(r, 300));
  }
  throw new Error(`timed out waiting for ${what}${last ? `: ${(last as Error).message}` : ""}`);
}

const children: ChildProcess[] = [];
function start(args: string[], name: string): ChildProcess {
  const child = spawn(process.execPath, ["--no-warnings=ExperimentalWarning", CLI, ...args], { env: { ...process.env, SPOOR_HOME: HOME }, stdio: ["ignore", "pipe", "pipe"], detached: true });
  let buf = "";
  child.stdout!.on("data", d => (buf += d));
  child.stderr!.on("data", d => (buf += d));
  child.on("exit", code => {
    if (process.env.E2E_VERBOSE || (code && code !== 0)) console.log(`[${name} exited ${code}]\n${buf.slice(-4000)}`);
  });
  children.push(child);
  return child;
}

const site = await startFakeSite();
console.log(`spoor home ${HOME}\nfake app ${site.app}, idp ${site.idp}`);

try {
  console.log("setup");
  check("install registers the native host", (await cli("install")).status === 0);
  check("build extension", spawnSync(process.execPath, ["--no-warnings=ExperimentalWarning", join(REPO, "scripts", "build-extension.ts")]).status === 0);
  start(["serve", "--port", String(PORT)], "serve");
  await until("service", async () => existsSync(paths.serve()) && (await api("GET", "/health", undefined, { start: false })).ok);
  store.addSite("testapp", { url: `${site.app}/`, check: "logged-in", min_gap_ms: 300 });
  store.writeScript("testapp", "logged-in", "// true when the app session works\nconst r = await fetch('/api/items');\nreturn r.status === 200;\n");
  start(["browser", "--dev", "--headless"], "firefox");
  const state = await until("Firefox to connect over native messaging", async () => {
    const s = await api("GET", "/state");
    return s.browser.connected ? s : undefined;
  }, 90_000);
  check("extension connected through the native host", state.browser.connected, state);
  check("extension reports the Firefox version", typeof state.browser.firefox === "string", state.browser);

  console.log("recording a login (SSO-style redirect across hosts)");
  const rec = await api("POST", "/sites/testapp/record/start", {});
  check("record start", rec.id);
  await until("the app to load after login", async () => site.hits.some(h => h.path === "/api/items" && h.cookie?.includes("APPSESSION")), 30_000);
  await api("POST", "/sites/testapp/record/mark", { label: "logged in" });
  await new Promise(r => setTimeout(r, 800));
  const stopped = await api("POST", "/sites/testapp/record/stop");
  check("record stop wrote cookies", stopped.cookies >= 2, stopped);
  const recRef = store.findRecording(`testapp:${rec.id}`);
  const flows = store.readFlows(recRef);
  const byPath = (p: string) => flows.find(f => f.kind === "http" && new URL(f.url).pathname === p);
  const authorize = byPath("/authorize");
  const login = byPath("/login");
  const cb = byPath("/cb");
  const home = flows.find(f => f.kind === "http" && f.url === `${site.app}/` && f.status === 200);
  check("first navigation redirects (302) to the idp", flows.some(f => f.url === `${site.app}/` && f.status === 302 && f.redirect_url?.startsWith(site.idp)), flows.map(f => `${f.sequence} ${f.status} ${f.url}`));
  check("idp authorize page recorded with HTML body", authorize?.response_body?.kind === "text" && authorize.response_body.text.includes("Sign in"), authorize);
  check("login POST recorded, password redacted", login?.request_body?.kind === "text" && login.request_body.text.includes("redacted") && !login.request_body.text.includes("hunter2"), login?.request_body);
  check("login response Set-Cookie captured", login?.set_cookies?.some(c => c.startsWith("IDPSESSION=")), login);
  check("callback sets the HttpOnly app session", cb?.set_cookies?.some(c => c.startsWith("APPSESSION=")), cb);
  check("app page recorded with body", home?.response_body?.kind === "text" && home.response_body.text.includes("csrf_token"), home?.response_body);
  const xhr = flows.find(f => f.type === "xmlhttprequest" && f.url.endsWith("/api/items"));
  check("page XHR recorded with request Cookie header and JSON body", xhr?.request_headers?.cookie?.includes("APPSESSION") && xhr.response_body?.kind === "text" && xhr.response_body.text.includes("4242"), xhr);
  check("marker recorded", flows.some(f => f.kind === "mark" && f.label === "logged in"));
  const cookies = store.readRecordingFile<CookieInfo[]>(recRef, "cookies.json", []);
  check("cookie snapshot has both hosts' cookies incl. HttpOnly", cookies.some(c => c.name === "APPSESSION" && c.httpOnly) && cookies.some(c => c.name === "IDPSESSION"), cookies);
  const storage = store.readRecordingFile<StorageSnapshot[]>(recRef, "storage.json", []);
  check("localStorage snapshot has the token", storage.some(s => s.local.authToken?.startsWith("ey")), storage);

  console.log("discovery views");
  const auth = await api("GET", `/recordings/${encodeURIComponent(`testapp:${rec.id}`)}/auth`);
  check("auth chain spans both hosts", auth.hosts.includes("127.0.0.1:7600") && auth.hosts.includes("localhost:7601"), auth.hosts);
  check("auth chain spots OAuth/OIDC parameters", auth.protocol_hints.some((h: string) => /OpenID|OAuth/.test(h)), auth.protocol_hints);
  const life = await api("GET", `/recordings/${encodeURIComponent(`testapp:${rec.id}`)}/lifetimes`);
  check("lifetimes decode the JWT and see it in localStorage", life.jwts.some((j: any) => j.lifetime_s === 3600 && j.seen_in.some((w: string) => w.includes("authToken"))), life.jwts);
  const tr = await api("GET", `/recordings/${encodeURIComponent(`testapp:${rec.id}`)}/trace?value=4242`);
  check("trace finds the item id in the page HTML", tr.appears_in.some((h: any) => h.place === "response body"), tr);
  const dossier = await api("POST", "/sites/testapp/dossier", {});
  check("dossier written", dossier.files.some((f: string) => f.endsWith("auth.md")), dossier);
  const replay = await api("POST", `/recordings/${encodeURIComponent(`testapp:${rec.id}`)}/replay`, { seq: xhr!.sequence, minimize: true });
  check("replay outside the browser matches", replay.baseline.matches, replay);
  check("replay minimize finds APPSESSION is the required cookie", replay.required_cookies?.includes("APPSESSION") && !replay.required_cookies.includes("IDPSESSION"), replay);
  const writeReplay = await api("POST", `/recordings/${encodeURIComponent(`testapp:${rec.id}`)}/replay`, { seq: login!.sequence }).catch(e => e);
  check("replaying a POST is refused without allow_write", writeReplay instanceof Error && /allow_write/.test(writeReplay.message), String(writeReplay));

  console.log("runs in the worker tab");
  const st = await api("GET", "/sites/testapp/status");
  check("status: check script says logged in", st.logged_in === true, st);
  const ex = await api("POST", "/sites/testapp/exec", { code: "console.log('hi', {a: 1}); return { title: document.title, csrf: document.querySelector('input[name=csrf_token]').value };" });
  check("exec returns the result", ex.ok && ex.result.title === "Fake App" && ex.result.csrf === "csrf-7f3a9c", ex);
  check("exec returns console output", ex.logs?.[0] === 'hi {"a":1}', ex.logs);
  const before = site.hits.length;
  const post = await api("POST", "/sites/testapp/exec", {
    code: `const csrf = document.querySelector('input[name=csrf_token]').value;
const r = await fetch('/api/items', { method: 'POST', headers: { 'content-type': 'application/json', 'x-csrf-token': csrf }, body: JSON.stringify({ id: args.id, name: 'new' }) });
return { status: r.status, body: await r.json() };`,
    args: { id: 5000 },
  });
  check("write through the page's session (cookie + CSRF) works", post.ok && post.result.status === 201, post);
  const postHit = site.hits.slice(before).find(h => h.method === "POST");
  check("request carries the page's Origin and cookies (content.fetch)", postHit?.origin === site.app && postHit.cookie?.includes("APPSESSION"), postHit);

  const t0 = site.hits.length;
  const paced = await api("POST", "/sites/testapp/exec", { code: "const rs = await Promise.all([1,2,3,4].map(i => fetch('/api/slow/' + i).then(r => r.json()))); return rs.length;" });
  const times = site.hits.slice(t0).filter(h => h.path.startsWith("/api/slow")).map(h => h.at).sort();
  const gaps = times.slice(1).map((t, i) => t - times[i]!);
  check("parallel fetches are paced (min_gap 300 ms)", paced.ok && gaps.length === 3 && gaps.every(g => g >= 270), { gaps, paced });
  check("requests counted for the run", paced.requests >= 4, paced.requests);

  const syntax = await api("POST", "/sites/testapp/exec", { code: "return (" });
  check("a syntax error fails the run with a message", !syntax.ok && /syntax|expected|unexpected/i.test(syntax.error ?? ""), syntax);
  const thrown = await api("POST", "/sites/testapp/exec", { code: "throw new Error('logged_out')" });
  check("a thrown error fails the run", !thrown.ok && thrown.error === "logged_out", thrown);

  store.writeScript("testapp", "batch", `// saves a backup, reports progress, asks before writing
const items = (await (await fetch('/api/items')).json()).items;
await spoor.save('backup.json', items);
for (let i = 0; i < items.length; i++) spoor.progress(i + 1, items.length, 'reading');
const ok = await spoor.confirm('Write ' + items.length + ' records?', { count: items.length, dry: args.dry });
return { ok, n: items.length };
`);
  const runMeta = await api("POST", "/sites/testapp/runs", { script: "batch", args: { dry: true } });
  await until("the run to wait for approval", async () => (await api("GET", `/sites/testapp/runs/${runMeta.id}`)).meta.status === "waiting", 20_000);
  const info = await api("GET", `/sites/testapp/runs/${runMeta.id}`);
  check("run folder has the saved backup", info.files.some((f: any) => f.name === "backup.json"), info.files);
  check("run progress recorded", info.meta.progress?.total >= 2, info.meta.progress);
  check("saved backup content is the items", JSON.parse(readFileSync(join(paths.run("testapp", runMeta.id), "backup.json"), "utf8")).length >= 2);
  await api("POST", `/sites/testapp/runs/${runMeta.id}/stop`);
  const final = await until("the run to stop", async () => {
    const m = (await api("GET", `/sites/testapp/runs/${runMeta.id}`)).meta;
    return m.status === "stopped" ? m : undefined;
  });
  check("waiting run can be stopped", final.status === "stopped", final);
  const events = store.readRunEvents("testapp", runMeta.id) as RunEvent[];
  check("event log has confirm, saved, progress", ["confirm", "saved", "progress"].every(t => events.some(e => e.type === t)), events.map(e => e.type));

  store.writeScript("testapp", "slow", "for (let i = 0; i < 6; i++) { await fetch('/api/slow/p' + i); spoor.progress(i + 1, 6); }\nreturn 'done';");
  const slow = await api("POST", "/sites/testapp/runs", { script: "slow" });
  await until("slow run to start", async () => (await api("GET", `/sites/testapp/runs/${slow.id}`)).meta.progress?.done >= 1);
  await api("POST", `/sites/testapp/runs/${slow.id}/pause`);
  await new Promise(r => setTimeout(r, 400));
  const pausedAt = (await api("GET", `/sites/testapp/runs/${slow.id}`)).meta.progress.done;
  await new Promise(r => setTimeout(r, 1200));
  const stillAt = (await api("GET", `/sites/testapp/runs/${slow.id}`)).meta;
  check("pause holds the run", stillAt.status === "paused" && stillAt.progress.done === pausedAt, stillAt);
  await api("POST", `/sites/testapp/runs/${slow.id}/resume`);
  const slowDone = await until("slow run to finish", async () => {
    const m = (await api("GET", `/sites/testapp/runs/${slow.id}`)).meta;
    return m.status === "ok" ? m : undefined;
  });
  check("resumed run finishes", slowDone.status === "ok", slowDone);

  const cliRun = await cli("run", "testapp", "logged-in");
  check("spoor run streams and prints the result", cliRun.status === 0 && cliRun.stdout.trim() === "true", cliRun.stdout + cliRun.stderr);
  const audit = store.readAudit("testapp", 1000) as { url: string; status?: number }[];
  check("audit log has the run requests", audit.some(a => a.url.includes("/api/slow/p5")), audit.length);
  const flowsCli = await cli("flows", `testapp:${rec.id}`, "--grep", "authorize");
  check("spoor flows reads the recording", flowsCli.status === 0 && flowsCli.stdout.includes("/authorize"), flowsCli.stdout + flowsCli.stderr);
  const killed = await api("POST", "/sites/testapp/stop");
  check("kill switch", killed.stopped);
} catch (e) {
  failures++;
  console.log(`  ✘ ${(e as Error).stack}`);
} finally {
  // Firefox first: while it runs, the extension would reconnect and start a new service.
  for (const c of children) {
    try {
      process.kill(-c.pid!, "SIGTERM");
    } catch {}
  }
  if (process.platform !== "win32") spawnSync("pkill", ["-f", join(HOME, "firefox")]);
  await new Promise(r => setTimeout(r, 1500));
  try {
    await api("POST", "/shutdown", undefined, { start: false });
  } catch {}
  await site.close();
  // Point the native host back at the normal home.
  spawnSync(process.execPath, ["--no-warnings=ExperimentalWarning", CLI, "install"], { env: { ...process.env, SPOOR_HOME: ORIGINAL_HOME ?? "" } });
  console.log(failures ? `\n${failures} failed (logs in ${HOME}/logs)` : "\nall passed");
  process.exit(failures ? 1 : 0);
}
