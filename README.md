# Spoor

Spoor is a personal workbench for building internal tools against websites
that have no API. It is not a product.

You log in to a site by hand in Spoor's Firefox. An agent (or you) writes
scripts that run inside that session. Spoor records traffic so the agent can
work out how the site behaves, and it enforces pacing and keeps an audit log.
You see and approve what happens in the browser itself: a **home panel** (a tab
that opens at start) and a **sidebar** next to every page.

The work produces one of two things:

1. **Jobs that run through Spoor.** The agent puts scripts in the site's
   library; scripts that declare their parameters are **actions**, callable
   by name from the home panel, the CLI, a web app (HTTP API) or an agent
   (MCP). Spoor starts its Firefox when a job needs it, uses the session you
   logged in with, and asks you to log in again when it has lapsed. Results,
   downloads and logs land in a run folder.
2. **A standalone webapp that doesn't need Spoor.** The agent reads Spoor's
   recordings (auth chain, cookies, token lifetimes, replay checks) and
   builds a client. If the login needs a real browser (SSO, MFA), the app uses
   the small [login harness](harness/README.md).

## How it works

```
Firefox Developer Edition (Spoor's own profile)
  Spoor extension (unsigned, sideloaded into the profile)
  ├─ home panel        extension page, opens at start
  ├─ sidebar           sidebar_action: runs, approvals, recording
  ├─ one container per site (its own cookie store = its own login)
  ├─ worker tab per site (hidden) where runs execute as content scripts
  ├─ downloads         spoor.download(): the download manager, inside the container
  ├─ webRequest        recording (bodies via filterResponseData), audit
  └─ native messaging ──► spoor-host ──► local service (WebSocket)

Local service (Node, TypeScript)        127.0.0.1:7517, bearer token
  ├─ HTTP API for the agent, CLI, MCP and web apps (the home panel uses it too)
  ├─ starts Spoor's Firefox on demand, keeps per-site login state
  └─ files in ~/.spoor/sites/<name>/: scripts, runs, recordings, dossier
```

Spoor uses no automation protocol: no CDP, no Marionette, no remote
debugging. To the site, it is an ordinary Firefox with an add-on. Scripts run
in the extension's content-script world, which the page can't see. They use
`content.fetch`, so their requests carry the page's own cookies and Origin.
Spoor's UI lives in the browser chrome, not in the page.

**Why Developer Edition:** release Firefox only keeps add-ons Mozilla signed.
Developer Edition (or Nightly) honours `xpinstall.signatures.required=false`,
so `spoor browser` packs `extension/dist` into the profile and it loads as a
normal add-on, no signing and no remote debugging. Pages can't tell the
channel; they see a Firefox one version ahead of release.

## Setup

