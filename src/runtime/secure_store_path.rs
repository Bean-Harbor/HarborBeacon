use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

#[cfg(windows)]
use atomicwrites::{AllowOverwrite, AtomicFile, DisallowOverwrite};
use cap_fs_ext::{FollowSymlinks, MetadataExt as _, OpenOptionsFollowExt as _};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use cap_std::io_lifetimes::AsFilelike;
#[cfg(not(windows))]
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SecureFileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

#[derive(Debug)]
pub(crate) struct SecureOpenedFile {
    pub(crate) file: File,
    pub(crate) identity: SecureFileIdentity,
    pub(crate) len: u64,
    pub(crate) modified_epoch_nanos: Option<u128>,
}

struct BoundDirectory {
    parent: Dir,
    name: OsString,
    identity: SecureFileIdentity,
}

pub(crate) struct SecureStorePath {
    data_path: PathBuf,
    parent_path: PathBuf,
    data_name: OsString,
    lock_name: OsString,
    parent: Dir,
    parent_identity: SecureFileIdentity,
    ancestor_bindings: Vec<BoundDirectory>,
    lock_file: Option<File>,
    lock_identity: SecureFileIdentity,
}

impl fmt::Debug for SecureStorePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecureStorePath")
            .field("data_path", &self.data_path)
            .field("parent_path", &self.parent_path)
            .field("data_name", &self.data_name)
            .field("lock_name", &self.lock_name)
            .finish_non_exhaustive()
    }
}

impl SecureStorePath {
    pub(crate) fn try_new(data_path: PathBuf, lock_path: PathBuf) -> Result<Self, String> {
        let data_path = absolute_store_path(data_path)?;
        let lock_path = absolute_store_path(lock_path)?;
        let parent_path = data_path
            .parent()
            .ok_or_else(|| "durable store path must have a parent directory".to_string())?
            .to_path_buf();
        if lock_path.parent() != Some(parent_path.as_path()) {
            return Err("durable store data and lock must share one parent directory".to_string());
        }
        let data_name = single_file_name(&data_path)?.to_os_string();
        let lock_name = single_file_name(&lock_path)?.to_os_string();
        let (parent, parent_identity, ancestor_bindings) = bind_directory_chain(&parent_path)?;
        let secure = Self {
            data_path,
            parent_path,
            data_name,
            lock_name,
            parent,
            parent_identity,
            ancestor_bindings,
            lock_file: None,
            lock_identity: SecureFileIdentity {
                device: 0,
                inode: 0,
            },
        };
        let mut secure = secure;
        secure.ensure_parent_identity()?;
        secure.reject_untrusted_existing_entry(&secure.data_name, "data")?;
        secure.reject_untrusted_existing_entry(&secure.lock_name, "lock")?;
        let opened_lock = secure.open_relative(
            &secure.lock_name,
            OpenOptions::new().read(true).write(true).create(true),
            "lock",
        )?;
        secure.lock_identity = opened_lock.identity;
        secure.lock_file = Some(opened_lock.file);
        Ok(secure)
    }

    pub(crate) fn data_path(&self) -> &Path {
        &self.data_path
    }

    pub(crate) fn data_file_name(&self) -> &OsStr {
        &self.data_name
    }

    pub(crate) fn sibling_names(&self) -> Result<Vec<OsString>, String> {
        self.ensure_parent_identity()?;
        self.parent
            .entries()
            .map_err(|error| format!("failed to list durable store parent: {error}"))?
            .map(|entry| {
                entry
                    .map(|entry| entry.file_name())
                    .map_err(|error| format!("failed to read durable store parent entry: {error}"))
            })
            .collect()
    }

