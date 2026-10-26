//! NFSv3 wire procedures from RFC 1813. All methods run on bounded blocking workers.
use crate::rpc::Dispatcher;
use crate::xdr::{Malformed, Reader, Writer};
use filemux_fs::{Node, NodeKind, SharedFs};
use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Seek, SeekFrom};
use std::time::{SystemTime, UNIX_EPOCH};

const OK: u32 = 0;
const IO: u32 = 5;
const NOTDIR: u32 = 20;
const ISDIR: u32 = 21;
const INVAL: u32 = 22;
const ROFS: u32 = 30;
const STALE: u32 = 70;
const BADHANDLE: u32 = 10001;
const BAD_COOKIE: u32 = 10003;
const TOOSMALL: u32 = 10005;
const MAX_READ: usize = 1024 * 1024;

pub(crate) fn handle(out: &mut Writer, fs: &SharedFs, id: u64) {
    out.u32(28);
    out.fixed(b"FMX1");
    out.fixed(&fs.export_id());
    out.u64(id);
}

fn handle_id(input: &mut Reader<'_>, fs: &SharedFs) -> Result<Result<u64, u32>, Malformed> {
    let bytes = input.opaque(64)?;
    if bytes.len() != 28 || &bytes[..4] != b"FMX1" {
        return Ok(Err(BADHANDLE));
    }
    if bytes[4..20] != fs.export_id() {
        return Ok(Err(STALE));
    }
    Ok(Ok(u64::from_be_bytes(
        bytes[20..28].try_into().map_err(|_| Malformed)?,
    )))
}

fn error(error: &filemux_fs::FsError) -> u32 {
    match error {
        filemux_fs::FsError::Stale => STALE,
        filemux_fs::FsError::DirectoryTooLarge => IO,
        filemux_fs::FsError::State(_) | filemux_fs::FsError::Database(_) => 10006,
        _ => io_error(error.kind()),
    }
}

fn io_error(kind: io::ErrorKind) -> u32 {
    match kind {
        io::ErrorKind::NotFound => 2,
        io::ErrorKind::PermissionDenied => 13,
        io::ErrorKind::AlreadyExists => 17,
        io::ErrorKind::NotADirectory => NOTDIR,
        io::ErrorKind::IsADirectory => ISDIR,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => INVAL,
        io::ErrorKind::Unsupported => 10004,
        _ => IO,
    }
}

fn node(fs: &SharedFs, id: Result<u64, u32>) -> Result<Node, u32> {
    fs.metadata(id?).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            STALE
        } else {
            error(&err)
        }
    })
}

fn timestamp(out: &mut Writer, value: SystemTime) {
    let duration = value.duration_since(UNIX_EPOCH).unwrap_or_default();
    out.u32(duration.as_secs().min(u64::from(u32::MAX)) as u32);
    out.u32(duration.subsec_nanos());
}

fn attributes(out: &mut Writer, fs: &SharedFs, value: &Node) {
    out.u32(match value.kind {
        NodeKind::File => 1,
        NodeKind::Directory => 2,
        NodeKind::BlockDevice => 3,
        NodeKind::CharDevice => 4,
        NodeKind::Symlink => 5,
        NodeKind::Socket => 6,
        NodeKind::Fifo => 7,
    });
    out.u32(value.mode & 0o7777);
    out.u32(value.nlink);
    out.u32(value.uid);
    out.u32(value.gid);
    out.u64(value.size);
    // SharedFs has no allocation-block field; used is the portable logical size estimate.
    out.u64(value.size);
    #[cfg(target_os = "linux")]
    let (major, minor) = (
        ((value.rdev >> 8) & 0xfff) | ((value.rdev >> 32) & 0xfffff000),
        (value.rdev & 0xff) | ((value.rdev >> 12) & 0xffffff00),
    );
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    let (major, minor) = ((value.rdev >> 24) & 0xff, value.rdev & 0xffffff);
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    )))]
    let (major, minor) = (0, 0);
    out.u32(major as u32);
    out.u32(minor as u32);
    out.fixed(&fs.export_id()[..8]); // fsid, stable across restarts
    out.u64(value.id);
    timestamp(out, value.atime);
    timestamp(out, value.mtime);
    timestamp(out, value.ctime);
}

