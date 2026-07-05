use harborbeacon_local_agent::runtime::privacy_gateway::{
    evaluate_privacy_gateway_cases, privacy_gateway_eval_cases,
};

fn main() {
    let report = evaluate_privacy_gateway_cases(&privacy_gateway_eval_cases());
    match serde_json::to_string_pretty(&report) {
        Ok(json) => println!("{json}"),
        Err(error) => {
            eprintln!("failed to serialize privacy gateway report: {error}");
            std::process::exit(1);
        }
    }

    if !report.passed {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use harborbeacon_local_agent::runtime::privacy_gateway::{
        evaluate_privacy_gateway_cases, privacy_gateway_eval_cases, PRIVACY_GATEWAY_POLICY_VERSION,
    };

    #[test]
    fn evaluate_privacy_gateway_pack_passes() {
        let report = evaluate_privacy_gateway_cases(&privacy_gateway_eval_cases());

        assert!(report.passed, "{report:#?}");
        assert_eq!(report.policy_version, PRIVACY_GATEWAY_POLICY_VERSION);
        assert_eq!(report.source_leak_count, 0);
        assert!(report.blocked_or_degraded_count >= 2);
        assert!(report.high_risk_count >= 1);
    }
}
