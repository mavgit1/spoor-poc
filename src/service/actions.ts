// Actions: scripts that declare a name, parameters and whether they write, in
// a block at the top:
//
//   /* spoor-action
//   {
//     "description": "Download a course's material",
//     "params": { "course": { "type": "string", "description": "ILIAS ref id" } },
//     "required": ["course"],
//     "writes": false
//   }
//   */
//
// The home panel, a web app (over the HTTP API) and MCP call them by name with
// validated arguments. Recordings have nothing to do with them.

import type { ActionSpec, ParamSpec } from "../shared/types.ts";

const BLOCK = /^\s*(?:\/\/[^\n]*\n\s*)*\/\*\s*spoor-action\b([\s\S]*?)\*\//;
const TYPES = new Set(["string", "number", "integer", "boolean", "array", "object"]);

/** The action block of a script: undefined if there is none, an error string if it is broken. */
export function parseAction(code: string): ActionSpec | { error: string } | undefined {
  const m = BLOCK.exec(code);
  if (!m) return undefined;
  let spec: unknown;
  try {
    spec = JSON.parse(m[1]!);
  } catch (e) {
    return { error: `spoor-action block is not JSON: ${(e as Error).message}` };
  }
  if (!spec || typeof spec !== "object" || Array.isArray(spec)) return { error: "spoor-action block must be a JSON object" };
  const s = spec as ActionSpec;
  for (const [name, p] of Object.entries(s.params ?? {})) {
    if (!p || !TYPES.has((p as ParamSpec).type)) return { error: `param ${name}: type must be one of ${[...TYPES].join(", ")}` };
  }
  for (const r of s.required ?? []) if (!s.params?.[r]) return { error: `required param ${r} is not in params` };
  return s;
}

function typeOk(type: ParamSpec["type"], v: unknown): boolean {
  switch (type) {
    case "string": return typeof v === "string";
    case "number": return typeof v === "number" && Number.isFinite(v);
    case "integer": return Number.isInteger(v);
    case "boolean": return typeof v === "boolean";
    case "array": return Array.isArray(v);
    case "object": return !!v && typeof v === "object" && !Array.isArray(v);
  }
}

/** Arguments with defaults applied, or the list of what is wrong with them. */
export function validateArgs(spec: ActionSpec, args: unknown): { args: Record<string, unknown> } | { errors: string[] } {
  if (args !== undefined && (typeof args !== "object" || args === null || Array.isArray(args))) return { errors: ["args must be an object"] };
  const given = { ...(args as Record<string, unknown> | undefined) };
  const params = spec.params ?? {};
  const errors: string[] = [];
  for (const k of Object.keys(given)) if (!params[k]) errors.push(`unknown argument ${k} (known: ${Object.keys(params).join(", ") || "none"})`);
  for (const [k, p] of Object.entries(params)) {
    if (given[k] === undefined && p.default !== undefined) given[k] = p.default;
    // CLI and form input arrive as strings; take "3" for a number.
    if (typeof given[k] === "string" && (p.type === "number" || p.type === "integer") && given[k] !== "" && !Number.isNaN(Number(given[k]))) given[k] = Number(given[k]);
    if (given[k] === undefined) {
      if (spec.required?.includes(k)) errors.push(`missing argument ${k}${p.description ? ` (${p.description})` : ""}`);
      continue;
    }
    if (!typeOk(p.type, given[k])) errors.push(`argument ${k} must be ${p.type}`);
    else if (p.enum && !p.enum.some(e => JSON.stringify(e) === JSON.stringify(given[k]))) errors.push(`argument ${k} must be one of ${p.enum.map(e => JSON.stringify(e)).join(", ")}`);
  }
  return errors.length ? { errors } : { args: given };
}

/** JSON Schema for an action's arguments (MCP inputSchema). */
export function inputSchema(spec: ActionSpec): Record<string, unknown> {
  return { type: "object", properties: spec.params ?? {}, required: spec.required ?? [], additionalProperties: false };
}
