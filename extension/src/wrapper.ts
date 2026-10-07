// The code a run injects into the worker tab (tabs.executeScript, content
// script world: the page can't see it). The script body becomes an async
// function with these in scope:
//
//   args                 the run's arguments
//   fetch                the page's own fetch (content.fetch): page cookies and
//                        Origin, paced and pausable by Spoor
//   console              log / info / warn / error, streamed to the run log
//   spoor.progress(done, total?, label?)
//   spoor.confirm(message, detail?)  → true / false; waits for a click in the sidebar
//   spoor.save(name, data)           string, object (as JSON) or Uint8Array → run folder
//   spoor.sleep(ms)                  pausable; not throttled in hidden tabs
//   spoor.checkpoint()               waits while paused; throws once stopped
//   spoor.run                        run id

export function buildWrapper(runId: string, args: unknown, body: string): string {
  return `(() => {
const __RUN = ${JSON.stringify(runId)};
const __ARGS = ${JSON.stringify(args ?? {})};
const __send = (kind, data) => browser.runtime.sendMessage({ spoor_run: __RUN, kind, data });
const __pageFetch = (typeof content !== "undefined" && content.fetch) ? content.fetch.bind(content) : window.fetch.bind(window);
const __fmt = a => { if (typeof a === "string") return a; if (a instanceof Error) return a.stack || String(a); try { return JSON.stringify(a); } catch { return String(a); } };
const __console = {};
for (const level of ["log", "info", "warn", "error", "debug"]) __console[level] = (...a) => { __send("log", { level, text: a.map(__fmt).join(" ") }).catch(() => {}); };
const __b64 = u8 => { let s = ""; for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode(...u8.subarray(i, i + 0x8000)); return btoa(s); };
const __spoor = Object.freeze({
  run: __RUN,
  progress: (done, total, label) => __send("progress", { done, total, label }),
  confirm: async (message, detail) => (await __send("confirm", { message, detail: detail === undefined ? undefined : JSON.parse(JSON.stringify(detail)) })).approved,
  save: (name, data) => {
    if (typeof data === "string") return __send("save", { name, data, encoding: "utf8" });
    if (data instanceof ArrayBuffer) data = new Uint8Array(data);
    if (data instanceof Uint8Array) return __send("save", { name, data: __b64(data), encoding: "base64" });
    return __send("save", { name, data: JSON.stringify(data, null, 2), encoding: "utf8" });
  },
  sleep: ms => __send("sleep", { ms }),
  checkpoint: () => __send("checkpoint"),
});
const __fetch = async (input, init) => { await __send("gate"); return __pageFetch(input, init); };
const __plain = v => v === undefined ? null : JSON.parse(JSON.stringify(v));
(async function (args, spoor, fetch, console) {
${body}
}).call(undefined, __ARGS, __spoor, __fetch, __console).then(
  r => { let result; try { result = __plain(r); } catch (e) { return __send("done", { ok: false, error: "result is not JSON-serializable: " + e.message }); } return __send("done", { ok: true, result }); },
  e => __send("done", { ok: false, error: (e && e.message) ? e.message : String(e) }),
).catch(() => {});
})();
void 0;
`;
}
