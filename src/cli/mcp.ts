// `spoor mcp`: Spoor as an MCP server on stdio (newline-delimited JSON-RPC).
// A thin layer over the HTTP API, started on demand like the CLI.
//
// Tools:
//   spoor_sites, spoor_session, spoor_login, spoor_session_clear
//   spoor_exec                  run code in a site's session (for the agent building things)
//   spoor_run_status, spoor_run_stop
//   <site>__<action>            one per action (scripts with a spoor-action block)
//
// Clearing a session is a tool of its own and needs `confirm: true`. Approvals
// (spoor.confirm) stay in the browser; there is no tool for them.

import { createInterface } from "node:readline";

import type { RunEvent, RunMeta } from "../shared/types.ts";
import { api } from "../service/client.ts";
import { VERSION } from "../service/server.ts";

type Json = Record<string, unknown>;
interface Tool {
  name: string;
  description: string;
  inputSchema: Json;
  annotations?: Json;
  call: (args: any) => Promise<unknown>;
}

/** How long a tool call waits for a run before handing back its id. */
const WAIT_SECS = 50;

const str = (description: string) => ({ type: "string", description });
const obj = (properties: Json, required: string[] = []) => ({ type: "object", properties, required, additionalProperties: false });

async function runReport(site: string, meta: RunMeta & { result?: unknown; still_running?: boolean }, after = 0): Promise<Json> {
  const { events } = await api<{ events: RunEvent[] }>("GET", `/sites/${site}/runs/${meta.id}/events?after=${after}`);
  const logs = events.flatMap(e => (e.type === "log" ? [`${e.level === "log" ? "" : `[${e.level}] `}${e.text}`] : [])).slice(-100);
  const waiting = events.filter(e => e.type === "confirm").length > events.filter(e => e.type === "confirmed").length;
  const files = (await api<{ files: { name: string; bytes: number }[] }>("GET", `/sites/${site}/runs/${meta.id}`)).files;
  return {
    run_id: meta.id,
    status: meta.status,
    ...(meta.status === "ok" ? { result: meta.result } : {}),
    ...(meta.error ? { error: meta.error } : {}),
    ...(meta.progress ? { progress: meta.progress } : {}),
    ...(waiting ? { note: "waiting for the human to approve in Spoor's sidebar" } : {}),
    ...(!["ok", "error", "stopped"].includes(meta.status) ? { next: `still running — call spoor_run_status with run_id ${meta.id} (after: ${events.at(-1)?.n ?? after})` } : {}),
    last_event: events.at(-1)?.n ?? after,
    logs,
    files,
  };
}

function fixedTools(): Tool[] {
  return [
    {
      name: "spoor_sites",
      description: "List the sites Spoor knows, with their last known login state.",
      inputSchema: obj({}),
      annotations: { readOnlyHint: true },
      call: async () => {
        const sites = await api<{ name: string; url: string }[]>("GET", "/sites");
        return Promise.all(sites.map(async s => ({ ...s, session: (await api<Json>("GET", `/sites/${s.name}/session?check=0`)).state })));
      },
    },
    {
      name: "spoor_session",
      description: "A site's login state: runs its check script if Spoor's Firefox is up (does not start it), plus a cookie summary.",
      inputSchema: obj({ site: str("site name") }, ["site"]),
      annotations: { readOnlyHint: true },
      call: a => api("GET", `/sites/${a.site}/session`),
    },
    {
      name: "spoor_login",
      description: "Ask the human to log in to a site: Spoor brings a tab of the site to the front and notifies them, then waits until the check script passes (or the timeout).",
      inputSchema: obj({ site: str("site name"), timeout_secs: { type: "integer", description: "how long to wait (default 600)" } }, ["site"]),
      call: a => api("POST", `/sites/${a.site}/login`, { timeout_secs: a.timeout_secs ?? 600 }),
    },
    {
      name: "spoor_session_clear",
      description: "Log a site out for good: remove every cookie and all storage of its container (and optionally Firefox's shared HTTP cache). The human has to log in again afterwards.",
      inputSchema: obj({ site: str("site name"), confirm: { type: "boolean", const: true, description: "must be true" }, cache: { type: "boolean", description: "also clear the HTTP cache (all sites)" } }, ["site", "confirm"]),
      annotations: { destructiveHint: true },
      call: async a => {
        if (a.confirm !== true) throw new Error("pass confirm: true to clear the session");
        return api("POST", `/sites/${a.site}/session/clear`, { cache: !!a.cache });
      },
    },
    {
      name: "spoor_exec",
      description:
        "Run JavaScript in a site's logged-in session (hidden tab, content-script world). The code is the body of an async function with args, fetch (the page's own, paced), console, and spoor (save, download, html, tab, state, progress, confirm) in scope; return a JSON value. For exploring a site and developing scripts.",
      inputSchema: obj({ site: str("site name"), code: str("function body"), args: { type: "object" }, url: str("page to load first (default: the site's start page)") }, ["site", "code"]),
      call: async a => {
        const meta = await api<RunMeta>("POST", `/sites/${a.site}/runs`, { code: a.code, args: a.args ?? {}, url: a.url });
        const done = await waitRun(a.site, meta.id);
        return runReport(a.site, done);
      },
    },
    {
      name: "spoor_run_status",
      description: "Status, new log lines, result and files of a run (from an action or spoor_exec). Waits up to 50 s for news while it runs.",
      inputSchema: obj({ site: str("site name"), run_id: str("run id"), after: { type: "integer", description: "last_event from the previous call" } }, ["site", "run_id"]),
      annotations: { readOnlyHint: true },
      call: async a => {
        const after = Number(a.after ?? 0);
        await api("GET", `/sites/${a.site}/runs/${a.run_id}/events?after=${after}&wait_ms=${WAIT_SECS * 1000}`);
        const { meta } = await api<{ meta: RunMeta & { result?: unknown } }>("GET", `/sites/${a.site}/runs/${a.run_id}`);
        if (meta.status === "ok" && meta.result === undefined) {
          const r = await api<{ events: RunEvent[] }>("GET", `/sites/${a.site}/runs/${a.run_id}/events?after=0`);
          const res = r.events.find(e => e.type === "result");
          if (res?.type === "result") meta.result = res.result;
        }
        return runReport(a.site, meta, after);
      },
    },
    {
      name: "spoor_run_stop",
      description: "Stop a run.",
      inputSchema: obj({ site: str("site name"), run_id: str("run id") }, ["site", "run_id"]),
      call: a => api("POST", `/sites/${a.site}/runs/${a.run_id}/stop`),
    },
  ];
}

