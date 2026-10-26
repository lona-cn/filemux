//! ONC RPC v2, MOUNT v3 (RFC 1813 Appendix I), and portmapper v2 (RFC 1057).
//! This implementation is original; no upstream server source is incorporated.

use super::nfs;
use super::xdr::{Malformed, Reader, Writer};
use crate::fs::{FsError, NodeKind, SharedFs};
use std::sync::Arc;

pub(crate) const PORTMAP: u32 = 100000;
pub(crate) const NFS: u32 = 100003;
pub(crate) const MOUNT: u32 = 100005;

#[derive(Clone, Copy)]
pub(crate) enum Service {
    Files,
    Portmapper,
}

pub(crate) struct Dispatcher {
    pub(crate) fs: Arc<SharedFs>,
    pub(crate) export: String,
    pub(crate) nfs_port: u16,
    pub(crate) portmap_port: Option<u16>,
}

fn accepted(xid: u32, status: u32) -> Writer {
    let mut out = Writer::default();
    for word in [xid, 1, 0, 0, 0, status] {
        out.u32(word);
    }
    out
}

fn denied(xid: u32, status: u32, detail: u32) -> Vec<u8> {
    let mut out = Writer::default();
    for word in [xid, 1, 1, status, detail] {
        out.u32(word);
    }
    if status == 0 {
        out.u32(detail);
    }
    out.0
}

fn auth_sys(bytes: &[u8]) -> Result<(), Malformed> {
    let mut input = Reader::new(bytes);
    input.u32()?; // stamp
    input.opaque(255)?; // machinename, not an authenticated identity
    input.u32()?; // uid
    input.u32()?; // gid
    let groups = input.u32()?;
    if groups > 16 {
        return Err(Malformed);
    }
    for _ in 0..groups {
        input.u32()?;
    }
    input.finish()
}

impl Dispatcher {
    /// Returns no response when there is no usable transaction id or this is not a CALL.
    /// Every transport uses exactly this dispatcher, always on a blocking worker.
    pub(crate) fn dispatch(
        &self,
        bytes: &[u8],
        service: Service,
        maximum: usize,
    ) -> Option<Vec<u8>> {
        let mut input = Reader::new(bytes);
        let xid = input.u32().ok()?;
        let result = (|| -> Result<Vec<u8>, Malformed> {
            if input.u32()? != 0 {
                return Ok(Vec::new());
            }
            if input.u32()? != 2 {
                return Ok(denied(xid, 0, 2)); // RPC_MISMATCH, low=high=2
            }
            let program = input.u32()?;
            let version = input.u32()?;
            let procedure = input.u32()?;
            let credential_flavor = input.u32()?;
            let credential = match input.opaque(400) {
                Ok(credential) => credential,
                Err(_) => return Ok(denied(xid, 1, 1)), // AUTH_BADCRED
            };
            let verifier_flavor = input.u32()?;
            let verifier = match input.opaque(400) {
                Ok(verifier) => verifier,
                Err(_) => return Ok(denied(xid, 1, 3)), // AUTH_BADVERF
            };
            let auth_error = match credential_flavor {
                0 if credential.is_empty() => None,
                1 if auth_sys(credential).is_ok() => None,
                0 | 1 => Some(1), // AUTH_BADCRED
                _ => Some(2),     // AUTH_REJECTEDCRED; no GSS/Kerberos
            };
            if let Some(error) = auth_error {
                return Ok(denied(xid, 1, error));
            }
            if verifier_flavor != 0 || !verifier.is_empty() {
                return Ok(denied(xid, 1, 3)); // AUTH_BADVERF
            }
            let expected = match (service, program) {
                (Service::Files, NFS | MOUNT) => 3,
                (Service::Portmapper, PORTMAP) => 2,
                _ => return Ok(accepted(xid, 1).0), // PROG_UNAVAIL
            };
            if version != expected {
                let mut out = accepted(xid, 2); // PROG_MISMATCH
                out.u32(expected);
                out.u32(expected);
                return Ok(out.0);
            }
            let supported = match program {
                NFS => procedure <= 21,
                MOUNT => procedure <= 5,
                PORTMAP => procedure <= 4,
                _ => false,
            };
            if !supported {
                return Ok(accepted(xid, 3).0); // PROC_UNAVAIL
            }
            let mut out = accepted(xid, 0);
            match program {
                NFS => nfs::call(self, procedure, &mut input, &mut out, maximum)?,
                MOUNT => self.mount(procedure, &mut input, &mut out)?,
                PORTMAP => self.portmap(procedure, &mut input, &mut out)?,
                _ => unreachable!(),
            }
            input.finish()?;
            Ok(out.0)
        })();
        match result {
            Ok(bytes) if bytes.is_empty() => None,
            Ok(bytes) if bytes.len() <= maximum => Some(bytes),
            Ok(_) => Some(accepted(xid, 5).0), // SYSTEM_ERR, never truncate XDR
            Err(_) => Some(accepted(xid, 4).0), // GARBAGE_ARGS
        }
    }