    pub(crate) fn ensure_parent_identity(&self) -> Result<(), String> {
        for binding in &self.ancestor_bindings {
            let (_, current_identity) = open_directory_nofollow(&binding.parent, &binding.name)
                .map_err(|error| {
                    format!(
                        "durable store ancestor was replaced or is unavailable {}: {error}",
                        self.parent_path.display()
                    )
                })?;
            if current_identity != binding.identity {
                return Err(format!(
                    "durable store ancestor identity was replaced: {}",
                    self.parent_path.display()
                ));
            }
        }
        let bound_identity =
            identity_from_metadata(&self.parent.dir_metadata().map_err(|error| {
                format!(
                    "failed to inspect bound durable store parent {}: {error}",
                    self.parent_path.display()
                )
            })?);
        if bound_identity != self.parent_identity {
            return Err(format!(
                "durable store parent identity was replaced: {}",
                self.parent_path.display()
            ));
        }
        Ok(())
    }

    pub(crate) fn open_lock(&self) -> Result<File, String> {
        self.ensure_parent_identity()?;
        self.ensure_lock_identity()?;
        self.lock_file
            .as_ref()
            .ok_or_else(|| "durable store bound lock is unavailable".to_string())?
            .try_clone()
            .map_err(|error| format!("failed to clone durable store bound lock: {error}"))
    }

    pub(crate) fn ensure_lock_identity(&self) -> Result<(), String> {
        self.ensure_parent_identity()?;
        let opened = self.open_relative(
            &self.lock_name,
            OpenOptions::new().read(true).write(true),
            "lock",
        )?;
        if opened.identity != self.lock_identity {
            return Err(format!(
                "durable store lock identity was replaced: {}",
                self.parent_path.join(&self.lock_name).display()
            ));
        }
        Ok(())
    }

    pub(crate) fn open_data_read(&self) -> Result<Option<SecureOpenedFile>, String> {
        self.open_sibling_read(&self.data_name)
    }

