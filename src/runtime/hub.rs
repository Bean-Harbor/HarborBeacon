//! Shared Agent Hub application services for onboarding, discovery, and registry updates.

#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::net::Ipv4Addr;
#[cfg(test)]
use std::net::{SocketAddrV4, TcpStream};
#[cfg(test)]
use std::thread;
#[cfg(test)]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[cfg(test)]
use crate::adapters::mdns::AvahiMdnsAdapter;
#[cfg(test)]
use crate::adapters::onvif::WsDiscoveryOnvifAdapter;
#[cfg(test)]
use crate::adapters::rtsp::{CommandRtspAdapter, RtspProbeAdapter};
#[cfg(test)]
use crate::adapters::ssdp::UdpSsdpAdapter;
use crate::connectors::harborlink_media::HarborLinkMediaClient;
use crate::connectors::im_gateway::{GatewayPlatformStatus, GatewayStatusClient};
use crate::connectors::storage::StorageTarget;
use crate::runtime::admin_console::{
    delivery_policy_summary, harboros_current_user_display_name, harboros_current_user_id,
    harboros_writable_root, sanitize_defaults, AdminBindingState, AdminConsoleState,
    AdminConsoleStore, AdminDefaults, BridgeProviderCapabilities, BridgeProviderConfig,
    DeliveryPolicySummary,
};
use crate::runtime::discovery::{default_rtsp_paths, DiscoveryProtocol};
#[cfg(test)]
use crate::runtime::discovery::{DiscoveryRequest, DiscoveryService, RtspProbeRequest};
use crate::runtime::dvr::DvrRecordingSettings;
use crate::runtime::media::{SnapshotCaptureResult, SnapshotFormat};
use crate::runtime::registry::{CameraDevice, DeviceRegistryStore, DeviceStatus, StreamTransport};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HubStateSnapshot {
    pub binding: AdminBindingState,
    pub defaults: AdminDefaults,
    pub bridge_provider: BridgeProviderConfig,
    pub dvr: DvrRecordingSettings,
    pub delivery_policy: DeliveryPolicySummary,
    pub writable_root: String,
    pub current_principal_user_id: String,
    pub current_principal_display_name: String,
    pub devices: Vec<CameraDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct HubScanRequest {
    #[serde(default)]
    pub cidr: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub rtsp_port: Option<u16>,
    #[serde(default)]
    pub rtsp_username: Option<String>,
    #[serde(default)]
    pub rtsp_password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HubScanResultItem {
    pub candidate_id: String,
    pub device_id: Option<String>,
    pub name: String,
    pub room: String,
    pub ip: String,
    pub port: u16,
    pub protocol: String,
    pub note: String,
    pub reachable: bool,
    pub registered: bool,
    #[serde(default)]
    pub requires_auth: bool,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub rtsp_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HubScanSummary {
    pub binding: AdminBindingState,
    pub defaults: AdminDefaults,
    pub devices: Vec<CameraDevice>,
    pub results: Vec<HubScanResultItem>,
    pub scanned_hosts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CameraConnectRequest {
    pub name: String,
    #[serde(default)]
    pub room: Option<String>,
    pub ip: String,
    #[serde(default)]
    pub path_candidates: Vec<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub snapshot_url: Option<String>,
    #[serde(default)]
    pub discovery_source: String,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HubManualAddSummary {
    pub binding: AdminBindingState,
    pub defaults: AdminDefaults,
    pub device: CameraDevice,
    pub devices: Vec<CameraDevice>,
    pub note: String,
}

#[derive(Debug, Clone)]
pub struct CameraHubService {
    admin_store: AdminConsoleStore,
}

#[cfg(test)]
#[derive(Debug, Clone, Default)]
struct RtspScanCredentials {
    username: Option<String>,
    password: Option<String>,
}

impl CameraHubService {
    pub fn new(admin_store: AdminConsoleStore) -> Self {
        Self { admin_store }
    }

    pub fn admin_store(&self) -> &AdminConsoleStore {
        &self.admin_store
    }

    pub fn load_admin_state(&self) -> Result<AdminConsoleState, String> {
        self.admin_store.load_or_create_state()
    }

    pub fn load_registered_cameras(&self) -> Result<Vec<CameraDevice>, String> {
        self.admin_store.registry_store().load_devices()
    }

    pub fn state_snapshot(&self, public_origin: Option<&str>) -> Result<HubStateSnapshot, String> {
        let state = self.load_admin_state()?;
        let devices = self.load_registered_cameras()?;
        Ok(HubStateSnapshot {
            binding: enrich_binding_urls(state.binding, public_origin),
            defaults: state.defaults,
            bridge_provider: state.bridge_provider,
            dvr: state.dvr,
            delivery_policy: delivery_policy_summary(),
            writable_root: harboros_writable_root(),
            current_principal_user_id: harboros_current_user_id(),
            current_principal_display_name: harboros_current_user_display_name(),
            devices,
        })
    }

    pub fn save_defaults(
        &self,
        defaults: AdminDefaults,
        public_origin: Option<&str>,
    ) -> Result<HubStateSnapshot, String> {
        self.admin_store.save_defaults(defaults)?;
        self.state_snapshot(public_origin)
    }

    pub fn refresh_bridge_provider_status(
        &self,
        public_origin: Option<&str>,
    ) -> Result<HubStateSnapshot, String> {
        let client = GatewayStatusClient::new()?;
        let status = client.fetch_status()?;
        let provider = bridge_provider_status_from_gateway_response(
            client.config().base_url.as_str(),
            &status.platforms,
        );
        self.admin_store.save_bridge_provider_status(provider)?;
        self.state_snapshot(public_origin)
    }

    pub fn scan(
        &self,
        request: HubScanRequest,
        public_origin: Option<&str>,
    ) -> Result<HubScanSummary, String> {
        let HubScanRequest {
            cidr,
            protocol,
            rtsp_port,
            rtsp_username,
            rtsp_password,
        } = request;
        let mut defaults = self.load_admin_state()?.defaults;
        if let Some(cidr) = cidr {
            let trimmed = cidr.trim();
            if !trimmed.is_empty() {
                defaults.cidr = trimmed.to_string();
            }
        }
        if let Some(protocol) = protocol {
            let trimmed = protocol.trim();
            if !trimmed.is_empty() {
                defaults.discovery = trimmed.to_string();
            }
        }
        let requested_rtsp_port = rtsp_port.filter(|port| *port > 0);
        defaults = sanitize_defaults(defaults);
        defaults.rtsp_username.clear();
        defaults.rtsp_password.clear();
        defaults.rtsp_port = 554;
        defaults.rtsp_paths.clear();
        let state = self.admin_store.save_defaults(defaults)?;
        let client = HarborLinkMediaClient::from_env()?;
        let rtsp_username = rtsp_username.as_deref().and_then(non_empty_opt);
        let rtsp_password = rtsp_password.as_deref().and_then(non_empty_opt);
        client.save_discovery_settings(
            &state.defaults.cidr,
            &state.defaults.discovery,
            requested_rtsp_port,
            &[],
            rtsp_username.as_deref(),
            rtsp_password.as_deref(),
        )?;
        let discovery = client.discover_cameras(
            &state.defaults.cidr,
            Some(&state.defaults.discovery),
            requested_rtsp_port,
            rtsp_username.as_deref(),
            rtsp_password.as_deref(),
            &[],
        )?;
        let mut registered_devices = self.load_registered_cameras()?;
        let mut results = Vec::new();
        for camera in discovery.cameras {
            let mut device = CameraDevice::new(
                camera.camera_id.clone(),
                camera.display_name.clone(),
                format!("harborlink://camera/{}", camera.camera_id),
            );
            device.status = if camera.reachable {
                DeviceStatus::Online
            } else {
                DeviceStatus::Unknown
            };
            device.vendor = camera.vendor.clone();
            device.model = camera.model.clone();
            device.discovery_source = "harborlink".to_string();
            device.primary_stream.transport = StreamTransport::Webrtc;
            device.primary_stream.requires_auth = camera.requires_auth;
            device.capabilities.stream = true;
            device.capabilities.snapshot = camera.protocol == "onvif";
            device.capabilities.ptz = camera.protocol == "onvif";
            if let Some(existing) = registered_devices
                .iter_mut()
                .find(|existing| existing.device_id == camera.camera_id)
            {
                device.room = existing.room.clone();
                *existing = device;
            } else {
                registered_devices.push(device);
            }
            results.push(HubScanResultItem {
                candidate_id: camera.candidate_id,
                device_id: Some(camera.camera_id),
                name: camera.display_name,
                room: "待确认".to_string(),
                ip: camera.ip_address,
                port: camera.port,
                protocol: format!("HarborLink {}", camera.protocol.to_ascii_uppercase()),
                note: if camera.reachable {
                    "HarborLink 已完成南向发现和 RTSP 验证。".to_string()
                } else {
                    "HarborLink 已发现设备，RTSP 仍需凭据或路径确认。".to_string()
                },
                reachable: camera.reachable,
                registered: camera.registered,
                requires_auth: camera.requires_auth,
                vendor: camera.vendor,
                model: camera.model,
                rtsp_paths: Vec::new(),
            });
        }
        self.admin_store
            .registry_store()
            .save_devices(&registered_devices)?;
        Ok(HubScanSummary {
            binding: enrich_binding_urls(state.binding, public_origin),
            defaults: state.defaults,
            devices: registered_devices,
            results,
            scanned_hosts: discovery.scanned_hosts,
        })
    }

    pub fn manual_add(
        &self,
        request: CameraConnectRequest,
        public_origin: Option<&str>,
    ) -> Result<HubManualAddSummary, String> {
        let state = self.load_admin_state()?;
        let ip = request.ip.trim();
        if ip.is_empty() {
            return Err("IP 地址不能为空".to_string());
        }

        let name = if request.name.trim().is_empty() {
            format!("Camera {ip}")
        } else {
            request.name.trim().to_string()
        };

        let port = request.port.filter(|port| *port > 0).unwrap_or(554);
        let path_candidates = if request.path_candidates.is_empty() {
            vec!["/stream1".to_string(), "/stream2".to_string()]
        } else {
            request.path_candidates.clone()
        };
        let path_candidates = effective_rtsp_path_candidates(
            &path_candidates,
            request.vendor.as_deref(),
            request.model.as_deref(),
        );
        let username = request.username.and_then(|value| non_empty_opt(&value));
        let password = request.password.and_then(|value| non_empty_opt(&value));
        let camera_id = device_id_for_ip(ip);
        let main_stream_url = format!("rtsp://{ip}:{port}{}", path_candidates[0]);
        let sub_stream_url = path_candidates
            .get(1)
            .map(|path| format!("rtsp://{ip}:{port}{path}"));
        let client = HarborLinkMediaClient::from_env()?;
        client.upsert_camera(
            &camera_id,
            &json!({
                "cameraId": camera_id,
                "displayName": name,
                "enabled": true,
                "status": "unknown",
                "room": request.room,
                "vendor": request.vendor,
                "model": request.model,
                "ipAddress": ip,
                "discoverySource": request.discovery_source,
                "streamProfiles": {"main": main_stream_url, "sub": sub_stream_url},
                "snapshotUrl": request.snapshot_url,
                "capabilities": {"snapshot": request.snapshot_url.is_some(), "stream": true, "ptz": false, "audio": false},
            }),
        )?;
        client.save_camera_credential(
            &camera_id,
            username.as_deref(),
            password.as_deref(),
            Some(port),
            Some(&path_candidates),
        )?;
        let probe = client.check_rtsp(&camera_id, None, None, None, None)?;
        if !probe.reachable {
            return Err(probe
                .error_message
                .unwrap_or_else(|| "RTSP 验证失败，未发现可用视频流".to_string()));
        }

        let mut device = CameraDevice::new(
            camera_id,
            name,
            format!("harborlink://camera/{}", device_id_for_ip(ip)),
        );
        device.status = DeviceStatus::Online;
        device.room = request
            .room
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        device.vendor = request.vendor.filter(|value| !value.trim().is_empty());
        device.model = request.model.filter(|value| !value.trim().is_empty());
        device.ip_address = None;
        device.snapshot_url = None;
        device.discovery_source = "harborlink".to_string();
        device.primary_stream.transport = StreamTransport::Webrtc;
        device.primary_stream.requires_auth = probe.requires_auth;
        device.capabilities = probe.capabilities;
        device.capabilities.snapshot =
            request.snapshot_url.is_some() || device.capabilities.snapshot;

        let devices = upsert_devices(self.admin_store.registry_store(), &[device.clone()])?;
        let saved = devices
            .iter()
            .find(|item| item.device_id == device.device_id)
            .cloned()
            .unwrap_or(device);

        Ok(HubManualAddSummary {
            binding: enrich_binding_urls(state.binding, public_origin),
            defaults: state.defaults,
            device: saved,
            devices,
            note: "设备已由 HarborLink 验证并写入南向设备库".to_string(),
        })
    }

    pub fn capture_camera_snapshot_result(
        &self,
        device_id: &str,
    ) -> Result<SnapshotCaptureResult, String> {
        let device = self
            .load_registered_cameras()?
            .into_iter()
            .find(|device| device.device_id == device_id)
            .ok_or_else(|| format!("device not found: {device_id}"))?;

        let bytes = HarborLinkMediaClient::from_env()?.capture_snapshot(&device.device_id)?;
        Ok(SnapshotCaptureResult::new(
            device.device_id,
            SnapshotFormat::Jpeg,
            base64::engine::general_purpose::STANDARD.encode(&bytes),
            bytes.len(),
            StorageTarget::LocalDisk,
        ))
    }

    pub fn capture_camera_snapshot(&self, device_id: &str) -> Result<Vec<u8>, String> {
        let result = self.capture_camera_snapshot_result(device_id)?;

        base64::engine::general_purpose::STANDARD
            .decode(result.bytes_base64.as_bytes())
            .map_err(|error| format!("snapshot bytes decode failed: {error}"))
    }

    #[cfg(test)]
    fn scan_with_rtsp_probe(
        &self,
        state: &AdminConsoleState,
        public_origin: Option<&str>,
        scan_credentials: &RtspScanCredentials,
    ) -> Result<HubScanSummary, String> {
        let existing_devices = self.load_registered_cameras()?;
        let candidate_ips = collect_candidate_ips(
            &state.defaults.cidr,
            &existing_devices,
            state.defaults.rtsp_port,
        )?;
        let adapter = CommandRtspAdapter::default();
        let mut discovered = Vec::new();
        let mut results = Vec::new();

        for ip in &candidate_ips {
            let existing = existing_devices
                .iter()
                .find(|device| device.ip_address.as_deref() == Some(ip.as_str()))
                .cloned();
            let path_candidates = effective_rtsp_path_candidates(
                &state.defaults.rtsp_paths,
                existing
                    .as_ref()
                    .and_then(|device| device.vendor.as_deref()),
                existing.as_ref().and_then(|device| device.model.as_deref()),
            );
            let saved_credential = existing.as_ref().and_then(|device| {
                state
                    .device_credentials
                    .iter()
                    .find(|credential| credential.device_id == device.device_id)
            });
            let username = saved_credential
                .and_then(|credential| non_empty_opt(&credential.username))
                .or_else(|| scan_credentials.username.clone());
            let password = saved_credential
                .and_then(|credential| non_empty_opt(&credential.password))
                .or_else(|| scan_credentials.password.clone());
            let probe_request = RtspProbeRequest {
                candidate_id: format!("rtsp-{}", ip.replace('.', "-")),
                ip_address: ip.clone(),
                port: state.defaults.rtsp_port,
                username,
                password,
                path_candidates: path_candidates.clone(),
            };
            let probe = adapter.probe(&probe_request)?;
            let requires_auth = probe.requires_auth
                || probe
                    .error_message
                    .as_deref()
                    .is_some_and(looks_like_auth_error);
            let has_password = probe_request.password.is_some();
            let can_register =
                probe.reachable && can_register_rtsp_scan_result(requires_auth, has_password);

            if can_register {
                let stream_url = probe
                    .stream_url
                    .clone()
                    .ok_or_else(|| format!("reachable RTSP probe missing stream url for {ip}"))?;
                let mut device = CameraDevice::new(
                    device_id_for_ip(ip),
                    existing
                        .as_ref()
                        .map(|device| device.name.clone())
                        .unwrap_or_else(|| format!("Camera {ip}")),
                    stream_url,
                );
                device.status = DeviceStatus::Online;
                device.room = existing.as_ref().and_then(|device| device.room.clone());
                device.vendor = existing.as_ref().and_then(|device| device.vendor.clone());
                device.model = existing.as_ref().and_then(|device| device.model.clone());
                device.ip_address = Some(ip.clone());
                device.discovery_source = "rtsp_probe".to_string();
                device.primary_stream.transport = StreamTransport::Rtsp;
                device.primary_stream.requires_auth = probe.requires_auth;
                device.capabilities = probe.capabilities;
                discovered.push(device.clone());

                results.push(HubScanResultItem {
                    candidate_id: probe_request.candidate_id.clone(),
                    device_id: Some(device.device_id.clone()),
                    name: device.name.clone(),
                    room: device
                        .room
                        .clone()
                        .unwrap_or_else(|| "未分配房间".to_string()),
                    ip: ip.clone(),
                    port: state.defaults.rtsp_port,
                    protocol: "RTSP / 已验证".to_string(),
                    note: "RTSP 链路已验证，可直接加入设备库并在 IM 对话中调用。".to_string(),
                    reachable: true,
                    registered: true,
                    requires_auth,
                    vendor: device.vendor.clone(),
                    model: device.model.clone(),
                    rtsp_paths: path_candidates.clone(),
                });
            } else {
                let auth_note =
                    "摄像头需要密码。请输入用户名/密码后重新扫描，或用“手动添加”接入。".to_string();
                results.push(HubScanResultItem {
                    candidate_id: probe_request.candidate_id.clone(),
                    device_id: existing.as_ref().map(|device| device.device_id.clone()),
                    name: existing
                        .as_ref()
                        .map(|device| device.name.clone())
                        .unwrap_or_else(|| format!("Camera {ip}")),
                    room: existing
                        .as_ref()
                        .and_then(|device| device.room.clone())
                        .unwrap_or_else(|| "待识别".to_string()),
                    ip: ip.clone(),
                    port: state.defaults.rtsp_port,
                    protocol: if requires_auth {
                        "RTSP / 需密码".to_string()
                    } else {
                        "RTSP / 未通过".to_string()
                    },
                    note: if requires_auth && !has_password {
                        auth_note
                    } else {
                        probe
                            .error_message
                            .clone()
                            .map(|value| humanize_probe_error(&value))
                            .unwrap_or_else(|| "未发现可用视频流".to_string())
                    },
                    reachable: probe.reachable && !requires_auth,
                    registered: existing.is_some(),
                    requires_auth,
                    vendor: existing.as_ref().and_then(|device| device.vendor.clone()),
                    model: existing.as_ref().and_then(|device| device.model.clone()),
                    rtsp_paths: path_candidates,
                });
            }
        }

        let devices = upsert_devices(self.admin_store.registry_store(), &discovered)?;
        Ok(HubScanSummary {
            binding: enrich_binding_urls(state.binding.clone(), public_origin),
            defaults: state.defaults.clone(),
            devices,
            results,
            scanned_hosts: candidate_ips.len(),
        })
    }

    #[cfg(test)]
    fn scan_with_discovery_service(
        &self,
        state: &AdminConsoleState,
        public_origin: Option<&str>,
        protocols: Vec<DiscoveryProtocol>,
        scan_credentials: &RtspScanCredentials,
    ) -> Result<HubScanSummary, String> {
        let service = DiscoveryService::new(
            Box::new(CommandRtspAdapter::default()),
            Some(Box::new(WsDiscoveryOnvifAdapter::default())),
            Some(Box::new(UdpSsdpAdapter::default())),
            Some(Box::new(AvahiMdnsAdapter::default())),
        );
        let discovery = service.discover(&DiscoveryRequest {
            scan_id: "hub-discovery-scan".to_string(),
            network_cidr: state.defaults.cidr.clone(),
            protocols,
            include_rtsp_probe: true,
            rtsp_port: Some(state.defaults.rtsp_port),
            rtsp_username: scan_credentials.username.clone(),
            rtsp_password: scan_credentials.password.clone(),
            rtsp_paths: state.defaults.rtsp_paths.clone(),
        })?;

        let devices = upsert_devices(
            self.admin_store.registry_store(),
            &discovery.connected_devices,
        )?;

        let mut results = Vec::new();
        for candidate in &discovery.candidates {
            let device = devices
                .iter()
                .find(|device| device.ip_address.as_deref() == Some(candidate.ip_address.as_str()));
            let probe = discovery
                .probe_results
                .iter()
                .find(|probe| probe.candidate_id == candidate.candidate_id);
            let reachable = probe.is_some_and(|probe| probe.reachable);
            let requires_auth = probe.is_some_and(|probe| {
                probe.requires_auth
                    || probe
                        .error_message
                        .as_deref()
                        .is_some_and(looks_like_auth_error)
            });
            let registered = device.is_some();
            let verified = reachable && (!requires_auth || registered);
            let port = candidate.port.unwrap_or(state.defaults.rtsp_port);
            let vendor = candidate
                .vendor
                .clone()
                .or_else(|| device.and_then(|item| item.vendor.clone()));
            let model = candidate
                .model
                .clone()
                .or_else(|| device.and_then(|item| item.model.clone()));
            let mut candidate_paths = candidate.rtsp_paths.clone();
            candidate_paths.extend(state.defaults.rtsp_paths.iter().cloned());
            let rtsp_paths = effective_rtsp_path_candidates(
                &candidate_paths,
                vendor.as_deref(),
                model.as_deref(),
            );

            let base = match candidate.protocol {
                DiscoveryProtocol::Onvif => "ONVIF",
                DiscoveryProtocol::Mdns => "mDNS",
                DiscoveryProtocol::Ssdp => "SSDP",
                DiscoveryProtocol::Matter => "Matter",
                DiscoveryProtocol::RtspProbe => "RTSP",
            };

            results.push(HubScanResultItem {
                candidate_id: candidate.candidate_id.clone(),
                device_id: device.map(|device| device.device_id.clone()),
                name: candidate
                    .name
                    .clone()
                    .or_else(|| device.map(|item| item.name.clone()))
                    .unwrap_or_else(|| format!("Camera {}", candidate.ip_address)),
                room: device
                    .and_then(|device| device.room.clone())
                    .unwrap_or_else(|| "待确认".to_string()),
                ip: candidate.ip_address.clone(),
                port,
                protocol: if verified {
                    format!("{base} + RTSP / 已验证")
                } else if requires_auth {
                    format!("{base} / 需密码")
                } else {
                    format!("{base} / 已发现")
                },
                note: if verified {
                    format!("已通过 {base} 发现并完成 RTSP 验证，可直接加入设备库。")
                } else if requires_auth {
                    "已发现摄像头，但需要用户名/密码后才能接入。请在自动发现里填写密码后重新扫描，或用“手动添加”接入。".to_string()
                } else {
                    probe
                        .and_then(|probe| probe.error_message.clone())
                        .map(|value| humanize_probe_error(&value))
                        .unwrap_or_else(|| "已发现 ONVIF 设备，但 RTSP 尚未验证成功；可以继续确认接入。".to_string())
                },
                reachable: verified,
                registered,
                requires_auth,
                vendor,
                model,
                rtsp_paths,
            });
        }

        Ok(HubScanSummary {
            binding: enrich_binding_urls(state.binding.clone(), public_origin),
            defaults: state.defaults.clone(),
            devices,
            results,
            scanned_hosts: discovery.candidates.len(),
        })
    }
}

pub fn build_mobile_setup_url(public_origin: &str, session_code: Option<&str>) -> String {
    let origin = public_origin.trim_end_matches('/');
    match session_code {
        Some(session_code) if !session_code.trim().is_empty() => {
            format!("{origin}/setup/mobile?session={session_code}")
        }
        _ => format!("{origin}/setup/mobile"),
    }
}

pub fn enrich_binding_urls(
    mut binding: AdminBindingState,
    _public_origin: Option<&str>,
) -> AdminBindingState {
    binding.setup_url.clear();
    binding.static_setup_url.clear();
    binding
}

pub fn prefers_onvif_discovery(value: &str) -> bool {
    value.to_lowercase().contains("onvif")
}

pub fn resolve_discovery_protocols(discovery: &str) -> Vec<DiscoveryProtocol> {
    let normalized = discovery.to_lowercase();
    let mut protocols = Vec::new();
    if normalized.contains("onvif") {
        protocols.push(DiscoveryProtocol::Onvif);
    }
    if normalized.contains("ssdp") {
        protocols.push(DiscoveryProtocol::Ssdp);
    }
    if normalized.contains("mdns") || normalized.contains("m-dns") || discovery.contains("mDNS") {
        protocols.push(DiscoveryProtocol::Mdns);
    }
    protocols.push(DiscoveryProtocol::RtspProbe);
    protocols
}

fn effective_rtsp_path_candidates(
    base_paths: &[String],
    vendor: Option<&str>,
    model: Option<&str>,
) -> Vec<String> {
    let mut paths = base_paths.to_vec();
    if is_tp_link_tapo_vendor(vendor, model) {
        paths.push("/stream1".to_string());
        paths.push("/stream2".to_string());
    }
    paths.extend(default_rtsp_paths());
    crate::runtime::admin_console::dedupe_rtsp_paths(paths)
}

fn is_tp_link_tapo_vendor(vendor: Option<&str>, model: Option<&str>) -> bool {
    let vendor = vendor.unwrap_or_default().to_ascii_lowercase();
    let model = model.unwrap_or_default().to_ascii_lowercase();
    vendor.contains("tapo")
        || vendor.contains("tp-link")
        || vendor.contains("tplink")
        || model.contains("tapo")
}

pub fn non_empty_opt(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn device_id_for_ip(ip: &str) -> String {
    format!("cam-rtsp-{}", ip.replace('.', "-"))
}

pub fn looks_like_auth_error(error: &str) -> bool {
    let normalized = error.to_lowercase();
    normalized.contains("401")
        || normalized.contains("unauthorized")
        || normalized.contains("authorization failed")
        || normalized.contains("auth failed")
}

pub fn humanize_probe_error(error: &str) -> String {
    if looks_like_auth_error(error) {
        "RTSP 返回 401，说明摄像头需要密码。".to_string()
    } else {
        error.to_string()
    }
}

pub fn upsert_devices(
    store: &DeviceRegistryStore,
    discovered: &[CameraDevice],
) -> Result<Vec<CameraDevice>, String> {
    let snapshot = store.upsert_devices(discovered)?;
    Ok(snapshot.to_camera_devices())
}

pub fn same_camera(existing: &CameraDevice, incoming: &CameraDevice) -> bool {
    existing.device_id == incoming.device_id
        || (existing.ip_address.is_some()
            && existing.ip_address == incoming.ip_address
            && existing.primary_stream.url == incoming.primary_stream.url)
        || existing.primary_stream.url == incoming.primary_stream.url
}

pub fn merge_camera(existing: CameraDevice, incoming: CameraDevice) -> CameraDevice {
    normalize_camera_metadata(CameraDevice {
        device_id: existing.device_id,
        name: if incoming.name.trim().is_empty() {
            existing.name
        } else {
            incoming.name
        },
        kind: incoming.kind,
        status: incoming.status,
        room: existing.room.or(incoming.room),
        vendor: incoming.vendor.or(existing.vendor),
        model: incoming.model.or(existing.model),
        ip_address: incoming.ip_address.or(existing.ip_address),
        mac_address: incoming.mac_address.or(existing.mac_address),
        discovery_source: if incoming.discovery_source.trim().is_empty() {
            existing.discovery_source
        } else {
            incoming.discovery_source
        },
        primary_stream: incoming.primary_stream,
        snapshot_url: incoming.snapshot_url.or(existing.snapshot_url),
        onvif_device_service_url: incoming
            .onvif_device_service_url
            .or(existing.onvif_device_service_url),
        ezviz_device_serial: incoming
            .ezviz_device_serial
            .or(existing.ezviz_device_serial),
        ezviz_camera_no: incoming.ezviz_camera_no.or(existing.ezviz_camera_no),
        capabilities: incoming.capabilities,
        last_seen_at: incoming.last_seen_at.or(existing.last_seen_at),
    })
}

pub fn normalize_camera_metadata(mut device: CameraDevice) -> CameraDevice {
    device.discovery_source = normalize_discovery_source(&device.discovery_source).to_string();
    if matches!(device.primary_stream.transport, StreamTransport::Unknown) {
        device.primary_stream.transport = StreamTransport::Rtsp;
    }
    device
}

pub fn normalize_discovery_source(value: &str) -> &str {
    match value {
        "rtspprobe" => "rtsp_probe",
        "mdns" => "mdns",
        "ssdp" => "ssdp",
        "onvif" => "onvif",
        "matter" => "matter",
        "rtsp_probe" => "rtsp_probe",
        _ => value,
    }
}

#[cfg(test)]
pub fn collect_candidate_ips(
    cidr: &str,
    devices: &[CameraDevice],
    rtsp_port: u16,
) -> Result<Vec<String>, String> {
    let (network, prefix) = parse_cidr(cidr)?;
    let mut ordered = Vec::new();
    let mut seen = HashSet::new();

    for device in devices {
        if let Some(ip) = &device.ip_address {
            if ip_in_cidr(ip, network, prefix) && seen.insert(ip.clone()) {
                ordered.push(ip.clone());
            }
        }
    }

    let hosts = enumerate_hosts(network, prefix)?;
    if hosts.len() > 256 {
        return Err(format!(
            "当前网段 {cidr} 包含 {} 个主机，超出快速扫描上限；请先缩小到 /24 或更小网段",
            hosts.len()
        ));
    }

    for ip in discover_open_rtsp_hosts(&hosts, rtsp_port) {
        if seen.insert(ip.clone()) {
            ordered.push(ip);
        }
    }

    if ordered.is_empty() {
        return Err(format!(
            "在 {cidr} 中没有探测到开放 {rtsp_port} 端口的候选主机；可以先手动添加第一台摄像头"
        ));
    }

    Ok(ordered)
}

fn bridge_provider_status_from_gateway_response(
    gateway_base_url: &str,
    platforms: &[GatewayPlatformStatus],
) -> BridgeProviderConfig {
    let selected = platforms
        .iter()
        .find(|platform| platform.connected)
        .or_else(|| platforms.iter().find(|platform| platform.enabled))
        .or_else(|| platforms.first());
    let mut provider = BridgeProviderConfig {
        gateway_base_url: gateway_base_url.trim().to_string(),
        last_checked_at: current_timestamp(),
        ..Default::default()
    };
    let Some(selected) = selected else {
        provider.status = "HarborGate 未配置平台".to_string();
        return provider;
    };

    provider.configured = selected.enabled;
    provider.connected = selected.connected;
    provider.platform = selected.platform.trim().to_string();
    provider.app_name = selected.display_name.trim().to_string();
    provider.status = if selected.connected {
        "已连接".to_string()
    } else if selected.enabled {
        "已启用，待连接".to_string()
    } else {
        "未启用".to_string()
    };
    provider.capabilities = BridgeProviderCapabilities {
        reply: selected.capabilities.reply,
        update: selected.capabilities.update,
        attachments: selected.capabilities.attachments,
    };
    provider
}

fn current_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[cfg(test)]
fn rtsp_scan_credentials(
    defaults: &AdminDefaults,
    username: Option<String>,
    password: Option<String>,
) -> RtspScanCredentials {
    RtspScanCredentials {
        username: username
            .as_deref()
            .and_then(non_empty_opt)
            .or_else(|| non_empty_opt(&defaults.rtsp_username)),
        password: password.as_deref().and_then(non_empty_opt),
    }
}

#[cfg(test)]
fn can_register_rtsp_scan_result(requires_auth: bool, has_password: bool) -> bool {
    !requires_auth || has_password
}

#[cfg(test)]
fn discover_open_rtsp_hosts(hosts: &[String], port: u16) -> Vec<String> {
    let mut handles = Vec::with_capacity(hosts.len());
    for ip in hosts {
        let ip = ip.clone();
        handles.push(thread::spawn(move || {
            let ipv4 = ip.parse::<Ipv4Addr>().ok()?;
            let socket = SocketAddrV4::new(ipv4, port);
            match TcpStream::connect_timeout(&socket.into(), Duration::from_millis(250)) {
                Ok(stream) => {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    Some(ip)
                }
                Err(_) => None,
            }
        }));
    }

    let mut found = Vec::new();
    for handle in handles {
        if let Ok(Some(ip)) = handle.join() {
            found.push(ip);
        }
    }
    found.sort();
    found
}

#[cfg(test)]
fn parse_cidr(cidr: &str) -> Result<(Ipv4Addr, u8), String> {
    let mut parts = cidr.trim().split('/');
    let network = parts
        .next()
        .ok_or_else(|| format!("invalid CIDR: {cidr}"))?
        .parse::<Ipv4Addr>()
        .map_err(|error| format!("invalid CIDR network {cidr}: {error}"))?;
    let prefix = parts
        .next()
        .ok_or_else(|| format!("invalid CIDR prefix: {cidr}"))?
        .parse::<u8>()
        .map_err(|error| format!("invalid CIDR prefix {cidr}: {error}"))?;
    if prefix > 32 {
        return Err(format!("CIDR prefix out of range: {cidr}"));
    }
    Ok((network, prefix))
}

#[cfg(test)]
fn ip_in_cidr(ip: &str, network: Ipv4Addr, prefix: u8) -> bool {
    let parsed = match ip.parse::<Ipv4Addr>() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    (u32::from(parsed) & mask) == (u32::from(network) & mask)
}

#[cfg(test)]
fn enumerate_hosts(network: Ipv4Addr, prefix: u8) -> Result<Vec<String>, String> {
    let normalized_network = normalize_network(network, prefix);
    let host_count = if prefix == 32 {
        1
    } else if prefix == 31 {
        2
    } else {
        (1u32 << (32 - prefix)) - 2
    };
    if host_count == 0 {
        return Err("CIDR does not contain usable hosts".to_string());
    }

    let base = u32::from(normalized_network);
    let start = if prefix >= 31 { base } else { base + 1 };
    let end = start + host_count;
    let mut hosts = Vec::with_capacity(host_count as usize);
    for value in start..end {
        hosts.push(Ipv4Addr::from(value).to_string());
    }
    Ok(hosts)
}

#[cfg(test)]
fn normalize_network(network: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    Ipv4Addr::from(u32::from(network) & mask)
}

#[cfg(test)]
mod tests {
    use super::{
        bridge_provider_status_from_gateway_response, build_mobile_setup_url,
        can_register_rtsp_scan_result, effective_rtsp_path_candidates, humanize_probe_error,
        looks_like_auth_error, merge_camera, normalize_camera_metadata,
        resolve_discovery_protocols, rtsp_scan_credentials, AdminDefaults,
    };
    use crate::connectors::im_gateway::{GatewayPlatformCapabilities, GatewayPlatformStatus};
    use crate::runtime::registry::{CameraDevice, StreamTransport};

    #[test]
    fn build_mobile_setup_url_supports_static_and_session_variants() {
        assert_eq!(
            build_mobile_setup_url("http://harborbeacon.local:4174", None),
            "http://harborbeacon.local:4174/setup/mobile"
        );
        assert_eq!(
            build_mobile_setup_url("http://harborbeacon.local:4174/", Some("ABCD-1234")),
            "http://harborbeacon.local:4174/setup/mobile?session=ABCD-1234"
        );
    }

    #[test]
    fn auth_error_is_humanized() {
        assert!(looks_like_auth_error("401 Unauthorized"));
        assert_eq!(
            humanize_probe_error("rtsp://demo: 401 Unauthorized"),
            "RTSP 返回 401，说明摄像头需要密码。"
        );
    }

    #[test]
    fn authenticated_scan_results_need_password_before_registration() {
        assert!(can_register_rtsp_scan_result(false, false));
        assert!(can_register_rtsp_scan_result(true, true));
        assert!(!can_register_rtsp_scan_result(true, false));
    }

    #[test]
    fn scan_credentials_do_not_reuse_default_password() {
        let defaults = AdminDefaults {
            rtsp_username: "admin".to_string(),
            rtsp_password: "old-secret".to_string(),
            ..AdminDefaults::default()
        };

        let without_explicit_password = rtsp_scan_credentials(&defaults, None, None);
        assert_eq!(without_explicit_password.username.as_deref(), Some("admin"));
        assert!(without_explicit_password.password.is_none());

        let with_explicit_password = rtsp_scan_credentials(
            &defaults,
            Some("admin".to_string()),
            Some("fresh-secret".to_string()),
        );
        assert_eq!(
            with_explicit_password.password.as_deref(),
            Some("fresh-secret")
        );
    }

    #[test]
    fn effective_rtsp_path_candidates_keep_existing_entries_and_add_tapo_fallbacks() {
        let paths = effective_rtsp_path_candidates(
            &["/Streaming/Channels/101".to_string()],
            Some("TP-Link"),
            Some("Tapo C200"),
        );

        assert_eq!(paths[0], "/Streaming/Channels/101");
        assert!(paths.contains(&"/stream1".to_string()));
        assert!(paths.contains(&"/stream2".to_string()));
        assert!(paths.contains(&"/live".to_string()));
        assert_eq!(
            paths
                .iter()
                .filter(|path| path.as_str() == "/stream1")
                .count(),
            1
        );
    }

    #[test]
    fn merge_camera_keeps_stable_identity_and_normalizes_source() {
        let mut existing = CameraDevice::new("cam-1", "Front Door", "rtsp://1.1.1.1/live");
        existing.room = Some("客厅".to_string());
        let mut incoming = CameraDevice::new("cam-2", "Front Door Cam", "rtsp://1.1.1.1/live");
        incoming.discovery_source = "onvif".to_string();
        incoming.primary_stream.transport = StreamTransport::Unknown;

        let merged = merge_camera(existing, incoming);
        assert_eq!(merged.device_id, "cam-1");
        assert_eq!(merged.room.as_deref(), Some("客厅"));
        assert_eq!(merged.discovery_source, "onvif");
        assert_eq!(merged.primary_stream.transport, StreamTransport::Rtsp);

        let normalized = normalize_camera_metadata(merged);
        assert_eq!(normalized.discovery_source, "onvif");
    }

    #[test]
    fn discovery_protocols_include_rtsp_probe_and_detect_keywords() {
        let protocols = resolve_discovery_protocols("ONVIF + RTSP");
        assert!(protocols.contains(&crate::runtime::discovery::DiscoveryProtocol::Onvif));
        assert!(protocols.contains(&crate::runtime::discovery::DiscoveryProtocol::RtspProbe));

        let protocols = resolve_discovery_protocols("mDNS + SSDP");
        assert!(protocols.contains(&crate::runtime::discovery::DiscoveryProtocol::Mdns));
        assert!(protocols.contains(&crate::runtime::discovery::DiscoveryProtocol::Ssdp));
        assert!(protocols.contains(&crate::runtime::discovery::DiscoveryProtocol::RtspProbe));
    }

    #[test]
    fn gateway_status_maps_to_redacted_bridge_provider_state() {
        let provider = bridge_provider_status_from_gateway_response(
            "http://gateway.local:4180",
            &[GatewayPlatformStatus {
                platform: "feishu".to_string(),
                enabled: true,
                connected: true,
                display_name: "HarborBeacon Bot".to_string(),
                capabilities: GatewayPlatformCapabilities {
                    reply: true,
                    update: false,
                    attachments: true,
                },
            }],
        );

        assert!(provider.configured);
        assert!(provider.connected);
        assert_eq!(provider.platform, "feishu");
        assert_eq!(provider.app_name, "HarborBeacon Bot");
        assert_eq!(provider.gateway_base_url, "http://gateway.local:4180");
        assert_eq!(provider.status, "已连接");
        assert!(provider.capabilities.reply);
        assert!(!provider.capabilities.update);
        assert!(provider.capabilities.attachments);
        assert_eq!(provider.app_secret, "");
        assert_eq!(provider.bot_open_id, "");
    }

    #[test]
    fn gateway_status_prefers_connected_platform_without_feishu_bias() {
        let provider = bridge_provider_status_from_gateway_response(
            "http://gateway.local:4180",
            &[
                GatewayPlatformStatus {
                    platform: "feishu".to_string(),
                    enabled: true,
                    connected: false,
                    display_name: "Feishu Bot".to_string(),
                    capabilities: GatewayPlatformCapabilities {
                        reply: true,
                        update: false,
                        attachments: true,
                    },
                },
                GatewayPlatformStatus {
                    platform: "telegram".to_string(),
                    enabled: true,
                    connected: true,
                    display_name: "Telegram Bot".to_string(),
                    capabilities: GatewayPlatformCapabilities {
                        reply: true,
                        update: true,
                        attachments: false,
                    },
                },
            ],
        );

        assert_eq!(provider.platform, "telegram");
        assert_eq!(provider.app_name, "Telegram Bot");
        assert_eq!(provider.status, "已连接");
        assert!(provider.capabilities.update);
    }
}
