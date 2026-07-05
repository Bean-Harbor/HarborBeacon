use std::env;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::thread;

use serde::Serialize;
use serde_json::json;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

#[path = "agent_hub_admin_api.rs"]
mod agent_hub_admin_api;
#[path = "assistant_task_api.rs"]
mod assistant_task_api;
#[path = "harbor_model_api_support.rs"]
mod harbor_model_api_support;

use harbor_model_api_support::{BackendKind, ModelApiConfig, ModelApiService};
use harborbeacon_local_agent::control_plane::models::{ModelEndpointStatus, ModelKind};
use harborbeacon_local_agent::runtime::admin_console::AdminConsoleStore;
use harborbeacon_local_agent::runtime::model_center::ADMIN_STATE_PATH_ENV;
use harborbeacon_local_agent::runtime::registry::DeviceRegistryStore;
use harborbeacon_local_agent::runtime::task_api::TaskApiService;
use harborbeacon_local_agent::runtime::task_session::TaskConversationStore;

#[derive(Debug, Clone)]
struct Cli {
    bind: String,
    admin_state: PathBuf,
    device_registry: PathBuf,
    conversations: PathBuf,
    harbor_assistant_dist: PathBuf,
    public_origin: String,
    service_token: Option<String>,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:4174".to_string(),
            admin_state: PathBuf::from(".harborbeacon/admin-console.json"),
            device_registry: PathBuf::from(".harborbeacon/device-registry.json"),
            conversations: PathBuf::from(".harborbeacon/task-api-conversations.json"),
            harbor_assistant_dist: PathBuf::from("frontend/harbor-assistant/dist/harbor-assistant"),
            public_origin: "http://harborbeacon.local:4174".to_string(),
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
                "--harbor-assistant-dist" => {
                    cli.harbor_assistant_dist =
                        PathBuf::from(take_value(&args, &mut index, "--harbor-assistant-dist"))
                }
                value if value.starts_with("--harbor-assistant-dist=") => {
                    cli.harbor_assistant_dist =
                        PathBuf::from(value["--harbor-assistant-dist=".len()..].to_string())
                }
                "--public-origin" => {
                    cli.public_origin = take_value(&args, &mut index, "--public-origin")
                }
                value if value.starts_with("--public-origin=") => {
                    cli.public_origin = value["--public-origin=".len()..].to_string();
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
                value if value.starts_with('-') => fail(&format!("unknown flag: {value}")),
                value => fail(&format!("unexpected positional argument: {value}")),
            }
            index += 1;
        }
        cli
    }
}

#[derive(Clone)]
struct HarborBeaconService {
    admin_api: agent_hub_admin_api::AdminApi,
    task_api: assistant_task_api::TaskApiHttpServer,
    model_api: Arc<RwLock<ModelApiService>>,
}

impl HarborBeaconService {
    fn new(cli: &Cli, service_token: String) -> Self {
        env::set_var(ADMIN_STATE_PATH_ENV, &cli.admin_state);
        let registry_store = DeviceRegistryStore::new(cli.device_registry.clone());
        let admin_store = AdminConsoleStore::new(cli.admin_state.clone(), registry_store);
        let conversation_store = TaskConversationStore::new(cli.conversations.clone());
        let task_service = TaskApiService::new(admin_store.clone(), conversation_store);
        let mut model_config = ModelApiConfig::from_env();
        model_config.bind = cli.bind.clone();
        apply_persisted_model_runtime_selection(&mut model_config, &admin_store);
        let model_api = Arc::new(RwLock::new(ModelApiService::new(model_config)));
        let model_runtime_activation = build_model_runtime_activation_handler(model_api.clone());
        Self {
            admin_api: agent_hub_admin_api::AdminApi::new(
                admin_store,
                task_service.clone(),
                cli.harbor_assistant_dist.clone(),
                cli.public_origin.clone(),
            )
            .with_model_runtime_activation_handler(model_runtime_activation),
            task_api: assistant_task_api::TaskApiHttpServer::new(task_service, service_token),
            model_api,
        }
    }

