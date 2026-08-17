use harborbeacon_local_agent::runtime::cat_recording_sampling::{
    build_cat_recording_sampling_plan, CatRecordingSamplingRequest,
};
use std::io::{self, Read};

const MAX_REQUEST_BYTES: u64 = 64 * 1024;

fn run() -> Result<(), String> {
    let mut payload = Vec::new();
    io::stdin()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut payload)
        .map_err(|error| format!("failed to read sampling request: {error}"))?;
    if payload.is_empty() || payload.len() as u64 > MAX_REQUEST_BYTES {
        return Err("sampling request is empty or exceeds 65536 bytes".to_string());
    }
    let request: CatRecordingSamplingRequest = serde_json::from_slice(&payload)
        .map_err(|error| format!("sampling request is invalid: {error}"))?;
    let plan = build_cat_recording_sampling_plan(&request)?;
    serde_json::to_writer(io::stdout().lock(), &plan)
        .map_err(|error| format!("failed to serialize sampling plan: {error}"))?;
    println!();
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("cat_sampling_plan_error={error}");
        std::process::exit(2);
    }
}
