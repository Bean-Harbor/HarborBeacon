use harborbeacon_local_agent::runtime::cat_recording_classifier::{
    aggregate_cat_recording_predictions, CatRecordingFramePrediction,
    CAT_RECORDING_CLASSIFIER_MIN_POSITIVE_FRAMES,
};
use std::fs;
use std::path::PathBuf;

fn prediction(frame_index: u8, probability_ppm: u32) -> CatRecordingFramePrediction {
    CatRecordingFramePrediction {
        frame_index,
        cat_probability_ppm: probability_ppm,
        inference_ms: 6,
    }
}

#[test]
fn classifier_requires_three_positive_frames_and_preserves_probabilities() {
    let predictions = (1..=9)
        .map(|frame_index| {
            let probability = match frame_index {
                2 => 810_000,
                5 => 830_000,
                7 => 790_000,
                _ => 120_000,
            };
            prediction(frame_index, probability)
        })
        .collect::<Vec<_>>();

    assert_eq!(CAT_RECORDING_CLASSIFIER_MIN_POSITIVE_FRAMES, 3);
    let result = aggregate_cat_recording_predictions(
        &predictions,
        620_000,
        CAT_RECORDING_CLASSIFIER_MIN_POSITIVE_FRAMES,
    )
    .expect("valid classifier result");

    assert!(result.cat_present);
    assert_eq!(result.reason_code, "cat_visible");
    assert_eq!(result.cat_frame_indices, vec![2, 5, 7]);
    assert_eq!(result.frame_predictions.len(), 9);
}

#[test]
fn classifier_keeps_two_positive_frames_out_of_accepted_state() {
    let predictions = (1..=9)
        .map(|frame_index| {
            let probability = if matches!(frame_index, 2 | 7) {
                810_000
            } else {
                120_000
            };
            prediction(frame_index, probability)
        })
        .collect::<Vec<_>>();

    let result = aggregate_cat_recording_predictions(
        &predictions,
        620_000,
        CAT_RECORDING_CLASSIFIER_MIN_POSITIVE_FRAMES,
    )
    .expect("valid classifier result");

    assert!(!result.cat_present);
    assert_eq!(result.reason_code, "uncertain");
    assert_eq!(result.cat_frame_indices, vec![2, 7]);
}

#[test]
fn classifier_rejects_duplicate_or_out_of_range_frame_indices() {
    let duplicate = vec![prediction(1, 100_000), prediction(1, 900_000)];
    assert!(aggregate_cat_recording_predictions(&duplicate, 620_000, 2)
        .unwrap_err()
        .contains("duplicate"));

    let invalid = vec![prediction(10, 900_000)];
    assert!(aggregate_cat_recording_predictions(&invalid, 620_000, 2)
        .unwrap_err()
        .contains("between 1 and 9"));
}

#[test]
fn packaged_classifier_policy_matches_the_runtime_gate() {
    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runtime_contract: serde_json::Value = serde_json::from_slice(
        &fs::read(repository_root.join(
            "config/harbornavi-k3/vision-models/\
mobilenetv2-cat-binary-v2-20260806/runtime-contract.json",
        ))
        .expect("runtime contract"),
    )
    .expect("valid runtime contract JSON");
    let python_classifier = fs::read_to_string(
        repository_root.join("scripts/harbornavi_k3_cat_recording_classifier.py"),
    )
    .expect("Python classifier");
    let build_script =
        fs::read_to_string(repository_root.join("scripts/build_harbornavi_k3_deb.sh"))
            .expect("package build script");

    assert_eq!(CAT_RECORDING_CLASSIFIER_MIN_POSITIVE_FRAMES, 3);
    assert_eq!(
        runtime_contract["video_decision"]["minimum_positive_frames"],
        3
    );
    assert_eq!(
        runtime_contract["video_decision"]["short_video_behavior"],
        "evaluate every distinct decodable sample; require 3 positives"
    );
    assert!(python_classifier.contains("MINIMUM_POSITIVE_FRAMES = 3"));
    assert!(python_classifier.contains("minimum_positive_frames: int = MINIMUM_POSITIVE_FRAMES"));
    assert!(python_classifier.contains("minimum_positive_frames=MINIMUM_POSITIVE_FRAMES"));
    assert!(
        build_script.contains("cat_recording_classifier_policy=up_to_9_frames_at_least_3_positive")
    );
    assert!(!build_script.contains("up_to_9_frames_at_least_2_positive"));

    let two_positive_predictions = (1..=9)
        .map(|frame_index| {
            prediction(
                frame_index,
                if matches!(frame_index, 2 | 5) {
                    810_000
                } else {
                    120_000
                },
            )
        })
        .collect::<Vec<_>>();
    assert!(
        !aggregate_cat_recording_predictions(
            &two_positive_predictions,
            620_000,
            CAT_RECORDING_CLASSIFIER_MIN_POSITIVE_FRAMES,
        )
        .expect("2/9 aggregation")
        .cat_present
    );
    let mut three_positive_predictions = two_positive_predictions;
    three_positive_predictions[6].cat_probability_ppm = 810_000;
    assert!(
        aggregate_cat_recording_predictions(
            &three_positive_predictions,
            620_000,
            CAT_RECORDING_CLASSIFIER_MIN_POSITIVE_FRAMES,
        )
        .expect("3/9 aggregation")
        .cat_present
    );
}
