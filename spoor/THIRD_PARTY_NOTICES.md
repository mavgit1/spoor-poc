# Third-party notices

Spoor incorporates material from the following third-party projects.

---

## mitm2openapi (vendored REST engine)

Portions of the REST path discovery, schema inference, and OpenAPI generation
logic in `src/rest/` are derived from mitm2openapi.

| Field | Value |
|-------|--------|
| Upstream | https://github.com/Arkptz/mitm2openapi |
| Crates.io | https://crates.io/crates/mitm2openapi |
| Version at import | git commit `002d148927a055526088ddf572857a0778b94d95` |
| SPDX license | MIT |

### MIT License

Copyright (c) 2026 Arkptz

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

### Modifications

Arkite GmbH / Spoor contributors have adapted the upstream code for in-memory
HAR input, integration with Spoor’s CDP capture and curation UI, and removal
of CLI/mitmproxy-only modules. See `PLAN.md` for scope.

When syncing fixes from upstream, update the **Version at import** commit hash
above and retain MIT attribution in modified files.

---

## Other dependencies

Runtime dependencies (e.g. `graphql-parser`, `chromiumoxide`, `openapiv3`) are
used via `Cargo.toml` and carry their own licenses in `Cargo.lock` / crate
metadata. This file documents **vendored** (copied-in) code only.
