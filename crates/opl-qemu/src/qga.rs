//! 게스트 에이전트(QGA) 채널에 `guest-ping`을 보낸다(`ha.guest_watch`).
//!
//! QEMU의 채널 소켓은 게스트가 포트를 열지 않았어도 연결을 받으므로, 게스트 에이전트가 없거나 멈추면 답이
//! 오지 않는다. 그래서 답이 없는 것은 오류가 아니라 `false`다.

use std::io;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// `guest-sync-delimited`의 답 앞에 에이전트가 보내는 구분 바이트. 보내는 쪽이 먼저 보내면 에이전트가 앞서
/// 받다 만 명령을 버린다.
const DELIMITER: u8 = 0xFF;

/// 에이전트에 `guest-sync-delimited`로 앞서 남은 답을 걸러 낸 뒤 `guest-ping`을 보낸다. `timeout` 안에 답이
/// 오면 true, 오지 않거나 연결이 끊기면 false다. 소켓에 연결하지 못하면(채널이 없음, 연결 거부) 오류다.
pub async fn ping(path: &Path, timeout: Duration) -> io::Result<bool> {
    let stream = UnixStream::connect(path).await?;
    match tokio::time::timeout(timeout, exchange(stream)).await {
        Ok(Ok(answered)) => Ok(answered),
        // 연결이 끊겼다(QEMU가 끝나는 중 등). 답이 없는 것으로 본다.
        Ok(Err(_)) | Err(_) => Ok(false),
    }
}

async fn exchange(stream: UnixStream) -> io::Result<bool> {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    // 앞선 연결이 남긴 답과 섞이지 않도록 이번 연결만의 id로 맞춘다.
    let id = u64::from(uuid::Uuid::now_v7().as_u128() as u32);
    let sync = serde_json::json!({ "execute": "guest-sync-delimited", "arguments": { "id": id } });
    write.write_all(&[DELIMITER]).await?;
    write.write_all(format!("{sync}\n").as_bytes()).await?;
    loop {
        let mut skipped = Vec::new();
        if read.read_until(DELIMITER, &mut skipped).await? == 0 {
            return Ok(false);
        }
        let Some(answer) = next_json(&mut read).await? else {
            return Ok(false);
        };
        if answer["return"].as_u64() == Some(id) {
            break;
        }
    }
    write.write_all(b"{\"execute\":\"guest-ping\"}\n").await?;
    // 오류 답이어도 에이전트가 살아 답한 것이다.
    Ok(next_json(&mut read)
        .await?
        .is_some_and(|answer| answer.get("return").is_some() || answer.get("error").is_some()))
}

/// 다음 JSON 한 줄. 연결이 끝나면 None, 읽을 수 없는 줄은 빈 값이다.
async fn next_json(
    read: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> io::Result<Option<Value>> {
    let mut line = Vec::new();
    if read.read_until(b'\n', &mut line).await? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&line).unwrap_or(Value::Null)))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qga.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                if read.read_until(b'\n', &mut buf).await.unwrap() == 0 {
                    return;
                }
                let start = buf.iter().position(|b| *b == b'{').unwrap();
                let cmd: Value = serde_json::from_slice(&buf[start..]).unwrap();
                let reply = match (agent, cmd["execute"].as_str().unwrap()) {
                    (Agent::Silent, _) | (Agent::SyncOnly, "guest-ping") => continue,
                    (_, "guest-sync-delimited") => {
                        // 앞선 연결의 sync 답이 먼저 남아 있다.
                        let id = &cmd["arguments"]["id"];
                        let mut reply = vec![DELIMITER];
                        reply.extend_from_slice(b"{\"return\": 7}\n");
                        reply.push(DELIMITER);
                        reply.extend_from_slice(format!("{{\"return\": {id}}}\n").as_bytes());
                        reply
                    }
                    (_, "guest-ping") => b"{\"return\": {}}\n".to_vec(),
                    (_, other) => panic!("unexpected command {other}"),
                };
                write.write_all(&reply).await.unwrap();
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
    #[tokio::test]
    async fn a_missing_channel_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qga.sock");
        assert!(ping(&path, Duration::from_millis(200)).await.is_err());
        drop(UnixListener::bind(&path).unwrap());
        let err = ping(&path, Duration::from_millis(200)).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
    }
}
