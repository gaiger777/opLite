//! 라이브 마이그레이션의 하이퍼바이저 밖 준비물: QEMU TLS 자격 증명 파일, 주소, 포트, 디스크 미러.

use std::io::Write;
use std::ops::RangeInclusive;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use orbivisor_types::runtime::ImageFormat;
use tokio::sync::watch;
use uuid::Uuid;

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
    /// [`mirror_disks`](crate::Hypervisor::mirror_disks)로 시작한 디스크 미러를 스위치오버 직전
    /// (`pre-switchover`, 게스트가 멈춘 때)에 끝낸 뒤 이어 간다.
    pub switchover_mirrors: bool,
    /// [`catch_up_disks`](crate::Hypervisor::catch_up_disks)로 따라잡은 디스크(계획 55, 대상의 복제본)에 스위치오버 직전
    /// (`pre-switchover`, 게스트가 멈춘 때) 마지막 패스를 보낸 뒤 이어 간다.
    pub switchover_catch_up: Option<CatchUpState>,
    /// 받는 쪽이 켤 핫플러그·디스크·NIC 장치 id([`orbivisor_types::hotplug::qemu_devices`]). 있으면 보내기 전에 원본
    /// QEMU의 `/machine/peripheral`과 견주고, 다르면(게스트가 아직 내려 주지 않은 장치 등) 보내지 않는다.
    pub devices: Option<Vec<String>>,
}

/// 받는 QEMU가 연 포트
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncomingPorts {
    /// 마이그레이션 스트림
    pub migration: u16,
    /// 디스크를 내보내는 NBD 서버(내보낼 디스크가 있을 때만)
    pub nbd: Option<u16>,
}

/// 원본 디스크 하나를 미러할 곳: 받는 QEMU의 NBD 내보내기
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorTarget {
    /// 디스크 순서 번호(노드 `disk{i}`)
    pub disk: usize,
    /// 받는 노드의 주소
    pub host: String,
    /// 받는 QEMU의 NBD 포트([`IncomingPorts::nbd`])
    pub port: u16,
    /// 내보내기 이름(`disk{i}`)
    pub export: String,
    /// 받는 노드 인증서에서 확인할 이름
    pub tls_hostname: String,
    /// 대상 이미지의 형식. qcow2 디스크를 raw로 옮기면 이미지에 저장된 비트맵을 담을 수 없으므로
    /// 미러 전에 그 디스크의 `opl-` 비트맵을 지운다.
    pub format: ImageFormat,
}

/// 미러들의 진행(모든 디스크의 합). 할당된 영역만 세고, 복사 중 쓰기만큼 둘 다 는다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MirrorProgress {
    pub done_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug)]
pub struct MirrorOptions {
    /// 디스크 하나의 최대 속도(바이트/초). `None`이면 제한 없음
    pub speed: Option<u64>,
    /// 모든 미러가 `ready`가 되기까지의 한도
    pub timeout: Duration,
    /// 복사량의 합이 이만큼 늘지 않으면 실패한다(끊긴 링크).
    pub stall: Duration,
    /// 있으면 진행이 바뀔 때마다 보낸다.
    pub progress: Option<watch::Sender<MirrorProgress>>,
}

/// 마이그레이션 따라잡기(계획 55, [`catch_up_disks`](crate::Hypervisor::catch_up_disks)): 대상 노드의 복제본에 그 뒤
/// 바뀐 블록만 몇 차례 보낸다.
#[derive(Debug)]
pub struct CatchUp {
    /// 디스크마다 받는 QEMU의 NBD 내보내기(대상은 복제본을 내보낸다)
    pub targets: Vec<MirrorTarget>,
    /// `targets` 차례로 첫 패스의 기준 비트맵(복제 비트맵). 없으면 그 디스크는 첫 패스에서 전체를 보낸다.
    pub base: Vec<Option<String>>,
    /// 마이그레이션 Task(임시 비트맵 `opl-mig-{task}-{k}`)
    pub task: Uuid,
    /// 패스 뒤 바뀐 바이트(새 비트맵 `count`의 합)가 이 이하면 멈춘다.
    pub limit: u64,
    /// 패스 수의 최댓값
    pub passes: u8,
    /// 디스크 하나의 최대 속도(바이트/초). `None`이면 제한 없음
    pub speed: Option<u64>,
    /// 보낸 양의 합이 이만큼 늘지 않으면 실패한다(끊긴 링크).
    pub stall: Duration,
    /// 따라잡기 전체(패스들)의 기한. 넘기면 작업을 취소하고 플리싱을 뗀 뒤 실패한다.
    pub timeout: Duration,
    /// 있으면 진행이 바뀔 때마다 보낸다(패스들의 합).
    pub progress: Option<watch::Sender<MirrorProgress>>,
}

