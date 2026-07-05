use std::env;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use harborbeacon_local_agent::runtime::admin_console::AdminConsoleStore;
use harborbeacon_local_agent::runtime::model_center::ADMIN_STATE_PATH_ENV;
use harborbeacon_local_agent::runtime::registry::DeviceRegistryStore;
use harborbeacon_local_agent::runtime::task_api::{
    TaskApiService, TaskTurnEnvelope, TaskTurnRequestAcceptance,
};
use harborbeacon_local_agent::runtime::task_session::TaskConversationStore;

const CONTRACT_VERSION: &str = "2.0";
const SERVICE_TOKEN_ENV: &str = "HARBOR_TASK_API_BEARER_TOKEN";
const HEADER_AUTHORIZATION: &str = "Authorization";
const HEADER_CONTRACT_VERSION: &str = "X-Contract-Version";

#[derive(Debug, Clone)]
struct Cli {
    bind: String,
    admin_state: PathBuf,
    device_registry: PathBuf,
    conversations: PathBuf,
    service_token: Option<String>,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:4175".to_string(),
            admin_state: PathBuf::from(".harborbeacon/admin-console.json"),
            device_registry: PathBuf::from(".harborbeacon/device-registry.json"),
            conversations: PathBuf::from(".harborbeacon/task-api-conversations.json"),
            service_token: None,
        }
    }
}

impl Cli {
    fn parse() -> Self {
        let args = env::args().skip(1).collect::<Vec<_>>();
        if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
            print_usage();
            std::process::exit(0);
        }

        let mut cli = Self::default();
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            match arg.as_str() {
                "--bind" => cli.bind = take_value(&args, &mut index, "--bind"),
                value if value.starts_with("--bind=") => {
                    cli.bind = value["--bind=".len()..].to_string();
                }
                "--admin-state" => {
                    cli.admin_state = PathBuf::from(take_value(&args, &mut index, "--admin-state"))
                }
                value if value.starts_with("--admin-state=") => {
                    cli.admin_state = PathBuf::from(value["--admin-state=".len()..].to_string())
                }
                "--device-registry" => {
                    cli.device_registry =
                        PathBuf::from(take_value(&args, &mut index, "--device-registry"))
                }
                value if value.starts_with("--device-registry=") => {
                    cli.device_registry =
                        PathBuf::from(value["--device-registry=".len()..].to_string())
                }
                "--conversations" => {
                    cli.conversations =
                        PathBuf::from(take_value(&args, &mut index, "--conversations"))
                }
                value if value.starts_with("--conversations=") => {
                    cli.conversations = PathBuf::from(value["--conversations=".len()..].to_string())
                }
                "--service-token" => {
                    cli.service_token = Some(take_value(&args, &mut index, "--service-token"))
                }
                value if value.starts_with("--service-token=") => {
                    cli.service_token = Some(value["--service-token=".len()..].to_string())
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                value if value.starts_with('-') => {
                    fail(&format!("unknown flag: {value}"));
                }
                value => {
                    fail(&format!("unexpected positional argument: {value}"));
                }
            }
            index += 1;
        }

        cli
    }
}

