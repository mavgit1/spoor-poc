import assert from "node:assert/strict";
import { test } from "node:test";

import type { Flow } from "../src/shared/types.ts";
import { authChain, decodeJwt, endpoints, lifetimes, matchingFlows, pathTemplate, snippet, trace } from "../src/service/inspect.ts";

function flow(seq: number, method: string, url: string, req?: string, resp?: string, extra: Partial<Flow> = {}): Flow {
  return {
    sequence: seq,
    kind: "http",
    timestamp_ms: seq,
    url,
    method,
    type: "xmlhttprequest",
    request_headers: {},
    request_body: req === undefined ? undefined : { kind: "text", text: req },
    status: 200,
    response_body: resp === undefined ? undefined : { kind: "text", text: resp },
    ...extra,
  };
}

/** A common shape: an id first shows up in page HTML, then is posted back. */
const idFromHtml = (): Flow[] => [
  flow(3, "POST", "https://app.test/items/edit?name=a", "op=load&id=4242", '{"items":[]}'),
  flow(1, "GET", "https://app.test/items/edit?name=a", undefined, '<form>  <input type="hidden" name="itemId" value="4242"> </form>'),
  flow(2, "GET", "https://app.test/api/me", undefined, '{"user":"x"}'),
];

test("trace finds origin and use", () => {
  const t = trace(idFromHtml(), "4242");
  assert.equal(t.appears_in.length, 1);
  assert.equal(t.appears_in[0]!.sequence, 1);
  assert.equal(t.appears_in[0]!.place, "response body");
  assert.match(t.appears_in[0]!.snippet, /name="itemId" value="4242"/);
  assert.equal(t.used_in.length, 1);
  assert.equal(t.used_in[0]!.place, "request body");
  assert.equal(t.used_in[0]!.sequence, 3);
});

test("matchingFlows is ordered and filtered", () => {
  assert.deepEqual(matchingFlows(idFromHtml()).map(f => f.sequence), [1, 2, 3]);
  assert.deepEqual(matchingFlows(idFromHtml(), "op=load").map(f => f.sequence), [3]);
});

test("snippet is unicode safe", () => {
  const s = snippet(`${"ü".repeat(80)}needle${"é".repeat(80)}`, "needle");
  assert.ok(s.startsWith("…") && s.endsWith("…") && s.includes("needle"));
  assert.equal(snippet("abc", "zzz"), "");
});

test("authChain follows redirects across hosts and spots OIDC", () => {
  const flows: Flow[] = [
    flow(1, "GET", "https://cas.test/", undefined, undefined, { type: "main_frame", status: 302, redirect_url: "https://login.microsoftonline.com/t/oauth2/v2.0/authorize?client_id=abc&redirect_uri=https%3A%2F%2Fcas.test%2Fcb&response_type=code&scope=openid" }),
    flow(2, "GET", "https://login.microsoftonline.com/t/oauth2/v2.0/authorize?client_id=abc&redirect_uri=https%3A%2F%2Fcas.test%2Fcb&response_type=code&scope=openid", undefined, "<html>", { type: "main_frame", set_cookies: ["ESTSAUTH=x; path=/"] }),
    flow(3, "GET", "https://cdn.test/app.js", undefined, undefined, { type: "script" }),
    flow(4, "POST", "https://cas.test/cb", "code=0.AAAAverylongauthorizationcodevalue123&state=s", "", { type: "main_frame", status: 302, request_headers: { "content-type": "application/x-www-form-urlencoded" }, set_cookies: ["CAS_SESSION=1; HttpOnly"] }),
  ];
  const c = authChain(flows);
  assert.deepEqual(c.hosts, ["cas.test", "login.microsoftonline.com"]);
  assert.deepEqual(c.steps.map(s => s.sequence), [1, 2, 4]);
  assert.ok(c.protocol_hints.includes("OpenID Connect"));
  assert.ok(c.protocol_hints.includes("Microsoft Entra ID"));
  assert.deepEqual(c.steps[2]!.sets_cookies, ["CAS_SESSION"]);
  assert.match(c.steps[2]!.params.code!, /…\(\d+ chars\)$/);
});

test("lifetimes decode JWTs and cookie expiry", () => {
  const payload = Buffer.from(JSON.stringify({ iss: "https://idp.test", aud: "app", iat: 1000, exp: 4600 })).toString("base64url");
  const jwt = `eyJhbGciOiJIUzI1NiJ9.${payload}.sig`;
  assert.equal(decodeJwt(jwt)!.aud, "app");
  const l = lifetimes(
    [flow(1, "GET", "https://app.test/api", undefined, JSON.stringify({ token: jwt }), { set_cookies: ["S=1"] })],
    [{ name: "S", value: "1", domain: "app.test", path: "/", secure: true, httpOnly: true, sameSite: "lax", session: false, expirationDate: 2000 }],
    [],
    1000_000,
  );
  assert.equal(l.jwts.length, 1);
  assert.equal(l.jwts[0]!.lifetime_s, 3600);
  assert.equal(l.cookies[0]!.remaining_s, 1000);
  assert.deepEqual(l.cookies[0]!.set_by, [1]);
});

test("endpoints group by path template and skip static", () => {
  assert.equal(pathTemplate("/zones/4242/records/0123456789abcdef0123"), "/zones/{id}/records/{id}");
  const eps = endpoints([
    flow(1, "GET", "https://app.test/api/zones/1?x=1", undefined, "{}"),
    flow(2, "GET", "https://app.test/api/zones/2", undefined, "{}"),
    flow(3, "GET", "https://app.test/app.css", undefined, undefined, { type: "stylesheet" }),
  ]);
  assert.equal(eps.length, 1);
  assert.equal(eps[0]!.count, 2);
  assert.deepEqual(eps[0]!.query_params, ["x"]);
});
