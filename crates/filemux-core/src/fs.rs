//! Rooted capability filesystem shared by all Filemux protocols.
//!
//! IDs are committed to a FULL-synchronous SQLite database before exposure. They
//! combine native device/inode identity, native creation time and object type;
//! no path hash or mutable ctime is used as an inode generation substitute.
//! Filesystems lacking creation time are deliberately unsupported, rather than
//! silently attaching an old handle to a recycled inode. State belongs outside
//! the export. A store is bound to one canonical root and rejects replacement.
//!
//! Unix attributes are native. Windows maps uid/gid/rdev to zero, regular files
//! and directories to virtual read-only executable 0555, symlinks to 0777, and
//! ctime to creation time. This is not an NTFS ACL translation: host ACLs still
//! govern actual file opens. Native metadata is never modified.
//!
//! Windows durable exports currently require NTFS (the dependency's native
//! identity API is 64-bit, insufficient for ReFS). Linux safe data opens use
//! O_PATH followed by an anchored /proc/self/fd reopen, requiring procfs.
//! Directories are limited to 10,000 scanned entries, including hidden names;
//! exceeding the limit returns an error rather than an incomplete listing.
//! Path traversal uses metadata-only component checks and at most 40 symlink
//! expansions; data is opened only after a regular file has been identified.
//! Windows checks each export-relative component's normalized long name, so
//! NTFS short aliases cannot expose reserved upload entries. Host ancestors
//! (including the chosen export directory's own name) are not export names.

use cap_fs_ext::{
    DirExt, FileTypeExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsSyncExt,
};
use cap_std::fs::{Dir, Metadata, OpenOptions};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub type Result<T, E = FsError> = std::result::Result<T, E>;
pub const MAX_DIRECTORY_ENTRIES: usize = 10_000;
const RESERVED: &str = ".filemux-";

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("invalid or reserved export path")]
    InvalidPath,
    #[error("stale filesystem object identity")]
    Stale,
    #[error("object is not a directory")]
    NotDirectory,
    #[error("only regular files may be read")]
    NotRegular,
    #[error("directory exceeds the {MAX_DIRECTORY_ENTRIES}-entry scan limit")]
    DirectoryTooLarge,
    #[error("persistent filesystem state: {0}")]
    State(String),
    #[error("native creation identity unavailable: {0}")]
    UnsupportedIdentity(String),
    #[error("persistent filesystem database: {0}")]
    Database(#[from] rusqlite::Error),
}