#[derive(Debug, Serialize)]
struct SharedHttpErrorDetail {
    code: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct SharedHttpErrorEnvelope {
    ok: bool,
    error: SharedHttpErrorDetail,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaskApiHttpServer {
    service: TaskApiService,
    service_token: String,
}

impl TaskApiHttpServer {
    pub fn new(service: TaskApiService, service_token: String) -> Self {
        Self {
            service,
            service_token,
        }
    }

    pub fn handle(&self, mut request: Request) {
        let method = request.method().clone();
        let path = request.url().split('?').next().unwrap_or("/").to_string();

        let response = match method {
            Method::Get if path == "/healthz" => ok_json(&json!({"status":"ok"})).boxed(),
            Method::Post if is_turn_api_path(&path) => self.handle_turn(&mut request).boxed(),
            Method::Options => no_content().boxed(),
            _ => shared_error_json(
                StatusCode(404),
                "ROUTE_NOT_FOUND",
                &format!("route not found: {path}"),
                None,
            )
            .boxed(),
        };

        let _ = request.respond(response);
    }

    fn handle_turn(&self, request: &mut Request) -> Response<Cursor<Vec<u8>>> {
        let headers = request.headers().to_vec();
        let body = match read_request_body(request) {
            Ok(body) => body,
            Err(error) => {
                return shared_error_json(StatusCode(500), "INFRASTRUCTURE_ERROR", &error, None)
            }
        };
        self.handle_turn_payload(&headers, &body)
    }

    fn handle_turn_payload(&self, headers: &[Header], body: &[u8]) -> Response<Cursor<Vec<u8>>> {
        let trace_id = trace_id_from_body(body);

        if !self.is_service_authorized(headers) {
            return service_auth_failed(trace_id);
        }

        let Some(contract_version) = header_value(headers, HEADER_CONTRACT_VERSION) else {
            return shared_error_json(
                StatusCode(400),
                "CONTRACT_VERSION_MISMATCH",
                &format!(
                    "missing {HEADER_CONTRACT_VERSION}; expected {HEADER_CONTRACT_VERSION}: {CONTRACT_VERSION}"
                ),
                trace_id,
            );
        };
        if contract_version != CONTRACT_VERSION {
            return shared_error_json(
                StatusCode(400),
                "CONTRACT_VERSION_MISMATCH",
                &format!(
                    "unsupported {HEADER_CONTRACT_VERSION}: {contract_version}; expected {CONTRACT_VERSION}"
                ),
                trace_id,
            );
        }

        let turn_envelope: TaskTurnEnvelope = match parse_json_body(body) {
            Ok(body) => body,
            Err(error) => {
                return shared_error_json(StatusCode(422), "VALIDATION_ERROR", &error, trace_id)
            }
        };
        if let Err(error) = validate_turn_request_contract(&turn_envelope) {
            return shared_error_json(StatusCode(422), "VALIDATION_ERROR", &error, trace_id);
        }

        match self.service.accept_or_replay_turn(&turn_envelope) {
            Ok(TaskTurnRequestAcceptance::Accept) => {
                let response = self.service.handle_turn(turn_envelope);
                ok_json(&response)
            }
            Ok(TaskTurnRequestAcceptance::Replay(response)) => ok_json(&response),
            Ok(TaskTurnRequestAcceptance::Conflict(message)) => shared_error_json(
                StatusCode(409),
                "IDEMPOTENCY_CONFLICT",
                &message,
                trace_id_from_body(body),
            ),
            Err(error) => shared_error_json(
                StatusCode(500),
                "INFRASTRUCTURE_ERROR",
                &error,
                trace_id_from_body(body),
            ),
        }
    }

    fn is_service_authorized(&self, headers: &[Header]) -> bool {
        header_value(headers, HEADER_AUTHORIZATION)
            .and_then(|value| parse_bearer_token(&value))
            .is_some_and(|value| value == self.service_token)
    }
}

pub(crate) fn is_turn_api_path(path: &str) -> bool {
    matches!(
        path,
        "/api/web/turns"
            | "/api/turns"
            | "/web/turns"
            | "/turns"
    )
}

fn main() {
    let cli = Cli::parse();
    let service_token = resolve_service_token(cli.service_token);
    let device_registry_path = resolve_state_path(&cli.device_registry);
    let admin_state_path = resolve_state_path(&cli.admin_state);
    std::env::set_var(ADMIN_STATE_PATH_ENV, &admin_state_path);
    let conversation_path = resolve_state_path(&cli.conversations);

    let registry_store = DeviceRegistryStore::new(device_registry_path);
    let admin_store = AdminConsoleStore::new(admin_state_path, registry_store);
    let conversation_store = TaskConversationStore::new(conversation_path);
    let service = TaskApiService::new(admin_store, conversation_store);
    let api = TaskApiHttpServer::new(service, service_token);

    let server = Server::http(&cli.bind).unwrap_or_else(|error| {
        panic!("failed to bind assistant task api on {}: {error}", cli.bind);
    });
    println!(
        "assistant-task-api listening on http://{} (contract {}, bearer token required)",
        cli.bind, CONTRACT_VERSION
    );

    for request in server.incoming_requests() {
        api.handle(request);
    }
}

fn resolve_service_token(cli_token: Option<String>) -> String {
    cli_token
        .or_else(|| env::var(SERVICE_TOKEN_ENV).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            eprintln!(
                "assistant-task-api requires a bearer token via --service-token or {SERVICE_TOKEN_ENV}"
            );
            std::process::exit(2);
        })
}

fn read_request_body(request: &mut Request) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    request
        .as_reader()
        .read_to_end(&mut body)
        .map_err(|error| format!("failed to read request body: {error}"))?;
    Ok(body)
}

