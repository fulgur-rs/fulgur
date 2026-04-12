# Threat Model: SaaS Multi-Tenant Invoice Generator

This document describes the threat model for deploying fulgur as the PDF
engine in a SaaS multi-tenant invoice (document) generator. The primary
use case is: **end-users design HTML templates via a WYSIWYG editor, supply
JSON data per document, and the server renders PDFs in batch.**

This analysis drives the design of the HTML sanitiser (fulgur-me2) and
the MiniJinja sandbox (fulgur-q1z).

Japanese version: [threat-model.ja.md](threat-model.ja.md)

## Architecture overview

```text
End-user (browser)
  │
  ├─ WYSIWYG editor ──► HTML template  ──┐
  │                                       ▼
  └─ Form / API      ──► JSON data    ──► fulgur Engine ──► PDF
                                            ▲
                                       SaaS operator
                                       (fonts, CSS, images)
```

Three distinct input channels feed the engine:

| Channel | Controlled by | Trust level |
|---|---|---|
| HTML template | End-user (via WYSIWYG) | **Untrusted** |
| JSON data | End-user (via API / form) | **Untrusted** |
| Asset bundle (fonts, CSS, images) | SaaS operator | Trusted |

## Threat actors

### A1 — Malicious template author

An end-user who crafts or modifies HTML templates through the WYSIWYG
editor (or by intercepting the API) to:

- Execute JavaScript in the rendering pipeline
- Reference external resources (SSRF)
- Escape the asset sandbox to read server files (path traversal)
- Leak data from other tenants
- Cause denial of service

### A2 — Malicious JSON data supplier

An end-user who submits crafted JSON payloads to:

- Inject HTML/JS via template variable expansion
- Trigger excessive loop iterations or deep recursion in the template engine
- Cause memory exhaustion through very large values

### A3 — Compromised SaaS operator

An operator (or supply-chain attacker who compromises operator assets) who:

- Injects malicious CSS or fonts into the shared asset bundle
- This actor is **lower priority** because operator assets are trusted by
  design; defence here focuses on limiting blast radius rather than
  preventing access

## Vulnerability categories and mitigations

### V1 — Script injection (XSS equivalent)

**Threat:** Template contains `<script>`, `<iframe>`, `on*` event
attributes, or `javascript:` URLs that could execute code if fulgur ever
gains a JS runtime, or if the generated PDF is rendered in a context that
interprets HTML.

**Current state:** Blitz does not execute JavaScript. There is no JS
runtime in the pipeline. However, a defence-in-depth approach is
warranted because:

- Future Blitz versions or alternative renderers might add scripting
- Generated PDFs viewed in certain tools could interpret embedded scripts
- The templates may be previewed in a browser before rendering

**Mitigations:**

- **[Planned: fulgur-me2]** HTML sanitiser DomPass that strips:
  - `<script>`, `<iframe>`, `<object>`, `<embed>`, `<applet>`
  - `<link rel="import">`, `<base>` (URL hijacking)
  - All `on*` event handler attributes
  - `javascript:`, `vbscript:`, `data:text/html` URLs in `href`/`src`/`action`
- **[Existing]** MiniJinja auto-escaping (`AutoEscape::Html`) escapes
  `<`, `>`, `&`, `"` in all `{{ variable }}` output
  (`crates/fulgur/src/template.rs:90`)

### V2 — Server-Side Request Forgery (SSRF)

**Threat:** Template references `http://`, `https://`, or other network
URLs via `<link>`, `<img src>`, CSS `url()`, or `@import`, causing the
server to make outbound requests to internal services or metadata
endpoints.

**Current state:** fulgur is **offline by design**.

**Mitigations:**

- **[Existing]** `FulgurNetProvider` (`crates/fulgur/src/net.rs`) only
  accepts `file://` URLs; all other schemes are silently dropped
- **[Existing]** Path traversal protection: resolved file paths must
  canonicalise inside the configured `base_path`
- **[Planned: fulgur-me2]** Sanitiser strips `<link rel="stylesheet"
  href="http://...">` and similar external-pointing attributes
- **Recommendation:** SaaS operators should additionally run fulgur in a
  network-isolated container (no outbound connectivity) as a
  defence-in-depth measure

### V3 — Path traversal / local file read

**Threat:** Template uses `<link href="file:///etc/passwd">` or relative
paths like `../../secrets/key.pem` to exfiltrate server files.

**Mitigations:**