    pub(crate) fn open_sibling_read(
        &self,
        name: &OsStr,
    ) -> Result<Option<SecureOpenedFile>, String> {
        ensure_single_relative_name(name)?;
        self.ensure_parent_identity()?;
        match self.open_relative(name, OpenOptions::new().read(true), "data") {
            Ok(opened) => Ok(Some(opened)),
            Err(error) if error.contains("not found") => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn open_data_read_write(&self) -> Result<Option<SecureOpenedFile>, String> {
        self.ensure_parent_identity()?;
        match self.open_relative(
            &self.data_name,
            OpenOptions::new().read(true).write(true),
            "data",
        ) {
            Ok(opened) => Ok(Some(opened)),
            Err(error) if error.contains("not found") => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn open_data_append_create(&self) -> Result<SecureOpenedFile, String> {
        self.ensure_parent_identity()?;
        self.open_relative(
            &self.data_name,
            OpenOptions::new().read(true).append(true).create(true),
            "data",
        )
    }

    pub(crate) fn replace_data_atomically(
        &self,
        bytes: &[u8],
        before_commit: impl FnOnce() -> io::Result<()>,
    ) -> Result<SecureFileIdentity, String> {
        self.ensure_parent_identity()?;
        self.reject_untrusted_existing_entry(&self.data_name, "data")?;
        self.replace_data_atomically_for_platform(bytes, before_commit)?;
        self.ensure_parent_identity()?;
        self.open_data_read()?
            .map(|opened| opened.identity)
            .ok_or_else(|| "durable store data disappeared after replace".to_string())
    }

    pub(crate) fn create_sibling_atomically(
        &self,
        name: &OsStr,
        bytes: &[u8],
        before_commit: impl FnOnce() -> io::Result<()>,
    ) -> Result<SecureFileIdentity, String> {
        ensure_single_relative_name(name)?;
        self.ensure_parent_identity()?;
        match self.parent.symlink_metadata(name) {
            Ok(_) => return Err("durable store data already exists".to_string()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "failed to inspect durable store data before create: {error}"
                ))
            }
        }
        self.create_sibling_atomically_for_platform(name, bytes, before_commit)?;
        self.ensure_parent_identity()?;
        self.open_sibling_read(name)?
            .map(|opened| opened.identity)
            .ok_or_else(|| "durable store data disappeared after create".to_string())
    }

    #[cfg(windows)]
    fn replace_data_atomically_for_platform(
        &self,
        bytes: &[u8],
        before_commit: impl FnOnce() -> io::Result<()>,
    ) -> Result<(), String> {
        AtomicFile::new(&self.data_path, AllowOverwrite)
            .write(|file| -> io::Result<()> {
                file.write_all(bytes)?;
                file.sync_all()?;
                before_commit()
            })
            .map_err(io::Error::from)
            .map_err(|error| format!("failed to replace durable store data: {error}"))
    }

    #[cfg(windows)]
    fn create_sibling_atomically_for_platform(
        &self,
        name: &OsStr,
        bytes: &[u8],
        before_commit: impl FnOnce() -> io::Result<()>,
    ) -> Result<(), String> {
        AtomicFile::new(self.parent_path.join(name), DisallowOverwrite)
            .write(|file| -> io::Result<()> {
                file.write_all(bytes)?;
                file.sync_all()?;
                before_commit()
            })
            .map_err(io::Error::from)
            .map_err(|error| format!("failed to create durable store data: {error}"))
    }

    #[cfg(not(windows))]
    fn replace_data_atomically_for_platform(
        &self,
        bytes: &[u8],
        before_commit: impl FnOnce() -> io::Result<()>,
    ) -> Result<(), String> {
        let temporary_name = OsString::from(format!(
            ".{}.{}.tmp",
            self.data_name.to_string_lossy(),
            Uuid::new_v4().simple()
        ));
        let result = (|| -> Result<(), String> {
            let mut temporary = self.open_relative(
                &temporary_name,
                OpenOptions::new().read(true).write(true).create_new(true),
                "temporary data",
            )?;
            temporary
                .file
                .write_all(bytes)
                .and_then(|_| temporary.file.sync_all())
                .and_then(|_| before_commit())
                .map_err(|error| {
                    format!("failed to persist durable store temporary data: {error}")
                })?;
            self.parent
                .rename(&temporary_name, &self.parent, &self.data_name)
                .map_err(|error| format!("failed to replace durable store data: {error}"))?;
            self.open_parent_directory_for_sync()?
                .sync_all()
                .map_err(|error| format!("failed to sync durable store parent: {error}"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = self.parent.remove_file(&temporary_name);
        }
        result
    }

    #[cfg(not(windows))]
    fn create_sibling_atomically_for_platform(
        &self,
        name: &OsStr,
        bytes: &[u8],
        before_commit: impl FnOnce() -> io::Result<()>,
    ) -> Result<(), String> {
        let temporary_name = OsString::from(format!(
            ".{}.{}.tmp",
            name.to_string_lossy(),
            Uuid::new_v4().simple()
        ));
        let result = (|| -> Result<(), String> {
            let mut temporary = self.open_relative(
                &temporary_name,
                OpenOptions::new().read(true).write(true).create_new(true),
                "temporary data",
            )?;
            temporary
                .file
                .write_all(bytes)
                .and_then(|_| temporary.file.sync_all())
                .and_then(|_| before_commit())
                .map_err(|error| {
                    format!("failed to persist durable store temporary data: {error}")
                })?;
            rustix::fs::renameat_with(
                &self.parent,
                Path::new(&temporary_name),
                &self.parent,
                Path::new(name),
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(|error| {
                format!("failed to publish durable store data without overwrite: {error}")
            })?;
            self.open_parent_directory_for_sync()?
                .sync_all()
                .map_err(|error| format!("failed to sync durable store parent: {error}"))
        })();
        if result.is_err() {
            let _ = self.parent.remove_file(&temporary_name);
        }
        result
    }

    #[cfg(not(windows))]
    fn open_parent_directory_for_sync(&self) -> Result<File, String> {
        let descriptor = rustix::fs::openat(
            &self.parent,
            Path::new("."),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| format!("failed to open durable store parent for sync: {error}"))?;
        Ok(File::from(descriptor))
    }

    fn reject_untrusted_existing_entry(&self, name: &OsStr, role: &str) -> Result<(), String> {
        match self.parent.symlink_metadata(name) {
            Ok(metadata) => validate_metadata(&metadata, role),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("failed to inspect durable store {role}: {error}")),
        }
    }

    fn open_relative(
        &self,
        name: &OsStr,
        options: &mut OpenOptions,
        role: &str,
    ) -> Result<SecureOpenedFile, String> {
        options.follow(FollowSymlinks::No);
        let file = self.parent.open_with(name, options).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                format!("durable store {role} not found")
            } else {
                format!("failed to open durable store {role} without symlink following: {error}")
            }
        })?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("failed to inspect open durable store {role}: {error}"))?;
        validate_metadata(&metadata, role)?;
        let identity = identity_from_metadata(&metadata);
        let len = metadata.len();
        let modified_epoch_nanos = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.into_std().duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        Ok(SecureOpenedFile {
            file: file.into_std(),
            identity,
            len,
            modified_epoch_nanos,
        })
    }
}

fn bind_directory_chain(
    parent_path: &Path,
) -> Result<(Dir, SecureFileIdentity, Vec<BoundDirectory>), String> {
    let (root_path, components) = absolute_root_and_components(parent_path)?;
    let mut current = Dir::open_ambient_dir(&root_path, ambient_authority()).map_err(|error| {
        format!(
            "failed to bind durable store filesystem root {}: {error}",
            root_path.display()
        )
    })?;
    let root_metadata = current.dir_metadata().map_err(|error| {
        format!(
            "failed to inspect durable store filesystem root {}: {error}",
            root_path.display()
        )
    })?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err("durable store filesystem root must be a trusted directory".to_string());
    }

