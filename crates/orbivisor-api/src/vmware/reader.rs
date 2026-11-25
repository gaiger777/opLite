//! 원본 디스크 읽기(설계 §7.4). 기본은 [`FolderReader`] — 기반 디스크의 flat extent를 데이터스토어 `/folder`에서
//! HTTPS `Range`로 읽는다. vCenter면 범위마다 호스트 티켓을 받아 VM이 있는 ESXi 호스트에 바로 붙고(호스트 인증서는
//! vCenter가 아는 `HostSystem.config.certificate`로 고정), 티켓이 거절되거나 호스트에 닿지 않으면 그 읽개가 끝날
//! 때까지 vCenter `/folder`로 내려가 경고를 남긴다. 선택 길 [`NbdkitReader`]는 운영자가 깐 nbdkit `vddk` 플러그인을
//! 유닉스 소켓으로 띄워 우리 NBD 클라이언트로 읽는다(vSAN·vVols).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::header::{CONTENT_RANGE, COOKIE, HeaderValue, RANGE};
use hyper::{Request, StatusCode};
use orbivisor_store::nbd::NbdClient;
use orbivisor_types::vmware::Secret;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use super::VsphereError;
use super::client::{Session, invalid, split_datastore_path};
use super::http::{HttpClient, percent_encode, split_host_port};
use crate::vm_imports::vmx::{MAX_TEXT_BYTES, check_flat, parse_descriptor};

/// 호스트 티켓 쿠키 이름
pub const TICKET_COOKIE: &str = "vmware_cgi_ticket";
/// 범위 읽기를 다시 해 보는 횟수(끊김·5xx)
const READ_ATTEMPTS: u32 = 3;
/// nbdkit이 소켓을 열 때까지 기다리는 시간
const NBDKIT_START: Duration = Duration::from_secs(30);

/// 원본 디스크 하나를 읽는 길
#[async_trait]
pub trait DiskReader: Send {
    /// `offset`부터 `len` 바이트(디스크 끝을 넘지 않는다)
    async fn read(&mut self, offset: u64, len: u32) -> Result<Bytes, VsphereError>;
    /// 디스크 크기(바이트)
    fn size(&self) -> u64;
    /// 읽는 동안 생긴 경고(예: 호스트 티켓이 거절되어 vCenter로 내려감)
    fn warnings(&self) -> Vec<String> {
        Vec::new()
    }
}

/// vCenter가 알려 준 VM의 ESXi 호스트(관리 주소 `호스트[:포트]`와 고정할 인증서 지문)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRoute {
    pub name: String,
    pub fingerprint: String,
}

/// `/folder/{path}?dcPath=…&dsName=…`
pub fn folder_url(datacenter: &str, datastore: &str, path: &str) -> String {
    format!(
        "/folder/{}?dcPath={}&dsName={}",
        percent_encode(path, true),
        percent_encode(datacenter, false),
        percent_encode(datastore, false)
    )
}

/// 디스크 파일 하나를 `/folder`로 읽는다.
pub struct FolderReader {
    session: Arc<Session>,
    datacenter: String,
    datastore: String,
    /// 데이터스토어 안 flat extent 경로(`web01/web01-flat.vmdk`)
    path: String,
    size: u64,
    /// 호스트로 바로 가는 길(vCenter). None이면 세션 쿠키로 원본에(ESXi, 또는 내려간 뒤)
    host: Option<(Arc<HttpClient>, String)>,
    warnings: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Debug for FolderReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FolderReader")
            .field("datastore", &self.datastore)
            .field("path", &self.path)
            .field("size", &self.size)
            .field("direct", &self.host.is_some())
            .finish_non_exhaustive()
    }
}

enum Fetch {
    Ok(Bytes),
    /// 호스트가 티켓을 받지 않았다(401·403)
    Refused(u16),
}

