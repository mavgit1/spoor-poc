// A small fake site for the end-to-end test: an app on 127.0.0.1 that logs in
// through an "identity provider" on localhost (a different host), OAuth-style:
//
//   app /            → 302 idp /authorize?client_id&redirect_uri&response_type=code&state
//   idp /authorize   → login form (auto-submits in tests, standing in for the human)
//   idp POST /login  → Set-Cookie IDPSESSION, 302 app /cb?code=…
//   app /cb          → Set-Cookie APPSESSION (HttpOnly), 302 /
//   app /            → page with a CSRF token and an item id in hidden inputs,
//                      and a JWT in localStorage
//   app /api/items   → JSON, needs the session cookie; POST needs the CSRF header
//
// It records the arrival time and headers of every API request so the test
// can check pacing and that requests carry the page's cookies and Origin.

import { createServer, type IncomingMessage, type Server } from "node:http";

export interface Hit {
  at: number;
  method: string;
  path: string;
  origin?: string;
  cookie?: string;
  csrf?: string;
}

export interface FakeSite {
  app: string;
  idp: string;
  hits: Hit[];
  close(): Promise<void>;
}

const CSRF = "csrf-7f3a9c";
const SESSION = "sess-5d2e81";

function jwt(): string {
  const b64 = (o: unknown) => Buffer.from(JSON.stringify(o)).toString("base64url");
  const now = Math.floor(Date.now() / 1000);
  return `${b64({ alg: "none" })}.${b64({ iss: "http://localhost/idp", aud: "spoor-test", iat: now, exp: now + 3600, sub: "tester" })}.x`;
}

function cookies(req: IncomingMessage): Record<string, string> {
  return Object.fromEntries((req.headers.cookie ?? "").split(/;\s*/).filter(Boolean).map(c => c.split("=", 2) as [string, string]));
}

function body(req: IncomingMessage): Promise<string> {
  return new Promise(resolve => {
    let s = "";
    req.on("data", c => (s += c));
    req.on("end", () => resolve(s));
  });
}

function listen(server: Server, port: number, host: string): Promise<void> {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, host, () => resolve());
  });
}

export async function startFakeSite(appPort = 7600, idpPort = 7601): Promise<FakeSite> {
  const app = `http://127.0.0.1:${appPort}`;
  const idp = `http://localhost:${idpPort}`;
  const hits: Hit[] = [];
  const items = [{ id: 4242, name: "first" }, { id: 4243, name: "second" }];

  const appServer = createServer(async (req, res) => {
    const url = new URL(req.url ?? "/", app);
    const c = cookies(req);
    const authed = c.APPSESSION === SESSION;
    if (url.pathname.startsWith("/api/")) {
      hits.push({ at: Date.now(), method: req.method!, path: url.pathname + url.search, origin: req.headers.origin, cookie: req.headers.cookie, csrf: req.headers["x-csrf-token"] as string | undefined });
    }
    if (url.pathname === "/cb") {
      if (url.searchParams.get("code") !== "authcode-123") return res.writeHead(400).end("bad code");
      res.writeHead(302, { location: "/", "set-cookie": `APPSESSION=${SESSION}; Path=/; HttpOnly; Max-Age=3600; SameSite=Lax` });
      return res.end();
    }
    if (url.pathname === "/" || url.pathname === "/index.html") {
      if (!authed) {
        const q = new URLSearchParams({ client_id: "spoor-test", redirect_uri: `${app}/cb`, response_type: "code", scope: "openid profile", state: "st-1" });
        res.writeHead(302, { location: `${idp}/authorize?${q}` });
        return res.end();
      }
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      return res.end(`<!doctype html><title>Fake App</title>
<form><input type="hidden" name="csrf_token" value="${CSRF}"><input type="hidden" name="itemId" value="4242"></form>
<p>Logged in.</p>
<script>localStorage.setItem("authToken", ${JSON.stringify(jwt())}); fetch("/api/items").then(r => r.json()).then(d => document.body.dataset.items = d.items.length);</script>`);
    }
    if (url.pathname === "/api/items") {
      if (!authed) return res.writeHead(401, { "content-type": "application/json" }).end('{"error":"login"}');
      if (req.method === "POST") {
        if (req.headers["x-csrf-token"] !== CSRF) return res.writeHead(403).end('{"error":"csrf"}');
        const item = JSON.parse(await body(req));
        items.push(item);
        return res.writeHead(201, { "content-type": "application/json" }).end(JSON.stringify(item));
      }
      return res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ items, at: Date.now() }));
    }
    if (url.pathname.startsWith("/api/slow")) {
      await new Promise(r => setTimeout(r, 50));
      return res.writeHead(200, { "content-type": "application/json" }).end(`{"path":${JSON.stringify(url.pathname)}}`);
    }
    res.writeHead(404).end("not found");
  });

  const idpServer = createServer(async (req, res) => {
    const url = new URL(req.url ?? "/", idp);
    if (url.pathname === "/authorize") {
      const redirect = url.searchParams.get("redirect_uri")!;
      if (cookies(req).IDPSESSION === "idp-1") {
        res.writeHead(302, { location: `${redirect}?code=authcode-123&state=${url.searchParams.get("state")}` });
        return res.end();
      }
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      return res.end(`<!doctype html><title>Sign in</title>
<form method="post" action="/login"><input name="login" value="tester"><input type="password" name="passwd" value="hunter2-secret">
<input type="hidden" name="redirect_uri" value="${redirect}"><input type="hidden" name="state" value="${url.searchParams.get("state")}"></form>
<script>setTimeout(() => document.forms[0].submit(), 300);</script>`);
    }
    if (url.pathname === "/login" && req.method === "POST") {
      const form = new URLSearchParams(await body(req));
      if (form.get("passwd") !== "hunter2-secret") return res.writeHead(401).end("wrong password");
      res.writeHead(302, { location: `${form.get("redirect_uri")}?code=authcode-123&state=${form.get("state")}`, "set-cookie": "IDPSESSION=idp-1; Path=/; HttpOnly; Max-Age=86400" });
      return res.end();
    }
    res.writeHead(404).end("not found");
  });

  await listen(appServer, appPort, "127.0.0.1");
  await listen(idpServer, idpPort, "127.0.0.1");
  return {
    app,
    idp,
    hits,
    close: async () => {
      appServer.closeAllConnections();
      idpServer.closeAllConnections();
      await Promise.all([new Promise(r => appServer.close(r)), new Promise(r => idpServer.close(r))]);
    },
  };
}
