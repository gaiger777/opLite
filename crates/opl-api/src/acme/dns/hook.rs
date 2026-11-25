//! 훅 공급자: `{path} add|remove {fqdn} {value}`를 이 노드에서 부른다(acme.sh·lego의 공급자를 잇는 길). 파일은 훅
//! 디렉터리 아래(`/etc/opl/acme-hooks/` — 스펙 검증), root(또는 opld를 돌리는 사용자) 소유, 그룹·다른 사용자가 쓸 수
//! 없어야 한다. 환경은 비우고(`PATH`만) 60초 한도, 표준 출력·오류는 4 KiB까지만 남긴다.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;

const TIMEOUT: Duration = Duration::from_secs(60);
const OUTPUT_LIMIT: usize = 4096;

pub struct Hook {
    /// 실제 파일(노드 설정의 훅 뿌리 아래로 옮긴 경로)
    pub path: PathBuf,
}

/// 스펙의 경로(`/etc/opl/acme-hooks/x`)를 이 노드의 훅 뿌리 아래 경로로
pub fn resolve(spec_path: &str, root: &Path) -> Option<PathBuf> {
    let rest = spec_path.strip_prefix(opl_types::acme::HOOK_DIR)?;
    opl_types::acme::hook_path_ok(spec_path).then(|| root.join(rest))
}

/// 주인·권한 검사
fn check_file(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).map_err(|e| format!("hook {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("hook {} is not a file", path.display()));
    }
    // SAFETY: geteuid는 실패하지 않는다.
    let me = unsafe { libc::geteuid() };
    if meta.uid() != 0 && meta.uid() != me {
        return Err(format!("hook {} must be owned by root", path.display()));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!(
            "hook {} must not be writable by group or others",
            path.display()
        ));
    }
    if meta.mode() & 0o100 == 0 {
        return Err(format!("hook {} is not executable", path.display()));
    }
    Ok(())
}

async fn read_limited(mut r: impl tokio::io::AsyncRead + Unpin) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while let Ok(n) = r.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        if buf.len() < OUTPUT_LIMIT {
            buf.extend_from_slice(&chunk[..n.min(OUTPUT_LIMIT - buf.len())]);
        }
    }
    String::from_utf8_lossy(&buf).trim().to_string()
}

impl Hook {
    pub async fn run(&self, action: &str, fqdn: &str, value: &str) -> Result<(), String> {
        check_file(&self.path)?;
        let mut child = tokio::process::Command::new(&self.path)
            .args([action, fqdn, value])
            .env_clear()
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("hook {}: {e}", self.path.display()))?;
        let out = read_limited(child.stdout.take().expect("piped"));
        let err = read_limited(child.stderr.take().expect("piped"));
        let run = async {
            let (out, err) = tokio::join!(out, err);
            (child.wait().await, out, err)
        };
        let (status, out, err) = tokio::time::timeout(TIMEOUT, run)
            .await
            .map_err(|_| format!("hook {} {action} timed out after 60s", self.path.display()))?;
        let status = status.map_err(|e| format!("hook {}: {e}", self.path.display()))?;
        if !out.is_empty() {
            tracing::info!(hook = %self.path.display(), action, output = %out, "ACME DNS hook");
        }
        if !status.success() {
            return Err(format!(
                "hook {} {action} failed ({status}){}",
                self.path.display(),
                if err.is_empty() {
                    String::new()
                } else {
                    format!(": {err}")
                }
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn runs_checked_hooks_only() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(
            resolve("/etc/opl/acme-hooks/a/b.sh", dir.path()),
            Some(dir.path().join("a/b.sh"))
        );
        assert_eq!(resolve("/etc/opl/acme-hooks/../x", dir.path()), None);
        assert_eq!(resolve("/usr/bin/x", dir.path()), None);
        let path = dir.path().join("h.sh");
        let log = dir.path().join("log");
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"$1 $2 $3 ${{HOME:-nohome}}\" >> {}\n[ \"$1\" = add ] || {{ echo bad >&2; exit 3; }}\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hook = Hook { path: path.clone() };
        hook.run("add", "_acme-challenge.a.test", "v1")
            .await
            .unwrap();
        let err = hook
            .run("remove", "_acme-challenge.a.test", "v1")
            .await
            .unwrap_err();
        assert!(err.contains("failed") && err.ends_with(": bad"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "add _acme-challenge.a.test v1 nohome\nremove _acme-challenge.a.test v1 nohome\n"
        );
        // 다른 사용자가 쓸 수 있으면 부르지 않는다.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o757)).unwrap();
        assert!(
            hook.run("add", "x", "y")
                .await
                .unwrap_err()
                .contains("writable")
        );
    }
}
