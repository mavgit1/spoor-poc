import assert from "node:assert/strict";
import { test } from "node:test";

import { parseAction, validateArgs } from "../src/service/actions.ts";
import { endpoints } from "../src/service/inspect.ts";
import { safeRelPath } from "../src/shared/relpath.ts";
import type { Flow } from "../src/shared/types.ts";

test("action block is parsed after leading comments", () => {
  const spec = parseAction('// hi\n/* spoor-action\n{"description":"d","params":{"n":{"type":"integer"}},"required":["n"]}\n*/\nreturn 1;');
  assert.deepEqual(spec, { description: "d", params: { n: { type: "integer" } }, required: ["n"] });
  assert.equal(parseAction("return 1;"), undefined);
  assert.match((parseAction("/* spoor-action {nope} */") as { error: string }).error, /not JSON/);
  assert.match((parseAction('/* spoor-action {"params":{"x":{"type":"date"}}} */') as { error: string }).error, /type must be/);
  assert.match((parseAction('/* spoor-action {"required":["x"]} */') as { error: string }).error, /not in params/);
});

test("action args: defaults, coercion, unknown, enum", () => {
  const spec = { params: { n: { type: "integer" as const }, mode: { type: "string" as const, enum: ["a", "b"], default: "a" } }, required: ["n"] };
  assert.deepEqual(validateArgs(spec, { n: "3" }), { args: { n: 3, mode: "a" } });
  const bad = validateArgs(spec, { x: 1, mode: "c" });
  assert.ok("errors" in bad);
  assert.deepEqual(bad.errors.sort(), ["argument mode must be one of \"a\", \"b\"", "missing argument n", "unknown argument x (known: n, mode)"].sort());
  assert.ok("errors" in validateArgs(spec, { n: 1.5 }));
});

test("relative paths are made safe", () => {
  assert.equal(safeRelPath("Week 1: Intro/Folien.pdf"), "Week 1_ Intro/Folien.pdf");
  assert.equal(safeRelPath("../../etc/passwd"), "etc/passwd");
  assert.equal(safeRelPath("/abs//x\\y"), "abs/x/y");
  assert.equal(safeRelPath(".."), "");
});

test("endpoints split on dispatch parameters", () => {
  const f = (seq: number, url: string): Flow => ({ sequence: seq, kind: "http", timestamp_ms: 0, url, method: "GET", type: "main_frame", status: 200 });
  const eps = endpoints([
    f(1, "https://x.ch/ilias.php?baseClass=ilrepositorygui&cmd=download&ref_id=1"),
    f(2, "https://x.ch/ilias.php?baseClass=ilrepositorygui&cmd=download&ref_id=2"),
    f(3, "https://x.ch/ilias.php?baseClass=ilhelpgui&cmd=showHelp"),
  ]);
  assert.equal(eps.length, 2);
  assert.deepEqual(eps.map(e => [e.dispatch, e.count]), [[{ baseClass: "ilhelpgui", cmd: "showhelp" }, 1], [{ baseClass: "ilrepositorygui", cmd: "download" }, 2]]);
});
