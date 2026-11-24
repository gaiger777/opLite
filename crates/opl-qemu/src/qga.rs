//! 게스트 에이전트(QGA) 채널의 클라이언트(계획 51 `guest-ping`, 계획 59 [`Session`]).
//!
//! QEMU의 채널 소켓은 게스트가 포트를 열지 않았어도 연결을 받으므로, 게스트 에이전트가 없거나 멈추면 답이
//! 오지 않는다. 그래서 [`ping`]에서 답이 없는 것은 오류가 아니라 `false`다. 채널은 한 번에 한 연결뿐이므로
//! 호출자가 VM마다 세션을 하나씩만 연다. 세션 명령이 시간을 넘기거나 이상한 답을 받으면 그 세션을 버린다(다음
//! 세션이 `guest-sync-delimited`로 다시 맞춘다).

use std::io;
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use base64::Engine;
use opl_types::guest::{GuestAddr, GuestFs, GuestInfo, GuestNic, GuestOs};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// `guest-sync-delimited`의 답 앞에 에이전트가 보내는 구분 바이트. 보내는 쪽이 먼저 보내면 에이전트가 앞서
/// 받다 만 명령을 버린다.
const DELIMITER: u8 = 0xFF;

/// 답 한 줄의 상한. 넘으면 세션을 버린다(게스트가 보낸 값은 믿지 않는다).
pub const MAX_REPLY_BYTES: usize = 1 << 20;
/// 연결 뒤 동기화(`guest-sync-delimited`)를 기다리는 시간
pub const SYNC_TIMEOUT: Duration = Duration::from_secs(2);
/// 명령 하나를 기다리는 시간
pub const AGENT_TIMEOUT: Duration = Duration::from_secs(5);
/// `guest-fsfreeze-freeze`를 기다리는 시간(게스트가 쓰기를 내려야 한다)
pub const FREEZE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum QgaError {
    /// 채널 소켓에 연결하지 못했다(채널 없음, QEMU가 받지 않음).
    #[error("cannot connect to the guest agent channel: {0}")]
    Connect(io::Error),
    #[error("the guest agent did not answer {0} in time")]
    Timeout(String),
    #[error("the guest agent channel was closed")]
    Closed,
    #[error("the guest agent sent a reply over {MAX_REPLY_BYTES} bytes")]
    TooLong,
    #[error("the guest agent sent an unreadable reply to {0}")]
    BadReply(String),
    /// 에이전트가 오류로 답했다(모르는 명령, 실패). 세션은 그대로 쓸 수 있다.
    #[error("guest agent error ({class}): {desc}")]
    Command { class: String, desc: String },
    #[error("guest agent channel i/o error: {0}")]
    Io(#[from] io::Error),
}

impl QgaError {
    /// 에이전트가 답한 오류(세션은 살아 있다)
    pub fn is_command(&self) -> bool {
        matches!(self, QgaError::Command { .. })
    }
}

/// 동기화를 마친 에이전트 연결 하나
pub struct Session {
    read: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
}

impl Session {
    /// 채널에 연결하고 `guest-sync-delimited`로 앞선 연결이 남긴 답을 걸러 낸다. 연결마다 다른 id를 쓰므로 지난 답과
    /// 섞이지 않는다. `timeout` 안에 맞추지 못하면 `Timeout`이다.
    pub async fn connect(path: &Path, timeout: Duration) -> Result<Self, QgaError> {
        let stream = UnixStream::connect(path).await.map_err(QgaError::Connect)?;
        let (read, write) = stream.into_split();
        let mut session = Self {
            read: BufReader::new(read),
            write,
        };
        tokio::time::timeout(timeout, session.sync())
            .await
            .map_err(|_| QgaError::Timeout("guest-sync-delimited".into()))??;
        Ok(session)
    }

    async fn sync(&mut self) -> Result<(), QgaError> {
        let id = u64::from(uuid::Uuid::now_v7().as_u128() as u32);
        let sync = json!({ "execute": "guest-sync-delimited", "arguments": { "id": id } });
        self.write.write_all(&[DELIMITER]).await?;
        self.write.write_all(format!("{sync}\n").as_bytes()).await?;
        loop {
            // 구분 바이트까지의 지난 답은 버린다.
            read_limited(&mut self.read, DELIMITER)
                .await?
                .ok_or(QgaError::Closed)?;
            let line = read_limited(&mut self.read, b'\n')
                .await?
                .ok_or(QgaError::Closed)?;
            let answer: Value = serde_json::from_slice(&line).unwrap_or(Value::Null);
            if answer["return"].as_u64() == Some(id) {
                return Ok(());
            }
        }
    }

    /// 명령을 보내고 답 한 줄을 받아 `return` 값을 돌려준다. 오류 답은 `Command`다. 다른 오류가 나면 세션을 버린다.
    pub async fn execute(
        &mut self,
        cmd: &str,
        args: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, QgaError> {
        self.send(cmd, args).await?;
        let line = tokio::time::timeout(timeout, read_limited(&mut self.read, b'\n'))
            .await
            .map_err(|_| QgaError::Timeout(cmd.to_string()))??
            .ok_or(QgaError::Closed)?;
        let mut answer: Value =
            serde_json::from_slice(&line).map_err(|_| QgaError::BadReply(cmd.to_string()))?;
        if let Some(err) = answer.get("error") {
            return Err(QgaError::Command {
                class: err["class"].as_str().unwrap_or_default().to_string(),
                desc: err["desc"].as_str().unwrap_or_default().to_string(),
            });
        }
        match answer.get_mut("return") {
            Some(ret) => Ok(ret.take()),
            None => Err(QgaError::BadReply(cmd.to_string())),
        }
    }

    /// 답을 기다리지 않고 명령만 보낸다.
    pub async fn send(&mut self, cmd: &str, args: Option<Value>) -> Result<(), QgaError> {
        let mut msg = json!({ "execute": cmd });
        if let Some(args) = args {
            msg["arguments"] = args;
        }
        self.write.write_all(format!("{msg}\n").as_bytes()).await?;
        Ok(())
    }
}

/// `delim`까지 읽어 `delim`을 뺀 바이트를 돌려준다. 연결이 끝나면 None, [`MAX_REPLY_BYTES`]를 넘으면 `TooLong`이다.
async fn read_limited(
    read: &mut BufReader<OwnedReadHalf>,
    delim: u8,
) -> Result<Option<Vec<u8>>, QgaError> {
    let mut out = Vec::new();
    loop {
        let buf = read.fill_buf().await?;
        if buf.is_empty() {
            return Ok(None);
        }
        let (used, done) = match buf.iter().position(|b| *b == delim) {
            Some(i) => {
                out.extend_from_slice(&buf[..i]);
                (i + 1, true)
            }
            None => {
                out.extend_from_slice(buf);
                (buf.len(), false)
            }
        };
        read.consume(used);
        if out.len() > MAX_REPLY_BYTES {
            return Err(QgaError::TooLong);
        }
        if done {
            return Ok(Some(out));
        }
    }
}

/// 에이전트에 `guest-sync-delimited`로 앞서 남은 답을 걸러 낸 뒤 `guest-ping`을 보낸다. `timeout` 안에 답이
/// 오면 true(오류 답이어도 에이전트가 살아 답한 것이다), 오지 않거나 연결이 끊기면 false다. 소켓에 연결하지
/// 못하면(채널이 없음, 연결 거부) 오류다.
pub async fn ping(path: &Path, timeout: Duration) -> io::Result<bool> {
    let exchange = async {
        let mut session = Session::connect(path, timeout).await?;
        session.execute("guest-ping", None, timeout).await
    };
    match tokio::time::timeout(timeout, exchange).await {
        Ok(Ok(_)) => Ok(true),
        Ok(Err(QgaError::Connect(e))) => Err(e),
        Ok(Err(e)) => Ok(e.is_command()),
        Err(_) => Ok(false),
    }
}

/// 게스트 정보를 모은다: `guest-info`, `guest-get-osinfo`, `guest-get-host-name`, `guest-network-get-interfaces`,
/// `guest-get-fsinfo`. 오류로 답한 명령(옛 에이전트, freeze 중)은 그 칸만 비운다. 다른 오류면 세션을 더 쓰지 않고
/// 모은 데까지 돌려준다. 결과는 상한으로 자른다.
pub async fn guest_info(session: &mut Session, collected_at: u64) -> GuestInfo {
    let mut info = GuestInfo {
        collected_at,
        ..GuestInfo::default()
    };
    for cmd in [
        "guest-info",
        "guest-get-osinfo",
        "guest-get-host-name",
        "guest-network-get-interfaces",
        "guest-get-fsinfo",
    ] {
        let value = match session.execute(cmd, None, AGENT_TIMEOUT).await {
            Ok(v) => v,
            Err(e) if e.is_command() => continue,
            Err(_) => break,
        };
        match cmd {
            "guest-info" => info.agent_version = str_of(&value["version"]),
            "guest-get-osinfo" => info.os = Some(parse_os(&value)),
            "guest-get-host-name" => info.hostname = str_of(&value["host-name"]),
            "guest-network-get-interfaces" => info.interfaces = parse_interfaces(&value),
            _ => info.filesystems = parse_filesystems(&value),
        }
    }
    info.truncate();
    info
}

fn str_of(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// `guest-get-osinfo`의 답
pub fn parse_os(v: &Value) -> GuestOs {
    GuestOs {
        id: str_of(&v["id"]),
        name: str_of(&v["pretty-name"]).or_else(|| str_of(&v["name"])),
        version: str_of(&v["version"]),
        kernel: str_of(&v["kernel-release"]),
    }
}

/// `guest-network-get-interfaces`의 답. 읽을 수 없는 주소는 버린다.
pub fn parse_interfaces(v: &Value) -> Vec<GuestNic> {
    let Some(nics) = v.as_array() else {
        return Vec::new();
    };
    nics.iter()
        .filter_map(|nic| {
            let name = str_of(&nic["name"])?;
            let addresses = nic["ip-addresses"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|addr| {
                            let ip: IpAddr = addr["ip-address"].as_str()?.parse().ok()?;
                            let prefix = u8::try_from(addr["prefix"].as_u64()?).ok()?;
                            let max = if ip.is_ipv4() { 32 } else { 128 };
                            (prefix <= max).then_some(GuestAddr { ip, prefix })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(GuestNic {
                name,
                mac: str_of(&nic["hardware-address"]),
                addresses,
            })
        })
        .collect()
}

/// `guest-get-fsinfo`의 답
pub fn parse_filesystems(v: &Value) -> Vec<GuestFs> {
    let Some(list) = v.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|fs| {
            Some(GuestFs {
                mountpoint: str_of(&fs["mountpoint"])?,
                fstype: str_of(&fs["type"]).unwrap_or_default(),
                used_bytes: fs["used-bytes"].as_u64(),
                total_bytes: fs["total-bytes"].as_u64(),
            })
        })
        .collect()
}

/// 게스트를 끈다(`guest-shutdown`, `powerdown`). 성공하면 에이전트가 답하지 않으므로 `wait`(보통 [`AGENT_TIMEOUT`])
/// 안에 답이 없거나 연결이 끊기면 보낸 것으로 보고 Ok다. 오류 답이면 `Command`다.
pub async fn shutdown(session: &mut Session, wait: Duration) -> Result<(), QgaError> {
    let args = json!({ "mode": "powerdown" });
    match session.execute("guest-shutdown", Some(args), wait).await {
        Ok(_) | Err(QgaError::Timeout(_) | QgaError::Closed) => Ok(()),
        Err(e) => Err(e),
    }
}

/// 게스트 파일 시스템을 얼리고 얼린 수를 돌려준다(`guest-fsfreeze-freeze`, [`FREEZE_TIMEOUT`]).
pub async fn freeze(session: &mut Session) -> Result<u64, QgaError> {
    let n = session
        .execute("guest-fsfreeze-freeze", None, FREEZE_TIMEOUT)
        .await?;
    Ok(n.as_u64().unwrap_or_default())
}

/// 얼린 파일 시스템을 녹인다(`guest-fsfreeze-thaw`). 얼리지 않았어도 성공한다.
pub async fn thaw(session: &mut Session) -> Result<(), QgaError> {
    session
        .execute("guest-fsfreeze-thaw", None, AGENT_TIMEOUT)
        .await
        .map(drop)
}

/// 게스트 사용자의 비밀번호를 바꾼다(`guest-set-user-password`, 평문을 base64로). 비밀번호는 오류에 넣지 않는다.
pub async fn set_user_password(
    session: &mut Session,
    user: &str,
    password: &str,
) -> Result<(), QgaError> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(password);
    let args = json!({ "username": user, "password": encoded, "crypted": false });
    session
        .execute("guest-set-user-password", Some(args), AGENT_TIMEOUT)
        .await
        .map(drop)
}

/// 파일 쓰기 명령 하나를 기다리는 시간(1 MiB `guest-file-write` — TCG 게스트에서도 넉넉히)
pub const FILE_TIMEOUT: Duration = Duration::from_secs(30);

/// 에이전트가 아는 명령과 허락 여부(`guest-info.supported_commands[].{name,enabled}`). RHEL·Rocky 에이전트는 막은 명령을
/// `enabled: false`로 알린다.
pub async fn supported_commands(session: &mut Session) -> Result<Vec<(String, bool)>, QgaError> {
    let info = session.execute("guest-info", None, AGENT_TIMEOUT).await?;
    let Some(list) = info["supported_commands"].as_array() else {
        return Err(QgaError::BadReply("guest-info".into()));
    };
    Ok(list
        .iter()
        .filter_map(|c| {
            let name = c["name"].as_str()?;
            Some((name.to_string(), c["enabled"].as_bool().unwrap_or(false)))
        })
        .take(1024)
        .collect())
}

/// 게스트 파일을 연다(`guest-file-open`, `mode`는 fopen 꼴 — `r`·`wb`) → 핸들. 핸들은 에이전트 전역이라 연결이 바뀌어도
/// 남는다(닫을 때까지).
pub async fn file_open(session: &mut Session, path: &str, mode: &str) -> Result<i64, QgaError> {
    let args = json!({ "path": path, "mode": mode });
    let handle = session
        .execute("guest-file-open", Some(args), AGENT_TIMEOUT)
        .await?;
    handle
        .as_i64()
        .ok_or_else(|| QgaError::BadReply("guest-file-open".into()))
}

/// 핸들에 쓴다(`guest-file-write`, 날 바이트의 표준 base64) → 쓴 바이트 수
pub async fn file_write(session: &mut Session, handle: i64, base64: &str) -> Result<u64, QgaError> {
    let args = json!({ "handle": handle, "buf-b64": base64 });
    let ret = session
        .execute("guest-file-write", Some(args), FILE_TIMEOUT)
        .await?;
    ret["count"]
        .as_u64()
        .ok_or_else(|| QgaError::BadReply("guest-file-write".into()))
}

/// 핸들의 버퍼를 비운다(`guest-file-flush`)
pub async fn file_flush(session: &mut Session, handle: i64) -> Result<(), QgaError> {
    let args = json!({ "handle": handle });
    session
        .execute("guest-file-flush", Some(args), FILE_TIMEOUT)
        .await
        .map(drop)
}

/// 핸들을 닫는다(`guest-file-close`)
pub async fn file_close(session: &mut Session, handle: i64) -> Result<(), QgaError> {
    let args = json!({ "handle": handle });
    session
        .execute("guest-file-close", Some(args), FILE_TIMEOUT)
        .await
        .map(drop)
}

/// 프로그램을 셸 없이 띄운다(`guest-exec`, 출력을 모음) → pid. 끝은 [`exec_status`]로 본다.
pub async fn exec(session: &mut Session, path: &str, args: &[String]) -> Result<i64, QgaError> {
    let args = json!({ "path": path, "arg": args, "capture-output": true });
    let ret = session
        .execute("guest-exec", Some(args), AGENT_TIMEOUT)
        .await?;
    ret["pid"]
        .as_i64()
        .ok_or_else(|| QgaError::BadReply("guest-exec".into()))
}

/// `guest-exec-status`의 답
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecStatus {
    pub exited: bool,
    /// 종료 코드(끝났고 시그널이 아니면)
    pub code: Option<i64>,
    /// 끝낸 시그널(Linux)
    pub signal: Option<i64>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// 띄운 프로그램의 상태(`guest-exec-status`). 끝난 뒤 한 번 답하면 에이전트가 그 pid를 잊는다.
pub async fn exec_status(session: &mut Session, pid: i64) -> Result<ExecStatus, QgaError> {
    let args = json!({ "pid": pid });
    let ret = session
        .execute("guest-exec-status", Some(args), AGENT_TIMEOUT)
        .await?;
    let data = |key: &str| {
        ret[key]
            .as_str()
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
            .unwrap_or_default()
    };
    Ok(ExecStatus {
        exited: ret["exited"].as_bool().unwrap_or(false),
        code: ret["exitcode"].as_i64(),
        signal: ret["signal"].as_i64(),
        stdout: data("out-data"),
        stderr: data("err-data"),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use tokio::net::UnixListener;

    use super::*;

    /// 가짜 에이전트가 하는 일
    #[derive(Clone, Copy)]
    enum Agent {
        /// sync와 ping에 모두 답한다(sync 앞에 지난 sync 답 하나를 섞는다).
        Answers,
        /// sync에만 답한다.
        SyncOnly,
        /// 아무것도 답하지 않는다(게스트 에이전트 없음).
        Silent,
    }

    /// 기본 TMPDIR(소켓 경로 길이 안)에 가짜 에이전트 소켓을 띄운다.
    fn serve(agent: Agent) -> (tempfile::TempDir, PathBuf) {
        serve_with(move |cmd| match (agent, cmd["execute"].as_str().unwrap()) {
            (Agent::Silent, _) | (Agent::SyncOnly, "guest-ping") => None,
            (_, "guest-ping") => Some(b"{\"return\": {}}\n".to_vec()),
            (_, other) => panic!("unexpected command {other}"),
        })
    }

    /// 가짜 에이전트: sync에는 지난 답 하나를 섞어 답하고, 다른 명령은 `reply`가 정한다(None이면 답하지 않음).
    /// 연결을 여러 번 받는다.
    fn serve_with(
        reply: impl Fn(&Value) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qga.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let reply = Arc::new(reply);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let reply = reply.clone();
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut read = BufReader::new(read);
                    let mut buf = Vec::new();
                    loop {
                        buf.clear();
                        if read.read_until(b'\n', &mut buf).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let Some(start) = buf.iter().position(|b| *b == b'{') else {
                            continue;
                        };
                        let cmd: Value = serde_json::from_slice(&buf[start..]).unwrap();
                        let out = if cmd["execute"] == "guest-sync-delimited" {
                            // 앞선 연결의 sync 답과 읽을 수 없는 줄이 먼저 남아 있다.
                            let id = &cmd["arguments"]["id"];
                            let mut r = vec![DELIMITER];
                            r.extend_from_slice(b"{\"return\": 7}\n");
                            r.push(DELIMITER);
                            r.extend_from_slice(b"garbage\n");
                            r.push(DELIMITER);
                            r.extend_from_slice(format!("{{\"return\": {id}}}\n").as_bytes());
                            Some(r)
                        } else {
                            reply(&cmd)
                        };
                        if let Some(out) = out
                            && write.write_all(&out).await.is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        (dir, path)
    }

    #[tokio::test]
    async fn an_answering_agent_is_alive() {
        let (_dir, path) = serve(Agent::Answers);
        assert!(ping(&path, Duration::from_secs(3)).await.unwrap());
    }

    #[tokio::test]
    async fn an_agent_that_does_not_answer_the_ping_is_not() {
        for agent in [Agent::SyncOnly, Agent::Silent] {
            let (_dir, path) = serve(agent);
            assert!(!ping(&path, Duration::from_millis(200)).await.unwrap());
        }
    }

    /// 채널이 없거나(파일 없음) 아무도 받지 않으면(연결 거부) 오류다.
    ///
    /// 듣는 소켓을 닫아도 같은 시험 프로세스의 다른 시험이 그 순간 자식 프로세스를 띄우면(fork) 자식이 exec할 때까지
    /// 그 소켓을 들고 있어 연결이 잠깐 받아진다(병렬 시험에서 가끔 `Ok(false)`). 그래서 연결이 한 번 거부될 때까지
    /// 기다린 뒤(그 뒤로는 소켓이 다시 살아나지 않는다) `ping`을 본다. 두 오류 모두 곧바로 나므로 시간 제한은 넉넉히 둔다.
    #[tokio::test]
    async fn a_missing_channel_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qga.sock");
        let generous = Duration::from_secs(30);
        let err = ping(&path, generous).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        let deadline = std::time::Instant::now() + generous;
        while std::os::unix::net::UnixStream::connect(&path).is_ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "the socket stayed open"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let err = ping(&path, generous).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
    }

    fn ret(v: Value) -> Option<Vec<u8>> {
        Some(format!("{}\n", json!({ "return": v })).into_bytes())
    }

    /// 다섯 명령의 답을 모으고, 오류로 답한 명령(`guest-get-fsinfo`)의 칸은 비운다. 읽을 수 없는 주소는 버린다.
    #[tokio::test]
    async fn guest_info_collects_what_the_agent_answers() {
        let (_dir, path) = serve_with(|cmd| match cmd["execute"].as_str().unwrap() {
            "guest-info" => ret(json!({ "version": "8.2.2", "supported_commands": [] })),
            "guest-get-osinfo" => ret(json!({
                "id": "ubuntu", "pretty-name": "Ubuntu 24.04.1 LTS", "version": "24.04.1 LTS (Noble Numbat)",
                "kernel-release": "6.8.0-45-generic", "machine": "x86_64"
            })),
            "guest-get-host-name" => ret(json!({ "host-name": "web-1" })),
            "guest-network-get-interfaces" => ret(json!([
                { "name": "lo", "hardware-address": "00:00:00:00:00:00", "ip-addresses": [
                    { "ip-address-type": "ipv4", "ip-address": "127.0.0.1", "prefix": 8 } ] },
                { "name": "enp0s2", "hardware-address": "52:54:00:12:34:56", "ip-addresses": [
                    { "ip-address-type": "ipv4", "ip-address": "10.0.2.15", "prefix": 24 },
                    { "ip-address-type": "ipv6", "ip-address": "fe80::5054:ff:fe12:3456", "prefix": 64 },
                    { "ip-address-type": "ipv4", "ip-address": "not-an-ip", "prefix": 24 },
                    { "ip-address-type": "ipv4", "ip-address": "10.0.0.1", "prefix": 99 } ] },
                { "hardware-address": "no name" }
            ])),
            "guest-get-fsinfo" => Some(
                b"{\"error\": {\"class\": \"CommandNotFound\", \"desc\": \"frozen\"}}\n".to_vec(),
            ),
            other => panic!("unexpected {other}"),
        });
        let mut s = Session::connect(&path, SYNC_TIMEOUT).await.unwrap();
        let info = guest_info(&mut s, 42).await;
        assert_eq!(info.collected_at, 42);
        assert_eq!(info.agent_version.as_deref(), Some("8.2.2"));
        let os = info.os.unwrap();
        assert_eq!(os.id.as_deref(), Some("ubuntu"));
        assert_eq!(os.name.as_deref(), Some("Ubuntu 24.04.1 LTS"));
        assert_eq!(os.kernel.as_deref(), Some("6.8.0-45-generic"));
        assert_eq!(info.hostname.as_deref(), Some("web-1"));
        assert_eq!(info.interfaces.len(), 2);
        let nic = &info.interfaces[1];
        assert_eq!(nic.mac.as_deref(), Some("52:54:00:12:34:56"));
        assert_eq!(
            nic.addresses,
            [
                GuestAddr {
                    ip: "10.0.2.15".parse().unwrap(),
                    prefix: 24
                },
                GuestAddr {
                    ip: "fe80::5054:ff:fe12:3456".parse().unwrap(),
                    prefix: 64
                }
            ]
        );
        assert!(info.filesystems.is_empty());
    }

    #[test]
    fn parses_filesystems_and_truncates_strings() {
        let fs = parse_filesystems(&json!([
            { "name": "sda1", "mountpoint": "/", "type": "ext4", "used-bytes": 100, "total-bytes": 1000, "disk": [] },
            { "name": "C:", "mountpoint": "C:\\", "type": "NTFS" },
            { "name": "no mountpoint" }
        ]));
        assert_eq!(
            fs,
            [
                GuestFs {
                    mountpoint: "/".into(),
                    fstype: "ext4".into(),
                    used_bytes: Some(100),
                    total_bytes: Some(1000)
                },
                GuestFs {
                    mountpoint: "C:\\".into(),
                    fstype: "NTFS".into(),
                    used_bytes: None,
                    total_bytes: None
                }
            ]
        );
        assert!(parse_interfaces(&json!({"not": "a list"})).is_empty());
        assert_eq!(parse_os(&json!({})), GuestOs::default());
    }

    /// 에이전트가 멈추면 명령은 시간 초과다. 다음 세션은 다시 맞춰서 쓸 수 있다.
    #[tokio::test]
    async fn a_hanging_agent_times_out_and_a_new_session_resyncs() {
        let calls = Arc::new(Mutex::new(0));
        let seen = calls.clone();
        let (_dir, path) = serve_with(move |cmd| {
            assert_eq!(cmd["execute"], "guest-ping");
            let mut n = seen.lock().unwrap();
            *n += 1;
            // 첫 ping에는 답하지 않는다.
            (*n > 1).then(|| b"{\"return\": {}}\n".to_vec())
        });
        let mut s = Session::connect(&path, SYNC_TIMEOUT).await.unwrap();
        let err = s
            .execute("guest-ping", None, Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(
            matches!(err, QgaError::Timeout(ref c) if c == "guest-ping"),
            "{err}"
        );
        drop(s);
        let mut s = Session::connect(&path, SYNC_TIMEOUT).await.unwrap();
        s.execute("guest-ping", None, AGENT_TIMEOUT).await.unwrap();
        assert_eq!(*calls.lock().unwrap(), 2);
    }

    /// 1 MiB를 넘는 줄은 `TooLong`, JSON이 아닌 답은 `BadReply`다.
    #[tokio::test]
    async fn oversized_and_malformed_replies_are_errors() {
        let (_dir, path) = serve_with(|cmd| match cmd["execute"].as_str().unwrap() {
            "guest-get-osinfo" => {
                let mut big = vec![b'x'; MAX_REPLY_BYTES + 10];
                big.push(b'\n');
                Some(big)
            }
            _ => Some(b"{not json\n".to_vec()),
        });
        let mut s = Session::connect(&path, SYNC_TIMEOUT).await.unwrap();
        let err = s
            .execute("guest-get-osinfo", None, AGENT_TIMEOUT)
            .await
            .unwrap_err();
        assert!(matches!(err, QgaError::TooLong), "{err}");
        let mut s = Session::connect(&path, SYNC_TIMEOUT).await.unwrap();
        let err = s
            .execute("guest-info", None, AGENT_TIMEOUT)
            .await
            .unwrap_err();
        assert!(matches!(err, QgaError::BadReply(_)), "{err}");
    }

    /// `guest-shutdown`은 성공하면 답하지 않는다: 답이 없거나 연결이 끊기면 보낸 것이다. 오류 답은 오류다.
    #[tokio::test]
    async fn shutdown_without_a_reply_counts_as_sent() {
        let (_dir, path) = serve_with(|cmd| {
            assert_eq!(cmd["arguments"]["mode"], "powerdown");
            None
        });
        let mut s = Session::connect(&path, SYNC_TIMEOUT).await.unwrap();
        shutdown(&mut s, Duration::from_millis(200)).await.unwrap();

        let (_dir, path) = serve_with(|_| {
            Some(
                b"{\"error\": {\"class\": \"GenericError\", \"desc\": \"not allowed\"}}\n".to_vec(),
            )
        });
        let mut s = Session::connect(&path, SYNC_TIMEOUT).await.unwrap();
        let err = shutdown(&mut s, AGENT_TIMEOUT).await.unwrap_err();
        assert!(err.is_command(), "{err}");
    }

    /// freeze 실패는 에이전트의 오류 그대로다. thaw는 그 뒤에도 같은 세션으로 보낼 수 있다.
    #[tokio::test]
    async fn freeze_failure_and_thaw() {
        let (_dir, path) = serve_with(|cmd| {
            match cmd["execute"].as_str().unwrap() {
            "guest-fsfreeze-freeze" => Some(
                b"{\"error\": {\"class\": \"GenericError\", \"desc\": \"failed to freeze /: Operation not supported\"}}\n"
                    .to_vec(),
            ),
            "guest-fsfreeze-thaw" => ret(json!(0)),
            other => panic!("unexpected {other}"),
        }
        });
        let mut s = Session::connect(&path, SYNC_TIMEOUT).await.unwrap();
        let err = freeze(&mut s).await.unwrap_err();
        assert!(err.to_string().contains("Operation not supported"), "{err}");
        thaw(&mut s).await.unwrap();

        let (_dir, path) = serve_with(|_| ret(json!(2)));
        let mut s = Session::connect(&path, SYNC_TIMEOUT).await.unwrap();
        assert_eq!(freeze(&mut s).await.unwrap(), 2);
    }

    /// 비밀번호는 base64로 보내고(`crypted: false`) 오류 문구에 들어가지 않는다.
    #[tokio::test]
    async fn set_user_password_sends_base64() {
        let (_dir, path) = serve_with(|cmd| {
            assert_eq!(cmd["execute"], "guest-set-user-password");
            assert_eq!(cmd["arguments"]["username"], "Administrator");
            assert_eq!(cmd["arguments"]["password"], "czNjcmV0IQ==");
            assert_eq!(cmd["arguments"]["crypted"], false);
            ret(json!({}))
        });
        let mut s = Session::connect(&path, SYNC_TIMEOUT).await.unwrap();
        set_user_password(&mut s, "Administrator", "s3cret!")
            .await
            .unwrap();

        let (_dir, path) = serve_with(|_| {
            Some(
                b"{\"error\": {\"class\": \"GenericError\", \"desc\": \"child process failed\"}}\n"
                    .to_vec(),
            )
        });
        let mut s = Session::connect(&path, SYNC_TIMEOUT).await.unwrap();
        let err = set_user_password(&mut s, "root", "s3cret!")
            .await
            .unwrap_err();
        assert!(!err.to_string().contains("s3cret"), "{err}");
    }

    /// 파일 명령과 exec: 인자 이름(`buf-b64`·`capture-output`)과 답 읽기, 막힌 명령은 `Command` 오류다.
    #[tokio::test]
    async fn file_and_exec_commands() {
        let (_dir, path) = serve_with(|cmd| {
            let a = &cmd["arguments"];
            match cmd["execute"].as_str().unwrap() {
                "guest-info" => ret(json!({ "version": "10.0", "supported_commands": [
                    { "name": "guest-file-open", "enabled": true, "success-response": true },
                    { "name": "guest-exec", "enabled": false, "success-response": true },
                    { "enabled": true }
                ] })),
                "guest-file-open" if a["path"] == "/missing" => Some(
                    b"{\"error\": {\"class\": \"GenericError\", \"desc\": \"No such file\"}}\n"
                        .to_vec(),
                ),
                "guest-file-open" => {
                    assert_eq!(a["mode"], "wb");
                    ret(json!(1000))
                }
                "guest-file-write" => {
                    assert_eq!(a["handle"], 1000);
                    assert_eq!(a["buf-b64"], "aGVsbG8=");
                    ret(json!({ "count": 5, "eof": false }))
                }
                "guest-file-flush" | "guest-file-close" => {
                    assert_eq!(a["handle"], 1000);
                    ret(json!({}))
                }
                "guest-exec" => {
                    assert_eq!(a["path"], "/bin/mkdir");
                    assert_eq!(a["arg"], json!(["-p", "--", "/x"]));
                    assert_eq!(a["capture-output"], true);
                    ret(json!({ "pid": 42 }))
                }
                "guest-exec-status" => ret(json!({
                    "exited": true, "exitcode": 1, "err-data": "bm8gd2F5", "out-data": "!!"
                })),
                other => panic!("unexpected {other}"),
            }
        });
        let mut s = Session::connect(&path, SYNC_TIMEOUT).await.unwrap();
        assert_eq!(
            supported_commands(&mut s).await.unwrap(),
            [
                ("guest-file-open".to_string(), true),
                ("guest-exec".to_string(), false)
            ]
        );
        let err = file_open(&mut s, "/missing", "r").await.unwrap_err();
        assert!(err.is_command(), "{err}");
        let h = file_open(&mut s, "/x/a", "wb").await.unwrap();
        assert_eq!(file_write(&mut s, h, "aGVsbG8=").await.unwrap(), 5);
        file_flush(&mut s, h).await.unwrap();
        file_close(&mut s, h).await.unwrap();
        let args = ["-p".to_string(), "--".into(), "/x".into()];
        let pid = exec(&mut s, "/bin/mkdir", &args).await.unwrap();
        let st = exec_status(&mut s, pid).await.unwrap();
        assert_eq!(
            st,
            ExecStatus {
                exited: true,
                code: Some(1),
                signal: None,
                stdout: Vec::new(),
                stderr: b"no way".to_vec()
            }
        );
    }

    /// sync에 답하지 않는 채널(게스트 에이전트 없음)은 `Timeout`이다.
    #[tokio::test]
    async fn a_silent_channel_does_not_sync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qga.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let hold = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(stream);
        });
        let err = Session::connect(&path, Duration::from_millis(200))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, QgaError::Timeout(_)), "{err}");
        hold.abort();
    }
}
