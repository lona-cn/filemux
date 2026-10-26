use filemux_fs::{FsError, MAX_DIRECTORY_ENTRIES, NodeKind, SharedFs};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::Path;
use tempfile::TempDir;

struct Fixture {
    base: TempDir,
}
impl Fixture {
    fn new() -> Self {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir(base.path().join("root")).unwrap();
        Self { base }
    }
    fn root(&self) -> std::path::PathBuf {
        self.base.path().join("root")
    }
    fn state(&self) -> std::path::PathBuf {
        self.base.path().join("state")
    }
    fn open(&self) -> SharedFs {
        SharedFs::open(&self.root(), &self.state()).unwrap()
    }
}

fn contents(mut file: std::fs::File) -> Vec<u8> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn durable_export_and_object_handles_survive_reopen_and_content_change() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.root().join("folder")).unwrap();
    std::fs::write(fixture.root().join("folder/data"), b"first").unwrap();
    let fs = fixture.open();
    let export = fs.export_id();
    let root = fs.root_id();
    let folder = fs.lookup(root, OsStr::new("folder")).unwrap();
    let file = fs.lookup(folder.id, OsStr::new("data")).unwrap();
    assert_eq!(file.kind, NodeKind::File);
    drop(fs);
    std::fs::write(fixture.root().join("folder/data"), b"changed").unwrap();
    let fs = fixture.open();
    assert_eq!(fs.export_id(), export);
    assert_eq!(fs.root_id(), root);
    assert_eq!(fs.metadata(folder.id).unwrap().kind, NodeKind::Directory);
    assert_eq!(
        fs.lookup_path(Path::new("folder/data")).unwrap().id,
        file.id
    );
    assert_eq!(contents(fs.open_by_id(file.id).unwrap()), b"changed");
}

#[test]
fn replacement_path_never_aliases_an_old_handle_even_after_restart() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root().join("data"), b"old").unwrap();
    let fs = fixture.open();
    let old = fs.lookup_path(Path::new("data")).unwrap().id;
    drop(fs);
    // Keep the old inode alive, making this deterministic even on filesystems
    // that aggressively recycle recently deleted inode numbers.
    std::fs::rename(fixture.root().join("data"), fixture.root().join("retired")).unwrap();
    std::fs::write(fixture.root().join("data"), b"replacement").unwrap();
    let fs = fixture.open();
    assert!(matches!(fs.metadata(old), Err(FsError::Stale)));
    assert!(fs.open_by_id(old).is_err());
    let new = fs.lookup_path(Path::new("data")).unwrap().id;
    assert_ne!(new, old);
    drop(fs);
    let fs = fixture.open();
    assert!(matches!(fs.metadata(old), Err(FsError::Stale)));
    assert_eq!(contents(fs.open_by_id(new).unwrap()), b"replacement");
}

#[test]
fn replacement_root_or_selected_root_mismatch_is_an_explicit_error() {
    let fixture = Fixture::new();
    let fs = fixture.open();
    let export = fs.export_id();
    drop(fs);
    let another = fixture.base.path().join("another-root");
    std::fs::create_dir(&another).unwrap();
    let error = match SharedFs::open(&another, &fixture.state()) {
        Ok(_) => panic!("different root reused the same state store"),
        Err(error) => error,
    };
    assert!(matches!(error, FsError::State(_)));
    assert_eq!(fixture.open().export_id(), export);
    std::fs::rename(fixture.root(), fixture.base.path().join("old-root")).unwrap();
    std::fs::create_dir(fixture.root()).unwrap();
    assert!(matches!(
        SharedFs::open(&fixture.root(), &fixture.state()),
        Err(FsError::State(_))
    ));
}