Requirements: Node ≥ 24 (it runs the TypeScript directly) and
[Firefox Developer Edition](https://www.mozilla.org/firefox/developer/)
(or Nightly; `SPOOR_FIREFOX` points at another binary).

```bash
npm install
npm run build              # extension → extension/dist, harness → harness/dist
bin/spoor install          # register the native messaging host with Firefox
bin/spoor browser          # optional: Spoor's Firefox also starts by itself when needed
```

After changing the extension, `npm run build` and restart Spoor's Firefox.

## Using it

```bash
spoor site add cas https://cas.example.ch/ --check logged-in --min-gap-ms 1000 --keepalive-mins 30
spoor script add cas logged-in checks/logged-in.js
spoor login cas                    # Spoor brings the site forward; you log in; it waits for the check
spoor session cas                  # logged_in, last check, cookie summary
```

`site add` on an existing site updates it (fields you leave out keep their value).

### Sessions

Each site's login lives in its container in Spoor's profile, and survives
restarts: Spoor's Firefox restores its last session, which keeps session
cookies (most logins are those).

- **Check script** (`--check`): a script that returns true when logged in.
  Spoor records its verdict in `session.json`.
- **Login** (`spoor login`, `POST /sites/{site}/login`, MCP `spoor_login`):
  brings a tab of the site to the front, sends a notification, and waits until
  the check passes. It only checks once that tab is back on the site's own
  origin, so it doesn't start a second SSO flow next to yours.
- **Keepalive** (`--keepalive-mins`): while Spoor's Firefox runs, the check
  runs on that schedule. That keeps the server session warm and tells you (a
  notification) when the login lapsed.
- **Clearing is deliberate**: `spoor session clear <site> --yes` (home panel:
  *Clear session…*, MCP: `spoor_session_clear` with `confirm: true`) removes
  every cookie and all storage in the site's container; `--cache` also clears
  Firefox's HTTP cache, which all sites share. Audited.
- `spoor session cookies <site>` prints the container's cookies (audited).

### Scripts

**Scripts** are the body of an async function. These are in scope:

| | |
|---|---|
| `args` | the run's arguments (`--arg key=value`, or `--args '{…}'` / `@file.json`) |
| `fetch` | the page's own fetch: page cookies and Origin; paced, pausable |
| `console.*` | streamed live to the run log, the sidebar and `spoor run` |
| `spoor.progress(done, total?, label?)` | progress bar in the sidebar |
| `await spoor.confirm(message, detail?)` | pauses until you click Approve or Reject in the sidebar; returns `true` or `false` |
| `spoor.save(name, data)` | writes a file into the run folder (string, JSON or bytes); → `{file, bytes, abs}` |
| `await spoor.download(url, path \| {path?, dir?, referer?, method?, headers?, body?})` | Firefox's download manager fetches it with the site's cookies, straight to disk, into the run's `files/` (named by `path`, or by the server's file name inside `dir`); → `{path, file, abs, bytes, mime}`. `referer` sets the Referer header, which the download manager doesn't send by default (signed CDN links often check it). Use it for anything big |
| `await spoor.html(url, init?)` | fetch and parse → `Document` |
| `await spoor.tab(url)` | another hidden tab in the site's container, for another origin (a video host, an SSO-protected API): `{fetch, html, eval(code \| fn, args), urls(), close}`; `urls()` lists the pages it committed, so you can see where a link led. Its `fetch` returns `{status, ok, headers.get, text(), json(), bytes()}` |
| `spoor.state.get(key)`, `.set(key, value)`, `.all()` | JSON this script keeps between runs (e.g. what it already downloaded) |
| `spoor.sleep(ms)`, `spoor.checkpoint()` | waits that respect Pause and Stop |

```js
// batch-add: adds DMARC records
const csrf = document.querySelector('meta[name=csrf]').content;
const zones = await (await fetch('/api/zones')).json();
await spoor.save('backup.json', zones);
if (!await spoor.confirm(`Add ${args.records.length} records?`, args.records)) return 'cancelled';
for (const [i, rec] of args.records.entries()) {
  await fetch('/api/records', { method: 'POST', headers: { 'x-csrf-token': csrf }, body: JSON.stringify(rec) });
  spoor.progress(i + 1, args.records.length, rec.zone);
}
```

```bash
spoor run cas batch-add --args @records.json   # streams logs; result on stdout
spoor exec cas -e "return document.title"      # one-off, inline
spoor runs cas                                 # history; spoor runs cas <id> for details
spoor stop cas                                 # kill switch
```

Each run gets a folder `~/.spoor/sites/<site>/runs/<id>/` with `run.json`,
`events.jsonl`, `script.js`, `result.json`, whatever the script saved, and
`files/` for downloads. Runs on a site queue up one after another, because
each site has one worker tab. Don't navigate the page inside a script
(`location = …`); use `fetch`.

### Actions

A script becomes an **action** when it starts with a `spoor-action` block:

```js
/* spoor-action
{
  "description": "Download this week's course material",
  "params": {
    "course": { "type": "string", "description": "ILIAS ref id" },
    "week":   { "type": "integer" }
  },
  "required": ["course"],
  "writes": false
}
*/
const doc = await spoor.html(`/ilias/goto.php/crs/${args.course}`);
…
```

