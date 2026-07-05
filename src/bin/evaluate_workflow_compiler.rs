use harborbeacon_local_agent::orchestrator::workflow_compiler::{
    build_workflow_shadow_evidence_report, evaluate_workflow, system_diagnostics_eval_cases,
    system_diagnostics_workflow_spec,
};

fn main() {
    let spec = system_diagnostics_workflow_spec();
    let cases = system_diagnostics_eval_cases();
    let report = evaluate_workflow(&spec, &cases);
    let evidence_mode = std::env::args().skip(1).any(|arg| arg == "--evidence");

    let output = if evidence_mode {
        serde_json::to_string_pretty(&build_workflow_shadow_evidence_report(&spec, &cases))
    } else {
        serde_json::to_string_pretty(&report)
    };

    match output {
        Ok(json) => println!("{json}"),
        Err(error) => {
            eprintln!("failed to serialize workflow compiler report: {error}");
            std::process::exit(1);
        }
    }

    if !report.passed {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use harborbeacon_local_agent::orchestrator::workflow_compiler::{
        build_workflow_shadow_evidence_report, evaluate_workflow, system_diagnostics_eval_cases,
        system_diagnostics_workflow_spec,
    };

    #[test]
    fn evaluate_workflow_compiler_system_diagnostics_pack_passes() {
        let spec = system_diagnostics_workflow_spec();
        let report = evaluate_workflow(&spec, &system_diagnostics_eval_cases());
        assert!(report.passed, "{report:#?}");
        assert_eq!(report.unauthorized_action_count, 0);
    }

    #[test]
    fn evaluate_workflow_compiler_evidence_pack_passes() {
        let spec = system_diagnostics_workflow_spec();
        let report = build_workflow_shadow_evidence_report(&spec, &system_diagnostics_eval_cases());
        assert!(report.passed, "{report:#?}");
        assert!(report.redacted);
        assert_eq!(report.unauthorized_action_count, 0);
    }
}