impl FolderReader {
    /// 기반 디스크 디스크립터(`[ds] web01/web01.vmdk`)를 읽어 flat extent를 연다. `host`가 있으면(vCenter) 범위
    /// 읽기를 그 호스트로 바로 보낸다.
    pub async fn open(
        session: Arc<Session>,
        datacenter: &str,
        descriptor: &str,
        host: Option<HostRoute>,
    ) -> Result<Self, VsphereError> {
        let (datastore, file) = split_datastore_path(descriptor)
            .ok_or_else(|| invalid(format!("invalid datastore path {descriptor:?}")))?;
        let host = host.map(|h| {
            let (name, port) = split_host_port(&h.name, 443);
            let client = Arc::new(HttpClient::new(&name, port, &h.fingerprint));
            (client, h.name)
        });
        let mut reader = FolderReader {
            session,
            datacenter: datacenter.to_string(),
            datastore: datastore.to_string(),
            path: file.to_string(),
            size: MAX_TEXT_BYTES as u64,
            host,
            warnings: Arc::default(),
        };
        // 디스크립터는 작다 — 처음 64 KiB를 범위로 읽는다(서버가 Range를 무시하면 통째로 받는다).
        let text = reader.get(0, MAX_TEXT_BYTES as u32, true).await?;
        let text = String::from_utf8_lossy(&text);
        let d = parse_descriptor(&text).map_err(|e| invalid(format!("{descriptor}: {}", e.0)))?;
        let (flat, size) = check_flat(&d).map_err(|e| invalid(format!("{descriptor}: {}", e.0)))?;
        reader.path = match file.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/{flat}"),
            None => flat,
        };
        reader.size = size;
        Ok(reader)
    }

    /// 읽는 flat 파일(`[ds] web01/web01-flat.vmdk`)
    pub fn flat_path(&self) -> String {
        format!("[{}] {}", self.datastore, self.path)
    }

    /// 호스트로 바로 읽고 있나(내려가지 않았나)
    pub fn direct(&self) -> bool {
        self.host.is_some()
    }

    fn warn(&self, w: String) {
        let mut ws = self.warnings.lock().unwrap_or_else(|e| e.into_inner());
        if !ws.contains(&w) {
            ws.push(w);
        }
    }

    async fn get(&mut self, offset: u64, len: u32, whole_ok: bool) -> Result<Bytes, VsphereError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let result = match self.host.clone() {
                Some((client, name)) => {
                    match self.get_direct(&client, &name, offset, len, whole_ok).await {
                        Ok(Fetch::Ok(b)) => Ok(b),
                        Ok(Fetch::Refused(status)) => {
                            self.fall_back(format!(
                                "the ESXi host {name} refused the service ticket (HTTP {status})"
                            ));
                            continue;
                        }
                        Err(e @ VsphereError::Connect(_)) if attempt >= READ_ATTEMPTS => {
                            self.fall_back(format!("cannot reach the ESXi host {name}: {e}"));
                            attempt = 0;
                            continue;
                        }
                        Err(
                            e @ (VsphereError::Fingerprint { .. } | VsphereError::Untrusted { .. }),
                        ) => {
                            self.fall_back(format!(
                                "the ESXi host {name} certificate does not match vCenter's: {e}"
                            ));
                            continue;
                        }
                        Err(e) => Err(e),
                    }
                }
                None => self.get_via_session(offset, len, whole_ok).await,
            };
            match result {
                Err(e) if e.is_transient() && attempt < READ_ATTEMPTS => {
                    tokio::time::sleep(Duration::from_millis(100 * u64::from(attempt))).await;
                }
                other => return other,
            }
        }
    }

    fn fall_back(&mut self, why: String) {
        self.host = None;
        self.warn(format!(
            "{why}; reading {} through vCenter instead",
            self.flat_path()
        ));
    }

    async fn get_direct(
        &self,
        client: &HttpClient,
        name: &str,
        offset: u64,
        len: u32,
        whole_ok: bool,
    ) -> Result<Fetch, VsphereError> {
        let path = folder_url(&self.datacenter, &self.datastore, &self.path);
        // 티켓은 한 번만 쓸 수 있다고 보고 범위마다 새로 받는다(설계 §4 가정 2).
        let ticket = self
            .session
            .acquire_ticket(&format!("https://{name}{path}"))
            .await?;
        let cookie = format!("{TICKET_COOKIE}={}", ticket.id);
        let (status, body) = range_get(client, &path, &cookie, offset, len).await?;
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Ok(Fetch::Refused(status.as_u16()));
        }
        self.check(status, body, offset, len, whole_ok)
            .map(Fetch::Ok)
    }

    async fn get_via_session(
        &self,
        offset: u64,
        len: u32,
        whole_ok: bool,
    ) -> Result<Bytes, VsphereError> {
        let path = folder_url(&self.datacenter, &self.datastore, &self.path);
        let cookie = self.session.cookie().unwrap_or_default();
        let (status, body) = range_get(self.session.http(), &path, &cookie, offset, len).await?;
        if status == StatusCode::UNAUTHORIZED {
            // 세션이 끝났다 — 한 번 다시 로그인한다.
            self.session.relogin().await?;
            let cookie = self.session.cookie().unwrap_or_default();
            let (status, body) =
                range_get(self.session.http(), &path, &cookie, offset, len).await?;
            return self.check(status, body, offset, len, whole_ok);
        }
        self.check(status, body, offset, len, whole_ok)
    }

    fn check(
        &self,
        status: StatusCode,
        body: (Bytes, Option<String>),
        offset: u64,
        len: u32,
        whole_ok: bool,
    ) -> Result<Bytes, VsphereError> {
        let (data, range) = body;
        match status {
            StatusCode::PARTIAL_CONTENT => {
                let start = range
                    .as_deref()
                    .and_then(|r| r.strip_prefix("bytes "))
                    .and_then(|r| r.split('-').next())
                    .and_then(|s| s.parse::<u64>().ok());
                if start != Some(offset) {
                    return Err(invalid(format!(
                        "{} answered range {range:?} for offset {offset}",
                        self.flat_path()
                    )));
                }
                Ok(data)
            }
            // Range를 무시하고 통째로 준 작은 파일(디스크립터)
            StatusCode::OK if whole_ok && offset == 0 => {
                Ok(data.slice(..data.len().min(len as usize)))
            }
            StatusCode::OK => Err(invalid(format!(
                "the server ignored the byte range for {}",
                self.flat_path()
            ))),
            StatusCode::RANGE_NOT_SATISFIABLE => Ok(Bytes::new()),
            StatusCode::NOT_FOUND => Err(VsphereError::NotFound {
                kind: "FileNotFound".into(),
                message: format!("{} was not found", self.flat_path()),
            }),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(VsphereError::Auth {
                kind: "NotAuthenticated".into(),
                message: format!("HTTP {} reading {}", status.as_u16(), self.flat_path()),
            }),
            s => Err(VsphereError::Http(s.as_u16())),
        }
    }
}