#[test]
fn incomplete_or_corrupt_state_is_not_silently_reset() {
    let fixture = Fixture::new();
    drop(fixture.open());
    let before = std::fs::read(fixture.state().join("store-id")).unwrap();
    std::fs::remove_file(fixture.state().join("objects.sqlite3")).unwrap();
    assert!(matches!(
        SharedFs::open(&fixture.root(), &fixture.state()),
        Err(FsError::State(_))
    ));
    assert_eq!(
        std::fs::read(fixture.state().join("store-id")).unwrap(),
        before
    );
    std::fs::write(
        fixture.state().join("objects.sqlite3"),
        b"not a SQLite database",
    )
    .unwrap();
    assert!(SharedFs::open(&fixture.root(), &fixture.state()).is_err());
    assert_eq!(
        std::fs::read(fixture.state().join("store-id")).unwrap(),
        before
    );
}

#[test]
fn state_must_be_outside_export_and_rejection_does_not_create_it() {
    let fixture = Fixture::new();
    let nested = fixture.root().join("new/state");
    assert!(SharedFs::open(&fixture.root(), &nested).is_err());
    assert!(!fixture.root().join("new").exists());
}

#[test]
fn path_validation_and_nfs_parent_clamping_are_shared() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.root().join("folder")).unwrap();
    let fs = fixture.open();
    for invalid in [
        "../outside",
        "/outside",
        "C:\\outside",
        "\\\\server\\share",
        "file:stream",
        ".filemux-hidden",
        "folder/../outside",
        "NUL",
        "COM1.txt",
        "trailing.",
    ] {
        assert!(
            fs.lookup_path(Path::new(invalid)).is_err(),
            "lookup accepted {invalid}"
        );
        assert!(
            fs.open_read(Path::new(invalid)).is_err(),
            "read accepted {invalid}"
        );
        assert!(
            fs.begin_upload(Path::new(invalid)).is_err(),
            "upload accepted {invalid}"
        );
    }
    let root = fs.root_id();
    assert_eq!(fs.lookup_path(Path::new("")).unwrap().id, root);
    assert_eq!(fs.lookup(root, OsStr::new(".")).unwrap().id, root);
    assert_eq!(fs.lookup(root, OsStr::new("..")).unwrap().id, root);
    let folder = fs.lookup(root, OsStr::new("folder")).unwrap().id;
    assert_eq!(fs.lookup(folder, OsStr::new("..")).unwrap().id, root);
    assert!(fs.lookup(root, OsStr::new("folder/file")).is_err());
    assert!(fs.open_by_id(root).is_err());
    assert!(fs.begin_upload(Path::new("missing/data")).is_err());
}

#[test]
fn uploads_remain_invisible_until_atomic_publish_and_drop_cleans_up() {
    let fixture = Fixture::new();
    let fs = fixture.open();
    let upload = fs.begin_upload(Path::new("completed")).unwrap();
    let mut writer = upload.writer().unwrap();
    writer.write_all(b"all bytes").unwrap();
    writer.sync_all().unwrap();
    drop(writer);
    assert!(fs.lookup_path(Path::new("completed")).is_err());
    assert!(fs.open_read(Path::new("completed")).is_err());
    assert!(fs.list(fs.root_id()).unwrap().is_empty());
    for native in std::fs::read_dir(fixture.root()).unwrap() {
        let temporary = native.unwrap().file_name();
        assert!(temporary.to_string_lossy().starts_with(".filemux-"));
        assert!(fs.lookup(fs.root_id(), &temporary).is_err());
        assert!(fs.open_read(Path::new(&temporary)).is_err());
    }
    upload.commit().unwrap();
    assert_eq!(
        contents(fs.open_read(Path::new("completed")).unwrap()),
        b"all bytes"
    );
    let abandoned = fs.begin_upload(Path::new("abandoned")).unwrap();
    abandoned.writer().unwrap().write_all(b"partial").unwrap();
    drop(abandoned);
    let names: Vec<_> = std::fs::read_dir(fixture.root())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("completed")]);
}

