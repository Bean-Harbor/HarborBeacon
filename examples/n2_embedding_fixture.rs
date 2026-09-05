//! Prepare native qualification inputs using the exact production tokenizer.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{env, fs, path::Path};
use tokenizers::Tokenizer;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        return Err("usage: n2_embedding_fixture TOKENIZER INPUT_JSON OUTPUT_JSON".into());
    }
    let bytes = fs::read(&args[1])?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if digest != harborbeacon_local_agent::runtime::fixed_models::TOKENIZER_SHA256 {
        return Err("tokenizer SHA does not match the official manifest".into());
    }
    let tokenizer = Tokenizer::from_file(Path::new(&args[1]))?;
    let inputs: Vec<Value> = serde_json::from_slice(&fs::read(&args[2])?)?;
    let mut cases = Vec::new();
    for input in inputs {
        let text = input["text"]
            .as_str()
            .ok_or("text must be a string")?
            .repeat(input["repeat"].as_u64().unwrap_or(1) as usize);
        let encoding = tokenizer.encode(text.as_str(), true)?;
        cases.push(json!({"id": input["id"], "text": text, "input_ids": encoding.get_ids()}));
    }
    fs::write(
        &args[3],
        serde_json::to_vec_pretty(&json!({"tokenizer_sha256": digest, "cases": cases}))?,
    )?;
    Ok(())
}