impl FsError {
    pub fn kind(&self) -> io::ErrorKind {
        match self {
            Self::Io(error) => error.kind(),
            Self::InvalidPath => io::ErrorKind::PermissionDenied,
            Self::Stale => io::ErrorKind::NotFound,
            Self::NotDirectory => io::ErrorKind::NotADirectory,
            Self::NotRegular => io::ErrorKind::PermissionDenied,
            Self::DirectoryTooLarge => io::ErrorKind::InvalidData,
            Self::UnsupportedIdentity(_) => io::ErrorKind::Unsupported,
            Self::State(_) | Self::Database(_) => io::ErrorKind::InvalidData,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Directory,
    Symlink,
    BlockDevice,
    CharDevice,
    Fifo,
    Socket,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: u64,
    pub kind: NodeKind,
    pub size: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u32,
    pub atime: SystemTime,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub rdev: u64,
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: OsString,
    pub node: Node,
}

pub struct SharedFs {
    root: Dir,
    root_path: PathBuf,
    db: Mutex<Connection>,
    export_id: [u8; 16],
    root_id: u64,
}

impl SharedFs {
    pub fn open(root: &Path, state_dir: &Path) -> Result<Self> {
        let root_path = std::fs::canonicalize(root)?;
        let root = Dir::open_ambient_dir(&root_path, cap_std::ambient_authority())?;
        #[cfg(windows)]
        require_ntfs(&root, &root_path)?;
        let root_metadata = root.dir_metadata()?;
        let root_identity = identity_at(&root_metadata, &root_path)?;
        // Resolve the existing ancestor *before* creating state: refusing a
        // nested state directory must not itself add files to the export.
        let state_absolute = absolute(state_dir)?;
        let mut ancestor = state_absolute.as_path();
        while !ancestor.exists() {
            ancestor = ancestor.parent().ok_or(FsError::InvalidPath)?;
        }
        if std::fs::canonicalize(ancestor)?.starts_with(&root_path) {
            return Err(FsError::State(
                "state directory must be outside the export".into(),
            ));
        }
        std::fs::create_dir_all(&state_absolute)?;
        let state_path = std::fs::canonicalize(&state_absolute)?;
        if state_path.starts_with(&root_path) {
            return Err(FsError::State(
                "state directory must be outside the export".into(),
            ));
        }
        let db_path = state_path.join("objects.sqlite3");
        let marker_path = state_path.join("store-id");
        let database_exists = db_path.try_exists()?;
        let marker_exists = marker_path.try_exists()?;
        if database_exists != marker_exists {
            return Err(FsError::State(
                "state store is incomplete; refusing to regenerate identities".into(),
            ));
        }
        let mut db = if database_exists {
            Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?
        } else {
            // create_new prevents two initializers from silently replacing a
            // partially initialized store. A crash leaves an explicit error.
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&db_path)?
                .sync_all()?;
            Connection::open(&db_path)?
        };
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let export_id;
        let root_id;
        if database_exists {
            let check: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
            if check != "ok" {
                return Err(FsError::State(check));
            }
            let (version, stored_root, stored_identity, stored_export): (
                i64,
                Vec<u8>,
                Vec<u8>,
                Vec<u8>,
            ) = db.query_row(
                "SELECT version,root_path,root_identity,export_id FROM export WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            if version != 1
                || stored_root != encode_path(&root_path)
                || stored_identity != root_identity
            {
                return Err(FsError::State("root path, native identity or schema does not match this state store; for a different export select a different state_dir (never delete the previous store to reuse its handles)".into()));
            }
            export_id = stored_export
                .try_into()
                .map_err(|_| FsError::State("invalid export identity".into()))?;
            if std::fs::read(&marker_path)? != export_id {
                return Err(FsError::State(
                    "state marker does not match database".into(),
                ));
            }
            root_id = db.query_row(
                "SELECT id FROM objects WHERE identity=?1",
                [&root_identity],
                |r| r.get::<_, i64>(0),
            )? as u64;
        } else {
            export_id = *Uuid::new_v4().as_bytes();
            let tx = db.transaction()?;
            tx.execute_batch("CREATE TABLE export(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL,root_path BLOB NOT NULL,root_identity BLOB NOT NULL,export_id BLOB NOT NULL);
                CREATE TABLE objects(id INTEGER PRIMARY KEY AUTOINCREMENT,identity BLOB UNIQUE NOT NULL);
                CREATE TABLE paths(path BLOB PRIMARY KEY,object_id INTEGER NOT NULL REFERENCES objects(id));")?;
            tx.execute(
                "INSERT INTO export VALUES(1,1,?1,?2,?3)",
                params![encode_path(&root_path), root_identity, export_id.as_slice()],
            )?;
            tx.execute("INSERT INTO objects(identity) VALUES(?1)", [&root_identity])?;
            root_id = tx.last_insert_rowid() as u64;
            tx.execute(
                "INSERT INTO paths VALUES(?1,?2)",
                params![encode_path(Path::new("")), root_id as i64],
            )?;
            tx.commit()?;
            use std::io::Write;
            let mut marker = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker_path)?;
            marker.write_all(&export_id)?;
            marker.sync_all()?;
            #[cfg(unix)]
            File::open(&state_path)?.sync_all()?;
        }
        let result = Self {
            root,
            root_path,
            db: Mutex::new(db),
            export_id,
            root_id,
        };
        result.metadata(root_id)?;
        Ok(result)
    }

    pub fn root_id(&self) -> u64 {
        self.root_id
    }
    pub fn export_id(&self) -> [u8; 16] {
        self.export_id
    }
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn metadata(&self, id: u64) -> Result<Node> {
        let (path, expected) = self.resolve_id(id)?;
        let metadata = self.lstat(&path)?;
        ensure_identity(&metadata, &expected)?;
        node(id, &metadata)
    }

    pub fn lookup(&self, parent: u64, name: &OsStr) -> Result<Node> {
        let (parent_path, expected) = self.resolve_id(parent)?;
        let resolved = self.resolve_path(&parent_path, false)?;
        ensure_identity(&resolved.metadata, &expected)?;
        let dir = resolved.open_directory()?;
        if name == OsStr::new(".") {
            ensure_identity(&self.lstat(&parent_path)?, &expected)?;
            return node(parent, &resolved.metadata);
        }
        if name == OsStr::new("..") {
            let path = parent_path.parent().unwrap_or(Path::new(""));
            let metadata = self.lstat(path)?;
            ensure_identity(&self.lstat(&parent_path)?, &expected)?;
            return self.register(path, &metadata);
        }
        validate_component(name)?;
        self.lookup_child(&parent_path, &expected, &dir, name)
    }

    pub fn lookup_path(&self, relative: &Path) -> Result<Node> {
        let relative = validate_path(relative)?;
        let resolved = self.resolve_path(&relative, false)?;
        self.register(&resolved.path, &resolved.metadata)
    }

    pub fn list(&self, dir_id: u64) -> Result<Vec<DirEntry>> {
        let (path, expected) = self.resolve_id(dir_id)?;
        let metadata = self.lstat(&path)?;
        ensure_identity(&metadata, &expected)?;
        if !metadata.is_dir() {
            return Err(FsError::NotDirectory);
        }
        let dir = self.root.open_dir_nofollow(cap_path(&path))?;
        ensure_identity(&dir.dir_metadata()?, &expected)?;
        let mut names = Vec::new();
        for (scanned, entry) in dir.entries()?.enumerate() {
            if scanned == MAX_DIRECTORY_ENTRIES {
                return Err(FsError::DirectoryTooLarge);
            }
            let entry = entry?;
            let name = entry.file_name();
            if reserved(&name) {
                continue;
            }
            // Host names that cannot be addressed safely are not advertised.
            if validate_component(&name).is_err() {
                continue;
            }
            names.push(name);
        }
        // Check the scan bound before allocating IDs or doing synchronous DB
        // commits; all returned entries are persisted in one transaction.
        let mut result = Vec::with_capacity(names.len());
        let mut db = self.db()?;
        let tx = db.transaction()?;
        for name in names {
            let metadata = dir.symlink_metadata(&name)?;
            let child = path.join(&name);
            let identity = identity_at(&metadata, &self.root_path.join(&child))?;
            let id = persist_object(&tx, &child, &identity)?;
            result.push(DirEntry {
                name,
                node: node(id, &metadata)?,
            });
        }
        // A concurrent directory replacement must not issue its children as
        // though they belonged to the original parent handle.
        ensure_identity(&self.lstat(&path)?, &expected)?;
        tx.commit()?;
        result.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Ok(result)
    }

    pub fn open_read(&self, relative: &Path) -> Result<File> {
        let relative = validate_path(relative)?;
        let resolved = self.resolve_path(&relative, true)?;
        if !resolved.metadata.is_file() {
            return Err(FsError::NotRegular);
        }
        let expected = identity(&resolved.metadata)?;
        self.open_resolved_regular(resolved, &expected)
    }

    pub fn open_by_id(&self, id: u64) -> Result<File> {
        let (path, expected) = self.resolve_id(id)?;
        self.open_regular(&path, &expected)
    }

    pub fn read_link(&self, id: u64) -> Result<PathBuf> {
        let (path, expected) = self.resolve_id(id)?;
        let metadata = self.lstat(&path)?;
        ensure_identity(&metadata, &expected)?;
        if !metadata.is_symlink() {
            return Err(FsError::InvalidPath);
        }
        let contents = self.root.read_link_contents(cap_path(&path))?;
        ensure_identity(&self.lstat(&path)?, &expected)?;
        Ok(contents)
    }

    pub fn begin_upload(&self, relative: &Path) -> Result<Upload> {
        let relative = validate_path(relative)?;
        let name = relative
            .file_name()
            .ok_or(FsError::InvalidPath)?
            .to_os_string();
        let parent_path = relative.parent().unwrap_or(Path::new(""));
        let resolved = self.resolve_path(parent_path, true)?;
        let parent = resolved.open_directory()?;
        match parent.symlink_metadata(&name) {
            Ok(_) => {
                return Err(
                    io::Error::new(io::ErrorKind::AlreadyExists, "upload target exists").into(),
                );
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let temporary = OsString::from(format!("{RESERVED}upload-{}", Uuid::new_v4()));
        let file = parent
            .open_with(
                &temporary,
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .follow(FollowSymlinks::No),
            )?
            .into_std();
        let expected = identity(&Metadata::from_file(&file)?)?;
        Ok(Upload {
            parent,
            temporary,
            target: name,
            file: Some(file),
            expected,
        })
    }

    fn db(&self) -> Result<MutexGuard<'_, Connection>> {
        self.db
            .lock()
            .map_err(|_| FsError::State("database lock poisoned".into()))
    }

    fn lstat(&self, path: &Path) -> Result<Metadata> {
        Ok(self.resolve_path(path, false)?.metadata)
    }

    fn lookup_child(
        &self,
        parent_path: &Path,
        expected: &[u8],
        dir: &Dir,
        name: &OsStr,
    ) -> Result<Node> {
        let metadata = checked_metadata(dir, name)?;
        // The child was inspected through the retained parent, never a reopen
        // of parent_path/name. Do not issue it under a replaced parent handle.
        ensure_identity(&self.lstat(parent_path)?, expected)?;
        let child = self.register(&parent_path.join(name), &metadata)?;
        ensure_identity(&self.lstat(parent_path)?, expected)?;
        Ok(child)
    }

    fn resolve_path(&self, path: &Path, follow_final: bool) -> Result<Resolved> {
        let mut pending: VecDeque<OsString> = validate_path(path)?
            .iter()
            .map(OsStr::to_os_string)
            .collect();
        let mut directories = vec![self.root.try_clone()?];
        let mut resolved = PathBuf::new();
        let mut links = 0;
        while let Some(name) = pending.pop_front() {
            if name == OsStr::new(".") {
                continue;
            }
            if name == OsStr::new("..") {
                if directories.len() == 1 {
                    return Err(FsError::InvalidPath);
                }
                directories.pop();
                resolved.pop();
                continue;
            }
            validate_component(&name)?;
            let parent = directories.last().expect("root directory retained");
            let metadata = checked_metadata(parent, &name)?;
            if metadata.is_symlink() && (follow_final || !pending.is_empty()) {
                links += 1;
                if links > 40 {
                    return Err(FsError::InvalidPath);
                }
                let expected = identity(&metadata)?;
                let target = parent.read_link_contents(&name)?;
                ensure_identity(&checked_metadata(parent, &name)?, &expected)?;
                // Only relative symlinks within the opened export are allowed.
                // Parent components pop retained capabilities, never open "..".
                for component in target.components().rev() {
                    let part = match component {
                        Component::Normal(part) => {
                            validate_component(part)?;
                            part.to_os_string()
                        }
                        Component::CurDir => OsString::from("."),
                        Component::ParentDir => OsString::from(".."),
                        _ => return Err(FsError::InvalidPath),
                    };
                    pending.push_front(part);
                }
                continue;
            }
            if pending.is_empty() {
                resolved.push(&name);
                return Ok(Resolved {
                    path: resolved,
                    parent: parent.try_clone()?,
                    name,
                    metadata,
                });
            }
            if !metadata.is_dir() {
                return Err(FsError::NotDirectory);
            }
            let directory = parent.open_dir_nofollow(&name)?;
            ensure_identity(&directory.dir_metadata()?, &identity(&metadata)?)?;
            #[cfg(windows)]
            check_windows_name(&directory)?;
            directories.push(directory);
            resolved.push(name);
        }
        let parent = directories.pop().expect("root directory retained");
        let metadata = parent.dir_metadata()?;
        Ok(Resolved {
            path: resolved,
            parent,
            name: OsString::from("."),
            metadata,
        })
    }

    fn register(&self, path: &Path, metadata: &Metadata) -> Result<Node> {
        let identity = identity_at(metadata, &self.root_path.join(path))?;
        let mut db = self.db()?;
        let tx = db.transaction()?;
        let id = persist_object(&tx, path, &identity)?;
        tx.commit()?;
        node(id, metadata)
    }

    fn resolve_id(&self, id: u64) -> Result<(PathBuf, Vec<u8>)> {
        if id == 0 || id > i64::MAX as u64 {
            return Err(FsError::Stale);
        }
        let db = self.db()?;
        let expected: Vec<u8> = db
            .query_row(
                "SELECT identity FROM objects WHERE id=?1",
                [id as i64],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(FsError::Stale)?;
        let mut statement =
            db.prepare("SELECT path FROM paths WHERE object_id=?1 ORDER BY path")?;
        let rows = statement.query_map([id as i64], |row| row.get::<_, Vec<u8>>(0))?;
        for row in rows {
            let path = decode_path(row?)?;
            validate_path(&path)?;
            match self.lstat(&path) {
                Ok(metadata) if identity(&metadata)? == expected => return Ok((path, expected)),
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound
                            | io::ErrorKind::NotADirectory
                            | io::ErrorKind::PermissionDenied
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        Err(FsError::Stale)
    }

    fn open_regular(&self, path: &Path, expected: &[u8]) -> Result<File> {
        let resolved = self.resolve_path(path, false)?;
        self.open_resolved_regular(resolved, expected)
    }

    fn open_resolved_regular(&self, resolved: Resolved, expected: &[u8]) -> Result<File> {
        ensure_identity(&resolved.metadata, expected)?;
        if !resolved.metadata.is_file() {
            return Err(FsError::NotRegular);
        }
        let mut options = OpenOptions::new();
        // openat2 rejects O_NONBLOCK with O_PATH; O_PATH itself performs no I/O.
        options
            .read(true)
            .follow(FollowSymlinks::No)
            .nonblock(!cfg!(target_os = "linux"));
        // Linux O_PATH performs no device open, even when an attacker swaps a
        // regular entry for a FIFO/device between lstat and open. Reopen only
        // after the anchored descriptor has been proved to be our regular file.
        #[cfg(target_os = "linux")]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(libc::O_PATH);
        }
        let file = resolved.parent.open_with(&resolved.name, &options)?;
        #[cfg(windows)]
        check_windows_name(&file)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(FsError::NotRegular);
        }
        ensure_identity(&metadata, expected)?;
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let reopened = File::open(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
            ensure_identity(&Metadata::from_file(&reopened)?, expected)?;
            Ok(reopened)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(file.into_std())
        }
    }
}

// The final component remains relative to its opened parent. No data open or
// directory reopen needs to traverse an already-checked path a second time.
struct Resolved {
    path: PathBuf,
    parent: Dir,
    name: OsString,
    metadata: Metadata,
}

impl Resolved {
    fn open_directory(&self) -> Result<Dir> {
        if !self.metadata.is_dir() {
            return Err(FsError::NotDirectory);
        }
        let directory = self.parent.open_dir_nofollow(&self.name)?;
        ensure_identity(&directory.dir_metadata()?, &identity(&self.metadata)?)?;
        #[cfg(windows)]
        if self.name != OsStr::new(".") {
            check_windows_name(&directory)?;
        }
        Ok(directory)
    }
}

#[cfg(not(windows))]
fn checked_metadata(parent: &Dir, name: &OsStr) -> Result<Metadata> {
    Ok(parent.symlink_metadata(name)?)
}

#[cfg(windows)]
fn checked_metadata(parent: &Dir, name: &OsStr) -> Result<Metadata> {
    use cap_std::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    // Like cap-primitives' native lstat: query attributes without data access
    // and retain the reparse point itself rather than following its target.
    let mut options = OpenOptions::new();
    options
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .follow(FollowSymlinks::No);
    let file = parent.open_with(name, &options)?.into_std();
    check_windows_name(&file)?;
    Ok(Metadata::from_file(&file)?)
}

#[cfg(windows)]
fn check_windows_name(file: &impl std::os::windows::io::AsRawHandle) -> Result<()> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    let mut stack = [0u16; 512];
    let mut heap = Vec::new();
    loop {
        let buffer = if heap.is_empty() {
            &mut stack[..]
        } else {
            &mut heap[..]
        };
        // Flags zero request the normalized (not opened/possibly short) name.
        // SAFETY: file remains live and buffer is writable for its full length.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        };
        if length == 0 {
            return Err(io::Error::last_os_error().into());
        }
        if length as usize >= buffer.len() {
            heap.resize(length as usize + 1, 0);
            continue;
        }
        let path = PathBuf::from(OsString::from_wide(&buffer[..length as usize]));
        // Only this export-relative component is policy checked. An approved
        // root can live beneath a reserved-looking host ancestor.
        return validate_component(path.file_name().ok_or(FsError::InvalidPath)?);
    }
}

/// An unpublished upload. The opened parent anchors creation, publication and
/// cleanup even if its path is concurrently renamed. The caller must drop all
/// cloned writers before committing; cloning shares the native file cursor.
pub struct Upload {
    parent: Dir,
    temporary: OsString,
    target: OsString,
    file: Option<File>,
    expected: Vec<u8>,
}

impl Upload {
    pub fn writer(&self) -> Result<File> {
        Ok(self
            .file
            .as_ref()
            .ok_or_else(|| FsError::State("upload already closed".into()))?
            .try_clone()?)
    }

    pub fn commit(self) -> Result<()> {
        let file = self
            .file
            .as_ref()
            .ok_or_else(|| FsError::State("upload already closed".into()))?;
        ensure_identity(&Metadata::from_file(file)?, &self.expected)?;
        file.sync_all()?;
        self.check_temporary()?;
        // Hard-link publication is atomic and fails if *any* target entry
        // exists, including a dangling symlink. Unlike rename it never replaces.
        self.parent
            .hard_link(&self.temporary, &self.parent, &self.target)?;
        // A host replacement between the source check and hard_link must not
        // be reported as an upload of the held writer's data. Never remove an
        // unexpected target: it may already belong to another host operation.
        self.check_target()?;
        self.remove_temporary()?;
        #[cfg(unix)]
        {
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(libc::O_DIRECTORY);
            // open_dir_nofollow can return Linux O_PATH, which cannot fsync.
            // Reopen "." for reading relative to the same anchored parent.
            self.parent.open_with(".", &options)?.sync_all()?;
        }
        self.check_target()?;
        Ok(())
    }

    fn check_temporary(&self) -> Result<()> {
        ensure_identity(
            &self.parent.symlink_metadata(&self.temporary)?,
            &self.expected,
        )
    }

    fn check_target(&self) -> Result<()> {
        ensure_identity(
            &checked_metadata(&self.parent, &self.target)?,
            &self.expected,
        )
    }

    fn remove_temporary(&self) -> Result<()> {
        self.check_temporary()?;
        self.parent.remove_file(&self.temporary)?;
        Ok(())
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        // Keep the original file alive until cleanup has checked ownership.
        // A replaced temporary name belongs to the host, not this Upload.
        let _ = self.remove_temporary();
        self.file.take();
    }
}

fn cap_path(path: &Path) -> &Path {
    if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(FsError::InvalidPath);
    }
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}

fn reserved(name: &OsStr) -> bool {
    name.as_encoded_bytes()
        .get(..RESERVED.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RESERVED.as_bytes()))
}

fn validate_component(name: &OsStr) -> Result<()> {
    let bytes = name.as_encoded_bytes();
    if bytes.is_empty()
        || bytes == b"."
        || bytes == b".."
        || reserved(name)
        || bytes.iter().any(|b| {
            matches!(
                *b,
                0..=31 | b'/' | b'\\' | b':' | b'<' | b'>' | b'\"' | b'|' | b'?' | b'*'
            )
        })
        || matches!(bytes.last(), Some(b' ' | b'.'))
    {
        return Err(FsError::InvalidPath);
    }
    let stem = bytes.split(|b| *b == b'.').next().unwrap_or_default();
    if [
        b"CON".as_slice(),
        b"PRN",
        b"AUX",
        b"NUL",
        b"CLOCK$",
        b"CONIN$",
        b"CONOUT$",
    ]
    .iter()
    .any(|s| stem.eq_ignore_ascii_case(s))
        || (stem.len() == 4
            && (stem[..3].eq_ignore_ascii_case(b"COM") || stem[..3].eq_ignore_ascii_case(b"LPT"))
            && matches!(stem[3], b'1'..=b'9'))
        || (stem.len() == 5
            && (stem[..3].eq_ignore_ascii_case(b"COM") || stem[..3].eq_ignore_ascii_case(b"LPT"))
            && [b"\xc2\xb9", b"\xc2\xb2", b"\xc2\xb3"]
                .iter()
                .any(|suffix| stem[3..] == suffix[..]))
    {
        return Err(FsError::InvalidPath);
    }
    Ok(())
}

pub(crate) fn validate_path(path: &Path) -> Result<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => {
                validate_component(name)?;
                result.push(name);
            }
            _ => return Err(FsError::InvalidPath),
        }
    }
    Ok(result)
}

