// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded serialization and no-clobber support-bundle publication on Unix
//! (Linux and macOS are the qualified targets) and Windows.
//!
//! Directory descriptors anchor every operation. The final directory must be
//! owned by the effective user and must not be writable by group or others;
//! traversed ancestors must be trusted too. This excludes other-user pathname
//! substitution, not sabotage by the same user or root. No directory is created,
//! no existing entry is replaced, and no foreign temporary entry is swept.

use std::io::{self, Write};
use std::path::Path;

use serde::Serialize;

pub(super) const MAX_ARTIFACT_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Unsupported,
    Cancelled,
    SizeLimit,
    UnsafeDestination,
    AlreadyExists,
    Io,
    Serialization,
}

#[derive(Debug)]
pub(super) struct ExportError {
    pub kind: Failure,
    pub temporary_cleanup: bool,
}

impl From<Failure> for ExportError {
    fn from(kind: Failure) -> Self {
        Self {
            kind,
            temporary_cleanup: true,
        }
    }
}

pub(super) struct Publication {
    pub temporary_cleanup: bool,
}

pub(super) fn supported() -> bool {
    cfg!(any(unix, windows))
}

/// The exact manifest, with one trailing newline. Bound serialized bytes before
/// creating any temporary file.
pub(super) fn serialize(
    value: &impl Serialize,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>, ExportError> {
    struct Bounded<'a> {
        bytes: Vec<u8>,
        failure: Option<Failure>,
        cancelled: &'a dyn Fn() -> bool,
    }
    impl Write for Bounded<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let failure = if (self.cancelled)() {
                Some(Failure::Cancelled)
            } else if bytes.len() > MAX_ARTIFACT_BYTES.saturating_sub(self.bytes.len()) {
                Some(Failure::SizeLimit)
            } else {
                None
            };
            if let Some(failure) = failure {
                self.failure = Some(failure);
                return Err(io::Error::other("support bundle serialization stopped"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded {
        bytes: Vec::new(),
        failure: None,
        cancelled,
    };
    if serde_json::to_writer(&mut output, value).is_err() || output.write_all(b"\n").is_err() {
        return Err(output.failure.unwrap_or(Failure::Serialization).into());
    }
    Ok(output.bytes)
}

pub(super) fn publish(
    destination: &Path,
    bytes: &[u8],
    cancelled: &dyn Fn() -> bool,
) -> Result<Publication, ExportError> {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(Failure::SizeLimit.into());
    }
    if cancelled() {
        return Err(Failure::Cancelled.into());
    }
    platform::publish(destination, bytes, cancelled)
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;

    pub(super) fn publish(
        _destination: &Path,
        _bytes: &[u8],
        _cancelled: &dyn Fn() -> bool,
    ) -> Result<Publication, ExportError> {
        Err(Failure::Unsupported.into())
    }
}

#[cfg(unix)]
mod platform {
    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::Component;
    use std::sync::atomic::{AtomicU64, Ordering};

    use nix::errno::Errno;
    use nix::fcntl::{AtFlags, OFlag, open, openat};
    use nix::sys::stat::{Mode, fstat, fstatat};
    use nix::unistd::{UnlinkatFlags, geteuid, linkat, unlinkat};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    const DIRECTORY_FLAGS: OFlag = OFlag::O_RDONLY
        .union(OFlag::O_DIRECTORY)
        .union(OFlag::O_NOFOLLOW)
        .union(OFlag::O_CLOEXEC);

    fn trusted_directory(directory: &File, final_directory: bool) -> Result<(), Failure> {
        let metadata = directory.metadata().map_err(|_| Failure::Io)?;
        let uid = geteuid().as_raw();
        let writable = metadata.mode() & 0o022 != 0;
        // A root-owned sticky ancestor (such as /tmp) protects the following
        // owned directory entry. It is never accepted as the output directory.
        let sticky_root = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        let trusted = if final_directory {
            metadata.uid() == uid && !writable
        } else {
            (metadata.uid() == uid || metadata.uid() == 0) && (!writable || sticky_root)
        };
        if trusted {
            Ok(())
        } else {
            Err(Failure::UnsafeDestination)
        }
    }

    fn anchor(destination: &Path) -> Result<(File, OsString), Failure> {
        if destination.as_os_str().as_bytes().len() > 4096 {
            return Err(Failure::UnsafeDestination);
        }
        let name = destination.file_name().ok_or(Failure::UnsafeDestination)?;
        let parent = destination.parent().ok_or(Failure::UnsafeDestination)?;
        let mut directory = File::from(
            open(
                if destination.is_absolute() { "/" } else { "." },
                DIRECTORY_FLAGS,
                Mode::empty(),
            )
            .map_err(|_| Failure::UnsafeDestination)?,
        );
        trusted_directory(&directory, false)?;
        for component in parent.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(part) => {
                    directory = File::from(
                        openat(&directory, part, DIRECTORY_FLAGS, Mode::empty())
                            .map_err(|_| Failure::UnsafeDestination)?,
                    );
                    trusted_directory(&directory, false)?;
                }
                Component::ParentDir | Component::Prefix(_) => {
                    return Err(Failure::UnsafeDestination);
                }
            }
        }
        trusted_directory(&directory, true)?;
        Ok((directory, name.to_os_string()))
    }

    struct Temporary {
        directory: File,
        file: File,
        name: OsString,
        // Raw platform types: `dev_t` is signed on macOS and unsigned on Linux.
        device: nix::libc::dev_t,
        inode: nix::libc::ino_t,
        cleanup_attempted: bool,
    }

    impl Temporary {
        fn create(directory: File) -> Result<Self, ExportError> {
            for _ in 0..16 {
                let next = NEXT_TEMP
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                    .map_err(|_| Failure::Io)?;
                let name = OsString::from(format!(
                    ".tungsten-support-{}-{next}.pending",
                    std::process::id()
                ));
                let descriptor = match openat(
                    &directory,
                    name.as_os_str(),
                    OFlag::O_WRONLY
                        | OFlag::O_CREAT
                        | OFlag::O_EXCL
                        | OFlag::O_NOFOLLOW
                        | OFlag::O_CLOEXEC,
                    Mode::S_IRUSR | Mode::S_IWUSR,
                ) {
                    Ok(descriptor) => descriptor,
                    Err(Errno::EEXIST) => continue,
                    Err(_) => return Err(Failure::Io.into()),
                };
                let file = File::from(descriptor);
                // fstat failure is exceptional: preserve the exclusively
                // created entry rather than unlink an unverified pathname.
                let metadata = fstat(&file).map_err(|_| ExportError {
                    kind: Failure::Io,
                    temporary_cleanup: false,
                })?;
                return Ok(Self {
                    directory,
                    file,
                    name,
                    device: metadata.st_dev,
                    inode: metadata.st_ino,
                    cleanup_attempted: false,
                });
            }
            Err(Failure::Io.into())
        }

        fn matches(&self) -> bool {
            fstatat(
                &self.directory,
                self.name.as_os_str(),
                AtFlags::AT_SYMLINK_NOFOLLOW,
            )
            .is_ok_and(|metadata| metadata.st_dev == self.device && metadata.st_ino == self.inode)
        }

        fn cleanup(&mut self) -> bool {
            self.cleanup_attempted = true;
            self.matches()
                && unlinkat(
                    &self.directory,
                    self.name.as_os_str(),
                    UnlinkatFlags::NoRemoveDir,
                )
                .is_ok()
        }

        fn failed(mut self, kind: Failure) -> ExportError {
            ExportError {
                kind,
                temporary_cleanup: self.cleanup(),
            }
        }
    }

    impl Drop for Temporary {
        fn drop(&mut self) {
            if !self.cleanup_attempted {
                self.cleanup();
            }
        }
    }

    pub(super) fn publish(
        destination: &Path,
        bytes: &[u8],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Publication, ExportError> {
        let (directory, name) = anchor(destination)?;
        match fstatat(&directory, name.as_os_str(), AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(_) => return Err(Failure::AlreadyExists.into()),
            Err(Errno::ENOENT) => {}
            Err(_) => return Err(Failure::Io.into()),
        }
        let mut temporary = Temporary::create(directory)?;
        for chunk in bytes.chunks(8192) {
            if cancelled() {
                return Err(temporary.failed(Failure::Cancelled));
            }
            if temporary.file.write_all(chunk).is_err() {
                return Err(temporary.failed(Failure::Io));
            }
        }
        if temporary.file.sync_all().is_err() {
            return Err(temporary.failed(Failure::Io));
        }
        if !temporary.matches()
            || !temporary.file.metadata().is_ok_and(|metadata| {
                metadata.is_file()
                    && metadata.nlink() == 1
                    && metadata.uid() == geteuid().as_raw()
                    && metadata.mode() & 0o077 == 0
            })
        {
            return Err(temporary.failed(Failure::UnsafeDestination));
        }
        if cancelled() {
            return Err(temporary.failed(Failure::Cancelled));
        }
        match linkat(
            &temporary.directory,
            temporary.name.as_os_str(),
            &temporary.directory,
            OsStr::new(&name),
            AtFlags::empty(),
        ) {
            Ok(()) => {
                // Publication is the commit point. Never delete the destination
                // or turn a late cancellation into an unpublished outcome.
                Ok(Publication {
                    temporary_cleanup: temporary.cleanup(),
                })
            }
            Err(error) => Err(temporary.failed(if error == Errno::EEXIST {
                Failure::AlreadyExists
            } else {
                Failure::Io
            })),
        }
    }
}

/// Windows has no descriptor-relative `openat` in safe Rust, so every traversed
/// directory is held open without `FILE_SHARE_DELETE`: while the handles live,
/// no component can be renamed, deleted or swapped for a junction. Reparse
/// points are refused, the temporary file is created with `CREATE_NEW` and
/// published with a hard link, which never replaces an existing entry. The
/// directory's owner and ACL are not inspected.
#[cfg(windows)]
mod platform {
    use std::ffi::OsString;
    use std::fs::{File, OpenOptions};
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::path::{Component, Path, PathBuf, Prefix};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

    fn pin(path: &Path) -> Result<File, Failure> {
        let directory = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | SYNCHRONIZE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|_| Failure::UnsafeDestination)?;
        let metadata = directory
            .metadata()
            .map_err(|_| Failure::UnsafeDestination)?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(Failure::UnsafeDestination);
        }
        Ok(directory)
    }

    fn safe_name(name: &str) -> bool {
        let stem = name.split('.').next().unwrap_or_default().trim_end();
        let device = RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r))
            || (stem.len() == 4
                && ["COM", "LPT"]
                    .iter()
                    .any(|r| stem.get(..3).is_some_and(|s| s.eq_ignore_ascii_case(r)))
                && stem.as_bytes()[3].is_ascii_digit());
        !device
            && !name.ends_with(['.', ' '])
            && !name
                .chars()
                .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    }

    /// Pins every directory from the root (or the working directory) to the
    /// destination's parent and returns the handles with the parent's path.
    fn anchor(destination: &Path) -> Result<(Vec<File>, PathBuf, OsString), Failure> {
        if destination.as_os_str().len() > 4096 {
            return Err(Failure::UnsafeDestination);
        }
        let name = destination.file_name().ok_or(Failure::UnsafeDestination)?;
        if !name.to_str().is_some_and(safe_name) {
            return Err(Failure::UnsafeDestination);
        }
        let parent = destination.parent().ok_or(Failure::UnsafeDestination)?;
        let mut path = PathBuf::new();
        let mut handles = Vec::new();
        if !destination.has_root() {
            path.push(".");
            handles.push(pin(&path)?);
        }
        for component in parent.components() {
            match component {
                // A drive-relative path such as `C:name` has no root to pin.
                Component::Prefix(_) if !destination.has_root() => {
                    return Err(Failure::UnsafeDestination);
                }
                Component::Prefix(prefix) => match prefix.kind() {
                    Prefix::Disk(_) | Prefix::VerbatimDisk(_) => {
                        path.push(prefix.as_os_str());
                    }
                    // Network shares and device namespaces are not local,
                    // pinnable directory trees.
                    _ => return Err(Failure::UnsafeDestination),
                },
                Component::RootDir => {
                    path.push(component);
                    handles.push(pin(&path)?);
                }
                Component::CurDir => {}
                Component::Normal(part) => {
                    path.push(part);
                    handles.push(pin(&path)?);
                }
                Component::ParentDir => return Err(Failure::UnsafeDestination),
            }
        }
        if handles.is_empty() {
            return Err(Failure::UnsafeDestination);
        }
        Ok((handles, path, name.to_os_string()))
    }

    struct Temporary {
        path: PathBuf,
        file: Option<File>,
        cleanup_attempted: bool,
    }

    impl Temporary {
        fn create(directory: &Path) -> Result<Self, ExportError> {
            for _ in 0..16 {
                let next = NEXT_TEMP
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                    .map_err(|_| Failure::Io)?;
                let path = directory.join(format!(
                    ".tungsten-support-{}-{next}.pending",
                    std::process::id()
                ));
                // CREATE_NEW never opens or follows an existing entry. Without
                // FILE_SHARE_DELETE the entry cannot be renamed or removed
                // while this handle is open.
                match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                    .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(&path)
                {
                    Ok(file) => {
                        return Ok(Self {
                            path,
                            file: Some(file),
                            cleanup_attempted: false,
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(_) => return Err(Failure::Io.into()),
                }
            }
            Err(Failure::Io.into())
        }

        fn cleanup(&mut self) -> bool {
            self.cleanup_attempted = true;
            // The handle must close before the name can be deleted.
            self.file = None;
            std::fs::remove_file(&self.path).is_ok()
        }

        fn failed(mut self, kind: Failure) -> ExportError {
            ExportError {
                kind,
                temporary_cleanup: self.cleanup(),
            }
        }
    }

    impl Drop for Temporary {
        fn drop(&mut self) {
            if !self.cleanup_attempted {
                self.cleanup();
            }
        }
    }

    pub(super) fn publish(
        destination: &Path,
        bytes: &[u8],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Publication, ExportError> {
        let (_pinned, directory, name) = anchor(destination)?;
        let target = directory.join(&name);
        match std::fs::symlink_metadata(&target) {
            Ok(_) => return Err(Failure::AlreadyExists.into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(Failure::Io.into()),
        }
        let mut temporary = Temporary::create(&directory)?;
        let Some(file) = temporary.file.as_mut() else {
            return Err(temporary.failed(Failure::Io));
        };
        for chunk in bytes.chunks(8192) {
            if cancelled() {
                return Err(temporary.failed(Failure::Cancelled));
            }
            if file.write_all(chunk).is_err() {
                return Err(temporary.failed(Failure::Io));
            }
        }
        if file.sync_all().is_err() {
            return Err(temporary.failed(Failure::Io));
        }
        if !file.metadata().is_ok_and(|metadata| {
            metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
        }) {
            return Err(temporary.failed(Failure::UnsafeDestination));
        }
        if cancelled() {
            return Err(temporary.failed(Failure::Cancelled));
        }
        // CreateHardLinkW fails when the new name exists; it never replaces.
        match std::fs::hard_link(&temporary.path, &target) {
            // The temporary handle stayed open without FILE_SHARE_DELETE, so
            // the linked entry is the file written above.
            Ok(()) => {
                // Publication is the commit point. Never delete the destination
                // or turn a late cancellation into an unpublished outcome.
                Ok(Publication {
                    temporary_cleanup: temporary.cleanup(),
                })
            }
            Err(error) => Err(
                temporary.failed(if error.kind() == io::ErrorKind::AlreadyExists {
                    Failure::AlreadyExists
                } else {
                    Failure::Io
                }),
            ),
        }
    }
}