    fn handle(&self, request: Request) {
        let path = request.url().split('?').next().unwrap_or("/").to_string();
        if path == "/healthz" {
            let _ = request.respond(ok_json(&json!({
                "status": "ok",
                "service": "harborbeacon",
                "topology": "single-port",
                "admin": "/api/admin/*",
                "web": "/api/web/*",
                "inference": "/api/inference/*",
                "harbor_beacon_inference": "/api/harbor-beacon/inference/*"
            })));
            return;
        }
        if assistant_task_api::is_turn_api_path(&path) {
            self.task_api.handle(request);
            return;
        }
        if is_inference_api_path(&path) {
            self.handle_inference(request);
            return;
        }
        self.admin_api.handle(request);
    }

    fn handle_inference(&self, mut request: Request) {
        let method = request.method().clone();
        let path = request.url().split('?').next().unwrap_or("/");
        let model_path = inference_model_path(path);
        let headers = request.headers().to_vec();
        let body = if method == Method::Post {
            match read_request_body(&mut request) {
                Ok(body) => body,
                Err(error) => {
                    let _ = request.respond(error_json(
                        StatusCode(500),
                        "INFRASTRUCTURE_ERROR",
                        &error,
                    ));
                    return;
                }
            }
        } else {
            Vec::new()
        };
        let response = match self.model_api.read() {
            Ok(model_api) => model_api.route(method, &model_path, &headers, &body),
            Err(_) => error_json(
                StatusCode(503),
                "MODEL_RUNTIME_LOCK_ERROR",
                "model runtime lock is poisoned",
            ),
        };
        let _ = request.respond(response);
    }
}

fn build_model_runtime_activation_handler(
    model_api: Arc<RwLock<ModelApiService>>,
) -> agent_hub_admin_api::ModelRuntimeActivationHandler {
    Arc::new(move |request| {
        let current_config = model_api
            .read()
            .map_err(|_| "model runtime lock is poisoned".to_string())?
            .config()
            .clone();
        let mut next_config = current_config;
        apply_activation_request_to_model_config(&mut next_config, &request)?;
        let runtime_model_id = runtime_model_id_for_activation(&next_config, request.model_kind);
        let next_service = ModelApiService::new(next_config);
        let mut guard = model_api
            .write()
            .map_err(|_| "model runtime lock is poisoned".to_string())?;
        *guard = next_service;
        let runtime_model_id = runtime_model_id.or_else(|| Some(request.model_id.clone()));
        Ok(agent_hub_admin_api::ModelRuntimeActivationResult {
            activated: true,
            status: "activated".to_string(),
            message: format!(
                "模型运行时已切换到 {}",
                runtime_model_id
                    .as_deref()
                    .unwrap_or(request.model_id.as_str())
            ),
            runtime_model_id,
        })
    })
}

fn apply_activation_request_to_model_config(
    config: &mut ModelApiConfig,
    request: &agent_hub_admin_api::ModelRuntimeActivationRequest,
) -> Result<(), String> {
    let use_embedded_candle =
        should_auto_switch_to_embedded_candle(request) && !model_backend_env_is_explicit();
    if use_embedded_candle {
        config.backend = BackendKind::Candle;
    }

    match request.model_kind {
        ModelKind::Llm => {
            config.chat_model = request.model_id.clone();
            if matches!(config.backend, BackendKind::Candle) {
                config.candle.chat_model_id = request
                    .local_path
                    .clone()
                    .unwrap_or_else(|| request.model_id.clone());
            }
            Ok(())
        }
        ModelKind::Embedder => {
            config.embedding_model = request.model_id.clone();
            if matches!(config.backend, BackendKind::Candle) {
                config.candle.embedding_model_id = request
                    .local_path
                    .clone()
                    .unwrap_or_else(|| request.model_id.clone());
            }
            Ok(())
        }
        _ => Err(format!(
            "当前运行时暂不支持自动启动 {} 模型",
            request.model_kind.as_str()
        )),
    }
}