    let mut ancestor_bindings = Vec::with_capacity(components.len());
    for component in components {
        let (next, identity) = match open_directory_nofollow(&current, &component) {
            Ok(opened) => opened,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match current.create_dir(&component) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(format!(
                            "failed to create durable store directory {}: {error}",
                            parent_path.display()
                        ))
                    }
                }
                open_directory_nofollow(&current, &component).map_err(|error| {
                    format!(
                        "failed to bind newly created durable store directory {}: {error}",
                        parent_path.display()
                    )
                })?
            }
            Err(error) => {
                return Err(format!(
                    "durable store ancestor alias or reparse path is rejected {}: {error}",
                    parent_path.display()
                ))
            }
        };
        ancestor_bindings.push(BoundDirectory {
            parent: current,
            name: component,
            identity,
        });
        current = next;
    }
    let parent_identity = identity_from_metadata(&current.dir_metadata().map_err(|error| {
        format!(
            "failed to inspect durable store parent {}: {error}",
            parent_path.display()
        )
    })?);
    Ok((current, parent_identity, ancestor_bindings))
}

fn open_directory_nofollow(parent: &Dir, name: &OsStr) -> io::Result<(Dir, SecureFileIdentity)> {
    ensure_single_relative_name(name)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let opened =
        cap_primitives::fs::open_dir_nofollow(&parent.as_filelike_view::<File>(), Path::new(name))?;
    let opened = Dir::from_std_file(opened);
    let metadata = opened.dir_metadata()?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "directory alias or reparse point is rejected",
        ));
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "durable store ancestor must be a directory",
        ));
    }
    let identity = identity_from_metadata(&metadata);
    Ok((opened, identity))
}

