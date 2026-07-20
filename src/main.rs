use std::fs;
use std::path::PathBuf;

use serde_json::json;

use harborbeacon_local_agent::orchestrator::approval::{AutonomyConfig, AutonomyLevel};
use harborbeacon_local_agent::orchestrator::executors::harbor_ops::{
    register_harbor_executors, HarborExecutorConfig,
};
use harborbeacon_local_agent::orchestrator::executors::vision::VisionExecutor;
use harborbeacon_local_agent::orchestrator::tool_loop::{
    ToolCall, ToolLoopConfig, ToolLoopEngine, ToolRegistry,
};
use harborbeacon_local_agent::orchestrator::{
    ApprovalContext, ApprovalManager, Router, Runtime, TaskPlan,
};

#[derive(Debug, Clone, Copy)]
enum RunMode {
    /// Classic: read plan JSON, execute, print report
    Plan,
    /// Tool-loop: run LLM→tool→LLM iteration from plan JSON
    Loop,
}

#[derive(Debug, Clone)]
struct Cli {
    plan: Option<PathBuf>,
    mode: RunMode,
    approval_token: Option<String>,
    required_approval_token: Option<String>,
    disable_middleware: bool,
    disable_midcli: bool,
    midcli_passthrough: bool,
    force_dry_run: bool,
    harbor_url: Option<String>,
    harbor_api_key: Option<String>,
    harbor_user: Option<String>,
    harbor_password: Option<String>,
    autonomy: AutonomyArg,
    max_iterations: usize,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            plan: None,
            mode: RunMode::Plan,
            approval_token: None,
            required_approval_token: None,
            disable_middleware: false,
            disable_midcli: false,
            midcli_passthrough: false,
            force_dry_run: false,
            harbor_url: None,
            harbor_api_key: None,
            harbor_user: None,
            harbor_password: None,
            autonomy: AutonomyArg::Supervised,
            max_iterations: 8,
        }
    }
}

impl Cli {
    fn parse() -> Self {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
            print_usage();
            std::process::exit(0);
        }

        let mut cli = Self::default();
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--plan" => cli.plan = Some(PathBuf::from(take_value(&args, &mut index, "--plan"))),
                value if value.starts_with("--plan=") => {
                    cli.plan = Some(PathBuf::from(value["--plan=".len()..].to_string()));
                }
                "--mode" => cli.mode = parse_run_mode(&take_value(&args, &mut index, "--mode")),
                value if value.starts_with("--mode=") => {
                    cli.mode = parse_run_mode(&value["--mode=".len()..]);
                }
                "--approval-token" => {
                    cli.approval_token = Some(take_value(&args, &mut index, "--approval-token"))
                }
                value if value.starts_with("--approval-token=") => {
                    cli.approval_token = Some(value["--approval-token=".len()..].to_string());
                }
                "--required-approval-token" => {
                    cli.required_approval_token =
                        Some(take_value(&args, &mut index, "--required-approval-token"))
                }
                value if value.starts_with("--required-approval-token=") => {
                    cli.required_approval_token =
                        Some(value["--required-approval-token=".len()..].to_string());
                }
                "--disable-middleware" => cli.disable_middleware = true,
                "--disable-midcli" => cli.disable_midcli = true,
                "--midcli-passthrough" => cli.midcli_passthrough = true,
                "--force-dry-run" => cli.force_dry_run = true,
                "--harbor-url" => {
                    cli.harbor_url = Some(take_value(&args, &mut index, "--harbor-url"))
                }
                value if value.starts_with("--harbor-url=") => {
                    cli.harbor_url = Some(value["--harbor-url=".len()..].to_string());
                }
                "--harbor-api-key" => {
                    cli.harbor_api_key = Some(take_value(&args, &mut index, "--harbor-api-key"))
                }
                value if value.starts_with("--harbor-api-key=") => {
                    cli.harbor_api_key = Some(value["--harbor-api-key=".len()..].to_string());
                }
                "--harbor-user" => {
                    cli.harbor_user = Some(take_value(&args, &mut index, "--harbor-user"))
                }
                value if value.starts_with("--harbor-user=") => {
                    cli.harbor_user = Some(value["--harbor-user=".len()..].to_string());
                }
                "--harbor-password" => {
                    cli.harbor_password = Some(take_value(&args, &mut index, "--harbor-password"))
                }
                value if value.starts_with("--harbor-password=") => {
                    cli.harbor_password = Some(value["--harbor-password=".len()..].to_string());
                }
                "--autonomy" => {
                    cli.autonomy = parse_autonomy(&take_value(&args, &mut index, "--autonomy"))
                }
                value if value.starts_with("--autonomy=") => {
                    cli.autonomy = parse_autonomy(&value["--autonomy=".len()..]);
                }
                "--max-iterations" => {
                    cli.max_iterations =
                        take_usize(&take_value(&args, &mut index, "--max-iterations"))
                }
                value if value.starts_with("--max-iterations=") => {
                    cli.max_iterations = take_usize(&value["--max-iterations=".len()..]);
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                value if value.starts_with('-') => fail(&format!("unknown flag: {value}")),
                value => {
                    if cli.plan.is_none() {
                        cli.plan = Some(PathBuf::from(value));
                    } else {
                        fail(&format!("unexpected positional argument: {value}"));
                    }
                }
            }
            index += 1;
        }

        cli
    }
}

#[derive(Debug, Clone, Copy)]
enum AutonomyArg {
    Readonly,
    Supervised,
    Full,
}