fn runtime_model_id_for_activation(config: &ModelApiConfig, kind: ModelKind) -> Option<String> {
    match kind {
        ModelKind::Llm => Some(match config.backend {
            BackendKind::Candle => config.candle.chat_model_id.clone(),
            BackendKind::OpenAIProxy => config.chat_model.clone(),
            BackendKind::SemanticRouter => config.chat_model.clone(),
        }),
        ModelKind::Embedder => match config.backend {
            BackendKind::Candle => Some(config.candle.embedding_model_id.clone()),
            BackendKind::OpenAIProxy => Some(config.embedding_model.clone()),
            BackendKind::SemanticRouter => None,
        },
        _ => None,
    }
}

fn apply_persisted_model_runtime_selection(
    config: &mut ModelApiConfig,
    admin_store: &AdminConsoleStore,
) {
    let Ok(state) = admin_store.load_or_create_state() else {
        return;
    };
    let backend_explicit = model_backend_env_is_explicit();
    for endpoint in state.models.endpoints {
        if endpoint.model_name.trim().is_empty() {
            continue;
        }
        let auto_activation = endpoint
            .metadata
            .get("runtime_auto_activation")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if !auto_activation && endpoint.status != ModelEndpointStatus::Active {
            continue;
        }
        match endpoint.model_endpoint_id.as_str() {
            "llm-local-openai-compatible" => {
                config.chat_model = endpoint.model_name.trim().to_string();
                if !backend_explicit
                    && endpoint_supports_embedded_candle(&endpoint.metadata, &endpoint.model_name)
                {
                    if let Some(local_path) = metadata_string(&endpoint.metadata, "local_path") {
                        config.backend = BackendKind::Candle;
                        config.candle.chat_model_id = local_path;
                    }
                }
            }
            "embed-local-openai-compatible" => {
                config.embedding_model = endpoint.model_name.trim().to_string();
                if !backend_explicit
                    && endpoint_supports_embedded_candle(&endpoint.metadata, &endpoint.model_name)
                {
                    if let Some(local_path) = metadata_string(&endpoint.metadata, "local_path") {
                        config.backend = BackendKind::Candle;
                        config.candle.embedding_model_id = local_path;
                    }
                }
            }
            _ => {}
        }
    }
}

fn should_auto_switch_to_embedded_candle(
    request: &agent_hub_admin_api::ModelRuntimeActivationRequest,
) -> bool {
    request
        .local_path
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && request
            .runtime_profiles
            .iter()
            .any(|profile| profile == "harbor-candle" || profile == "harbor-model-api-candle")
}

fn endpoint_supports_embedded_candle(metadata: &serde_json::Value, model_name: &str) -> bool {
    let profiles_match = metadata
        .get("runtime_profiles")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .any(|profile| profile == "harbor-candle" || profile == "harbor-model-api-candle");
    profiles_match || matches_legacy_candle_model_id(model_name)
}

fn matches_legacy_candle_model_id(model_id: &str) -> bool {
    matches!(
        model_id.trim(),
        "Qwen/Qwen2.5-0.5B-Instruct" | "qwen2.5-1.5b-instruct" | "jina-embeddings-v2-base-zh"
    )
}

