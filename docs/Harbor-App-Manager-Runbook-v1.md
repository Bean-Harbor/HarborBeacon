# Harbor App Manager Runbook v1

## Status

This runbook defines the first single-host HarborOS app management lane.

It is a platform lane, not a business app. It keeps the existing core shape:

- `harboros-beacon` stays a deb/systemd service
- `harboros-im-gate` stays a deb/systemd service
- business apps run through Docker Compose

App Manager v1 is plan-first. It stores app registry truth, validates manifests,
returns execution plans, and records metadata-only audit events. It deliberately
does not execute Docker Compose, rewrite nginx, create volumes, or open tunnels.
Those materializer steps belong to the next HarborOS System Domain increment.

## Ownership

`harbor-framework` owns:

- app manifest validation
- app registry truth
- least-privilege platform capability grants
- approval and audit semantics
- App Manager northbound API shape

`harbor-hos-control` owns:

- Docker Compose materialization
- nginx route materialization
- app volume creation and validation
- service lifecycle inspection
- host logs and resource inspection
- optional tunnel materialization

`harbor-architect` owns:

- release gates
- rollback gates
- cross-lane acceptance
- boundary arbitration

## Directory Layout

```text
/var/lib/harbor/apps/<app_id>/
  app.manifest.yaml
  compose.yaml
  runtime.json

/mnt/software/harbor-apps/<app_id>/
  data/
  cache/
  exports/

/etc/harbor/apps/<app_id>.env
```

The app data root is app-private. The manager must reject manifests that require
direct mounts into HarborBeacon, HarborGate, or another app root.

## Lifecycle

### Install

1. Validate `app.manifest.yaml` against `harbor.app.v1`.
2. Register app metadata and default paths in HarborBeacon admin state.
3. Return a managed Compose/route execution plan without running it.
4. Write an audit record with app id, version, capabilities, exposure, and
   operator identity.

### Start

1. Confirm manifest still validates.
2. Return the managed Compose start command preview.
3. Record `plan_ready` status and audit metadata.
4. Defer actual Docker execution and LAN health checks to the materializer
   increment.

### Stop / Restart

Stop and restart operate only on the selected app project. They must not restart
`harboros-beacon` or `harboros-im-gate`.

### Logs

Logs are read-only. The manager returns bounded recent logs and redacts values
that match app secret names.

### Exposure

`lan` is the default exposure for routed apps.

`tunnel` must be explicitly enabled per app. Enabling tunnel exposure requires:

- an approval ticket
- HTTPS or tunnel provider TLS
- route allowlist
- audit event
- visible warning that the app is reachable from outside the LAN

In v1, a tunnel request returns `approval_required` and records the audit marker.
It does not create or update any tunnel.

## First Batch

### Finance Audit

Route:

```text
/apps/finance-audit/
```

Expected platform capabilities:

- `platform.models.infer`
- `platform.audit.events.write`
- `platform.privacy.redact`

### NAVI Card

Route:

```text
/apps/navi-card/
```

Expected platform capabilities:

- `platform.nsp.plan`
- `platform.models.infer`
- `platform.audit.events.write`

### Outreach

Route:

```text
/apps/outreach/
```

Expected platform capabilities:

- `platform.nsp.plan`
- `platform.router.route`
- `platform.compliance.evaluate`
- `platform.privacy.redact`
- `platform.approval.tickets.create`
- `platform.audit.events.write`

## Out Of Scope

HA Bridge / `home-event-rule-bridge` is not in the first Harbor App batch. It
keeps its GitHub-first README, Docker Compose quick start, `NSP_PROFILE`, and
public Home Assistant positioning.

## Smoke

Run this smoke after implementing the runtime wiring:

1. `GET /api/apps` returns the three first-batch app ids.
2. Each app route returns HTTP 200 or an app-specific healthy status page.
3. Stopping `finance-audit` does not affect `navi-card`, `outreach`,
   `harboros-beacon`, or `harboros-im-gate`.
4. A token for `navi-card` cannot call `platform.compliance.evaluate`.
5. Tunnel exposure is disabled by default for every app.
6. Enabling tunnel exposure requires approval and emits audit metadata.