fn post(out: &mut Writer, fs: &SharedFs, value: Option<&Node>) {
    out.boolean(value.is_some());
    if let Some(value) = value {
        attributes(out, fs, value);
    }
}

fn wcc(out: &mut Writer, fs: &SharedFs, value: Option<&Node>) {
    out.boolean(value.is_some());
    if let Some(value) = value {
        out.u64(value.size);
        timestamp(out, value.mtime);
        timestamp(out, value.ctime);
    }
    post(out, fs, value);
}

fn failure(out: &mut Writer, fs: &SharedFs, status: u32, value: Option<&Node>) {
    out.u32(status);
    post(out, fs, value);
}

#[cfg(unix)]
fn name_bytes(value: &OsStr) -> Result<Cow<'_, [u8]>, u32> {
    use std::os::unix::ffi::OsStrExt;
    Ok(Cow::Borrowed(value.as_bytes()))
}

#[cfg(not(unix))]
fn name_bytes(value: &OsStr) -> Result<Cow<'_, [u8]>, u32> {
    value
        .to_str()
        .map(|name| Cow::Borrowed(name.as_bytes()))
        .ok_or(INVAL)
}

pub(crate) fn os_name(bytes: &[u8]) -> Result<OsString, u32> {
    if bytes.is_empty() || bytes.contains(&0) || bytes.contains(&b'/') || bytes.contains(&b'\\') {
        return Err(INVAL);
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(OsStr::from_bytes(bytes).to_os_string())
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .map(OsString::from)
            .map_err(|_| INVAL)
    }
}

fn directory(value: &Node) -> Result<(), u32> {
    if value.kind == NodeKind::Directory {
        Ok(())
    } else {
        Err(NOTDIR)
    }
}

fn transfer_limit(maximum: usize) -> usize {
    // Leave room for RPC envelope, status, attributes and XDR padding, on both transports.
    maximum.saturating_sub(256).min(MAX_READ)
}

