# spoor-login-harness

This package gets a session for a site that needs a real browser login (SSO,
MFA, device-bound sessions), so your app can then use plain HTTP. It works
like `aws sso login`: a window opens, you log in (or nothing happens, if the
persistent profile still has a valid IdP session), it captures what the app
needs, and the window closes.

It is built on Playwright, not on Spoor, so your apps don't depend on Spoor.
You work out the `start`, `done` and `capture` values from a Spoor recording
of the login (`spoor auth`, `spoor lifetimes`).

```ts
import { loginHarness } from "spoor-login-harness";

const session = await loginHarness({
  start: "https://cas.example.ch/",
  profile: "cas",                       // ~/.spoor-harness/cas, kept between runs
  done: s => s.url.startsWith("https://cas.example.ch/app") && s.cookies.has("CAS_SESSION"),
  capture: { cookies: ["cas.example.ch"], storage: ["authToken"] },
});

const r = await session.fetch("https://cas.example.ch/api/me");   // adds the Cookie header
session.cookieHeader(url);   // or build requests yourself
session.storage.authToken;   // captured storage values
session.expiresAt;           // earliest cookie expiry
```

- **`silentFirst`** (default `true`): the harness first tries without any
  window, for up to `silentTimeoutMs` (15 s). If the IdP session in the
  profile is still valid, the login finishes without you seeing anything.
  Only if it doesn't finish does a window open for you.
- **Renewal:** when your app's requests start returning 401 or redirecting to
  a login page, call `loginHarness` again.
- **`browser`:** `"firefox"` (default) or `"chromium"`. The first time, run
  `npx playwright install firefox`.

Install it from this repo (`npm install ../spoor-poc/harness`, with `playwright`
as a dependency of your app). `prepare` builds `dist/`.