- Arguments are checked against `params` before anything runs (types,
  `required`, `enum`, `default`; unknown arguments are refused).
- If the site has a check script, the action checks the login first. When it
  has lapsed, you get a notification and the call fails with
  `code: "login_required"`, or with `login: "wait"` it waits for you to log in.
- `writes: true` is a label (home panel, MCP hints). Writes still go through
  `spoor.confirm`, which only a click in the browser answers.
- Actions are just scripts. Recordings are separate: data gathering only.

```bash
spoor actions [site]                               # list, with parameters
spoor act cas batch-add --arg course=123 --login-wait
```

### MCP

`spoor mcp` is an MCP server on stdio. Register it with your agent, e.g.
`claude mcp add spoor -- /path/to/spoor-poc/bin/spoor mcp`. Tools:

| tool | |
|---|---|
| `<site>__<action>` | one per action, input schema from its params |
| `spoor_sites`, `spoor_session` | sites and login state |
| `spoor_login` | ask the human to log in, wait for it |
| `spoor_session_clear` | needs `confirm: true` |
| `spoor_exec` | run code in a site's session (exploring, developing scripts) |
| `spoor_run_status`, `spoor_run_stop` | follow long runs (calls wait 50 s, then hand back the run id) |

### Web apps

A web app that controls Spoor (e.g. "download this week's material") is a
small server of its own that calls the HTTP API below with the token from
`~/.spoor/serve.json`: list actions, start one, follow its events, serve its
files. The API doesn't send CORS headers, so a page can't call it directly.

## Figuring a site out

```bash
spoor record start cas          # opens the site; use it, mark steps in the sidebar
spoor record stop cas           # flows + cookies + localStorage/sessionStorage snapshot
spoor flows cas:latest --grep Zeiterfassung
spoor trace cas:latest 4242     # where did this value appear, and where was it sent?
spoor auth cas:latest           # redirect chain across hosts, cookies set, OAuth/SAML params
spoor lifetimes cas:latest      # cookie expiry, JWT claims (exp, aud, iss …)
spoor endpoints cas:latest      # API-ish requests grouped by path template (and cmd=… style dispatch)
spoor replay cas:latest 57 --minimize   # does it work from plain HTTP? which cookies/headers are needed?
spoor dossier cas               # writes all of the above to sites/cas/dossier/
```

