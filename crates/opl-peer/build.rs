//! 노드 간 gRPC 서비스 정의. `.proto` 대신 Rust 타입을 serde JSON으로 주고받는다(`protoc`가 필요 없다).
//! 콘솔 스트림은 바이트를 그대로, UEFI 변수 조각은 이진 형식으로 보낸다.

use tonic_build::manual::{Builder, Method, Service};

fn json(name: &str, route: &str, input: &str, output: &str) -> Method {
    Method::builder()
        .name(name)
        .route_name(route)
        .input_type(input)
        .output_type(output)
        .codec_path("crate::codec::JsonCodec")
        .build()
}

fn main() {
    let service = Service::builder()
        .name("Peer")
        .package("opl.peer.v1")
        .method(json(
            "append_entries",
            "AppendEntries",
            "crate::wire::AppendEntriesRequest",
            "crate::wire::AppendEntriesReply",
        ))
        .method(json(
            "install_snapshot",
            "InstallSnapshot",
            "crate::wire::InstallSnapshotRequest",
            "crate::wire::InstallSnapshotReply",
        ))
        .method(json(
            "vote",
            "Vote",
            "crate::wire::VoteRequest",
            "crate::wire::VoteReply",
        ))
        .method(json(
            "leader",
            "Leader",
            "opl_cluster::types::LeaderRequest",
            "crate::wire::LeaderReply",
        ))
        .method(json(
            "ping",
            "Ping",
            "crate::wire::Empty",
            "opl_cluster::types::NodeId",
        ))
        .method(json(
            "migrate_incoming",
            "MigrateIncoming",
            "crate::migration::MigrateIncomingRequest",
            "crate::wire::MigrateIncomingReply",
        ))
        .method(json(
            "migrate_incoming_wait",
            "MigrateIncomingWait",
            "crate::migration::IncomingMigration",
            "crate::wire::IncomingReply",
        ))
        .method(json(
            "migrate_incoming_abort",
            "MigrateIncomingAbort",
            "crate::migration::IncomingMigration",
            "crate::wire::IncomingReply",
        ))
        .method(
            Method::builder()
                .name("uefi_vars")
                .route_name("UefiVars")
                .input_type("crate::firmware::UefiVarsRequest")
                .output_type("crate::firmware::UefiVarsChunk")
                .codec_path("crate::codec::BinaryCodec")
                .server_streaming()
                .build(),
        )
        .method(
            Method::builder()
                .name("console")
                .route_name("Console")
                .input_type("bytes::Bytes")
                .output_type("bytes::Bytes")
                .codec_path("crate::codec::RawCodec")
                .client_streaming()
                .server_streaming()
                .build(),
        )
        .build();
    Builder::new().compile(&[service]);
}
