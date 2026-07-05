# Harbor App Contract v1

## Status

This is the first HarborOS application contract for apps that run beside
HarborBeacon on a single HarborOS host.

It does not change the HarborBeacon <-> HarborGate v2.0 IM contract. It also
does not make application containers the owner of HarborBeacon business truth.

The first implementation of this contract is metadata/control-plane only.
HarborBeacon validates manifests, stores the app registry, records audit
metadata, and returns HarborOS execution plans. It does not execute Docker,
nginx, or tunnel commands in App Manager v1.

## Purpose

HarborOS now carries more than one demo or product surface on the same host. The
goal of this contract is to make every business application declare how it is
installed, routed, authorized, and observed before it is allowed to call shared
HarborBeacon capabilities.

The first app batch is:

- `finance-audit`
- `navi-card`
- `outreach`

`home-event-rule-bridge` / HA Bridge is intentionally out of scope for this
batch. It remains a public GitHub-first Home Assistant tool unless a later
optional HarborOS packaging adapter is approved.

## Boundary

HarborOS owns:

- OS services
- Docker Compose runtime
- app volumes
- nginx route materialization
- optional tunnel materialization
- host-level logs and resource inspection

HarborBeacon owns:

- app registry truth
- app permissions and capability grants
- NSP, Router, Model Center, compliance, redaction, approval, audit, and RAG
  capability policy
- app lifecycle audit records

HarborGate owns:

- IM transport
- route registry
- platform credentials
- outbound delivery formatting

Harbor apps must not:

- read or write `.harborbeacon/*.json`
- read HarborGate raw credentials
- share HarborBeacon or HarborGate runtime state files
- call Docker, nginx, or tunnel tools directly when managed by HarborOS
- bypass approval gates for high-risk operations

## Manifest

Each Harbor app must ship an `app.manifest.yaml` with these fields:

```yaml
contract: harbor.app.v1
id: finance-audit
name: Finance Audit Demo
version: 0.1.0
image: harbor.local/finance-audit:0.1.0
build: null
routes:
  - path_prefix: /apps/finance-audit/
    service_port: 4190
    strip_prefix: false
health:
  path: /healthz
  port: 4190
  interval_seconds: 30
permissions:
  - capability: platform.models.infer
    actions: [call]
    risk: medium
volumes:
  - name: data
    mount_path: /data
    kind: data
platform_capabilities:
  - platform.models.infer
  - platform.audit.events.write
exposure: lan
```

Exactly one of `image` or `build` must be present.

Allowed exposure values:

- `none`: no route is exposed
- `lan`: route is exposed on the local HarborOS LAN entrypoint
- `tunnel`: route may be exposed through an explicitly enabled tunnel

## Default Paths

The default application paths are:

```text
/var/lib/harbor/apps/<app_id>/        compose/spec/runtime metadata
/mnt/software/harbor-apps/<app_id>/   app data
/etc/harbor/apps/<app_id>.env         app env/secrets
```

Apps must mount only their own data root unless a future contract explicitly
grants a shared read-only root.

## Manager API

HarborBeacon exposes app control-plane APIs. HarborOS System Domain executors
materialize the Docker Compose, nginx, volume, and tunnel actions.

```text
GET  /api/apps
POST /api/apps/install
POST /api/apps/{id}/start
POST /api/apps/{id}/stop
POST /api/apps/{id}/restart
GET  /api/apps/{id}/health
GET  /api/apps/{id}/logs
POST /api/apps/{id}/exposure
```

These APIs must preserve metadata-only readiness and audit output. They must not
return raw app secrets or HarborGate raw credentials.

In App Manager v1:

- `POST /api/apps/install` validates and registers the manifest, then returns an
  install execution plan.
- lifecycle APIs return command previews and record metadata-only audit entries.
- `GET /api/apps/{id}/health` and `GET /api/apps/{id}/logs` return read-only
  previews with `unknown` runtime status until the materializer is active.
- `tunnel` exposure returns `approval_required`; no tunnel is opened by v1.

## Platform Capability API

Apps call HarborBeacon through HTTP APIs with least-privilege app tokens:

```text
POST /api/platform/nsp/plan
POST /api/platform/router/route
POST /api/platform/privacy/redact
POST /api/platform/compliance/evaluate
POST /api/platform/models/infer
POST /api/platform/approval/tickets
GET  /api/platform/audit/events
```

The app token must only authorize capabilities declared by the app manifest.

## Routing

The first-batch route prefixes are:

```text
/apps/finance-audit/*
/apps/navi-card/*
/apps/outreach/*
/api/beacon/*
```

`/api/beacon/*` remains the HarborBeacon platform API surface. Business apps do
not own that prefix.

## Release Gate

A Harbor app release is acceptable only when:

- the manifest validates under this contract
- the app starts through Docker Compose
- health check succeeds through the LAN route
- app token cannot call undeclared capabilities
- tunnel exposure is disabled by default
- opt-in tunnel exposure is audited
- app install, stop, uninstall, or rollback does not restart Beacon or Gate
- no app can read another app data root
- no app can read HarborGate raw credentials
