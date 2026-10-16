//! File operations that survive power loss and do not trust the requests
//! directory.
//!
//! Every file the updater writes goes to a temporary name in the same
//! directory, is fsynced, renamed over the target, and the directory is
//! fsynced: after a crash a reader finds the old file or the new one, never
//! half of either.
//!
//! The api container writes into the requests directory. A process in it can
//! put a symlink, a FIFO or a directory where the request should be, so the
//! request is opened relative to a directory descriptor, without following
//! symlinks and without blocking, and read only when it is a regular file.

use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

fn cstr(path: &OsStr) -> io::Result<CString> {
    CString::new(path.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn check(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

/// Create a directory, and its parents, when it is missing, with `mode`.
///
/// An existing one has to be this user's directory, not a symlink to one, and
/// is given `mode`.
pub fn ensure_dir(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            if meta.uid() != unsafe { libc::geteuid() } {
                return Err(io::Error::other(format!(
                    "{} belongs to another user",
                    path.display()
                )));
            }
            if meta.mode() & 0o7777 != mode {
                fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
            }
            Ok(())
        }
        Ok(_) => Err(io::Error::other(format!(
            "{} exists and is not a directory",
            path.display()
        ))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::create_dir(path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
            fsync_parent(path)
        }
        Err(e) => Err(e),
    }
}

/// Give a directory to this process's uid and gid with exactly `mode` and no
/// POSIX ACL, without following a symlink to it.
///
/// Run on the requests directory inside `podman unshare`, where uid 0 is the
/// stack user and the api's uid is mapped: whatever an api did to it while it
/// owned it, the stack user owns it again and the api, no longer the owner,
/// cannot change it back.
pub fn reclaim_dir(dir: &Path, mode: u32) -> io::Result<()> {
    let fd = open_dir_nofollow(dir)?;
    let raw = fd.as_raw_fd();
    check(unsafe { libc::fchown(raw, libc::geteuid(), libc::getegid()) })?;
    remove_acls(raw)?;
    check(unsafe { libc::fchmod(raw, mode as libc::mode_t) })?;
    check(unsafe { libc::fsync(raw) }).map(drop)
}

/// Both ACLs a directory can carry: the one that grants and denies access to
/// it, and the default one that every entry created in it inherits.
#[cfg(any(target_os = "linux", target_os = "android"))]
const ACL_XATTRS: [&CStr; 2] = [c"system.posix_acl_access", c"system.posix_acl_default"];

#[cfg(any(target_os = "linux", target_os = "android"))]
fn remove_acls(fd: RawFd) -> io::Result<()> {
    for name in ACL_XATTRS {
        if unsafe { libc::fremovexattr(fd, name.as_ptr()) } < 0 {
            let e = io::Error::last_os_error();
            // None set, or a filesystem without ACLs.
            if !matches!(
                e.raw_os_error(),
                Some(libc::ENODATA) | Some(libc::EOPNOTSUPP)
            ) {
                return Err(e);
            }
        }
    }
    Ok(())
}

/// The updater runs on Linux; elsewhere only the tests run.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn remove_acls(_fd: RawFd) -> io::Result<()> {
    Ok(())
}

pub fn fsync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

pub fn fsync_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => fsync_dir(parent),
        _ => fsync_dir(Path::new(".")),
    }
}

fn temporary_for(path: &Path) -> PathBuf {
    let mut name = OsStr::new(".").to_os_string();
    name.push(path.file_name().unwrap_or_default());
    name.push(".tmp");
    path.with_file_name(name)
}

/// Replace `path` with `contents` atomically, with `mode`.
pub fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    let tmp = temporary_for(path);
    // A leftover from a crash. Every directory written through here belongs
    // to the stack user alone.
    match fs::remove_file(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&tmp)?;
    file.write_all(contents)?;
    // The mode given to open is filtered through the umask.
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)?;
    fsync_parent(path)
}

/// Read a file the updater itself wrote, without following a symlink.
pub fn read_own(path: &Path, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut out)?;
    if out.len() > limit {
        return Err(io::Error::other(format!(
            "{} is larger than {limit} bytes",
            path.display()
        )));
    }
    Ok(Some(out))
}