/// `Range: bytes=a-b` GET 하나. (상태, (본문, Content-Range))
async fn range_get(
    client: &HttpClient,
    path: &str,
    cookie: &str,
    offset: u64,
    len: u32,
) -> Result<(StatusCode, (Bytes, Option<String>)), VsphereError> {
    let end = offset + u64::from(len.max(1)) - 1;
    let mut request = Request::get(path).header(RANGE, format!("bytes={offset}-{end}"));
    if !cookie.is_empty() {
        request = request.header(
            COOKIE,
            HeaderValue::from_str(cookie).map_err(|_| invalid("invalid cookie"))?,
        );
    }
    let request = request
        .body(Full::new(Bytes::new()))
        .map_err(|e| invalid(e.to_string()))?;
    let response = client.send(request, len as usize).await?;
    let range = response
        .headers
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    Ok((response.status, (response.body, range)))
}

#[async_trait]
impl DiskReader for FolderReader {
    async fn read(&mut self, offset: u64, len: u32) -> Result<Bytes, VsphereError> {
        if offset >= self.size || len == 0 {
            return Ok(Bytes::new());
        }
        let len = (self.size - offset).min(u64::from(len)) as u32;
        let data = self.get(offset, len, false).await?;
        if data.len() != len as usize {
            return Err(VsphereError::Connect(format!(
                "short read of {}: {} of {len} bytes at {offset}",
                self.flat_path(),
                data.len()
            )));
        }
        Ok(data)
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn warnings(&self) -> Vec<String> {
        self.warnings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

// ---- nbdkit(VDDK) ----

/// nbdkit `vddk` 플러그인으로 스냅샷 시점의 디스크 하나를 연다(설계 §4·§7.4).
#[derive(Debug, Clone)]
pub struct VddkParams {
    pub libdir: PathBuf,
    /// vCenter·ESXi 호스트
    pub server: String,
    pub port: u16,
    pub user: String,
    pub password: Secret,
    /// 서버 인증서의 SHA-1(대문자 콜론 구분)
    pub thumbprint: String,
    /// VM moref(`vm-42`)
    pub vm: String,
    /// 스냅샷 moref(없으면 지금 디스크)
    pub snapshot: Option<String>,
    /// 디스크 파일(`[ds] web/web.vmdk`)
    pub file: String,
    /// `nbdssl:nbd`처럼. 없으면 nbdkit 기본
    pub transports: Option<String>,
}

/// vddk 플러그인 인자. 비밀번호는 argv에 넣지 않는다(`password=-` — 표준 입력으로 준다).
pub fn vddk_args(p: &VddkParams) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "vddk".into(),
        format!("libdir={}", p.libdir.display()).into(),
        format!("server={}", p.server).into(),
        format!("user={}", p.user).into(),
        "password=-".into(),
        format!("thumbprint={}", p.thumbprint).into(),
        format!("vm=moref={}", p.vm).into(),
        format!("file={}", p.file).into(),
    ];
    if p.port != 443 {
        args.push(format!("port={}", p.port).into());
    }
    if let Some(s) = &p.snapshot {
        args.push(format!("snapshot={s}").into());
    }
    if let Some(t) = &p.transports {
        args.push(format!("transports={t}").into());
    }
    args
}

/// nbdkit을 유닉스 소켓으로 띄우고 우리 NBD 클라이언트로 읽는다. 버리면 nbdkit을 끝내고 소켓 디렉터리를 지운다.
pub struct NbdkitReader {
    client: NbdClient<UnixStream>,
    size: u64,
    child: tokio::process::Child,
    dir: PathBuf,
}

impl std::fmt::Debug for NbdkitReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NbdkitReader")
            .field("size", &self.size)
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

fn nbd_error(e: impl std::fmt::Display) -> VsphereError {
    VsphereError::Connect(format!("nbdkit: {e}"))
}

impl NbdkitReader {
    /// VDDK로 디스크를 연다. 소켓은 `run_dir` 아래 새 0700 디렉터리에 둔다.
    pub async fn open_vddk(
        nbdkit: &Path,
        run_dir: &Path,
        params: &VddkParams,
    ) -> Result<Self, VsphereError> {
        Self::spawn(nbdkit, run_dir, vddk_args(params), Some(&params.password)).await
    }