To record a login, clear the site's session first (`spoor session clear cas
--yes`), then record.

A recording captures every request in the site's container, across every
host it touches. A Microsoft SSO detour counts, because it happens in the same
container. For each request it keeps the request and response headers
(including `Cookie` and each `Set-Cookie`), the redirect targets and the
bodies. Images, fonts, scripts and stylesheets are left out; a response that
became a download is marked as such. Request-body fields that look like
passwords or one-time codes are replaced with `[redacted by spoor]`. Tokens
the site hands out are kept, because auth analysis needs them.

Spoor doesn't interpret any of this. It lays out the evidence, and the agent
decides: "there's an API", "a plain form login is enough", or "a harness is
needed".

`replay` sends real requests to the site, paced like runs. It only replays
GET/HEAD unless you pass `--allow-write`. `--live` uses the browser's current
cookies instead of the recorded ones.

## API

Every request needs `Authorization: Bearer <token>`. The URL and token are in
`~/.spoor/serve.json`. Errors are `{error, code?}`.

```
GET  /health  /state  /sites  /sites/{site}  /recordings
POST /sites                                {name, url, check?, min_gap_ms?, keepalive_mins?}
GET|PUT /sites/{site}/notes                {text}
GET  /sites/{site}/status                  → browser state, logged_in (check script)
GET  /sites/{site}/session[?check=0]       → state, checked_at, cookie summary
POST /sites/{site}/login                   {timeout_secs?, wait?}
POST /sites/{site}/session/clear           {cache?}
GET  /sites/{site}/session/cookies
POST /sites/{site}/open                    {url?, wait?, timeout_secs?}
GET|PUT|DELETE /sites/{site}/scripts/{name}      {code}
GET  /actions   /sites/{site}/actions
POST /sites/{site}/actions/{name}          {args, wait?: secs|true, login?: "wait", timeout_secs?}
POST /sites/{site}/runs                    {script | code, args?, url?, timeout_secs?, wait?}
POST /sites/{site}/exec                    same, waits; → {ok, result, error, logs, requests}
GET  /sites/{site}/runs[/{id}[/events?after=&wait_ms=]]   long-poll for live events
GET  /sites/{site}/runs/{id}/files/{name}  name URL-encoded, e.g. files%2FWeek%201%2Fa.pdf
POST /sites/{site}/runs/{id}/pause|resume|stop
POST /sites/{site}/record/start|stop|mark  {url?, open?} / {label}
GET  /recordings/{rec}/flows?grep=&full=1  /flows/{seq}  /trace?value=  /auth  /lifetimes  /endpoints
POST /recordings/{rec}/replay              {seq, live?, minimize?, allow_write?}
POST /sites/{site}/dossier                 {recording?}
POST /sites/{site}/stop   /stop   /shutdown
```

Anything that needs the browser starts it (in the background) if it isn't
running. Approvals (`spoor.confirm`) are deliberately missing from the API.
Only a click in the browser answers them.

## Files

```
~/.spoor/                      (SPOOR_HOME)
  sites.json                   registry
  serve.json                   service url + token
  firefox/                     Spoor's Firefox profile
  downloads/                   Firefox's download folder (spoor.download passes through)
  logs/                        service.log, host.log
  sites/<name>/
    notes.md                   shared notes (home panel ↔ agent)
    session.json               last known login state
    scripts/  runs/  recordings/  dossier/  state/  audit.jsonl
```

## Development

```bash
npm run typecheck && npm test   # unit tests
npm run e2e                     # headless Developer Edition + extension + service against a fake SSO site
npm run e2e:harness             # login harness against the same fake site (npx playwright install firefox)
```

The e2e test re-points the native messaging host at a temporary
`SPOOR_HOME`, and points it back at `~/.spoor` when it finishes. It covers
recording, runs, downloads, `spoor.tab`, actions, MCP, a Firefox restart
(session restore, on-demand start), clearing a session and logging in again.

| Path | What |
|---|---|
| `extension/src/background.ts` | containers, worker tabs, runs, downloads, sessions, recording, bridge |
| `extension/src/wrapper.ts` | the `spoor` API scripts see |
| `extension/src/{home,sidebar}.ts` | the two UIs |
| `src/service/` | HTTP API, run manager, sessions, actions, file store, inspect/replay/dossier |
| `src/host/main.ts` | native messaging host (stdio ↔ service WebSocket) |
| `src/cli/` | CLI, MCP server, Firefox launcher, native host installer |
| `harness/` | standalone `loginHarness` package |

## Known gaps

- **Not yet tried on Windows.** The native host is registered through
  `HKCU\Software\Mozilla\NativeMessagingHosts`, and the launcher is a `.cmd`
  file. Both are written but haven't run on Windows.
- **Not yet tried on a real SSO site end to end.** HSLU (Keycloak → SWITCH
  edu-ID, ILIAS, Zoom) is the first one; see `PLAN.md`.
- **If Spoor's Firefox is open but not connected** (native host broken), an
  on-demand start runs into Firefox's "already running" message.
- **WebSocket frames are not recorded.** Firefox's webRequest API doesn't
  expose them.
- **Only `fetch` is paced.** `XMLHttpRequest` calls a script makes are audited
  but not paced.
- **`spoor.tab` can't script built-in viewers** (PDFs open in pdf.js). Open it
  on an HTML page of that origin and `fetch` from there.
