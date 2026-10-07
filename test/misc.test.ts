import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { REDACTED, redactBody } from "../extension/src/redact.ts";
import { ChunkAssembler, CHUNK_CHARS, splitMessage } from "../src/shared/rpc.ts";
import { frameReader } from "../src/host/main.ts";
import { parseCookieHeader, compare } from "../src/service/replay.ts";

process.env.SPOOR_HOME = mkdtempSync(join(tmpdir(), "spoor-test-"));
const store = await import("../src/service/store.ts");

test("redaction removes typed secrets, keeps site tokens", () => {
  const form = redactBody({ kind: "text", text: "login=me&passwd=hunter2&csrf=abc" }, "application/x-www-form-urlencoded");
  assert.equal(form.kind === "text" && new URLSearchParams(form.text).get("passwd"), REDACTED);
  assert.equal(form.kind === "text" && new URLSearchParams(form.text).get("csrf"), "abc");
  const json = redactBody({ kind: "text", text: '{"user":{"password":"x","otp":"123456"},"access_token":"t"}' }, "application/json");
  assert.deepEqual(JSON.parse(json.kind === "text" ? json.text : ""), { user: { password: REDACTED, otp: REDACTED }, access_token: "t" });
  const plain = { kind: "text" as const, text: "hello" };
  assert.equal(redactBody(plain, "text/plain"), plain);
});

test("large messages split into chunks and reassemble", () => {
  const msg = { t: "event" as const, name: "x", data: "é".repeat(CHUNK_CHARS * 2 + 5) };
  const json = JSON.stringify(msg);
  const chunks = splitMessage(json, "c1")!;
  assert.equal(chunks.length, 3);
  const a = new ChunkAssembler();
  let out = null;
  for (const c of chunks.reverse()) out = a.add(JSON.parse(JSON.stringify(c))) ?? out;
  assert.deepEqual(out, msg);
  assert.equal(splitMessage("{}", "c2"), null);
});

test("native messaging frames parse across chunk boundaries", () => {
  const got: string[] = [];
  const read = frameReader(j => got.push(j));
  const frame = (s: string) => {
    const b = Buffer.from(s);
    const h = Buffer.alloc(4);
    h.writeUInt32LE(b.length);
    return Buffer.concat([h, b]);
  };
  const all = Buffer.concat([frame('{"a":1}'), frame('{"b":"ü"}')]);
  read(all.subarray(0, 5));
  read(all.subarray(5));
  assert.deepEqual(got, ['{"a":1}', '{"b":"ü"}']);
});

test("site registry, scripts and runs round-trip on disk", () => {
  assert.throws(() => store.addSite("Bad", { url: "https://x.test/" }));
  assert.throws(() => store.addSite("ok", { url: "ftp://x.test/" }));
  store.addSite("panel", { url: "https://panel.test/", min_gap_ms: 250 });
  assert.equal(store.getSite("panel").min_gap_ms, 250);
  assert.throws(() => store.getSite("missing"), /no site/);
  store.writeScript("panel", "list-zones", "// lists zones\r\nreturn 1;\r\n");
  assert.equal(store.readScript("panel", "list-zones"), "// lists zones\nreturn 1;\n");
  assert.equal(store.listScripts("panel")[0]!.description, "lists zones");
  assert.throws(() => store.readScript("panel", "../../etc/passwd"));
  assert.throws(() => store.saveRunFile("panel", "20260101-000000-abcd", "../x", "", "utf8"));
});

test("replay compare and cookie parsing", () => {
  assert.deepEqual(parseCookieHeader("a=1; b=x=y"), [["a", "1"], ["b", "x=y"]]);
  const f = { sequence: 1, kind: "http" as const, timestamp_ms: 0, url: "https://a.test/x", status: 200, response_headers: { "content-type": "application/json" } };
  assert.equal(compare({ status: 200, bytes: 10, matches: false, content_type: "application/json" }, f), undefined);
  assert.match(compare({ status: 302, bytes: 0, matches: false, location: "/login" }, f)!, /status 302/);
});