impl From<AutonomyArg> for AutonomyLevel {
    fn from(a: AutonomyArg) -> Self {
        match a {
            AutonomyArg::Readonly => AutonomyLevel::ReadOnly,
            AutonomyArg::Supervised => AutonomyLevel::Supervised,
            AutonomyArg::Full => AutonomyLevel::Full,
        }
    }
}

fn main() {
    let cli = Cli::parse();

    let plan_path = match &cli.plan {
        Some(path) => path,
        None => {
            eprintln!("--plan is required");
            std::process::exit(1);
        }
    };

    let plan_text = match fs::read_to_string(plan_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to read plan file: {e}");
            std::process::exit(1);
        }
    };

    let mut plan = match TaskPlan::from_json_str(&plan_text) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("failed to parse plan JSON: {e}");
            std::process::exit(1);
        }
    };

    if cli.force_dry_run {
        for action in &mut plan.steps {
            action.dry_run = true;
        }
    }

    // Build approval manager
    let _approval_mgr = ApprovalManager::from_config(&AutonomyConfig {
        level: cli.autonomy.into(),
        auto_approve: vec![],
        always_ask: vec![],
        non_cli_excluded: vec![],
    });

    match cli.mode {
        RunMode::Plan => run_plan_mode(&cli, plan),
        RunMode::Loop => run_loop_mode(&cli, &plan),
    }
}

fn take_value(args: &[String], index: &mut usize, flag: &str) -> String {
    *index += 1;
    if *index >= args.len() {
        fail(&format!("missing value for {flag}"));
    }
    args[*index].clone()
}

fn take_usize(value: &str) -> usize {
    value
        .parse::<usize>()
        .unwrap_or_else(|_| fail(&format!("invalid integer value: {value}")))
}

fn parse_run_mode(value: &str) -> RunMode {
    match value.to_ascii_lowercase().as_str() {
        "plan" => RunMode::Plan,
        "loop" => RunMode::Loop,
        _ => fail(&format!("invalid mode: {value}")),
    }
}

fn parse_autonomy(value: &str) -> AutonomyArg {
    match value.to_ascii_lowercase().as_str() {
        "readonly" => AutonomyArg::Readonly,
        "supervised" => AutonomyArg::Supervised,
        "full" => AutonomyArg::Full,
        _ => fail(&format!("invalid autonomy level: {value}")),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}

fn print_usage() {
    eprintln!(
        "Usage: harborbeacon-agent --plan FILE [--mode plan|loop] [--approval-token TOKEN] [--required-approval-token TOKEN] [--disable-middleware] [--disable-midcli] [--midcli-passthrough] [--force-dry-run] [--harbor-url URL] [--harbor-api-key KEY] [--harbor-user USER] [--harbor-password PASS] [--autonomy readonly|supervised|full] [--max-iterations N]"
    );
}

fn run_plan_mode(cli: &Cli, plan: TaskPlan) {
    let mut router = Router::new();
    let harbor_config = HarborExecutorConfig::from_cli(
        cli.harbor_url.clone(),
        cli.harbor_api_key.clone(),
        cli.harbor_user.clone(),
        cli.harbor_password.clone(),
        cli.disable_middleware,
        cli.disable_midcli,
        cli.midcli_passthrough,
    );
    if let Err(error) = register_harbor_executors(&mut router, &harbor_config) {
        eprintln!("failed to configure HarborOS executors: {error}");
        std::process::exit(1);
    }

    router.register(Box::new(VisionExecutor::new(
        harborbeacon_local_agent::runtime::registry::DeviceRegistryStore::new(PathBuf::from(
            ".harborbeacon/device-registry.json",
        )),
    )));

    let approval = ApprovalContext {
        token: cli.approval_token.clone(),
        required_token: cli.required_approval_token.clone(),
        approver_id: None,
    };

    let mut runtime = Runtime::new(router, Some(approval));
    let task_result = runtime.execute_plan(plan);

    let report = json!({
        "task_result": task_result,
        "summary": task_result.summary(),
        "audit_events": runtime.audit().events(),
    });

    match serde_json::to_string_pretty(&report) {
        Ok(out) => println!("{out}"),
        Err(e) => {
            eprintln!("failed to serialize report: {e}");
            std::process::exit(1);
        }
    }
}

fn run_loop_mode(cli: &Cli, plan: &TaskPlan) {
    let registry = ToolRegistry::new();
    // In production: register real tools from skills registry here.

    let engine = ToolLoopEngine::new(
        registry,
        ToolLoopConfig {
            max_iterations: cli.max_iterations,
            timeout_ms: 30_000,
        },
    );

    // The resolve_fn simulates planner logic: emit plan steps as tool calls,
    // then FinalAnswer. A real integration replaces this with LLM calls.
    let steps_snapshot: Vec<_> = plan
        .steps
        .iter()
        .map(|a| {
            json!({
                "domain": a.domain,
                "operation": a.operation,
                "resource": a.resource,
            })
        })
        .collect();

    let total_planned = steps_snapshot.len();

    let trace = engine.run(|history| {
        let done_count = history.len();
        if done_count < total_planned {
            ToolCall::Invoke {
                tool: format!(
                    "{}.{}",
                    steps_snapshot[done_count]["domain"].as_str().unwrap_or("?"),
                    steps_snapshot[done_count]["operation"]
                        .as_str()
                        .unwrap_or("?")
                ),
                args: steps_snapshot[done_count].clone(),
            }
        } else {
            ToolCall::FinalAnswer {
                answer: format!("completed {total_planned} steps via tool-loop"),
            }
        }
    });

    match serde_json::to_string_pretty(&trace) {
        Ok(out) => println!("{out}"),
        Err(e) => {
            eprintln!("failed to serialize trace: {e}");
            std::process::exit(1);
        }
    }
}
