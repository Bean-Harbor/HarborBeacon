//! Product jobs use the authenticated edge principal, never caller-supplied actor IDs.

use super::{
    authorize_authenticated_principal, error_json, json_response, ok_json, read_json_body,
    AccessAction, AdminApi, GateAuthenticatedPrincipal,
};
use harborbeacon_local_agent::runtime::product_jobs::{valid_id, ProductJob};
use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, ResponseBox, StatusCode};

const PREFIX: &str = "/api/product-jobs";

pub(super) fn is_product_jobs_path(path: &str) -> bool {
    path == PREFIX || path.starts_with("/api/product-jobs/")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    job_type: String,
    idempotency_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryRequest {
    idempotency_key: String,
}

fn projection(job: &ProductJob) -> Value {
    let mut value = serde_json::to_value(job).expect("product job serializes");
    value["can_cancel"] = json!(job.can_cancel());
    value["can_retry"] = json!(job.can_retry());
    value
}

fn job_error(error: &str) -> ResponseBox {
    let (status, code, message) = if error.starts_with("NOT_FOUND:") {
        (
            404,
            "TASK_NOT_FOUND",
            "Task or export is unavailable. Refresh and try again.",
        )
    } else if error.starts_with("CONFLICT:") {
        (
            409,
            "TASK_CONFLICT",
            "The task changed or this action is unavailable. Refresh its status.",
        )
    } else if error.starts_with("CAPACITY:") {
        (
            429,
            "TASK_CAPACITY",
            "Task capacity has been reached. Wait for active tasks or contact support.",
        )
    } else if error.starts_with("VALIDATION:") {
        (422, "TASK_INVALID", "The task request is invalid.")
    } else {
        (
            503,
            "TASK_STORAGE_UNAVAILABLE",
            "Tasks are temporarily unavailable. Try again later.",
        )
    };
    json_response(
        StatusCode(status),
        &json!({"error": {"code": code, "message": message}}),
    )
    .boxed()
}

fn parse_path(path: &str) -> Option<(&str, Option<&str>)> {
    let mut parts = path.strip_prefix("/api/product-jobs/")?.split('/');
    let id = parts.next()?;
    let action = parts.next();
    if !valid_id(id) || parts.next().is_some() || action == Some("") {
        return None;
    }
    Some((id, action))
}

impl AdminApi {
    pub(super) fn handle_product_jobs_request(
        &self,
        request: &mut Request,
        path: &str,
        authenticated: &GateAuthenticatedPrincipal,
    ) -> ResponseBox {
        let state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(_) => return job_error("STORAGE:"),
        };
        let action = if request.method() == &Method::Get {
            AccessAction::AdminReadState
        } else {
            AccessAction::AdminManage
        };
        let principal = authenticated.access_principal();
        if authorize_authenticated_principal(
            &state,
            principal,
            action,
            &format!("workspace:{}", principal.workspace_id),
        )
        .is_err()
        {
            return error_json(StatusCode(403), "Task access is not permitted").boxed();
        }
        let home = &principal.workspace_id;
        let actor = &principal.user_id;
        if path == PREFIX {
            return match request.method() {
                Method::Get => match self.product_jobs.list(home, actor) {
                    Ok(jobs) => {
                        ok_json(&json!({"jobs": jobs.iter().map(projection).collect::<Vec<_>>()}))
                            .boxed()
                    }
                    Err(error) => job_error(&error),
                },
                Method::Post => {
                    let body: CreateRequest = match read_json_body(request) {
                        Ok(body) => body,
                        Err(_) => return job_error("VALIDATION:"),
                    };
                    if body.job_type != "rules_history_export" {
                        return job_error("VALIDATION:");
                    }
                    self.create_product_export(home, actor, &body.idempotency_key, None)
                }
                _ => error_json(StatusCode(405), "Method not allowed").boxed(),
            };
        }
        let Some((id, suffix)) = parse_path(path) else {
            return job_error("NOT_FOUND:");
        };
        match (request.method(), suffix) {
            (Method::Get, None) => match self.product_jobs.get(home, actor, id) {
                Ok(job) => ok_json(&json!({"job": projection(&job)})).boxed(),
                Err(error) => job_error(&error),
            },
            (Method::Get, Some("result")) => match self.product_jobs.download(home, actor, id) {
                Ok(bytes) => Response::from_data(bytes)
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
                    .with_header(
                        Header::from_bytes(
                            "Content-Disposition",
                            "attachment; filename=\"rules-history.json\"",
                        )
                        .unwrap(),
                    )
                    .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
                    .boxed(),
                Err(error) => job_error(&error),
            },
            (Method::Post, Some("cancel")) => match self.product_jobs.cancel(home, actor, id) {
                Ok(job) => ok_json(&json!({"job": projection(&job)})).boxed(),
                Err(error) => job_error(&error),
            },
            (Method::Post, Some("retry")) => {
                let body: RetryRequest = match read_json_body(request) {
                    Ok(body) => body,
                    Err(_) => return job_error("VALIDATION:"),
                };
                self.create_product_export(home, actor, &body.idempotency_key, Some(id))
            }
            _ => error_json(StatusCode(405), "Method not allowed").boxed(),
        }
    }

    fn create_product_export(
        &self,
        home: &str,
        actor: &str,
        key: &str,
        retry: Option<&str>,
    ) -> ResponseBox {
        match self.product_jobs.create(home, actor, key, retry) {
            Ok((job, created)) => {
                if created {
                    let jobs = self.product_jobs.clone();
                    let rules = self.rules_store.clone();
                    let id = job.job_id.clone();
                    if std::thread::Builder::new()
                        .name("product-export".into())
                        .spawn(move || jobs.run_export(&id, &rules))
                        .is_err()
                    {
                        self.product_jobs.fail_to_start(&job.job_id);
                        return job_error("STORAGE:");
                    }
                }
                json_response(
                    StatusCode(if created { 202 } else { 200 }),
                    &json!({"job": projection(&job)}),
                )
                .boxed()
            }
            Err(error) => job_error(&error),
        }
    }
}
