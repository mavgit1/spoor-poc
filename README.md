# Spoor

Spoor keeps **one logged-in browser session per website** and lets scripts
run inside it. It's for sites with no API: automating DNS entries at a
hosting provider, filing time reports, pulling data out of a slow panel.

You log in once, by hand, in Spoor's browser window. From then on, a script,
an agent, or an integration you build (an MCP server, a cron job) runs
JavaScript inside that session. Because requests come from the real browser
tab, cookies, SSO, CSRF tokens and the site's own token refresh all behave
exactly as they do for you.

Spoor does not try to understand sites. It has no auth detection and no API
classification. Site knowledge lives in the scripts. Spoor provides the
session, a way to run code in it, recordings to learn from, and two
safeguards: pacing and an audit log.

## The model

| | |
|---|---|
| **Session** | A persistent Chromium profile per site (`profiles/<site>/`). The profile *is* the session; Spoor never reads or refreshes cookies. |
| **exec** | Runs a script (an async function body) in a tab of that browser, in its own minimized window, and returns its result as JSON. |
| **open** | Shows the site's window so a human can log in again or fix something. `--wait` blocks until the site's check script passes. |
| **record** | Captures traffic while you use the site, into files you can grep or `spoor trace`. |
| **status** | Runs the site's `check` script and reports `logged_in: true/false`. |
| **stop** | Kill switch: closes the site's browser and everything running on it. |

Spoor enforces two things itself, because they need no knowledge of the site:

- **Pacing**: a minimum gap (`min_gap_ms`, default 1000) between requests an
  exec makes, enforced below the script with CDP request interception, so
  parallel `fetch`es are paced too.
- **Audit log**: every request an exec makes, in `audit/<site>.jsonl`.

Whether a request reads or writes is not Spoor's call. That depends on the
site's API design (a Hostpoint DNS *read* is a POST), so it stays with the
script and whoever runs it.

## Quick start

```bash
cargo build --release -p spoor          # binary: target/release/spoor

spoor site add hostpoint https://admin.hostpoint.ch/customer/Index \
    --check examples/hostpoint/check.js --min-gap-ms 1500
spoor open hostpoint                    # log in in the window that appears
spoor status hostpoint                  # → "logged_in": true
spoor exec hostpoint -e "return document.title"
spoor exec hostpoint examples/hostpoint/export-dns.js --timeout 1800 > dns.json
```

The first command that needs the browser starts `spoor serve` in the
background, and downloads the pinned Chromium on first use. `spoor shutdown`
stops the service.

## Writing scripts

A script is the **body of an async function** with `args` in scope. Return
anything JSON-able:

```js
const csrf = document.querySelector('input[name="csrf_token"]').value;
const r = await fetch('/api/items?page=' + args.page, { headers: { 'X-CSRF-Token': csrf } });
if (r.redirected && r.url.includes('/login')) throw new Error('logged_out');
return await r.json();
```

- The tab first loads the site's url (or `--url`), then runs the script, so
  the page DOM, cookies and storage are available.
- `console.*` output is returned in `logs` (the CLI prints it to stderr).
- A thrown error returns `ok: false` with the message. The CLI exits 1.
- Don't navigate the tab (`location = …`). That loses the script's state. Use `fetch`.
- `--args '{"page":2}'` or `--args @args.json` sets `args`.
- `--timeout` defaults to 300s. Long exports are fine: nothing on the CDP
  side waits for the script, Spoor polls for the result.

## Figuring a site out

```bash
spoor record cas            # use the site; press Enter to stop
spoor sessions              # recordings, newest first
spoor flows <id>            # seq METHOD status type url — one line per request
spoor flows <id> --grep Zeiterfassung --full   # matching flows as JSON lines
spoor trace <id> 805415     # where did this value first appear, and where was it sent?
```

`trace` finds values that come from HTML: an id in a hidden `<input>`, a
CSRF token in a meta tag. Knowing where a value comes from is usually what
you need to reproduce a request. Recordings are plain `flows.jsonl.gz` plus
`meta.json` under `sessions/<id>/`. Cookies are not captured.

## Building an integration

An integration is ordinary code that calls the local API. It never sees
passwords or cookies.

```
GET  /health
GET  /sites
GET  /sites/{site}/status                 → {running, recording, logged_in, error}
POST /sites/{site}/open                   {url?, wait?, timeout_secs?}
POST /sites/{site}/exec                   {script, args?, url?, timeout_secs?}
                                          → {ok, result, error, logs, requests, duration_ms}
POST /sites/{site}/record/start           {url?}
POST /sites/{site}/record/stop
POST /sites/{site}/stop                   kill switch, one site
POST /stop                                kill switch, all sites
POST /shutdown                            stop all sites and exit
```

The service binds `127.0.0.1` only. Every request needs
`Authorization: Bearer <token>`; `url` and `token` are in `serve.json`:

```python
import json, os, pathlib, urllib.request

# Spoor's directory — see "Files" below for each OS.
home = pathlib.Path(os.environ["LOCALAPPDATA"]) / "spoor"
serve = json.loads((home / "serve.json").read_text())

def exec_(site, script, args=None):
    req = urllib.request.Request(
        f"{serve['url']}/sites/{site}/exec",
        data=json.dumps({"script": script, "args": args or {}}).encode(),
        headers={"Authorization": f"Bearer {serve['token']}", "Content-Type": "application/json"},
    )
    out = json.load(urllib.request.urlopen(req))
    if not out["ok"]:
        raise RuntimeError(out["error"])
    return out["result"]
```

When a script reports it was logged out, call `POST /sites/{site}/open`
with `{"wait": true}`. The window pops up for the human, and the call
returns once the check script passes.

## Files

Everything lives in one directory: `%LOCALAPPDATA%\spoor` on Windows,
`~/Library/Caches/spoor` on macOS, `~/.cache/spoor` on Linux. Override it
with `SPOOR_CACHE_DIR`.

```
sites.toml          site registry (hand-editable; re-read on every call)
serve.json          url + token of the running service
profiles/<site>/    browser profile = the session
sessions/<id>/      recordings
audit/<site>.jsonl  requests made by execs
logs/spoor.log      process log
```

`sites.toml`:

```toml
[sites.cas]
url = "https://cas.example.ch/"
check = "C:/work/cas/logged-in.js"   # optional
min_gap_ms = 1000                    # optional
```

## Browser

Spoor downloads one pinned Chromium build on first use, the same revision
on every machine (it lives in `…/spoor/chromium`, or `SPOOR_CHROMIUM_DIR`).
`SPOOR_CHROME=<path>` uses another Chromium-based browser instead. It runs
headed, without `--enable-automation` (so no `navigator.webdriver`) and
with `AutomationControlled` disabled. Edge does not work as `SPOOR_CHROME`:
it never reports its DevTools port on stderr, where the launcher looks for it.

## Desktop app

`spoor-app/` is a tray app. It runs the same service in-process, or attaches
to a running `spoor serve`, and shows your sites with Open / Record / Check /
Close buttons. See [spoor-app/README.md](spoor-app/README.md).

## Development

```bash
cd spoor
cargo test                                              # unit tests
cargo test -- --ignored --nocapture --test-threads=1    # live browser tests (opens windows)
cargo clippy --all-targets -- -D warnings
```

Known gap: a tab that navigates the instant it is created (an OAuth
`window.open` popup) loses its first requests in a recording. See
`tests/smoke_capture.rs`.