pub(crate) fn call(
    context: &Dispatcher,
    procedure: u32,
    input: &mut Reader<'_>,
    out: &mut Writer,
    maximum: usize,
) -> Result<(), Malformed> {
    let fs = &context.fs;
    if procedure == 0 {
        return Ok(());
    }
    if matches!(procedure, 2 | 7..=15 | 21) {
        return modifying(context, procedure, input, out);
    }
    let object = node(fs, handle_id(input, fs)?);
    match procedure {
        1 => match object {
            Ok(value) => {
                out.u32(OK);
                attributes(out, fs, &value);
            }
            Err(status) => out.u32(status),
        },
        3 => {
            let name = input.opaque(255)?;
            let result = (|| {
                let dir = object.as_ref().map_err(|status| *status)?;
                directory(dir)?;
                fs.lookup(dir.id, &os_name(name)?)
                    .map_err(|err| error(&err))
            })();
            match result {
                Ok(value) => {
                    out.u32(OK);
                    handle(out, fs, value.id);
                    post(out, fs, Some(&value));
                    post(out, fs, object.as_ref().ok());
                }
                Err(status) => failure(out, fs, status, object.as_ref().ok()),
            }
        }
        4 => {
            let requested = input.u32()?;
            match object {
                Ok(value) => {
                    out.u32(OK);
                    post(out, fs, Some(&value));
                    // Anonymous export permissions come from the server's opened capability.
                    // Native mode bits remain intact in fattr; write/delete/extend are never granted.
                    let supported = match value.kind {
                        NodeKind::Directory => 1 | 2, // READ, LOOKUP
                        NodeKind::File => 1 | if value.mode & 0o111 != 0 { 32 } else { 0 },
                        _ => 0,
                    };
                    out.u32(requested & supported);
                }
                Err(status) => failure(out, fs, status, None),
            }
        }
        5 => {
            let result = (|| {
                let value = object.as_ref().map_err(|status| *status)?;
                if value.kind != NodeKind::Symlink {
                    return Err(INVAL);
                }
                let target = fs.read_link(value.id).map_err(|err| error(&err))?;
                if name_bytes(target.as_os_str())?.len() > 1024 {
                    return Err(63);
                }
                Ok(target)
            })();
            match result {
                Ok(target) => {
                    let bytes =
                        name_bytes(target.as_os_str()).expect("validated readlink encoding");
                    out.u32(OK);
                    post(out, fs, object.as_ref().ok());
                    #[cfg(not(windows))]
                    out.opaque(&bytes);
                    #[cfg(windows)]
                    {
                        out.u32(bytes.len() as u32);
                        out.0.extend(
                            bytes
                                .iter()
                                .map(|byte| if *byte == b'\\' { b'/' } else { *byte }),
                        );
                        out.0.resize(out.0.len() + (4 - bytes.len() % 4) % 4, 0);
                    }
                }
                Err(status) => failure(out, fs, status, object.as_ref().ok()),
            }
        }
        6 => {
            let offset = input.u64()?;
            let count = (input.u32()? as usize).min(transfer_limit(maximum));
            // U-Boot net/nfs-common.c:nfs_read_req appends one zero word
            // after the v3 count (https://github.com/u-boot/u-boot/blob/master/net/nfs-common.c).
            // Accept only that exact optional extension, never arbitrary trailing data.
            if input.finish().is_err() {
                if input.u32()? != 0 {
                    return Err(Malformed);
                }
                input.finish()?;
            }
            read(fs, object, offset, count, out);
        }
        16 | 17 => {
            let cookie = input.u64()?;
            let verifier = input.fixed(8)?;
            let dircount = input.u32()? as usize;
            let maxcount = if procedure == 17 {
                input.u32()? as usize
            } else {
                dircount
            };
            readdir(
                fs,
                object,
                cookie,
                verifier,
                dircount,
                maxcount.min(maximum.saturating_sub(24)),
                procedure == 17,
                out,
            );
        }
        18 => {
            match object {
                Ok(value) => match fs2::statvfs(fs.root_path()) {
                    Ok(stats) => {
                        out.u32(OK);
                        post(out, fs, Some(&value));
                        out.u64(stats.total_space());
                        out.u64(stats.free_space());
                        out.u64(0); // no writable bytes in this read-only export
                        // fs2 does not expose inode counts portably. Zero denotes unavailable counts.
                        out.u64(0);
                        out.u64(0);
                        out.u64(0);
                        out.u32(0); // volatile, no invariant interval
                    }
                    Err(_) => failure(out, fs, IO, Some(&value)),
                },
                Err(status) => failure(out, fs, status, None),
            }
        }
        19 => match object {
            Ok(value) => {
                out.u32(OK);
                post(out, fs, Some(&value));
                let limit = transfer_limit(maximum) as u32;
                for word in [limit, limit.min(32768), 4, 0, 0, 4, limit.min(32768)] {
                    out.u32(word);
                }
                out.u64(i64::MAX as u64);
                out.u32(1); // conservative portable time_delta = 1 second
                out.u32(0);
                out.u32(2); // FSF3_SYMLINK, not CANSETTIME/HOMOGENEOUS
            }
            Err(status) => failure(out, fs, status, None),
        },
        20 => match object {
            Ok(value) => {
                out.u32(OK);
                post(out, fs, Some(&value));
                out.u32(1); // conservative minimum link limit; creating links is forbidden
                out.u32(255);
                out.boolean(true); // no truncation
                out.boolean(true); // chown restricted
                out.boolean(cfg!(windows));
                out.boolean(true); // case preserving
            }
            Err(status) => failure(out, fs, status, None),
        },
        _ => unreachable!(),
    }
    Ok(())
}

