use std::path::{Path, PathBuf};

use thiserror::Error;
use uuid::Uuid;

/// macOS의 sun_path 한도(104바이트)에서 NUL 종단 1바이트를 뺀 값. Linux(108)보다 작아 이쪽에 맞춘다.
pub const MAX_SOCKET_PATH: usize = 103;

/// VM 하나의 런타임 파일 위치: `{run_dir}/vm/{uuid}/...`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    pub dir: PathBuf,
    pub qmp_socket: PathBuf,
    pub vnc_socket: PathBuf,
    pub pidfile: PathBuf,
    pub uefi_vars: PathBuf,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("socket path {path:?} is {len} bytes; the limit is {MAX_SOCKET_PATH}")]
pub struct PathError {
    pub path: PathBuf,
    pub len: usize,
}

impl RuntimePaths {
    pub fn new(run_dir: &Path, id: Uuid) -> Result<Self, PathError> {
        let dir = run_dir.join("vm").join(id.to_string());
        let paths = Self {
            qmp_socket: dir.join("qmp.sock"),
            vnc_socket: dir.join("vnc.sock"),
            pidfile: dir.join("qemu.pid"),
            uefi_vars: dir.join("uefi-vars.fd"),
            dir,
        };
        for sock in [&paths.qmp_socket, &paths.vnc_socket] {
            let len = sock.as_os_str().len();
            if len > MAX_SOCKET_PATH {
                return Err(PathError {
                    path: sock.clone(),
                    len,
                });
            }
        }
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_files_under_vm_dir() {
        let id = Uuid::nil();
        let p = RuntimePaths::new(Path::new("/run/opl"), id).unwrap();
        let dir = "/run/opl/vm/00000000-0000-0000-0000-000000000000";
        assert_eq!(p.dir, Path::new(dir));
        assert_eq!(p.qmp_socket, Path::new(dir).join("qmp.sock"));
        assert_eq!(p.vnc_socket, Path::new(dir).join("vnc.sock"));
        assert_eq!(p.pidfile, Path::new(dir).join("qemu.pid"));
        assert_eq!(p.uefi_vars, Path::new(dir).join("uefi-vars.fd"));
    }

    #[test]
    fn rejects_socket_paths_over_the_limit() {
        let long = Path::new("/").join("x".repeat(60));
        let err = RuntimePaths::new(&long, Uuid::nil()).unwrap_err();
        assert!(err.len > MAX_SOCKET_PATH);
    }
}
