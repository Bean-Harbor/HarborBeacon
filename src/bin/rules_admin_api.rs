//! Structured Harbor Rules admin surface. Guardian reviews remain independent.

use super::{
    error_json, filter_home_assistant_entities, home_assistant_client_from_state, ok_json,
    read_json_body, AccessAction, AccessIdentityHints, AdminApi, AdminConsoleStore,
    HomeAssistantServiceActionRequest,
};
use harborbeacon_local_agent::connectors::home_assistant::{
    validate_home_assistant_service_action_request, HomeAssistantEntity,
};
use harborbeacon_local_agent::runtime::automation::{
    RuleAction, RuleDefinition, RuleRecord, RuleRun, RuleTrigger, RulesStore,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tiny_http::{Method, Request, Response, StatusCode};

const RULES_PREFIX: &str = "/api/automation/rules";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionRequest {
    revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunRequest {
    revision: u64,
    trigger_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventRequest {
    event_id: String,
    event_type: String,
}

pub(super) fn is_rules_path(path: &str) -> bool {
    path == RULES_PREFIX || path.starts_with(&format!("{RULES_PREFIX}/"))
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn rule_error(error: &str) -> Response<Cursor<Vec<u8>>> {
    let status = if error.starts_with("NOT_FOUND:") {
        404
    } else if error.starts_with("CONFLICT:") {
        409
    } else if error.starts_with("STORAGE:") {
        return error_json(StatusCode(500), "Rules storage is unavailable");
    } else {
        422
    };
    error_json(StatusCode(status), error)
}

fn parse_rule_path(path: &str) -> Option<(&str, Option<&str>)> {
    let suffix = path.strip_prefix("/api/automation/rules/")?;
    let mut parts = suffix.split('/');
    let id = parts.next()?;
    if id.is_empty()
        || id.len() > 100
        || !id
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_'))
    {
        return None;
    }
    let action = parts.next();
    if parts.next().is_some() || action == Some("") {
        return None;
    }
    Some((id, action))
}

impl AdminApi {
    pub(super) fn handle_rules_request(
        &self,
        request: &mut Request,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<Cursor<Vec<u8>>> {
        let action = if request.method() == &Method::Get {
            AccessAction::AdminReadState
        } else {
            AccessAction::AdminManage
        };
        let principal = match self.authorize_admin_action(hints, action) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let now = now_seconds();
        if path == RULES_PREFIX {
            return match request.method() {
                Method::Get => match self.rules_store.list(now) {
                    Ok(rules) => ok_json(&json!({"rules": rules})),
                    Err(error) => rule_error(&error),
                },
                Method::Post => {
                    let definition: RuleDefinition = match read_json_body(request) {
                        Ok(body) => body,
                        Err(error) => return error_json(StatusCode(400), &error),
                    };
                    match self.rules_store.create(definition, now) {
                        Ok(rule) => {
                            self.record_admin_audit(
                                &principal,
                                "rule",
                                &rule.rule_id,
                                "rules.create",
                                json!({}),
                                json!({"revision": rule.revision}),
                            );
                            ok_json(&json!({"rule": rule}))
                        }
                        Err(error) => rule_error(&error),
                    }
                }
                _ => error_json(StatusCode(405), "Method not allowed"),
            };
        }
        if path == "/api/automation/rules/events" && request.method() == &Method::Post {
            let event: EventRequest = match read_json_body(request) {
                Ok(body) => body,
                Err(error) => return error_json(StatusCode(400), &error),
            };
            if !valid_event_value(&event.event_id) || !valid_event_value(&event.event_type) {
                return error_json(
                    StatusCode(422),
                    "Event ID and type must be 1-128 ASCII identifier characters",
                );
            }
            return match dispatch_event(&self.rules_store, &self.admin_store, &event, now) {
                Ok(runs) => ok_json(&json!({"runs": runs})),
                Err(error) => rule_error(&error),
            };
        }
        let Some((id, suffix)) = parse_rule_path(path) else {
            return error_json(StatusCode(404), "Rule route not found");
        };
        if request.method() == &Method::Get && suffix == Some("runs") {
            return match self.rules_store.history(id) {
                Ok(runs) => ok_json(&json!({"runs": runs})),
                Err(error) => rule_error(&error),
            };
        }
        if request.method() == &Method::Put && suffix.is_none() {
            let mut body: serde_json::Value = match read_json_body(request) {
                Ok(body) => body,
                Err(error) => return error_json(StatusCode(400), &error),
            };
            let Some(revision) = body
                .as_object_mut()
                .and_then(|body| body.remove("revision"))
                .and_then(|value| value.as_u64())
            else {
                return error_json(StatusCode(400), "A rule revision is required");
            };
            let definition: RuleDefinition = match serde_json::from_value(body) {
                Ok(definition) => definition,
                Err(error) => return error_json(StatusCode(400), &error.to_string()),
            };
            return match self.rules_store.update(id, revision, definition, now) {
                Ok(rule) => {
                    self.record_admin_audit(
                        &principal,
                        "rule",
                        id,
                        "rules.update",
                        json!({"revision": revision}),
                        json!({"revision": rule.revision}),
                    );
                    ok_json(&json!({"rule": rule}))
                }
                Err(error) => rule_error(&error),
            };
        }
        if request.method() != &Method::Post {
            return error_json(StatusCode(405), "Method not allowed");
        }
        if suffix == Some("run") {
            let body: RunRequest = match read_json_body(request) {
                Ok(body) => body,
                Err(error) => return error_json(StatusCode(400), &error),
            };
            if !valid_event_value(&body.trigger_id) {
                return error_json(
                    StatusCode(422),
                    "Trigger ID must be 1-128 ASCII identifier characters",
                );
            }
            let rule = match find_rule(&self.rules_store, id, now) {
                Ok(rule) => rule,
                Err(error) => return rule_error(&error),
            };
            if !matches!(rule.definition.trigger, RuleTrigger::Manual) {
                return error_json(
                    StatusCode(422),
                    "Only manual rules can be run from this command",
                );
            }
            let context = condition_context(&self.admin_store, std::slice::from_ref(&rule));
            return match self.rules_store.run(
                id,
                body.revision,
                &format!("manual:{}", body.trigger_id),
                "manual",
                &context,
                now,
                |action| execute_action(&self.admin_store, action),
            ) {
                Ok(run) => ok_json(&json!({"run": run})),
                Err(error) => rule_error(&error),
            };
        }
        if !matches!(suffix, Some("preview" | "enable" | "pause" | "delete")) {
            return error_json(StatusCode(404), "Rule route not found");
        }
        let body: RevisionRequest = match read_json_body(request) {
            Ok(body) => body,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        if suffix == Some("preview") {
            let rule = match find_rule(&self.rules_store, id, now) {
                Ok(rule) => rule,
                Err(error) => return rule_error(&error),
            };
            let context = condition_context(&self.admin_store, std::slice::from_ref(&rule));
            return match self.rules_store.preview(id, body.revision, &context, now) {
                Ok(mut preview) => {
                    if rule
                        .definition
                        .actions
                        .iter()
                        .any(|action| matches!(action, RuleAction::HomeAssistant { .. }))
                    {
                        if let Err(error) = load_entities(&self.admin_store) {
                            preview.warnings.push(error);
                        }
                    }
                    ok_json(&preview)
                }
                Err(error) => rule_error(&error),
            };
        }
        let status = match suffix {
            Some("enable") => "enabled",
            Some("pause") => "paused",
            _ => "deleted",
        };
        match self.rules_store.set_status(id, body.revision, status, now) {
            Ok(rule) => {
                self.record_admin_audit(
                    &principal,
                    "rule",
                    id,
                    "rules.status.update",
                    json!({"revision": body.revision}),
                    json!({"status": status}),
                );
                ok_json(&json!({"rule": rule}))
            }
            Err(error) => rule_error(&error),
        }
    }

    pub(super) fn start_rules_worker(&self) -> Result<(), String> {
        let lifetime = Arc::downgrade(&self.rules_worker_lifetime);
        let rules = self.rules_store.clone();
        let admin = self.admin_store.clone();
        thread::Builder::new()
            .name("harbor-rules".into())
            .spawn(move || {
                let mut previous_states = BTreeMap::new();
                while lifetime.strong_count() > 0 {
                    if let Err(error) =
                        tick_rules(&rules, &admin, &mut previous_states, now_seconds())
                    {
                        eprintln!(
                            "Harbor Rules worker: {}",
                            if error.starts_with("STORAGE:") {
                                "rule storage unavailable"
                            } else {
                                "evaluation failed; no success assumed"
                            }
                        );
                    }
                    thread::sleep(Duration::from_secs(2));
                }
            })
            .map(|_| ())
            .map_err(|_| "RULES_WORKER_START_FAILED".to_string())
    }
}

fn valid_event_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.' | b':'))
}

fn find_rule(store: &RulesStore, id: &str, now: u64) -> Result<RuleRecord, String> {
    store
        .list(now)?
        .into_iter()
        .find(|rule| rule.rule_id == id)
        .ok_or_else(|| "NOT_FOUND: Rule does not exist".into())
}

fn load_entities(store: &AdminConsoleStore) -> Result<Vec<HomeAssistantEntity>, String> {
    let state = store
        .home_assistant_state()
        .map_err(|_| "Home Assistant configuration is unavailable".to_string())?;
    let client = home_assistant_client_from_state(&state)
        .map_err(|_| "Home Assistant is not configured or enabled".to_string())?;
    let entities = client
        .fetch_entities()
        .map_err(|_| "Home Assistant entities are unavailable".to_string())?;
    Ok(filter_home_assistant_entities(
        entities,
        &state.exposed_domains,
    ))
}

fn condition_context(store: &AdminConsoleStore, rules: &[RuleRecord]) -> BTreeMap<String, String> {
    if !rules
        .iter()
        .any(|rule| !rule.definition.conditions.items.is_empty())
    {
        return BTreeMap::new();
    }
    load_entities(store)
        .unwrap_or_default()
        .into_iter()
        .map(|entity| (entity.entity_id, entity.state))
        .collect()
}

fn execute_action(store: &AdminConsoleStore, action: &RuleAction) -> Result<String, String> {
    let RuleAction::HomeAssistant {
        entity_id,
        domain,
        service,
        fields,
    } = action
    else {
        return Err("Unsupported external rule action".into());
    };
    let state = store
        .home_assistant_state()
        .map_err(|_| "Home Assistant configuration is unavailable".to_string())?;
    let request = HomeAssistantServiceActionRequest {
        entity_id: entity_id.clone(),
        domain: domain.clone(),
        service: service.clone(),
        fields: fields.clone(),
    };
    validate_home_assistant_service_action_request(
        &request,
        state.enabled,
        &state.exposed_domains,
    )?;
    let client = home_assistant_client_from_state(&state)
        .map_err(|_| "Home Assistant is not configured or enabled".to_string())?;
    let entities = client
        .fetch_entities()
        .map_err(|_| "Home Assistant entities are unavailable".to_string())?;
    let entity = entities
        .iter()
        .find(|entity| &entity.entity_id == entity_id)
        .ok_or_else(|| "Home Assistant entity does not exist".to_string())?;
    if matches!(entity.state.as_str(), "unavailable" | "unknown") {
        return Err("Home Assistant entity is unavailable".into());
    }
    let result = client
        .call_service(domain, service, entity_id, Some(fields))
        .map_err(|_| {
            "UNKNOWN: Home Assistant action outcome is unknown; automatic retry is disabled"
                .to_string()
        })?;
    if !result.ok {
        return Err("Home Assistant did not confirm the service action".into());
    }
    Ok("Home Assistant accepted the service action".into())
}

fn dispatch_event(
    rules: &RulesStore,
    admin: &AdminConsoleStore,
    event: &EventRequest,
    now: u64,
) -> Result<Vec<RuleRun>, String> {
    let matching = rules
        .list(now)?
        .into_iter()
        .filter(|rule| {
            rule.status == "enabled"
                && matches!(&rule.definition.trigger,
            RuleTrigger::Event { event_type } if event_type == &event.event_type)
        })
        .collect::<Vec<_>>();
    let context = condition_context(admin, &matching);
    let key = event_trigger_id(event);
    matching
        .iter()
        .map(|rule| {
            rules.run_for_activation(
                &rule.rule_id,
                rule.revision,
                &key,
                "event",
                &context,
                now,
                rule.activation_id.as_deref(),
                |action| execute_action(admin, action),
            )
        })
        .collect()
}

fn event_trigger_id(event: &EventRequest) -> String {
    let payload = serde_json::to_vec(&(&event.event_type, &event.event_id)).expect("string tuple");
    format!("event:{:x}", Sha256::digest(payload))
}

fn tick_rules(
    rules: &RulesStore,
    admin: &AdminConsoleStore,
    previous_states: &mut BTreeMap<String, String>,
    now: u64,
) -> Result<(), String> {
    let active = rules
        .list(now)?
        .into_iter()
        .filter(|rule| rule.status == "enabled")
        .collect::<Vec<_>>();
    if active.is_empty() {
        previous_states.clear();
        return Ok(());
    }
    let state_rules = active
        .iter()
        .filter(|rule| matches!(rule.definition.trigger, RuleTrigger::State { .. }))
        .collect::<Vec<_>>();
    let due = rules.due(now)?;
    if state_rules.is_empty() && due.is_empty() {
        previous_states.clear();
        return Ok(());
    }
    let needs_entities = !state_rules.is_empty()
        || due
            .iter()
            .any(|rule| !rule.definition.conditions.items.is_empty());
    let entities = if needs_entities {
        match load_entities(admin) {
            Ok(entities) => entities,
            Err(_) => {
                previous_states.clear();
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let context = entities
        .iter()
        .map(|entity| (entity.entity_id.clone(), entity.state.clone()))
        .collect::<BTreeMap<_, _>>();
    for rule in due {
        let trigger_id = format!("schedule:{}", rule.next_run_at.unwrap_or(now));
        rules.run_for_activation(
            &rule.rule_id,
            rule.revision,
            &trigger_id,
            "schedule",
            &context,
            now,
            rule.activation_id.as_deref(),
            |action| execute_action(admin, action),
        )?;
    }
    let mut observed = BTreeMap::new();
    for rule in state_rules {
        if let RuleTrigger::State { entity_id, to } = &rule.definition.trigger {
            if let Some(entity) = entities
                .iter()
                .find(|entity| &entity.entity_id == entity_id)
            {
                let observation_key = format!(
                    "{}:{}:{}",
                    rule.rule_id,
                    rule.revision,
                    rule.activation_id.as_deref().unwrap_or("legacy")
                );
                if matches!(entity.state.as_str(), "unknown" | "unavailable") {
                    continue;
                }
                observed.insert(observation_key.clone(), entity.state.clone());
                if state_edge_matches(previous_states.get(&observation_key), &entity.state, to) {
                    let Some(changed) = entity.last_changed.as_ref() else {
                        continue;
                    };
                    let trigger_id = format!("state:{entity_id}:{changed}");
                    rules.run_for_activation(
                        &rule.rule_id,
                        rule.revision,
                        &trigger_id,
                        "state",
                        &context,
                        now,
                        rule.activation_id.as_deref(),
                        |action| execute_action(admin, action),
                    )?;
                }
            }
        }
    }
    // Missing/unavailable entities lose their baseline; reconnect only primes the next edge.
    *previous_states = observed;
    Ok(())
}

fn state_edge_matches(previous: Option<&String>, current: &str, target: &str) -> bool {
    !matches!(current, "unknown" | "unavailable")
        && previous.is_some_and(|state| state != current)
        && current == target
}

#[cfg(test)]
mod tests {
    use super::*;
    use harborbeacon_local_agent::connectors::home_assistant::HomeAssistantServiceCallResponse;
    use harborbeacon_local_agent::runtime::registry::DeviceRegistryStore;
    use harborbeacon_local_agent::runtime::task_api::TaskApiService;
    use harborbeacon_local_agent::runtime::task_session::TaskConversationStore;
    use serde_json::Value;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard};
    use tiny_http::Server;
    use uuid::Uuid;

    static HARBORLINK_FIXTURE_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct HarborLinkFixtureEnv {
        saved: Vec<(&'static str, Option<OsString>)>,
        _lock: MutexGuard<'static, ()>,
    }

    impl HarborLinkFixtureEnv {
        fn new(base_url: &str) -> Self {
            let lock = HARBORLINK_FIXTURE_ENV_LOCK.lock().unwrap();
            let mut saved = Vec::new();
            for (key, value) in [
                ("HARBORBEACON_SOUTHBOUND_MODE", "harborlink"),
                ("HARBORLINK_MEDIA_API_URL", base_url),
                ("HARBORLINK_LOCAL_API_TOKEN", "rules-fixture-local-token"),
            ] {
                saved.push((key, std::env::var_os(key)));
                unsafe { std::env::set_var(key, value) };
            }
            Self { saved, _lock: lock }
        }
    }

    impl Drop for HarborLinkFixtureEnv {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                if let Some(value) = value {
                    unsafe { std::env::set_var(key, value) };
                } else {
                    unsafe { std::env::remove_var(key) };
                }
            }
        }
    }

    fn harborlink_entities(state: &str, last_changed: Option<String>) -> String {
        serde_json::to_string(&vec![HomeAssistantEntity {
            entity_id: "light.test".into(),
            domain: "light".into(),
            state: state.into(),
            display_name: "Test light".into(),
            area_id: None,
            device_class: None,
            last_changed,
            last_updated: None,
            attributes: json!({}),
        }])
        .unwrap()
    }

    fn assert_harborlink_request(request: &Request, method: Method, path: &str) {
        assert_eq!(request.method(), &method);
        assert_eq!(request.url(), path);
        for (name, expected) in [
            ("X-HarborLink-Contract-Version", "1.0"),
            ("Authorization", "Bearer rules-fixture-local-token"),
        ] {
            let header = request
                .headers()
                .iter()
                .find(|header| header.field.equiv(name))
                .unwrap();
            assert_eq!(header.value.as_str(), expected);
        }
    }

    struct TestApi {
        base: String,
        root: PathBuf,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl TestApi {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("harbor-rules-http-{}", Uuid::new_v4()));
            let registry = DeviceRegistryStore::new(root.join("devices.json"));
            let admin = AdminConsoleStore::new(root.join("admin.json"), registry);
            let tasks = TaskApiService::new(
                admin.clone(),
                TaskConversationStore::new(root.join("tasks.json")),
            );
            let server = Server::http("127.0.0.1:0").unwrap();
            let base = format!("http://{}", server.server_addr());
            let api = AdminApi::new_for_test(admin, tasks, root.join("webui"), base.clone());
            let worker = thread::spawn(move || {
                for request in server.incoming_requests() {
                    if request.url() == "/__test_stop" {
                        request.respond(Response::from_string("stopped")).unwrap();
                        break;
                    }
                    api.handle(request);
                }
            });
            Self {
                base,
                root,
                worker: Some(worker),
            }
        }

        fn request(&self, method: reqwest::Method, path: &str, body: Value) -> (u16, Value) {
            let response = reqwest::blocking::Client::new()
                .request(method, format!("{}{path}", self.base))
                .timeout(Duration::from_secs(10))
                .json(&body)
                .send()
                .unwrap();
            (response.status().as_u16(), response.json().unwrap())
        }
    }

    impl Drop for TestApi {
        fn drop(&mut self) {
            let _ = reqwest::blocking::get(format!("{}/__test_stop", self.base));
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn record_definition() -> Value {
        json!({"name":"Local rule", "trigger":{"kind":"manual"},
            "conditions":{"match_mode":"all","items":[]},
            "actions":[{"kind":"record","message":"Local execution recorded"}], "expires_at":null})
    }

    #[test]
    fn rules_paths_are_exact_and_preserve_review_routes() {
        assert!(is_rules_path("/api/automation/rules"));
        assert!(is_rules_path("/api/automation/rules/rule-1/preview"));
        assert!(!is_rules_path("/api/automation/rules-extra"));
        assert!(!is_rules_path("/api/automation/reviews"));
        assert_eq!(
            parse_rule_path("/api/automation/rules/rule-1/runs"),
            Some(("rule-1", Some("runs")))
        );
        assert!(parse_rule_path("/api/automation/rules/../runs").is_none());
        assert!(parse_rule_path("/api/automation/rules/rule-1/runs/extra").is_none());
    }

    #[test]
    fn failure_codes_do_not_leak_storage_paths() {
        assert_eq!(rule_error("NOT_FOUND: Rule").status_code(), StatusCode(404));
        assert_eq!(
            rule_error("CONFLICT: Revision").status_code(),
            StatusCode(409)
        );
        assert_eq!(
            rule_error("VALIDATION: Definition").status_code(),
            StatusCode(422)
        );
        let mut body = String::new();
        std::io::Read::read_to_string(
            &mut rule_error("STORAGE: private/path").into_reader(),
            &mut body,
        )
        .unwrap();
        assert!(!body.contains("private/path"));
    }

    #[test]
    fn state_edges_require_a_live_previous_observation() {
        assert!(!state_edge_matches(None, "on", "on"));
        assert!(!state_edge_matches(Some(&"on".into()), "on", "on"));
        assert!(state_edge_matches(Some(&"off".into()), "on", "on"));
        assert!(!state_edge_matches(
            Some(&"on".into()),
            "unavailable",
            "unavailable"
        ));
    }

    #[test]
    fn event_keys_are_bounded_and_do_not_alias_delimiters() {
        let first = EventRequest {
            event_type: "a:b".into(),
            event_id: "c".into(),
        };
        let second = EventRequest {
            event_type: "a".into(),
            event_id: "b:c".into(),
        };
        assert_ne!(event_trigger_id(&first), event_trigger_id(&second));
        let maximum = EventRequest {
            event_type: "a".repeat(128),
            event_id: "b".repeat(128),
        };
        assert!(event_trigger_id(&maximum).len() < 128);
        assert_eq!(event_trigger_id(&maximum), event_trigger_id(&maximum));
    }

    #[test]
    fn rules_http_lifecycle_uses_product_prefix_and_real_store() {
        let api = TestApi::new();
        let prefix = "/api/harbor-beacon/automation/rules";
        let (status, created) = api.request(reqwest::Method::POST, prefix, record_definition());
        assert_eq!(status, 200, "{created}");
        let id = created["rule"]["rule_id"].as_str().unwrap();
        let path = format!("{prefix}/{id}");
        let revision = json!({"revision":1});
        assert_eq!(
            api.request(
                reqwest::Method::POST,
                &format!("{path}/enable"),
                revision.clone()
            )
            .0,
            409
        );
        assert_eq!(
            api.request(
                reqwest::Method::POST,
                &format!("{path}/preview"),
                revision.clone()
            )
            .0,
            200
        );
        assert_eq!(
            api.request(
                reqwest::Method::POST,
                &format!("{path}/enable"),
                revision.clone()
            )
            .0,
            200
        );
        let run_input = json!({"revision":1,"trigger_id":"http-run-1"});
        let (status, first) = api.request(
            reqwest::Method::POST,
            &format!("{path}/run"),
            run_input.clone(),
        );
        assert_eq!(status, 200, "{first}");
        assert_eq!(first["run"]["status"], "completed");
        let (_, replay) = api.request(reqwest::Method::POST, &format!("{path}/run"), run_input);
        assert_eq!(first["run"]["run_id"], replay["run"]["run_id"]);
        let mut update = record_definition();
        update["revision"] = json!(1);
        update["name"] = json!("Edited local rule");
        let mut invalid_update = update.clone();
        invalid_update["run_without_confirmation"] = json!(true);
        assert_eq!(
            api.request(reqwest::Method::PUT, &path, invalid_update).0,
            400
        );
        let (status, edited) = api.request(reqwest::Method::PUT, &path, update);
        assert_eq!(status, 200, "{edited}");
        assert_eq!(edited["rule"]["status"], "draft");
        assert_eq!(edited["rule"]["revision"], 2);
        assert_eq!(
            api.request(reqwest::Method::POST, &format!("{path}/enable"), revision)
                .0,
            409
        );
        assert_eq!(
            api.request(
                reqwest::Method::POST,
                &format!("{path}/delete"),
                json!({"revision":2})
            )
            .0,
            200
        );
        let (_, history) = api.request(reqwest::Method::GET, &format!("{path}/runs"), Value::Null);
        assert_eq!(history["runs"].as_array().unwrap().len(), 1);
        let response = reqwest::blocking::Client::new()
            .get(format!("{}{prefix}", api.base))
            .header("X-Harbor-User-Id", "not-a-member")
            .send()
            .unwrap();
        assert_eq!(response.status().as_u16(), 403);
    }

    #[test]
    fn events_are_explicit_idempotent_inputs_not_manual_run_shortcuts() {
        let api = TestApi::new();
        let prefix = "/api/automation/rules";
        let mut definition = record_definition();
        definition["trigger"] = json!({"kind":"event","event_type":"local.test"});
        let (_, created) = api.request(reqwest::Method::POST, prefix, definition);
        let path = format!("{prefix}/{}", created["rule"]["rule_id"].as_str().unwrap());
        for action in ["preview", "enable"] {
            assert_eq!(
                api.request(
                    reqwest::Method::POST,
                    &format!("{path}/{action}"),
                    json!({"revision":1})
                )
                .0,
                200
            );
        }
        assert_eq!(
            api.request(
                reqwest::Method::POST,
                &format!("{path}/run"),
                json!({"revision":1,"trigger_id":"manual"})
            )
            .0,
            422
        );
        let event = json!({"event_type":"local.test","event_id":"one"});
        let (_, first) = api.request(
            reqwest::Method::POST,
            &format!("{prefix}/events"),
            event.clone(),
        );
        let (_, replay) = api.request(reqwest::Method::POST, &format!("{prefix}/events"), event);
        assert_eq!(first["runs"][0]["status"], "completed");
        assert_eq!(first["runs"][0]["run_id"], replay["runs"][0]["run_id"]);
        let (_, history) = api.request(reqwest::Method::GET, &format!("{path}/runs"), Value::Null);
        assert_eq!(history["runs"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn accepted_ha_action_completes_through_harborlink_and_is_not_retried() {
        let response = HomeAssistantServiceCallResponse {
            domain: "light".into(),
            service: "turn_on".into(),
            entity_id: "light.test".into(),
            ok: true,
            changed_entity_count: 1,
        };
        assert_ha_action_outcome(
            serde_json::to_string(&response).unwrap(),
            "completed",
            "succeeded",
        );
    }

    #[test]
    fn accepted_ha_action_with_unreadable_response_is_unknown_and_not_retried() {
        assert_ha_action_outcome("invalid-json".into(), "unknown", "unknown");
    }

    fn assert_ha_action_outcome(
        response_body: String,
        expected_status: &str,
        expected_action_status: &str,
    ) {
        let root = std::env::temp_dir().join(format!("harbor-rules-ha-{}", Uuid::new_v4()));
        let admin = AdminConsoleStore::new(
            root.join("admin.json"),
            DeviceRegistryStore::new(root.join("devices.json")),
        );
        let server = Server::http("127.0.0.1:0").unwrap();
        let _env = HarborLinkFixtureEnv::new(&format!("http://{}", server.server_addr()));
        admin
            .save_home_assistant_orchestration_state(true, vec!["light".into()])
            .unwrap();
        let state = admin.home_assistant_state().unwrap();
        assert!(state.base_url.is_empty());
        assert!(state.access_token.is_empty());
        let worker = thread::spawn(move || {
            let states = server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap();
            assert_harborlink_request(&states, Method::Get, "/v1/home-assistant/entities");
            states
                .respond(Response::from_string(harborlink_entities("off", None)))
                .unwrap();
            let mut action = server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap();
            assert_harborlink_request(
                &action,
                Method::Post,
                "/v1/home-assistant/services/light/turn_on",
            );
            assert!(action.headers().iter().any(|header| {
                header.field.equiv("X-Request-Id") && header.value.as_str().starts_with("beacon-")
            }));
            let mut body = String::new();
            std::io::Read::read_to_string(action.as_reader(), &mut body).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&body).unwrap(),
                json!({"entity_id":"light.test","fields":{}})
            );
            action
                .respond(Response::from_string(response_body))
                .unwrap();
            assert!(server
                .recv_timeout(Duration::from_millis(150))
                .unwrap()
                .is_none());
        });
        let rules = RulesStore::new(root.join("rules.json"));
        let mut definition = record_definition();
        definition["actions"] = json!([{"kind":"home_assistant","entity_id":"light.test","domain":"light","service":"turn_on","fields":{}}]);
        let rule = rules
            .create(serde_json::from_value(definition).unwrap(), 100)
            .unwrap();
        rules
            .preview(&rule.rule_id, 1, &BTreeMap::new(), 100)
            .unwrap();
        rules.set_status(&rule.rule_id, 1, "enabled", 100).unwrap();
        let first = rules
            .run(
                &rule.rule_id,
                1,
                "manual:one",
                "manual",
                &BTreeMap::new(),
                101,
                |action| execute_action(&admin, action),
            )
            .unwrap();
        assert_eq!(first.status, expected_status);
        assert_eq!(first.actions[0].status, expected_action_status);
        let replay = rules
            .run(
                &rule.rule_id,
                1,
                "manual:one",
                "manual",
                &BTreeMap::new(),
                102,
                |_| panic!("must not retry"),
            )
            .unwrap();
        assert_eq!(first.run_id, replay.run_id);
        assert_eq!(rules.history(&rule.rule_id).unwrap().len(), 1);
        worker.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduled_record_completes_without_ha_or_model_configuration() {
        let root = std::env::temp_dir().join(format!("harbor-rules-local-{}", Uuid::new_v4()));
        let admin = AdminConsoleStore::new(
            root.join("admin.json"),
            DeviceRegistryStore::new(root.join("devices.json")),
        );
        assert!(!admin.home_assistant_state().unwrap().enabled);
        let rules = RulesStore::new(root.join("rules.json"));
        let mut definition = record_definition();
        definition["trigger"] = json!({"kind":"schedule","interval_seconds":10});
        let rule = rules
            .create(serde_json::from_value(definition).unwrap(), 100)
            .unwrap();
        rules
            .preview(&rule.rule_id, 1, &BTreeMap::new(), 100)
            .unwrap();
        rules.set_status(&rule.rule_id, 1, "enabled", 100).unwrap();
        let mut previous = BTreeMap::new();
        tick_rules(&rules, &admin, &mut previous, 109).unwrap();
        assert!(rules.history(&rule.rule_id).unwrap().is_empty());
        tick_rules(&rules, &admin, &mut previous, 135).unwrap();
        tick_rules(&rules, &admin, &mut previous, 135).unwrap();
        let history = RulesStore::new(root.join("rules.json"))
            .history(&rule.rule_id)
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].trigger_kind, "schedule");
        assert_eq!(history[0].status, "completed");
        assert_eq!(history[0].actions[0].status, "succeeded");
        assert_eq!(history[0].actions[0].message, "Local execution recorded");
        assert_eq!(
            find_rule(&rules, &rule.rule_id, 135).unwrap().next_run_at,
            Some(145)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_worker_reprimes_after_disconnect_and_same_second_reenable() {
        let root = std::env::temp_dir().join(format!("harbor-rules-state-{}", Uuid::new_v4()));
        let admin = AdminConsoleStore::new(
            root.join("admin.json"),
            DeviceRegistryStore::new(root.join("devices.json")),
        );
        let server = Server::http("127.0.0.1:0").unwrap();
        let _env = HarborLinkFixtureEnv::new(&format!("http://{}", server.server_addr()));
        admin
            .save_home_assistant_orchestration_state(true, vec!["light".into()])
            .unwrap();
        let worker = thread::spawn(move || {
            for (index, state) in [
                Some("off"),
                None,
                Some("on"),
                Some("off"),
                Some("on"),
                Some("off"),
                Some("on"),
            ]
            .into_iter()
            .enumerate()
            {
                let request = server
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap()
                    .unwrap();
                assert_harborlink_request(&request, Method::Get, "/v1/home-assistant/entities");
                match state {
                    Some(state) => {
                        let payload = harborlink_entities(state, Some(format!("change-{index}")));
                        request.respond(Response::from_string(payload)).unwrap();
                    }
                    None => request
                        .respond(Response::from_string("offline").with_status_code(StatusCode(503)))
                        .unwrap(),
                }
            }
        });
        let rules = RulesStore::new(root.join("rules.json"));
        let mut definition = record_definition();
        definition["trigger"] = json!({"kind":"state","entity_id":"light.test","to":"on"});
        let rule = rules
            .create(serde_json::from_value(definition).unwrap(), 100)
            .unwrap();
        rules
            .preview(&rule.rule_id, 1, &BTreeMap::new(), 100)
            .unwrap();
        let first_activation = rules
            .set_status(&rule.rule_id, 1, "enabled", 100)
            .unwrap()
            .activation_id;
        let mut previous = BTreeMap::new();
        for time in 101..=104 {
            tick_rules(&rules, &admin, &mut previous, time).unwrap();
        }
        assert!(rules.history(&rule.rule_id).unwrap().is_empty());
        rules.set_status(&rule.rule_id, 1, "paused", 104).unwrap();
        let reenabled = rules.set_status(&rule.rule_id, 1, "enabled", 104).unwrap();
        assert_ne!(first_activation, reenabled.activation_id);
        let stale_run = rules.run_for_activation(
            &rule.rule_id,
            1,
            "stale:state",
            "state",
            &BTreeMap::new(),
            104,
            first_activation.as_deref(),
            |_| panic!("stale activation cannot execute"),
        );
        assert!(stale_run.unwrap_err().starts_with("CONFLICT:"));
        tick_rules(&rules, &admin, &mut previous, 105).unwrap();
        assert!(rules.history(&rule.rule_id).unwrap().is_empty());
        tick_rules(&rules, &admin, &mut previous, 106).unwrap();
        tick_rules(&rules, &admin, &mut previous, 107).unwrap();
        assert_eq!(rules.history(&rule.rule_id).unwrap().len(), 1);
        worker.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