#[test]
fn upload_does_not_clobber_existing_or_concurrently_created_target() {
    let fixture = Fixture::new();
    let fs = fixture.open();
    std::fs::write(fixture.root().join("existing"), b"keep me").unwrap();
    assert_eq!(
        fs.begin_upload(Path::new("existing")).err().unwrap().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    let upload = fs.begin_upload(Path::new("raced")).unwrap();
    upload.writer().unwrap().write_all(b"upload").unwrap();
    std::fs::write(fixture.root().join("raced"), b"other writer").unwrap();
    assert_eq!(
        upload.commit().unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        std::fs::read(fixture.root().join("raced")).unwrap(),
        b"other writer"
    );
    assert_eq!(
        std::fs::read(fixture.root().join("existing")).unwrap(),
        b"keep me"
    );
    assert!(std::fs::read_dir(fixture.root()).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".filemux-")
    }));
}

#[test]
fn replaced_upload_temporary_is_not_published_or_removed() {
    for commit in [false, true] {
        let fixture = Fixture::new();
        let fs = fixture.open();
        let upload = fs.begin_upload(Path::new("completed")).unwrap();
        upload.writer().unwrap().write_all(b"held upload").unwrap();
        let temporary = std::fs::read_dir(fixture.root())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".filemux-")
            })
            .unwrap();
        let retired = fixture.root().join(".filemux-retained-original");
        std::fs::rename(&temporary, &retired).unwrap();
        std::fs::write(&temporary, b"host replacement").unwrap();
        if commit {
            assert!(matches!(upload.commit(), Err(FsError::Stale)));
        } else {
            drop(upload);
        }
        assert!(!fixture.root().join("completed").exists());
        assert_eq!(std::fs::read(&temporary).unwrap(), b"host replacement");
        assert_eq!(std::fs::read(&retired).unwrap(), b"held upload");
    }
}

#[test]
fn lookup_refuses_children_of_a_replaced_parent_handle() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.root().join("parent")).unwrap();
    std::fs::write(fixture.root().join("parent/child"), b"original").unwrap();
    let fs = fixture.open();
    let parent = fs.lookup(fs.root_id(), OsStr::new("parent")).unwrap();
    std::fs::rename(
        fixture.root().join("parent"),
        fixture.root().join("retired"),
    )
    .unwrap();
    std::fs::create_dir(fixture.root().join("parent")).unwrap();
    std::fs::write(fixture.root().join("parent/child"), b"replacement").unwrap();
    assert!(matches!(
        fs.lookup(parent.id, OsStr::new("child")),
        Err(FsError::Stale)
    ));
}

#[test]
fn directory_limit_errors_instead_of_silently_truncating() {
    let fixture = Fixture::new();
    for index in 0..=MAX_DIRECTORY_ENTRIES {
        std::fs::File::create(fixture.root().join(format!("entry-{index}"))).unwrap();
    }
    let fs = fixture.open();
    assert!(matches!(
        fs.list(fs.root_id()),
        Err(FsError::DirectoryTooLarge)
    ));
}

#[cfg(unix)]
#[test]
fn rooted_links_are_safe_and_native_lstat_readlink_semantics_are_preserved() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.root().join("folder")).unwrap();
    std::fs::write(fixture.root().join("real"), b"inside").unwrap();
    std::fs::write(fixture.base.path().join("secret"), b"outside").unwrap();
    symlink("../real", fixture.root().join("folder/safe")).unwrap();
    symlink("../secret", fixture.root().join("escape")).unwrap();
    symlink(
        fixture.base.path().join("secret"),
        fixture.root().join("absolute"),
    )
    .unwrap();
    symlink("../state", fixture.root().join("state-link")).unwrap();
    let fs = fixture.open();
    let link = fs.lookup_path(Path::new("folder/safe")).unwrap();
    assert_eq!(link.kind, NodeKind::Symlink);
    assert_eq!(fs.read_link(link.id).unwrap(), Path::new("../real"));
    assert!(fs.open_by_id(link.id).is_err());
    assert_eq!(
        contents(fs.open_read(Path::new("folder/safe")).unwrap()),
        b"inside"
    );
    for outside in ["escape", "absolute", "state-link/objects.sqlite3"] {
        assert!(fs.open_read(Path::new(outside)).is_err());
    }
    let absolute = fs.lookup_path(Path::new("absolute")).unwrap();
    assert_eq!(
        fs.read_link(absolute.id).unwrap(),
        fixture.base.path().join("secret")
    );
    let upload = fs.begin_upload(Path::new("pending")).unwrap();
    let temporary = std::fs::read_dir(fixture.root())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .find(|n| n.to_string_lossy().starts_with(".filemux-"))
        .unwrap();
    symlink(&temporary, fixture.root().join("temp-alias")).unwrap();
    assert!(fs.open_read(Path::new("temp-alias")).is_err());
    drop(upload);
}