fn read(fs: &SharedFs, object: Result<Node, u32>, offset: u64, count: usize, out: &mut Writer) {
    let file = (|| {
        let value = object.as_ref().map_err(|status| *status)?;
        if value.kind != NodeKind::File {
            return Err(if value.kind == NodeKind::Directory {
                ISDIR
            } else {
                INVAL
            });
        }
        let mut file = fs.open_by_id(value.id).map_err(|err| error(&err))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|err| io_error(err.kind()))?;
        Ok(file)
    })();
    let mut file = match file {
        Ok(file) => file,
        Err(status) => {
            failure(out, fs, status, object.as_ref().ok());
            return;
        }
    };
    let start = out.0.len();
    out.u32(OK);
    post(out, fs, object.as_ref().ok());
    let lengths = out.0.len();
    out.u32(0); // count, eof and opaque length patched after bounded direct read
    out.u32(0);
    out.u32(0);
    let data_start = out.0.len();
    let requested = count;
    if let Err(err) = (&mut file).take(count as u64).read_to_end(&mut out.0) {
        out.0.truncate(start);
        failure(out, fs, io_error(err.kind()), object.as_ref().ok());
        return;
    }
    let count = out.0.len() - data_start;
    let eof = count < requested
        || object
            .as_ref()
            .is_ok_and(|value| offset.saturating_add(count as u64) >= value.size);
    out.0[lengths..lengths + 4].copy_from_slice(&(count as u32).to_be_bytes());
    out.0[lengths + 4..lengths + 8].copy_from_slice(&u32::from(eof).to_be_bytes());
    out.0[lengths + 8..lengths + 12].copy_from_slice(&(count as u32).to_be_bytes());
    out.0.resize(out.0.len() + (4 - count % 4) % 4, 0);
}

#[allow(clippy::too_many_arguments)]
fn readdir(
    fs: &SharedFs,
    object: Result<Node, u32>,
    cookie: u64,
    verifier: &[u8],
    dircount: usize,
    maxcount: usize,
    plus: bool,
    out: &mut Writer,
) {
    let result = (|| {
        let value = object.as_ref().map_err(|status| *status)?;
        directory(value)?;
        let mut entries = fs.list(value.id).map_err(|err| error(&err))?;
        // SharedFs bounds listing at 10,000. Check again to retain this wire layer's own bound.
        if entries.len() > 10000 {
            return Err(IO);
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        // FNV-1a over exact entry identities/names plus directory identity. A verifier describes
        // this listing, not a boot session, so old directory handles survive restarts.
        let mut hash = 0xcbf29ce484222325_u64;
        let mut mix = |bytes: &[u8]| {
            for byte in bytes {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x100000001b3);
            }
        };
        mix(&fs.export_id());
        mix(&value.id.to_be_bytes());
        for entry in &entries {
            let name = name_bytes(&entry.name)?;
            if name.len() > 255 {
                return Err(63);
            }
            mix(&(name.len() as u32).to_be_bytes());
            mix(&name);
            mix(&entry.node.id.to_be_bytes());
        }
        let valid = hash.to_be_bytes();
        if cookie != 0 && (verifier != valid || cookie > entries.len() as u64) {
            return Err(BAD_COOKIE);
        }
        let start = out.0.len();
        out.u32(OK);
        post(out, fs, Some(value));
        out.fixed(&valid);
        // Account for the terminating list boolean and eof, not just entry payloads.
        let mut basic_size = 8;
        let mut next = cookie as usize;
        if out.0.len() - start + 8 > maxcount {
            out.0.truncate(start);
            return Err(TOOSMALL);
        }
        while next < entries.len() {
            let entry = &entries[next];
            let name = name_bytes(&entry.name)?;
            let basic = 4 + 8 + 4 + name.len().div_ceil(4) * 4 + 8;
            let extra = if plus { 4 + 84 + 4 + 4 + 28 } else { 0 };
            if out.0.len() - start + basic + extra + 8 > maxcount
                || (plus && basic_size + basic > dircount)
            {
                if next == cookie as usize {
                    out.0.truncate(start);
                    return Err(TOOSMALL);
                }
                break;
            }
            out.boolean(true);
            out.u64(entry.node.id);
            out.opaque(&name);
            out.u64(next as u64 + 1);
            if plus {
                post(out, fs, Some(&entry.node));
                out.boolean(true);
                handle(out, fs, entry.node.id);
            }
            basic_size += basic;
            next += 1;
        }
        out.boolean(false);
        out.boolean(next == entries.len());
        Ok(())
    })();
    if let Err(status) = result {
        failure(out, fs, status, object.as_ref().ok());
    }
}

fn sattr(input: &mut Reader<'_>) -> Result<(), Malformed> {
    for _ in 0..3 {
        // mode, uid, gid
        if input.boolean()? {
            input.u32()?;
        }
    }
    if input.boolean()? {
        input.u64()?;
    } // size
    for _ in 0..2 {
        // atime, mtime
        match input.u32()? {
            0 | 1 => {}
            2 => {
                nfstime(input)?;
            }
            _ => return Err(Malformed),
        }
    }
    Ok(())
}

