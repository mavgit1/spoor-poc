// Recordings must not keep what the human typed to log in. Request-body fields
// whose names look like passwords or one-time codes are replaced. Tokens the
// *site* hands out stay: they are the evidence auth analysis needs.

import type { Body } from "../../src/shared/types.ts";

export const REDACTED = "[redacted by spoor]";
const SECRET_FIELD = /^(pass(word)?|passwd|pwd|.*[_-]?password|otc|otp|totp|pin|mfa[_-]?code|verification[_-]?code|one[_-]?time[_-]?code|credentials?)$/i;

export function isSecretField(name: string): boolean {
  return SECRET_FIELD.test(name.split(/[.\[\]]/).filter(Boolean).pop() ?? name);
}

function redactJson(v: unknown): unknown {
  if (Array.isArray(v)) return v.map(redactJson);
  if (v && typeof v === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, val] of Object.entries(v)) out[k] = isSecretField(k) && (typeof val === "string" || typeof val === "number") ? REDACTED : redactJson(val);
    return out;
  }
  return v;
}

export function redactBody(body: Body, contentType: string | undefined): Body {
  if (body.kind !== "text") return body;
  const text = body.text;
  const ct = (contentType ?? "").toLowerCase();
  const trimmed = text.trimStart();
  if (ct.includes("json") || trimmed.startsWith("{") || trimmed.startsWith("[")) {
    try {
      const redacted = JSON.stringify(redactJson(JSON.parse(text)));
      return redacted === JSON.stringify(JSON.parse(text)) ? body : { ...body, text: redacted };
    } catch {
      // not JSON after all
    }
  }
  if (ct.includes("x-www-form-urlencoded") || (!ct && /^[^\s=&]+=[^\s]*(&[^\s=&]+=[^\s]*)*$/.test(text))) {
    const params = new URLSearchParams(text);
    let changed = false;
    for (const k of [...new Set(params.keys())]) {
      if (isSecretField(k)) {
        params.set(k, REDACTED);
        changed = true;
      }
    }
    return changed ? { ...body, text: params.toString() } : body;
  }
  return body;
}