#[cfg(unix)]
#[test]
fn native_permissions_and_special_objects_are_not_flattened_into_data_files() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    let fixture = Fixture::new();
    std::fs::write(fixture.root().join("executable"), b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(
        fixture.root().join("executable"),
        std::fs::Permissions::from_mode(0o751),
    )
    .unwrap();
    let socket = UnixListener::bind(fixture.root().join("socket")).unwrap();
    let fs = fixture.open();
    let executable = fs.lookup_path(Path::new("executable")).unwrap();
    assert_eq!(executable.mode & 0o777, 0o751);
    let special = fs.lookup_path(Path::new("socket")).unwrap();
    assert_eq!(special.kind, NodeKind::Socket);
    assert!(fs.open_by_id(special.id).is_err());
    assert!(fs.open_read(Path::new("socket")).is_err());
    drop(socket);
}

#[cfg(unix)]
#[test]
fn fifo_and_internal_fifo_symlink_reads_are_refused_without_waiting_for_a_writer() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::sync::mpsc;
    use std::time::Duration;
    let fixture = Fixture::new();
    let fifo = CString::new(fixture.root().join("pipe").as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo is a live NUL-terminated path; mkfifo has no pointer outputs.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    symlink("pipe", fixture.root().join("pipe-link")).unwrap();
    symlink("loop-b", fixture.root().join("loop-a")).unwrap();
    symlink("loop-a", fixture.root().join("loop-b")).unwrap();
    let fs = fixture.open();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        for path in ["pipe", "pipe-link"] {
            assert!(matches!(
                fs.open_read(Path::new(path)),
                Err(FsError::NotRegular)
            ));
        }
        assert!(fs.open_read(Path::new("loop-a")).is_err());
        send.send(()).unwrap();
    });
    receive
        .recv_timeout(Duration::from_secs(5))
        .expect("metadata-only resolution must not wait for a FIFO writer");
}

#[cfg(unix)]
#[test]
fn uploads_remain_anchored_to_the_opened_parent() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.root().join("parent")).unwrap();
    let fs = fixture.open();
    let upload = fs.begin_upload(Path::new("parent/data")).unwrap();
    upload
        .writer()
        .unwrap()
        .write_all(b"original parent")
        .unwrap();
    std::fs::rename(
        fixture.root().join("parent"),
        fixture.root().join("renamed"),
    )
    .unwrap();
    std::fs::create_dir(fixture.root().join("parent")).unwrap();
    upload.commit().unwrap();
    assert!(!fixture.root().join("parent/data").exists());
    assert_eq!(
        std::fs::read(fixture.root().join("renamed/data")).unwrap(),
        b"original parent"
    );
}

#[cfg(windows)]
#[test]
fn windows_junction_escape_is_refused_without_extra_privileges() {
    let fixture = Fixture::new();
    let outside = fixture.base.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret"), b"outside").unwrap();
    junction::create(&outside, fixture.root().join("escape")).unwrap();
    let fs = fixture.open();
    assert!(fs.open_read(Path::new("escape/secret")).is_err());
    assert!(fs.lookup_path(Path::new("escape/secret")).is_err());
    assert!(fs.begin_upload(Path::new("escape/new-file")).is_err());
    let names: Vec<_> = std::fs::read_dir(&outside)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [std::ffi::OsString::from("secret")]);
}