fn absolute_root_and_components(path: &Path) -> Result<(PathBuf, Vec<OsString>), String> {
    let mut root = PathBuf::new();
    let mut components = Vec::new();
    let mut reached_normal = false;
    let mut saw_root = false;
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir if !reached_normal => {
                root.push(component.as_os_str());
                if matches!(component, Component::RootDir) {
                    saw_root = true;
                }
            }
            Component::Normal(name) => {
                reached_normal = true;
                components.push(name.to_os_string());
            }
            Component::CurDir | Component::ParentDir => {
                return Err(
                    "durable store path aliases using dot components are rejected".to_string(),
                )
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err("durable store path has an invalid absolute component order".to_string())
            }
        }
    }
    if !saw_root || root.as_os_str().is_empty() {
        return Err("durable store path must be absolute".to_string());
    }
    Ok((root, components))
}

fn validate_metadata(metadata: &cap_std::fs::Metadata, role: &str) -> Result<(), String> {
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "durable store {role} symlink or reparse point is rejected"
        ));
    }
    if !metadata.is_file() {
        return Err(format!("durable store {role} must be a regular file"));
    }
    if metadata.nlink() != 1 {
        return Err(format!("durable store {role} hardlink alias is rejected"));
    }
    Ok(())
}

fn identity_from_metadata(metadata: &cap_std::fs::Metadata) -> SecureFileIdentity {
    SecureFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

fn single_file_name(path: &Path) -> Result<&OsStr, String> {
    path.file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "durable store path must end in one file name".to_string())
}

fn ensure_single_relative_name(name: &OsStr) -> Result<(), String> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if !component.is_empty() => Ok(()),
        _ => Err("durable store sibling must be one relative file name".to_string()),
    }
}

