on purpose:

Spoor's browser profile for Hostpoint, which is your login. Without it you'd have to log in again.

The deliverables in the IntegratedWorkspace (reports/2026-09-30-hostpoint/). I didn't touch them; they're outside this repo.

README.md

+34

\-1



Not stupid at all. That's the right shape, and a well-known pattern: CLI tools like the AWS SSO login, rclone or Playwright's storageState work exactly like this. One refinement to the token point, so the plan holds up:



What you actually get from the login window



The Microsoft token itself is bound to CAS (audience/client\_id = CAS), and it usually never reaches the browser anyway: CAS redeems it on the server and gives you its own CAS session (cookie, sometimes a CAS token). So what your app captures is CAS's session, and it works for CAS, not for "whatever". That's no restriction, though, because it's exactly what your app needs.



What to watch with that kind of session:



HttpOnly cookies: page JavaScript can't read them, so the harness has to take them from the browser's cookie store. That's no problem for an extension or Playwright.

Binding: some sites tie the session to IP, user agent or device. Then it only works from the same machine. The replay check shows that.

Lifetime: if the session expires daily, the window opens daily. With a persistent profile, though, the Microsoft SSO is often still valid, and the window opens and closes by itself without a click.

Spoor's role: evidence, not answers



I agree completely. Spoor doesn't have to conclude anything; it has to make sure the agent can see everything that's needed:



the complete recording of the auth chain across all domains, including cookies, Set-Cookie, redirects and browser storage;

lifetimes (cookie expiry, token exp);

the result of the replay check (does this request work outside the browser, and with what?);

whatever is visible on the site that points to an API (e.g. CAS's own REST endpoints in the traffic, or a /api/ documentation link).



The agent draws the conclusion itself: "there's a CAS API", "simple form login, plain HTTP is enough", or "harness needed". That keeps Spoor general and small.



The harness package



As a small, standalone library your app uses:



const session = await loginHarness({

&#x20; start: "https://cas.example.ch/",

&#x20; profile: "cas",                                  // persistent → often no click needed next time

&#x20; done: s => s.url.startsWith("https://cas.example.ch/app") \&\& s.cookies.has("CAS\_SESSION"),

&#x20; capture: { cookies: \["cas.example.ch"], storage: \["authToken"] },

});

// window closes; the app uses session.cookies / session.storage for its own HTTP requests

What goes in it (start, done, capture) is exactly what the agent works out from the Spoor recording. It's the "small package" output of use case 2.

Technically, there are two variants:

Playwright (Firefox or Chromium) with a persistent profile: mature, storageState export built in, usable from Node and Python. Stealth hardly matters here, because a human logs in and Microsoft's login doesn't block that.

The Spoor extension in its own profile: no extra dependency, but tied to the Spoor stack.



I'd use Playwright for the harness. It decouples your apps from Spoor, which fits "the product isn't Spoor".



Renewal: your app calls loginHarness again when its requests start failing with 401 or a login redirect. With the persistent profile, that's usually silent.

Where it fits in the plan

Spoor core in Firefox, recording cookies and storage too (that's important for the auth evidence).

Use case 1: scripts, runs, confirm, run folders.

Use case 2: replay check, plus the harness package as a separate small library with CAS as the first case.



It's a clean split: Spoor shows how a site works; your app uses the result, if needed via the harness, without depending on Spoor.



Should I capture this as a PLAN.md (architecture, the two use cases, harness concept, spike criteria with CAS)?



commit all current and write a plan, into the repo, based on all we discussed