#[cfg(windows)]
#[test]
fn windows_virtual_export_modes_allow_execution_without_changing_host_metadata() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root().join("program"), b"ELF-like test content").unwrap();
    let native_before = std::fs::metadata(fixture.root().join("program"))
        .unwrap()
        .permissions()
        .readonly();
    let fs = fixture.open();
    let program = fs.lookup_path(Path::new("program")).unwrap();
    assert_eq!(program.mode, 0o100555);
    assert_eq!(fs.metadata(fs.root_id()).unwrap().mode, 0o040555);
    assert_eq!((program.uid, program.gid, program.rdev), (0, 0, 0));
    assert_eq!(
        std::fs::metadata(fixture.root().join("program"))
            .unwrap()
            .permissions()
            .readonly(),
        native_before
    );
}

#[cfg(windows)]
#[test]
fn windows_normalized_name_checks_ignore_host_ancestors_and_allow_literal_tildes() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join(".filemux-smoke").join("export");
    std::fs::create_dir_all(root.join("normal~folder")).unwrap();
    std::fs::write(root.join("normal~folder/release~candidate"), b"normal name").unwrap();
    let fs = SharedFs::open(&root, &base.path().join("state")).unwrap();
    assert_eq!(
        contents(
            fs.open_read(Path::new("normal~folder/release~candidate"))
                .unwrap()
        ),
        b"normal name"
    );
    let upload = fs
        .begin_upload(Path::new("normal~folder/upload~candidate"))
        .unwrap();
    upload.writer().unwrap().write_all(b"upload").unwrap();
    upload.commit().unwrap();
    assert_eq!(
        contents(
            fs.open_read(Path::new("normal~folder/upload~candidate"))
                .unwrap()
        ),
        b"upload"
    );
}

#[cfg(windows)]
#[test]
fn windows_assigned_short_aliases_cannot_expose_reserved_files_or_directories() {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    fn short_name(path: &Path) -> std::ffi::OsString {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: wide is NUL-terminated; the first call only queries size.
        let size = unsafe { GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0) };
        assert_ne!(
            size,
            0,
            "short-name query failed: {}",
            std::io::Error::last_os_error()
        );
        let mut output = vec![0; size as usize];
        // SAFETY: output has the size reported by the native API.
        let length = unsafe { GetShortPathNameW(wide.as_ptr(), output.as_mut_ptr(), size) };
        assert!(length > 0 && length < size);
        output.truncate(length as usize);
        let short = std::path::PathBuf::from(std::ffi::OsString::from_wide(&output));
        short.file_name().unwrap().to_os_string()
    }
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.root().join(".filemux-private-directory")).unwrap();
    std::fs::write(
        fixture.root().join(".filemux-private-directory/data"),
        b"hidden directory",
    )
    .unwrap();
    let fs = fixture.open();
    let upload = fs.begin_upload(Path::new("unpublished")).unwrap();
    upload
        .writer()
        .unwrap()
        .write_all(b"hidden upload")
        .unwrap();
    let temporary = std::fs::read_dir(fixture.root())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .find(|name| name.to_string_lossy().starts_with(".filemux-upload-"))
        .unwrap();
    let mut aliases = 0;
    for long in [
        temporary,
        std::ffi::OsString::from(".filemux-private-directory"),
    ] {
        let short = short_name(&fixture.root().join(&long));
        if short == long {
            eprintln!(
                "NTFS did not assign an 8.3 alias to {long:?}; native normalized-name guard is covered separately, alias branch unavailable"
            );
            continue;
        }
        aliases += 1;
        assert!(fs.lookup(fs.root_id(), &short).is_err());
        assert!(fs.lookup_path(Path::new(&short)).is_err());
        assert!(fs.open_read(Path::new(&short)).is_err());
        assert!(fs.open_read(&Path::new(&short).join("data")).is_err());
        assert!(
            fs.begin_upload(&Path::new(&short).join("new-file"))
                .is_err()
        );
    }
    eprintln!("Exercised {aliases}/2 assigned NTFS short aliases; no volume settings changed");
    drop(upload);
}
