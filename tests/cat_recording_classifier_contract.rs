use harborbeacon_local_agent::runtime::cat_recording_classifier::{
    aggregate_cat_recording_predictions, CatRecordingFramePrediction,
    CAT_RECORDING_CLASSIFIER_MIN_POSITIVE_FRAMES,
};

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
