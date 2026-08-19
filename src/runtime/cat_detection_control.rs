//! Durable per-camera control policies for cat detection.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::runtime::secure_store_path::{SecureFileIdentity, SecureStorePath};

pub const CAT_DETECTION_CONTROL_PATH_ENV: &str = "HARBOR_K3_CAT_DETECTION_CONTROL_PATH";
const DEFAULT_CONTROL_PATH: &str = ".harborbeacon/cat-detection-controls.json";
const MAX_CONTROL_BYTES: u64 = 1024 * 1024;
pub const MAX_PENDING_DETECTION_LEASES: usize = 64;
pub const MAX_CAT_DETECTION_CONTROL_POLICIES: usize = 256;
const MAX_DETECTION_LEASE_ID_BYTES: usize = 256;
const MAX_DETECTION_LEASE_CREATE_ATTEMPT_ID_BYTES: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatDetectionControlPolicy {
    pub camera_id: String,
    pub desired_enabled: bool,
    pub stream_profile: String,
    pub updated_at_epoch_ms: u128,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_detection_lease_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_lease_create_attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_lease_create_attempt_stream_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_detection_lease_create_attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_stream_profile: Option<String>,
}

impl CatDetectionControlPolicy {
    pub fn new(
        camera_id: impl Into<String>,
        desired_enabled: bool,
        stream_profile: impl Into<String>,
        updated_at_epoch_ms: u128,
    ) -> Result<Self, String> {
        let camera_id = normalize_camera_id(camera_id.into())?;
        let stream_profile = validate_stream_profile(stream_profile.into())?;
        Ok(Self {
            camera_id,
            desired_enabled,
            stream_profile,
            updated_at_epoch_ms,
            pending_detection_lease_ids: Vec::new(),
            detection_lease_create_attempt_id: None,
            detection_lease_create_attempt_stream_profile: None,
            rollback_detection_lease_create_attempt_id: None,
            rollback_stream_profile: None,
        })
    }

    pub fn set_pending_detection_lease_ids(
        &mut self,
        lease_ids: impl IntoIterator<Item = String>,
    ) -> Result<(), String> {
        self.pending_detection_lease_ids = normalize_pending_detection_lease_ids(lease_ids)?;
        Ok(())
    }

    pub fn set_detection_lease_create_attempt_id(
        &mut self,
        attempt_id: Option<String>,
    ) -> Result<(), String> {
        let attempt_id = attempt_id
            .map(validate_detection_lease_create_attempt_id)
            .transpose()?;
        self.detection_lease_create_attempt_stream_profile =
            attempt_id.as_ref().map(|_| self.stream_profile.clone());
        self.detection_lease_create_attempt_id = attempt_id;
        Ok(())
    }

    pub fn set_detection_lease_create_attempt(
        &mut self,
        attempt_id: Option<String>,
        stream_profile: Option<String>,
    ) -> Result<(), String> {
        let attempt_id = attempt_id
            .map(validate_detection_lease_create_attempt_id)
            .transpose()?;
        let stream_profile = stream_profile.map(validate_stream_profile).transpose()?;
        if attempt_id.is_none() && stream_profile.is_some() {
            return Err(
                "cat detection control create attempt profile requires an attempt ID".to_string(),
            );
        }
        self.detection_lease_create_attempt_id = attempt_id;
        self.detection_lease_create_attempt_stream_profile = stream_profile;
        Ok(())
    }

    pub fn detection_lease_create_attempt_stream_profile(&self) -> Option<&str> {
        self.detection_lease_create_attempt_id.as_ref().map(|_| {
            self.detection_lease_create_attempt_stream_profile
                .as_deref()
                .unwrap_or(self.stream_profile.as_str())
        })
    }

