//! Device discovery over LAN protocols such as ONVIF, mDNS, and SSDP.

use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::adapters::mdns::MdnsDiscoveryAdapter;
use crate::adapters::onvif::OnvifDiscoveryAdapter;
use crate::adapters::rtsp::RtspProbeAdapter;
use crate::adapters::ssdp::SsdpDiscoveryAdapter;
use crate::runtime::media::{
    SnapshotCaptureRequest, SnapshotCaptureResult, StreamOpenRequest, StreamOpenResult,
};
use crate::runtime::registry::{
    CameraCapabilities, CameraDevice, DeviceRegistrySnapshot, DeviceStatus, StreamTransport,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryProtocol {
    Onvif,
    Mdns,
    Ssdp,
    Matter,
    RtspProbe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryCandidateStatus {
    Discovered,
    Validated,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveryRequest {
    pub scan_id: String,
    pub network_cidr: String,
    #[serde(default)]
    pub protocols: Vec<DiscoveryProtocol>,
    #[serde(default)]
    pub include_rtsp_probe: bool,
    #[serde(default)]
    pub rtsp_port: Option<u16>,
    #[serde(default)]
    pub rtsp_username: Option<String>,
    #[serde(default)]
    pub rtsp_password: Option<String>,
    #[serde(default)]
    pub rtsp_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveryCandidate {
    pub candidate_id: String,
    pub protocol: DiscoveryProtocol,
    pub name: Option<String>,
    pub ip_address: String,
    pub port: Option<u16>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub rtsp_paths: Vec<String>,
    pub status: DiscoveryCandidateStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RtspProbeRequest {
    pub candidate_id: String,
    pub ip_address: String,
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub path_candidates: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RtspProbeResult {
    pub candidate_id: String,
    pub reachable: bool,
    pub stream_url: Option<String>,
    pub transport: StreamTransport,
    #[serde(default)]
    pub requires_auth: bool,
    #[serde(default)]
    pub capabilities: CameraCapabilities,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveryBatchResult {
    pub scan_id: String,
    #[serde(default)]
    pub candidates: Vec<DiscoveryCandidate>,
    #[serde(default)]
    pub connected_devices: Vec<CameraDevice>,
    #[serde(default)]
    pub probe_results: Vec<RtspProbeResult>,
}

pub struct DiscoveryService {
    onvif: Option<Box<dyn OnvifDiscoveryAdapter>>,
    ssdp: Option<Box<dyn SsdpDiscoveryAdapter>>,
    mdns: Option<Box<dyn MdnsDiscoveryAdapter>>,
    rtsp: Box<dyn RtspProbeAdapter>,
}

impl DiscoveryService {
    const MAX_RTSP_SEED_HOSTS: u32 = 1_024;

    pub fn new(
        rtsp: Box<dyn RtspProbeAdapter>,
        onvif: Option<Box<dyn OnvifDiscoveryAdapter>>,
        ssdp: Option<Box<dyn SsdpDiscoveryAdapter>>,
        mdns: Option<Box<dyn MdnsDiscoveryAdapter>>,
    ) -> Self {
        Self {
            onvif,
            ssdp,
            mdns,
            rtsp,
        }
    }

    pub fn discover(&self, request: &DiscoveryRequest) -> Result<DiscoveryBatchResult, String> {
        let mut candidates = Vec::new();

        for protocol in &request.protocols {
            match protocol {
                DiscoveryProtocol::Onvif => {
                    if let Some(adapter) = &self.onvif {
                        candidates.extend(adapter.discover(request)?);
                    }
                }
                DiscoveryProtocol::Ssdp => {
                    if let Some(adapter) = &self.ssdp {
                        candidates.extend(adapter.discover(request)?);
                    }
                }
                DiscoveryProtocol::Mdns => {
                    if let Some(adapter) = &self.mdns {
                        candidates.extend(adapter.discover(request)?);
                    }
                }
                DiscoveryProtocol::Matter | DiscoveryProtocol::RtspProbe => {}
            }
        }

        if request.include_rtsp_probe && should_seed_rtsp_candidates(request, &candidates) {
            candidates.extend(self.seed_rtsp_candidates(request)?);
        }

        let mut connected_snapshot = DeviceRegistrySnapshot::default();
        let mut probe_results = Vec::new();
        if request.include_rtsp_probe {
            for candidate in &candidates {
                let probe_request = RtspProbeRequest {
                    candidate_id: candidate.candidate_id.clone(),
                    ip_address: candidate.ip_address.clone(),
                    port: candidate.port.or(request.rtsp_port).unwrap_or(554),
                    username: request.rtsp_username.clone(),
                    password: request.rtsp_password.clone(),
                    path_candidates: preferred_rtsp_paths(request, candidate),
                };
                let result = self.rtsp.probe(&probe_request)?;
                probe_results.push(result.clone());
                if can_promote_probe_result(&result, request) {
                    if let Some(device) = result
                        .into_camera_device(candidate, format!("cam-{}", candidate.candidate_id))
                    {
                        connected_snapshot
                            .upsert_camera_devices_preserving_platform_records(&[device]);
                    }
                }
            }
        }

        let connected_devices = connected_snapshot.to_camera_devices();

        Ok(DiscoveryBatchResult {
            scan_id: request.scan_id.clone(),
            candidates,
            connected_devices,
            probe_results,
        })
    }

    pub fn capture_snapshot(
        &self,
        request: &SnapshotCaptureRequest,
    ) -> Result<SnapshotCaptureResult, String> {
        self.rtsp.capture_snapshot(request)
    }

    pub fn open_stream(&self, request: &StreamOpenRequest) -> Result<StreamOpenResult, String> {
        self.rtsp.open_stream(request)
    }

    fn seed_rtsp_candidates(
        &self,
        request: &DiscoveryRequest,
    ) -> Result<Vec<DiscoveryCandidate>, String> {
        let (network, prefix) = parse_ipv4_cidr(&request.network_cidr)?;
        let host_count = host_count_for_prefix(prefix);
        if host_count > Self::MAX_RTSP_SEED_HOSTS {
            return Err(format!(
                "network {} is too large for RTSP fallback scan ({} hosts > max {})",
                request.network_cidr,
                host_count,
                Self::MAX_RTSP_SEED_HOSTS
            ));
        }

        let base = u32::from(network);
        let start = if prefix >= 31 {
            base
        } else {
            base.saturating_add(1)
        };
        let end = if prefix >= 31 {
            base.saturating_add(host_count)
        } else {
            base.saturating_add(host_count + 1)
        };

        let mut candidates = Vec::with_capacity(host_count as usize);
        for host in start..end {
            let ip = Ipv4Addr::from(host).to_string();
            candidates.push(DiscoveryCandidate {
                candidate_id: format!("rtsp-{}", ip.replace('.', "-")),
                protocol: DiscoveryProtocol::RtspProbe,
                name: None,
                ip_address: ip,
                port: request.rtsp_port.or(Some(554)),
                vendor: None,
                model: None,
                rtsp_paths: if request.rtsp_paths.is_empty() {
                    default_rtsp_paths()
                } else {
                    request.rtsp_paths.clone()
                },
                status: DiscoveryCandidateStatus::Discovered,
            });
        }

        Ok(candidates)
    }
}

fn preferred_rtsp_paths(request: &DiscoveryRequest, candidate: &DiscoveryCandidate) -> Vec<String> {
    let mut paths = Vec::new();
    extend_rtsp_paths(&mut paths, &candidate.rtsp_paths);
    extend_rtsp_paths(&mut paths, &request.rtsp_paths);

    if let Some(vendor_paths) =
        vendor_rtsp_paths(candidate.vendor.as_deref(), candidate.model.as_deref())
    {
        extend_rtsp_paths(&mut paths, &vendor_paths);
    }

    extend_rtsp_paths(&mut paths, &default_rtsp_paths());
    paths
}

fn vendor_rtsp_paths(vendor: Option<&str>, model: Option<&str>) -> Option<Vec<String>> {
    if is_tp_link_tapo_vendor(vendor, model) {
        Some(vec!["/stream1".to_string(), "/stream2".to_string()])
    } else {
        None
    }
}

fn is_tp_link_tapo_vendor(vendor: Option<&str>, model: Option<&str>) -> bool {
    let vendor = vendor.unwrap_or_default().to_ascii_lowercase();
    let model = model.unwrap_or_default().to_ascii_lowercase();
    vendor.contains("tapo")
        || vendor.contains("tp-link")
        || vendor.contains("tplink")
        || model.contains("tapo")
}

fn extend_rtsp_paths(paths: &mut Vec<String>, candidates: &[String]) {
    for path in candidates {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = if trimmed.starts_with('/') {
            trimmed.to_string()
        } else {
            format!("/{trimmed}")
        };
        if !paths.iter().any(|existing| existing == &normalized) {
            paths.push(normalized);
        }
    }
}

fn should_seed_rtsp_candidates(
    request: &DiscoveryRequest,
    candidates: &[DiscoveryCandidate],
) -> bool {
    candidates.is_empty() && request.protocols.contains(&DiscoveryProtocol::RtspProbe)
}

fn can_promote_probe_result(result: &RtspProbeResult, request: &DiscoveryRequest) -> bool {
    result.reachable
        && (!result.requires_auth
            || request
                .rtsp_password
                .as_deref()
                .is_some_and(|password| !password.trim().is_empty()))
}

pub fn default_rtsp_paths() -> Vec<String> {
    vec![
        "/ch01_sub.264".to_string(),
        "/ch01.264".to_string(),
        "/stream1".to_string(),
        "/stream2".to_string(),
        "/h264/ch1/main/av_stream".to_string(),
        "/ch1/main".to_string(),
        "/Streaming/Channels/101".to_string(),
        "/live".to_string(),
        "/h264/ch1/sub/av_stream".to_string(),
        "/ch1/sub".to_string(),
        "/Streaming/Channels/102".to_string(),
    ]
}

fn parse_ipv4_cidr(value: &str) -> Result<(Ipv4Addr, u8), String> {
    let (ip, prefix) = value
        .split_once('/')
        .ok_or_else(|| format!("invalid CIDR, expected a.b.c.d/prefix: {value}"))?;
    let prefix: u8 = prefix
        .parse()
        .map_err(|e| format!("invalid CIDR prefix in {value}: {e}"))?;
    if prefix > 32 {
        return Err(format!("CIDR prefix must be <= 32: {value}"));
    }

    let ip: Ipv4Addr = ip
        .parse()
        .map_err(|e| format!("invalid IPv4 address in CIDR {value}: {e}"))?;
    let ip_u32 = u32::from(ip);
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    Ok((Ipv4Addr::from(ip_u32 & mask), prefix))
}

fn host_count_for_prefix(prefix: u8) -> u32 {
    match prefix {
        32 => 1,
        31 => 2,
        _ => (1u32 << (32 - prefix)) - 2,
    }
}

impl RtspProbeResult {
    pub fn into_camera_device(
        self,
        candidate: &DiscoveryCandidate,
        device_id: impl Into<String>,
    ) -> Option<CameraDevice> {
        let stream_url = self.stream_url?;
        let mut device = CameraDevice::new(
            device_id.into(),
            candidate
                .name
                .clone()
                .unwrap_or_else(|| format!("camera-{}", candidate.ip_address)),
            stream_url,
        );
        device.status = if self.reachable {
            DeviceStatus::Online
        } else {
            DeviceStatus::Unknown
        };
        device.vendor = candidate.vendor.clone();
        device.model = candidate.model.clone();
        device.ip_address = Some(candidate.ip_address.clone());
        device.discovery_source = protocol_name(candidate.protocol).to_string();
        device.primary_stream.transport = self.transport;
        device.primary_stream.requires_auth = self.requires_auth;
        device.capabilities = self.capabilities;
        if matches!(candidate.protocol, DiscoveryProtocol::Onvif) {
            device.capabilities.ptz = true;
            device.onvif_device_service_url = Some(format!(
                "http://{}/onvif/device_service",
                candidate.ip_address
            ));
        }
        Some(device)
    }
}

fn protocol_name(protocol: DiscoveryProtocol) -> &'static str {
    match protocol {
        DiscoveryProtocol::Onvif => "onvif",
        DiscoveryProtocol::Mdns => "mdns",
        DiscoveryProtocol::Ssdp => "ssdp",
        DiscoveryProtocol::Matter => "matter",
        DiscoveryProtocol::RtspProbe => "rtsp_probe",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{
        preferred_rtsp_paths, DiscoveryBatchResult, DiscoveryCandidate, DiscoveryCandidateStatus,
        DiscoveryProtocol, DiscoveryRequest, DiscoveryService, RtspProbeRequest, RtspProbeResult,
    };
    use crate::adapters::mdns::MdnsDiscoveryAdapter;
    use crate::adapters::onvif::OnvifDiscoveryAdapter;
    use crate::adapters::rtsp::RtspProbeAdapter;
    use crate::adapters::ssdp::SsdpDiscoveryAdapter;
    use crate::connectors::storage::StorageTarget;
    use crate::runtime::media::{
        SnapshotCaptureRequest, SnapshotCaptureResult, SnapshotFormat, StreamOpenRequest,
        StreamOpenResult,
    };
    use crate::runtime::registry::StreamTransport;

    struct StaticOnvifAdapter;
    struct PathlessOnvifAdapter;
    struct EmptySsdpAdapter;
    struct EmptyMdnsAdapter;
    struct StaticRtspAdapter;
    struct RecordingRtspAdapter {
        seen_requests: Arc<Mutex<Vec<RtspProbeRequest>>>,
    }

    impl OnvifDiscoveryAdapter for StaticOnvifAdapter {
        fn discover(&self, _request: &DiscoveryRequest) -> Result<Vec<DiscoveryCandidate>, String> {
            Ok(vec![DiscoveryCandidate {
                candidate_id: "cand-1".to_string(),
                protocol: DiscoveryProtocol::Onvif,
                name: Some("Gate Cam".to_string()),
                ip_address: "192.168.1.20".to_string(),
                port: Some(554),
                vendor: Some("Demo".to_string()),
                model: Some("X1".to_string()),
                rtsp_paths: vec!["/live".to_string()],
                status: DiscoveryCandidateStatus::Discovered,
            }])
        }
    }

    impl OnvifDiscoveryAdapter for PathlessOnvifAdapter {
        fn discover(&self, _request: &DiscoveryRequest) -> Result<Vec<DiscoveryCandidate>, String> {
            Ok(vec![DiscoveryCandidate {
                candidate_id: "cand-pathless".to_string(),
                protocol: DiscoveryProtocol::Onvif,
                name: Some("Yard Cam".to_string()),
                ip_address: "192.168.1.30".to_string(),
                port: Some(554),
                vendor: Some("Demo".to_string()),
                model: Some("Y1".to_string()),
                rtsp_paths: vec![],
                status: DiscoveryCandidateStatus::Discovered,
            }])
        }
    }

    impl SsdpDiscoveryAdapter for EmptySsdpAdapter {
        fn discover(&self, _request: &DiscoveryRequest) -> Result<Vec<DiscoveryCandidate>, String> {
            Ok(vec![])
        }
    }

    impl MdnsDiscoveryAdapter for EmptyMdnsAdapter {
        fn discover(&self, _request: &DiscoveryRequest) -> Result<Vec<DiscoveryCandidate>, String> {
            Ok(vec![])
        }
    }

    impl RtspProbeAdapter for StaticRtspAdapter {
        fn probe(&self, request: &RtspProbeRequest) -> Result<RtspProbeResult, String> {
            Ok(RtspProbeResult {
                candidate_id: request.candidate_id.clone(),
                reachable: true,
                stream_url: Some(format!("rtsp://{}{}", request.ip_address, "/live")),
                transport: StreamTransport::Rtsp,
                requires_auth: request.username.is_some(),
                capabilities: Default::default(),
                error_message: None,
            })
        }

        fn capture_snapshot(
            &self,
            request: &SnapshotCaptureRequest,
        ) -> Result<SnapshotCaptureResult, String> {
            Ok(SnapshotCaptureResult::new(
                request.device_id.clone(),
                request.format,
                "ZmFrZS1qcGVn",
                9,
                request.storage_target,
            ))
        }

        fn open_stream(&self, request: &StreamOpenRequest) -> Result<StreamOpenResult, String> {
            Ok(StreamOpenResult::new(
                request.device_id.clone(),
                request.stream_url.clone(),
                "ffplay",
                "/usr/bin/ffplay".into(),
                4242,
            ))
        }
    }

    impl RtspProbeAdapter for RecordingRtspAdapter {
        fn probe(&self, request: &RtspProbeRequest) -> Result<RtspProbeResult, String> {
            self.seen_requests
                .lock()
                .expect("record probe requests")
                .push(request.clone());
            let suffix = request
                .path_candidates
                .first()
                .cloned()
                .unwrap_or_else(|| "/".to_string());
            Ok(RtspProbeResult {
                candidate_id: request.candidate_id.clone(),
                reachable: true,
                stream_url: Some(format!("rtsp://{}{}", request.ip_address, suffix)),
                transport: StreamTransport::Rtsp,
                requires_auth: request.username.is_some(),
                capabilities: Default::default(),
                error_message: None,
            })
        }

        fn capture_snapshot(
            &self,
            request: &SnapshotCaptureRequest,
        ) -> Result<SnapshotCaptureResult, String> {
            Ok(SnapshotCaptureResult::new(
                request.device_id.clone(),
                request.format,
                "ZmFrZS1qcGVn",
                9,
                request.storage_target,
            ))
        }

        fn open_stream(&self, request: &StreamOpenRequest) -> Result<StreamOpenResult, String> {
            Ok(StreamOpenResult::new(
                request.device_id.clone(),
                request.stream_url.clone(),
                "ffplay",
                "/usr/bin/ffplay".into(),
                4242,
            ))
        }
    }

    #[test]
    fn probe_result_can_become_camera_device() {
        let candidate = DiscoveryCandidate {
            candidate_id: "cand-1".to_string(),
            protocol: DiscoveryProtocol::Onvif,
            name: Some("Gate Cam".to_string()),
            ip_address: "192.168.1.20".to_string(),
            port: Some(554),
            vendor: Some("Demo".to_string()),
            model: Some("X1".to_string()),
            rtsp_paths: vec!["/live".to_string()],
            status: DiscoveryCandidateStatus::Validated,
        };
        let probe = RtspProbeResult {
            candidate_id: "cand-1".to_string(),
            reachable: true,
            stream_url: Some("rtsp://192.168.1.20/live".to_string()),
            transport: StreamTransport::Rtsp,
            requires_auth: false,
            capabilities: Default::default(),
            error_message: None,
        };

        let device = probe
            .into_camera_device(&candidate, "cam-1")
            .expect("device");
        assert_eq!(device.device_id, "cam-1");
        assert_eq!(device.ip_address.as_deref(), Some("192.168.1.20"));
        assert_eq!(device.discovery_source, "onvif");
    }

    #[test]
    fn discovery_service_promotes_candidates_to_devices() {
        let service = DiscoveryService::new(
            Box::new(StaticRtspAdapter),
            Some(Box::new(StaticOnvifAdapter)),
            Some(Box::new(EmptySsdpAdapter)),
            Some(Box::new(EmptyMdnsAdapter)),
        );
        let request = DiscoveryRequest {
            scan_id: "scan-1".to_string(),
            network_cidr: "192.168.1.0/24".to_string(),
            protocols: vec![
                DiscoveryProtocol::Onvif,
                DiscoveryProtocol::Ssdp,
                DiscoveryProtocol::Mdns,
            ],
            include_rtsp_probe: true,
            rtsp_port: None,
            rtsp_username: None,
            rtsp_password: None,
            rtsp_paths: vec![],
        };

        let result: DiscoveryBatchResult = service.discover(&request).expect("discovery result");
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.connected_devices.len(), 1);
        assert_eq!(
            result.connected_devices[0].primary_stream.url,
            "rtsp://192.168.1.20/live"
        );
    }

    #[test]
    fn discovery_service_delegates_snapshot_capture() {
        let service = DiscoveryService::new(Box::new(StaticRtspAdapter), None, None, None);
        let result = service
            .capture_snapshot(&SnapshotCaptureRequest::new(
                "cam-1",
                "rtsp://192.168.1.20/live",
                SnapshotFormat::Jpeg,
                StorageTarget::LocalDisk,
            ))
            .expect("snapshot result");

        assert_eq!(result.device_id, "cam-1");
        assert_eq!(result.mime_type, "image/jpeg");
        assert!(result.storage.relative_path.ends_with(".jpg"));
    }

    #[test]
    fn discovery_service_delegates_stream_open() {
        let service = DiscoveryService::new(Box::new(StaticRtspAdapter), None, None, None);
        let result = service
            .open_stream(&StreamOpenRequest::new(
                "cam-1",
                "rtsp://192.168.1.20/live",
                Some("ffplay".to_string()),
            ))
            .expect("stream open result");

        assert_eq!(result.device_id, "cam-1");
        assert_eq!(result.player, "ffplay");
        assert_eq!(result.process_id, 4242);
    }

    #[test]
    fn probe_result_preserves_device_media_and_control_metadata() {
        let candidate = DiscoveryCandidate {
            candidate_id: "cand-onvif".to_string(),
            protocol: DiscoveryProtocol::Onvif,
            name: Some("Front Door".to_string()),
            ip_address: "192.168.1.20".to_string(),
            port: Some(554),
            vendor: Some("Demo".to_string()),
            model: Some("X1".to_string()),
            rtsp_paths: vec!["/live".to_string()],
            status: DiscoveryCandidateStatus::Validated,
        };
        let probe = RtspProbeResult {
            candidate_id: "cand-onvif".to_string(),
            reachable: true,
            stream_url: Some("rtsp://192.168.1.20/live".to_string()),
            transport: StreamTransport::Rtsp,
            requires_auth: true,
            capabilities: crate::runtime::registry::CameraCapabilities {
                snapshot: true,
                stream: true,
                ptz: true,
                audio: true,
            },
            error_message: None,
        };

        let device = probe
            .into_camera_device(&candidate, "cam-1")
            .expect("camera device");

        assert_eq!(device.discovery_source, "onvif");
        assert_eq!(device.primary_stream.url, "rtsp://192.168.1.20/live");
        assert!(device.primary_stream.requires_auth);
        assert!(device.capabilities.snapshot);
        assert!(device.capabilities.stream);
        assert!(device.capabilities.ptz);
        assert!(device.capabilities.audio);
        assert_eq!(
            device.onvif_device_service_url.as_deref(),
            Some("http://192.168.1.20/onvif/device_service")
        );
    }

    #[test]
    fn discovery_service_can_seed_rtsp_scan_from_cidr() {
        let service = DiscoveryService::new(Box::new(StaticRtspAdapter), None, None, None);
        let request = DiscoveryRequest {
            scan_id: "scan-rtsp".to_string(),
            network_cidr: "192.168.3.72/30".to_string(),
            protocols: vec![DiscoveryProtocol::RtspProbe],
            include_rtsp_probe: true,
            rtsp_port: Some(554),
            rtsp_username: Some("admin".to_string()),
            rtsp_password: Some("secret".to_string()),
            rtsp_paths: vec!["/ch1/main".to_string()],
        };

        let result = service.discover(&request).expect("discovery result");

        assert_eq!(result.candidates.len(), 2);
        assert_eq!(result.connected_devices.len(), 2);
        assert_eq!(result.candidates[0].ip_address, "192.168.3.73");
        assert_eq!(result.candidates[1].ip_address, "192.168.3.74");
        assert_eq!(
            result.candidates[0].rtsp_paths,
            vec!["/ch1/main".to_string()]
        );
        assert_eq!(
            result.connected_devices[0].primary_stream.url,
            "rtsp://192.168.3.73/live"
        );
    }

    #[test]
    fn discovery_service_falls_back_to_rtsp_scan_when_onvif_finds_nothing() {
        let seen_requests = Arc::new(Mutex::new(Vec::new()));
        let service = DiscoveryService::new(
            Box::new(RecordingRtspAdapter {
                seen_requests: Arc::clone(&seen_requests),
            }),
            Some(Box::new(EmptyOnvifAdapter)),
            None,
            None,
        );
        let request = DiscoveryRequest {
            scan_id: "scan-onvif-empty".to_string(),
            network_cidr: "192.168.3.0/24".to_string(),
            protocols: vec![DiscoveryProtocol::Onvif, DiscoveryProtocol::RtspProbe],
            include_rtsp_probe: true,
            rtsp_port: Some(554),
            rtsp_username: Some("admin".to_string()),
            rtsp_password: None,
            rtsp_paths: vec!["/ch1/main".to_string()],
        };

        let result = service.discover(&request).expect("discovery result");
        let seen_requests = seen_requests.lock().expect("read probe requests");

        assert_eq!(result.candidates.len(), 254);
        assert_eq!(result.connected_devices.len(), 0);
        assert_eq!(seen_requests.len(), 254);
    }

    #[test]
    fn discovery_service_uses_request_rtsp_paths_for_discovered_candidates_without_paths() {
        let seen_requests = Arc::new(Mutex::new(Vec::new()));
        let service = DiscoveryService::new(
            Box::new(RecordingRtspAdapter {
                seen_requests: Arc::clone(&seen_requests),
            }),
            Some(Box::new(PathlessOnvifAdapter)),
            None,
            None,
        );
        let request = DiscoveryRequest {
            scan_id: "scan-onvif".to_string(),
            network_cidr: "192.168.1.0/24".to_string(),
            protocols: vec![DiscoveryProtocol::Onvif],
            include_rtsp_probe: true,
            rtsp_port: Some(554),
            rtsp_username: Some("admin".to_string()),
            rtsp_password: None,
            rtsp_paths: vec!["/ch1/main".to_string()],
        };

        let result = service.discover(&request).expect("discovery result");
        let seen_requests = seen_requests.lock().expect("read probe requests");

        assert_eq!(seen_requests.len(), 1);
        assert_eq!(seen_requests[0].path_candidates[0], "/ch1/main");
        assert!(seen_requests[0]
            .path_candidates
            .contains(&"/stream1".to_string()));
        assert!(result.connected_devices.is_empty());
    }

    #[test]
    fn discovery_service_rejects_overly_large_rtsp_seed_scan() {
        let service = DiscoveryService::new(Box::new(StaticRtspAdapter), None, None, None);
        let request = DiscoveryRequest {
            scan_id: "scan-rtsp".to_string(),
            network_cidr: "10.0.0.0/16".to_string(),
            protocols: vec![DiscoveryProtocol::RtspProbe],
            include_rtsp_probe: true,
            rtsp_port: None,
            rtsp_username: None,
            rtsp_password: None,
            rtsp_paths: vec![],
        };

        let error = service
            .discover(&request)
            .expect_err("large scan should be rejected");
        assert!(error.contains("too large"));
    }

    #[test]
    fn preferred_rtsp_paths_prioritize_tapo_vendor_presets() {
        let request = DiscoveryRequest {
            scan_id: "scan-tapo".to_string(),
            network_cidr: "192.168.1.0/24".to_string(),
            protocols: vec![DiscoveryProtocol::RtspProbe],
            include_rtsp_probe: true,
            rtsp_port: Some(554),
            rtsp_username: None,
            rtsp_password: None,
            rtsp_paths: vec![],
        };
        let candidate = DiscoveryCandidate {
            candidate_id: "cand-tapo".to_string(),
            protocol: DiscoveryProtocol::RtspProbe,
            name: Some("Porch Cam".to_string()),
            ip_address: "192.168.1.88".to_string(),
            port: Some(554),
            vendor: Some("TP-Link Tapo".to_string()),
            model: Some("C200".to_string()),
            rtsp_paths: vec![],
            status: DiscoveryCandidateStatus::Discovered,
        };

        let paths = preferred_rtsp_paths(&request, &candidate);

        assert_eq!(paths[0], "/stream1");
        assert_eq!(paths[1], "/stream2");
        assert!(paths.contains(&"/h264/ch1/main/av_stream".to_string()));
        assert!(paths.contains(&"/ch01_sub.264".to_string()));
        assert!(paths.contains(&"/ch01.264".to_string()));
    }

    struct EmptyOnvifAdapter;

    impl OnvifDiscoveryAdapter for EmptyOnvifAdapter {
        fn discover(&self, _request: &DiscoveryRequest) -> Result<Vec<DiscoveryCandidate>, String> {
            Ok(vec![])
        }
    }
}
