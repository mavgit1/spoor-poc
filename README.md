# Spoor

Spoor is a personal workbench for building internal tools against websites
that have no API. It is not a product.

You log in to a site by hand in Spoor's Firefox. An agent (or you) writes
scripts that run inside that session. Spoor records traffic so the agent can
work out how the site behaves, and it enforces pacing and keeps an audit log.
You see and approve what happens in the browser itself: a **home panel** (a tab
that opens at start) and a **sidebar** next to every page.

The work produces one of two things:

1. **A batch job that runs through Spoor.** The agent puts a script in the
   site's library. You run it, watch its progress and approve writes in the
   sidebar. Results, backups and logs land in a run folder.
2. **A standalone webapp that doesn't need Spoor.** The agent reads Spoor's
   recordings (auth chain, cookies, token lifetimes, replay checks) and
   builds a client. If the login needs a real browser (SSO, MFA), the app uses
   the small [login harness](harness/README.md).

## How it works

```
Firefox (regular release, Spoor's own profile)
  Spoor extension
  ├─ home panel        extension page, opens at start
  ├─ sidebar           sidebar_action: runs, approvals, recording
  ├─ one container per site (its own cookie store = its own login)
  ├─ worker tab per site (hidden) where runs execute as content scripts
  ├─ webRequest        recording (bodies via filterResponseData), audit
  └─ native messaging ──► spoor-host ──► local service (WebSocket)

Local service (Node, TypeScript)        127.0.0.1:7517, bearer token
  ├─ HTTP API for the agent and CLI (the home panel uses the same API)
  └─ files in ~/.spoor/sites/<name>/: scripts, runs, recordings, dossier
```

Spoor uses no automation protocol: no CDP, no Marionette, no remote
debugging. To the site, it is an ordinary Firefox with an add-on. Scripts run
in the extension's content-script world, which the page can't see. They use
`content.fetch`, so their requests carry the page's own cookies and Origin.
Spoor's UI lives in the browser chrome, not in the page.

## Setup

Requirements: Node ≥ 24 (it runs the TypeScript directly) and Firefox.

```bash
npm install
npm run build              # extension → extension/dist, harness → harness/dist
bin/spoor install          # register the native messaging host with Firefox
```

Then start Spoor's Firefox, in one of two ways:

- **Day-to-day use:** sign the extension once as an *unlisted* add-on. Mozilla
  signs it automatically within minutes and nothing is published. This needs
  [AMO API keys](https://addons.mozilla.org/developers/addon/api/key/).

  ```bash
  AMO_JWT_ISSUER=… AMO_JWT_SECRET=… bin/spoor sign   # → ~/.spoor/spoor.xpi
  bin/spoor browser
  ```

- **Developing Spoor:** `bin/spoor browser --dev` loads `extension/dist` as a
  temporary add-on through web-ext. web-ext installs it over Firefox's remote
  debugging, so don't use this mode on sites you care about.

The service starts on its own when the browser or the CLI needs it.

## Using it

```bash
spoor site add cas https://cas.example.ch/ --check logged-in --min-gap-ms 1000
spoor open cas                     # log in by hand, in the site's container
spoor script add cas logged-in checks/logged-in.js
spoor status cas                   # runs the check script → logged_in: true
```

**Scripts** are the body of an async function. These are in scope:

| | |
|---|---|
| `args` | the run's arguments (`--arg key=value`, or `--args '{…}'` / `@file.json`) |
| `fetch` | the page's own fetch: page cookies and Origin; paced, pausable |
| `console.*` | streamed live to the run log, the sidebar and `spoor run` |
| `spoor.progress(done, total?, label?)` | progress bar in the sidebar |
| `await spoor.confirm(message, detail?)` | pauses until you click Approve or Reject in the sidebar; returns `true` or `false` |
| `spoor.save(name, data)` | writes a file into the run folder (string, JSON or bytes) |
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
`events.jsonl`, `script.js`, `result.json` and whatever the script saved.
Runs on a site queue up one after another, because each site has one worker
tab. Don't navigate the page inside a script (`location = …`); use `fetch`.

## Figuring a site out

```bash
spoor record start cas          # opens the site; use it, mark steps in the sidebar
spoor record stop cas           # flows + cookies + localStorage/sessionStorage snapshot
spoor flows cas:latest --grep Zeiterfassung
spoor trace cas:latest 4242     # where did this value appear, and where was it sent?
spoor auth cas:latest           # redirect chain across hosts, cookies set, OAuth/SAML params
spoor lifetimes cas:latest      # cookie expiry, JWT claims (exp, aud, iss …)
spoor endpoints cas:latest      # API-ish requests grouped by path template
spoor replay cas:latest 57 --minimize   # does it work from plain HTTP? which cookies/headers are needed?
spoor dossier cas               # writes all of the above to sites/cas/dossier/
```

A recording captures every request in the site's container, across every
host it touches. A Microsoft SSO detour counts, because it happens in the same
container. For each request it keeps the request and response headers
(including `Cookie` and each `Set-Cookie`), the redirect targets and the
bodies. Images, fonts, scripts and stylesheets are left out. Request-body
fields that look like passwords or one-time codes are replaced with
`[redacted by spoor]`. Tokens the site hands out are kept, because auth
analysis needs them.

Spoor doesn't interpret any of this. It lays out the evidence, and the agent
decides: "there's an API", "a plain form login is enough", or "a harness is
needed".

`replay` sends real requests to the site, paced like runs. It only replays
GET/HEAD unless you pass `--allow-write`. `--live` uses the browser's current
cookies instead of the recorded ones.

## API

Every request needs `Authorization: Bearer <token>`. The URL and token are in
`~/.spoor/serve.json`.

```
GET  /health  /state  /sites  /sites/{site}  /recordings
POST /sites                                {name, url, check?, min_gap_ms?}
GET|PUT /sites/{site}/notes                {text}
GET  /sites/{site}/status                  → browser state, logged_in (check script)
POST /sites/{site}/open                    {url?, wait?, timeout_secs?}
GET|PUT|DELETE /sites/{site}/scripts/{name}      {code}
POST /sites/{site}/runs                    {script | code, args?, url?, timeout_secs?, wait?}
POST /sites/{site}/exec                    same, waits; → {ok, result, error, logs, requests}
GET  /sites/{site}/runs[/{id}[/events?after=&wait_ms=]]   long-poll for live events
GET  /sites/{site}/runs/{id}/files/{name}
POST /sites/{site}/runs/{id}/pause|resume|stop
POST /sites/{site}/record/start|stop|mark  {url?, open?} / {label}
GET  /recordings/{rec}/flows?grep=&full=1  /flows/{seq}  /trace?value=  /auth  /lifetimes  /endpoints
POST /recordings/{rec}/replay              {seq, live?, minimize?, allow_write?}
POST /sites/{site}/dossier                 {recording?}
POST /sites/{site}/stop   /stop   /shutdown
```

Approvals (`spoor.confirm`) are deliberately missing from the API. Only a
click in the browser answers them.

## Files

```
~/.spoor/                      (SPOOR_HOME)
  sites.json                   registry
  serve.json                   service url + token
  firefox/                     Spoor's Firefox profile
  logs/                        service.log, host.log
  sites/<name>/
    notes.md                   shared notes (home panel ↔ agent)
    scripts/  runs/  recordings/  dossier/  audit.jsonl
```

## Development

```bash
npm run typecheck && npm test   # unit tests
npm run e2e                     # real headless Firefox + extension + service against a fake SSO site
npm run e2e:harness             # login harness against the same fake site (npx playwright install firefox)
```

The e2e test re-points the native messaging host at a temporary
`SPOOR_HOME`, and points it back at `~/.spoor` when it finishes.

| Path | What |
|---|---|
| `extension/src/background.ts` | containers, worker tabs, runs, pacing, recording, bridge |
| `extension/src/{home,sidebar}.ts` | the two UIs |
| `src/service/` | HTTP API, run manager, file store, inspect/replay/dossier |
| `src/host/main.ts` | native messaging host (stdio ↔ service WebSocket) |
| `src/cli/` | CLI, Firefox launcher, native host installer |
| `harness/` | standalone `loginHarness` package |

## Known gaps

- **Not yet tried on Windows.** The native host is registered through
  `HKCU\Software\Mozilla\NativeMessagingHosts`, and the launcher is a `.cmd`
  file. Both are written but haven't run on Windows.
- **Not yet tried on a real SSO site.** The e2e test covers an OAuth-style
  redirect across two hosts, but not Microsoft's login.
- **WebSocket frames are not recorded.** Firefox's webRequest API doesn't
  expose them.
- **Only `fetch` is paced.** `XMLHttpRequest` calls a script makes are audited
  but not paced.
