//! Durable per-camera control policies for package detection.

use std::env;
use std::path::PathBuf;

pub use super::detection_control::{
    DetectionControlPolicy as PackageDetectionControlPolicy,
    DetectionControlStore as PackageDetectionControlStore, MAX_CAT_DETECTION_CONTROL_POLICIES,
    MAX_PENDING_DETECTION_LEASES,
};

pub const PACKAGE_DETECTION_CONTROL_PATH_ENV: &str = "HARBOR_K3_PACKAGE_DETECTION_CONTROL_PATH";
const DEFAULT_CONTROL_PATH: &str = ".harborbeacon/package-detection-controls.json";

pub fn default_control_path() -> PathBuf {
    env::var_os(PACKAGE_DETECTION_CONTROL_PATH_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONTROL_PATH))
}

pub fn validate_package_detection_control_camera_id(camera_id: &str) -> Result<(), String> {
    super::detection_control::validate_detection_control_camera_id(camera_id)
}
