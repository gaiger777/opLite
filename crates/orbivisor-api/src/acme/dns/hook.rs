//! 훅 공급자: `{path} add|remove {fqdn} {value}`를 이 노드에서 부른다(acme.sh·lego의 공급자를 잇는 길). 파일은 훅
//! 디렉터리 아래(`/etc/orbivisor/acme-hooks/` — 스펙 검증), root(또는 opld를 돌리는 사용자) 소유, 그룹·다른 사용자가 쓸 수
//! 없어야 한다. 환경은 비우고(`PATH`만) 60초 한도, 표준 출력·오류는 4 KiB까지만 읽어 이 노드 로그(debug)에만 남긴다.
//! 실패하면 오류(인증서 status `error` — `cluster.read`가 보고 경보 알림에도 실린다)에는 끝 상태와 표준 오류 첫 줄
//! 200자까지만 싣는다(lego·acme.sh를 감싼 훅은 실패할 때 API 자격 증명이나 요청 덤프를 찍곤 한다).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;

const TIMEOUT: Duration = Duration::from_secs(60);
const OUTPUT_LIMIT: usize = 4096;
/// 오류에 싣는 표준 오류 첫 줄의 한도(글자)
const ERROR_LINE_LIMIT: usize = 200;

/// 오류에 실을 표준 오류 요약: 첫 줄, 200자까지
fn first_line(err: &str) -> String {
    let line = err.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(ERROR_LINE_LIMIT).collect();
    if line.chars().count() > ERROR_LINE_LIMIT {
        out.push('…');
    }
    out
}

pub struct Hook {
    /// 실제 파일(노드 설정의 훅 뿌리 아래로 옮긴 경로)
    pub path: PathBuf,
}

/// 스펙의 경로(`/etc/orbivisor/acme-hooks/x`, 옛 `/etc/opl/acme-hooks/x`)를 이 노드의 훅 뿌리 아래 경로로
pub fn resolve(spec_path: &str, root: &Path) -> Option<PathBuf> {
    orbivisor_types::acme::hook_rest(spec_path).map(|rest| root.join(rest))
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
        if !out.is_empty() || !err.is_empty() {
            tracing::debug!(hook = %self.path.display(), action, stdout = %out, stderr = %err, "ACME DNS hook output");
        }
        if !status.success() {
            let line = first_line(&err);
            return Err(format!(
                "hook {} {action} failed ({status}){}",
                self.path.display(),
                if line.is_empty() {
                    String::new()
                } else {
                    format!(": {line}")
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
            resolve("/etc/orbivisor/acme-hooks/a/b.sh", dir.path()),
            Some(dir.path().join("a/b.sh"))
        );
        assert_eq!(resolve("/etc/orbivisor/acme-hooks/../x", dir.path()), None);
        assert_eq!(
            resolve("/etc/opl/acme-hooks/a/b.sh", dir.path()),
            Some(dir.path().join("a/b.sh"))
        );
        assert_eq!(resolve("/usr/bin/x", dir.path()), None);
        let path = dir.path().join("h.sh");
        let log = dir.path().join("log");
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"$1 $2 $3 ${{HOME:-nohome}}\" >> {}\n[ \"$1\" = add ] || {{ echo bad >&2; echo AWS_SECRET=hunter2 >&2; exit 3; }}\n", log.display()),
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
        // 표준 오류는 첫 줄만(뒤 줄의 비밀은 status·경보에 가지 않는다 — 리뷰 1)
        assert!(err.contains("failed") && err.ends_with(": bad"), "{err}");
        assert!(!err.contains("hunter2"), "{err}");
        assert_eq!(first_line(&"x".repeat(300)).chars().count(), 201);
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