    fn mount(
        &self,
        procedure: u32,
        input: &mut Reader<'_>,
        out: &mut Writer,
    ) -> Result<(), Malformed> {
        match procedure {
            0 | 4 => {} // NULL / UMNTALL: no per-client mount state or resources
            1 => {
                let path = input.opaque(1024)?;
                match self.mount_id(path) {
                    Ok(id) => {
                        out.u32(0);
                        nfs::handle(out, &self.fs, id);
                        out.u32(2); // auth_flavors<>
                        out.u32(0); // AUTH_NONE, anonymous trusted-LAN export
                        out.u32(1); // AUTH_SYS; uid/gid are not a security boundary
                    }
                    Err(status) => out.u32(status),
                }
            }
            2 => out.boolean(false), // DUMP: stateless mount service has no mount records
            3 => {
                input.opaque(1024)?;
            } // UMNT
            5 => {
                out.boolean(true);
                out.opaque(self.export.as_bytes());
                out.boolean(false); // no group restriction
                out.boolean(false); // end exports
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    fn mount_id(&self, path: &[u8]) -> Result<u64, u32> {
        // Check wire components before any host path conversion. Neither dot
        // traversal nor platform-specific separators may escape the export.
        if !path.starts_with(b"/")
            || path.contains(&0)
            || path.contains(&b'\\')
            || path
                .split(|byte| *byte == b'/')
                .any(|part| part == b"." || part == b"..")
        {
            return Err(13); // MNT3ERR_ACCES
        }
        let export = self.export.trim_end_matches('/').as_bytes();
        let suffix = path.strip_prefix(export).ok_or(2_u32)?; // MNT3ERR_NOENT
        if !suffix.is_empty() && !suffix.starts_with(b"/") {
            return Err(2); // "/bootleg" is not within "/boot"
        }
        let mount_error = |error: FsError| -> u32 {
            match error.kind() {
                std::io::ErrorKind::NotFound => 2,
                std::io::ErrorKind::PermissionDenied
                | std::io::ErrorKind::InvalidInput
                | std::io::ErrorKind::InvalidData => 13,
                std::io::ErrorKind::NotADirectory => 20,
                _ => 5, // MNT3ERR_IO; never return an NFS-only status
            }
        };
        let mut node = self.fs.metadata(self.fs.root_id()).map_err(mount_error)?;
        for part in suffix
            .split(|byte| *byte == b'/')
            .filter(|part| !part.is_empty())
        {
            if node.kind != NodeKind::Directory {
                return Err(20); // MNT3ERR_NOTDIR, including symlink intermediates
            }
            let name = nfs::os_name(part).map_err(|_| 13_u32)?;
            node = self.fs.lookup(node.id, &name).map_err(mount_error)?;
        }
        if node.kind != NodeKind::Directory {
            return Err(20); // only real directories can be mounted; never follow links
        }
        Ok(node.id)
    }

    fn portmap(
        &self,
        procedure: u32,
        input: &mut Reader<'_>,
        out: &mut Writer,
    ) -> Result<(), Malformed> {
        match procedure {
            0 => {}
            1..=3 => {
                let program = input.u32()?;
                let version = input.u32()?;
                let protocol = input.u32()?;
                input.u32()?; // supplied port is ignored by GETPORT
                if procedure != 3 {
                    out.boolean(false); // SET/UNSET cannot alter the configured registry
                } else {
                    let port = if protocol != 6 && protocol != 17 {
                        0
                    } else {
                        match (program, version) {
                            (NFS | MOUNT, 3) => self.nfs_port,
                            (PORTMAP, 2) => self.portmap_port.unwrap_or(0),
                            _ => 0,
                        }
                    };
                    out.u32(u32::from(port));
                }
            }
            4 => {
                for protocol in [6, 17] {
                    for (program, version, port) in [
                        (PORTMAP, 2, self.portmap_port.unwrap_or(0)),
                        (NFS, 3, self.nfs_port),
                        (MOUNT, 3, self.nfs_port),
                    ] {
                        if port == 0 {
                            continue;
                        }
                        out.boolean(true);
                        out.u32(program);
                        out.u32(version);
                        out.u32(protocol);
                        out.u32(u32::from(port));
                    }
                }
                out.boolean(false);
            }
            _ => unreachable!(),
        }
        Ok(())
    }
}