fn kind(metadata: &Metadata) -> Result<NodeKind> {
    let t = metadata.file_type();
    Ok(if t.is_file() {
        NodeKind::File
    } else if t.is_dir() {
        NodeKind::Directory
    } else if t.is_symlink() {
        NodeKind::Symlink
    } else if t.is_block_device() {
        NodeKind::BlockDevice
    } else if t.is_char_device() {
        NodeKind::CharDevice
    } else if t.is_fifo() {
        NodeKind::Fifo
    } else if t.is_socket() {
        NodeKind::Socket
    } else {
        return Err(FsError::UnsupportedIdentity("unknown file type".into()));
    })
}

fn identity(metadata: &Metadata) -> Result<Vec<u8>> {
    let created = metadata
        .created()
        .map_err(|e| FsError::UnsupportedIdentity(e.to_string()))?
        .into_std();
    let duration = created
        .duration_since(UNIX_EPOCH)
        .map_err(|_| FsError::UnsupportedIdentity("pre-epoch creation time".into()))?;
    let mut bytes = Vec::with_capacity(29);
    bytes.extend_from_slice(&metadata.dev().to_le_bytes());
    bytes.extend_from_slice(&metadata.ino().to_le_bytes());
    bytes.extend_from_slice(&duration.as_secs().to_le_bytes());
    bytes.extend_from_slice(&duration.subsec_nanos().to_le_bytes());
    bytes.push(kind(metadata)? as u8);
    Ok(bytes)
}