/// 따라잡기가 끝난 상태: 스위치오버의 마지막 패스가 쓴다([`MigrateOptions::switchover_catch_up`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatchUpState {
    /// 따라잡은 디스크 순서 번호(노드 `disk{i}`)
    pub disks: Vec<usize>,
    /// 마지막 패스가 만든 비트맵. 마지막 패스는 그 뒤 바뀐 블록을 보낸다.
    pub bitmap: String,
    /// 한 패스 수
    pub passes: u8,
}

/// QEMU의 TLS 자격 증명 객체 id. 보내는 쪽과 받는 쪽 모두 이 이름으로 만든다.
pub const MIGRATION_TLS_ID: &str = "opl-mig-tls";
/// 디스크 미러(NBD)의 TLS 자격 증명 객체 id. NBD 노드가 쥔 객체는 마이그레이션 설정이 지우고 다시
/// 만들 수 없으므로 마이그레이션 것과 나눈다.
pub const NBD_TLS_ID: &str = "opl-nbd-tls";

/// 시간을 설정 파일과 같은 모양(`12h`, `10m`, `1h30m`, `90s`)으로 쓴다.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    let mut out = String::new();
    for (n, unit) in [(h, "h"), (m, "m"), (s, "s")] {
        if n > 0 {
            out.push_str(&format!("{n}{unit}"));
        }
    }
    if out.is_empty() {
        format!("{}ms", d.as_millis())
    } else {
        out
    }
}

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

/// 받는 포트가 모두 쓰는 중일 때 오류의 머리(뒤에 `A-B on host`) — API가 Task 실패의 원인을 이 머리로 안다
pub const NO_FREE_MIGRATION_PORT: &str = "no free migration port in ";
/// 받는 주소에서 포트를 열지 못했을 때(그 노드의 주소가 아님 등) 오류의 머리(뒤에 `host`)
pub const CANNOT_LISTEN_FOR_MIGRATION: &str = "cannot listen for migrations on ";

/// [`pick_port`]가 포트를 고르지 못한 까닭
#[derive(Debug)]
pub(crate) enum NoPort {
    /// 범위의 포트가 모두 쓰는 중이다(또는 이미 고른 것).
    Busy,
    /// 쓰는 중이 아닌 까닭으로 열지 못했다(주소가 이 기계의 것이 아님, 권한 등).
    Bind(std::io::Error),
}

/// `ports`에서 `except`가 아니고 `host`에 지금 열 수 있는 첫 포트. 잠깐 열어 보고 닫는다. QEMU의
/// `migrate-incoming`은 한 번만 부를 수 있어 포트가 쓰는 중이면 그 QEMU를 버려야 하므로 미리 고른다.
/// 쓰는 중(`AddrInUse`)이 아닌 까닭으로 열지 못하면 다른 포트도 같으므로 바로 그 오류를 준다.
pub(crate) fn pick_port(
    host: &str,
    ports: RangeInclusive<u16>,
    except: &[u16],
) -> Result<u16, NoPort> {
    for port in ports {
        if except.contains(&port) {
            continue;
        }
        match std::net::TcpListener::bind(socket_addr(host, port)) {
            Ok(_) => return Ok(port),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(e) => return Err(NoPort::Bind(e)),
        }
    }
    Err(NoPort::Busy)
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
        assert!(matches!(
            pick_port("127.0.0.1", port..=port, &[]),
            Err(NoPort::Busy)
        ));
        drop(busy);
        // 같은 시험 프로세스의 다른 시험이 그 순간 자식 프로세스를 띄우면(fork) 자식이 exec할 때까지 닫은 소켓을
        // 들고 있어 포트가 잠깐 쓰이는 것으로 보인다(swtpm 시험의 `ps` 등). 잠시 다시 본다.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while pick_port("127.0.0.1", port..=port, &[]).is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "port {port} stayed busy"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(pick_port("127.0.0.1", port..=port, &[]).unwrap(), port);
        // 이미 고른 포트(아직 열지 않음)는 건너뛴다.
        assert!(matches!(
            pick_port("127.0.0.1", port..=port, &[port]),
            Err(NoPort::Busy)
        ));
    }

    /// 이 기계의 주소가 아니면 쓰는 중과 따로 알린다(TEST-NET-1은 어느 기계의 주소도 아니다).
    #[test]
    fn a_foreign_address_is_not_a_busy_port() {
        assert!(matches!(
            pick_port("192.0.2.1", 60000..=60001, &[]),
            Err(NoPort::Bind(_))
        ));
    }

    #[test]
    fn formats_durations_like_the_config() {
        assert_eq!(format_duration(Duration::from_secs(600)), "10m");
        assert_eq!(format_duration(Duration::from_secs(12 * 3600)), "12h");
        assert_eq!(format_duration(Duration::from_secs(5400)), "1h30m");
        assert_eq!(format_duration(Duration::from_secs(90)), "1m30s");
        assert_eq!(format_duration(Duration::from_millis(200)), "200ms");
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