    pub fn set_rollback_detection_lease_create_attempt(
        &mut self,
        attempt_id: Option<String>,
        stream_profile: Option<String>,
    ) -> Result<(), String> {
        let attempt_id = attempt_id
            .map(validate_detection_lease_create_attempt_id)
            .transpose()?;
        let stream_profile = stream_profile.map(validate_stream_profile).transpose()?;
        if attempt_id.is_some() != stream_profile.is_some() {
            return Err(
                "cat detection control rollback attempt ID and profile must be paired".to_string(),
            );
        }
        if self.desired_enabled && stream_profile.as_deref() == Some(self.stream_profile.as_str()) {
            return Err(
                "cat detection control rollback profile must differ from desired profile"
                    .to_string(),
            );
        }
        self.rollback_detection_lease_create_attempt_id = attempt_id;
        self.rollback_stream_profile = stream_profile;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct CatDetectionControlStore {
    path: PathBuf,
    secure_path: Arc<SecureStorePath>,
    operation_lock: Arc<Mutex<()>>,
    bound_data_identity: Arc<Mutex<Option<SecureFileIdentity>>>,
}

impl CatDetectionControlStore {
    pub fn try_new(path: PathBuf) -> Result<Self, String> {
        let lock_path = lock_path_for(&path)?;
        let secure_path = SecureStorePath::try_new(path, lock_path)?;
        let bound_data_identity = secure_path.open_data_read()?.map(|opened| opened.identity);
        let path = secure_path.data_path().to_path_buf();
        Ok(Self {
            path,
            secure_path: Arc::new(secure_path),
            operation_lock: Arc::new(Mutex::new(())),
            bound_data_identity: Arc::new(Mutex::new(bound_data_identity)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<BTreeMap<String, CatDetectionControlPolicy>, String> {
        self.with_lock(|| self.load_unlocked())
    }

    pub fn upsert(&self, policy: CatDetectionControlPolicy) -> Result<(), String> {
        validate_policy(&policy)?;
        self.with_lock(|| {
            let mut policies = self.load_unlocked()?;
            policies.insert(policy.camera_id.clone(), policy);
            self.write_unlocked(&policies)
        })
    }

    fn with_lock<T>(&self, action: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let _operation_guard = self
            .operation_lock
            .lock()
            .map_err(|_| "cat detection control operation lock is poisoned".to_string())?;
        let lock_file = self.secure_path.open_lock()?;
        #[cfg(test)]
        notify_lock_acquire_observer();
        lock_file.lock_exclusive().map_err(|error| {
            format!(
                "failed to acquire cat detection control lock for {}: {error}",
                self.path.display()
            )
        })?;
        let result = (|| {
            self.secure_path.ensure_lock_identity()?;
            self.refresh_bound_data_identity()?;
            let value = action()?;
            self.verify_bound_data_identity()?;
            Ok(value)
        })();
        let unlock_result = FileExt::unlock(&lock_file).map_err(|error| {
            format!(
                "failed to release cat detection control lock for {}: {error}",
                self.path.display()
            )
        });
        match (result, unlock_result) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    fn refresh_bound_data_identity(&self) -> Result<(), String> {
        self.secure_path.ensure_parent_identity()?;
        let expected = *self
            .bound_data_identity
            .lock()
            .map_err(|_| "cat detection control data identity lock is poisoned".to_string())?;
        let observed = self.current_data_identity()?;
        if expected == observed {
            return Ok(());
        }

        // A peer store may atomically replace the file. Re-observe it through the
        // bound directory before accepting the new identity as the current binding.
        let revalidated = self.current_data_identity()?;
        if observed != revalidated {
            return Err("cat detection control data identity changed during refresh".to_string());
        }
        *self
            .bound_data_identity
            .lock()
            .map_err(|_| "cat detection control data identity lock is poisoned".to_string())? =
            revalidated;
        Ok(())
    }

    fn current_data_identity(&self) -> Result<Option<SecureFileIdentity>, String> {
        self.secure_path
            .open_data_read()
            .map(|opened| opened.map(|opened| opened.identity))
    }

    fn verify_bound_data_identity(&self) -> Result<(), String> {
        self.verify_data_identity(self.current_data_identity()?)
    }

    fn verify_data_identity(&self, actual: Option<SecureFileIdentity>) -> Result<(), String> {
        let expected = *self
            .bound_data_identity
            .lock()
            .map_err(|_| "cat detection control data identity lock is poisoned".to_string())?;
        if expected != actual {
            return Err("cat detection control data identity changed unexpectedly".to_string());
        }
        Ok(())
    }

    fn load_unlocked(&self) -> Result<BTreeMap<String, CatDetectionControlPolicy>, String> {
        let opened = self.secure_path.open_data_read()?;
        self.verify_data_identity(opened.as_ref().map(|opened| opened.identity))?;
        let Some(opened) = opened else {
            return Ok(BTreeMap::new());
        };
        if opened.len > MAX_CONTROL_BYTES {
            return Err("cat detection control store exceeds size limit".to_string());
        }
        let mut bytes = Vec::with_capacity(opened.len as usize);
        opened
            .file
            .take(MAX_CONTROL_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                format!(
                    "failed to read cat detection control store {}: {error}",
                    self.path.display()
                )
            })?;
        if bytes.len() as u64 > MAX_CONTROL_BYTES {
            return Err("cat detection control store exceeds size limit".to_string());
        }
        if bytes.is_empty() {
            return Err("cat detection control store is empty".to_string());
        }
        let policies: BTreeMap<String, CatDetectionControlPolicy> = serde_json::from_slice(&bytes)
            .map_err(|error| format!("cat detection control store is invalid: {error}"))?;
        validate_policy_map(&policies)?;
        Ok(policies)
    }

    fn write_unlocked(
        &self,
        policies: &BTreeMap<String, CatDetectionControlPolicy>,
    ) -> Result<(), String> {
        validate_policy_map(policies)?;
        let bytes = serde_json::to_vec(policies)
            .map_err(|error| format!("failed to serialize cat detection control state: {error}"))?;
        if bytes.len() as u64 > MAX_CONTROL_BYTES {
            return Err("cat detection control store exceeds size limit".to_string());
        }
        let identity = self
            .secure_path
            .replace_data_atomically(&bytes, || Ok(()))
            .map_err(|error| {
                format!(
                    "failed to atomically replace cat detection control store {}: {error}",
                    self.path.display()
                )
            })?;
        *self
            .bound_data_identity
            .lock()
            .map_err(|_| "cat detection control data identity lock is poisoned".to_string())? =
            Some(identity);
        Ok(())
    }
}

#[cfg(test)]
struct LockAcquireObserverGuard;

#[cfg(test)]
impl Drop for LockAcquireObserverGuard {
    fn drop(&mut self) {
        *lock_acquire_observer()
            .lock()
            .expect("lock acquire observer lock") = None;
    }
}

#[cfg(test)]
fn install_lock_acquire_observer(
    observer: std::sync::mpsc::Sender<()>,
) -> LockAcquireObserverGuard {
    let mut installed = lock_acquire_observer()
        .lock()
        .expect("lock acquire observer lock");
    assert!(
        installed.is_none(),
        "lock acquire observer is already installed"
    );
    *installed = Some(observer);
    LockAcquireObserverGuard
}

#[cfg(test)]
fn notify_lock_acquire_observer() {
    if let Some(observer) = lock_acquire_observer()
        .lock()
        .expect("lock acquire observer lock")
        .as_ref()
    {
        let _ = observer.send(());
    }
}

#[cfg(test)]
fn lock_acquire_observer() -> &'static Mutex<Option<std::sync::mpsc::Sender<()>>> {
    static OBSERVER: OnceLock<Mutex<Option<std::sync::mpsc::Sender<()>>>> = OnceLock::new();
    OBSERVER.get_or_init(|| Mutex::new(None))
}

pub fn default_control_path() -> PathBuf {
    env::var_os(CAT_DETECTION_CONTROL_PATH_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONTROL_PATH))
}

pub fn validate_cat_detection_control_camera_id(camera_id: &str) -> Result<(), String> {
    normalize_camera_id(camera_id.to_string()).map(|_| ())
}

fn normalize_camera_id(camera_id: String) -> Result<String, String> {
    if camera_id.trim() != camera_id {
        return Err("cat detection control camera_id must not have edge whitespace".to_string());
    }
    if camera_id.is_empty() || camera_id.len() > 128 || camera_id.chars().any(char::is_control) {
        return Err("cat detection control camera_id is invalid".to_string());
    }
    Ok(camera_id)
}

fn lock_path_for(path: &Path) -> Result<PathBuf, String> {
    let data_name = path
        .file_name()
        .ok_or_else(|| "cat detection control path must include a file name".to_string())?;
    let mut lock_name = OsString::from(data_name);
    lock_name.push(".lock");
    Ok(path.with_file_name(lock_name))
}

fn validate_stream_profile(stream_profile: String) -> Result<String, String> {
    match stream_profile.as_str() {
        "sub" | "main" => Ok(stream_profile),
        _ => Err("cat detection control stream_profile must be sub or main".to_string()),
    }
}

fn validate_policy(policy: &CatDetectionControlPolicy) -> Result<(), String> {
    let normalized_camera_id = normalize_camera_id(policy.camera_id.clone())?;
    if normalized_camera_id != policy.camera_id {
        return Err("cat detection control camera_id must be normalized".to_string());
    }
    validate_stream_profile(policy.stream_profile.clone())?;
    let normalized =
        normalize_pending_detection_lease_ids(policy.pending_detection_lease_ids.iter().cloned())?;
    if normalized != policy.pending_detection_lease_ids {
        return Err(
            "cat detection control pending lease IDs must be sorted and unique".to_string(),
        );
    }
    if let Some(attempt_id) = policy.detection_lease_create_attempt_id.clone() {
        validate_detection_lease_create_attempt_id(attempt_id)?;
    }
    if let Some(stream_profile) = policy.detection_lease_create_attempt_stream_profile.clone() {
        validate_stream_profile(stream_profile)?;
        if policy.detection_lease_create_attempt_id.is_none() {
            return Err(
                "cat detection control create attempt profile requires an attempt ID".to_string(),
            );
        }
    }
    match (
        policy.rollback_detection_lease_create_attempt_id.clone(),
        policy.rollback_stream_profile.clone(),
    ) {
        (Some(attempt_id), Some(stream_profile)) => {
            validate_detection_lease_create_attempt_id(attempt_id)?;
            validate_stream_profile(stream_profile.clone())?;
            if policy.desired_enabled && stream_profile == policy.stream_profile {
                return Err(
                    "cat detection control rollback profile must differ from desired profile"
                        .to_string(),
                );
            }
        }
        (None, None) => {}
        _ => {
            return Err(
                "cat detection control rollback attempt ID and profile must be paired".to_string(),
            )
        }
    }
    Ok(())
}

fn validate_detection_lease_create_attempt_id(attempt_id: String) -> Result<String, String> {
    if attempt_id.is_empty()
        || attempt_id.len() > MAX_DETECTION_LEASE_CREATE_ATTEMPT_ID_BYTES
        || !attempt_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("cat detection control create attempt ID is invalid".to_string());
    }
    Ok(attempt_id)
}

fn normalize_pending_detection_lease_ids(
    lease_ids: impl IntoIterator<Item = String>,
) -> Result<Vec<String>, String> {
    let mut normalized = BTreeSet::new();
    for lease_id in lease_ids {
        if lease_id.is_empty()
            || lease_id.len() > MAX_DETECTION_LEASE_ID_BYTES
            || lease_id.chars().any(char::is_control)
        {
            return Err("cat detection control pending detection lease ID is invalid".to_string());
        }
        normalized.insert(lease_id);
    }
    if normalized.len() > MAX_PENDING_DETECTION_LEASES {
        return Err(format!(
            "cat detection control cannot retain more than {MAX_PENDING_DETECTION_LEASES} pending detection leases"
        ));
    }
    Ok(normalized.into_iter().collect())
}

fn validate_policy_map(
    policies: &BTreeMap<String, CatDetectionControlPolicy>,
) -> Result<(), String> {
    if policies.len() > MAX_CAT_DETECTION_CONTROL_POLICIES {
        return Err(format!(
            "cat detection control store cannot contain more than {MAX_CAT_DETECTION_CONTROL_POLICIES} camera policies"
        ));
    }
    for (camera_id, policy) in policies {
        if camera_id != &policy.camera_id {
            return Err(
                "cat detection control store key does not match policy camera_id".to_string(),
            );
        }
        validate_policy(policy)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    #[cfg(windows)]
    use std::process::Command;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use fs2::FileExt;
    use uuid::Uuid;

    use super::{
        install_lock_acquire_observer, CatDetectionControlPolicy, CatDetectionControlStore,
        MAX_CONTROL_BYTES,
    };

    fn temporary_test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "harborbeacon-cat-detection-control-{name}-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ))
    }

    fn policy(
        camera_id: &str,
        desired_enabled: bool,
        stream_profile: &str,
        updated_at_epoch_ms: u128,
    ) -> CatDetectionControlPolicy {
        CatDetectionControlPolicy::new(
            camera_id,
            desired_enabled,
            stream_profile,
            updated_at_epoch_ms,
        )
        .expect("valid control policy")
    }

    #[cfg(windows)]
    fn create_directory_alias(alias: &Path, target: &Path) {
        let output = Command::new("cmd.exe")
            .args([
                "/c",
                "mklink",
                "/J",
                &alias.to_string_lossy(),
                &target.to_string_lossy(),
            ])
            .output()
            .expect("directory junction command should run");
        assert!(
            output.status.success(),
            "directory junction should be created: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(not(windows))]
    fn replace_data_with_alias(path: &Path, target: &Path) {
        fs::remove_file(path).expect("remove original control main");
        std::os::unix::fs::symlink(target, path).expect("replace main with symlink");
    }

    #[cfg(windows)]
    fn replace_data_with_alias(path: &Path, target: &Path) {
        fs::remove_file(path).expect("remove original control main");
        fs::create_dir_all(target).expect("attacker junction target");
        create_directory_alias(path, target);
    }

    #[test]
    fn missing_file_loads_as_empty_control_map() {
        let root = temporary_test_root("missing");
        let path = root.join("controls.json");
        let store = CatDetectionControlStore::try_new(path.clone()).expect("control store");

        assert_eq!(store.path(), path.canonicalize().unwrap_or(path).as_path());
        assert!(store.load().expect("missing store loads empty").is_empty());
    }

    #[test]
    fn upsert_roundtrips_enabled_and_explicitly_disabled_policies() {
        let root = temporary_test_root("roundtrip");
        let path = root.join("controls.json");
        let store = CatDetectionControlStore::try_new(path).expect("control store");
        let enabled = policy("camera-enabled", true, "sub", 1_700_000_000_000);
        let disabled = policy("camera-disabled", false, "main", 1_700_000_000_001);

        store
            .upsert(enabled.clone())
            .expect("enabled policy persists");
        store
            .upsert(disabled.clone())
            .expect("explicitly disabled policy persists");

        let loaded = store.load().expect("policies load");
        assert_eq!(loaded.get("camera-enabled"), Some(&enabled));
        assert_eq!(loaded.get("camera-disabled"), Some(&disabled));
        assert!(!loaded["camera-disabled"].desired_enabled);
    }

    #[test]
    fn policy_store_rejects_more_than_two_hundred_fifty_six_cameras() {
        let root = temporary_test_root("policy-capacity");
        let path = root.join("controls.json");
        let store = CatDetectionControlStore::try_new(path.clone()).expect("control store");
        for index in 0..256 {
            store
                .upsert(policy(&format!("camera-{index:03}"), false, "sub", index))
                .expect("policy within capacity persists");
        }
        let error = store
            .upsert(policy("camera-overflow", false, "sub", 257))
            .expect_err("257th policy must be rejected");
        assert!(error.contains("256"), "{error}");

        let oversized = (0..257)
            .map(|index| {
                let camera_id = format!("stored-camera-{index:03}");
                (camera_id.clone(), policy(&camera_id, false, "sub", index))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        fs::write(
            &path,
            serde_json::to_vec(&oversized).expect("oversized policies json"),
        )
        .expect("oversized policy store");
        let store = CatDetectionControlStore::try_new(path).expect("reopened control store");
        let error = store
            .load()
            .expect_err("oversized stored policy map must be rejected");
        assert!(error.contains("256"), "{error}");
    }

    #[test]
    fn legacy_policy_schema_loads_with_no_pending_detection_leases() {
        let root = temporary_test_root("legacy-policy-schema");
        let path = root.join("controls.json");
        fs::create_dir_all(&root).expect("test root");
        fs::write(
            &path,
            br#"{"camera-legacy":{"camera_id":"camera-legacy","desired_enabled":false,"stream_profile":"sub","updated_at_epoch_ms":7}}"#,
        )
        .expect("legacy control file");
        let store = CatDetectionControlStore::try_new(path).expect("control store");

        let loaded = store.load().expect("legacy policies load");

        assert!(loaded["camera-legacy"]
            .pending_detection_lease_ids
            .is_empty());
        assert!(loaded["camera-legacy"]
            .detection_lease_create_attempt_id
            .is_none());
        assert!(loaded["camera-legacy"]
            .detection_lease_create_attempt_stream_profile
            .is_none());
        assert!(loaded["camera-legacy"]
            .rollback_detection_lease_create_attempt_id
            .is_none());
        assert!(loaded["camera-legacy"].rollback_stream_profile.is_none());
    }

    #[test]
    fn rollback_attempt_is_paired_bounded_and_roundtrips() {
        let root = temporary_test_root("rollback-attempt");
        let path = root.join("controls.json");
        let store = CatDetectionControlStore::try_new(path).expect("control store");
        let mut control = policy("camera-1", true, "main", 1);

        control
            .set_rollback_detection_lease_create_attempt(
                Some("rollback-attempt-0123456789".to_string()),
                Some("sub".to_string()),
            )
            .expect("valid rollback attempt");
        store.upsert(control.clone()).expect("policy persists");
        assert_eq!(store.load().expect("policy loads")["camera-1"], control);

        assert!(control
            .set_rollback_detection_lease_create_attempt(Some("rollback-only".to_string()), None,)
            .is_err());
        assert!(control
            .set_rollback_detection_lease_create_attempt(None, Some("sub".to_string()),)
            .is_err());
        assert!(control
            .set_rollback_detection_lease_create_attempt(
                Some("rollback-same-profile".to_string()),
                Some("main".to_string()),
            )
            .is_err());
        assert!(control
            .set_rollback_detection_lease_create_attempt(
                Some("x".repeat(129)),
                Some("sub".to_string()),
            )
            .is_err());
        assert!(control
            .set_rollback_detection_lease_create_attempt(
                Some("rollback-invalid-profile".to_string()),
                Some("high".to_string()),
            )
            .is_err());
        control
            .set_rollback_detection_lease_create_attempt(None, None)
            .expect("rollback marker can be cleared");
        assert!(control.rollback_detection_lease_create_attempt_id.is_none());
        assert!(control.rollback_stream_profile.is_none());
    }

    #[test]
    fn disabled_rollback_attempt_allows_profile_matching_desired_for_cleanup() {
        let root = temporary_test_root("disabled-rollback-same-profile");
        let path = root.join("controls.json");
        let store = CatDetectionControlStore::try_new(path).expect("control store");
        let mut control = policy("camera-1", false, "sub", 2);

        control
            .set_rollback_detection_lease_create_attempt(
                Some("rollback-cleanup-attempt".to_string()),
                Some("sub".to_string()),
            )
            .expect("disabled rollback marker is cleanup-only ownership");
        store.upsert(control.clone()).expect("policy persists");

        assert_eq!(store.load().expect("policy loads")["camera-1"], control);
    }

    #[test]
    fn stored_rollback_attempt_requires_a_valid_distinct_profile_pair() {
        for (name, rollback_id, rollback_profile) in [
            ("id-only", Some("rollback-id"), None),
            ("profile-only", None, Some("sub")),
            ("same-profile", Some("rollback-id"), Some("main")),
            ("invalid-profile", Some("rollback-id"), Some("high")),
        ] {
            let root = temporary_test_root(name);
            let path = root.join("controls.json");
            fs::create_dir_all(&root).expect("test root");
            fs::write(
                &path,
                serde_json::to_vec(&serde_json::json!({
                    "camera-1": {
                        "camera_id": "camera-1",
                        "desired_enabled": true,
                        "stream_profile": "main",
                        "updated_at_epoch_ms": 7,
                        "rollback_detection_lease_create_attempt_id": rollback_id,
                        "rollback_stream_profile": rollback_profile
                    }
                }))
                .expect("invalid rollback policy json"),
            )
            .expect("control file");
            let store = CatDetectionControlStore::try_new(path).expect("control store");

            let error = store
                .load()
                .expect_err("invalid rollback pair must be rejected");
            assert!(
                error.contains("rollback") || error.contains("stream_profile"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn detection_lease_create_attempt_id_is_optional_bounded_and_roundtrips() {
        let root = temporary_test_root("create-attempt-id");
        let path = root.join("controls.json");
        let store = CatDetectionControlStore::try_new(path).expect("control store");
        let mut control = policy("camera-1", true, "sub", 1);

        control
            .set_detection_lease_create_attempt_id(Some("attempt-0123456789".to_string()))
            .expect("valid create attempt ID");
        assert_eq!(
            control
                .detection_lease_create_attempt_stream_profile
                .as_deref(),
            Some("sub")
        );
        store.upsert(control.clone()).expect("policy persists");
        assert_eq!(store.load().expect("policy loads")["camera-1"], control);

        assert!(control
            .set_detection_lease_create_attempt_id(Some(String::new()))
            .is_err());
        assert!(control
            .set_detection_lease_create_attempt_id(Some("bad\nattempt".to_string()))
            .is_err());
        assert!(control
            .set_detection_lease_create_attempt_id(Some("bad attempt".to_string()))
            .is_err());
        assert!(control
            .set_detection_lease_create_attempt_id(Some("x".repeat(129)))
            .is_err());
        control
            .set_detection_lease_create_attempt_id(None)
            .expect("attempt can be cleared");
        assert!(control.detection_lease_create_attempt_id.is_none());
        assert!(control
            .detection_lease_create_attempt_stream_profile
            .is_none());
        assert!(control
            .set_detection_lease_create_attempt(None, Some("sub".to_string()))
            .is_err());
        assert!(control
            .set_detection_lease_create_attempt(
                Some("attempt-profile".to_string()),
                Some("invalid".to_string()),
            )
            .is_err());
    }

    #[test]
    fn id_only_attempt_schema_uses_policy_profile_as_compatible_context() {
        let root = temporary_test_root("id-only-create-attempt");
        let path = root.join("controls.json");
        fs::create_dir_all(&root).expect("test root");
        fs::write(
            &path,
            br#"{"camera-legacy":{"camera_id":"camera-legacy","desired_enabled":true,"stream_profile":"main","updated_at_epoch_ms":8,"detection_lease_create_attempt_id":"attempt-legacy"}}"#,
        )
        .expect("legacy attempt control file");
        let store = CatDetectionControlStore::try_new(path).expect("control store");

        let loaded = store.load().expect("legacy attempt policy loads");
        let policy = &loaded["camera-legacy"];

        assert_eq!(
            policy.detection_lease_create_attempt_stream_profile(),
            Some("main")
        );
        assert!(policy
            .detection_lease_create_attempt_stream_profile
            .is_none());
    }

    #[test]
    fn pending_detection_lease_ids_are_deduplicated_and_validated() {
        let mut control = policy("camera-1", false, "sub", 1);

        control
            .set_pending_detection_lease_ids(vec![
                "detect-b".to_string(),
                "detect-a".to_string(),
                "detect-b".to_string(),
            ])
            .expect("valid pending leases");

        assert_eq!(
            control.pending_detection_lease_ids,
            vec!["detect-a".to_string(), "detect-b".to_string()]
        );
        assert!(control
            .set_pending_detection_lease_ids(vec![String::new()])
            .is_err());
        assert!(control
            .set_pending_detection_lease_ids(vec!["bad\nlease".to_string()])
            .is_err());
        assert!(control
            .set_pending_detection_lease_ids(vec!["x".repeat(257)])
            .is_err());
        assert!(control
            .set_pending_detection_lease_ids((0..65).map(|index| format!("detect-{index}")),)
            .is_err());
    }

    #[test]
    fn pending_detection_lease_capacity_is_checked_after_deduplication() {
        let root = temporary_test_root("deduplicated-capacity");
        let path = root.join("controls.json");
        let store = CatDetectionControlStore::try_new(path).expect("control store");
        let mut control = policy("camera-1", false, "sub", 1);
        let unique_ids = (0..64)
            .map(|index| format!("detect-{index:02}"))
            .collect::<Vec<_>>();
        let duplicated_ids = unique_ids
            .iter()
            .chain(unique_ids.iter())
            .cloned()
            .collect::<Vec<_>>();

        control
            .set_pending_detection_lease_ids(duplicated_ids)
            .expect("64 unique pending leases remain within capacity");
        store
            .upsert(control.clone())
            .expect("deduplicated boundary policy persists");

        assert_eq!(control.pending_detection_lease_ids, unique_ids);
        assert_eq!(
            store.load().expect("deduplicated boundary policy loads")["camera-1"],
            control
        );
    }

    #[test]
    fn stored_pending_detection_lease_ids_must_be_bounded_and_canonical() {
        let root = temporary_test_root("invalid-pending-leases");
        let path = root.join("controls.json");
        fs::create_dir_all(&root).expect("test root");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "camera-1": {
                    "camera_id": "camera-1",
                    "desired_enabled": false,
                    "stream_profile": "sub",
                    "updated_at_epoch_ms": 7,
                    "pending_detection_lease_ids": ["detect-b", "detect-a", "detect-b"]
                }
            }))
            .expect("invalid policy json"),
        )
        .expect("control file");
        let store = CatDetectionControlStore::try_new(path).expect("control store");

        let error = store
            .load()
            .expect_err("non-canonical pending leases must be rejected");

        assert!(error.contains("sorted and unique"), "{error}");
    }

    #[test]
    fn camera_id_with_slash_roundtrips_as_json_data() {
        let root = temporary_test_root("camera-id-slash");
        let path = root.join("controls.json");
        let store = CatDetectionControlStore::try_new(path).expect("control store");
        let control = policy("camera 1/left", true, "sub", 1);

        store.upsert(control.clone()).expect("policy persists");

        assert_eq!(
            store.load().expect("policy loads").get("camera 1/left"),
            Some(&control)
        );
    }

    #[test]
    fn camera_id_rejects_edge_whitespace_without_collapsing_distinct_ids() {
        assert!(CatDetectionControlPolicy::new(" camera ", true, "sub", 1).is_err());
        assert!(CatDetectionControlPolicy::new("camera\t", true, "sub", 1).is_err());

        let internal = CatDetectionControlPolicy::new("camera 1/left", true, "sub", 1)
            .expect("internal spaces and slash remain valid");
        let plain = CatDetectionControlPolicy::new("camera", false, "main", 2)
            .expect("plain camera ID remains valid");

        assert_eq!(internal.camera_id, "camera 1/left");
        assert_eq!(plain.camera_id, "camera");
    }

    #[test]
    fn stored_policy_with_edge_whitespace_is_rejected() {
        let root = temporary_test_root("edge-whitespace");
        let path = root.join("controls.json");
        fs::create_dir_all(&root).expect("test root");
        fs::write(
            &path,
            br#"{" camera ":{"camera_id":" camera ","desired_enabled":false,"stream_profile":"sub","updated_at_epoch_ms":7}}"#,
        )
        .expect("control file");
        let store = CatDetectionControlStore::try_new(path).expect("control store");

        let error = store
            .load()
            .expect_err("edge-whitespace policy must be rejected");

        assert!(error.contains("camera_id"), "{error}");
    }

    #[test]
    fn invalid_camera_id_and_profile_do_not_write_a_control_file() {
        let root = temporary_test_root("invalid-without-write");
        let path = root.join("controls.json");
        let _store = CatDetectionControlStore::try_new(path.clone()).expect("control store");

        assert!(CatDetectionControlPolicy::new("", true, "sub", 1).is_err());
        assert!(CatDetectionControlPolicy::new("camera-1", true, "high", 1).is_err());
        assert!(!path.exists(), "invalid input must not create data file");
    }

    #[test]
    fn corrupt_json_is_rejected() {
        let root = temporary_test_root("corrupt-json");
        let path = root.join("controls.json");
        fs::create_dir_all(&root).expect("test root");
        fs::write(&path, b"{not-json").expect("corrupt file");
        let store = CatDetectionControlStore::try_new(path).expect("control store");

        let error = store.load().expect_err("corrupt json must fail");
        assert!(error.contains("invalid"), "{error}");
    }

    #[test]
    fn empty_file_is_rejected() {
        let root = temporary_test_root("empty-file");
        let path = root.join("controls.json");
        fs::create_dir_all(&root).expect("test root");
        fs::write(&path, []).expect("empty file");
        let store = CatDetectionControlStore::try_new(path).expect("control store");

        let error = store.load().expect_err("empty file must fail");
        assert!(error.contains("empty"), "{error}");
    }

    #[test]
    fn oversized_json_is_rejected() {
        let root = temporary_test_root("oversized");
        let path = root.join("controls.json");
        fs::create_dir_all(&root).expect("test root");
        fs::write(&path, vec![b'x'; MAX_CONTROL_BYTES as usize + 1]).expect("oversized file");
        let store = CatDetectionControlStore::try_new(path).expect("control store");

        let error = store.load().expect_err("oversized json must fail");
        assert!(error.contains("size limit"), "{error}");
    }

    #[test]
    fn independently_constructed_stores_follow_atomic_replacement() {
        let root = temporary_test_root("atomic-replacement");
        let path = root.join("controls.json");
        let first = CatDetectionControlStore::try_new(path.clone()).expect("first store");
        let second = CatDetectionControlStore::try_new(path).expect("second store");

        let first_policy = policy("camera-first", true, "sub", 1);
        let second_policy = policy("camera-second", false, "main", 2);
        first
            .upsert(first_policy.clone())
            .expect("first policy persists");
        assert_eq!(
            second
                .load()
                .expect("second store observes first atomic write")
                .get("camera-first"),
            Some(&first_policy)
        );
        second
            .upsert(second_policy.clone())
            .expect("second policy persists through atomic replacement");

        let loaded = first.load().expect("first store observes replacement");
        assert_eq!(loaded.get("camera-first"), Some(&first_policy));
        assert_eq!(loaded.get("camera-second"), Some(&second_policy));
    }

    #[test]
    fn independent_stores_block_on_the_same_lock_before_preserving_both_policies() {
        let root = temporary_test_root("lock-contention");
        let path = root.join("controls.json");
        let first = Arc::new(CatDetectionControlStore::try_new(path.clone()).expect("first store"));
        let second = Arc::new(CatDetectionControlStore::try_new(path).expect("second store"));
        let first_policy = policy("camera-first", true, "sub", 1);
        let second_policy = policy("camera-second", false, "main", 2);

        let held_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("controls.json.lock"))
            .expect("store lock file");
        held_lock.lock_exclusive().expect("hold store lock");
        let (observer_sender, observer_receiver) = mpsc::channel();
        let observer_guard = install_lock_acquire_observer(observer_sender);
        let (result_sender, result_receiver) = mpsc::channel();

        let first_task = {
            let first = Arc::clone(&first);
            thread::spawn(move || {
                result_sender
                    .send(first.upsert(first_policy))
                    .expect("upsert result");
            })
        };

        observer_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("upsert reaches lock_exclusive while lock is held");
        let blocked = result_receiver.recv_timeout(Duration::from_millis(200));
        FileExt::unlock(&held_lock).expect("release store lock");
        assert!(
            matches!(blocked, Err(mpsc::RecvTimeoutError::Timeout)),
            "upsert must block on the held store lock"
        );
        result_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("blocked upsert completes after lock release")
            .expect("first upsert");
        first_task.join().expect("first upsert thread");
        drop(observer_guard);
        second.upsert(second_policy).expect("second upsert");

        let loaded = first.load().expect("policies load after lock contention");
        assert!(loaded.contains_key("camera-first"));
        assert!(loaded.contains_key("camera-second"));
    }

    #[test]
    fn replacement_refresh_rejects_tampered_data_alias() {
        let root = temporary_test_root("tampered-data-alias");
        let path = root.join("controls.json");
        let attacker = root.join("attacker-target");
        let store = CatDetectionControlStore::try_new(path.clone()).expect("control store");
        store
            .upsert(policy("camera-original", true, "sub", 1))
            .expect("original policy persists");
        #[cfg(not(windows))]
        fs::write(&attacker, b"external-marker").expect("external marker");
        #[cfg(windows)]
        fs::create_dir_all(&attacker).expect("external marker directory");
        replace_data_with_alias(&path, &attacker);

        let error = store.load().expect_err("tampered data alias must fail");
        assert!(
            error.contains("symlink")
                || error.contains("reparse")
                || error.contains("regular file"),
            "{error}"
        );
        #[cfg(not(windows))]
        assert_eq!(
            fs::read(&attacker).expect("external marker"),
            b"external-marker"
        );
    }

    #[test]
    fn data_path_with_lock_extension_uses_a_distinct_lock_file() {
        let root = temporary_test_root("lock-extension");
        let path = root.join("controls.lock");
        let store = CatDetectionControlStore::try_new(path.clone()).expect("control store");
        let control = policy("camera-1", true, "sub", 1);

        store.upsert(control.clone()).expect("policy persists");

        assert_eq!(
            store.load().expect("policy loads").get("camera-1"),
            Some(&control)
        );
        assert!(root.join("controls.lock.lock").is_file());
    }
}