fn ensure_identity(metadata: &Metadata, expected: &[u8]) -> Result<()> {
    if identity(metadata)? == expected {
        Ok(())
    } else {
        Err(FsError::Stale)
    }
}

fn node(id: u64, metadata: &Metadata) -> Result<Node> {
    let kind = kind(metadata)?;
    let atime = metadata.accessed()?.into_std();
    let mtime = metadata.modified()?.into_std();
    #[cfg(unix)]
    let (mode, uid, gid, ctime, rdev) = {
        use cap_std::fs::MetadataExt as UnixMetadataExt;
        let seconds = UnixMetadataExt::ctime(metadata);
        let nanos = UnixMetadataExt::ctime_nsec(metadata);
        let ctime = if seconds >= 0 {
            UNIX_EPOCH + std::time::Duration::new(seconds as u64, nanos as u32)
        } else {
            UNIX_EPOCH - std::time::Duration::from_secs(seconds.unsigned_abs())
                + std::time::Duration::from_nanos(nanos as u64)
        };
        (
            UnixMetadataExt::mode(metadata),
            UnixMetadataExt::uid(metadata),
            UnixMetadataExt::gid(metadata),
            ctime,
            UnixMetadataExt::rdev(metadata),
        )
    };
    #[cfg(windows)]
    let (mode, uid, gid, ctime, rdev) = {
        let mode = match kind {
            NodeKind::Directory => 0o040555,
            NodeKind::Symlink => 0o120777,
            _ => 0o100555,
        };
        (mode, 0, 0, metadata.created()?.into_std(), 0)
    };
    Ok(Node {
        id,
        kind,
        size: metadata.len(),
        mode,
        uid,
        gid,
        nlink: metadata.nlink().min(u32::MAX as u64) as u32,
        atime,
        mtime,
        ctime,
        rdev,
    })
}

