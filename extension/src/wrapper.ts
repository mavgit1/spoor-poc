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
//   spoor.save(name, data)           string, object (as JSON) or bytes → run folder; → {file, bytes, abs}
//   spoor.download(url, path | {path?, dir?, referer?, method?, headers?, body?})
//                                    Firefox downloads it with the site's cookies, straight
//                                    to the run's files/; → {path, file, abs, bytes, mime}
//   spoor.html(url, init?)           fetch + parse → Document (links resolve against the final URL)
//   spoor.tab(url)                   another hidden tab in the same container (another
//                                    origin, e.g. a video host): {fetch, html, eval, urls, close};
//                                    urls() = the pages it committed, in order (where a link went)
//                                    its fetch gives {status, ok, headers.get, text, json, bytes}
//   spoor.state.get(key) / set(key, value) / all()
//                                    JSON values this script keeps between runs
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
    // Not instanceof: bytes from the page's fetch belong to another compartment,
    // and get copied before use.
    const isBuffer = Object.prototype.toString.call(data) === "[object ArrayBuffer]";
    if (isBuffer || ArrayBuffer.isView(data)) {
      const buf = isBuffer ? data : data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength);
      return __send("save", { name, data: __b64(new Uint8Array(structuredClone(buf))), encoding: "base64" });
    }
    return __send("save", { name, data: JSON.stringify(data, null, 2), encoding: "utf8" });
  },
  download: (url, opts) => {
    const o = typeof opts === "string" ? { path: opts } : { ...(opts || {}) };
    if (o.headers instanceof Headers) o.headers = Object.fromEntries(o.headers);
    if (o.body instanceof URLSearchParams) o.body = o.body.toString();
    return __send("download", { ...o, url: new URL(url, location.href).href });
  },
  html: async (url, init) => { const r = await __fetch(url, init); return __parse(await r.text(), r.url || new URL(url, location.href).href); },
  tab: async url => {
    const href = new URL(url, location.href).href;
    const { tab } = await __send("tab.open", { url: href });
    const tfetch = async (u, init) => {
      const r = await __send("tab.fetch", { tab, url: String(u), init: __init(init) });
      return __response(r);
    };
    return Object.freeze({
      fetch: tfetch,
      html: async (u, init) => { const r = await tfetch(u, init); return __parse(await r.text(), r.url); },
      eval: (code, args) => __send("tab.eval", { tab, code: typeof code === "function" ? "return (" + code + ")(args);" : code, args: __plain(args) }),
      urls: () => __send("tab.urls", { tab }),
      close: () => __send("tab.close", { tab }),
    });
  },
  state: Object.freeze({
    get: key => __send("state", { op: "get", key }),
    set: (key, value) => __send("state", { op: "set", key, value: __plain(value) }),
    all: () => __send("state", { op: "all" }),
  }),
  sleep: ms => __send("sleep", { ms }),
  checkpoint: () => __send("checkpoint"),
});
const __fetch = async (input, init) => { await __send("gate"); return __pageFetch(input, init); };
// What spoor.tab(...).fetch resolves to: the parts of a Response scripts use.
// (A real Response would come from the page's compartment and can't take our bytes.)
const __response = r => {
  const bytes = Uint8Array.from(atob(r.body), c => c.charCodeAt(0));
  const text = () => new TextDecoder().decode(bytes);
  const h = new Map(r.headers.map(([k, v]) => [k.toLowerCase(), v]));
  return Object.freeze({
    status: r.status, statusText: r.statusText, ok: r.status >= 200 && r.status < 300, url: r.url, redirected: r.redirected,
    headers: Object.freeze({ get: k => h.get(String(k).toLowerCase()) ?? null, has: k => h.has(String(k).toLowerCase()), entries: () => h.entries(), forEach: f => h.forEach((v, k) => f(v, k)), [Symbol.iterator]: () => h.entries() }),
    text: async () => text(),
    json: async () => JSON.parse(text()),
    arrayBuffer: async () => bytes.buffer,
    bytes: async () => bytes,
  });
};
const __parse = (text, base) => { const doc = new DOMParser().parseFromString(text, "text/html"); if (!doc.querySelector("base")) { const b = doc.createElement("base"); b.href = base; doc.head.prepend(b); } return doc; };
const __init = init => {
  if (!init) return {};
  const o = { ...init };
  if (o.headers instanceof Headers) o.headers = [...o.headers];
  if (o.body instanceof URLSearchParams) {
    o.body = o.body.toString();
    const h = new Headers(o.headers || {});
    if (!h.has("content-type")) h.set("content-type", "application/x-www-form-urlencoded;charset=UTF-8");
    o.headers = [...h];
  }
  if (o.body !== undefined && typeof o.body !== "string") throw new Error("spoor.tab fetch: body must be a string or URLSearchParams");
  delete o.signal;
  return o;
};
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