fn absolute_store_path(path: PathBuf) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| format!("failed to resolve durable store working directory: {error}"))?
            .join(path)
    };
    if absolute
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err("durable store path aliases using dot components are rejected".to_string());
    }
    Ok(absolute)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::ffi::OsStr;
    use std::fs;
    use std::path::{Path, PathBuf};
    #[cfg(windows)]
    use std::process::Command;

    use uuid::Uuid;

    use super::SecureStorePath;

    fn temporary_test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "harborbeacon-secure-store-{name}-{}-{}",
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

    #[test]
    fn atomic_replace_source_keeps_platform_durability_contract() {
        let source = include_str!("secure_store_path.rs");
        assert!(source.contains("#[cfg(windows)]"));
        assert!(source.contains("AtomicFile::new(&self.data_path, AllowOverwrite)"));
        assert!(source.contains("AtomicFile::new(self.parent_path.join(name), DisallowOverwrite)"));
        assert!(source.contains("#[cfg(not(windows))]"));
        assert!(source.contains("self.parent\n                .rename"));
        assert!(source.contains("rustix::fs::RenameFlags::NOREPLACE"));
        assert!(source.contains("failed to sync durable store parent"));
    }

    #[test]
    fn construction_binds_each_ancestor_without_ambient_parent_resolution() {
        let source = include_str!("secure_store_path.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source");
        assert!(!source.contains("std::fs::create_dir_all(&parent_path)"));
        assert!(!source.contains("std::fs::canonicalize(&parent_path)"));
        assert!(source.contains("open_directory_nofollow"));
        assert!(source.contains("ancestor_bindings"));
    }

    #[test]
    fn construction_rejects_preexisting_ancestor_alias() {
        let root = temporary_test_root("ancestor-alias");
        let trusted = root.join("trusted");
        let alias = root.join("alias");
        fs::create_dir_all(trusted.join("state")).expect("trusted state directory");
        create_directory_alias(&alias, &trusted);

        let error = SecureStorePath::try_new(
            alias.join("state").join("store.jsonl"),
            alias.join("state").join("store.lock"),
        )
        .expect_err("ancestor alias must be rejected");

        assert!(
            error.contains("alias") || error.contains("reparse"),
            "{error}"
        );
        assert!(!trusted.join("state").join("store.jsonl").exists());
    }

    #[test]
    fn higher_ancestor_swap_cannot_redirect_bound_store_mutation() {
        let root = temporary_test_root("ancestor-swap");
        let trusted_ancestor = root.join("state-root");
        let moved_ancestor = root.join("state-root-original");
        let attacker_ancestor = root.join("attacker-root");
        let trusted_parent = trusted_ancestor.join("nested").join("ledger");
        let attacker_parent = attacker_ancestor.join("nested").join("ledger");
        fs::create_dir_all(&trusted_parent).expect("trusted parent");
        fs::create_dir_all(&attacker_parent).expect("attacker parent");
        let data_path = trusted_parent.join("store.jsonl");
        let lock_path = trusted_parent.join("store.lock");
        fs::write(&data_path, b"original-ledger\n").expect("original ledger");
        fs::write(attacker_parent.join("store.jsonl"), b"external-marker\n")
            .expect("external marker");
        let secure = SecureStorePath::try_new(data_path.clone(), lock_path)
            .expect("bind trusted ancestor chain");

        if fs::rename(&trusted_ancestor, &moved_ancestor).is_err() {
            assert_eq!(
                fs::read(&data_path).expect("original ledger"),
                b"original-ledger\n"
            );
            assert_eq!(
                fs::read(attacker_parent.join("store.jsonl")).expect("external marker"),
                b"external-marker\n"
            );
            return;
        }
        fs::rename(&attacker_ancestor, &trusted_ancestor)
            .expect("attacker ancestor should replace trusted name");

        let error = secure
            .replace_data_atomically(b"redirected\n", || Ok(()))
            .expect_err("ancestor replacement must fail closed");

        assert!(
            error.contains("ancestor") || error.contains("parent") || error.contains("identity")
        );
        assert_eq!(
            fs::read(moved_ancestor.join("nested/ledger/store.jsonl"))
                .expect("original ledger after move"),
            b"original-ledger\n"
        );
        assert_eq!(
            fs::read(trusted_ancestor.join("nested/ledger/store.jsonl"))
                .expect("external marker after replacement"),
            b"external-marker\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_parent_directory_fsync_completes_capability_relative_replace() {
        let root = std::env::temp_dir().join(format!(
            "harborbeacon-secure-replace-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).expect("secure replace parent");
        let data_path = root.join("store.json");
        let lock_path = root.join("store.lock");
        let marker_path = root.join("external.marker");
        fs::write(&data_path, b"original-main").expect("original main");
        fs::write(&marker_path, b"external-marker").expect("external marker");
        let secure = SecureStorePath::try_new(data_path.clone(), lock_path)
            .expect("bind secure replace store");

        secure
            .replace_data_atomically(b"committed-main", || Ok(()))
            .expect("capability-relative replace and parent fsync");

        assert_eq!(
            fs::read(data_path).expect("committed main"),
            b"committed-main"
        );
        assert_eq!(
            fs::read(marker_path).expect("unchanged external marker"),
            b"external-marker"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_parent_directory_fsync_completes_noreplace_and_preserves_collision() {
        let root = std::env::temp_dir().join(format!(
            "harborbeacon-secure-noreplace-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).expect("secure noreplace parent");
        let data_path = root.join("store.json");
        let lock_path = root.join("store.lock");
        let marker_path = root.join("external.marker");
        fs::write(&marker_path, b"external-marker").expect("external marker");
        let secure =
            SecureStorePath::try_new(data_path, lock_path).expect("bind secure noreplace store");
        let archive_name = OsStr::new("store.json.archive.00000000000000000001.jsonl");

        secure
            .create_sibling_atomically(archive_name, b"original-archive\n", || Ok(()))
            .expect("capability-relative noreplace and parent fsync");
        let collision = secure
            .create_sibling_atomically(archive_name, b"replacement-archive\n", || Ok(()))
            .expect_err("noreplace collision must fail");

        assert!(collision.contains("already exists"), "{collision}");
        assert_eq!(
            fs::read(root.join(archive_name)).expect("original archive"),
            b"original-archive\n"
        );
        assert_eq!(
            fs::read(marker_path).expect("unchanged external marker"),
            b"external-marker"
        );
    }
}