- **[Existing]** `FulgurNetProvider::resolve_local_path`
  (`crates/fulgur/src/net.rs:90-101`) canonicalises paths and rejects
  anything outside `base_path`; symlink traversal is also blocked by
  `canonicalize()`
- **[Existing]** `AssetBundle` image lookup uses explicit name→data
  mapping; no filesystem reads at render time for images

### V4 — Template engine abuse (DoS)

**Threat:** Malicious templates or JSON data cause excessive resource
consumption:

- `{% for i in range(100000000) %}` — CPU/memory exhaustion
- Deeply nested object access `{{ a.b.c.d.e.f.g... }}`
- Very large JSON values (multi-MB strings) expanded in loops
- Recursive `{% include %}` / `{% import %}` (if enabled)

**Current state:** MiniJinja `Environment::new()` has no resource limits
by default (`crates/fulgur/src/template.rs:89`).

**Mitigations:**

- **[Planned: fulgur-q1z]** MiniJinja sandbox mode with:
  - Loop iteration limit (e.g. 10,000 iterations)
  - Rendering timeout (wall-clock)
  - Template source size limit
  - Attribute access depth limit
  - `include`/`import` disabled (currently achieved by not registering a
    loader, but should be explicitly enforced)
- **Recommendation:** SaaS operators should enforce JSON payload size
  limits at the API gateway layer before reaching fulgur

### V5 — Cross-tenant data leakage

**Threat:** Tenant A's template or data somehow accesses Tenant B's
assets, rendered output, or template variables.

**Mitigations:**

- **[By design]** Each `Engine` instance is independent; there is no
  shared mutable state between renders (no global template registry, no
  shared asset cache)
- **[By design]** `AssetBundle` is explicitly constructed per render;
  assets not added to the bundle are inaccessible
- **Recommendation:** SaaS operators must ensure tenant isolation at the
  orchestration layer (separate `Engine` instances, no shared temp
  directories)

### V6 — PDF-level attacks

**Threat:** Crafted input produces a PDF that exploits vulnerabilities in
PDF viewers (e.g. malicious JavaScript in PDF, font parsing exploits,
action triggers).

**Mitigations:**

- **[By design]** Krilla generates a constrained subset of PDF; it does
  not embed JavaScript actions, form fields, or launch actions
- **[By design]** Font subsetting reduces attack surface on font parsers
- **Low risk** given fulgur's output is static content (text, images,
  vector paths)

### V7 — Rendering resource exhaustion (non-template)

**Threat:** Even without template engine abuse, a malicious HTML document
can cause excessive resource use:

- Very deeply nested DOM (stack overflow in layout/pagination)
- Extremely large inline images (base64-encoded in HTML)
- Thousands of pages generated from a single document

**Mitigations:**

- **[Recommendation]** SaaS operators should enforce:
  - HTML input size limit
  - Maximum page count per render
  - Render timeout (process-level, e.g. via container runtime limits)
  - Memory limit (via cgroup / container)

## Out of scope

The following are **not** addressed by fulgur and are the responsibility
of the SaaS operator:

| Item | Reason |
|---|---|
| Browser-level sandboxing of WYSIWYG editor | Client-side concern; fulgur is server-side only |
| Full CSS parser attack defence | Blitz/stylo CSS parser hardening is an upstream concern |
| Authentication / authorisation | Application layer responsibility |
| Encryption of PDFs at rest | Operator's storage layer concern |
| PDF digital signatures | Separate feature track, not a rendering security control |
| Network-level isolation | Infrastructure concern (but recommended above) |

## Summary matrix

| ID | Category | Severity | Status | Tracking |
|---|---|---|---|---|
| V1 | Script injection | High | Partially mitigated (auto-escape); sanitiser planned | fulgur-me2 |
| V2 | SSRF | High | **Mitigated** (offline-only NetProvider) | — |
| V3 | Path traversal | High | **Mitigated** (canonicalise + base_path check) | — |
| V4 | Template DoS | Medium | Unmitigated; sandbox planned | fulgur-q1z |
| V5 | Cross-tenant leakage | High | **Mitigated by design** (no shared state) | — |
| V6 | PDF viewer exploits | Low | **Mitigated by design** (constrained PDF output) | — |
| V7 | Rendering resource exhaustion | Medium | Operator responsibility (limits) | — |

## Revision history

| Date | Author | Change |
|---|---|---|
| 2026-04-12 | Mitsuru Hayasaka | Initial version |
