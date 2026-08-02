use std::env;
use std::path::PathBuf;
use std::time::Instant;

use harborbeacon_local_agent::runtime::knowledge_index::rebuild_embedding_hnsw;
use serde_json::json;

fn main() {
    let store_path = env::args_os().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        eprintln!("usage: build-knowledge-hnsw <embedding-store.json>");
        std::process::exit(2);
    });
    let started = Instant::now();
    match rebuild_embedding_hnsw(&store_path) {
        Ok(hnsw_path) => {
            let bytes = hnsw_path.metadata().map(|value| value.len()).unwrap_or(0);
            println!(
                "{}",
                json!({
                    "status": "completed",
                    "embedding_store": store_path,
                    "hnsw_index": hnsw_path,
                    "hnsw_bytes": bytes,
                    "elapsed_seconds": started.elapsed().as_secs_f64()
                })
            );
        }
        Err(error) => {
            eprintln!(
                "{}",
                json!({
                    "status": "failed",
                    "embedding_store": store_path,
                    "error": error,
                    "elapsed_seconds": started.elapsed().as_secs_f64()
                })
            );
            std::process::exit(1);
        }
    }
}