fn metadata_string(metadata: &serde_json::Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn model_backend_env_is_explicit() -> bool {
    env::var("HARBOR_MODEL_API_BACKEND")
        .ok()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn main() {
    let cli = Cli::parse();
    let service_token = resolve_service_token(cli.service_token.clone());
    let service = HarborBeaconService::new(&cli, service_token);
    let server = Server::http(&cli.bind).unwrap_or_else(|error| {
        panic!(
            "failed to bind harborbeacon service on {}: {error}",
            cli.bind
        );
    });
    println!(
        "harborbeacon-service listening on http://{} (admin/web/inference single-port)",
        cli.bind
    );

    for request in server.incoming_requests() {
        let service = service.clone();
        thread::spawn(move || service.handle(request));
    }
}

fn inference_model_path(path: &str) -> String {
    for prefix in [
        "/api/beacon/inference",
        "/api/harbor-beacon/inference",
        "/api/inference",
    ] {
        if let Some(tail) = path.strip_prefix(prefix) {
            if tail.is_empty() || tail == "/" {
                return "/healthz".to_string();
            }
            if let Some(tail) = tail.strip_prefix('/') {
                return format!("/{tail}");
            }
        }
    }
    format!("/{}", path.trim_start_matches('/'))
}

fn is_inference_api_path(path: &str) -> bool {
    path == "/api/inference"
        || path.starts_with("/api/inference/")
        || path == "/api/beacon/inference"
        || path.starts_with("/api/beacon/inference/")
        || path == "/api/harbor-beacon/inference"
        || path.starts_with("/api/harbor-beacon/inference/")
}

fn resolve_service_token(cli_token: Option<String>) -> String {
    cli_token
        .or_else(|| env::var("HARBOR_TASK_API_BEARER_TOKEN").ok())
        .or_else(|| env::var("HARBORBEACON_SERVICE_TOKEN").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            eprintln!(
                "harborbeacon-service requires a bearer token via --service-token, HARBOR_TASK_API_BEARER_TOKEN, or HARBORBEACON_SERVICE_TOKEN"
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

fn ok_json(payload: &impl Serialize) -> Response<Cursor<Vec<u8>>> {
    json_response(StatusCode(200), payload)
}

fn error_json(status: StatusCode, code: &'static str, message: &str) -> Response<Cursor<Vec<u8>>> {
    json_response(
        status,
        &json!({
            "ok": false,
            "error": {
                "code": code,
                "message": message
            }
        }),
    )
}

fn json_response(status: StatusCode, payload: &impl Serialize) -> Response<Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(payload)
        .unwrap_or_else(|_| b"{\"ok\":false,\"error\":{\"code\":\"SERIALIZE_FAILED\"}}".to_vec());
    let mut response = Response::from_data(body).with_status_code(status);
    response.add_header(
        Header::from_bytes(
            b"Content-Type".as_slice(),
            b"application/json; charset=utf-8".as_slice(),
        )
        .expect("header"),
    );
    response.add_header(
        Header::from_bytes(b"Cache-Control".as_slice(), b"no-store".as_slice()).expect("header"),
    );
    response
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
        "Usage: harborbeacon-service [--bind ADDR] [--admin-state PATH] [--device-registry PATH] [--conversations PATH] [--harbor-assistant-dist PATH] [--public-origin URL] [--service-token TOKEN]"
    );
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use harborbeacon_local_agent::control_plane::models::{ModelEndpoint, ModelEndpointKind};
    use harborbeacon_local_agent::runtime::registry::DeviceRegistryStore;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn temp_path(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("harborbeacon-service-{label}-{suffix}.json"))
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var(key).ok();
            env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = env::var(key).ok();
            env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = self.previous.as_ref() {
                env::set_var(self.key, value);
            } else {
                env::remove_var(self.key);
            }
        }
    }

    fn candle_llm_activation_request() -> agent_hub_admin_api::ModelRuntimeActivationRequest {
        agent_hub_admin_api::ModelRuntimeActivationRequest {
            capability_id: "semantic_router".to_string(),
            model_id: "qwen2.5-1.5b-instruct".to_string(),
            model_kind: ModelKind::Llm,
            local_path: Some(
                "/mnt/software/harborbeacon-agent-ci/model-store/qwen2.5-1.5b-instruct".to_string(),
            ),
            runtime_profiles: vec!["harbor-model-api-candle".to_string()],
        }
    }

    #[test]
    fn harbor_beacon_inference_prefix_maps_to_embedded_model_api() {
        assert!(is_inference_api_path("/api/inference/healthz"));
        assert!(is_inference_api_path(
            "/api/harbor-beacon/inference/healthz"
        ));
        assert!(is_inference_api_path("/api/beacon/inference/healthz"));
        assert!(!is_inference_api_path("/api/harbor-beacon/models/runtimes"));
        assert!(!is_inference_api_path("/api/beacon/models/runtimes"));
        assert_eq!(inference_model_path("/api/inference"), "/healthz");
        assert_eq!(inference_model_path("/api/inference/"), "/healthz");
        assert_eq!(
            inference_model_path("/api/inference/v1/embeddings"),
            "/v1/embeddings"
        );
        assert_eq!(
            inference_model_path("/api/harbor-beacon/inference"),
            "/healthz"
        );
        assert_eq!(
            inference_model_path("/api/harbor-beacon/inference/healthz"),
            "/healthz"
        );
        assert_eq!(
            inference_model_path("/api/harbor-beacon/inference/v1/chat/completions"),
            "/v1/chat/completions"
        );
        assert_eq!(inference_model_path("/api/beacon/inference"), "/healthz");
        assert_eq!(
            inference_model_path("/api/beacon/inference/healthz"),
            "/healthz"
        );
        assert_eq!(
            inference_model_path("/api/beacon/inference/v1/chat/completions"),
            "/v1/chat/completions"
        );
    }

    #[test]
    fn single_port_turn_ingress_accepts_harboros_stripped_aliases() {
        assert!(assistant_task_api::is_turn_api_path("/api/web/turns"));
        assert!(assistant_task_api::is_turn_api_path("/api/turns"));
        assert!(assistant_task_api::is_turn_api_path("/web/turns"));
        assert!(assistant_task_api::is_turn_api_path("/turns"));
        assert!(!assistant_task_api::is_turn_api_path(
            "/api/harbor-beacon/web/turns"
        ));
    }

    #[test]
    fn local_candle_profile_auto_switches_when_backend_is_implicit() {
        let _lock = env_lock().lock().expect("env lock");
        let _backend = EnvGuard::remove("HARBOR_MODEL_API_BACKEND");
        let mut config = ModelApiConfig::default();

        apply_activation_request_to_model_config(&mut config, &candle_llm_activation_request())
            .expect("apply activation");

        assert_eq!(config.backend, BackendKind::Candle);
        assert_eq!(config.chat_model, "qwen2.5-1.5b-instruct");
        assert_eq!(
            config.candle.chat_model_id,
            "/mnt/software/harborbeacon-agent-ci/model-store/qwen2.5-1.5b-instruct"
        );
    }

    #[test]
    fn explicit_backend_env_prevents_auto_candle_switch() {
        let _lock = env_lock().lock().expect("env lock");
        let _backend = EnvGuard::set("HARBOR_MODEL_API_BACKEND", "openai_proxy");
        let mut config = ModelApiConfig {
            backend: BackendKind::OpenAIProxy,
            ..ModelApiConfig::default()
        };
        let original_candle_chat_model_id = config.candle.chat_model_id.clone();

        apply_activation_request_to_model_config(&mut config, &candle_llm_activation_request())
            .expect("apply activation");

        assert_eq!(config.backend, BackendKind::OpenAIProxy);
        assert_eq!(config.chat_model, "qwen2.5-1.5b-instruct");
        assert_eq!(config.candle.chat_model_id, original_candle_chat_model_id);
    }

    #[test]
    fn persisted_candle_endpoint_restores_embedded_backend_when_implicit() {
        let _lock = env_lock().lock().expect("env lock");
        let _backend = EnvGuard::remove("HARBOR_MODEL_API_BACKEND");
        let registry_path = temp_path("registry-persisted-candle");
        let admin_path = temp_path("admin-persisted-candle");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let local_path = "/mnt/software/harborbeacon-agent-ci/model-store/qwen2.5-1.5b-instruct";

        admin_store
            .save_model_endpoint(ModelEndpoint {
                model_endpoint_id: "llm-local-openai-compatible".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Llm,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "qwen".to_string(),
                model_name: "qwen2.5-1.5b-instruct".to_string(),
                capability_tags: Vec::new(),
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({
                    "local_path": local_path,
                    "runtime_auto_activation": true,
                    "runtime_profiles": ["harbor-model-api-candle"],
                }),
            })
            .expect("save endpoint");

        let mut config = ModelApiConfig::default();
        apply_persisted_model_runtime_selection(&mut config, &admin_store);

        assert_eq!(config.backend, BackendKind::Candle);
        assert_eq!(config.chat_model, "qwen2.5-1.5b-instruct");
        assert_eq!(config.candle.chat_model_id, local_path);

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }
}