fn parse_json_body<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, String> {
    serde_json::from_slice(body).map_err(|error| format!("invalid JSON body: {error}"))
}

fn trace_id_from_body(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            ["/turn/trace_id", "/trace_id"]
                .iter()
                .filter_map(|path| value.pointer(path).and_then(Value::as_str))
                .map(|value| value.trim().to_string())
                .find(|value| !value.is_empty())
        })
}

fn header_value(headers: &[Header], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|header| header.field.as_str().to_string().eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str().trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_bearer_token(value: &str) -> Option<String> {
    let prefix = "bearer ";
    value
        .trim()
        .to_ascii_lowercase()
        .starts_with(prefix)
        .then(|| value.trim()[prefix.len()..].trim().to_string())
        .filter(|value| !value.is_empty())
}

fn validate_turn_request_contract(envelope: &TaskTurnEnvelope) -> Result<(), String> {
    for (field, value) in [
        ("turn.turn_id", envelope.turn.turn_id.trim()),
        ("turn.trace_id", envelope.turn.trace_id.trim()),
        ("actor.user_id", envelope.actor.user_id.trim()),
        ("conversation.channel", envelope.conversation.channel.trim()),
        ("conversation.surface", envelope.conversation.surface.trim()),
        (
            "conversation.thread_id",
            envelope.conversation.thread_id.trim(),
        ),
        ("transport.route_key", envelope.transport.route_key.trim()),
    ] {
        if value.is_empty() {
            return Err(format!(
                "missing required field for HarborGate caller: {field}"
            ));
        }
    }

    if envelope
        .conversation
        .chat_type
        .trim()
        .eq_ignore_ascii_case("group")
    {
        return Err("group chat is out of scope for the v2.0 upgrade".to_string());
    }
    if !envelope
        .conversation
        .surface
        .trim()
        .eq_ignore_ascii_case("harborgate")
    {
        return Err("conversation.surface must be harborgate for the v2.0 upgrade".to_string());
    }
    Ok(())
}

fn resolve_state_path(preferred: &Path) -> PathBuf {
    preferred.to_path_buf()
}

fn take_value(args: &[String], index: &mut usize, flag: &str) -> String {
    *index += 1;
    if *index >= args.len() {
        fail(&format!("missing value for {flag}"));
    }
    args[*index].clone()
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}

fn print_usage() {
    eprintln!(
        "Usage: assistant-task-api [--bind ADDR] [--admin-state PATH] [--device-registry PATH] [--conversations PATH] [--service-token TOKEN]"
    );
}

fn ok_json(payload: &impl Serialize) -> Response<Cursor<Vec<u8>>> {
    json_response(StatusCode(200), payload)
}

fn no_content() -> Response<Cursor<Vec<u8>>> {
    let mut response = Response::from_data(Vec::new()).with_status_code(StatusCode(204));
    add_common_headers(&mut response);
    response
}

fn service_auth_failed(trace_id: Option<String>) -> Response<Cursor<Vec<u8>>> {
    let mut response = shared_error_json(
        StatusCode(401),
        "SERVICE_AUTH_FAILED",
        "missing or invalid bearer token",
        trace_id,
    );
    response.add_header(
        Header::from_bytes(b"WWW-Authenticate".as_slice(), b"Bearer".as_slice()).expect("header"),
    );
    response
}

fn shared_error_json(
    status: StatusCode,
    code: &'static str,
    message: &str,
    trace_id: Option<String>,
) -> Response<Cursor<Vec<u8>>> {
    json_response(
        status,
        &SharedHttpErrorEnvelope {
            ok: false,
            error: SharedHttpErrorDetail {
                code,
                message: message.to_string(),
            },
            trace_id,
        },
    )
}

fn json_response(status: StatusCode, payload: &impl Serialize) -> Response<Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(payload).unwrap_or_else(|_| {
        serde_json::to_vec(&json!({
            "ok": false,
            "error": {
                "code": "INFRASTRUCTURE_ERROR",
                "message": "serialize failed"
            }
        }))
        .unwrap_or_else(|_| b"{\"ok\":false}".to_vec())
    });
    let mut response = Response::from_data(body).with_status_code(status);
    add_common_headers(&mut response);
    response.add_header(
        Header::from_bytes(
            b"Content-Type".as_slice(),
            b"application/json; charset=utf-8".as_slice(),
        )
        .expect("header"),
    );
    response
}