    /// nbdkit을 `plugin_args`로 띄운다(읽기 전용, 앞에서). `secret`은 표준 입력 첫 줄로 준다.
    pub async fn spawn(
        nbdkit: &Path,
        run_dir: &Path,
        plugin_args: Vec<OsString>,
        secret: Option<&Secret>,
    ) -> Result<Self, VsphereError> {
        let mut name = [0u8; 8];
        getrandom::fill(&mut name).map_err(nbd_error)?;
        let dir = run_dir.join(format!("vddk-{}", hex::encode(name)));
        std::fs::create_dir_all(run_dir).map_err(nbd_error)?;
        std::fs::create_dir(&dir).map_err(nbd_error)?;
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .map_err(nbd_error)?;
        }
        let socket = dir.join("nbd.sock");
        let result = Self::start(nbdkit, &dir, &socket, plugin_args, secret).await;
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        result
    }

    async fn start(
        nbdkit: &Path,
        dir: &Path,
        socket: &Path,
        plugin_args: Vec<OsString>,
        secret: Option<&Secret>,
    ) -> Result<Self, VsphereError> {
        let mut cmd = tokio::process::Command::new(nbdkit);
        cmd.arg("--foreground")
            .arg("--readonly")
            .arg("--unix")
            .arg(socket);
        #[cfg(target_os = "linux")]
        cmd.arg("--exit-with-parent");
        cmd.args(plugin_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| nbd_error(format!("cannot run {}: {e}", nbdkit.display())))?;
        if let Some(mut stdin) = child.stdin.take() {
            if let Some(s) = secret {
                let _ = stdin
                    .write_all(format!("{}\n", s.expose()).as_bytes())
                    .await;
            }
            drop(stdin);
        }
        // stderr를 비워 nbdkit이 막히지 않게 하고 끝 4 KiB를 오류 메시지로 둔다.
        let tail = Arc::new(Mutex::new(Vec::<u8>::new()));
        if let Some(mut stderr) = child.stderr.take() {
            let tail = tail.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = stderr.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let mut t = tail.lock().unwrap_or_else(|e| e.into_inner());
                    t.extend_from_slice(&buf[..n]);
                    let over = t.len().saturating_sub(4096);
                    t.drain(..over);
                }
            });
        }
        let started = tokio::time::Instant::now();
        let client = loop {
            if let Some(status) = child.try_wait().map_err(nbd_error)? {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let msg = String::from_utf8_lossy(&tail.lock().unwrap_or_else(|e| e.into_inner()))
                    .trim()
                    .to_string();
                return Err(nbd_error(format!("exited with {status}: {msg}")));
            }
            if socket.exists()
                && let Ok(c) = NbdClient::connect(socket, "", &[]).await
            {
                break c;
            }
            if started.elapsed() > NBDKIT_START {
                return Err(nbd_error("did not open its socket in time"));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        Ok(Self {
            size: client.size(),
            client,
            child,
            dir: dir.to_path_buf(),
        })
    }
}