/// Copy a file the updater wrote to another place, atomically.
pub fn copy_atomic(from: &Path, to: &Path, mode: u32, limit: usize) -> io::Result<()> {
    let contents = read_own(from, limit)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is missing", from.display()),
        )
    })?;
    write_atomic(to, &contents, mode)
}

/// Remove a file if it is there, durably.
pub fn remove_durable(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => fsync_parent(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Everything in a directory the updater owns, and the directory stays.
pub fn empty_own_dir(dir: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    fsync_dir(dir)
}

fn open_dir_nofollow(dir: &Path) -> io::Result<OwnedFd> {
    let c = cstr(dir.as_os_str())?;
    let fd = check(unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn fstatat_nofollow(dir: RawFd, name: &CStr) -> io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    check(unsafe { libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) })?;
    Ok(st)
}

fn is_regular(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFREG
}

fn is_dir(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFDIR
}

/// Why a file in the requests directory was not read.
#[derive(Debug)]
pub enum UntrustedRead {
    /// A symlink, a FIFO, a directory or a device.
    NotRegular,
    TooLarge(u64),
    NotText,
    Io(io::Error),
}

impl std::fmt::Display for UntrustedRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UntrustedRead::NotRegular => f.write_str("not a regular file"),
            UntrustedRead::TooLarge(len) => write!(f, "{len} bytes, over the limit"),
            UntrustedRead::NotText => f.write_str("not UTF-8"),
            UntrustedRead::Io(e) => write!(f, "{e}"),
        }
    }
}

/// Read `name` from a directory another uid can write to.
///
/// Nothing is opened unless `fstatat` without following says regular file;
/// the open itself refuses a symlink and does not block on a FIFO swapped in
/// after that check, and `fstat` on the descriptor decides. `Ok(None)` when
/// there is no such entry.
pub fn read_untrusted(
    dir: &Path,
    name: &str,
    limit: usize,
) -> Result<Option<String>, UntrustedRead> {
    // One entry of `dir`, never a path that leads out of it.
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(UntrustedRead::Io(io::Error::from(
            io::ErrorKind::InvalidInput,
        )));
    }
    let dirfd = open_dir_nofollow(dir).map_err(UntrustedRead::Io)?;
    let cname = cstr(OsStr::new(name)).map_err(UntrustedRead::Io)?;
    match fstatat_nofollow(dirfd.as_raw_fd(), &cname) {
        Ok(st) if is_regular(&st) => {}
        Ok(_) => return Err(UntrustedRead::NotRegular),
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return Ok(None),
        Err(e) => return Err(UntrustedRead::Io(e)),
    }
    let fd = unsafe {
        libc::openat(
            dirfd.as_raw_fd(),
            cname.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOCTTY,
        )
    };
    if fd < 0 {
        let e = io::Error::last_os_error();
        return match e.raw_os_error() {
            Some(libc::ENOENT) => Ok(None),
            Some(libc::ELOOP) => Err(UntrustedRead::NotRegular),
            _ => Err(UntrustedRead::Io(e)),
        };
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    check(unsafe { libc::fstat(file.as_raw_fd(), &mut st) }).map_err(UntrustedRead::Io)?;
    if !is_regular(&st) {
        return Err(UntrustedRead::NotRegular);
    }
    if st.st_size as u64 > limit as u64 {
        return Err(UntrustedRead::TooLarge(st.st_size as u64));
    }
    let mut out = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut out)
        .map_err(UntrustedRead::Io)?;
    if out.len() > limit {
        return Err(UntrustedRead::TooLarge(out.len() as u64));
    }
    String::from_utf8(out)
        .map(Some)
        .map_err(|_| UntrustedRead::NotText)
}

