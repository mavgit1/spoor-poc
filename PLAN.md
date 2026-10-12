# Plan

**Status (2026-10-12):** v0.8 on `main`. The rebuild from
Chromium/CDP/Rust to a Firefox extension, a local service and a CLI is done
(history: [discussion.md](discussion.md), git log). This file holds what's
next.

## Where Spoor stands

- Firefox Developer Edition with the unsigned extension sideloaded into
  Spoor's profile. No signing, no remote debugging, no temporary add-ons.
- Sessions per site in containers. They survive restarts (session restore).
  The login state comes from the check script, with optional keepalive.
  Logging in and clearing a session are explicit, audited actions.
- The service starts Firefox on demand (minimized) when a run, a login or a
  recording needs it.
- Scripts can download files straight to disk with the site's cookies
  (`spoor.download`), work across origins in the same container
  (`spoor.tab`), parse pages (`spoor.html`) and keep state between runs
  (`spoor.state`).
- Actions are scripts with a declared parameter schema, callable from the
  home panel, the CLI (`spoor act`), the HTTP API and MCP (`spoor mcp`).
- Recordings stay what they were: data gathering only.
- `npm run e2e` covers all of the above against a fake SSO site.

## Next: first real site, HSLU course material

The task, and what one recording already showed, is in the site's notes:
`~/.spoor/sites/hslu/notes.md` (home panel → hslu → Overview).

1. Clear the hslu session and record a fresh one that includes the login
   (Keycloak → SWITCH edu-ID), a course, a folder download and a Zoom
   recording behind a weblink.
2. Write the check script from that recording. Turn on keepalive.
3. Write the actions: list courses, then download a course (ILIAS files with
   their folder structure, plus the Zoom recordings linked from it). It
   should only fetch what's new on later runs (`spoor.state`).
4. Run it, weekly. Note anything about Spoor that got in the way, and fix it
   in Spoor rather than in the script.

## Open

- **Windows:** the native host registry entry and `.cmd` launcher are written
  but have never run.
- **Microsoft SSO** (the other common case) is still untried.
- **Pacing:** only `fetch` (and downloads) are paced; `XMLHttpRequest` is
  audited only.
- **WebSocket frames** aren't recorded (webRequest doesn't see them).
- **Web apps:** they go through the HTTP API from their own server. Revisit
  CORS or scoped tokens if a page-only app is ever wanted.
- **Later:** a browser worker on a VM for always-on use. A relogin script
  that only bothers you when MFA is actually needed. Generated client
  modules from the dossier.
