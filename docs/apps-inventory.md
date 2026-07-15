# HarborOS .82 Apps Inventory

## Status

This is the initial planning inventory for the HarborOS `.82` dogfood host. It
is based on current repo evidence and prior live-session notes, not a fresh
same-day SSH inspection.

Before using this as release evidence, re-check `.82` directly and replace the
`needs_live_check` entries with current process, port, route, and data-root
truth.

## Core Platform

| Component | Current role | Target management |
|---|---|---|
| HarborBeacon / Harbor Assistant | Business truth, NSP, Router, Model Center, RAG, Privacy Gateway, approval, audit | deb/systemd core service |
| HarborGate / IM Gate | IM transport, route registry, platform credentials, delivery formatting | deb/systemd core service |
| nginx / WebUI entry | LAN route and same-origin API proxy | HarborOS system domain |

## First Harbor Apps

| App id | Source | Current evidence | Target route | Target management | Status |
|---|---|---|---|---|---|
| `finance-audit` | `C:\Users\beanw\OpenSource\FinanceAuditDemo` | Local-only `.82` deployment used `LOCAL_LLM_BASE_URL`, `LOCAL_EMBED_BASE_URL`, `LOCAL_VLM_BASE_URL`, PaddleOCR, evidence refs, and run evidence | `/apps/finance-audit/` | Docker Compose app | needs manifest and route normalization |
| `navi-card` | needs repo/path confirmation | Mentioned as NAVI card fortune/game demo on `.82` | `/apps/navi-card/` | Docker Compose app | needs live inventory |
| `outreach` | planning-stage app | Creator outreach/operator tool planned to call NSP, Router, compliance, redaction, approval, and audit | `/apps/outreach/` | Docker Compose app | needs app scaffold |

## Out Of Scope For This Batch

| Project | Reason |
|---|---|
| `home-event-rule-bridge` / HA Bridge | Public GitHub-first Home Assistant tool for user validation and stars; keep its standalone Docker quick start and public README positioning. Add only an optional HarborOS adapter in a later batch if needed. |

## Live Check TODO

For each first-batch app, refresh these fields on `.82`:

- process manager
- listen address and port
- nginx route
- data root
- model endpoint usage
- external credentials
- current health endpoint
- rollback path
- whether stop/restart affects `harboros-beacon` or `harboros-im-gate`
