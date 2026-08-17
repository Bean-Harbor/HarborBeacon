//! Durable policy for automatic cat-activity monitoring.

use std::env;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::runtime::secure_store_path::{SecureFileIdentity, SecureStorePath};

pub const CAT_ACTIVITY_POLICY_PATH_ENV: &str = "HARBOR_K3_CAT_ACTIVITY_POLICY_PATH";
const DEFAULT_POLICY_PATH: &str = "/data/harborbeacon/cat-activity/policy.json";
const MAX_POLICY_BYTES: u64 = 64 * 1024;
const MAX_DISABLED_CAMERAS: usize = 256;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatActivityMode {
    #[default]
    AllEnabled,
    Off,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatActivityPolicy {
    pub schema_version: u8,
    pub mode: CatActivityMode,
    #[serde(default)]
    pub disabled_camera_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl Default for CatActivityPolicy {
    fn default() -> Self {
        Self {
            schema_version: 1,
            mode: CatActivityMode::AllEnabled,
            disabled_camera_ids: Vec::new(),
            updated_at: None,
        }
    }
}

impl CatActivityPolicy {
    pub fn normalize(mut self) -> Result<Self, String> {
        if self.schema_version != 1 {
            return Err("cat activity policy schema_version must be 1".to_string());
        }
        if self.disabled_camera_ids.len() > MAX_DISABLED_CAMERAS {
            return Err(format!(
                "cat activity policy supports at most {MAX_DISABLED_CAMERAS} disabled cameras"
            ));
        }
        for camera_id in &mut self.disabled_camera_ids {
            *camera_id = camera_id.trim().to_string();
            validate_camera_id(camera_id)?;
        }
        self.disabled_camera_ids.sort();
        if self
            .disabled_camera_ids
            .windows(2)
            .any(|ids| ids[0] == ids[1])
        {
            return Err("cat activity policy contains duplicate camera IDs".to_string());
        }
        if let Some(updated_at) = self.updated_at.as_mut() {
            *updated_at = updated_at.trim().to_string();
            if updated_at.is_empty()
                || updated_at.len() > 64
                || updated_at.chars().any(char::is_control)
            {
                return Err("cat activity policy updated_at is invalid".to_string());
            }
        }
        Ok(self)
    }

    pub fn camera_disabled(&self, camera_id: &str) -> bool {
        self.disabled_camera_ids
            .binary_search_by(|item| item.as_str().cmp(camera_id))
            .is_ok()
    }
}

#[derive(Debug, Clone)]
pub struct CatActivityPolicyStore {
    path: PathBuf,
    secure_path: Arc<SecureStorePath>,
    operation_lock: Arc<Mutex<()>>,
    bound_data_identity: Arc<Mutex<Option<SecureFileIdentity>>>,
}

impl Default for CatActivityPolicyStore {
    fn default() -> Self {
        Self::new(default_policy_path())
    }
}

impl CatActivityPolicyStore {
    pub fn new(path: PathBuf) -> Self {
        Self::try_new(path).unwrap_or_else(|error| {
            panic!("failed to initialize cat activity policy store: {error}")
        })
    }

    pub fn try_new(path: PathBuf) -> Result<Self, String> {
        let lock_path = path.with_extension("lock");
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

    pub fn load(&self) -> Result<CatActivityPolicy, String> {
        self.with_lock(|| self.load_unlocked())
    }

    pub fn save(&self, policy: CatActivityPolicy) -> Result<CatActivityPolicy, String> {
        let policy = policy.normalize()?;
        self.with_lock(|| {
            let bytes = serde_json::to_vec(&policy)
                .map_err(|error| format!("failed to serialize cat activity policy: {error}"))?;
            if bytes.len() as u64 > MAX_POLICY_BYTES {
                return Err("cat activity policy exceeds size limit".to_string());
            }
            self.secure_path
                .replace_data_atomically(&bytes, || Ok(()))
                .map_err(|error| {
                    format!(
                        "failed to atomically replace cat activity policy {}: {error}",
                        self.path.display()
                    )
                })?;
            Ok(policy)
        })
    }

    fn with_lock<T>(&self, action: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let _operation_guard = self
            .operation_lock
            .lock()
            .map_err(|_| "cat activity policy operation lock is poisoned".to_string())?;
        let lock_file = self.secure_path.open_lock()?;
        lock_file.lock_exclusive().map_err(|error| {
            format!(
                "failed to acquire cat activity policy lock for {}: {error}",
                self.path.display()
            )
        })?;
        let result = (|| {
            self.secure_path.ensure_lock_identity()?;
            self.refresh_bound_data_identity()?;
            let value = action()?;
            self.rebind_data_identity()?;
            Ok(value)
        })();
        let unlock_result = FileExt::unlock(&lock_file).map_err(|error| {
            format!(
                "failed to release cat activity policy lock for {}: {error}",
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
        let current = self
            .secure_path
            .open_data_read()?
            .map(|opened| opened.identity);
        *self
            .bound_data_identity
            .lock()
            .map_err(|_| "cat activity policy data identity lock is poisoned".to_string())? =
            current;
        Ok(())
    }

    fn rebind_data_identity(&self) -> Result<(), String> {
        let current = self
            .secure_path
            .open_data_read()?
            .map(|opened| opened.identity);
        *self
            .bound_data_identity
            .lock()
            .map_err(|_| "cat activity policy data identity lock is poisoned".to_string())? =
            current;
        Ok(())
    }

    fn load_unlocked(&self) -> Result<CatActivityPolicy, String> {
        let Some(mut opened) = self.secure_path.open_data_read()? else {
            return Ok(CatActivityPolicy::default());
        };
        if opened.len == 0 || opened.len > MAX_POLICY_BYTES {
            return Err("cat activity policy has an invalid size".to_string());
        }
        let mut bytes = Vec::with_capacity(opened.len as usize);
        opened.file.read_to_end(&mut bytes).map_err(|error| {
            format!(
                "failed to read cat activity policy {}: {error}",
                self.path.display()
            )
        })?;
        serde_json::from_slice::<CatActivityPolicy>(&bytes)
            .map_err(|error| format!("cat activity policy is invalid: {error}"))?
            .normalize()
    }
}

pub fn default_policy_path() -> PathBuf {
    env::var_os(CAT_ACTIVITY_POLICY_PATH_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_POLICY_PATH))
}

fn validate_camera_id(camera_id: &str) -> Result<(), String> {
    if camera_id.is_empty()
        || camera_id.len() > 128
        || camera_id.chars().any(char::is_control)
        || camera_id
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '?' | '#'))
    {
        return Err("cat activity policy contains an invalid camera ID".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use uuid::Uuid;

    use super::{CatActivityMode, CatActivityPolicy, CatActivityPolicyStore};

    fn store(name: &str) -> (std::path::PathBuf, CatActivityPolicyStore) {
        let root = std::env::temp_dir().join(format!(
            "harborbeacon-cat-policy-{name}-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).expect("policy test root");
        let store = CatActivityPolicyStore::new(root.join("policy.json"));
        (root, store)
    }

    #[test]
    fn missing_policy_defaults_to_all_enabled() {
        let (root, store) = store("default");
        assert_eq!(
            store.load().expect("default policy"),
            CatActivityPolicy::default()
        );
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn policy_round_trip_sorts_camera_ids() {
        let (root, store) = store("round-trip");
        let saved = store
            .save(CatActivityPolicy {
                schema_version: 1,
                mode: CatActivityMode::Off,
                disabled_camera_ids: vec!["camera.b".to_string(), "camera.a".to_string()],
                updated_at: Some("2026-08-17T00:00:00Z".to_string()),
            })
            .expect("save policy");
        assert_eq!(
            saved.disabled_camera_ids,
            vec!["camera.a".to_string(), "camera.b".to_string()]
        );
        assert_eq!(store.load().expect("load policy"), saved);
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn policy_rejects_duplicate_or_path_like_camera_ids() {
        for camera_ids in [
            vec!["camera.a".to_string(), "camera.a".to_string()],
            vec!["../camera".to_string()],
        ] {
            let error = CatActivityPolicy {
                schema_version: 1,
                mode: CatActivityMode::AllEnabled,
                disabled_camera_ids: camera_ids,
                updated_at: None,
            }
            .normalize()
            .expect_err("invalid camera IDs must fail");
            assert!(error.contains("camera"));
        }
    }
}