#[cfg(unix)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}
#[cfg(unix)]
fn decode_path(bytes: Vec<u8>) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(bytes).into())
}
#[cfg(windows)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}
#[cfg(windows)]
fn decode_path(bytes: Vec<u8>) -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    if bytes.len() % 2 != 0 {
        return Err(FsError::State("invalid persisted Windows path".into()));
    }
    let wide: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Ok(OsString::from_wide(&wide).into())
}

fn identity_at(metadata: &Metadata, path: &Path) -> Result<Vec<u8>> {
    identity(metadata).map_err(|error| match error {
        FsError::UnsupportedIdentity(reason) => FsError::UnsupportedIdentity(format!(
            "{}: this filesystem must supply native creation/birth time and non-recycled native IDs: {reason}", path.display()
        )),
        other => other,
    })
}

#[cfg(windows)]
fn require_ntfs(root: &Dir, path: &Path) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationByHandleW;
    let handle = root.try_clone()?.into_std_file();
    let mut filesystem = [0u16; 64];
    // SAFETY: the borrowed directory handle is live for the call; the only
    // non-null output buffer has its stated capacity. Other outputs are unused.
    let result = unsafe {
        GetVolumeInformationByHandleW(
            handle.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let length = filesystem
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(filesystem.len());
    let filesystem = String::from_utf16_lossy(&filesystem[..length]);
    // cap-fs-ext currently exposes 64-bit file indexes. ReFS requires its
    // 128-bit identifier, while FAT timestamps are too coarse to serve as a
    // safe inode generation. Do not advertise durable handles on those volumes.
    if !filesystem.eq_ignore_ascii_case("NTFS") {
        return Err(FsError::UnsupportedIdentity(format!(
            "{}: Windows volume {filesystem} is unsupported; durable identity requires NTFS",
            path.display()
        )));
    }
    Ok(())
}

fn persist_object(tx: &rusqlite::Transaction<'_>, path: &Path, identity: &[u8]) -> Result<u64> {
    let known: Option<i64> = tx
        .query_row(
            "SELECT id FROM objects WHERE identity=?1",
            [identity],
            |r| r.get(0),
        )
        .optional()?;
    let id = match known {
        Some(id) => id,
        None => {
            tx.execute("INSERT INTO objects(identity) VALUES(?1)", [identity])?;
            tx.last_insert_rowid()
        }
    };
    if id <= 0 {
        return Err(FsError::State("invalid persisted object ID".into()));
    }
    let path = encode_path(path);
    let mapped: Option<i64> = tx
        .query_row("SELECT object_id FROM paths WHERE path=?1", [&path], |r| {
            r.get(0)
        })
        .optional()?;
    if mapped != Some(id) {
        tx.execute("INSERT INTO paths(path,object_id) VALUES(?1,?2) ON CONFLICT(path) DO UPDATE SET object_id=excluded.object_id", params![path, id])?;
    }
    Ok(id as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn lookup_keeps_the_validated_parent_capability_during_replacement() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("root");
        std::fs::create_dir_all(root.join("parent")).unwrap();
        std::fs::write(root.join("parent/child"), b"original").unwrap();
        let fs = SharedFs::open(&root, &base.path().join("state")).unwrap();
        let parent = fs.lookup(fs.root_id(), OsStr::new("parent")).unwrap();
        let (path, expected) = fs.resolve_id(parent.id).unwrap();
        let anchored = fs
            .resolve_path(&path, false)
            .unwrap()
            .open_directory()
            .unwrap();
        // Exercise exactly the window after the parent was identity checked
        // and opened, without scheduler-dependent races or test-only hooks.
        std::fs::rename(root.join("parent"), root.join("retired")).unwrap();
        std::fs::create_dir(root.join("parent")).unwrap();
        std::fs::write(root.join("parent/child"), b"replacement").unwrap();
        assert!(matches!(
            fs.lookup_child(&path, &expected, &anchored, OsStr::new("child")),
            Err(FsError::Stale)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn normalized_long_name_guard_rejects_reserved_objects_without_alias_settings() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join(".filemux-smoke").join("root");
        std::fs::create_dir_all(root.join(".filemux-hidden-directory")).unwrap();
        std::fs::write(root.join(".filemux-hidden-file"), b"hidden").unwrap();
        let fs = SharedFs::open(&root, &base.path().join("state")).unwrap();
        // Bypass lexical validation deliberately: this is the native-handle
        // guard also used when an 8.3 alias supplied no reserved prefix.
        for name in [".filemux-hidden-file", ".filemux-hidden-directory"] {
            assert!(matches!(
                checked_metadata(&fs.root, OsStr::new(name)),
                Err(FsError::InvalidPath)
            ));
        }
    }

    #[test]
    fn publication_identity_check_detects_a_replaced_target() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let fs = SharedFs::open(&root, &base.path().join("state")).unwrap();
        let upload = fs.begin_upload(Path::new("published")).unwrap();
        upload
            .parent
            .hard_link(&upload.temporary, &upload.parent, &upload.target)
            .unwrap();
        upload.check_target().unwrap();
        std::fs::remove_file(root.join("published")).unwrap();
        std::fs::write(root.join("published"), b"host replacement").unwrap();
        assert!(matches!(upload.check_target(), Err(FsError::Stale)));
        drop(upload);
        assert_eq!(
            std::fs::read(root.join("published")).unwrap(),
            b"host replacement"
        );
    }
}
