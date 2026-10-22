//! 로컬 TCP 포트를 열어 VNC 뷰어의 연결을 API의 콘솔 WebSocket으로 잇는다.

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::Connector;
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

/// API의 콘솔 WebSocket을 연다. 저장한 클러스터 CA가 있으면 서버를 그 CA로만 확인한다.
async fn open(
    client: &Client,
    vm: &str,
) -> anyhow::Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>>
{
    let url = client
        .url(&format!("/vms/{vm}/console"))
        .replacen("http", "ws", 1);
    let mut req = url.into_client_request()?;
    if let Some(token) = &client.token {
        req.headers_mut()
            .insert("authorization", format!("Bearer {token}").parse()?);
    }
    let connector = match &client.ca_pem {
        Some(pem) => Some(Connector::NativeTls(
            native_tls::TlsConnector::builder()
                .add_root_certificate(native_tls::Certificate::from_pem(pem.as_bytes())?)
                .disable_built_in_roots(true)
                .build()?,
        )),
        None => None,
    };
    let (ws, _) = tokio_tungstenite::connect_async_tls_with_config(req, None, false, connector)
        .await
        .context("cannot open console")?;
    Ok(ws)
}

async fn bridge(client: &Client, vm: &str, mut tcp: TcpStream) -> anyhow::Result<()> {
    let ws = open(client, vm).await?;
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
