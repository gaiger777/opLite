//! 로컬 TCP 포트를 열어 VNC 뷰어의 연결을 API의 콘솔 WebSocket으로 잇는다.

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::client::Client;

/// 연결을 받을 때마다 WebSocket을 하나 연다. 호출자가 멈출 때까지 돈다.
pub async fn serve(client: Client, vm: String, listener: TcpListener) -> anyhow::Result<()> {
    loop {
        let (tcp, _) = listener.accept().await?;
        let client = client.clone();
        let vm = vm.clone();
        tokio::spawn(async move {
            if let Err(e) = bridge(&client, &vm, tcp).await {
                eprintln!("console connection closed: {e:#}");
            }
        });
    }
}

async fn bridge(client: &Client, vm: &str, mut tcp: TcpStream) -> anyhow::Result<()> {
    let url = client
        .url(&format!("/vms/{vm}/console"))
        .replacen("http", "ws", 1);
    let mut req = url.into_client_request()?;
    if let Some(token) = &client.token {
        req.headers_mut()
            .insert("authorization", format!("Bearer {token}").parse()?);
    }
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .context("cannot open console")?;
    let (mut sink, mut stream) = ws.split();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        tokio::select! {
            n = tcp.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    let _ = sink.send(Message::Close(None)).await;
                    return Ok(());
                }
                sink.send(Message::binary(buf[..n].to_vec())).await?;
            }
            msg = stream.next() => match msg {
                Some(Ok(Message::Binary(data))) => tcp.write_all(&data).await?,
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
            },
        }
    }
}
