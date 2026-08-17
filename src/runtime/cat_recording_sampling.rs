use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

pub const CAT_RECORDING_SAMPLING_SCHEMA_VERSION: u8 = 1;
pub const CAT_RECORDING_SAMPLING_FRAME_MARGIN_MS: u64 = 100;
pub const CAT_RECORDING_SAMPLING_MAX_EVIDENCE: usize = 256;
pub const CAT_RECORDING_SAMPLING_MAX_FRAMES: usize = 9;
pub const CAT_RECORDING_SAMPLING_MAX_GUIDED_FRAMES: usize = 5;
pub const CAT_RECORDING_SAMPLING_MIN_DURATION_MS: u64 = 5_000;
pub const CAT_RECORDING_SAMPLING_MAX_DURATION_MS: u64 = 600_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatRecordingSamplingEvidence {
    pub sequence: u64,
    pub frame_epoch_ms: u64,
    pub confidence_ppm: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatRecordingSamplingRequest {
    pub schema_version: u8,
    pub duration_ms: u64,
    pub recording_started_at_epoch_ms: u64,
    pub recording_ended_at_epoch_ms: u64,
    pub detection_evidence: Vec<CatRecordingSamplingEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatRecordingSamplingPlan {
    pub schema_version: u8,
    pub strategy: String,
    pub duration_ms: u64,
    pub eligible_detection_evidence_count: usize,
    pub sample_offsets_ms: Vec<u64>,
}

pub fn build_cat_recording_sampling_plan(
    request: &CatRecordingSamplingRequest,
) -> Result<CatRecordingSamplingPlan, String> {
    validate_request(request)?;
    let evidence = eligible_evidence_offsets(request);
    let mut offsets = select_diverse_offsets(&evidence, CAT_RECORDING_SAMPLING_MAX_GUIDED_FRAMES);
    let strategy = if evidence.is_empty() {
        "uniform_9"
    } else {
        "yolo_guided_hybrid_9"
    };
    for candidate in uniform_offsets(request.duration_ms) {
        if offsets.len() >= CAT_RECORDING_SAMPLING_MAX_FRAMES {
            break;
        }
        if offsets
            .iter()
            .all(|offset| offset.abs_diff(candidate) >= CAT_RECORDING_SAMPLING_FRAME_MARGIN_MS)
        {
            offsets.push(candidate);
        }
    }
    offsets.truncate(CAT_RECORDING_SAMPLING_MAX_FRAMES);
    if offsets.len() != CAT_RECORDING_SAMPLING_MAX_FRAMES {
        return Err("sampling plan did not produce exactly nine distinct frames".to_string());
    }
    Ok(CatRecordingSamplingPlan {
        schema_version: CAT_RECORDING_SAMPLING_SCHEMA_VERSION,
        strategy: strategy.to_string(),
        duration_ms: request.duration_ms,
        eligible_detection_evidence_count: evidence.len(),
        sample_offsets_ms: offsets,
    })
}

fn validate_request(request: &CatRecordingSamplingRequest) -> Result<(), String> {
    if request.schema_version != CAT_RECORDING_SAMPLING_SCHEMA_VERSION {
        return Err("sampling request schema_version must be 1".to_string());
    }
    if !(CAT_RECORDING_SAMPLING_MIN_DURATION_MS..=CAT_RECORDING_SAMPLING_MAX_DURATION_MS)
        .contains(&request.duration_ms)
    {
        return Err("sampling duration_ms must be within 5000..600000".to_string());
    }
    if request.recording_started_at_epoch_ms == 0
        || request.recording_ended_at_epoch_ms < request.recording_started_at_epoch_ms
    {
        return Err("recording time bounds are invalid".to_string());
    }
    if request.detection_evidence.len() > CAT_RECORDING_SAMPLING_MAX_EVIDENCE {
        return Err("sampling request has more than 256 evidence records".to_string());
    }
    let mut sequences = HashSet::with_capacity(request.detection_evidence.len());
    for evidence in &request.detection_evidence {
        if evidence.sequence == 0 || evidence.frame_epoch_ms == 0 {
            return Err("detection evidence sequence and frame time must be positive".to_string());
        }
        if evidence.confidence_ppm > 1_000_000 {
            return Err("detection evidence confidence_ppm exceeds 1000000".to_string());
        }
        if !sequences.insert(evidence.sequence) {
            return Err("detection evidence sequences must be unique".to_string());
        }
    }
    Ok(())
}

fn eligible_evidence_offsets(request: &CatRecordingSamplingRequest) -> Vec<(u64, u32)> {
    let lower_bound = request.recording_started_at_epoch_ms.saturating_sub(2_000);
    let upper_bound = request
        .recording_ended_at_epoch_ms
        .max(
            request
                .recording_started_at_epoch_ms
                .saturating_add(request.duration_ms),
        )
        .saturating_add(1_000);
    let mut offsets = BTreeMap::<u64, u32>::new();
    for evidence in &request.detection_evidence {
        if evidence.frame_epoch_ms < lower_bound || evidence.frame_epoch_ms > upper_bound {
            continue;
        }
        let raw_offset = evidence
            .frame_epoch_ms
            .saturating_sub(request.recording_started_at_epoch_ms);
        let offset = bounded_offset(raw_offset, request.duration_ms);
        offsets
            .entry(offset)
            .and_modify(|confidence| *confidence = (*confidence).max(evidence.confidence_ppm))
            .or_insert(evidence.confidence_ppm);
    }
    offsets.into_iter().collect()
}

fn uniform_offsets(duration_ms: u64) -> Vec<u64> {
    [50_u64, 10, 90, 30, 70, 20, 80, 40, 60]
        .into_iter()
        .map(|percent| bounded_offset(duration_ms.saturating_mul(percent) / 100, duration_ms))
        .fold(Vec::new(), |mut offsets, offset| {
            if !offsets.contains(&offset) {
                offsets.push(offset);
            }
            offsets
        })
}

fn bounded_offset(offset_ms: u64, duration_ms: u64) -> u64 {
    if duration_ms <= CAT_RECORDING_SAMPLING_FRAME_MARGIN_MS.saturating_mul(2) {
        return duration_ms / 2;
    }
    offset_ms.clamp(
        CAT_RECORDING_SAMPLING_FRAME_MARGIN_MS,
        duration_ms - CAT_RECORDING_SAMPLING_FRAME_MARGIN_MS,
    )
}

fn select_diverse_offsets(candidates: &[(u64, u32)], limit: usize) -> Vec<u64> {
    if candidates.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut selected = Vec::<usize>::new();
    let peak_index = candidates
        .iter()
        .enumerate()
        .max_by_key(|(_, (_, confidence))| *confidence)
        .map(|(index, _)| index)
        .unwrap_or(0);
    selected.push(peak_index);
    for index in [0, candidates.len() - 1] {
        if selected.len() >= limit {
            break;
        }
        if !selected.contains(&index) {
            selected.push(index);
        }
    }
    while selected.len() < limit && selected.len() < candidates.len() {
        let next = candidates
            .iter()
            .enumerate()
            .filter(|(index, _)| !selected.contains(index))
            .map(|(index, (offset, confidence))| {
                let minimum_distance = selected
                    .iter()
                    .map(|selected_index| offset.abs_diff(candidates[*selected_index].0))
                    .min()
                    .unwrap_or_default();
                (index, minimum_distance, *confidence)
            })
            .max_by_key(|(_, distance, confidence)| (*distance, *confidence))
            .map(|(index, _, _)| index);
        let Some(next) = next else {
            break;
        };
        selected.push(next);
    }
    selected
        .into_iter()
        .map(|index| candidates[index].0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        build_cat_recording_sampling_plan, CatRecordingSamplingEvidence,
        CatRecordingSamplingRequest,
    };

    fn request(evidence: Vec<CatRecordingSamplingEvidence>) -> CatRecordingSamplingRequest {
        CatRecordingSamplingRequest {
            schema_version: 1,
            duration_ms: 10_000,
            recording_started_at_epoch_ms: 100_000,
            recording_ended_at_epoch_ms: 110_000,
            detection_evidence: evidence,
        }
    }

    #[test]
    fn guided_plan_matches_the_production_selection_order() {
        let plan = build_cat_recording_sampling_plan(&request(vec![
            CatRecordingSamplingEvidence {
                sequence: 1,
                frame_epoch_ms: 99_500,
                confidence_ppm: 700_000,
            },
            CatRecordingSamplingEvidence {
                sequence: 2,
                frame_epoch_ms: 101_000,
                confidence_ppm: 600_000,
            },
            CatRecordingSamplingEvidence {
                sequence: 3,
                frame_epoch_ms: 105_000,
                confidence_ppm: 950_000,
            },
            CatRecordingSamplingEvidence {
                sequence: 4,
                frame_epoch_ms: 109_000,
                confidence_ppm: 800_000,
            },
        ]))
        .expect("guided sampling plan");

        assert_eq!(plan.strategy, "yolo_guided_hybrid_9");
        assert_eq!(plan.eligible_detection_evidence_count, 4);
        assert_eq!(
            plan.sample_offsets_ms,
            vec![5_000, 100, 9_000, 1_000, 3_000, 7_000, 2_000, 8_000, 4_000]
        );
    }

    #[test]
    fn uniform_plan_is_the_same_defensive_fallback_used_by_production() {
        let plan =
            build_cat_recording_sampling_plan(&request(Vec::new())).expect("uniform sampling plan");
        assert_eq!(plan.strategy, "uniform_9");
        assert_eq!(
            plan.sample_offsets_ms,
            vec![5_000, 1_000, 9_000, 3_000, 7_000, 2_000, 8_000, 4_000, 6_000]
        );
    }

    #[test]
    fn malformed_or_oversized_evidence_fails_closed() {
        let error = build_cat_recording_sampling_plan(&request(vec![
            CatRecordingSamplingEvidence {
                sequence: 1,
                frame_epoch_ms: 101_000,
                confidence_ppm: 700_000,
            },
            CatRecordingSamplingEvidence {
                sequence: 1,
                frame_epoch_ms: 102_000,
                confidence_ppm: 800_000,
            },
        ]))
        .expect_err("duplicate sequence must fail");
        assert!(error.contains("unique"));

        let mut oversized = Vec::new();
        for sequence in 1..=257 {
            oversized.push(CatRecordingSamplingEvidence {
                sequence,
                frame_epoch_ms: 100_000 + sequence,
                confidence_ppm: 700_000,
            });
        }
        assert!(build_cat_recording_sampling_plan(&request(oversized)).is_err());
    }
}
