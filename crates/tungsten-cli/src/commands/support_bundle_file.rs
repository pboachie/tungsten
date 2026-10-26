// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded serialization and no-clobber support-bundle publication on Unix
//! platforms (Linux and macOS are the qualified targets).
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
    cfg!(unix)
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

#[cfg(not(unix))]
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
