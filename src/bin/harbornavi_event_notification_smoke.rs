use std::env;
use std::fs;

use harborbeacon_local_agent::connectors::notifications::{
    NotificationDeliveryError, NotificationDeliveryService, NotificationGatewayConfig,
};
use harborbeacon_local_agent::runtime::vision_event::{
    build_ha_mqtt_payload, build_local_vision_notification_intent, LocalVisionEvent,
    StoredLocalVisionEvent,
};
use harborbeacon_local_agent::service_auth::beacon_to_gate_sender_token;
use serde_json::{json, Value};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse(env::args().skip(1).collect())?;
    let stored = read_stored_event(&args.event_json)?;
    let intent = build_local_vision_notification_intent(&stored, &args.route_key)?;

    let mut output = json!({
        "ok": true,
        "mode": "dry_run",
        "classification": "notification-intent-built",
        "notification_request": intent.notification_request,
        "audit_record": intent.audit_record,
    });

    if let Some(gateway_url) = args.gateway_url.as_deref() {
        let token = args
            .bearer_token
            .clone()
            .map(Ok)
            .unwrap_or_else(beacon_to_gate_sender_token)?;
        let service = NotificationDeliveryService::from_config(
            NotificationGatewayConfig::new(gateway_url, token)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        match service.deliver(&intent.notification_request) {
            Ok(record) => {
                output["mode"] = json!("sent");
                output["classification"] = json!("notification-delivery-smoke");
                output["delivery_record"] = json!(record);
            }
            Err(error) => {
                output["ok"] = json!(false);
                output["mode"] = json!("delivery_failed");
                output["classification"] = json!(classify_delivery_error(&error));
                output["delivery_error"] = delivery_error_json(error);
            }
        }
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&output)
            .map_err(|error| format!("failed to serialize smoke output: {error}"))?
    );
    Ok(())
}

struct Args {
    event_json: String,
    route_key: String,
    gateway_url: Option<String>,
    bearer_token: Option<String>,
}

impl Args {
    fn parse(raw: Vec<String>) -> Result<Self, String> {
        let mut event_json = None;
        let mut route_key = None;
        let mut gateway_url = None;
        let mut bearer_token = None;
        let mut index = 0;
        while index < raw.len() {
            match raw[index].as_str() {
                "-h" | "--help" => return Err(help_text()),
                "--event-json" => {
                    index += 1;
                    event_json = raw.get(index).cloned();
                }
                "--route-key" => {
                    index += 1;
                    route_key = raw.get(index).cloned();
                }
                "--gateway-url" => {
                    index += 1;
                    gateway_url = raw.get(index).cloned();
                }
                "--bearer-token" => {
                    index += 1;
                    bearer_token = raw.get(index).cloned();
                }
                other => return Err(format!("unknown argument: {other}\n\n{}", help_text())),
            }
            index += 1;
        }
        Ok(Self {
            event_json: event_json.ok_or_else(help_text)?,
            route_key: route_key.ok_or_else(help_text)?,
            gateway_url,
            bearer_token,
        })
    }
}

fn read_stored_event(path: &str) -> Result<StoredLocalVisionEvent, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("failed to read {path}: {error}"))?;
    serde_json::from_str::<StoredLocalVisionEvent>(&text).or_else(|_| {
        let event = serde_json::from_str::<LocalVisionEvent>(&text)
            .map_err(|error| format!("failed to parse LocalVisionEvent fixture: {error}"))?;
        Ok(StoredLocalVisionEvent {
            received_at: "offline_fixture".to_string(),
            audit_record: json!({
                "audit_kind": "local_vision_event.offline_fixture",
                "event_id": event.event_id,
            }),
            ha_mqtt_payload: build_ha_mqtt_payload(&event),
            event,
        })
    })
}

fn classify_delivery_error(error: &NotificationDeliveryError) -> &'static str {
    match error {
        NotificationDeliveryError::RequestRejected { envelope, .. }
            if matches!(
                envelope.error.code.as_str(),
                "ROUTE_NOT_FOUND" | "ROUTE_EXPIRED"
            ) =>
        {
            "notification-channel-blocker"
        }
        NotificationDeliveryError::RequestRejected { envelope, .. }
            if envelope.error.code == "CONTRACT_VERSION_MISMATCH" =>
        {
            "notification-contract-error"
        }
        NotificationDeliveryError::RequestRejected { .. } => "notification-request-rejected",
        NotificationDeliveryError::Transport(_) => "gateway-unreachable",
        NotificationDeliveryError::MissingConfiguration(_) => "notification-configuration-error",
        NotificationDeliveryError::InvalidResponse(_) => "notification-contract-error",
    }
}

fn delivery_error_json(error: NotificationDeliveryError) -> Value {
    match error {
        NotificationDeliveryError::RequestRejected {
            status_code,
            envelope,
        } => json!({
            "kind": "request_rejected",
            "status_code": status_code,
            "code": envelope.error.code,
            "message": envelope.error.message,
            "trace_id": envelope.trace_id,
        }),
        NotificationDeliveryError::Transport(message) => {
            json!({"kind": "transport", "message": message})
        }
        NotificationDeliveryError::MissingConfiguration(message) => {
            json!({"kind": "missing_configuration", "message": message})
        }
        NotificationDeliveryError::InvalidResponse(message) => {
            json!({"kind": "invalid_response", "message": message})
        }
    }
}

fn help_text() -> String {
    "usage: harbornavi-event-notification-smoke --event-json <stored-event.json> --route-key <gw_route> [--gateway-url <url> --bearer-token <token>]\n\
     Builds a text-only HarborGate v2.0 notification request from a LocalVisionEvent fixture. Without --gateway-url it performs a dry run."
        .to_string()
}
