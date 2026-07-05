pub mod approval;
pub mod audit;
pub mod channel;
pub mod contracts;
pub mod executors;
pub mod policy;
pub mod router;
pub mod runtime;
pub mod tool_loop;
pub mod workflow_compiler;

pub use approval::{ApprovalManager, ApprovalResponse, AutonomyConfig, AutonomyLevel};
pub use channel::{Channel, CliChannel, HarborBeaconChannel, InboundMessage, OutboundMessage};
pub use contracts::{Action, ExecutionResult, RiskLevel, Route, StepStatus, TaskPlan, TaskResult};
pub use policy::{enforce, ApprovalContext, PolicyViolation};
pub use router::{allowed_routes, Executor, Router};
pub use runtime::Runtime;
pub use tool_loop::{
    Tool, ToolCall, ToolLoopConfig, ToolLoopEngine, ToolLoopTrace, ToolOutput, ToolRegistry,
};
pub use workflow_compiler::{
    build_workflow_shadow_evidence_report, compile_system_diagnostics_candidate,
    compile_workflow_candidate, evaluate_workflow, system_diagnostics_eval_cases,
    system_diagnostics_workflow_spec, WorkflowCandidate, WorkflowEvalCase, WorkflowEvalReport,
    WorkflowNode, WorkflowShadowEvidenceCase, WorkflowShadowEvidenceReport, WorkflowSpec,
};