fn add_common_headers<R: Read>(response: &mut Response<R>) {
    for header in [
        ("Access-Control-Allow-Origin", "*"),
        (
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization, X-Contract-Version",
        ),
        ("Access-Control-Allow-Methods", "GET, POST, OPTIONS"),
        ("Access-Control-Expose-Headers", "X-Contract-Version"),
        ("Cache-Control", "no-store"),
        ("X-Contract-Version", CONTRACT_VERSION),
    ] {
        response.add_header(
            Header::from_bytes(header.0.as_bytes(), header.1.as_bytes()).expect("header"),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Cursor, Read};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    use harborbeacon_local_agent::control_plane::approvals::ApprovalStatus;
    use harborbeacon_local_agent::control_plane::tasks::ExecutionRoute;
    use serde_json::{json, Value};
    use tiny_http::{Header, StatusCode};

    use super::{
        header_value, is_turn_api_path, parse_bearer_token, TaskApiHttpServer, HEADER_AUTHORIZATION,
        HEADER_CONTRACT_VERSION,
    };
    use harborbeacon_local_agent::runtime::admin_console::AdminConsoleStore;
    use harborbeacon_local_agent::runtime::registry::DeviceRegistryStore;
    use harborbeacon_local_agent::runtime::task_api::TaskApiService;
    use harborbeacon_local_agent::runtime::task_session::TaskConversationStore;

    static HARBOROS_HTTP_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn unique_path(prefix: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{unique}.json"))
    }

    fn build_server(token: &str) -> (TaskApiHttpServer, Vec<std::path::PathBuf>) {
        let admin_path = unique_path("assistant-task-api-admin");
        let registry_path = unique_path("assistant-task-api-registry");
        let conversation_path = unique_path("assistant-task-api-conversations");
        let service = TaskApiService::new(
            AdminConsoleStore::new(
                admin_path.clone(),
                DeviceRegistryStore::new(registry_path.clone()),
            ),
            TaskConversationStore::new(conversation_path.clone()),
        );
        (
            TaskApiHttpServer::new(service, token.to_string()),
            vec![admin_path, registry_path, conversation_path],
        )
    }

    fn header(name: &str, value: &str) -> Header {
        Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("header")
    }

    fn response_json(
        response: tiny_http::Response<Cursor<Vec<u8>>>,
    ) -> (StatusCode, Value, Vec<Header>) {
        let status = response.status_code();
        let headers = response.headers().to_vec();
        let mut reader = response.into_reader();
        let mut body = String::new();
        reader
            .read_to_string(&mut body)
            .expect("read response body");
        let payload = serde_json::from_str(&body).expect("parse response body json");
        (status, payload, headers)
    }

    fn cleanup(paths: Vec<std::path::PathBuf>) {
        for path in paths {
            let _ = fs::remove_file(path);
        }
    }

    fn reset_harboros_http_test_env() {
        for name in [
            "HARBOR_FORCE_MIDDLEWARE_ERROR",
            "HARBOR_URL",
            "HARBOR_MIDDLEWARE_URL",
            "HARBOR_API_KEY",
            "HARBOR_MIDDLEWARE_API_KEY",
            "HARBOR_USER",
            "HARBOR_PASSWORD",
            "HARBOR_MIDCLI_URL",
            "HARBOR_MIDCLI_USER",
            "HARBOR_MIDCLI_PASSWORD",
            "HARBOR_DISABLE_MIDDLEWARE",
            "HARBOR_DISABLE_MIDCLI",
            "HARBOR_MIDCLI_BIN",
            "HARBOR_MIDCLI_PASSTHROUGH",
        ] {
            std::env::remove_var(name);
        }
    }

    fn harbor_headers(token: &str) -> [Header; 2] {
        [
            header(HEADER_AUTHORIZATION, &format!("Bearer {token}")),
            header(HEADER_CONTRACT_VERSION, "2.0"),
        ]
    }

    fn harbor_task_request(
        task_id: &str,
        trace_id: &str,
        _step_id: &str,
        message_id: &str,
        domain: &str,
        action: &str,
        args: Value,
    ) -> Vec<u8> {
        serde_json::to_vec(&harbor_turn_value(
            task_id, trace_id, message_id, domain, action, args,
        ))
        .expect("encode request")
    }

    fn harbor_turn_value(
        task_id: &str,
        trace_id: &str,
        message_id: &str,
        domain: &str,
        action: &str,
        args: Value,
    ) -> Value {
        json!({
            "turn": {
                "turn_id": task_id,
                "trace_id": trace_id,
                "occurred_at": "2026-04-26T00:00:00Z",
                "retry_of": null
            },
            "actor": {
                "user_id": "user-1",
                "workspace_id": "home-1",
                "account_id": null
            },
            "conversation": {
                "handle": format!("conv-{task_id}"),
                "channel": "im_bridge",
                "surface": "harborgate",
                "thread_id": format!("chat-{task_id}"),
                "chat_type": "p2p"
            },
            "transport": {
                "route_key": format!("gw_route_{task_id}"),
                "message_id": message_id,
                "capabilities": {
                    "text": true,
                    "image": true,
                    "file": true,
                    "video": true
                },
                "metadata": {
                    "intent": {
                        "domain": domain,
                        "action": action,
                        "raw_text": format!("{domain}.{action}")
                    },
                    "entity_refs": {},
                    "args": args
                }
            },
            "input": {
                "text": format!("{domain}.{action}"),
                "parts": []
            },
            "continuation": null,
            "autonomy": {
                "level": "supervised"
            }
        })
    }

    #[test]
    fn bearer_parser_requires_bearer_prefix() {
        assert_eq!(
            parse_bearer_token("Bearer token-1"),
            Some("token-1".to_string())
        );
        assert_eq!(
            parse_bearer_token("bearer token-2"),
            Some("token-2".to_string())
        );
        assert_eq!(parse_bearer_token("token-3"), None);
    }

    #[test]
    fn turn_api_path_accepts_harboros_stripped_aliases() {
        assert!(is_turn_api_path("/api/web/turns"));
        assert!(is_turn_api_path("/api/turns"));
        assert!(is_turn_api_path("/web/turns"));
        assert!(is_turn_api_path("/turns"));
        assert!(!is_turn_api_path("/api/harbor-beacon/web/turns"));
        assert!(!is_turn_api_path("/api/beacon/state"));
    }

    #[test]
    fn task_endpoint_rejects_missing_auth() {
        let (server, paths) = build_server("shared-token");
        let (status, payload, headers) = response_json(server.handle_turn_payload(
            &[header(HEADER_CONTRACT_VERSION, "2.0")],
            br#"{"trace_id":"trace-auth"}"#,
        ));

        assert_eq!(status.0, 401);
        assert_eq!(payload["ok"], false);
        assert_eq!(payload["error"]["code"], "SERVICE_AUTH_FAILED");
        assert_eq!(payload["trace_id"], "trace-auth");
        assert_eq!(
            header_value(&headers, "WWW-Authenticate"),
            Some("Bearer".to_string())
        );
        cleanup(paths);
    }

    #[test]
    fn task_endpoint_rejects_contract_version_mismatch() {
        let (server, paths) = build_server("shared-token");
        let headers = [
            header(HEADER_AUTHORIZATION, "Bearer shared-token"),
            header(HEADER_CONTRACT_VERSION, "1.4"),
        ];
        let (status, payload, response_headers) =
            response_json(server.handle_turn_payload(&headers, br#"{"trace_id":"trace-version"}"#));

        assert_eq!(status.0, 400);
        assert_eq!(payload["error"]["code"], "CONTRACT_VERSION_MISMATCH");
        assert_eq!(payload["trace_id"], "trace-version");
        assert_eq!(
            header_value(&response_headers, HEADER_CONTRACT_VERSION),
            Some("2.0".to_string())
        );
        cleanup(paths);
    }

    #[test]
    fn task_endpoint_rejects_invalid_json_with_validation_error() {
        let (server, paths) = build_server("shared-token");
        let headers = [
            header(HEADER_AUTHORIZATION, "Bearer shared-token"),
            header(HEADER_CONTRACT_VERSION, "2.0"),
        ];
        let (status, payload, _) = response_json(server.handle_turn_payload(&headers, br#"{"#));

        assert_eq!(status.0, 422);
        assert_eq!(payload["error"]["code"], "VALIDATION_ERROR");
        cleanup(paths);
    }

    #[test]
    fn task_endpoint_rejects_harborgate_request_without_route_key() {
        let (server, paths) = build_server("shared-token");
        let headers = [
            header(HEADER_AUTHORIZATION, "Bearer shared-token"),
            header(HEADER_CONTRACT_VERSION, "2.0"),
        ];
        let mut body = harbor_turn_value(
            "task-http-no-route",
            "trace-http-no-message",
            "om_http_no_route",
            "system",
            "ping",
            json!({}),
        );
        body["transport"]["route_key"] = Value::String(String::new());
        let encoded = serde_json::to_vec(&body).expect("encode request");
        let (status, payload, _) = response_json(server.handle_turn_payload(&headers, &encoded));

        assert_eq!(status.0, 422);
        assert_eq!(payload["error"]["code"], "VALIDATION_ERROR");
        assert!(payload["error"]["message"]
            .as_str()
            .is_some_and(|value| value.contains("transport.route_key")));
        cleanup(paths);
    }

    #[test]
    fn task_endpoint_returns_business_response_when_headers_are_valid() {
        let (server, paths) = build_server("shared-token");
        let headers = [
            header(HEADER_AUTHORIZATION, "Bearer shared-token"),
            header(HEADER_CONTRACT_VERSION, "2.0"),
        ];
        let body = harbor_task_request(
            "task-http-ok",
            "trace-http-ok",
            "step-http-ok",
            "om_http_ok",
            "system",
            "ping",
            json!({}),
        );
        let (status, payload, response_headers) =
            response_json(server.handle_turn_payload(&headers, &body));

        assert_eq!(status.0, 200);
        assert_eq!(payload["turn"]["turn_id"], "task-http-ok");
        assert_eq!(payload["turn"]["trace_id"], "trace-http-ok");
        assert_eq!(payload["turn"]["status"], "failed");
        assert!(payload["reply"]["text"]
            .as_str()
            .is_some_and(|value| value.contains("system.ping")));
        assert_eq!(
            header_value(&response_headers, HEADER_CONTRACT_VERSION),
            Some("2.0".to_string())
        );
        cleanup(paths);
    }

    #[test]
    fn task_endpoint_dispatches_service_status_to_harboros_route() {
        let _guard = HARBOROS_HTTP_TEST_LOCK.lock().expect("lock");
        reset_harboros_http_test_env();
        let (server, paths) = build_server("shared-token");
        let body = harbor_task_request(
            "task-http-service-status",
            "trace-http-service-status",
            "step-http-service-status",
            "om_http_service_status",
            "service",
            "status",
            json!({"service_name": "ssh"}),
        );

        let (status, payload, _) =
            response_json(server.handle_turn_payload(&harbor_headers("shared-token"), &body));

        assert_eq!(status.0, 200);
        assert_eq!(payload["turn"]["status"], "completed");
        assert_eq!(payload["reply"]["kind"], "tool_result");
        assert_eq!(
            payload["observability"]["route_key"],
            "gw_route_task-http-service-status"
        );

        let task_step = server
            .service
            .conversation_store()
            .load_task_step("turn:task-http-service-status")
            .expect("load task step")
            .expect("task step");
        assert_eq!(task_step.route, ExecutionRoute::MiddlewareApi);
        assert_eq!(task_step.executor_used, "middleware_api");

        cleanup(paths);
        reset_harboros_http_test_env();
    }

    #[test]
    fn task_endpoint_falls_back_to_midcli_when_harbor_middleware_fails() {
        let _guard = HARBOROS_HTTP_TEST_LOCK.lock().expect("lock");
        reset_harboros_http_test_env();
        std::env::set_var("HARBOR_FORCE_MIDDLEWARE_ERROR", "1");
        let (server, paths) = build_server("shared-token");
        let body = harbor_task_request(
            "task-http-service-fallback",
            "trace-http-service-fallback",
            "step-http-service-fallback",
            "om_http_service_fallback",
            "service",
            "status",
            json!({"service_name": "ssh"}),
        );

        let (status, payload, _) =
            response_json(server.handle_turn_payload(&harbor_headers("shared-token"), &body));

        assert_eq!(status.0, 200);
        assert_eq!(payload["turn"]["status"], "completed");
        assert_eq!(payload["reply"]["kind"], "tool_result");
        assert_eq!(
            payload["observability"]["route_key"],
            "gw_route_task-http-service-fallback"
        );

        let task_step = server
            .service
            .conversation_store()
            .load_task_step("turn:task-http-service-fallback")
            .expect("load task step")
            .expect("task step");
        assert_eq!(task_step.route, ExecutionRoute::Midcli);
        assert_eq!(task_step.executor_used, "midcli");

        cleanup(paths);
        reset_harboros_http_test_env();
    }

    #[test]
    fn task_endpoint_records_restart_approval_gate_for_harboros_tasks() {
        let _guard = HARBOROS_HTTP_TEST_LOCK.lock().expect("lock");
        reset_harboros_http_test_env();
        let (server, paths) = build_server("shared-token");
        let body = harbor_task_request(
            "task-http-service-restart",
            "trace-http-service-restart",
            "step-http-service-restart",
            "om_http_service_restart",
            "service",
            "restart",
            json!({"service_name": "ssh"}),
        );

        let (status, payload, _) =
            response_json(server.handle_turn_payload(&harbor_headers("shared-token"), &body));

        assert_eq!(status.0, 200);
        assert_eq!(payload["turn"]["status"], "needs_input");
        assert_eq!(payload["reply"]["kind"], "frame_prompt");
        assert_eq!(payload["active_frame"]["kind"], "task.needs_input");
        assert!(payload["active_frame"]["expected_reply"][0]
            .as_str()
            .is_some_and(|value| value.starts_with("approval_token ")));

        let approvals = server
            .service
            .conversation_store()
            .approvals_for_task("task-http-service-restart")
            .expect("load approvals");
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].status, ApprovalStatus::Pending);

        cleanup(paths);
        reset_harboros_http_test_env();
    }

    #[test]
    fn task_endpoint_dispatches_files_list_to_harboros_route() {
        let _guard = HARBOROS_HTTP_TEST_LOCK.lock().expect("lock");
        reset_harboros_http_test_env();
        let (server, paths) = build_server("shared-token");
        let body = harbor_task_request(
            "task-http-files-list",
            "trace-http-files-list",
            "step-http-files-list",
            "om_http_files_list",
            "files",
            "list",
            json!({"path": "/mnt"}),
        );

        let (status, payload, _) =
            response_json(server.handle_turn_payload(&harbor_headers("shared-token"), &body));

        assert_eq!(status.0, 200);
        assert_eq!(payload["turn"]["status"], "completed");
        assert_eq!(payload["reply"]["kind"], "tool_result");
        assert_eq!(
            payload["observability"]["route_key"],
            "gw_route_task-http-files-list"
        );

        let task_step = server
            .service
            .conversation_store()
            .load_task_step("turn:task-http-files-list")
            .expect("load task step")
            .expect("task step");
        assert_eq!(task_step.route, ExecutionRoute::MiddlewareApi);
        assert_eq!(task_step.executor_used, "middleware_api");

        cleanup(paths);
        reset_harboros_http_test_env();
    }

    #[test]
    fn task_endpoint_rejects_legacy_im_gateway_surface_after_v20_cutover() {
        let (server, paths) = build_server("shared-token");
        let headers = [
            header(HEADER_AUTHORIZATION, "Bearer shared-token"),
            header(HEADER_CONTRACT_VERSION, "2.0"),
        ];
        let mut body = harbor_turn_value(
            "task-http-legacy-surface",
            "trace-http-legacy-surface",
            "om_http_legacy_surface",
            "system",
            "ping",
            json!({}),
        );
        body["conversation"]["surface"] = Value::String("im_gateway".to_string());
        let encoded = serde_json::to_vec(&body).expect("encode request");
        let (status, payload, _) = response_json(server.handle_turn_payload(&headers, &encoded));

        assert_eq!(status.0, 422);
        assert_eq!(payload["error"]["code"], "VALIDATION_ERROR");
        assert!(payload["error"]["message"]
            .as_str()
            .is_some_and(|value| value.contains("conversation.surface")));
        cleanup(paths);
    }

    #[test]
    fn resolve_state_path_keeps_harborbeacon_location() {
        let preferred = std::env::temp_dir()
            .join(".harborbeacon")
            .join("admin-console.json");
        let resolved = super::resolve_state_path(&preferred);
        assert_eq!(resolved, preferred);
    }

    #[test]
    fn task_endpoint_rejects_conflicting_reuse_of_task_id() {
        let (server, paths) = build_server("shared-token");
        let headers = [
            header(HEADER_AUTHORIZATION, "Bearer shared-token"),
            header(HEADER_CONTRACT_VERSION, "2.0"),
        ];
        let initial = harbor_task_request(
            "task-http-conflict",
            "trace-http-conflict",
            "step-http-conflict",
            "om_http_conflict",
            "system",
            "ping",
            json!({}),
        );
        let mut conflicting = harbor_turn_value(
            "task-http-conflict",
            "trace-http-conflict",
            "om_http_conflict",
            "system",
            "ping",
            json!({}),
        );
        conflicting["input"]["text"] = Value::String("ping again".to_string());
        conflicting["transport"]["metadata"]["intent"]["raw_text"] =
            Value::String("ping again".to_string());
        let conflicting = serde_json::to_vec(&conflicting).expect("encode conflicting request");

        let first = response_json(server.handle_turn_payload(&headers, &initial));
        assert_eq!(first.0 .0, 200);

        let (status, payload, _) =
            response_json(server.handle_turn_payload(&headers, &conflicting));
        assert_eq!(status.0, 409);
        assert_eq!(payload["error"]["code"], "IDEMPOTENCY_CONFLICT");

        cleanup(paths);
    }

    #[test]
    fn task_endpoint_replays_original_response_when_turn_id_is_reused() {
        let (server, paths) = build_server("shared-token");
        let headers = [
            header(HEADER_AUTHORIZATION, "Bearer shared-token"),
            header(HEADER_CONTRACT_VERSION, "2.0"),
        ];
        let first = harbor_task_request(
            "task-http-step-a",
            "trace-http-step-a",
            "step-a",
            "om_http_step_a",
            "system",
            "ping",
            json!({}),
        );
        let second = harbor_task_request(
            "task-http-step-b",
            "trace-http-step-b",
            "step-b",
            "om_http_step_b",
            "system",
            "status",
            json!({}),
        );

        let first_response = response_json(server.handle_turn_payload(&headers, &first));
        assert_eq!(first_response.0 .0, 200);
        assert_eq!(first_response.1["turn"]["status"], "failed");
        assert!(first_response.1["reply"]["text"]
            .as_str()
            .is_some_and(|value| value.contains("system.ping")));

        let second_response = response_json(server.handle_turn_payload(&headers, &second));
        assert_eq!(second_response.0 .0, 200);
        assert_eq!(second_response.1["turn"]["status"], "failed");
        assert!(second_response.1["reply"]["text"]
            .as_str()
            .is_some_and(|value| value.contains("system.status")));

        let replay_response = response_json(server.handle_turn_payload(&headers, &first));
        assert_eq!(replay_response.0 .0, 200);
        assert_eq!(replay_response.1["turn"]["status"], "failed");
        assert!(replay_response.1["reply"]["text"]
            .as_str()
            .is_some_and(|value| value.contains("system.ping")));
        assert!(server
            .service
            .conversation_store()
            .load_task_step("step_01")
            .expect("load raw step id")
            .is_none());

        cleanup(paths);
    }
}
