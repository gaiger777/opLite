//! 시험 고리 `fault-it`: 끊으면 이 프로세스의 보내기와 받기가 모두 `unavailable`이고, 이으면 다시 된다.
//! 프로세스 전역 값이라 다른 시험과 같은 바이너리에 두지 않는다.
//! `cargo test -p opl-peer --features fault-it --test fault`
#![cfg(feature = "fault-it")]

use std::time::Duration;

use bytes::{Buf, BufMut};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{GrpcNetwork, PeerService, Peers, fault};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::codegen::http::uri::PathAndQuery;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity};
use tonic::{Code, Status};
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(5);

fn tls_for(ca: &ClusterCa, id: NodeId) -> NodeTls {
    let (key_pem, csr) = node_key_and_csr().unwrap();
    NodeTls {
        ca_pem: ca.cert_pem.clone(),
        cert_pem: ca.sign_node(&csr, id.0, &["127.0.0.1".into()]).unwrap(),
        key_pem,
    }
}

/// `Peers`를 거치지 않는 Ping(받는 쪽만 보려고): 빈 요청 `null`, 답은 JSON 그대로
#[derive(Default)]
struct PingCodec;

struct PingEncoder;
struct PingDecoder;

impl Codec for PingCodec {
    type Encode = ();
    type Decode = serde_json::Value;
    type Encoder = PingEncoder;
    type Decoder = PingDecoder;

    fn encoder(&mut self) -> PingEncoder {
        PingEncoder
    }

    fn decoder(&mut self) -> PingDecoder {
        PingDecoder
    }
}

impl Encoder for PingEncoder {
    type Item = ();
    type Error = Status;

    fn encode(&mut self, _: (), dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        dst.put_slice(b"null");
        Ok(())
    }
}

impl Decoder for PingDecoder {
    type Item = serde_json::Value;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<serde_json::Value>, Status> {
        serde_json::from_slice(&src.copy_to_bytes(src.remaining()))
            .map(Some)
            .map_err(|e| Status::internal(e.to_string()))
    }
}

async fn raw_ping(tls: &NodeTls, addr: &str) -> Result<serde_json::Value, Status> {
    let config = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(&tls.ca_pem))
        .identity(Identity::from_pem(&tls.cert_pem, &tls.key_pem));
    let channel = Endpoint::from_shared(format!("https://{addr}"))
        .unwrap()
        .tls_config(config)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready().await.unwrap();
    let reply = grpc
        .unary(
            tonic::Request::new(()),
            PathAndQuery::from_static("/opl.peer.v1.Peer/Ping"),
            PingCodec,
        )
        .await?;
    Ok(reply.into_inner())
}

#[tokio::test]
async fn an_isolated_process_neither_sends_nor_receives_until_healed() {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    // 받는 노드(초기화하지 않은 클러스터라 인증서만 맞으면 받는다)와 부르는 노드
    let server_id = NodeId(Uuid::now_v7());
    let server_tls = tls_for(&ca, server_id);
    let caller_tls = tls_for(&ca, NodeId(Uuid::now_v7()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server_peers = std::sync::Arc::new(Peers::new(&server_tls));
    let cluster = std::sync::Arc::new(
        Cluster::open(
            ClusterConfig::new(server_id, root.path().join("server")),
            GrpcNetwork::new(server_peers),
        )
        .await
        .unwrap(),
    );
    let service = PeerService::new(cluster.clone(), None);
    let server = tokio::spawn(async move {
        opl_peer::serve(listener, &server_tls, service, std::future::pending::<()>())
            .await
            .unwrap();
    });
    let peers = Peers::new(&caller_tls);
    assert!(!fault::isolated());
    assert_eq!(
        peers.ping(server_id, &addr, TIMEOUT).await.unwrap(),
        server_id
    );
    assert!(raw_ping(&caller_tls, &addr).await.is_ok());

    fault::isolate();
    assert!(fault::isolated());
    // 보내기: 나가기 전에 막힌다.
    let err = peers.ping(server_id, &addr, TIMEOUT).await.unwrap_err();
    assert_eq!(err.code(), Code::Unavailable, "{err}");
    assert!(err.message().contains("not sending"), "{err}");
    // 받기: `Peers`를 거치지 않은 호출도 받는 쪽이 거절한다.
    let err = raw_ping(&caller_tls, &addr).await.unwrap_err();
    assert_eq!(err.code(), Code::Unavailable, "{err}");
    assert!(err.message().contains("not receiving"), "{err}");

    fault::heal();
    assert!(!fault::isolated());
    assert_eq!(
        peers.ping(server_id, &addr, TIMEOUT).await.unwrap(),
        server_id
    );
    assert!(raw_ping(&caller_tls, &addr).await.is_ok());

    server.abort();
    cluster.shutdown().await.unwrap();
}
