//! Durable active-state ledger for cat recording reconciliation.

use std::collections::BTreeMap;
use std::env;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::runtime::cat_recording_validation::CatDetectionEvidence;
use crate::runtime::secure_store_path::{SecureFileIdentity, SecureStorePath};

pub const CAT_RECORDING_RECONCILIATION_PATH_ENV: &str =
    "HARBOR_K3_CAT_RECORDING_RECONCILIATION_PATH";
const DEFAULT_RECONCILIATION_PATH: &str = ".harborbeacon/cat-recording-reconciliation.json";
const MAX_RECONCILIATION_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatRecordingReconciliationPhase {
    PendingStart,
    #[default]
    Active,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatRecordingReconciliationState {
    pub camera_id: String,
    #[serde(default)]
    pub phase: CatRecordingReconciliationPhase,
    #[serde(default)]
    pub created_at_epoch_ms: u128,
    #[serde(default)]
    pub last_sequence: Option<u64>,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub lease_id: Option<String>,
    #[serde(default)]
    pub stream_profile: Option<String>,
    #[serde(default)]
    pub last_renewed_epoch_ms: u128,
    #[serde(default)]
    pub detection_evidence: Vec<CatDetectionEvidence>,
}

#[derive(Debug, Clone)]
pub struct CatRecordingReconciliationStore {
    path: PathBuf,
    secure_path: Arc<SecureStorePath>,
    bound_data_identity: Arc<Mutex<Option<SecureFileIdentity>>>,
}

impl Default for CatRecordingReconciliationStore {
    fn default() -> Self {
        Self::new(default_reconciliation_path())
    }
}

impl CatRecordingReconciliationStore {
    pub fn new(path: PathBuf) -> Self {
        Self::try_new(path).unwrap_or_else(|error| {
            panic!("failed to initialize cat recording reconciliation store: {error}")
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
            bound_data_identity: Arc::new(Mutex::new(bound_data_identity)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<BTreeMap<String, CatRecordingReconciliationState>, String> {
        self.with_lock(|| self.load_unlocked())
    }

    pub fn upsert(&self, state: CatRecordingReconciliationState) -> Result<(), String> {
        if state.camera_id.trim().is_empty() {
            return Err("cat recording reconciliation camera_id is required".to_string());
        }
        self.with_lock(|| {
            let mut states = self.load_unlocked()?;
            states.insert(state.camera_id.clone(), state);
            self.write_unlocked(&states)
        })
    }

    pub fn remove(&self, camera_id: &str) -> Result<(), String> {
        self.with_lock(|| {
            let mut states = self.load_unlocked()?;
            if states.remove(camera_id).is_some() {
                self.write_unlocked(&states)?;
            }
            Ok(())
        })
    }

    fn with_lock<T>(&self, action: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let lock_file = self.secure_path.open_lock()?;
        lock_file.lock_exclusive().map_err(|error| {
            format!(
                "failed to acquire cat reconciliation lock for {}: {error}",
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
                "failed to release cat reconciliation lock for {}: {error}",
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
            .map_err(|_| "cat reconciliation data identity lock is poisoned".to_string())? =
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
            .map_err(|_| "cat reconciliation data identity lock is poisoned".to_string())? =
            current;
        Ok(())
    }

    fn load_unlocked(&self) -> Result<BTreeMap<String, CatRecordingReconciliationState>, String> {
        let Some(mut opened) = self.secure_path.open_data_read()? else {
            return Ok(BTreeMap::new());
        };
        if opened.len > MAX_RECONCILIATION_BYTES {
            return Err("cat reconciliation store exceeds size limit".to_string());
        }
        let mut bytes = Vec::with_capacity(opened.len as usize);
        opened.file.read_to_end(&mut bytes).map_err(|error| {
            format!(
                "failed to read cat reconciliation store {}: {error}",
                self.path.display()
            )
        })?;
        if bytes.is_empty() {
            return Err("cat reconciliation store is empty".to_string());
        }
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("cat reconciliation store is invalid: {error}"))
    }

    fn write_unlocked(
        &self,
        states: &BTreeMap<String, CatRecordingReconciliationState>,
    ) -> Result<(), String> {
        let bytes = serde_json::to_vec(states)
            .map_err(|error| format!("failed to serialize cat reconciliation state: {error}"))?;
        if bytes.len() as u64 > MAX_RECONCILIATION_BYTES {
            return Err("cat reconciliation store exceeds size limit".to_string());
        }
        self.secure_path
            .replace_data_atomically(&bytes, || Ok(()))
            .map(|_| ())
            .map_err(|error| {
                format!(
                    "failed to atomically replace cat reconciliation store {}: {error}",
                    self.path.display()
                )
            })
    }
}

pub fn default_reconciliation_path() -> PathBuf {
    env::var_os(CAT_RECORDING_RECONCILIATION_PATH_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_RECONCILIATION_PATH))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    #[cfg(windows)]
    use std::process::Command;

    use uuid::Uuid;

    use super::{CatRecordingReconciliationState, CatRecordingReconciliationStore};

    fn temporary_test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "harborbeacon-cat-reconciliation-{name}-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ))
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
    fn create_directory_alias(alias: &Path, target: &Path) {
        std::os::unix::fs::symlink(target, alias).expect("directory symlink should be created");
    }

    fn state(camera_id: &str) -> CatRecordingReconciliationState {
        CatRecordingReconciliationState {
            camera_id: camera_id.to_string(),
            ..CatRecordingReconciliationState::default()
        }
    }

    #[test]
    fn reconciliation_store_rejects_directory_alias_without_changing_main() {
        let root = temporary_test_root("alias");
        let trusted = root.join("trusted");
        let alias = root.join("alias");
        fs::create_dir_all(&trusted).expect("trusted directory should be created");
        let trusted_path = trusted.join("reconciliation.json");
        let store = CatRecordingReconciliationStore::try_new(trusted_path.clone())
            .expect("trusted reconciliation store should open");
        store
            .upsert(state("camera-trusted"))
            .expect("trusted state should persist");
        let original = fs::read(&trusted_path).expect("trusted bytes should exist");
        create_directory_alias(&alias, &trusted);

        let error = CatRecordingReconciliationStore::try_new(alias.join("reconciliation.json"))
            .expect_err("directory alias must be rejected");

        assert!(error.contains("alias") || error.contains("reparse"));
        assert_eq!(
            fs::read(&trusted_path).expect("trusted bytes should remain readable"),
            original
        );
    }

    #[test]
    fn reconciliation_store_rejects_parent_swap_after_capability_binding() {
        let root = temporary_test_root("parent-swap");
        let trusted = root.join("trusted");
        let moved = root.join("trusted-original");
        let attacker = root.join("attacker");
        fs::create_dir_all(&trusted).expect("trusted directory should be created");
        fs::create_dir_all(&attacker).expect("attacker directory should be created");
        let trusted_path = trusted.join("reconciliation.json");
        let store = CatRecordingReconciliationStore::try_new(trusted_path.clone())
            .expect("trusted reconciliation store should open");
        store
            .upsert(state("camera-trusted"))
            .expect("trusted state should persist");
        let original = fs::read(&trusted_path).expect("trusted bytes should exist");
        let attacker_path = attacker.join("reconciliation.json");
        fs::write(&attacker_path, b"attacker-owned").expect("attacker bytes should persist");

        if fs::rename(&trusted, &moved).is_err() {
            assert_eq!(
                fs::read(&trusted_path).expect("trusted bytes should remain in place"),
                original
            );
            assert_eq!(
                fs::read(&attacker_path).expect("attacker bytes should remain in place"),
                b"attacker-owned"
            );
            return;
        }
        fs::rename(&attacker, &trusted).expect("attacker directory should replace trusted path");

        let error = store
            .upsert(state("camera-second"))
            .expect_err("parent replacement must be rejected");

        assert!(error.contains("parent") || error.contains("identity"));
        assert_eq!(
            fs::read(trusted.join("reconciliation.json"))
                .expect("attacker bytes should remain readable"),
            b"attacker-owned"
        );
        assert_eq!(
            fs::read(moved.join("reconciliation.json"))
                .expect("original bytes should remain readable"),
            original
        );
    }

    #[test]
    fn reconciliation_revalidates_lock_identity_after_exclusive_acquire() {
        let source = include_str!("cat_recording_reconciliation.rs");
        let with_lock = source
            .split("fn with_lock<T>")
            .nth(1)
            .expect("with_lock source");
        let acquired = with_lock
            .find("lock_exclusive()")
            .expect("exclusive lock acquisition");
        let revalidated = with_lock
            .find("self.secure_path.ensure_lock_identity()?")
            .expect("lock identity revalidation");
        let data_check = with_lock
            .find("self.refresh_bound_data_identity()?")
            .expect("data identity refresh");
        assert!(acquired < revalidated && revalidated < data_check);
    }

    #[test]
    fn independently_constructed_stores_follow_legitimate_atomic_replacement() {
        let root = temporary_test_root("two-store-replace");
        let path = root.join("reconciliation.json");
        let first = CatRecordingReconciliationStore::try_new(path.clone())
            .expect("first reconciliation store");
        let second = CatRecordingReconciliationStore::try_new(path)
            .expect("second reconciliation store before main exists");

        first
            .upsert(state("camera-first"))
            .expect("first store should atomically create main");
        assert!(second
            .load()
            .expect("second store should refresh legitimate main identity")
            .contains_key("camera-first"));
        second
            .upsert(state("camera-second"))
            .expect("second store should atomically replace main");
        let latest = first
            .load()
            .expect("first store should refresh second replacement");
        assert!(latest.contains_key("camera-first"));
        assert!(latest.contains_key("camera-second"));
    }

    #[cfg(not(windows))]
    fn replace_data_with_alias(path: &Path, target: &Path) {
        fs::remove_file(path).expect("remove original reconciliation main");
        std::os::unix::fs::symlink(target, path).expect("replace main with symlink");
    }

    #[cfg(windows)]
    fn replace_data_with_alias(path: &Path, target: &Path) {
        fs::remove_file(path).expect("remove original reconciliation main");
        fs::create_dir_all(target).expect("attacker junction target");
        create_directory_alias(path, target);
    }

    #[test]
    fn replacement_refresh_still_rejects_tampered_main_alias() {
        let root = temporary_test_root("tampered-main-alias");
        let path = root.join("reconciliation.json");
        let attacker = root.join("attacker-target");
        let store =
            CatRecordingReconciliationStore::try_new(path.clone()).expect("reconciliation store");
        store
            .upsert(state("camera-original"))
            .expect("original reconciliation state");
        #[cfg(not(windows))]
        fs::write(&attacker, b"external-marker").expect("external marker");
        #[cfg(windows)]
        fs::create_dir_all(&attacker).expect("external marker directory");
        replace_data_with_alias(&path, &attacker);

        let error = store
            .load()
            .expect_err("tampered reconciliation main alias must be rejected");

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
}
