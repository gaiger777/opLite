//! 라이브 마이그레이션의 하이퍼바이저 밖 준비물: QEMU TLS 자격 증명 파일, 주소, 포트.

use std::io::Write;
use std::ops::RangeInclusive;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 마이그레이션을 보낼 곳
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateTarget {
    /// 받는 노드의 주소(호스트 이름이나 IP)
    pub host: String,
    /// 받는 QEMU가 연 포트(`start_incoming`의 결과)
    pub port: u16,
    /// 받는 노드 인증서에서 확인할 이름(인증서의 SAN에 있어야 한다)
    pub tls_hostname: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateOptions {
    /// 최대 대역폭(바이트/초). `None`이면 QEMU 기본값
    pub bandwidth: Option<u64>,
    /// 이 시간 안에 끝나지 않으면 취소한다(원본은 계속 돈다).
    pub timeout: Duration,
}

/// QEMU의 TLS 자격 증명 객체 id. 보내는 쪽과 받는 쪽 모두 이 이름으로 만든다.
pub const MIGRATION_TLS_ID: &str = "opl-mig-tls";

const CA_FILE: &str = "ca-cert.pem";
/// QEMU `tls-creds-x509`가 읽는 이름. 노드 인증서 하나를 받는 쪽(server)과 보내는 쪽(client)에 같이 쓴다.
const CERT_FILES: [&str; 2] = ["server-cert.pem", "client-cert.pem"];
const KEY_FILES: [&str; 2] = ["server-key.pem", "client-key.pem"];

/// QEMU에 줄 TLS 자격 증명 디렉터리 `{state_dir}/migration-tls`
pub fn migration_tls_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("migration-tls")
}

/// 클러스터 CA, 노드 인증서와 키를 QEMU `tls-creds-x509`의 파일 이름으로 `dir`에 쓴다. 디렉터리는
/// 주인만(0700), 키는 주인만 읽을 수 있다(0600). 이미 있으면 바꾼다(인증서 갱신). 파일마다 임시
/// 파일에 쓴 뒤 이름을 바꾸므로 QEMU가 읽는 도중에 반쪽 파일을 보지 않는다.
pub fn write_migration_tls(
    dir: &Path,
    ca_pem: &str,
    cert_pem: &str,
    key_pem: &str,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let files = [(CA_FILE, ca_pem, 0o644)]
        .into_iter()
        .chain(CERT_FILES.map(|f| (f, cert_pem, 0o644)))
        .chain(KEY_FILES.map(|f| (f, key_pem, 0o600)));
    for (name, content, mode) in files {
        let tmp = dir.join(format!(".{name}.tmp"));
        match std::fs::remove_file(&tmp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, dir.join(name))?;
    }
    Ok(())
}

/// QEMU 마이그레이션 URI `tcp:host:port`. IPv6 주소는 괄호로 감싼다.
pub fn tcp_uri(host: &str, port: u16) -> String {
    format!("tcp:{}", socket_addr(host, port))
}

fn socket_addr(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// `ports`에서 `host`에 지금 열 수 있는 첫 포트. 잠깐 열어 보고 닫는다. QEMU의 `migrate-incoming`은
/// 한 번만 부를 수 있어 포트가 쓰는 중이면 그 QEMU를 버려야 하므로 미리 고른다.
pub(crate) fn pick_port(host: &str, ports: RangeInclusive<u16>) -> Option<u16> {
    ports
        .into_iter()
        .find(|&port| std::net::TcpListener::bind(socket_addr(host, port)).is_ok())
}

/// `qemu-system-* --version`의 첫 줄(`QEMU emulator version 8.2.2 (Debian ...)`)에서 버전을 읽는다.
/// QMP `query-version`의 `major.minor.micro`와 같은 모양이다.
pub(crate) fn parse_qemu_version(output: &str) -> Option<String> {
    let rest = output.lines().next()?.split("version ").nth(1)?;
    let version = rest.split_whitespace().next()?;
    version
        .split('.')
        .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .then(|| version.to_string())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    #[test]
    fn writes_tls_files_with_private_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = migration_tls_dir(tmp.path());
        assert_eq!(dir, tmp.path().join("migration-tls"));
        write_migration_tls(&dir, "CA", "CERT", "KEY").unwrap();

        let mode = |name: &str| std::fs::metadata(dir.join(name)).unwrap().mode() & 0o777;
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
        assert_eq!(read("ca-cert.pem"), "CA");
        for name in ["server-cert.pem", "client-cert.pem"] {
            assert_eq!(read(name), "CERT");
            assert_eq!(mode(name), 0o644);
        }
        for name in ["server-key.pem", "client-key.pem"] {
            assert_eq!(read(name), "KEY");
            assert_eq!(mode(name), 0o600, "{name}");
        }

        // 인증서가 바뀌면 다시 쓴다. 남은 임시 파일도 넘어간다.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join(".server-key.pem.tmp"), "half").unwrap();
        write_migration_tls(&dir, "CA2", "CERT2", "KEY2").unwrap();
        assert_eq!(read("server-key.pem"), "KEY2");
        assert_eq!(read("client-cert.pem"), "CERT2");
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "ca-cert.pem",
                "client-cert.pem",
                "client-key.pem",
                "server-cert.pem",
                "server-key.pem"
            ]
        );
    }

    #[test]
    fn uri_brackets_ipv6() {
        assert_eq!(tcp_uri("10.0.0.2", 60000), "tcp:10.0.0.2:60000");
        assert_eq!(tcp_uri("node-b.lan", 60001), "tcp:node-b.lan:60001");
        assert_eq!(tcp_uri("fd00::2", 60002), "tcp:[fd00::2]:60002");
        assert_eq!(tcp_uri("[fd00::2]", 60002), "tcp:[fd00::2]:60002");
    }

    #[test]
    fn picks_first_port_that_is_free() {
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = busy.local_addr().unwrap().port();
        assert_eq!(pick_port("127.0.0.1", port..=port), None);
        drop(busy);
        assert_eq!(pick_port("127.0.0.1", port..=port), Some(port));
    }

    #[test]
    fn parses_qemu_version_output() {
        assert_eq!(
            parse_qemu_version(
                "QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.4)\nCopyright (c) 2003-2023 Fabrice Bellard\n"
            )
            .as_deref(),
            Some("8.2.2")
        );
        assert_eq!(
            parse_qemu_version("QEMU emulator version 11.1.2\nCopyright\n").as_deref(),
            Some("11.1.2")
        );
        assert_eq!(parse_qemu_version("qemu: unknown option"), None);
        assert_eq!(parse_qemu_version("version x.y"), None);
        assert_eq!(parse_qemu_version(""), None);
    }
}
