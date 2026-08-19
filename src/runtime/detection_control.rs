//! Shared durable state primitives for per-camera detector controls.

pub use super::cat_detection_control::{
    CatDetectionControlPolicy as DetectionControlPolicy,
    CatDetectionControlStore as DetectionControlStore, MAX_CAT_DETECTION_CONTROL_POLICIES,
    MAX_PENDING_DETECTION_LEASES,
};

pub fn validate_detection_control_camera_id(camera_id: &str) -> Result<(), String> {
    super::cat_detection_control::validate_cat_detection_control_camera_id(camera_id)
}