/// Remove every entry of a directory another uid can write to, without
/// following a symlink anywhere. Run inside `podman unshare`, where the
/// entries the api created are removable.
///
/// The api can still be writing into the directory, or swapping an entry
/// for a directory between the stat and the unlink, so the directory is
/// listed again after every pass and is only reported empty once a listing
/// finds nothing. A directory that never empties is an error.
pub fn clear_untrusted(dir: &Path) -> io::Result<usize> {
    let dirfd = open_dir_nofollow(dir)?;
    let mut removed = 0;
    for _ in 0..CLEAR_PASSES {
        if list(&dirfd)?.is_empty() {
            check(unsafe { libc::fsync(dirfd.as_raw_fd()) })?;
            return Ok(removed);
        }
        removed += clear_fd(&dirfd, 0)?;
    }
    Err(io::Error::other(format!(
        "{} is still not empty after {CLEAR_PASSES} passes",
        dir.display()
    )))
}

const MAX_DEPTH: usize = 16;
const CLEAR_PASSES: usize = 8;

/// One pass over a directory. An entry that changed under it is left for
/// the next pass rather than reported.
fn clear_fd(dirfd: &OwnedFd, depth: usize) -> io::Result<usize> {
    if depth > MAX_DEPTH {
        return Err(io::Error::other("directory nesting too deep"));
    }
    let names = list(dirfd)?;
    let mut removed = 0;
    for name in names {
        let st = match fstatat_nofollow(dirfd.as_raw_fd(), &name) {
            Ok(st) => st,
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => continue,
            Err(e) => return Err(e),
        };
        if is_dir(&st) {
            let fd = unsafe {
                libc::openat(
                    dirfd.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd >= 0 {
                let sub = unsafe { OwnedFd::from_raw_fd(fd) };
                removed += clear_fd(&sub, depth + 1)?;
            }
            match check(unsafe {
                libc::unlinkat(dirfd.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR)
            }) {
                Ok(_) => removed += 1,
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {}
                // Written into, or swapped for a file, since the stat.
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(libc::ENOTEMPTY) | Some(libc::EEXIST) | Some(libc::ENOTDIR)
                    ) => {}
                Err(e) => return Err(e),
            }
        } else {
            match check(unsafe { libc::unlinkat(dirfd.as_raw_fd(), name.as_ptr(), 0) }) {
                Ok(_) => removed += 1,
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {}
                // Swapped for a directory since the stat: the next pass gets it.
                Err(e) if matches!(e.raw_os_error(), Some(libc::EISDIR) | Some(libc::EPERM)) => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(removed)
}

fn list(dirfd: &OwnedFd) -> io::Result<Vec<CString>> {
    // fdopendir takes the descriptor it is given, so it gets a duplicate.
    let dup = check(unsafe { libc::fcntl(dirfd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) })?;
    let dirp = unsafe { libc::fdopendir(dup) };
    if dirp.is_null() {
        let e = io::Error::last_os_error();
        unsafe { libc::close(dup) };
        return Err(e);
    }
    unsafe { libc::rewinddir(dirp) };
    let mut names = Vec::new();
    loop {
        // readdir returns null both at the end and on an error; only errno
        // tells them apart.
        unsafe { *errno() = 0 };
        let entry = unsafe { libc::readdir(dirp) };
        if entry.is_null() {
            let code = unsafe { *errno() };
            unsafe { libc::closedir(dirp) };
            if code != 0 {
                return Err(io::Error::from_raw_os_error(code));
            }
            return Ok(names);
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            names.push(name.to_owned());
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn errno() -> *mut libc::c_int {
    unsafe { libc::__errno_location() }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
unsafe fn errno() -> *mut libc::c_int {
    unsafe { libc::__error() }
}

/// An exclusive `flock` on the lock file, released when dropped or when the
/// process ends however it ends. The descriptor is close-on-exec, so podman
/// and the conmon processes it leaves behind never hold it.
pub struct Lock {
    _file: File,
}

impl Lock {
    /// `Ok(None)` when another run holds it and `wait` is false.
    pub fn acquire(path: &Path, wait: bool) -> io::Result<Option<Lock>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let op = if wait {
            libc::LOCK_EX
        } else {
            libc::LOCK_EX | libc::LOCK_NB
        };
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
                return Ok(Some(Lock { _file: file }));
            }
            let e = io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EWOULDBLOCK) => return Ok(None),
                _ => return Err(e),
            }
        }
    }
}

/// Bytes available to an unprivileged writer on the filesystem holding `path`.
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    let c = cstr(path.as_os_str())?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    check(unsafe { libc::statvfs(c.as_ptr(), &mut st) })?;
    #[allow(clippy::unnecessary_cast)]
    Ok(st.f_bavail as u64 * st.f_frsize as u64)
}

/// Whether two paths are on the same filesystem.
pub fn same_device(a: &Path, b: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(fs::metadata(a)?.dev() == fs::metadata(b)?.dev())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    pub(crate) fn tempdir(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "laysi-updater-fsx-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_existing_directory_is_given_its_mode() {
        let dir = tempdir("mode");
        let sub = dir.join("state");
        fs::create_dir(&sub).unwrap();
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o777)).unwrap();
        ensure_dir(&sub, 0o700).unwrap();
        assert_eq!(
            fs::metadata(&sub).unwrap().permissions().mode() & 0o777,
            0o700
        );
        ensure_dir(&dir.join("fresh"), 0o755).unwrap();
        assert_eq!(
            fs::metadata(dir.join("fresh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        symlink(&sub, dir.join("link")).unwrap();
        assert!(ensure_dir(&dir.join("link"), 0o700).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_reclaimed_directory_is_this_users_with_exactly_the_mode() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempdir("reclaim");
        let requests = dir.join("requests");
        fs::create_dir(&requests).unwrap();
        fs::write(requests.join("request"), "x").unwrap();
        fs::set_permissions(&requests, fs::Permissions::from_mode(0o2700)).unwrap();
        reclaim_dir(&requests, 0o1777).unwrap();
        let meta = fs::metadata(&requests).unwrap();
        assert_eq!(meta.mode() & 0o7777, 0o1777);
        assert_eq!(meta.uid(), unsafe { libc::geteuid() });
        assert_eq!(meta.gid(), unsafe { libc::getegid() });
        // What is in it is left to the clearing.
        assert_eq!(fs::read_to_string(requests.join("request")).unwrap(), "x");

        // Neither a symlink to a directory nor what it points to.
        let elsewhere = dir.join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&elsewhere, dir.join("link")).unwrap();
        assert!(reclaim_dir(&dir.join("link"), 0o1777).is_err());
        assert_eq!(fs::metadata(&elsewhere).unwrap().mode() & 0o7777, 0o700);
        assert!(reclaim_dir(&requests.join("request"), 0o1777).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    /// An access ACL can deny the api the directory once it is no longer the
    /// owner, and a default ACL is inherited by every request it writes and
    /// can deny the stack user reading them.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn a_reclaimed_directory_has_no_acl() {
        // ACL_USER_OBJ rwx, ACL_USER 0 ---, ACL_GROUP_OBJ r-x, ACL_MASK r-x,
        // ACL_OTHER r-x, in the kernel's xattr layout.
        let entry = |tag: u16, perm: u16, id: u32| {
            let mut e = tag.to_le_bytes().to_vec();
            e.extend(perm.to_le_bytes());
            e.extend(id.to_le_bytes());
            e
        };
        let mut acl = 2u32.to_le_bytes().to_vec();
        for e in [
            entry(0x01, 7, u32::MAX),
            entry(0x02, 0, 0),
            entry(0x04, 5, u32::MAX),
            entry(0x10, 5, u32::MAX),
            entry(0x20, 5, u32::MAX),
        ] {
            acl.extend(e);
        }
        let dir = tempdir("acl");
        let requests = dir.join("requests");
        fs::create_dir(&requests).unwrap();
        let path = cstr(requests.as_os_str()).unwrap();
        for name in ACL_XATTRS {
            let set = unsafe {
                libc::setxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    acl.as_ptr().cast(),
                    acl.len(),
                    0,
                )
            };
            if set < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EOPNOTSUPP) {
                eprintln!("skipped: {} has no ACLs", dir.display());
                fs::remove_dir_all(dir).unwrap();
                return;
            }
            check(set).unwrap();
        }

        reclaim_dir(&requests, 0o1777).unwrap();

        for name in ACL_XATTRS {
            let got =
                unsafe { libc::getxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
            assert_eq!(got, -1, "{name:?} is still set");
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::ENODATA),
                "{name:?}"
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn writes_are_atomic_and_take_the_mode() {
        let dir = tempdir("write");
        let path = dir.join("f");
        write_atomic(&path, b"one", 0o600).unwrap();
        write_atomic(&path, b"two", 0o644).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        // A temporary left by a crash does not stop the next write.
        fs::write(temporary_for(&path), b"half").unwrap();
        write_atomic(&path, b"three", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"three");
        assert!(!temporary_for(&path).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_regular_request_is_read() {
        let dir = tempdir("regular");
        fs::write(dir.join("request"), "hello").unwrap();
        assert_eq!(
            read_untrusted(&dir, "request", 10).unwrap().as_deref(),
            Some("hello")
        );
        assert!(read_untrusted(&dir, "absent", 10).unwrap().is_none());
        for name in ["", ".", "..", "../request", "/etc/passwd", "a/b"] {
            assert!(read_untrusted(&dir, name, 10).is_err(), "{name:?}");
        }
        assert!(matches!(
            read_untrusted(&dir, "request", 3),
            Err(UntrustedRead::TooLarge(5))
        ));
        fs::write(dir.join("binary"), [0xff, 0xfe]).unwrap();
        assert!(matches!(
            read_untrusted(&dir, "binary", 10),
            Err(UntrustedRead::NotText)
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_symlinked_request_is_not_followed() {
        let dir = tempdir("symlink");
        let outside = tempdir("outside");
        fs::write(outside.join("secret"), "do not read").unwrap();
        symlink(outside.join("secret"), dir.join("request")).unwrap();
        assert!(matches!(
            read_untrusted(&dir, "request", 100),
            Err(UntrustedRead::NotRegular)
        ));
        // Nor is a symlinked directory.
        let linked = dir.join("linked");
        symlink(&outside, &linked).unwrap();
        assert!(read_untrusted(&linked, "secret", 100).is_err());
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn a_fifo_request_does_not_block() {
        let dir = tempdir("fifo");
        let path = cstr(dir.join("request").as_os_str()).unwrap();
        check(unsafe { libc::mkfifo(path.as_ptr(), 0o644) }).unwrap();
        let started = std::time::Instant::now();
        assert!(matches!(
            read_untrusted(&dir, "request", 100),
            Err(UntrustedRead::NotRegular)
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn clearing_removes_entries_but_follows_nothing() {
        let dir = tempdir("clear");
        let outside = tempdir("kept");
        fs::write(outside.join("keep"), "x").unwrap();
        fs::write(dir.join("request"), "x").unwrap();
        fs::write(dir.join(".request.tmp"), "x").unwrap();
        symlink(&outside, dir.join("to-outside")).unwrap();
        symlink(outside.join("keep"), dir.join("file-link")).unwrap();
        fs::create_dir_all(dir.join("nested/deeper")).unwrap();
        fs::write(dir.join("nested/deeper/x"), "x").unwrap();
        symlink(&outside, dir.join("nested/link")).unwrap();
        let fifo = cstr(dir.join("fifo").as_os_str()).unwrap();
        check(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }).unwrap();

        clear_untrusted(&dir).unwrap();
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(fs::read_to_string(outside.join("keep")).unwrap(), "x");
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn the_lock_is_exclusive() {
        let dir = tempdir("lock");
        let path = dir.join("lock");
        let held = Lock::acquire(&path, false)
            .unwrap()
            .expect("first run gets it");
        // flock locks belong to the open file description, so a second open
        // in the same process conflicts like another process would.
        assert!(Lock::acquire(&path, false).unwrap().is_none());
        drop(held);
        assert!(Lock::acquire(&path, false).unwrap().is_some());
        fs::remove_dir_all(dir).unwrap();
    }
}