fn nfstime(input: &mut Reader<'_>) -> Result<(), Malformed> {
    input.u32()?;
    if input.u32()? >= 1_000_000_000 {
        return Err(Malformed);
    }
    Ok(())
}

fn diropargs(input: &mut Reader<'_>, fs: &SharedFs) -> Result<Result<u64, u32>, Malformed> {
    let id = handle_id(input, fs)?;
    input.opaque(255)?;
    Ok(id)
}

fn modifying(
    context: &Dispatcher,
    procedure: u32,
    input: &mut Reader<'_>,
    out: &mut Writer,
) -> Result<(), Malformed> {
    let fs = &context.fs;
    let first = handle_id(input, fs)?;
    let mut second = None;
    match procedure {
        2 => {
            sattr(input)?;
            if input.boolean()? {
                nfstime(input)?;
            }
        }
        7 => {
            input.u64()?;
            let count = input.u32()?;
            if input.u32()? > 2 {
                return Err(Malformed);
            }
            let data = input.opaque(4 * 1024 * 1024)?;
            if data.len() != count as usize {
                return Err(Malformed);
            }
        }
        8 => {
            input.opaque(255)?;
            match input.u32()? {
                0 | 1 => sattr(input)?,
                2 => {
                    input.fixed(8)?;
                }
                _ => return Err(Malformed),
            }
        }
        9 => {
            input.opaque(255)?;
            sattr(input)?;
        }
        10 => {
            input.opaque(255)?;
            sattr(input)?;
            input.opaque(1024)?;
        }
        11 => {
            input.opaque(255)?;
            match input.u32()? {
                3 | 4 => {
                    sattr(input)?;
                    input.u32()?;
                    input.u32()?;
                }
                6 | 7 => sattr(input)?,
                _ => return Err(Malformed),
            }
        }
        12 | 13 => {
            input.opaque(255)?;
        }
        14 => {
            input.opaque(255)?;
            second = Some(diropargs(input, fs)?);
        }
        15 => {
            second = Some(diropargs(input, fs)?);
        }
        21 => {
            let offset = input.u64()?;
            let count = input.u32()?;
            let object = node(fs, first);
            // COMMIT is not a modifying request that may simply return ROFS (RFC 1813
            // excludes ROFS here). Validate the object and actually sync it. No unstable
            // WRITE is ever accepted; the stable export identity is a valid verifier.
            let status = match object.as_ref() {
                Err(status) => {
                    if matches!(*status, STALE | BADHANDLE) {
                        *status
                    } else {
                        IO
                    }
                }
                Ok(value)
                    if value.kind == NodeKind::File
                        && offset.checked_add(u64::from(count)).is_some() =>
                {
                    match fs.open_by_id(value.id) {
                        Ok(file) => {
                            if file.sync_all().is_ok() {
                                OK
                            } else {
                                IO
                            }
                        }
                        Err(err) => {
                            if err.kind() == io::ErrorKind::NotFound {
                                STALE
                            } else {
                                IO
                            }
                        }
                    }
                }
                Ok(_) => IO,
            };
            out.u32(status);
            wcc(out, fs, object.as_ref().ok());
            if status == OK {
                out.fixed(&fs.export_id()[..8]);
            }
            return Ok(());
        }
        _ => unreachable!(),
    }
    let object = node(fs, first);
    let other = second.map(|id| node(fs, id));
    let directory_required = matches!(procedure, 8..=14);
    let status = match (&object, &other) {
        (Err(status), _) => *status,
        (_, Some(Err(status))) => *status,
        (Ok(value), _) if directory_required && value.kind != NodeKind::Directory => NOTDIR,
        (_, Some(Ok(value))) if value.kind != NodeKind::Directory => NOTDIR,
        _ => ROFS,
    };
    out.u32(status);
    if procedure == 15 {
        post(out, fs, object.as_ref().ok());
        wcc(
            out,
            fs,
            other.as_ref().and_then(|value| value.as_ref().ok()),
        );
    } else {
        wcc(out, fs, object.as_ref().ok());
        if procedure == 14 {
            wcc(
                out,
                fs,
                other.as_ref().and_then(|value| value.as_ref().ok()),
            );
        }
    }
    Ok(())
}
