//! Vision domain actions for detection and image understanding.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::connectors::storage::StorageObjectRef;
use crate::runtime::media::DeviceArtifactMetadata;
use crate::runtime::registry::ResolvedCameraTarget;

pub const DOMAIN: &str = "vision";
pub const OP_ANALYZE_CAMERA: &str = "analyze_camera";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisionAnalyzeCameraArgs {
    pub device_id: String,
    #[serde(default = "default_detect_label")]
    pub detect_label: String,
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f32,
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisionDetection {
    pub label: String,
    pub confidence: f32,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisionImageIndexSidecar {
    pub artifact_role: String,
    pub image_path: String,
    #[serde(default)]
    pub source_image_path: Option<String>,
    #[serde(default)]
    pub annotated_image_path: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub derived_text: Option<String>,
    pub mime_type: String,
    #[serde(default)]
    pub source_storage: Option<StorageObjectRef>,
    #[serde(default)]
    pub byte_size: Option<u64>,
    #[serde(default)]
    pub captured_at_epoch_ms: Option<u128>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub ingest_metadata: Option<DeviceArtifactMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisionImageArtifact {
    pub image_path: String,
    #[serde(default)]
    pub source_image_path: Option<String>,
    pub annotated_image_path: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub derived_text: Option<String>,
    pub mime_type: String,
    #[serde(default)]
    pub source_storage: Option<StorageObjectRef>,
    #[serde(default)]
    pub byte_size: Option<u64>,
    #[serde(default)]
    pub captured_at_epoch_ms: Option<u128>,
    #[serde(default)]
    pub index_sidecar_path: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub ingest_metadata: Option<DeviceArtifactMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisionAnalyzeCameraPayload {
    #[serde(rename = "camera_target", alias = "device")]
    pub camera_target: ResolvedCameraTarget,
    pub summary: String,
    pub summary_source: String,
    #[serde(default)]
    pub detections: Vec<VisionDetection>,
    pub snapshot: VisionImageArtifact,
    pub detection_summary: String,
    #[serde(default = "default_notification_channel")]
    pub notification_channel: String,
    #[serde(default = "default_notification_format")]
    pub notification_format: String,
    #[serde(default, alias = "feishu_card")]
    pub notification_card: Value,
    #[serde(default)]
    pub delivery_hints: Vec<Value>,
}

impl VisionAnalyzeCameraPayload {
    pub fn with_delivery_contract(mut self) -> Self {
        let mut hints = vec![vision_image_delivery_hint(
            "analysis_snapshot",
            &self.snapshot.image_path,
        )];
        if let Some(path) = self
            .snapshot
            .annotated_image_path
            .as_deref()
            .filter(|path| !path.trim().is_empty())
        {
            hints.push(vision_image_delivery_hint("analysis_annotation", path));
        }
        self.delivery_hints = hints;
        self
    }
}

pub fn stable_vision_image_artifact_id(artifact_role: &str, path: &str) -> String {
    let digest = Sha256::digest(format!("{artifact_role}\0{}", path.trim()).as_bytes());
    let suffix = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("vision-image-{suffix}")
}

fn vision_image_delivery_hint(artifact_role: &str, path: &str) -> Value {
    json!({
        "kind": "native_image",
        "artifact_id": stable_vision_image_artifact_id(artifact_role, path),
        "fallback": "file",
        "metadata": {
            "artifact_role": artifact_role,
        },
    })
}

fn default_detect_label() -> String {
    "person".to_string()
}

fn default_min_confidence() -> f32 {
    0.25
}

fn default_notification_channel() -> String {
    "im_bridge".to_string()
}

fn default_notification_format() -> String {
    "lark_card".to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        VisionAnalyzeCameraArgs, VisionAnalyzeCameraPayload, VisionImageArtifact, OP_ANALYZE_CAMERA,
    };
    use crate::connectors::storage::{StorageObjectRef, StorageTarget};
    use crate::runtime::media::DeviceArtifactMetadata;

    #[test]
    fn analyze_camera_args_use_person_defaults() {
        let args: VisionAnalyzeCameraArgs =
            serde_json::from_str(r#"{"device_id":"cam-1"}"#).expect("parse args");

        assert_eq!(OP_ANALYZE_CAMERA, "analyze_camera");
        assert_eq!(args.detect_label, "person");
        assert!((args.min_confidence - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn payload_accepts_legacy_feishu_card_alias() {
        let payload: VisionAnalyzeCameraPayload = serde_json::from_value(json!({
            "device": {
                "device_id": "cam-1",
                "name": "Front Door",
                "kind": "camera",
                "status": "online",
                "room": null,
                "vendor": null,
                "model": null,
                "ip_address": "192.168.1.10",
                "mac_address": null,
                "discovery_source": "onvif",
                "primary_stream": {
                    "transport": "rtsp",
                    "url": "rtsp://192.168.1.10/live",
                    "requires_auth": false
                },
                "onvif_device_service_url": null,
                "ezviz_device_serial": null,
                "ezviz_camera_no": null,
                "capabilities": {
                    "snapshot": true,
                    "stream": true,
                    "ptz": false,
                    "audio": false
                },
                "last_seen_at": null
            },
            "summary": "画面中有 1 人",
            "summary_source": "openai_compatible",
            "detections": [],
            "snapshot": {
                "image_path": "snap.jpg",
                "source_image_path": "snap.jpg",
                "annotated_image_path": null,
                "caption": null,
                "derived_text": null,
                "mime_type": "image/jpeg",
                "source_storage": null,
                "byte_size": null,
                "captured_at_epoch_ms": null,
                "tags": [],
                "labels": [],
                "index_sidecar_path": null,
                "ingest_metadata": null
            },
            "detection_summary": "检测到 1 个 person，最高置信度 88%。",
            "feishu_card": {"header": {"title": {"content": "Front Door AI 分析"}}}
        }))
        .expect("payload");

        assert_eq!(payload.notification_channel, "im_bridge");
        assert_eq!(payload.notification_format, "lark_card");
        assert!(payload.delivery_hints.is_empty());
        assert_eq!(payload.camera_target.device_id, "cam-1");
        assert_eq!(payload.snapshot.source_storage, None);
        assert_eq!(payload.snapshot.byte_size, None);
        assert_eq!(payload.snapshot.captured_at_epoch_ms, None);
        assert_eq!(
            payload.snapshot.source_image_path.as_deref(),
            Some("snap.jpg")
        );
        assert_eq!(payload.snapshot.caption, None);
        assert_eq!(payload.snapshot.derived_text, None);
        assert!(payload.snapshot.tags.is_empty());
        assert!(payload.snapshot.labels.is_empty());
        assert_eq!(payload.snapshot.index_sidecar_path, None);
        assert_eq!(payload.snapshot.ingest_metadata, None);
        assert_eq!(
            payload.notification_card["header"]["title"]["content"],
            "Front Door AI 分析"
        );
    }

    #[test]
    fn image_artifact_accepts_ingest_metadata_for_future_indexing() {
        let artifact = VisionImageArtifact {
            image_path: "snapshot.jpg".to_string(),
            annotated_image_path: Some("snapshot-annotated.jpg".to_string()),
            mime_type: "image/jpeg".to_string(),
            source_storage: Some(StorageObjectRef {
                target: StorageTarget::LocalDisk,
                relative_path: "snapshots/cam-1/123.jpg".to_string(),
            }),
            byte_size: Some(123),
            captured_at_epoch_ms: Some(1710000000000),
            source_image_path: Some("snapshots/cam-1/123.jpg".to_string()),
            index_sidecar_path: Some("snapshots/cam-1/123.json".to_string()),
            caption: Some("Front Door AI snapshot".to_string()),
            derived_text: Some("检测到 person; labels: person".to_string()),
            tags: vec!["camera".to_string(), "snapshot".to_string()],
            labels: vec!["person".to_string()],
            ingest_metadata: Some(
                DeviceArtifactMetadata::knowledge_index_candidate("cam-1", 1710000000000)
                    .with_device_context(
                        Some("Front Door".to_string()),
                        Some("Entry".to_string()),
                        Some("DemoCam".to_string()),
                        Some("C1".to_string()),
                        Some("manual_entry".to_string()),
                        Some("rtsp".to_string()),
                        Some(false),
                    ),
            ),
        };

        let encoded = serde_json::to_value(&artifact).expect("serialize");
        assert_eq!(encoded["ingest_metadata"]["device_id"], "cam-1");
        assert_eq!(encoded["ingest_metadata"]["provenance"], "media");
        assert_eq!(encoded["source_image_path"], "snapshots/cam-1/123.jpg");
        assert_eq!(encoded["caption"], "Front Door AI snapshot");
        assert_eq!(encoded["derived_text"], "检测到 person; labels: person");
        assert_eq!(encoded["tags"], json!(["camera", "snapshot"]));
        assert_eq!(encoded["labels"], json!(["person"]));
        assert_eq!(encoded["index_sidecar_path"], "snapshots/cam-1/123.json");
        assert_eq!(
            encoded["ingest_metadata"]["ingest_disposition"],
            "knowledge_index_candidate"
        );

        let decoded: VisionImageArtifact = serde_json::from_value(encoded).expect("decode");
        assert_eq!(
            decoded.ingest_metadata.expect("metadata").room.as_deref(),
            Some("Entry")
        );
    }
}