impl Drop for NbdkitReader {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[async_trait]
impl DiskReader for NbdkitReader {
    async fn read(&mut self, offset: u64, len: u32) -> Result<Bytes, VsphereError> {
        if offset >= self.size || len == 0 {
            return Ok(Bytes::new());
        }
        let len = (self.size - offset).min(u64::from(len)) as u32;
        let data = self.client.read(offset, len).await.map_err(nbd_error)?;
        Ok(Bytes::from(data))
    }

    fn size(&self) -> u64 {
        self.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_urls_encode_paths_and_names() {
        assert_eq!(
            folder_url("DC 1", "ssd & co", "my vm/my vm-flat.vmdk"),
            "/folder/my%20vm/my%20vm-flat.vmdk?dcPath=DC%201&dsName=ssd%20%26%20co"
        );
        assert_eq!(
            folder_url("Europe/DC2", "ds1", "a/b-flat.vmdk"),
            "/folder/a/b-flat.vmdk?dcPath=Europe%2FDC2&dsName=ds1"
        );
    }

    #[test]
    fn vddk_arguments_never_carry_the_password() {
        let p = VddkParams {
            libdir: "/opt/vmware-vix-disklib-distrib".into(),
            server: "vcenter.lab".into(),
            port: 8443,
            user: "migr@vsphere.local".into(),
            password: Secret::new("Sup3r-secret"),
            thumbprint: "AA:BB".into(),
            vm: "vm-42".into(),
            snapshot: Some("snapshot-7".into()),
            file: "[ds 1] web/web.vmdk".into(),
            transports: Some("nbdssl:nbd".into()),
        };
        let args: Vec<String> = vddk_args(&p)
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        assert_eq!(
            args,
            vec![
                "vddk",
                "libdir=/opt/vmware-vix-disklib-distrib",
                "server=vcenter.lab",
                "user=migr@vsphere.local",
                "password=-",
                "thumbprint=AA:BB",
                "vm=moref=vm-42",
                "file=[ds 1] web/web.vmdk",
                "port=8443",
                "snapshot=snapshot-7",
                "transports=nbdssl:nbd",
            ]
        );
        assert!(args.iter().all(|a| !a.contains("Sup3r")));
    }

    #[tokio::test]
    async fn nbdkit_that_cannot_start_reports_why_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let err = NbdkitReader::spawn(
            Path::new("/nonexistent/nbdkit"),
            dir.path(),
            vec!["file".into()],
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("cannot run"), "{err}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        // 곧 끝나는 프로그램(가짜 nbdkit) — stderr를 오류에 보인다.
        let bin = tempfile::tempdir().unwrap();
        let fake = bin.path().join("nbdkit");
        std::fs::write(&fake, "#!/bin/sh\necho plugin failed >&2\nexit 3\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let err = NbdkitReader::spawn(
            &fake,
            dir.path(),
            vec!["file".into()],
            Some(&Secret::new("pw")),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("plugin failed"), "{err}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
