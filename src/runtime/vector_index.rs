use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use fast_hnsw::distance::Cosine;
use fast_hnsw::{persist, Builder, Hnsw, PruneStrategy, SearchResult};

const HNSW_M: usize = 16;
const HNSW_EF_CONSTRUCTION: usize = 200;
const HNSW_EF_SEARCH: usize = 512;
const HNSW_SEED: u64 = 0x4841_5242_4f52_4f53;

type CachedIndex = Arc<Hnsw<Cosine>>;

fn index_cache() -> &'static RwLock<HashMap<PathBuf, CachedIndex>> {
    static CACHE: OnceLock<RwLock<HashMap<PathBuf, CachedIndex>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn build(
    path: &Path,
    vectors: impl IntoIterator<Item = Vec<f32>>,
    count: usize,
) -> Result<(), String> {
    let mut index: Hnsw<Cosine> = Builder::new()
        .m(HNSW_M)
        .ef_construction(HNSW_EF_CONSTRUCTION)
        .capacity(count)
        .seed(HNSW_SEED)
        .prune_strategy(PruneStrategy::Heuristic)
        .build(Cosine);
    for vector in vectors {
        index.insert(vector);
    }
    persist::save(&index, path)
        .map_err(|error| format!("failed to persist HNSW index {}: {error}", path.display()))
}

pub fn search(path: &Path, query: &[f32], top_k: usize) -> Result<Vec<SearchResult>, String> {
    if !path.is_file() {
        return Err(format!("knowledge HNSW index not found: {}", path.display()));
    }
    let cached = index_cache()
        .read()
        .map_err(|_| "knowledge HNSW cache lock is poisoned".to_string())?
        .get(path)
        .cloned();
    let index = if let Some(index) = cached {
        index
    } else {
        let loaded = Arc::new(
            persist::load_mmap(path, Cosine).map_err(|error| {
                format!("failed to load HNSW index {}: {error}", path.display())
            })?,
        );
        let mut cache = index_cache()
            .write()
            .map_err(|_| "knowledge HNSW cache lock is poisoned".to_string())?;
        cache.retain(|cached_path, _| cached_path.is_file());
        cache
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::clone(&loaded))
            .clone()
    };
    Ok(index.search(
        query,
        top_k.max(1),
        HNSW_EF_SEARCH.max(top_k.max(1)),
    ))
}

#[cfg(test)]
mod tests {
    use super::{build, search};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("harborbeacon-hnsw-{nonce}.hnsw"))
    }

    fn cosine(left: &[f32], right: &[f32]) -> f32 {
        let dot = left.iter().zip(right).map(|(a, b)| a * b).sum::<f32>();
        let left_norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
        let right_norm = right.iter().map(|value| value * value).sum::<f32>().sqrt();
        dot / (left_norm * right_norm)
    }

    #[test]
    fn hnsw_top_ten_overlap_stays_above_99_percent() {
        let path = unique_path();
        let vectors = (0..2_000)
            .map(|row| {
                (0..64)
                    .map(|column| {
                        let raw = ((row * 131 + column * 17 + row * column * 7) % 997) as f32;
                        raw / 498.5 - 1.0
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        build(&path, vectors.iter().cloned(), vectors.len()).expect("build HNSW fixture");

        let mut overlap = 0usize;
        let mut expected = 0usize;
        for query_index in (0..2_000).step_by(40) {
            let mut query = vectors[query_index].clone();
            for (column, value) in query.iter_mut().enumerate() {
                *value += ((query_index + column * 11) % 19) as f32 * 0.0005;
            }
            let mut exact = vectors
                .iter()
                .enumerate()
                .map(|(index, vector)| (cosine(&query, vector), index))
                .collect::<Vec<_>>();
            exact.sort_by(|left, right| right.0.total_cmp(&left.0));
            let exact = exact
                .into_iter()
                .take(10)
                .map(|(_, index)| index)
                .collect::<std::collections::HashSet<_>>();
            let approximate = search(&path, &query, 10).expect("search HNSW fixture");
            overlap += approximate
                .iter()
                .filter(|result| exact.contains(&result.id))
                .count();
            expected += exact.len();
        }

        let recall = overlap as f64 / expected as f64;
        assert!(recall >= 0.99, "HNSW overlap recall={recall:.4}");
        let _ = fs::remove_file(path);
    }
}
