# Plan: Spoor as a Firefox extension workbench

**Status:** not started — spike first  
**Branch:** `v0.6-session-runtime` (current Chromium/CDP codebase, 4 local commits)  
**Context:** [discussion.md](discussion.md) has the full conversation that led here

---

## What Spoor is

Spoor is a personal tool for building internal webapps that talk to sites
with no API. It is **not** a product.

You log in to a site by hand in Spoor's browser. An agent (or you) writes
scripts that run in that session. Spoor records traffic so the agent can
figure out how a site works, and it enforces pacing and an audit log.

The output of using Spoor is either:
- a batch job that runs through Spoor (use case 1), or
- a standalone webapp that doesn't need Spoor at all (use case 2).

## Why rebuild

The current stack (Chromium + CDP in Rust) has three problems:
1. **Detectable.** CDP's `Runtime.enable` is the most precise bot signal.
   Hardening means forking chromiumoxide.
2. **No UI for the human.** The Hostpoint run proved it: the user saw
   windows flash and had to trust chat. No way to see, approve or intervene.
3. **Chrome 137 removed `--load-extension`** for branded builds, making
   extensions hard on Chrome.

Firefox with its own extension solves all three: no automation protocol
(nothing to detect), the extension provides home panel + sidebar (UI inside
the browser), and Firefox keeps blocking webRequest in MV3 (the powerful
APIs we need).

## Architecture

```
┌─────────────────────────────────────────────────┐
│  Firefox (regular release, per-site profile)    │
│                                                 │
│  Extension (unlisted, Mozilla-signed)           │
│  ├─ Home panel    (extension page, tab)         │
│  ├─ Sidebar       (sidebar_action, every page)  │
│  ├─ content.fetch (page cookies, invisible)     │
│  ├─ webRequest    (pacing, recording bodies)    │
│  └─ native messaging ←→ local service (stdio)   │
└─────────────────────────────────────────────────┘
        │ stdio (native messaging)
┌───────┴─────────────────────────────────────────┐
│  Local service (TypeScript, single executable)  │
│  ├─ HTTP API  127.0.0.1:7517  (agent / CLI)    │
│  ├─ File store   sites/<name>/                  │
│  │   ├─ scripts/                                │
│  │   ├─ runs/<id>/  (result, logs, backups)     │
│  │   ├─ recordings/                             │
│  │   └─ dossier/  (auth flow, endpoints, client)│
│  └─ CLI  (spoor site / exec / record / …)       │
└─────────────────────────────────────────────────┘
```

**Language:** TypeScript for everything (extension, service, CLI). One
language, shared types, faster iteration. The service ships as a single
executable (Bun compile or pkg). Rust leaves the stack.

**Per-site isolation:** separate Firefox profiles (one per site), or
Firefox containers (separate cookie stores in one window). The spike
decides which.

## Use cases

### 1. Batch job interface (the Hostpoint case)

Agent writes scripts → user sees/approves/runs them in Spoor.

What Spoor needs:
- Script library per site (agent stores, user sees in home panel)
- Runs with live progress in the sidebar, Pause / Stop
- `spoor.confirm("Save 48 records?", plan)` — script pauses until click
- Run folders with results, backups, logs (agent and user both see)
- `spoor.save("backup", data)` from inside scripts

### 2. Discovery tool → standalone webapp

Agent uses Spoor recordings to understand a site, then builds a client
that works without Spoor.

What Spoor needs beyond the core:
- Recording includes **cookies, Set-Cookie, redirects, browser storage**
  (today: no cookies at all)
- Auth chain across domains (e.g. CAS → login.microsoftonline.com → back)
- Lifetimes: cookie expiry, token `exp`, when re-login happens
- Protocol hints: OIDC / SAML, `client_id`, `redirect_uri`
- **Replayability check:** does a request work outside the browser? With
  which cookies/headers? How long?
- **Site dossier** output: auth flow, endpoint catalog, replayability
  results, generated client module

The agent draws conclusions from the evidence; Spoor doesn't interpret.

### The harness pattern (output of use case 2)

When a site needs browser login (SSO, MFA, device-bound sessions), the
output is a small standalone library:

```ts
const session = await loginHarness({
  start: "https://app.example.com/",
  profile: "mysite",           // persistent → often silent next time
  done: s => s.cookies.has("SESSION_ID"),
  capture: { cookies: ["app.example.com"], storage: ["authToken"] },
});
// browser closes; app uses session.cookies for plain HTTP
```

Built on Playwright (not Spoor) so the webapp is independent. The
`start`/`done`/`capture` config is what the agent figures out from Spoor's
recording. Like AWS SSO login: spawn window, wait, capture, close.

## Phases

### Phase 0: Spike (1–2 days)

Validate the Firefox extension approach on a real site. Pick whichever
site exercises the most: SSO redirects, short-lived sessions and sidebar
UX are harder than a simple cookie login, so prefer a site that has them.