async function waitRun(site: string, id: string): Promise<RunMeta & { result?: unknown }> {
  const deadline = Date.now() + WAIT_SECS * 1000;
  let after = 0;
  for (;;) {
    const left = deadline - Date.now();
    const r = await api<{ events: RunEvent[]; status: string }>("GET", `/sites/${site}/runs/${id}/events?after=${after}&wait_ms=${Math.max(0, Math.min(left, 25_000))}`);
    for (const e of r.events) after = Math.max(after, e.n);
    const result = r.events.find(e => e.type === "result");
    if (["ok", "error", "stopped"].includes(r.status) || left <= 0) {
      const { meta } = await api<{ meta: RunMeta & { result?: unknown } }>("GET", `/sites/${site}/runs/${id}`);
      if (result?.type === "result") meta.result = result.result;
      else if (meta.status === "ok") {
        const all = await api<{ events: RunEvent[] }>("GET", `/sites/${site}/runs/${id}/events?after=0`);
        const res = all.events.find(e => e.type === "result");
        if (res?.type === "result") meta.result = res.result;
      }
      return meta;
    }
  }
}

/** Tool names: [a-zA-Z0-9_-], at most 64. */
function toolName(site: string, action: string): string {
  return `${site}__${action}`.replace(/[^a-zA-Z0-9_-]/g, "_").slice(0, 64);
}

async function actionTools(): Promise<Tool[]> {
  const actions = await api<any[]>("GET", "/actions");
  return actions.filter(a => !a.error).map(a => ({
    name: toolName(a.site, a.name),
    description: `[${a.site}] ${a.description ?? a.name}${a.writes ? " (changes data on the site; asks the human to approve in the browser)" : ""}. Runs in the human's logged-in session; if the login lapsed, the human is asked to log in and the call fails with login_required.`,
    inputSchema: a.input_schema,
    annotations: { readOnlyHint: !a.writes, destructiveHint: !!a.writes, title: `${a.site}: ${a.name}` },
    call: async (args: Json) => {
      const meta = await api<RunMeta & { result?: unknown }>("POST", `/sites/${a.site}/actions/${a.name}`, { args, wait: WAIT_SECS });
      return runReport(a.site, meta);
    },
  }));
}

export async function mcp(): Promise<void> {
  const send = (msg: Json) => process.stdout.write(JSON.stringify(msg) + "\n");
  const tools = async () => [...fixedTools(), ...(await actionTools().catch(() => []))];

  const handle = async (msg: any): Promise<Json | undefined> => {
    switch (msg.method) {
      case "initialize":
        return {
          protocolVersion: msg.params?.protocolVersion ?? "2025-06-18",
          capabilities: { tools: { listChanged: false } },
          serverInfo: { name: "spoor", version: VERSION },
          instructions:
            "Spoor runs code in websites the human is logged in to, in their own Firefox. Use the per-site action tools for finished jobs; spoor_exec to explore. If a call reports login_required, call spoor_login (the human logs in by hand) and retry. Never clear a session unless the human asked for it.",
        };
      case "ping":
        return {};
      case "tools/list":
        return { tools: (await tools()).map(({ call: _call, ...t }) => t) };
      case "tools/call": {
        const tool = (await tools()).find(t => t.name === msg.params?.name);
        if (!tool) return { content: [{ type: "text", text: `unknown tool ${msg.params?.name}` }], isError: true };
        try {
          const out = await tool.call(msg.params?.arguments ?? {});
          const failed = !!out && typeof out === "object" && ["error", "stopped"].includes((out as Json).status as string);
          return { content: [{ type: "text", text: JSON.stringify(out, null, 2) }], isError: failed };
        } catch (e) {
          return { content: [{ type: "text", text: (e as Error).message }], isError: true };
        }
      }
    }
    if (msg.id !== undefined) throw Object.assign(new Error(`method not found: ${msg.method}`), { code: -32601 });
    return undefined;
  };

  const rl = createInterface({ input: process.stdin });
  rl.on("line", line => {
    if (!line.trim()) return;
    let msg: any;
    try {
      msg = JSON.parse(line);
    } catch {
      return send({ jsonrpc: "2.0", id: null, error: { code: -32700, message: "parse error" } });
    }
    void handle(msg).then(
      result => {
        if (msg.id !== undefined && result !== undefined) send({ jsonrpc: "2.0", id: msg.id, result });
      },
      e => {
        if (msg.id !== undefined) send({ jsonrpc: "2.0", id: msg.id, error: { code: e.code ?? -32603, message: e.message } });
      },
    );
  });
  rl.on("close", () => process.exit(0));
}