Pass/fail criteria:

- [ ] Mozilla signs an unlisted add-on (automatic, <15 min)
- [ ] `content.fetch()` in MV3 sends the page's cookies and origin
- [ ] Blocking `webRequest` + `filterResponseData` work for pacing and
      recording bodies
- [ ] Native messaging on Windows: registry entry, stdio communication
- [ ] Per-site profiles or containers: which works, what the UX is
- [ ] On the chosen site: login works, sidebar visible, record one action
      with bodies, run one script with pacing

### Phase 1: Core (≈2 weeks)

Replace the CDP stack with Firefox + extension + local service.

- Firefox profile management (create, launch, per site)
- Extension: background script, content script, native messaging
- Home panel (extension page) — site list, status, launch
- Sidebar (sidebar_action) — current site, recording, progress
- `content.fetch()` execution with pacing
- webRequest recording with full bodies and cookies
- Local service: HTTP API, file store, native messaging host
- CLI: `spoor site add/list`, `spoor exec`, `spoor record`, `spoor flows`,
  `spoor trace`
- Audit log (same as today)

Carry forward from the current codebase (logic, not Rust code):
- `inspect.rs` → `trace` and `matching_flows` (port to TS)
- `capture/model.rs` → CaptureRecord schema
- Site registry concept (sites.toml or equivalent)
- Pacing logic (min_gap_ms)

Delete: all Chromium/CDP code, chromiumoxide dependency, spoor-app (Tauri).

### Phase 2: Use case 1 — scripts and runs (≈1 week)

- Per-site script library (stored in `sites/<name>/scripts/`)
- `spoor.confirm(message, detail)` — sidebar prompt, blocks script
- `spoor.save(name, data)` — writes to the run folder
- Run folders: `sites/<name>/runs/<id>/` with result, logs, backups
- Live progress: console output + custom progress streamed to sidebar
- `spoor run <site> <script> --arg key=value` (better than JSON quoting)
- Home panel: run history, re-run, view results

### Phase 3: Use case 2 — discovery and dossier (≈2 weeks)

- Recording captures cookies, Set-Cookie, redirects across domains
- Recording captures browser storage snapshots (localStorage, sessionStorage)
- Auth-chain view: the full redirect sequence with tokens and cookies
- Lifetime extraction: cookie expiry, token claims
- Replayability check: replay a recorded request with only cookies+headers,
  report what's needed and what breaks
- Site dossier: structured output per site (auth flow, endpoints, replay
  results)
- Credential redaction in recordings (passwords, tokens in bodies)

### Phase 4: Harness package (≈3 days)

- Small standalone library (TypeScript, uses Playwright)
- `loginHarness({ start, profile, done, capture })` API
- Persistent Firefox profile for silent re-login
- Cookie and storage capture on `done` condition
- Published as a local package the user's webapps import

### Later / if needed

- Desktop notifications for login-needed and approval-waiting (native
  messaging can trigger system notifications)
- Browser worker on a VM (same extension, remote service) for always-on
- Auto-relogin: check script detects expired session, runs relogin script,
  notifies only if MFA is needed
- Generated client modules from the dossier (TypeScript client per site)

## What stays from the current codebase

| Current | Keeps | Form |
|---------|-------|------|
| Session model (one profile per site, login by hand) | yes | same concept |
| `exec` (run script in page context) | yes | `content.fetch()` instead of CDP |
| Recording + `flows` + `trace` | yes | port to TS, add cookies |
| Pacing (min_gap_ms, enforced below the script) | yes | webRequest instead of CDP Fetch |
| Audit log (per-site JSONL) | yes | same |
| Site registry (sites.toml) | yes | same or JSON |
| Chromium download + CDP launch | no | Firefox profiles, no protocol |
| chromiumoxide, all CDP code | no | extension APIs |
| spoor-app (Tauri desktop app) | no | extension home panel + sidebar |
| Rust code | no | TypeScript |

## Decisions still open

1. **Per-site profiles vs. Firefox containers** — spike decides
2. **Service runtime:** Bun vs. Node — Bun preferred (single binary, fast)
3. **Extension distribution:** unlisted signing is expected to work; if
   Mozilla changes policy, self-signed with `about:config` override is the
   fallback (internal tool, own machines only)

## Effort estimate

| Phase | Effort | Cumulative |
|-------|--------|------------|
| 0. Spike | 1–2 days | 1–2 days |
| 1. Core | ~2 weeks | ~2.5 weeks |
| 2. Scripts & runs | ~1 week | ~3.5 weeks |
| 3. Discovery & dossier | ~2 weeks | ~5.5 weeks |
| 4. Harness package | ~3 days | ~6 weeks |

Phase 0 is the gate. If the spike fails on a critical point (e.g.
`content.fetch()` doesn't send cookies, or native messaging is broken on
Windows), we re-evaluate before committing to the rebuild.
