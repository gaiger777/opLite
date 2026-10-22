# opLite 계획 6: 클러스터 부트스트랩과 노드 간 통신 (M6) 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 노드 여러 개가 하나의 클러스터로 동작하게 한다. `oplctl cluster init`으로 클러스터 CA와 1인 클러스터를 만들고, 조인 URL로 노드를 더하고(learner → voter), 탈퇴·죽은 노드 제거를 한다. 노드 간 통신은 gRPC over mTLS, 외부 API는 HTTPS다. 팔로워가 받은 쓰기는 리더로 전달하고, 다른 노드의 VM 콘솔은 중계하고, 리더가 노드 생존을 기록한다. macOS에서 opld 프로세스 3개로 클러스터를 구성하는 E2E 테스트로 고정한다(M6).

**Architecture:** 새 크레이트 `opl-peer`가 클러스터 PKI(rcgen)와 노드 간 gRPC(tonic, rustls ring)를 맡는다. `.proto` 대신 tonic의 수동 서비스 정의로 Rust 타입을 serde JSON으로 보낸다(`protoc`가 필요 없다). `opl-cluster`는 리더 요청(`LeaderRequest`: 쓰기, learner 추가, voter 승격, 제거)과 그것을 리더로 보내는 `Forwarder` 트레이트를 갖고, 상태 머신이 VM의 볼륨·MAC 중복을 거부한다. `opld`는 노드 인증서가 없으면 제어 소켓(유닉스 소켓)만 열고 `init`/`join`을 기다리고, 멤버가 되면 노드 간 서버·저장소·reconciler·Task 실행기·HTTPS API·노드 감시를 띄운다. `oplctl`은 처음 로그인할 때 CA 지문으로 서버를 확인한다.

**Tech Stack:** Rust 2024(MSRV 1.88), tonic 0.14(수동 서비스, tls-ring) + tonic-build 0.14, rcgen 0.14(x509-parser), rustls 0.23(ring) + tokio-rustls 0.26, x509-parser 0.18, reqwest 0.13(native-tls), native-tls 0.2, openraft 0.9, axum 0.8

**설계 문서:** `docs/superpowers/specs/2026-10-20-opl-node-core-design.md` (2.2, 3.2, 4, 8, 10, 11, 12절)
**선행 계획:** 계획 1~5 (완료)

---

## 범위

| 포함 | 제외 (담당 계획) |
|---|---|
| 클러스터 CA, 노드 키·CSR·인증서, CA 지문 | 노드 인증서 갱신(824일 만료 전), 사용자 인증서로 교체 — 다음 계획 |
| 노드 간 gRPC over mTLS: Raft RPC, 리더 전달, Ping, 콘솔 중계, 멤버 확인 | 노드 주소 바꾸기 |
| `oplctl cluster init/join/leave/status/join-token/remove-node`, 제어 소켓 | 위트니스, HA·펜싱 — 하위 프로젝트 5 |
| 팔로워 쓰기 전달(같은 노드에서 곧바로 읽기 보장), 리더 선출 중 재시도 | 로그인 시도 제한, 감사 로그 — 나중에 |
| 상태 머신의 VM 볼륨·MAC 중복 거부 | 볼륨 삭제와 붙이기의 노드 간 경합(Ready 확인은 API에서) — 나중에 |
| HTTPS API, `Secure` 쿠키, `oplctl` CA 지문 확인과 WSS 콘솔 | 웹 UI — 하위 프로젝트 2 |
| 리더의 노드 생존 감시(`NodeStatus.online`), 노드별 고아 정리, Task 기다리기 제한 시간 | QemuKvm, bridge 네트워크 — 계획 7 |
| E2E: opld 프로세스 3개(init, join, 쓰기 전달, 리더 kill, 제거, 탈퇴·재합류), 실제 QEMU E2E를 HTTPS로 | 기존 M5 데이터 디렉터리 이전(지우고 다시 만든다) |

## 이전 계획에서 이어받는 사항

계획 5의 "다음 계획으로 넘길 사항"에서:
- `memnet`을 gRPC + mTLS로 바꾸고 HTTPS를 연다. opld의 루프백 제한을 풀고 세션 쿠키에 `Secure`를 붙인다. → Task 4, 5, 7, 10
- 팔로워가 받은 API 쓰기를 리더로 전달한다. 다른 노드에서 도는 VM의 콘솔은 노드 간 중계한다. → Task 3, 5, 7
- 노드 heartbeat로 `NodeStatus.online`을 판정한다. → Task 6 (리더가 Ping)
- Task 실행기의 고아 정리는 자기 노드에서 실행할 볼륨만 본다. → Task 1, 7 (`VolumeStatus.node`)
- 볼륨·MAC 중복을 클러스터 수준에서 막는다. → Task 2 (상태 머신)
- `opld`의 첫 관리자 비밀번호 파일과 1인 클러스터 초기화를 `oplctl cluster init`/`join`으로 옮긴다. → Task 9, 10
- `oplctl`의 Task 기다리기에 제한 시간을 둔다. → Task 9

## 설계 결정

- **노드 간 통신은 tonic gRPC, 메시지는 serde JSON.** 설계 문서대로 gRPC(HTTP/2)를 쓰되 `.proto`와 prost 없이 `tonic-build`의 수동 서비스 정의(`build.rs`)로 Rust 타입을 그대로 보낸다. Raft RPC는 openraft 타입과 그 오류(`RaftError`)를 그대로 보내 상대 노드의 openraft가 해석한다. 콘솔 스트림만 바이트를 그대로 보내는 코덱을 쓴다. `protoc`가 필요 없다. 스냅샷 조각도 JSON(바이트가 숫자 배열)이라 크지만, 설정만 담는 저장소라 받아들인다. 한 메시지 최대 64MiB, 스냅샷 조각 1MiB.
- **TLS는 rustls + ring.** aws-lc는 라이선스 검사에 걸릴 수 있어 쓰지 않는다. 노드 간 서버는 클라이언트 인증서를 요구하고(클러스터 CA), 상대 인증서의 CN(노드 id)이 지금 멤버인지도 확인한다. 아직 멤버십을 받지 못한 노드(막 합류하는 중)는 CA 서명만 확인한다. 제거된 노드는 인증서가 유효해도 막힌다.
- **클라이언트 쪽 HTTPS는 native-tls(OS 구현).** reqwest의 rustls 경로는 `rustls-platform-verifier`를 거쳐 CDLA-Permissive-2.0 라이선스(`webpki-root-certs`)를 끌어온다. 허용 목록을 넓히는 대신 OS TLS를 쓴다(2026-10-22 결정). 그래서 Linux 빌드에 `libssl-dev`가 필요하다. macOS는 유효 기간이 825일을 넘는 TLS 서버 인증서를 거부하므로 노드 인증서는 824일짜리다(CA는 20년). 갱신은 다음 계획에서 한다.
- **CA 개인키는 Raft에 복제한다.** `Cluster` 리소스(이름 = 클러스터 이름)에 CA 인증서와 키를 둔다. 어느 노드든 합류를 받을 수 있고, 첫 노드가 죽어도 노드를 더할 수 있다(Proxmox pmxcfs와 같다). API는 키를 내보내지 않고, `Debug`에도 찍히지 않는다.
- **합류는 CSR로 한다.** 새 노드가 키를 만들고 CSR만 보낸다. 받는 노드는 CSR에서 공개키만 쓰고 이름(호스트 + localhost/127.0.0.1/::1), 용도(serverAuth + clientAuth), 기간은 스스로 정한다. CSR이 CA 권한을 요청해도 따르지 않는다. 조인 토큰은 `oplj_<id>_<비밀>`이고 해시만 저장하며 한 번 쓰면 지운다(동시 사용은 `expected_revision`으로 하나만 성공). CSR 서명이 끝난 뒤에 토큰을 지우므로 잘못된 CSR로 토큰이 버려지지 않는다. learner로 멈춘 노드는 새 토큰으로 다시 합류할 수 있다.
- **조인 URL은 `opl-join://<API 주소>?token=…&ca=sha256:…`.** 새 노드는 상대의 CA를 인증서 검증 없이 받아 URL의 지문과 맞는지 확인하고, 그 뒤로는 그 CA만 믿는다.
- **승격은 새 노드가 요청한다.** 받는 노드는 새 노드를 learner로만 더하고(따라잡기를 기다리지 않는다) 인증서를 돌려준다. 새 노드는 받은 인증서로 노드 간 서버를 띄운 뒤, 받아 준 노드에 `AddVoter`를 보낸다. 리더는 learner가 자기 마지막 로그까지 받으면(최대 60초) 투표 멤버로 올린다. 받은 노드가 리더가 아니면 알려 준 리더로 다시 보낸다.
- **쓰기 전달은 `Cluster` 안에서 한다.** `Cluster::request(LeaderRequest)`는 리더면 처리하고, 아니면 `Forwarder`로 리더에 보낸 뒤 그 인덱스까지 이 노드에도 적용되기를 기다린다. API, reconciler, Task 실행기 모두 그대로 쓴다. 리더가 없거나(선거 중) 닿지 않으면 `write_timeout`까지 다시 시도한다. openraft는 리더 임기 동안 다른 노드에 투표하지 않아 리더를 잃으면 보통 선거 제한 시간의 두 배쯤 걸리므로, opld의 `write_timeout`은 10초다. 전달기가 없으면(테스트의 인메모리 클러스터) 예전처럼 `NotLeader`를 돌려준다.
- **VM 볼륨·MAC 중복은 상태 머신이 거부한다.** 여러 노드의 API가 각자 확인하고 쓰면 경합한다. 상태 머신이 VM spec의 `disks[].volume`과 `nics[].mac`을 커밋 순서대로 확인하고 `Rejection::InUse`로 거부한다(API는 409 `volume_in_use`/`mac_in_use`). API의 기존 확인(친절한 오류)은 그대로 둔다.
- **opld의 두 상태.** 노드 인증서(`{data_dir}/pki`)가 없으면 미초기화 노드다. 제어 소켓(`{run_dir}/opld.sock`, 0600, 파일 권한이 곧 인증)만 열고 `init`/`join`을 기다린다. 멤버가 되면 모든 것을 띄운다. 인증서는 init/join의 마지막에 저장하므로 도중에 실패하면 미초기화로 남고, 남은 클러스터 저장소는 다음 init/join이 지운다. `leave`는 VM이 배정되어 있거나 유일한 투표 멤버면 거부하고, Node 리소스를 지운 뒤 멤버십에서 빠지고, 저장소와 인증서를 지우고 미초기화로 돌아간다.
- **`init`이 관리자 비밀번호를 한 번 출력한다.** M5의 `initial-admin-password` 파일은 없앤다. `init`은 CA 지문, 다음 노드의 조인 URL(1시간), 로그인 명령도 출력한다.
- **노드 생존은 리더가 Ping으로 본다.** 리더만 2초마다 다른 멤버에 Ping을 보내고, 10초 동안 응답이 없으면 `online: false`를 기록한다. 바뀔 때만 기록하므로 Raft에 주기적인 쓰기가 없다(설계 2.1의 "Raft에는 설정만"). 새 리더는 리더가 된 시점을 마지막 응답으로 쳐서 곧바로 다른 노드를 오프라인으로 만들지 않는다. `remove-node`는 오프라인 노드만 뺀다.
- **콘솔은 VM이 도는 노드로 중계한다.** API가 받은 노드에 VM이 없으면 그 노드의 노드 간 주소로 gRPC 양방향 스트림을 열어 WebSocket과 잇는다.
- **설정에 `address`가 필수다.** 다른 노드와 클라이언트가 이 노드에 닿는 호스트다. 노드 인증서 이름과, 클러스터에 알리는 주소(`address:listen 포트`, `address:cluster_listen 포트`)가 된다. 나중에 바꾸는 것은 지원하지 않는다. `listen`(기본 `0.0.0.0:8006`)의 루프백 제한은 푼다.

## 파일 구조

```
Cargo.toml                                   # 수정: opl-peer, tonic, rcgen, rustls 등, reqwest native-tls
README.md                                    # 수정: M6, 실행 방법(init/login/join), 테스트, 문서 링크
.github/workflows/ci.yml                     # 수정: Linux에 libssl-dev
docs/superpowers/specs/...-design.md         # 수정: 2.2, 3.2, 4, 8, 10, 12절
crates/opl-types/src/
├── resource.rs                              # 수정: Kind::Cluster, Kind::JoinToken
├── cluster.rs                               # 새로: ClusterSpec(CA), JoinTokenSpec
└── storage.rs                               # 수정: VolumeStatus.node
crates/opl-cluster/
├── src/types.rs                             # 수정: Rejection::InUse, LeaderRequest
├── src/exclusive.rs                         # 새로: VM의 볼륨·MAC 값 꺼내기
├── src/state_machine.rs                     # 수정: 중복 거부
├── src/leader.rs                            # 새로: 리더 쪽 처리, Forwarder, LeaderError
├── src/cluster.rs                           # 수정: request(재시도·전달), members, Unreachable
├── src/memnet.rs                            # 수정: MemForwarder
└── tests/{common/mod.rs,single_node.rs,three_nodes.rs}
crates/opl-peer/                             # 새 크레이트
├── build.rs                                 # gRPC 서비스 정의(수동)
├── src/lib.rs
├── src/pki.rs                               # CA, CSR, 서명, 지문, NodeTls(디스크)
├── src/tls.rs                               # HTTPS 리스너(axum)
├── src/codec.rs, src/wire.rs                # JSON/바이트 코덱, 메시지 타입
├── src/client.rs                            # Peers, GrpcNetwork(Raft), Forwarder, 콘솔
├── src/server.rs                            # PeerService, serve
├── src/monitor.rs                           # 리더의 노드 생존 감시
└── tests/grpc.rs                            # 실제 mTLS로 잇는 노드들
crates/opl-api/
├── src/cluster.rs                           # 새로: 클러스터 정보, CA, 조인 토큰, 합류, 노드 제거
├── src/console.rs                           # 수정: 다른 노드로 중계
├── src/{state,users,volumes,tasks,auth,error,lib}.rs
└── tests/{common/mod.rs,cluster.rs,two_nodes.rs,tasks.rs}
crates/oplctl/src/
├── cli.rs                                   # 수정: cluster 명령, --ca-fingerprint
├── client.rs                                # 수정: CA, fetch_ca, fingerprint, 제어 소켓, Task 제한 시간
├── console.rs                               # 수정: WSS(native-tls)
└── config.rs                                # 수정: ca_pem
crates/opld/
├── src/main.rs                              # 수정: 미초기화 ↔ 멤버 루프
├── src/config.rs                            # 수정: address, cluster_listen, control_socket
├── src/bootstrap.rs                         # 수정: create_cluster, register_node
├── src/member.rs                            # 새로: 멤버로 도는 노드(시작, 탈퇴, 닫기)
├── src/join.rs                              # 새로: 합류 요청
├── src/control.rs                           # 새로: 제어 소켓
└── tests/{common/mod.rs,smoke.rs,cluster.rs,e2e.rs}
```

---
### Task 1: 리소스 타입: 클러스터 정보, 조인 토큰, 볼륨을 만드는 노드

클러스터 CA를 담는 `Cluster`와 조인 토큰 `JoinToken` 리소스를 더한다. API로 직접 다루지 않는다(watch 대상도 아니다). 볼륨 status에는 파일을 만드는 노드를 기록해, 그 노드만 시작할 때 고아 볼륨을 정리하게 한다(Task 7).

**Files:**
- Create: `crates/opl-types/src/cluster.rs`
- Modify: `crates/opl-types/src/{lib,resource,storage}.rs`

- [ ] **Step 1: 테스트 먼저**

`crates/opl-types/src/resource.rs`의 `kind_serializes_as_its_str` 테스트에서 종류 목록 끝(`Kind::ApiToken,` 다음)에 두 줄을 더한다:

```rust
            Kind::Cluster,
            Kind::JoinToken,
```

`crates/opl-types/src/cluster.rs`를 만든다(테스트 포함 전체):

```rust
//! 클러스터 자체의 정보와 노드 합류 토큰. 클러스터를 만들고 노드를 더할 때만 쓴다.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::resource::{Kind, Resource, ResourceSpec};

/// 클러스터에 하나만 있다. 리소스 이름이 클러스터 이름이다.
pub type ClusterInfo = Resource<ClusterSpec, ClusterStatus>;
/// 노드 합류 토큰. 한 번 쓰면 지운다. 리소스 이름은 id 문자열이다.
pub type JoinToken = Resource<JoinTokenSpec, JoinTokenStatus>;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterSpec {
    /// 클러스터 CA 인증서(PEM)
    pub ca_cert_pem: String,
    /// CA 개인키(PEM). 어느 노드든 합류 요청에 서명할 수 있게 복제한다. API로 내보내지 않는다.
    pub ca_key_pem: String,
}

/// 로그에 개인키가 찍히지 않게 한다.
impl fmt::Debug for ClusterSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClusterSpec")
            .field("ca_cert_pem", &self.ca_cert_pem)
            .field("ca_key_pem", &"<redacted>")
            .finish()
    }
}

impl ResourceSpec for ClusterSpec {
    const KIND: Kind = Kind::Cluster;
    type Status = ClusterStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ClusterStatus {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinTokenSpec {
    /// 비밀 값의 SHA-256 (16진수)
    pub secret_hash: String,
    /// 만료 시각(유닉스 초)
    pub expires_at: u64,
}

impl ResourceSpec for JoinTokenSpec {
    const KIND: Kind = Kind::JoinToken;
    type Status = JoinTokenStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct JoinTokenStatus {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_ca_key() {
        let spec = ClusterSpec {
            ca_cert_pem: "CERT".into(),
            ca_key_pem: "SECRET-KEY".into(),
        };
        let shown = format!("{spec:?}");
        assert!(shown.contains("CERT"));
        assert!(!shown.contains("SECRET-KEY"));
    }
}
```

`crates/opl-types/src/lib.rs`에서 `pub mod auth;` 다음 줄에 `pub mod cluster;`를 더한다.

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p opl-types`
Expected: 컴파일 실패 — `no variant named Cluster`, `no variant named JoinToken`

- [ ] **Step 3: 종류 추가**

`crates/opl-types/src/resource.rs`의 `enum Kind`에 `ApiToken,` 다음으로 `Cluster,`와 `JoinToken,`을 더하고, `as_str`의 `Kind::ApiToken => "api-token",` 다음에 두 줄을 더한다:

```rust
            Kind::Cluster => "cluster",
            Kind::JoinToken => "join-token",
```

`crates/opl-types/src/storage.rs`의 `VolumeStatus`에 `attached_to` 다음 필드를 더한다(구조체에 `#[serde(default)]`가 있으므로 저장 형식 호환 규칙을 지킨다):

```rust
    /// 볼륨을 만든 Task를 실행하는 노드. 그 노드만 시작할 때 남은 고아 볼륨을 정리한다.
    pub node: Option<Uuid>,
```

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p opl-types && cargo build --workspace --all-targets`
Expected: PASS (`debug_hides_the_ca_key`, `kind_serializes_as_its_str` 포함). 다른 크레이트의 `VolumeStatus { .. }` 생성은 모두 `..` 나머지 문법을 쓰므로 그대로 빌드된다.

- [ ] **Step 5: 커밋**

```bash
git add crates/opl-types
git commit -m "feat(types): 클러스터 정보·조인 토큰 리소스와 볼륨을 만드는 노드"
```

---
### Task 2: 상태 머신: VM의 볼륨·MAC 중복 거부

여러 노드의 API가 각자 확인하고 쓰면 같은 볼륨을 두 VM에 붙이거나 MAC이 겹칠 수 있다. 상태 머신은 커밋 순서대로 적용하므로 여기서 거부하면 하나만 성공한다. VM spec의 `disks[].volume`과 `nics[].mac`(채워진 것만)을 같은 종류의 다른 리소스, 그리고 같은 spec 안에서 확인한다. 거부는 `Rejection::InUse`이고 API는 409로 옮긴다.

**Files:**
- Create: `crates/opl-cluster/src/exclusive.rs`
- Modify: `crates/opl-cluster/src/{types,state_machine,lib}.rs`, `crates/opl-cluster/tests/single_node.rs`, `crates/opl-api/src/error.rs`

- [ ] **Step 1: 실패하는 테스트**

`crates/opl-cluster/tests/single_node.rs` 끝에 더한다:

```rust
fn vm(name: &str, volumes: &[&str], macs: &[&str]) -> opl_types::vm::Vm {
    let spec = serde_json::json!({
        "cpu": {"sockets": 1, "cores": 1},
        "memory_mib": 256,
        "machine": "virt",
        "disks": volumes.iter().map(|v| serde_json::json!({"volume": v, "bus": "virtio-blk"})).collect::<Vec<_>>(),
        "nics": macs.iter().map(|m| serde_json::json!({"network": "user", "model": "virtio", "mac": m})).collect::<Vec<_>>(),
    });
    opl_types::resource::Resource::new(name, serde_json::from_value(spec).unwrap())
}

#[tokio::test]
async fn a_volume_or_mac_belongs_to_one_vm() {
    use opl_types::vm::VmSpec;
    let tc = TestCluster::start(1).await;
    let c = tc.get(node(1));
    let mac = "52:54:00:00:00:01";
    let a = vm("a", &["root-a"], &[mac]);
    c.create(&a).await.unwrap();

    let in_use = |e: ClusterError, field: &str| match e {
        ClusterError::Rejected(Rejection::InUse {
            kind: Kind::Vm,
            field: f,
            holder,
            ..
        }) => f == field && holder == a.id,
        _ => false,
    };
    let err = c.create(&vm("b", &["root-a"], &[])).await.unwrap_err();
    assert!(in_use(err, "volume"));
    let err = c.create(&vm("b", &["root-b"], &[mac])).await.unwrap_err();
    assert!(in_use(err, "mac"));

    // 다른 VM을 바꿔 겹치게 해도 거부한다. 자기 spec을 다시 쓰는 것은 된다.
    let b = vm("b", &["root-b"], &[]);
    let rev = c.create(&b).await.unwrap();
    let err = c
        .update_spec(
            b.id,
            Some(rev),
            BTreeMap::new(),
            &vm("b", &["root-a"], &[]).spec,
        )
        .await
        .unwrap_err();
    assert!(in_use(err, "volume"));
    c.update_spec::<VmSpec>(a.id, None, BTreeMap::new(), &a.spec)
        .await
        .unwrap();

    // 한 VM 안에서 같은 볼륨을 두 번 붙일 수도 없다.
    let c2 = vm("c", &["root-c", "root-c"], &[]);
    let err = c.create(&c2).await.unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::InUse { holder, .. }) if holder == c2.id
    ));
}
```

`crates/opl-api/src/error.rs`의 `mod tests`에서 `internal_error_hides_details` 앞에 더한다:

```rust
    #[test]
    fn values_used_by_another_vm_are_conflicts() {
        let e = ApiError::from(ClusterError::Rejected(Rejection::InUse {
            kind: opl_types::resource::Kind::Vm,
            field: "mac".into(),
            value: "52:54:00:00:00:01".into(),
            holder: uuid::Uuid::nil(),
        }));
        assert_eq!(e.status, StatusCode::CONFLICT);
        assert_eq!(e.code, "mac_in_use");
        assert!(e.message.contains("52:54:00:00:00:01"));
    }
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p opl-cluster --test single_node a_volume_or_mac_belongs_to_one_vm`
Expected: 컴파일 실패 — `no variant named InUse`

- [ ] **Step 3: 거부 사유와 값 꺼내기**

`crates/opl-cluster/src/types.rs`의 `enum Rejection`에서 `RevisionMismatch { expected: u64, actual: u64 },` 다음에 더한다:

```rust
    /// 클러스터 안에서 하나만 쓸 수 있는 값(VM에 붙인 볼륨, MAC 주소)이 이미 쓰이고 있다.
    #[error("{field} {value} is already used by {} {holder}", kind.as_str())]
    InUse {
        kind: Kind,
        field: String,
        value: String,
        holder: Uuid,
    },
```

`crates/opl-cluster/src/exclusive.rs`를 만든다:

```rust
//! 클러스터 안에서 리소스 하나만 쓸 수 있는 값. 상태 머신이 커밋 순서대로 확인하므로, 여러 노드의
//! API가 동시에 같은 값을 써도 하나만 성공한다.

use opl_types::resource::Kind;
use serde_json::Value;

/// spec에서 다른 리소스와 겹치면 안 되는 값을 `(필드, 값)`으로 꺼낸다. 지금은 VM이 붙인
/// 볼륨 이름과 NIC의 MAC 주소다.
pub(crate) fn exclusive_values(kind: Kind, spec: &Value) -> Vec<(&'static str, String)> {
    if kind != Kind::Vm {
        return Vec::new();
    }
    let field = |list: &str, key: &str, name: &'static str| {
        spec[list]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(move |item| item[key].as_str().map(|v| (name, v.to_string())))
            .collect::<Vec<_>>()
    };
    let mut values = field("disks", "volume", "volume");
    values.extend(field("nics", "mac", "mac"));
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_vm_volumes_and_macs() {
        let spec = json!({
            "disks": [{"volume": "root"}, {"volume": "data"}],
            "nics": [{"network": "user", "mac": "52:54:00:00:00:01"}, {"network": "user", "mac": null}]
        });
        assert_eq!(
            exclusive_values(Kind::Vm, &spec),
            vec![
                ("volume", "root".to_string()),
                ("volume", "data".to_string()),
                ("mac", "52:54:00:00:00:01".to_string()),
            ]
        );
        assert!(exclusive_values(Kind::Volume, &spec).is_empty());
    }
}
```

`crates/opl-cluster/src/lib.rs`의 `mod cluster;` 다음 줄에 `mod exclusive;`를 더한다.

- [ ] **Step 4: 상태 머신에서 확인**

`crates/opl-cluster/src/state_machine.rs`:

1. `use crate::store::{` 앞에 `use crate::exclusive::exclusive_values;`를 더한다.
2. `apply_command`의 `Command::Create` 갈래에서 이름 중복을 확인한 뒤, `names.insert(` 바로 앞에 더한다:

```rust
            if let Err(r) = check_exclusive(objects, kind, resource.id, &resource.spec)? {
                return Ok(Err(r));
            }
```

3. `Command::UpdateSpec` 갈래에서 `check(expected_revision, resource.revision)`를 확인한 뒤, `resource.labels = labels;` 바로 앞에 더한다:

```rust
            if let Err(r) = check_exclusive(objects, kind, id, &spec)? {
                return Ok(Err(r));
            }
```

4. `impl RaftSnapshotBuilder<TypeConfig> for StateMachine {` 바로 앞에 함수를 더한다:

```rust
/// `spec`의 독점 값이 같은 종류의 다른 리소스나 같은 spec 안에서 겹치면 거부한다.
fn check_exclusive(
    objects: &Table<(&'static str, u128), &'static [u8]>,
    kind: Kind,
    id: Uuid,
    spec: &serde_json::Value,
) -> Result<Result<(), Rejection>, StoreError> {
    let wanted = exclusive_values(kind, spec);
    if wanted.is_empty() {
        return Ok(Ok(()));
    }
    let in_use = |field: &str, value: &str, holder: Uuid| Rejection::InUse {
        kind,
        field: field.to_string(),
        value: value.to_string(),
        holder,
    };
    for (i, (field, value)) in wanted.iter().enumerate() {
        if wanted[..i].contains(&(field, value.clone())) {
            return Ok(Err(in_use(field, value, id)));
        }
    }
    for item in objects.range((kind.as_str(), 0u128)..=(kind.as_str(), u128::MAX))? {
        let (key, v) = item?;
        let other = Uuid::from_u128(key.value().1);
        if other == id {
            continue;
        }
        let other_spec: RawResource = serde_json::from_slice(v.value())?;
        for (field, value) in exclusive_values(kind, &other_spec.spec) {
            if wanted.iter().any(|(f, v)| *f == field && *v == value) {
                return Ok(Err(in_use(field, &value, other)));
            }
        }
    }
    Ok(Ok(()))
}
```

- [ ] **Step 5: API 오류로 옮기기**

`crates/opl-api/src/error.rs`의 `impl From<ClusterError> for ApiError`에서 `Rejection::RevisionMismatch { .. } => ApiError::new(...)` 갈래 다음에 더한다:

```rust
                    Rejection::InUse { field, .. } => ApiError::conflict(
                        if field == "mac" {
                            "mac_in_use"
                        } else {
                            "volume_in_use"
                        },
                        message,
                    ),
```

- [ ] **Step 6: 통과 확인**

Run: `cargo test -p opl-cluster && cargo test -p opl-api --lib`
Expected: PASS (`a_volume_or_mac_belongs_to_one_vm`, `extracts_vm_volumes_and_macs`, `values_used_by_another_vm_are_conflicts` 포함)

- [ ] **Step 7: 커밋**

```bash
git add crates/opl-cluster crates/opl-api/src/error.rs
git commit -m "feat(cluster): 상태 머신이 VM의 볼륨·MAC 중복을 거부"
```

---
### Task 3: 리더 요청과 전달

팔로워가 받은 쓰기와 멤버십 변경을 리더로 보낸다. `LeaderRequest`(쓰기, learner 추가, voter 승격, 제거)를 리더에서 처리하는 함수와, 그것을 리더에 보내는 `Forwarder` 트레이트를 둔다. `Cluster::request`는 리더면 처리하고, 아니면 전달한 뒤 그 인덱스까지 이 노드에도 적용되기를 기다린다(같은 노드에서 곧바로 읽으면 결과가 보인다). 리더가 없거나 닿지 않으면 `write_timeout`까지 다시 시도한다. 기존 쓰기 메서드(`create`, `update_spec` 등)는 모두 이것을 거친다. 실제 전달기(gRPC)는 Task 5에서, 테스트용 `MemForwarder`는 여기서 만든다.

**Files:**
- Create: `crates/opl-cluster/src/leader.rs`
- Modify: `crates/opl-cluster/Cargo.toml`, `crates/opl-cluster/src/{types,cluster,lib,memnet}.rs`, `crates/opl-cluster/tests/{common/mod.rs,three_nodes.rs}`, `crates/opl-api/src/error.rs`

- [ ] **Step 1: 하네스에 전달 켜기**

`crates/opl-cluster/tests/common/mod.rs`:

1. `TestCluster`의 `write_timeout` 필드 다음에 더한다:

```rust
    /// 팔로워가 받은 요청을 리더로 전달한다.
    pub forward: bool,
```

2. `start_with`를 다음으로 바꾼다(본문의 `let mut tc = TestCluster { ... };`까지가 `build`로 옮겨 간다. 그 아래 `for i in 1..=n {`부터는 그대로 `build`의 나머지다):

```rust
    pub async fn start_with(n: u64, raft: openraft::Config, write_timeout: Duration) -> Self {
        Self::build(n, raft, write_timeout, false).await
    }

    /// 팔로워가 요청을 리더로 전달하는 클러스터
    pub async fn start_forwarding(n: u64) -> Self {
        Self::build(n, raft_config(), Duration::from_secs(5), true).await
    }

    async fn build(n: u64, raft: openraft::Config, write_timeout: Duration, forward: bool) -> Self {
        let mut tc = TestCluster {
            router: MemRouter::default(),
            nodes: BTreeMap::new(),
            raft,
            write_timeout,
            forward,
            root: TempDir::new().unwrap(),
        };
```

3. `open`에서 `Cluster::open`을 다시 시도하는 `loop`가 끝난 바로 다음(`self.router.register(...)` 앞)에 더한다:

```rust
        let cluster = if self.forward {
            cluster.with_forwarder(self.router.forwarder(id))
        } else {
            cluster
        };
```

- [ ] **Step 2: 실패하는 테스트**

`crates/opl-cluster/tests/three_nodes.rs`의 맨 위 설명을 `//! 한 프로세스 안의 3노드 클러스터: 리더 장애, 네트워크 분할, 스냅샷으로 따라잡기, 리더로 전달.`로 바꾸고, `use opl_cluster::ClusterError;` 다음 줄에 `use opl_cluster::types::{LeaderRequest, Rejection};`를 더한다. 파일 끝에 더한다:

```rust
#[tokio::test]
async fn follower_forwards_writes_and_sees_them_at_once() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = *tc.nodes.keys().find(|id| **id != leader).unwrap();

    let v = volume("disk-a");
    let rev = tc.get(follower).create(&v).await.unwrap();
    // 전달한 쓰기는 이 노드에도 적용된 뒤에 돌아온다.
    let got = tc.get(follower).get::<VolumeSpec>(v.id).unwrap().unwrap();
    assert_eq!(got.revision, rev);
    // 리더의 거부도 그대로 돌아온다.
    let err = tc.get(follower).create(&v).await.unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::AlreadyExists { .. })
    ));
}

#[tokio::test]
async fn forwarded_writes_wait_for_a_new_leader() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = *tc.nodes.keys().find(|id| **id != leader).unwrap();
    tc.stop(leader).await;
    // 남은 두 노드가 새 리더를 뽑는 동안 쓰기는 기다렸다가 성공한다.
    let v = volume("disk-b");
    tc.get(follower).create(&v).await.unwrap();
    assert!(tc.get(follower).get::<VolumeSpec>(v.id).unwrap().is_some());
}

#[tokio::test]
async fn membership_changes_go_through_any_member() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = *tc.nodes.keys().find(|id| **id != leader).unwrap();
    let rev = tc.get(leader).create(&volume("before")).await.unwrap();

    let n4 = node(4);
    tc.open(n4).await;
    tc.get(follower)
        .request(LeaderRequest::AddLearner {
            id: n4,
            addr: addr(4).addr,
        })
        .await
        .unwrap();
    assert!(tc.get(follower).members().learners.contains_key(&n4));
    tc.get(follower)
        .request(LeaderRequest::AddVoter { id: n4 })
        .await
        .unwrap();
    tc.wait_applied(n4, rev).await;
    let members = tc.get(follower).members();
    assert_eq!(members.voters.len(), 4);
    assert!(members.voters.contains_key(&n4));
    // 바쁘면 그 사이 리더가 바뀔 수 있다.
    assert!(members.leader.is_some());
    // 이미 투표 멤버면 그대로 성공한다.
    tc.get(n4)
        .request(LeaderRequest::AddVoter { id: n4 })
        .await
        .unwrap();

    tc.get(n4)
        .request(LeaderRequest::Remove { id: n4 })
        .await
        .unwrap();
    let members = tc.get(leader).members();
    assert!(!members.contains(n4));
    assert_eq!(members.voters.len(), 3);
}
```

- [ ] **Step 3: 실패 확인**

Run: `cargo test -p opl-cluster --test three_nodes`
Expected: 컴파일 실패 — `LeaderRequest`, `MemRouter::forwarder`, `Cluster::with_forwarder`, `Cluster::request`, `Cluster::members`가 없다

- [ ] **Step 4: 요청 타입과 리더 쪽 처리**

`crates/opl-cluster/Cargo.toml`의 `[dependencies]` 맨 위에 `async-trait.workspace = true`를 더한다.

`crates/opl-cluster/src/types.rs`에서 `/// 명령 적용 결과.` 주석 바로 앞에 더한다:

```rust
/// 리더가 처리하는 요청. 팔로워는 받은 쓰기와 멤버십 변경을 이것으로 리더에 전달한다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum LeaderRequest {
    Write {
        command: Command,
    },
    /// 노드를 learner로 더한다. 따라잡기를 기다리지 않는다.
    AddLearner {
        id: NodeId,
        addr: String,
    },
    /// learner가 로그를 따라잡으면 투표 멤버로 올린다.
    AddVoter {
        id: NodeId,
    },
    /// 노드를 멤버십에서 뺀다(투표 멤버이든 learner이든).
    Remove {
        id: NodeId,
    },
}
```

`crates/opl-cluster/src/leader.rs`를 만든다:

```rust
//! 리더가 처리하는 요청(쓰기, 멤버십 변경)과, 팔로워가 그것을 리더에 전달하는 통로.

use std::collections::BTreeSet;
use std::time::Duration;

use async_trait::async_trait;
use openraft::{BasicNode, ChangeMembers, Raft};
use serde::{Deserialize, Serialize};

use crate::cluster::ClusterError;
use crate::types::{LeaderRequest, NodeId, Rejection, Response, TypeConfig};

/// learner가 로그를 따라잡기를 기다리는 최대 시간. 스냅샷을 받아야 하면 오래 걸린다.
pub const CATCH_UP_TIMEOUT: Duration = Duration::from_secs(60);

/// 팔로워가 받은 요청을 리더에게 보낸다. 노드 간 네트워크가 구현한다.
#[async_trait]
pub trait Forwarder: Send + Sync + 'static {
    /// `addr`(리더의 Raft 주소)의 리더에게 요청을 보내고 리더의 결과를 돌려준다. 리더가 그 사이
    /// 바뀌었으면 `NotLeader`, 리더에 닿지 못했으면 `Unreachable`이다.
    async fn forward(
        &self,
        leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError>;
}

/// 노드 사이로 오가는 리더의 오류
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "kebab-case")]
pub enum LeaderError {
    Rejected {
        rejection: Rejection,
    },
    NotLeader {
        leader_id: Option<NodeId>,
        leader_addr: Option<String>,
    },
    Timeout,
    Other {
        message: String,
    },
}

impl From<ClusterError> for LeaderError {
    fn from(e: ClusterError) -> Self {
        match e {
            ClusterError::Rejected(rejection) => LeaderError::Rejected { rejection },
            ClusterError::NotLeader {
                leader_id,
                leader_addr,
            } => LeaderError::NotLeader {
                leader_id,
                leader_addr,
            },
            ClusterError::Timeout => LeaderError::Timeout,
            other => LeaderError::Other {
                message: other.to_string(),
            },
        }
    }
}

impl From<LeaderError> for ClusterError {
    fn from(e: LeaderError) -> Self {
        match e {
            LeaderError::Rejected { rejection } => ClusterError::Rejected(rejection),
            LeaderError::NotLeader {
                leader_id,
                leader_addr,
            } => ClusterError::NotLeader {
                leader_id,
                leader_addr,
            },
            LeaderError::Timeout => ClusterError::Timeout,
            LeaderError::Other { message } => ClusterError::Raft(message),
        }
    }
}

/// 이 노드가 리더라고 보고 요청을 처리한다. 리더가 아니면 `NotLeader`다. 성공하면 커밋된 로그 인덱스.
pub(crate) async fn execute(
    raft: &Raft<TypeConfig>,
    req: LeaderRequest,
    timeout: Duration,
) -> Result<u64, ClusterError> {
    match req {
        LeaderRequest::Write { command } => {
            let resp = tokio::time::timeout(timeout, raft.client_write(command))
                .await
                .map_err(|_| ClusterError::Timeout)??;
            let Response(result) = resp.data;
            Ok(result?)
        }
        LeaderRequest::AddLearner { id, addr } => {
            let resp =
                tokio::time::timeout(timeout, raft.add_learner(id, BasicNode::new(addr), false))
                    .await
                    .map_err(|_| ClusterError::Timeout)??;
            Ok(resp.log_id.index)
        }
        LeaderRequest::AddVoter { id } => {
            let m = raft.metrics().borrow().clone();
            let membership = m.membership_config.membership();
            if membership.voter_ids().any(|v| v == id) {
                return Ok(m.last_applied.map_or(0, |l| l.index));
            }
            // learner가 지금 리더의 마지막 로그까지 받으면 올린다.
            let target = m.last_log_index;
            raft.wait(Some(CATCH_UP_TIMEOUT))
                .metrics(
                    |m| {
                        m.state != openraft::ServerState::Leader
                            || m.replication.as_ref().is_some_and(|r| {
                                r.get(&id).is_some_and(|l| l.map(|l| l.index) >= target)
                            })
                    },
                    "learner caught up",
                )
                .await
                .map_err(|_| ClusterError::Timeout)?;
            let change = ChangeMembers::AddVoterIds(BTreeSet::from([id]));
            let resp = tokio::time::timeout(timeout, raft.change_membership(change, false))
                .await
                .map_err(|_| ClusterError::Timeout)??;
            Ok(resp.log_id.index)
        }
        LeaderRequest::Remove { id } => {
            let m = raft.metrics().borrow().clone();
            let membership = m.membership_config.membership();
            let change = if membership.voter_ids().any(|v| v == id) {
                ChangeMembers::RemoveVoters(BTreeSet::from([id]))
            } else if membership.learner_ids().any(|v| v == id) {
                ChangeMembers::RemoveNodes(BTreeSet::from([id]))
            } else {
                return Ok(m.last_applied.map_or(0, |l| l.index));
            };
            let resp = tokio::time::timeout(timeout, raft.change_membership(change, false))
                .await
                .map_err(|_| ClusterError::Timeout)??;
            Ok(resp.log_id.index)
        }
    }
}
```

- [ ] **Step 5: `Cluster`가 전달하게 하기**

`crates/opl-cluster/src/cluster.rs` 전체:

```rust
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use openraft::error::{CheckIsLeaderError, ClientWriteError, RaftError};
use openraft::{BasicNode, Raft, RaftNetworkFactory};
use opl_types::resource::{Resource, ResourceSpec};
use redb::Database;
use uuid::Uuid;

use crate::leader::{self, Forwarder};
use crate::store::{Store, StoreError, get_object, get_object_by_name, list_objects};
use crate::types::{Command, LeaderRequest, NodeId, RawResource, Rejection, TypeConfig};
use crate::watch::{Watch, WatchError, WatchHub};

/// spec 타입 `S`의 리소스.
pub type Typed<S> = Resource<S, <S as ResourceSpec>::Status>;

/// 리더가 없거나 닿지 않을 때 다시 시도하는 간격
const RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// 저장 파일 이름. `data_dir` 아래에 둔다.
pub const DB_FILE: &str = "cluster.redb";

#[derive(Debug, Clone)]
pub struct ClusterConfig {
    pub node_id: NodeId,
    pub data_dir: PathBuf,
    pub raft: openraft::Config,
    /// 쓰기가 커밋되기를 기다리는 최대 시간. 과반수가 없으면 이 시간 뒤 `Timeout`이 된다.
    pub write_timeout: Duration,
}

impl ClusterConfig {
    pub fn new(node_id: NodeId, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            node_id,
            data_dir: data_dir.into(),
            raft: openraft::Config::default(),
            write_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClusterError {
    /// 상태 머신이 명령을 거부했다(이름 중복, revision 불일치 등).
    #[error(transparent)]
    Rejected(#[from] Rejection),
    /// 이 노드는 리더가 아니다. 알려진 리더가 있으면 함께 돌려준다.
    #[error("not the leader (leader: {leader_id:?})")]
    NotLeader {
        leader_id: Option<NodeId>,
        leader_addr: Option<String>,
    },
    /// `write_timeout` 안에 커밋되지 않았다. 과반수가 없을 때 생긴다.
    /// 명령은 나중에 커밋될 수도 있다.
    #[error("write was not committed within the timeout")]
    Timeout,
    /// 리더에게 요청을 전달하지 못했다.
    #[error("cannot reach the leader: {0}")]
    Unreachable(String),
    #[error("raft: {0}")]
    Raft(String),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("resource encoding: {0}")]
    Encoding(#[from] serde_json::Error),
}

impl From<RaftError<NodeId, ClientWriteError<NodeId, BasicNode>>> for ClusterError {
    fn from(e: RaftError<NodeId, ClientWriteError<NodeId, BasicNode>>) -> Self {
        match e {
            RaftError::APIError(ClientWriteError::ForwardToLeader(f)) => ClusterError::NotLeader {
                leader_id: f.leader_id,
                leader_addr: f.leader_node.map(|n| n.addr),
            },
            other => ClusterError::Raft(other.to_string()),
        }
    }
}

/// 노드 하나의 클러스터 저장소. 쓰기는 Raft로 복제하고, 읽기는 로컬 상태 머신에서 한다.
pub struct Cluster {
    id: NodeId,
    raft: Raft<TypeConfig>,
    db: Arc<Database>,
    watch: Arc<WatchHub>,
    write_timeout: Duration,
    forwarder: Option<Arc<dyn Forwarder>>,
}

/// 지금 알고 있는 멤버십. 값은 노드의 Raft 주소다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Members {
    pub leader: Option<NodeId>,
    pub voters: BTreeMap<NodeId, String>,
    pub learners: BTreeMap<NodeId, String>,
}

impl Members {
    pub fn contains(&self, id: NodeId) -> bool {
        self.voters.contains_key(&id) || self.learners.contains_key(&id)
    }

    pub fn addr(&self, id: NodeId) -> Option<&str> {
        self.voters
            .get(&id)
            .or_else(|| self.learners.get(&id))
            .map(String::as_str)
    }
}

impl Cluster {
    /// 저장소를 열고 Raft를 시작한다. 처음 만든 클러스터는 `initialize`나
    /// 다른 노드의 `add_learner`로 멤버십을 받기 전까지 아무것도 하지 않는다.
    pub async fn open<N>(config: ClusterConfig, network: N) -> Result<Self, ClusterError>
    where
        N: RaftNetworkFactory<TypeConfig>,
    {
        std::fs::create_dir_all(&config.data_dir)?;
        let store = Store::open(&config.data_dir.join(DB_FILE))?;
        let raft_config = Arc::new(
            config
                .raft
                .validate()
                .map_err(|e| ClusterError::Raft(e.to_string()))?,
        );
        let raft = Raft::new(
            config.node_id,
            raft_config,
            network,
            store.log_store(),
            store.state_machine(),
        )
        .await
        .map_err(|e| ClusterError::Raft(e.to_string()))?;
        Ok(Self {
            id: config.node_id,
            raft,
            db: store.db().clone(),
            watch: store.watch_hub().clone(),
            write_timeout: config.write_timeout,
            forwarder: None,
        })
    }

    /// 팔로워가 받은 쓰기와 멤버십 변경을 이것으로 리더에 전달한다. 없으면 `NotLeader`를 돌려준다.
    pub fn with_forwarder(mut self, forwarder: Arc<dyn Forwarder>) -> Self {
        self.forwarder = Some(forwarder);
        self
    }

    pub fn id(&self) -> NodeId {
        self.id
    }

    /// 네트워크 계층이 받은 RPC를 넘기고, 테스트가 메트릭을 볼 때 쓴다.
    pub fn raft(&self) -> &Raft<TypeConfig> {
        &self.raft
    }

    /// 새 클러스터를 만든다. 첫 노드에서 한 번만 부른다.
    pub async fn initialize(
        &self,
        members: BTreeMap<NodeId, BasicNode>,
    ) -> Result<(), ClusterError> {
        self.raft
            .initialize(members)
            .await
            .map_err(|e| ClusterError::Raft(e.to_string()))
    }

    /// 노드를 learner로 추가하고 로그를 따라잡을 때까지 기다린다. 리더에서 부른다.
    /// `write_timeout` 안에 끝나지 않으면 `Timeout`이다. 이때도 learner 추가는 나중에
    /// 커밋될 수 있고, 커밋되면 리더는 계속 따라잡게 한다.
    pub async fn add_learner(&self, id: NodeId, node: BasicNode) -> Result<(), ClusterError> {
        tokio::time::timeout(self.write_timeout, self.raft.add_learner(id, node, true))
            .await
            .map_err(|_| ClusterError::Timeout)??;
        Ok(())
    }

    /// 투표 멤버를 `voters`로 바꾼다. 빠진 노드는 learner로 남는다. 리더에서 부른다.
    /// `write_timeout` 안에 커밋되지 않으면 `Timeout`이다. 변경은 나중에 커밋될 수도 있다.
    pub async fn change_membership(&self, voters: BTreeSet<NodeId>) -> Result<(), ClusterError> {
        tokio::time::timeout(
            self.write_timeout,
            self.raft.change_membership(voters, true),
        )
        .await
        .map_err(|_| ClusterError::Timeout)??;
        Ok(())
    }

    pub async fn create<S: ResourceSpec>(&self, resource: &Typed<S>) -> Result<u64, ClusterError> {
        self.write(Command::Create {
            kind: S::KIND,
            resource: to_raw(resource)?,
        })
        .await
    }

    pub async fn update_spec<S: ResourceSpec>(
        &self,
        id: Uuid,
        expected_revision: Option<u64>,
        labels: BTreeMap<String, String>,
        spec: &S,
    ) -> Result<u64, ClusterError> {
        self.write(Command::UpdateSpec {
            kind: S::KIND,
            id,
            expected_revision,
            labels,
            spec: serde_json::to_value(spec)?,
        })
        .await
    }

    pub async fn update_status<S: ResourceSpec>(
        &self,
        id: Uuid,
        status: &S::Status,
    ) -> Result<u64, ClusterError> {
        self.write(Command::UpdateStatus {
            kind: S::KIND,
            id,
            status: serde_json::to_value(status)?,
        })
        .await
    }

    pub async fn delete<S: ResourceSpec>(
        &self,
        id: Uuid,
        expected_revision: Option<u64>,
    ) -> Result<u64, ClusterError> {
        self.write(Command::Delete {
            kind: S::KIND,
            id,
            expected_revision,
        })
        .await
    }

    /// 명령을 리더에 제안하고 커밋·적용될 때까지 기다린다. 성공하면 새 revision을 돌려준다.
    /// 상태 머신은 spec/status JSON이 `kind`와 맞는지 보지 않으므로 타입 있는 메서드로만 부른다.
    pub(crate) async fn write(&self, command: Command) -> Result<u64, ClusterError> {
        self.request(LeaderRequest::Write { command }).await
    }

    /// 요청을 리더에서 처리한다. 이 노드가 리더가 아니면 전달자로 리더에 보내고, 리더가 커밋한
    /// 인덱스까지 이 노드에도 적용되기를 기다린다(이 노드에서 곧바로 읽어도 결과가 보인다).
    /// 리더가 없거나(선거 중) 닿지 않으면 `write_timeout`까지 다시 시도하고, 그래도 안 되면
    /// `Timeout`이다. 전달이 실패해 다시 보낸 요청이 이미 적용된 것이었다면 두 번째는 거부될 수 있다.
    pub async fn request(&self, req: LeaderRequest) -> Result<u64, ClusterError> {
        let deadline = tokio::time::Instant::now() + self.write_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let result = match leader::execute(&self.raft, req.clone(), remaining).await {
                Err(ClusterError::NotLeader {
                    leader_id: Some(leader),
                    leader_addr: Some(addr),
                }) if leader != self.id
                    && let Some(forwarder) = &self.forwarder =>
                {
                    match tokio::time::timeout(
                        remaining,
                        forwarder.forward(leader, &addr, req.clone()),
                    )
                    .await
                    {
                        Err(_) => Err(ClusterError::Timeout),
                        Ok(Ok(index)) => {
                            let _ = self
                                .raft
                                .wait(Some(
                                    deadline.saturating_duration_since(tokio::time::Instant::now()),
                                ))
                                .applied_index_at_least(Some(index), "forwarded request")
                                .await;
                            Ok(index)
                        }
                        Ok(Err(e)) => Err(e),
                    }
                }
                other => other,
            };
            let retry = match &result {
                Err(ClusterError::NotLeader {
                    leader_id: None, ..
                })
                | Err(ClusterError::Unreachable(_)) => true,
                Err(ClusterError::NotLeader { .. }) => self.forwarder.is_some(),
                _ => false,
            };
            if !retry {
                return result;
            }
            if tokio::time::Instant::now() + RETRY_INTERVAL >= deadline {
                return Err(ClusterError::Timeout);
            }
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    }

    /// 이 노드가 리더로서 요청을 처리한다. 노드 간 네트워크가 다른 노드에서 전달받은 요청을 넘긴다.
    /// 리더가 아니면 `NotLeader`다(다시 전달하지 않는다).
    pub async fn execute_as_leader(&self, req: LeaderRequest) -> Result<u64, ClusterError> {
        leader::execute(&self.raft, req, self.write_timeout).await
    }

    /// 이 노드가 아는 멤버십과 리더
    pub fn members(&self) -> Members {
        let m = self.raft.metrics().borrow().clone();
        let membership = m.membership_config.membership();
        let voters: BTreeSet<NodeId> = membership.voter_ids().collect();
        let mut out = Members {
            leader: m.current_leader,
            voters: BTreeMap::new(),
            learners: BTreeMap::new(),
        };
        for (id, node) in membership.nodes() {
            let list = if voters.contains(id) {
                &mut out.voters
            } else {
                &mut out.learners
            };
            list.insert(*id, node.addr.clone());
        }
        out
    }

    /// 로컬 상태 머신에서 읽는다. 팔로워에서는 최신 커밋이 아직 반영되지 않았을 수 있다.
    /// 최신 값이 필요하면 먼저 `read_barrier`를 부른다.
    pub fn get<S: ResourceSpec>(&self, id: Uuid) -> Result<Option<Typed<S>>, ClusterError> {
        get_object(&self.db, S::KIND, id)?.map(from_raw).transpose()
    }

    pub fn get_by_name<S: ResourceSpec>(
        &self,
        name: &str,
    ) -> Result<Option<Typed<S>>, ClusterError> {
        get_object_by_name(&self.db, S::KIND, name)?
            .map(from_raw)
            .transpose()
    }

    /// 한 종류의 리소스 전체와 그 목록의 revision. `watch(revision)`으로 이어서 구독한다.
    pub fn list<S: ResourceSpec>(&self) -> Result<(u64, Vec<Typed<S>>), ClusterError> {
        let (revision, raw) = list_objects(&self.db, S::KIND)?;
        let items = raw.into_iter().map(from_raw).collect::<Result<_, _>>()?;
        Ok((revision, items))
    }

    /// 이 노드가 아직 리더인지 과반수에게 확인하고, 그 시점의 커밋까지 로컬에 적용될 때까지
    /// 기다린다(선형화 읽기). 리더에서만 쓸 수 있다. 팔로워와 learner에서는 리더 정보를 담은
    /// `NotLeader`를 돌려준다.
    pub async fn read_barrier(&self) -> Result<(), ClusterError> {
        match self.raft.ensure_linearizable().await {
            Ok(_) => Ok(()),
            Err(RaftError::APIError(CheckIsLeaderError::ForwardToLeader(f))) => {
                Err(ClusterError::NotLeader {
                    leader_id: f.leader_id,
                    leader_addr: f.leader_node.map(|n| n.addr),
                })
            }
            Err(e) => Err(ClusterError::Raft(e.to_string())),
        }
    }

    /// `since` 이후에 커밋된 변경을 구독한다.
    pub fn watch(&self, since: u64) -> Result<Watch, WatchError> {
        self.watch.subscribe(since)
    }

    pub async fn shutdown(&self) -> Result<(), ClusterError> {
        self.raft
            .shutdown()
            .await
            .map_err(|e| ClusterError::Raft(e.to_string()))
    }
}

fn to_raw<S: ResourceSpec>(r: &Typed<S>) -> Result<RawResource, serde_json::Error> {
    Ok(Resource {
        id: r.id,
        name: r.name.clone(),
        revision: r.revision,
        labels: r.labels.clone(),
        spec: serde_json::to_value(&r.spec)?,
        status: serde_json::to_value(&r.status)?,
    })
}

fn from_raw<S: ResourceSpec>(r: RawResource) -> Result<Typed<S>, ClusterError> {
    Ok(Resource {
        id: r.id,
        name: r.name,
        revision: r.revision,
        labels: r.labels,
        spec: serde_json::from_value(r.spec)?,
        status: serde_json::from_value(r.status)?,
    })
}
```

`crates/opl-cluster/src/lib.rs` 전체:

```rust
//! Raft로 복제되는 opLite 리소스 저장소.

mod cluster;
mod exclusive;
mod leader;
mod log_store;
pub mod memnet;
mod state_machine;
pub mod store;
pub mod types;
pub mod watch;

pub use cluster::{Cluster, ClusterConfig, ClusterError, DB_FILE, Members, Typed};
pub use leader::{CATCH_UP_TIMEOUT, Forwarder, LeaderError};
pub use log_store::LogStore;
pub use state_machine::StateMachine;
```

`crates/opl-cluster/src/memnet.rs` 전체(`MemForwarder` 추가):

```rust
//! 한 프로세스 안의 노드들을 잇는 Raft 네트워크와 리더 전달자. 클러스터 테스트에 쓴다.
//! 노드 간 실제 통신(gRPC + mTLS)은 `opl-peer`에 있다.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use openraft::error::{InstallSnapshotError, RPCError, RaftError, RemoteError, Unreachable};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::{BasicNode, Raft};

use crate::cluster::ClusterError;
use crate::leader::{self, Forwarder};
use crate::types::{LeaderRequest, NodeId, TypeConfig};

/// 전달받은 요청을 리더가 처리하는 최대 시간
const FORWARD_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Routes {
    nodes: HashMap<NodeId, Raft<TypeConfig>>,
    isolated: HashSet<NodeId>,
}

/// 노드 등록부. 등록되지 않았거나 격리된 노드로 가는 RPC는 `Unreachable`이 된다.
#[derive(Clone, Default)]
pub struct MemRouter {
    routes: Arc<Mutex<Routes>>,
}

impl MemRouter {
    /// `from` 노드가 쓸 네트워크. `Cluster::open`에 넘긴다.
    pub fn network(&self, from: NodeId) -> MemNetwork {
        MemNetwork {
            from,
            router: self.clone(),
        }
    }

    /// `from` 노드가 쓸 리더 전달자. `Cluster::with_forwarder`에 넘긴다.
    pub fn forwarder(&self, from: NodeId) -> Arc<MemForwarder> {
        Arc::new(MemForwarder {
            from,
            router: self.clone(),
        })
    }

    pub fn register(&self, id: NodeId, raft: Raft<TypeConfig>) {
        self.routes.lock().unwrap().nodes.insert(id, raft);
    }

    pub fn unregister(&self, id: NodeId) {
        self.routes.lock().unwrap().nodes.remove(&id);
    }

    /// 노드를 네트워크에서 끊는다. 그 노드로 가거나 그 노드에서 나가는 RPC가 모두 실패한다.
    pub fn isolate(&self, id: NodeId) {
        self.routes.lock().unwrap().isolated.insert(id);
    }

    pub fn heal(&self, id: NodeId) {
        self.routes.lock().unwrap().isolated.remove(&id);
    }

    fn route(&self, from: NodeId, to: NodeId) -> Result<Raft<TypeConfig>, Unreachable> {
        let routes = self.routes.lock().unwrap();
        if routes.isolated.contains(&from) || routes.isolated.contains(&to) {
            return Err(unreachable("isolated"));
        }
        routes
            .nodes
            .get(&to)
            .cloned()
            .ok_or_else(|| unreachable("not registered"))
    }
}

fn unreachable(why: &str) -> Unreachable {
    Unreachable::new(&std::io::Error::other(why.to_string()))
}

pub struct MemForwarder {
    from: NodeId,
    router: MemRouter,
}

#[async_trait]
impl Forwarder for MemForwarder {
    async fn forward(
        &self,
        leader: NodeId,
        _addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        let raft = self
            .router
            .route(self.from, leader)
            .map_err(|e| ClusterError::Unreachable(e.to_string()))?;
        leader::execute(&raft, req, FORWARD_TIMEOUT).await
    }
}

pub struct MemNetwork {
    from: NodeId,
    router: MemRouter,
}

impl RaftNetworkFactory<TypeConfig> for MemNetwork {
    type Network = MemConnection;

    async fn new_client(&mut self, target: NodeId, _node: &BasicNode) -> MemConnection {
        MemConnection {
            from: self.from,
            target,
            router: self.router.clone(),
        }
    }
}

pub struct MemConnection {
    from: NodeId,
    target: NodeId,
    router: MemRouter,
}

type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, BasicNode, RaftError<NodeId, E>>>;

impl RaftNetwork<TypeConfig> for MemConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        let raft = self.router.route(self.from, self.target)?;
        raft.append_entries(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<NodeId>, InstallSnapshotError> {
        let raft = self.router.route(self.from, self.target)?;
        raft.install_snapshot(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> RpcResult<VoteResponse<NodeId>> {
        let raft = self.router.route(self.from, self.target)?;
        raft.vote(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }
}
```

`crates/opl-api/src/error.rs`에서 `ClusterError::Timeout => ApiError::new(`를 `ClusterError::Timeout | ClusterError::Unreachable(_) => ApiError::new(`로 바꾼다(리더에 닿지 못하는 것도 과반수 없음과 같이 503 `cluster_no_quorum`).

- [ ] **Step 6: 통과 확인**

Run: `cargo test -p opl-cluster && cargo test -p opl-api --lib`
Expected: PASS. 새 테스트 `follower_forwards_writes_and_sees_them_at_once`, `forwarded_writes_wait_for_a_new_leader`, `membership_changes_go_through_any_member`와 기존 `follower_rejects_writes_with_leader_hint`(전달기가 없으면 예전처럼 `NotLeader`) 모두 통과한다.

- [ ] **Step 7: 커밋**

```bash
git add crates/opl-cluster crates/opl-api/src/error.rs
git commit -m "feat(cluster): 리더 요청을 리더로 전달하고 선거 중에는 다시 시도"
```

---
### Task 4: `opl-peer` 크레이트: 클러스터 PKI와 HTTPS 리스너

새 크레이트 `opl-peer`를 만든다. 이 Task는 PKI와 외부 API용 TLS 리스너만 넣는다(gRPC는 Task 5).

- **CA:** ECDSA P-256, 20년.
- **노드 인증서:** CN이 노드 id이고, 용도는 serverAuth + clientAuth이며, 유효 기간은 824일이다. macOS는 825일을 넘는 TLS 서버 인증서를 거부한다.
- **서명:** 노드의 CSR에서 공개키만 쓰고 나머지는 서명하는 쪽이 정한다.
- **디스크:** 노드 TLS 자료는 `{data_dir}/pki`에 원자적으로 쓰고, 키는 0600이다.
- **HTTPS 리스너:** 핸드셰이크를 연결마다 따로 돌려, 느린 클라이언트가 다른 연결을 막지 못하게 한다.

**Files:**
- Create: `crates/opl-peer/Cargo.toml`, `crates/opl-peer/src/{lib,pki,tls}.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: 워크스페이스 의존성**

루트 `Cargo.toml`의 `[workspace.dependencies]`에서 `opl-cluster = { path = "crates/opl-cluster" }` 다음 줄에 `opl-peer = { path = "crates/opl-peer" }`를 더하고, 끝에 더한다(gRPC 의존성도 여기서 한 번에 더한다. ring만 쓰고 aws-lc는 쓰지 않는다):

```toml
tonic = { version = "0.14.6", default-features = false, features = ["transport", "codegen", "router", "tls-ring"] }
tonic-build = { version = "0.14.6", default-features = false }
tokio-stream = { version = "0.1.19", features = ["net"] }
bytes = "1.12.1"
rcgen = { version = "0.14.10", features = ["x509-parser"] }
x509-parser = "0.18.1"
time = "0.3"
rustls = { version = "0.23.46", default-features = false, features = ["ring", "std", "logging"] }
tokio-rustls = { version = "0.26.6", default-features = false, features = ["ring", "logging"] }
rustls-pki-types = "1.15.1"
```

`crates/opl-peer/Cargo.toml`(이 Task의 내용. Task 5에서 늘린다):

```toml
[package]
name = "opl-peer"
description = "Node-to-node communication (gRPC over mTLS) and cluster PKI for opLite"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
axum.workspace = true
hex.workspace = true
rcgen.workspace = true
rustls.workspace = true
rustls-pki-types.workspace = true
sha2.workspace = true
thiserror.workspace = true
time.workspace = true
tokio.workspace = true
tokio-rustls.workspace = true
tracing.workspace = true
uuid.workspace = true
x509-parser.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

`crates/opl-peer/src/lib.rs`(Step 3과 Task 5에서 늘린다):

```rust
//! 노드 간 통신과 클러스터 PKI.

pub mod pki;
```

- [ ] **Step 2: PKI (테스트 포함)**

`crates/opl-peer/src/pki.rs`를 만든다. 테스트가 확인하는 것:

- CA 권한을 요청한 CSR이어도 서명한 인증서는 CA가 아니고, 요청한 이름도 들어가지 않는다.
- 유효 기간이 825일 이하다.
- 지문 형식이 맞다.
- 디스크에 저장했다가 다시 읽을 수 있고, 키 파일은 0600이며, 이미 있는 자료는 덮어쓰지 않는다.
- `Debug` 출력에 키가 보이지 않는다.

```rust
//! 클러스터 CA와 노드 인증서.
//!
//! 노드는 자기 키를 직접 만들고 CSR만 보낸다. CA를 가진 노드는 CSR에서 공개키만 쓰고, 이름과 용도와
//! 기간은 스스로 정해 서명한다(CSR이 CA 권한을 요청해도 따르지 않는다). 노드 인증서의 CN은 노드 id다.

use std::fmt;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// CA 인증서의 유효 기간(일)
const CA_DAYS: i64 = 365 * 20;
/// 노드 인증서의 유효 기간(일). macOS는 유효 기간이 825일을 넘는 TLS 서버 인증서를 거부한다
/// (시작 하루 전부터 유효하므로 하루를 뺀다). 만료 전에 갱신해야 한다.
const NODE_DAYS: i64 = 824;

/// 데이터 디렉터리 아래 노드 TLS 자료를 두는 디렉터리
pub const PKI_DIR: &str = "pki";
const CA_FILE: &str = "ca.crt";
const CERT_FILE: &str = "node.crt";
const KEY_FILE: &str = "node.key";

#[derive(Debug, thiserror::Error)]
pub enum PkiError {
    #[error("certificate: {0}")]
    Cert(#[from] rcgen::Error),
    #[error("invalid PEM: {0}")]
    Pem(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// 클러스터 CA
pub struct ClusterCa {
    pub cert_pem: String,
    pub key_pem: String,
}

impl fmt::Debug for ClusterCa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClusterCa").finish_non_exhaustive()
    }
}

impl ClusterCa {
    /// 새 CA를 만든다(ECDSA P-256).
    pub fn generate(cluster_name: &str) -> Result<Self, PkiError> {
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.distinguished_name.push(
            DnType::CommonName,
            format!("opLite cluster {cluster_name} CA"),
        );
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        set_validity(&mut params, CA_DAYS);
        let cert = params.self_signed(&key)?;
        Ok(Self {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
        })
    }

    /// 노드의 CSR에 서명한다. `sans`는 노드에 닿는 호스트 이름과 IP다.
    pub fn sign_node(
        &self,
        csr_pem: &str,
        node: Uuid,
        sans: &[String],
    ) -> Result<String, PkiError> {
        let csr = CertificateSigningRequestParams::from_pem(csr_pem)?;
        let mut params = CertificateParams::new(sans.to_vec())?;
        params
            .distinguished_name
            .push(DnType::CommonName, node.to_string());
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        set_validity(&mut params, NODE_DAYS);
        let issuer = Issuer::from_ca_cert_pem(&self.cert_pem, KeyPair::from_pem(&self.key_pem)?)?;
        let cert = CertificateSigningRequestParams {
            params,
            public_key: csr.public_key,
        }
        .signed_by(&issuer)?;
        Ok(cert.pem())
    }
}

/// 하루 전부터 `days`일 뒤까지 유효하다. 노드 시계가 조금 어긋나도 바로 쓸 수 있다.
fn set_validity(params: &mut CertificateParams, days: i64) {
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(days);
}

/// 노드 키와 CSR(PEM)을 만든다. 이름은 서명하는 쪽이 정하므로 CSR에는 넣지 않는다.
pub fn node_key_and_csr() -> Result<(String, String), PkiError> {
    let key = KeyPair::generate()?;
    let csr = CertificateParams::default().serialize_request(&key)?;
    Ok((key.serialize_pem(), csr.pem()?))
}

/// PEM의 첫 인증서
pub fn first_cert(pem: &str) -> Result<CertificateDer<'static>, PkiError> {
    CertificateDer::from_pem_slice(pem.as_bytes()).map_err(|e| PkiError::Pem(e.to_string()))
}

/// 인증서(PEM)의 SHA-256 지문. `sha256:<16진수>` 형식이다. 노드가 합류할 때 CA를 확인하는 데 쓴다.
pub fn fingerprint(cert_pem: &str) -> Result<String, PkiError> {
    let der = first_cert(cert_pem)?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(&der))))
}

/// 노드 인증서(DER)의 CN에 담긴 노드 id
pub fn node_id_of(cert_der: &[u8]) -> Option<Uuid> {
    let (_, cert) = x509_parser::parse_x509_certificate(cert_der).ok()?;
    let cn = cert.subject().iter_common_name().next()?;
    Uuid::try_parse(cn.as_str().ok()?).ok()
}

/// 노드가 디스크에 두는 TLS 자료: 클러스터 CA 인증서, 자기 인증서와 키
#[derive(Clone)]
pub struct NodeTls {
    pub ca_pem: String,
    pub cert_pem: String,
    pub key_pem: String,
}

impl fmt::Debug for NodeTls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeTls").finish_non_exhaustive()
    }
}

impl NodeTls {
    /// `dir`이 없으면 `None`이다(아직 클러스터에 속하지 않은 노드).
    pub fn load(dir: &Path) -> Result<Option<Self>, PkiError> {
        if !dir.exists() {
            return Ok(None);
        }
        let read = |name: &str| std::fs::read_to_string(dir.join(name));
        Ok(Some(Self {
            ca_pem: read(CA_FILE)?,
            cert_pem: read(CERT_FILE)?,
            key_pem: read(KEY_FILE)?,
        }))
    }

    /// `dir`에 쓴다. 옆의 임시 디렉터리에 다 쓴 뒤 이름을 바꾸므로, 도중에 죽으면 `dir`은 생기지
    /// 않는다. 키는 주인만 읽을 수 있다(0600).
    pub fn save(&self, dir: &Path) -> Result<(), PkiError> {
        let mut tmp = dir.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        if tmp.exists() {
            std::fs::remove_dir_all(&tmp)?;
        }
        std::fs::create_dir_all(&tmp)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700))?;
        for (name, content, mode) in [
            (CA_FILE, &self.ca_pem, 0o644),
            (CERT_FILE, &self.cert_pem, 0o644),
            (KEY_FILE, &self.key_pem, 0o600),
        ] {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(tmp.join(name))?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, dir)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_tls(ca: &ClusterCa, node: Uuid) -> NodeTls {
        let (key_pem, csr) = node_key_and_csr().unwrap();
        NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, node, &["127.0.0.1".into()]).unwrap(),
            key_pem,
        }
    }

    #[test]
    fn signed_node_cert_carries_node_id_and_is_not_a_ca() {
        let ca = ClusterCa::generate("prod").unwrap();
        let node = Uuid::now_v7();
        // CA 권한을 요청하는 CSR이어도 서명한 인증서는 CA가 아니다.
        let key = KeyPair::generate().unwrap();
        let mut greedy = CertificateParams::new(vec!["evil.example".to_string()]).unwrap();
        greedy.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let csr = greedy.serialize_request(&key).unwrap().pem().unwrap();
        let pem = ca.sign_node(&csr, node, &["10.0.0.5".into()]).unwrap();

        let der = first_cert(&pem).unwrap();
        assert_eq!(node_id_of(&der), Some(node));
        let (_, cert) = x509_parser::parse_x509_certificate(&der).unwrap();
        assert!(!cert.is_ca());
        let validity = cert.validity();
        let days = (validity.not_after.timestamp() - validity.not_before.timestamp()) / 86400;
        assert!(days <= 825, "{days} days");
        let sans = cert.subject_alternative_name().unwrap().unwrap();
        let shown = format!("{:?}", sans.value.general_names);
        assert!(
            shown.contains("10.0.0.5") || shown.contains("[10, 0, 0, 5]"),
            "{shown}"
        );
        assert!(!shown.contains("evil"), "{shown}");
    }

    #[test]
    fn fingerprint_is_sha256_of_the_der() {
        let ca = ClusterCa::generate("prod").unwrap();
        let fp = fingerprint(&ca.cert_pem).unwrap();
        assert!(fp.starts_with("sha256:"));
        assert_eq!(fp.len(), "sha256:".len() + 64);
        assert_eq!(fingerprint(&ca.cert_pem).unwrap(), fp);
        assert_ne!(
            fingerprint(&ClusterCa::generate("prod").unwrap().cert_pem).unwrap(),
            fp
        );
        assert!(fingerprint("not a pem").is_err());
    }

    #[test]
    fn node_tls_round_trips_with_a_private_key() {
        let dir = tempfile::tempdir().unwrap();
        let pki = dir.path().join(PKI_DIR);
        assert!(NodeTls::load(&pki).unwrap().is_none());
        let ca = ClusterCa::generate("prod").unwrap();
        let tls = node_tls(&ca, Uuid::now_v7());
        tls.save(&pki).unwrap();
        let back = NodeTls::load(&pki).unwrap().unwrap();
        assert_eq!(back.cert_pem, tls.cert_pem);
        assert_eq!(back.key_pem, tls.key_pem);
        let mode = std::fs::metadata(pki.join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // 이미 있으면 덮어쓰지 않는다.
        assert!(tls.save(&pki).is_err());
    }

    #[test]
    fn debug_does_not_show_keys() {
        let ca = ClusterCa::generate("prod").unwrap();
        assert!(!format!("{ca:?}").contains("PRIVATE"));
        let tls = node_tls(&ca, Uuid::now_v7());
        assert!(!format!("{tls:?}").contains("PRIVATE"));
    }
}
```

Run: `cargo test -p opl-peer pki`
Expected: PASS (4개)

- [ ] **Step 3: HTTPS 리스너 (테스트 포함)**

`crates/opl-peer/src/lib.rs`에 `pub mod tls;`를 더하고(최종 모습은 아래), `crates/opl-peer/src/tls.rs`를 만든다. 테스트는 axum 라우터를 이 리스너로 띄우고 핸드셰이크를 끝내지 않는 연결 하나를 열어 둔 채, CA만 믿는 rustls 클라이언트로 `GET /`이 200인지 본다:

```rust
//! 노드 간 통신과 클러스터 PKI.

pub mod pki;
pub mod tls;
```

```rust
//! 외부 API의 HTTPS. 노드 인증서로 TLS를 열고 axum에 넘긴다.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rustls::ServerConfig;
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::pem::PemObject;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

use crate::pki::{NodeTls, PkiError, first_cert};

/// TLS 핸드셰이크를 기다리는 최대 시간. 느린 클라이언트가 연결을 붙잡고 있지 못하게 한다.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 외부 API용 서버 설정. 클라이언트 인증서는 요구하지 않는다(사용자 인증은 API가 한다).
pub fn api_server_config(tls: &NodeTls) -> Result<ServerConfig, PkiError> {
    let key = PrivateKeyDer::from_pem_slice(tls.key_pem.as_bytes())
        .map_err(|e| PkiError::Pem(e.to_string()))?;
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| PkiError::Pem(e.to_string()))?
            .with_no_client_auth()
            .with_single_cert(vec![first_cert(&tls.cert_pem)?], key)
            .map_err(|e| PkiError::Pem(e.to_string()))?;
    // WebSocket을 HTTP/1.1로 받으므로 h2는 알리지 않는다.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

/// TLS로 감싼 TCP 리스너. `axum::serve`에 넘긴다. 핸드셰이크는 연결마다 따로 돌아 느린 클라이언트가
/// 다른 연결을 막지 않는다.
pub struct TlsListener {
    rx: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
    accept: JoinHandle<()>,
}

impl TlsListener {
    pub fn new(tcp: TcpListener, config: ServerConfig) -> io::Result<Self> {
        let local = tcp.local_addr()?;
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let (tx, rx) = mpsc::channel(64);
        let accept = tokio::spawn(async move {
            loop {
                let (stream, addr) = match tcp.accept().await {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot accept a connection");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let acceptor = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(tls)) => {
                            let _ = tx.send((tls, addr)).await;
                        }
                        Ok(Err(e)) => tracing::debug!(%addr, error = %e, "TLS handshake failed"),
                        Err(_) => tracing::debug!(%addr, "TLS handshake timed out"),
                    }
                });
            }
        });
        Ok(Self { rx, local, accept })
    }
}

impl Drop for TlsListener {
    fn drop(&mut self) {
        // 포트를 놓는다.
        self.accept.abort();
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(conn) => conn,
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pki::{ClusterCa, node_key_and_csr};
    use rustls_pki_types::ServerName;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn serves_axum_over_tls_with_the_node_certificate() {
        let ca = ClusterCa::generate("test").unwrap();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca
                .sign_node(&csr, uuid::Uuid::now_v7(), &["127.0.0.1".into()])
                .unwrap(),
            key_pem,
        };
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let listener = TlsListener::new(tcp, api_server_config(&tls).unwrap()).unwrap();
        let app = axum::Router::new().route("/", axum::routing::get(|| async { "hello" }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // 핸드셰이크를 끝내지 않는 연결이 있어도 다른 연결은 받는다.
        let _stalled = TcpStream::connect(addr).await.unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(first_cert(&ca.cert_pem).unwrap()).unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let tcp = TcpStream::connect(addr).await.unwrap();
        let name = ServerName::try_from("127.0.0.1").unwrap();
        let mut stream = connector.connect(name, tcp).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("hello"), "{response}");
    }
}
```

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p opl-peer && cargo deny check licenses bans sources`
Expected: PASS (5개), `licenses ok`

- [ ] **Step 5: 커밋**

```bash
git add Cargo.toml Cargo.lock crates/opl-peer
git commit -m "feat(peer): 클러스터 PKI와 HTTPS 리스너"
```

---
### Task 5: 노드 간 gRPC: Raft 네트워크, 리더 전달, 콘솔 중계

`tonic-build`의 수동 서비스 정의로 gRPC 서비스 `opl.peer.v1.Peer`를 만든다. `.proto` 파일이 없고, 메시지는 serde JSON이다. 메서드는 다음과 같다.

- `AppendEntries`, `InstallSnapshot`, `Vote`: openraft 타입을 그대로 보낸다.
- `Leader`: `LeaderRequest`를 받아 `Result<u64, LeaderError>`를 돌려준다.
- `Ping`: 상대 노드의 id를 돌려준다.
- `Console`: 양방향 바이트 스트림이다. VM id는 메타데이터 `opl-vm`에 담는다.

클라이언트와 서버는 다음과 같이 나눈다.

- **`Peers`:** 주소마다 채널을 하나 만들어 다시 쓴다. 이 노드의 인증서를 내밀고, 상대는 클러스터 CA로 확인한다. `Forwarder`를 구현한다.
- **`GrpcNetwork`:** openraft의 `RaftNetworkFactory`다. 연결 실패는 `Unreachable`로 돌려준다.
- **`PeerService`:** 상대 인증서의 CN(노드 id)이 지금 멤버인지 확인한다. 아직 멤버십이 없으면 CA 서명만 확인한다.

**Files:**
- Create: `crates/opl-peer/build.rs`, `crates/opl-peer/src/{codec,wire,client,server}.rs`, `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opl-peer/Cargo.toml`, `crates/opl-peer/src/lib.rs`

- [ ] **Step 1: 의존성과 서비스 정의**

`crates/opl-peer/Cargo.toml` 전체:

```toml
[package]
name = "opl-peer"
description = "Node-to-node communication (gRPC over mTLS) and cluster PKI for opLite"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
async-trait.workspace = true
axum.workspace = true
bytes.workspace = true
futures-util.workspace = true
hex.workspace = true
openraft.workspace = true
opl-cluster.workspace = true
opl-driver.workspace = true
opl-types.workspace = true
rcgen.workspace = true
rustls.workspace = true
rustls-pki-types.workspace = true
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true
thiserror.workspace = true
time.workspace = true
tokio.workspace = true
tokio-rustls.workspace = true
tokio-stream.workspace = true
tonic.workspace = true
tracing.workspace = true
uuid.workspace = true
x509-parser.workspace = true

[build-dependencies]
tonic-build.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

`crates/opl-peer/build.rs`:

```rust
//! 노드 간 gRPC 서비스 정의. `.proto` 대신 Rust 타입을 serde JSON으로 주고받는다(`protoc`가 필요 없다).
//! 콘솔 스트림만 바이트를 그대로 보낸다.

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
```

- [ ] **Step 2: 실패하는 테스트**

`crates/opl-peer/tests/grpc.rs`. 노드마다 `127.0.0.1`의 빈 포트에 실제 mTLS 서버를 띄우고, 다음을 확인한다.

- 팔로워를 통한 쓰기와, 리더가 죽은 뒤의 쓰기
- 다른 CA의 인증서 거부, 같은 CA여도 멤버가 아니면 `PermissionDenied`
- learner로 합류한 뒤 스스로 voter 승격을 요청하기
- 다른 노드의 VM 콘솔 중계와, 그 노드에서 돌지 않는 VM의 거부

테스트가 동시에 많이 돌면 선거가 몇 초씩 걸리므로 기다리는 시간을 30초로 둔다. 리더 생존 감시 테스트는 Task 6에서 더한다.

```rust
//! 실제 gRPC + mTLS로 잇는 노드들(한 프로세스 안, 127.0.0.1의 빈 포트).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openraft::{BasicNode, ServerState};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{Hypervisor, MockHypervisor};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{GrpcNetwork, PeerService, Peers};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::storage::{Volume, VolumeSpec};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;

/// 테스트가 동시에 많이 돌아 CPU가 바쁘면 선거가 몇 초씩 걸린다(openraft는 리더 임기 동안 다른
/// 노드에 투표하지 않으므로 리더를 잃으면 보통 선거 제한 시간의 두 배쯤 걸린다).
const WAIT: Duration = Duration::from_secs(30);

fn tls_for(ca: &ClusterCa, id: NodeId) -> NodeTls {
    let (key_pem, csr) = node_key_and_csr().unwrap();
    NodeTls {
        ca_pem: ca.cert_pem.clone(),
        cert_pem: ca.sign_node(&csr, id.0, &["127.0.0.1".into()]).unwrap(),
        key_pem,
    }
}

fn volume(name: &str) -> Volume {
    Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    )
}

struct Node {
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    stop: Option<oneshot::Sender<()>>,
}

impl Node {
    async fn start(ca: &ClusterCa, root: &TempDir) -> Node {
        let id = NodeId(Uuid::now_v7());
        let tls = tls_for(ca, id);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let peers = Arc::new(Peers::new(&tls));
        let mut config = ClusterConfig::new(id, root.path().join(id.to_string()));
        config.write_timeout = WAIT;
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        let cluster = Arc::new(
            Cluster::open(config, GrpcNetwork::new(peers.clone()))
                .await
                .unwrap()
                .with_forwarder(peers.clone()),
        );
        let hv = Arc::new(MockHypervisor::default());
        let service = PeerService::new(cluster.clone(), Some(hv.clone() as Arc<dyn Hypervisor>));
        let (stop, rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, async {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });
        Node {
            addr,
            cluster,
            peers,
            hv,
            stop: Some(stop),
        }
    }

    fn id(&self) -> NodeId {
        self.cluster.id()
    }

    async fn stop(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.cluster.shutdown().await.unwrap();
    }
}

/// 노드 `n`개를 띄우고 첫 노드에서 클러스터를 초기화한 뒤 모두가 같은 리더를 알 때까지 기다린다.
async fn start(n: usize) -> (ClusterCa, TempDir, Vec<Node>) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut nodes = Vec::new();
    for _ in 0..n {
        nodes.push(Node::start(&ca, &root).await);
    }
    let members: BTreeMap<_, _> = nodes
        .iter()
        .map(|n| (n.id(), BasicNode::new(&n.addr)))
        .collect();
    nodes[0].cluster.initialize(members).await.unwrap();
    wait_leader(&nodes).await;
    (ca, root, nodes)
}

async fn wait_leader(nodes: &[Node]) -> NodeId {
    let deadline = Instant::now() + WAIT;
    loop {
        let leaders: Vec<_> = nodes
            .iter()
            .map(|n| n.cluster.raft().metrics().borrow().current_leader)
            .collect();
        if let Some(Some(leader)) = leaders.first()
            && leaders.iter().all(|l| *l == Some(*leader))
            && let Some(node) = nodes.iter().find(|n| n.id() == *leader)
            && node.cluster.raft().metrics().borrow().state == ServerState::Leader
        {
            return *leader;
        }
        assert!(Instant::now() < deadline, "no leader");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn nodes_replicate_and_forward_over_mtls() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();

    let v = volume("disk-a");
    nodes[follower].cluster.create(&v).await.unwrap();
    assert!(
        nodes[follower]
            .cluster
            .get::<VolumeSpec>(v.id)
            .unwrap()
            .is_some()
    );

    // 리더가 죽어도 남은 두 노드로 쓰기가 이어진다.
    let leader = nodes.iter().position(|n| n.id() == leader).unwrap();
    nodes[leader].stop().await;
    let rest: Vec<usize> = (0..3).filter(|i| *i != leader).collect();
    let v = volume("disk-b");
    nodes[rest[0]].cluster.create(&v).await.unwrap();
    assert!(
        nodes[rest[0]]
            .cluster
            .get::<VolumeSpec>(v.id)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn strangers_and_non_members_are_refused() {
    let (ca, root, nodes) = start(1).await;
    // 다른 CA의 인증서로는 TLS 연결이 맺어지지 않는다.
    let other = ClusterCa::generate("other").unwrap();
    let stranger = Peers::new(&tls_for(&other, NodeId(Uuid::now_v7())));
    assert!(stranger.ping(&nodes[0].addr, WAIT).await.is_err());
    // 같은 CA라도 멤버가 아니면 거부한다(제거된 노드).
    let outsider = Node::start(&ca, &root).await;
    let err = outsider.peers.ping(&nodes[0].addr, WAIT).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err}");
    let err = outsider
        .peers
        .leader_request(&nodes[0].addr, LeaderRequest::Remove { id: nodes[0].id() })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a member"), "{err}");
    // 멤버는 된다.
    assert_eq!(
        nodes[0].peers.ping(&nodes[0].addr, WAIT).await.unwrap(),
        nodes[0].id()
    );
}

#[tokio::test]
async fn a_new_node_joins_as_learner_then_voter() {
    let (ca, root, mut nodes) = start(1).await;
    let rev = nodes[0].cluster.create(&volume("before")).await.unwrap();

    let new = Node::start(&ca, &root).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: new.id(),
            addr: new.addr.clone(),
        })
        .await
        .unwrap();
    new.cluster
        .raft()
        .wait(Some(WAIT))
        .applied_index_at_least(Some(rev), "learner caught up")
        .await
        .unwrap();
    // 새 노드가 스스로 승격을 요청한다(리더로 전달된다).
    new.cluster
        .request(LeaderRequest::AddVoter { id: new.id() })
        .await
        .unwrap();
    let members = new.cluster.members();
    assert_eq!(members.voters.len(), 2);
    assert!(members.voters.contains_key(&new.id()));
    assert!(
        new.cluster
            .get_by_name::<VolumeSpec>("before")
            .unwrap()
            .is_some()
    );
    nodes.push(new);
    // 새 노드에서 한 쓰기도 커밋된다(이제 과반수는 2).
    nodes[1].cluster.create(&volume("after")).await.unwrap();
}

#[tokio::test]
async fn console_is_relayed_to_the_node_running_the_vm() {
    let (_ca, _root, nodes) = start(2).await;
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let target = &nodes[1];
    target.hv.set_console_dir(tmp.path());
    let vm = Uuid::now_v7();
    let spec: VmRuntimeSpec = serde_json::from_value(serde_json::json!({
        "id": vm, "name": "web", "cpu": {"sockets": 1, "cores": 1}, "memory_mib": 256,
        "firmware": "uefi", "machine": "virt", "disks": [], "nics": []
    }))
    .unwrap();
    target.hv.start(&spec).await.unwrap();
    let listener = tokio::net::UnixListener::bind(tmp.path().join(format!("{vm}.vnc"))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });

    let mut console = nodes[0].peers.console(&target.addr, vm).await.unwrap();
    assert_eq!(&console.recv().await.unwrap()[..], b"RFB 003.008\n");
    assert!(console.send(bytes::Bytes::from_static(b"ping")).await);
    assert_eq!(&console.recv().await.unwrap()[..], b"ping");

    // 그 노드에서 돌지 않는 VM은 거부한다.
    let err = nodes[0]
        .peers
        .console(&nodes[0].addr, vm)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, opl_peer::ConsoleError::NotRunning), "{err}");
}
```

Run: `cargo test -p opl-peer --test grpc`
Expected: 컴파일 실패 — `GrpcNetwork`, `Peers`, `PeerService`, `serve`가 없다

- [ ] **Step 3: 코덱과 메시지 타입**

`crates/opl-peer/src/codec.rs`:

```rust
//! gRPC 메시지 형식. 보통 메시지는 JSON이고, 콘솔 스트림은 바이트 그대로다.

use std::marker::PhantomData;

use bytes::{Buf, BufMut, Bytes};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tonic::Status;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};

pub struct JsonCodec<T, U>(PhantomData<(T, U)>);

impl<T, U> Default for JsonCodec<T, U> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T, U> Codec for JsonCodec<T, U>
where
    T: Serialize + Send + 'static,
    U: DeserializeOwned + Send + 'static,
{
    type Encode = T;
    type Decode = U;
    type Encoder = JsonEncoder<T>;
    type Decoder = JsonDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        JsonEncoder(PhantomData)
    }

    fn decoder(&mut self) -> Self::Decoder {
        JsonDecoder(PhantomData)
    }
}

pub struct JsonEncoder<T>(PhantomData<T>);

impl<T: Serialize> Encoder for JsonEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: T, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        serde_json::to_writer(dst.writer(), &item).map_err(|e| Status::internal(e.to_string()))
    }
}

pub struct JsonDecoder<U>(PhantomData<U>);

impl<U: DeserializeOwned> Decoder for JsonDecoder<U> {
    type Item = U;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<U>, Status> {
        serde_json::from_reader(src.reader())
            .map(Some)
            .map_err(|e| Status::invalid_argument(e.to_string()))
    }
}

#[derive(Default)]
pub struct RawCodec;

impl Codec for RawCodec {
    type Encode = Bytes;
    type Decode = Bytes;
    type Encoder = RawCodec;
    type Decoder = RawCodec;

    fn encoder(&mut self) -> Self::Encoder {
        RawCodec
    }

    fn decoder(&mut self) -> Self::Decoder {
        RawCodec
    }
}

impl Encoder for RawCodec {
    type Item = Bytes;
    type Error = Status;

    fn encode(&mut self, item: Bytes, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        dst.put(item);
        Ok(())
    }
}

impl Decoder for RawCodec {
    type Item = Bytes;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Bytes>, Status> {
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}
```

`crates/opl-peer/src/wire.rs`:

```rust
//! gRPC로 주고받는 타입. Raft 오류도 그대로 보내 상대 노드의 openraft가 해석하게 한다.

use openraft::error::{InstallSnapshotError, RaftError};
use opl_cluster::LeaderError;
use opl_cluster::types::{NodeId, TypeConfig};

pub type AppendEntriesRequest = openraft::raft::AppendEntriesRequest<TypeConfig>;
pub type AppendEntriesReply =
    Result<openraft::raft::AppendEntriesResponse<NodeId>, RaftError<NodeId>>;
pub type InstallSnapshotRequest = openraft::raft::InstallSnapshotRequest<TypeConfig>;
pub type InstallSnapshotReply = Result<
    openraft::raft::InstallSnapshotResponse<NodeId>,
    RaftError<NodeId, InstallSnapshotError>,
>;
pub type VoteRequest = openraft::raft::VoteRequest<NodeId>;
pub type VoteReply = Result<openraft::raft::VoteResponse<NodeId>, RaftError<NodeId>>;
pub type LeaderReply = Result<u64, LeaderError>;
/// 내용 없는 메시지(JSON `null`)
pub type Empty = ();
```

- [ ] **Step 4: 클라이언트**

`crates/opl-peer/src/client.rs`:

```rust
//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 콘솔.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use openraft::BasicNode;
use openraft::error::{
    InstallSnapshotError, NetworkError, RPCError, RaftError, RemoteError, Unreachable,
};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use opl_cluster::types::{LeaderRequest, NodeId, TypeConfig};
use opl_cluster::{ClusterError, Forwarder};
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Code, Request, Status, Streaming};
use uuid::Uuid;

use crate::pki::NodeTls;
use crate::proto::peer_client::PeerClient;
use crate::{MAX_MESSAGE_BYTES, VM_METADATA};

/// 연결을 맺기까지 기다리는 최대 시간
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// 다른 노드로 가는 연결. 주소마다 채널 하나를 만들어 다시 쓴다(끊기면 tonic이 다시 잇는다).
/// 연결할 때 이 노드의 인증서를 내밀고, 상대 인증서는 클러스터 CA로 확인한다.
pub struct Peers {
    tls: ClientTlsConfig,
    channels: Mutex<HashMap<String, Channel>>,
}

impl Peers {
    pub fn new(tls: &NodeTls) -> Self {
        Self {
            tls: ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(&tls.ca_pem))
                .identity(Identity::from_pem(&tls.cert_pem, &tls.key_pem)),
            channels: Mutex::new(HashMap::new()),
        }
    }

    fn client(&self, addr: &str) -> Result<PeerClient<Channel>, Status> {
        let mut channels = self.channels.lock().unwrap();
        let channel = match channels.get(addr) {
            Some(channel) => channel.clone(),
            None => {
                let channel = Endpoint::from_shared(format!("https://{addr}"))
                    .and_then(|e| e.tls_config(self.tls.clone()))
                    .map_err(|e| Status::invalid_argument(format!("bad address {addr}: {e}")))?
                    .connect_timeout(CONNECT_TIMEOUT)
                    .connect_lazy();
                channels.insert(addr.to_string(), channel.clone());
                channel
            }
        };
        Ok(PeerClient::new(channel)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES))
    }

    /// `addr`의 노드가 응답하면 그 노드의 id를 돌려준다.
    pub async fn ping(&self, addr: &str, timeout: Duration) -> Result<NodeId, Status> {
        let mut req = Request::new(());
        req.set_timeout(timeout);
        Ok(self.client(addr)?.ping(req).await?.into_inner())
    }

    /// `addr`의 노드에 리더 요청을 보낸다. 그 노드가 리더가 아니면 `NotLeader`다.
    pub async fn leader_request(
        &self,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        let reply = self
            .client(addr)
            .map_err(|s| ClusterError::Raft(s.to_string()))?
            .leader(req)
            .await
            .map_err(|s| match s.code() {
                Code::Unavailable => ClusterError::Unreachable(s.message().to_string()),
                _ => ClusterError::Raft(s.to_string()),
            })?;
        reply.into_inner().map_err(ClusterError::from)
    }

    /// `addr`의 노드에서 도는 VM의 콘솔(VNC 바이트 스트림)을 연다.
    pub async fn console(&self, addr: &str, vm: Uuid) -> Result<ConsoleStream, ConsoleError> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let mut req = Request::new(ReceiverStream::new(rx));
        let value = MetadataValue::try_from(vm.to_string())
            .map_err(|e| ConsoleError::Unavailable(e.to_string()))?;
        req.metadata_mut().insert(VM_METADATA, value);
        let unavailable = |s: Status| match s.code() {
            Code::FailedPrecondition => ConsoleError::NotRunning,
            _ => ConsoleError::Unavailable(s.message().to_string()),
        };
        let inbound = self
            .client(addr)
            .map_err(unavailable)?
            .console(req)
            .await
            .map_err(unavailable)?
            .into_inner();
        Ok(ConsoleStream { tx, inbound })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConsoleError {
    #[error("the VM is not running on that node")]
    NotRunning,
    #[error("cannot open the console: {0}")]
    Unavailable(String),
}

/// 다른 노드의 VM 콘솔 연결
pub struct ConsoleStream {
    tx: tokio::sync::mpsc::Sender<Bytes>,
    inbound: Streaming<Bytes>,
}

impl ConsoleStream {
    /// VNC로 보낸다. 연결이 끝났으면 false다.
    pub async fn send(&self, data: Bytes) -> bool {
        self.tx.send(data).await.is_ok()
    }

    /// VNC가 보낸 다음 바이트. 연결이 끝났으면 `None`이다.
    pub async fn recv(&mut self) -> Option<Bytes> {
        self.inbound.message().await.ok().flatten()
    }
}

#[async_trait]
impl Forwarder for Peers {
    async fn forward(
        &self,
        _leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        self.leader_request(addr, req).await
    }
}

/// gRPC로 가는 Raft 네트워크. `Cluster::open`에 넘긴다. `BasicNode.addr`는 노드의 클러스터 주소다.
#[derive(Clone)]
pub struct GrpcNetwork {
    peers: Arc<Peers>,
}

impl GrpcNetwork {
    pub fn new(peers: Arc<Peers>) -> Self {
        Self { peers }
    }
}

impl RaftNetworkFactory<TypeConfig> for GrpcNetwork {
    type Network = GrpcConnection;

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> GrpcConnection {
        GrpcConnection {
            target,
            addr: node.addr.clone(),
            peers: self.peers.clone(),
        }
    }
}

pub struct GrpcConnection {
    target: NodeId,
    addr: String,
    peers: Arc<Peers>,
}

type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, BasicNode, RaftError<NodeId, E>>>;

/// 연결하지 못한 것은 `Unreachable`로 돌려 openraft가 잠시 쉬었다가 다시 시도하게 한다.
fn rpc_error<E: std::error::Error>(s: Status) -> RPCError<NodeId, BasicNode, RaftError<NodeId, E>> {
    if s.code() == Code::Unavailable {
        RPCError::Unreachable(Unreachable::new(&s))
    } else {
        RPCError::Network(NetworkError::new(&s))
    }
}

impl GrpcConnection {
    fn request<T>(&self, message: T, option: &RPCOption) -> Request<T> {
        let mut req = Request::new(message);
        req.set_timeout(option.hard_ttl());
        req
    }
}

impl RaftNetwork<TypeConfig> for GrpcConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        let mut client = self.peers.client(&self.addr).map_err(rpc_error)?;
        let reply = client
            .append_entries(self.request(rpc, &option))
            .await
            .map_err(rpc_error)?
            .into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<NodeId>, InstallSnapshotError> {
        let mut client = self.peers.client(&self.addr).map_err(rpc_error)?;
        let reply = client
            .install_snapshot(self.request(rpc, &option))
            .await
            .map_err(rpc_error)?
            .into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> RpcResult<VoteResponse<NodeId>> {
        let mut client = self.peers.client(&self.addr).map_err(rpc_error)?;
        let reply = client
            .vote(self.request(rpc, &option))
            .await
            .map_err(rpc_error)?
            .into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }
}
```

- [ ] **Step 5: 서버**

`crates/opl-peer/src/server.rs`:

```rust
//! 다른 노드의 요청을 받는 gRPC 서버.

use std::future::Future;
use std::sync::Arc;

use bytes::Bytes;
use opl_cluster::Cluster;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_driver::{DriverError, Hypervisor};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixStream};
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use crate::pki::{NodeTls, node_id_of};
use crate::proto::peer_server::{Peer, PeerServer};
use crate::wire::{
    AppendEntriesReply, AppendEntriesRequest, InstallSnapshotReply, InstallSnapshotRequest,
    LeaderReply, VoteReply, VoteRequest,
};
use crate::{MAX_MESSAGE_BYTES, VM_METADATA};

/// 다른 노드가 부르는 서비스. 상대 인증서의 노드 id가 이 노드가 아는 멤버여야 한다. 아직 멤버십을
/// 받지 못한 노드(막 합류하는 중)는 클러스터 CA가 서명한 인증서면 받는다.
pub struct PeerService {
    cluster: Arc<Cluster>,
    hv: Option<Arc<dyn Hypervisor>>,
}

impl PeerService {
    /// `hv`는 콘솔 중계에 쓴다. 없으면 콘솔 요청을 거부한다.
    pub fn new(cluster: Arc<Cluster>, hv: Option<Arc<dyn Hypervisor>>) -> Self {
        Self { cluster, hv }
    }

    fn authorize<T>(&self, req: &Request<T>) -> Result<(), Status> {
        let certs = req
            .peer_certs()
            .ok_or_else(|| Status::unauthenticated("client certificate required"))?;
        let id = certs
            .first()
            .and_then(|c| node_id_of(c))
            .ok_or_else(|| Status::unauthenticated("certificate does not name a node"))?;
        let members = self.cluster.members();
        let uninitialized = members.voters.is_empty() && members.learners.is_empty();
        if uninitialized || members.contains(NodeId(id)) {
            Ok(())
        } else {
            Err(Status::permission_denied(format!(
                "node {id} is not a member of this cluster"
            )))
        }
    }
}

#[tonic::async_trait]
impl Peer for PeerService {
    async fn append_entries(
        &self,
        req: Request<AppendEntriesRequest>,
    ) -> Result<Response<AppendEntriesReply>, Status> {
        self.authorize(&req)?;
        let reply = self.cluster.raft().append_entries(req.into_inner()).await;
        Ok(Response::new(reply))
    }

    async fn install_snapshot(
        &self,
        req: Request<InstallSnapshotRequest>,
    ) -> Result<Response<InstallSnapshotReply>, Status> {
        self.authorize(&req)?;
        let reply = self.cluster.raft().install_snapshot(req.into_inner()).await;
        Ok(Response::new(reply))
    }

    async fn vote(&self, req: Request<VoteRequest>) -> Result<Response<VoteReply>, Status> {
        self.authorize(&req)?;
        let reply = self.cluster.raft().vote(req.into_inner()).await;
        Ok(Response::new(reply))
    }

    async fn leader(&self, req: Request<LeaderRequest>) -> Result<Response<LeaderReply>, Status> {
        self.authorize(&req)?;
        let reply = self
            .cluster
            .execute_as_leader(req.into_inner())
            .await
            .map_err(Into::into);
        Ok(Response::new(reply))
    }

    async fn ping(&self, req: Request<()>) -> Result<Response<NodeId>, Status> {
        self.authorize(&req)?;
        Ok(Response::new(self.cluster.id()))
    }

    type ConsoleStream = ReceiverStream<Result<Bytes, Status>>;

    async fn console(
        &self,
        req: Request<Streaming<Bytes>>,
    ) -> Result<Response<Self::ConsoleStream>, Status> {
        self.authorize(&req)?;
        let vm = req
            .metadata()
            .get(VM_METADATA)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| Uuid::try_parse(s).ok())
            .ok_or_else(|| Status::invalid_argument("missing VM id"))?;
        let hv = self
            .hv
            .as_ref()
            .ok_or_else(|| Status::unimplemented("this node does not serve consoles"))?;
        let not_running = || Status::failed_precondition(format!("VM {vm} is not running here"));
        let path = match hv.console_endpoint(vm).await {
            Ok(path) => path,
            Err(DriverError::NotRunning(_)) => return Err(not_running()),
            Err(e) => return Err(Status::internal(e.to_string())),
        };
        let vnc = UnixStream::connect(&path)
            .await
            .map_err(|_| not_running())?;
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(bridge(req.into_inner(), vnc, tx));
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

/// 요청 스트림을 VNC 소켓으로, VNC 소켓을 응답 스트림으로 잇는다. 어느 쪽이든 끝나면 모두 닫는다.
async fn bridge(
    mut inbound: Streaming<Bytes>,
    mut vnc: UnixStream,
    tx: mpsc::Sender<Result<Bytes, Status>>,
) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        tokio::select! {
            msg = inbound.message() => match msg {
                Ok(Some(data)) => {
                    if vnc.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Ok(None) | Err(_) => break,
            },
            n = vnc.read(&mut buf) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(Ok(Bytes::copy_from_slice(&buf[..n]))).await.is_err() {
                        break;
                    }
                }
            },
        }
    }
}

/// 노드 간 서버가 멈춘 이유
pub type ServeError = tonic::transport::Error;

/// `listener`에서 노드 간 gRPC를 받는다. 클라이언트 인증서가 클러스터 CA로 서명되어 있어야 한다.
/// `shutdown`이 끝나면 멈춘다.
pub async fn serve(
    listener: TcpListener,
    tls: &NodeTls,
    service: PeerService,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(&tls.cert_pem, &tls.key_pem))
        .client_ca_root(Certificate::from_pem(&tls.ca_pem));
    let service = PeerServer::new(service)
        .max_decoding_message_size(MAX_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_MESSAGE_BYTES);
    Server::builder()
        .tls_config(tls)?
        .add_service(service)
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown)
        .await
}
```

`crates/opl-peer/src/lib.rs` 전체. 생성 코드가 단위 구조체 코덱을 `default()`로 만들어 clippy가 경고하므로 생성 모듈에만 허용한다:

```rust
//! 노드 간 통신과 클러스터 PKI.
//!
//! 노드는 gRPC(HTTP/2) over mTLS로 이야기한다. 클러스터 CA가 서명한 인증서를 가진 노드만 연결할 수
//! 있고, 서버는 상대 인증서의 노드 id가 지금 멤버인지도 확인한다. 메시지는 Rust 타입을 serde JSON으로
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, VM 콘솔 중계다.

mod client;
mod codec;
pub mod pki;
mod server;
pub mod tls;
mod wire;

pub use client::{ConsoleError, ConsoleStream, GrpcNetwork, Peers};
pub use server::{PeerService, ServeError, serve};

/// 생성된 gRPC 클라이언트와 서버. 생성 코드가 단위 구조체 코덱을 `default()`로 만든다.
#[allow(clippy::default_constructed_unit_structs)]
mod proto {
    tonic::include_proto!("opl.peer.v1.Peer");
}

/// 콘솔 스트림에서 VM id를 담는 메타데이터 키
pub const VM_METADATA: &str = "opl-vm";

/// 한 메시지의 최대 크기. Raft 로그 묶음과 스냅샷 조각을 JSON으로 보내므로 기본값(4MiB)보다 크게 둔다.
pub const MAX_MESSAGE_BYTES: usize = 64 << 20;
```

- [ ] **Step 6: 통과 확인**

Run: `cargo test -p opl-peer && cargo clippy -p opl-peer --all-targets -- -D warnings`
Expected: PASS (`nodes_replicate_and_forward_over_mtls`, `strangers_and_non_members_are_refused`, `a_new_node_joins_as_learner_then_voter`, `console_is_relayed_to_the_node_running_the_vm`), 경고 없음

- [ ] **Step 7: 커밋**

```bash
git add Cargo.lock crates/opl-peer
git commit -m "feat(peer): 노드 간 gRPC over mTLS(Raft, 리더 전달, Ping, 콘솔)"
```

---
### Task 6: 리더의 노드 생존 감시

리더만 주기적으로 다른 멤버에 `Ping`을 보낸다. 마지막 응답 뒤 `offline_after`가 지나면 `NodeStatus.online`을 false로 기록하고, 다시 응답하면 true로 기록한다. 값이 바뀔 때만 쓰므로 Raft에 주기적인 쓰기가 생기지 않는다. 리더가 된 시점을 첫 응답으로 쳐서, 새 리더가 다른 노드를 곧바로 오프라인으로 만들지 않게 한다. 멤버가 아닌 Node(제거되는 중)는 오프라인이다. 기본값은 간격 2초, 10초 응답이 없으면 오프라인이다.

**Files:**
- Create: `crates/opl-peer/src/monitor.rs`
- Modify: `crates/opl-peer/src/lib.rs`, `crates/opl-peer/tests/grpc.rs`

- [ ] **Step 1: 실패하는 테스트**

`crates/opl-peer/tests/grpc.rs`에서 `use opl_peer::{GrpcNetwork, PeerService, Peers};`를 `use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};`로 바꾸고, `use opl_types::resource::Resource;` 앞에 `use opl_types::node::{NodeSpec, NodeStatus};`를 더한다. 파일 끝에 더한다. 하나는 오프라인으로 시작해 응답하면 온라인이 되는지, 팔로워를 멈추면 오프라인이 되는지 본다:

```rust
#[tokio::test]
async fn leader_records_when_nodes_go_offline_and_come_back() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    for (i, node) in nodes.iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
            },
        );
        r.id = node.id().0;
        // 하나는 오프라인으로 시작해, 응답하면 온라인이 되는지 본다.
        r.status.online = node.id() == leader;
        nodes[0].cluster.create(&r).await.unwrap();
    }
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let leader_cluster = nodes
        .iter()
        .find(|n| n.id() == leader)
        .unwrap()
        .cluster
        .clone();
    let online = |id: NodeId| {
        leader_cluster
            .get::<NodeSpec>(id.0)
            .unwrap()
            .unwrap()
            .status
            .online
    };
    let wait_for = |want: bool, id: NodeId| {
        let online = &online;
        async move {
            let deadline = Instant::now() + WAIT;
            while online(id) != want {
                assert!(
                    Instant::now() < deadline,
                    "node {id} never became online={want}"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    let follower_id = nodes[follower].id();
    wait_for(true, follower_id).await;
    nodes[follower].stop().await;
    wait_for(false, follower_id).await;
    assert!(online(leader));
    for m in monitors {
        m.shutdown().await;
    }
}
```

Run: `cargo test -p opl-peer --test grpc leader_records`
Expected: 컴파일 실패 — `NodeMonitor`, `NodeMonitorConfig`가 없다

- [ ] **Step 2: 구현**

`crates/opl-peer/src/monitor.rs`:

```rust
//! 노드 생존 감시. 리더만 다른 노드에 주기적으로 `Ping`을 보내고, 노드의 온라인 여부가 바뀔 때만
//! `NodeStatus.online`을 기록한다(Raft에 주기적인 쓰기가 생기지 않는다). 스케줄러가 이 값을 본다.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_types::node::{NodeSpec, NodeStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::client::Peers;

#[derive(Debug, Clone)]
pub struct NodeMonitorConfig {
    /// `Ping`을 보내는 간격
    pub interval: Duration,
    /// 마지막 응답 뒤 이만큼 지나면 오프라인이다.
    pub offline_after: Duration,
}

impl Default for NodeMonitorConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(2),
            offline_after: Duration::from_secs(10),
        }
    }
}

pub struct NodeMonitor {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl NodeMonitor {
    pub fn spawn(cluster: Arc<Cluster>, peers: Arc<Peers>, config: NodeMonitorConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(cluster, peers, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    config: NodeMonitorConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    // 노드별 마지막 응답 시각. 리더가 된 시점에 비우고, 처음에는 그 시점을 응답으로 쳐서
    // 새 리더가 다른 노드를 곧바로 오프라인으로 만들지 않게 한다.
    let mut last_seen: HashMap<Uuid, Instant> = HashMap::new();
    let mut interval = tokio::time::interval(config.interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        let members = cluster.members();
        if members.leader != Some(cluster.id()) {
            last_seen.clear();
            continue;
        }
        let Ok((_, nodes)) = cluster.list::<NodeSpec>() else {
            continue;
        };
        let now = Instant::now();
        let pings = nodes.iter().map(|node| {
            let addr = members.addr(NodeId(node.id)).map(str::to_string);
            let peers = peers.clone();
            let me = node.id == cluster.id().0;
            let timeout = config.interval;
            async move {
                match addr {
                    _ if me => Some(true),
                    Some(addr) => Some(peers.ping(&addr, timeout).await.is_ok()),
                    // 멤버가 아니다(제거되는 중).
                    None => None,
                }
            }
        });
        let results = futures_util::future::join_all(pings).await;
        for (node, result) in nodes.iter().zip(results) {
            let online = match result {
                None => false,
                Some(ok) => {
                    let seen = last_seen.entry(node.id).or_insert(now);
                    if ok {
                        *seen = now;
                    }
                    now.duration_since(*seen) < config.offline_after
                }
            };
            if node.status.online != online {
                tracing::info!(node = %node.name, online, "node availability changed");
                let status = NodeStatus {
                    online,
                    ..node.status.clone()
                };
                if let Err(e) = cluster.update_status::<NodeSpec>(node.id, &status).await {
                    tracing::warn!(node = %node.name, error = %e, "cannot record node availability");
                }
            }
        }
    }
}
```

`crates/opl-peer/src/lib.rs` 전체:

```rust
//! 노드 간 통신과 클러스터 PKI.
//!
//! 노드는 gRPC(HTTP/2) over mTLS로 이야기한다. 클러스터 CA가 서명한 인증서를 가진 노드만 연결할 수
//! 있고, 서버는 상대 인증서의 노드 id가 지금 멤버인지도 확인한다. 메시지는 Rust 타입을 serde JSON으로
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, VM 콘솔 중계다.

mod client;
mod codec;
pub mod monitor;
pub mod pki;
mod server;
pub mod tls;
mod wire;

pub use client::{ConsoleError, ConsoleStream, GrpcNetwork, Peers};
pub use monitor::{NodeMonitor, NodeMonitorConfig};
pub use server::{PeerService, ServeError, serve};

/// 생성된 gRPC 클라이언트와 서버. 생성 코드가 단위 구조체 코덱을 `default()`로 만든다.
#[allow(clippy::default_constructed_unit_structs)]
mod proto {
    tonic::include_proto!("opl.peer.v1.Peer");
}

/// 콘솔 스트림에서 VM id를 담는 메타데이터 키
pub const VM_METADATA: &str = "opl-vm";

/// 한 메시지의 최대 크기. Raft 로그 묶음과 스냅샷 조각을 JSON으로 보내므로 기본값(4MiB)보다 크게 둔다.
pub const MAX_MESSAGE_BYTES: usize = 64 << 20;
```

- [ ] **Step 3: 통과 확인**

Run: `cargo test -p opl-peer`
Expected: PASS (`leader_records_when_nodes_go_offline_and_come_back` 포함)

- [ ] **Step 4: 커밋**

```bash
git add crates/opl-peer
git commit -m "feat(peer): 리더가 노드 생존을 Ping으로 보고 바뀔 때만 기록"
```

---
### Task 7: API가 여러 노드를 안다: 콘솔 중계, `Secure` 쿠키, 노드별 고아 정리

- **콘솔:** VM이 다른 노드에서 돌면 그 노드의 노드 간 주소(멤버십)로 gRPC 콘솔 스트림을 열어 WebSocket과 잇는다.
  - 그 노드에서 VM이 돌지 않으면 409 `vm_not_running`을 돌려준다.
  - 그 노드에 닿지 않으면 502 `console_unavailable`을 돌려준다.
  - `AppState.peers`가 없으면(테스트의 1인 클러스터) 이 노드의 콘솔만 연다.
- **`Secure` 쿠키:** HTTPS로 서비스하면(`secure_cookies`) 세션 쿠키에 `Secure`를 붙인다.
- **노드별 고아 정리:** 볼륨을 만들 때 status에 그 파일을 만드는 노드를 기록한다. Task 실행기는 시작할 때 자기 노드가 만들던 고아만 지운다. 다른 노드의 API가 방금 만든 볼륨을 지우지 않는다. `node`가 없는 예전 볼륨은 예전처럼 어느 노드든 지운다.

**Files:**
- Create: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-api/Cargo.toml`, `crates/opl-api/src/{state,users,console,volumes,tasks}.rs`, `crates/opl-api/tests/{common/mod.rs,tasks.rs}`

- [ ] **Step 1: 하네스 정리**

`crates/opl-api/tests/common/mod.rs`에서 `Env::login`의 본문을 다음 한 줄로 바꾼다:

```rust
        login_at(&self.base, user, password).await
```

그리고 원래 본문을 옮긴 함수를 `/// 디스크 없는 작은 VM 생성 요청` 앞에 더한다(다른 노드의 API에도 로그인할 수 있게):

```rust
/// `base`의 API에 로그인해 API 토큰을 만들고, 그 토큰을 쓰는 클라이언트를 돌려준다.
pub async fn login_at(base: &str, user: &str, password: &str) -> Client {
    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({ "username": user, "password": password }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "login as {user}");
    let cookie = resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let resp = http
        .post(format!("{base}/api/v1/tokens"))
        .header("cookie", cookie)
        .json(&json!({ "name": format!("{user}-{}", Uuid::new_v4().simple()) }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = resp.json().await.unwrap();
    Client {
        base: base.to_string(),
        http,
        token: Some(body["secret"].as_str().unwrap().to_string()),
    }
}
```

- [ ] **Step 2: 실패하는 테스트**

`crates/opl-api/tests/tasks.rs`에서 `use opl_types::storage::VolumeSpec;`를 `use opl_types::storage::{Volume, VolumeSpec};`로 바꾸고 끝에 더한다:

```rust
#[tokio::test]
async fn orphans_are_removed_only_by_the_node_that_made_them() {
    let other = Uuid::now_v7();
    let env = Env::start_with(
        |_| {},
        move |cluster| async move {
            let me = cluster.id().0;
            for (name, node) in [("mine", Some(me)), ("theirs", Some(other))] {
                let mut volume: Volume = Resource::new(
                    name,
                    VolumeSpec {
                        pool: "local".into(),
                        size_bytes: 1,
                        format: ImageFormat::Raw,
                    },
                );
                volume.status.node = node;
                cluster.create(&volume).await.unwrap();
            }
        },
    )
    .await;
    // 실행기는 고아를 지운 뒤에 뜬다.
    assert!(
        env.cluster
            .get_by_name::<VolumeSpec>("mine")
            .unwrap()
            .is_none()
    );
    assert!(
        env.cluster
            .get_by_name::<VolumeSpec>("theirs")
            .unwrap()
            .is_some()
    );
}
```

`crates/opl-api/tests/two_nodes.rs`를 만든다. 두 노드를 실제 gRPC(mTLS)로 잇고, 노드마다 reconciler, Task 실행기, API(HTTP)를 띄운다. 노드 A의 API로 노드 B에 VM을 만들어 켜고, A의 API로 콘솔을 열어 B의 가짜 VNC 서버와 이야기한다:

```rust
//! 실제 gRPC(mTLS)로 이은 두 노드: 한 노드의 API로 다른 노드에 VM을 띄우고 콘솔을 중계한다.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use openraft::BasicNode;
use opl_api::{AppState, TaskRunner, hash_password};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{DirStorage, Hypervisor, MockHypervisor, Storage};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{GrpcNetwork, PeerService, Peers};
use opl_reconciler::Reconciler;
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use reqwest::StatusCode;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

use common::{ADMIN_PASSWORD, WAIT, login_at, reconciler_config, runner_config, vm_request};

struct Node {
    id: Uuid,
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    tls: NodeTls,
    listener: Option<TcpListener>,
    base: String,
    _workers: Option<(Reconciler, TaskRunner)>,
}

impl Node {
    async fn open(ca: &ClusterCa, root: &TempDir) -> Node {
        let id = Uuid::now_v7();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, id, &["127.0.0.1".into()]).unwrap(),
            key_pem,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let peers = Arc::new(Peers::new(&tls));
        let mut config = ClusterConfig::new(NodeId(id), root.path().join(id.to_string()));
        // 테스트가 동시에 많이 돌면 첫 선거가 몇 초씩 걸린다.
        config.write_timeout = Duration::from_secs(30);
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        let cluster = Arc::new(
            Cluster::open(config, GrpcNetwork::new(peers.clone()))
                .await
                .unwrap()
                .with_forwarder(peers.clone()),
        );
        Node {
            id,
            addr,
            cluster,
            peers,
            hv: Arc::new(MockHypervisor::default()),
            tls,
            listener: Some(listener),
            base: String::new(),
            _workers: None,
        }
    }

    /// 노드 간 서버, reconciler, Task 실행기, API 서버(HTTP)를 띄운다.
    async fn serve(&mut self, root: &TempDir) {
        let hv: Arc<dyn Hypervisor> = self.hv.clone();
        let service = PeerService::new(self.cluster.clone(), Some(hv.clone()));
        let listener = self.listener.take().unwrap();
        let tls = self.tls.clone();
        tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, std::future::pending())
                .await
                .unwrap();
        });
        let reconciler =
            Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config(self.id));
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new("qemu-img"));
        let work = root.path().join(format!("{}-work", self.id));
        let runner = TaskRunner::spawn(
            self.cluster.clone(),
            storage,
            reconciler.handle(),
            runner_config(self.id, &work),
        )
        .await;
        let mut state = AppState::new(self.cluster.clone(), self.id, hv, work.join("uploads"));
        state.peers = Some(self.peers.clone());
        let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.base = format!("http://{}", api.local_addr().unwrap());
        let app = opl_api::router(Arc::new(state));
        tokio::spawn(async move { axum::serve(api, app).await.unwrap() });
        self._workers = Some((reconciler, runner));
    }
}

/// 두 노드를 띄워 클러스터를 만들고 Node 리소스와 관리자를 채운다. 노드 간 서버가 먼저 떠야
/// 선거가 끝나므로 서버부터 띄운다.
async fn two_nodes() -> (TempDir, Node, Node) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut a = Node::open(&ca, &root).await;
    let mut b = Node::open(&ca, &root).await;
    a.serve(&root).await;
    b.serve(&root).await;
    let members = BTreeMap::from([
        (NodeId(a.id), BasicNode::new(&a.addr)),
        (NodeId(b.id), BasicNode::new(&b.addr)),
    ]);
    a.cluster.initialize(members).await.unwrap();
    for (i, node) in [&a, &b].into_iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node{}", i + 1),
            NodeSpec {
                address: node.base.clone(),
                maintenance: false,
            },
        );
        r.id = node.id;
        r.status = NodeStatus {
            online: true,
            cpus: 4,
            memory_mib: 8192,
            hypervisor: "mock".into(),
        };
        // 리더가 뽑힐 때까지 쓰기가 기다린다.
        a.cluster.create(&r).await.unwrap();
    }
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: hash_password(ADMIN_PASSWORD.into()).await.unwrap(),
            role: Role::Admin,
        },
    );
    a.cluster.create(&admin).await.unwrap();
    (root, a, b)
}

#[tokio::test]
async fn console_of_a_vm_on_another_node_is_relayed() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    b.hv.set_console_dir(tmp.path());

    // 노드 A의 API로 노드 B에 VM을 만들고 켠다. Task는 B의 실행기가 끝낸다.
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(b.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = b
        .cluster
        .get_by_name::<opl_types::vm::VmSpec>("web")
        .unwrap()
        .unwrap();
    assert!(b.hv.inspect(vm.id).await.unwrap().is_some());
    assert!(a.hv.inspect(vm.id).await.unwrap().is_none());

    let listener =
        tokio::net::UnixListener::bind(tmp.path().join(format!("{}.vnc", vm.id))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });
    let url = format!("{}/api/v1/vms/web/console", a.base).replacen("http", "ws", 1);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {}", admin.token.as_ref().unwrap())
            .parse()
            .unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let greeting = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(greeting, Message::binary(b"RFB 003.008\n".to_vec()));
    ws.send(Message::binary(b"ping".to_vec())).await.unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap(),
        Message::binary(b"ping".to_vec())
    );

    // B에서 만든 것은 A에서도 곧 보인다(같은 저장소).
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let deadline = Instant::now() + WAIT;
    while admin_b.get("/vms/web").await.0 != StatusCode::OK {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
```

Run: `cargo test -p opl-api --test tasks --test two_nodes`
Expected: 컴파일 실패 — `opl_peer`(의존성 없음), `AppState.peers`가 없다

- [ ] **Step 3: 상태와 쿠키**

`crates/opl-api/Cargo.toml`의 `[dependencies]`에서 `opl-driver.workspace = true` 다음 줄에 `opl-peer.workspace = true`를 더한다.

`crates/opl-api/src/state.rs`: `use opl_driver::Hypervisor;` 다음 줄에 `use opl_peer::Peers;`를 더한다. `AppState`의 `pub(crate) sessions: Sessions,` 앞에 필드 두 개를 더하고, `AppState::new`에서 `upload_dir,` 다음에 `peers: None,`과 `secure_cookies: false,`를 더한다:

```rust
    /// 다른 노드에서 도는 VM의 콘솔을 중계할 때 쓴다. 없으면 이 노드의 VM 콘솔만 연다.
    pub peers: Option<Arc<Peers>>,
    /// 세션 쿠키에 `Secure`를 붙인다(HTTPS로 서비스할 때).
    pub secure_cookies: bool,
```

`crates/opl-api/src/users.rs`의 `login`에서 쿠키를 만드는 부분을 다음으로 바꾼다:

```rust
    let mut cookie = format!(
        "{SESSION_COOKIE}={session}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION_TTL.as_secs()
    );
    if state.secure_cookies {
        cookie.push_str("; Secure");
    }
```

- [ ] **Step 4: 콘솔 중계**

`crates/opl-api/src/console.rs` 전체:

```rust
//! `GET /api/v1/vms/{id}/console`: WebSocket 바이너리 메시지를 VM의 VNC 유닉스 소켓으로 잇는다.
//! VM이 다른 노드에서 돌면 노드 간 gRPC로 그 노드에 중계한다.

use std::sync::Arc;

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use opl_cluster::types::NodeId;
use opl_driver::DriverError;
use opl_peer::{ConsoleError, ConsoleStream};
use opl_types::resource::ResourceSpec;
use opl_types::vm::VmSpec;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::auth::{Action, Principal};
use crate::error::ApiError;
use crate::resources::lookup;
use crate::state::AppState;

#[utoipa::path(get, path = "/vms/{key}/console", tag = "vms",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 101, description = "WebSocket carrying the VNC (RFB) stream"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn console(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let not_running = || {
        ApiError::conflict(
            "vm_not_running",
            format!("VM {:?} is not running on this node", vm.name),
        )
    };
    let Some(node) = vm.status.node else {
        return Err(not_running());
    };
    if node != state.node {
        let Some(peers) = &state.peers else {
            return Err(not_running());
        };
        let Some(addr) = state
            .cluster
            .members()
            .addr(NodeId(node))
            .map(str::to_string)
        else {
            return Err(not_running());
        };
        let ws = ws?;
        let stream = peers.console(&addr, vm.id).await.map_err(|e| match e {
            ConsoleError::NotRunning => not_running(),
            ConsoleError::Unavailable(_) => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "console_unavailable",
                format!("cannot reach the node running VM {:?}", vm.name),
            ),
        })?;
        return Ok(ws.on_upgrade(move |socket| relay(socket, stream)));
    }
    let path = match state.hv.console_endpoint(vm.id).await {
        Ok(path) => path,
        Err(DriverError::NotRunning(_)) => return Err(not_running()),
        Err(e) => return Err(ApiError::internal(e)),
    };
    let ws = ws?;
    let vnc = UnixStream::connect(&path)
        .await
        .map_err(|_| not_running())?;
    Ok(ws.on_upgrade(move |socket| bridge(socket, vnc)))
}

/// WebSocket과 다른 노드의 콘솔 스트림을 잇는다.
async fn relay(mut socket: WebSocket, mut stream: ConsoleStream) {
    loop {
        tokio::select! {
            data = stream.recv() => match data {
                Some(data) => {
                    if socket.send(Message::binary(data)).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(data))) => {
                    if !stream.send(data).await {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

async fn bridge(mut socket: WebSocket, mut vnc: UnixStream) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        tokio::select! {
            n = vnc.read(&mut buf) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if socket.send(Message::binary(buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(data))) => {
                    if vnc.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}
```

- [ ] **Step 5: 볼륨을 만드는 노드와 고아 정리**

`crates/opl-api/src/volumes.rs`의 `create_volume`에서 풀을 찾은 뒤를 다음으로 바꾼다:

```rust
    let pool = find_pool(&state, &req.spec.pool)?;
    let node = pool_node(&state, &pool);
    let mut volume: Volume = Resource::new(req.name, req.spec);
    volume.labels = req.labels;
    volume.status.node = Some(node);
    state.cluster.create(&volume).await?;
    let task = create_task(&state.cluster, TaskOp::VolumeCreate, volume.id, node).await?;
```

`import_volume`에서는 `let volume: Volume = Resource::new(`를 `let mut volume: Volume = Resource::new(`로 바꾸고, 그 생성 바로 다음(`let length = content_length(&headers)?;` 앞)에 `volume.status.node = Some(state.node);`를 더한다.

`crates/opl-api/src/tasks.rs`의 `sweep_orphans`에서 설명 주석과 첫 확인을 다음으로 바꾼다:

```rust
/// 이전 opld가 남긴 고아를 지운다: 이 노드가 만들던, 실행할 Task가 없는 Pending 볼륨(볼륨을 만든
/// 뒤 Task를 만들기 전에, 또는 업로드 도중 opld가 죽었다)과, 가져오기 Task가 없는 올린 파일.
/// `tasks`는 `revision`에서 읽은 Task 목록이다. 다른 노드가 만드는 볼륨은 그 노드가 정리한다.
```

```rust
                if volume.revision > revision
                    || volume.status.node.is_some_and(|n| n != ctx.config.node)
                {
                    continue;
                }
```

- [ ] **Step 6: 통과 확인**

Run: `cargo test -p opl-api --all-features`
Expected: PASS (`orphans_are_removed_only_by_the_node_that_made_them`, `console_of_a_vm_on_another_node_is_relayed` 포함)

- [ ] **Step 7: 커밋**

```bash
git add Cargo.lock crates/opl-api
git commit -m "feat(api): 다른 노드의 콘솔 중계, Secure 쿠키, 노드별 고아 정리"
```

---
### Task 8: 클러스터 API: 정보, CA, 조인 토큰, 합류, 노드 제거

- **`GET /cluster`:** 클러스터 이름, CA 지문, 리더, 멤버를 돌려준다. 멤버는 Raft 멤버십(투표 멤버/learner, 노드 간 주소)과 Node 리소스(이름, API 주소, 온라인)를 합친 것이다.
- **`GET /cluster/ca`:** 인증 없이 CA PEM을 돌려준다. 받는 쪽이 지문을 확인한다.
- **`POST /cluster/join-tokens`:** 관리자만 쓴다. 유효 시간은 기본 1시간, 최대 7일이다. 조인 URL에는 이 노드의 API 주소를 넣는다. `issue_join_token`은 `opld`가 클러스터를 만들 때도 부른다.
- **`POST /cluster/join`:** 조인 토큰으로 인증한다. 순서는 다음과 같다.
  1. 토큰을 확인한다(해시는 상수 시간으로 비교하고, 만료도 본다).
  2. 이름과 주소를 검증한다.
  3. 이미 투표 멤버면 409 `already_member`를 돌려준다.
  4. 다른 노드가 쓰는 이름이면 409 `name_taken`을 돌려준다.
  5. CSR에 서명한다. 실패하면 400이고, 토큰은 쓰지 않는다.
  6. 토큰을 지운다. 동시에 같은 토큰을 쓴 요청은 하나만 성공한다.
  7. Node 리소스를 만들거나(전에 실패한 같은 노드면) 다시 쓴다.
  8. learner로 더한다.
- **`DELETE /nodes/{key}`:** 오프라인 노드만 뺀다. 이 노드와 온라인 노드는 409 `node_online`이다.

조인 토큰은 계정처럼 관리자만 다룬다(`Authorizer`).

**Files:**
- Create: `crates/opl-api/src/cluster.rs`, `crates/opl-api/tests/cluster.rs`
- Modify: `crates/opl-api/src/{lib,auth}.rs`, `crates/opl-api/tests/common/mod.rs`

- [ ] **Step 1: 하네스가 클러스터 정보를 만든다**

`crates/opl-api/tests/common/mod.rs`의 `start_with`에서 `cluster.create(&admin).await.unwrap();` 다음 줄에 `seed_cluster_info(&cluster).await;`를 더하고, `/// 디스크 없는 작은 VM 생성 요청` 앞에 더한다:

```rust
/// 클러스터 정보(이름 `test`, 새 CA)를 저장소에 만든다. 클러스터를 만들 때 opld가 하는 일이다.
pub async fn seed_cluster_info(cluster: &Cluster) {
    let ca = opl_peer::pki::ClusterCa::generate("test").unwrap();
    let info = Resource::new(
        "test",
        opl_types::cluster::ClusterSpec {
            ca_cert_pem: ca.cert_pem,
            ca_key_pem: ca.key_pem,
        },
    );
    cluster.create(&info).await.unwrap();
}
```

- [ ] **Step 2: 실패하는 테스트**

`crates/opl-api/tests/cluster.rs`:

```rust
//! 클러스터 정보, 조인 토큰, 노드 합류와 제거.

mod common;

use common::Env;
use opl_api::cluster::{JoinRequest, JoinUrl};
use opl_cluster::types::NodeId;
use opl_peer::pki::{fingerprint, first_cert, node_id_of, node_key_and_csr};
use opl_types::cluster::JoinTokenSpec;
use opl_types::node::{NodeSpec, NodeStatus};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn cluster_view_and_ca() {
    let env = Env::start().await;
    let viewer = env.user("viewer", "viewer").await;
    let (status, view) = viewer.get("/cluster").await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["name"], "test");
    assert_eq!(view["leader"], env.node.to_string());
    assert_eq!(view["members"].as_array().unwrap().len(), 1);
    assert_eq!(view["members"][0]["name"], "node1");
    assert_eq!(view["members"][0]["voter"], true);
    assert_eq!(view["members"][0]["online"], true);

    // CA는 인증 없이 받고, 지문은 클러스터 정보와 같다.
    let anon = env.anonymous();
    assert_eq!(anon.get("/cluster").await.0, StatusCode::UNAUTHORIZED);
    let resp = anon
        .http
        .get(format!("{}/api/v1/cluster/ca", env.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let pem = resp.text().await.unwrap();
    assert_eq!(fingerprint(&pem).unwrap(), view["ca_fingerprint"]);
}

#[tokio::test]
async fn join_tokens_are_for_admins() {
    let env = Env::start().await;
    let operator = env.user("op", "operator").await;
    assert_eq!(
        operator.post("/cluster/join-tokens", json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    let admin = env.admin().await;
    let (status, body) = admin
        .post("/cluster/join-tokens", json!({ "ttl_secs": 0 }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, created) = admin.post("/cluster/join-tokens", json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let url: JoinUrl = created["url"].as_str().unwrap().parse().unwrap();
    assert_eq!(url.address, "127.0.0.1:0");
    let (_, view) = admin.get("/cluster").await;
    assert_eq!(url.ca_fingerprint, view["ca_fingerprint"]);
    // 토큰은 사용자 목록처럼 아무도 볼 수 없다(목록 경로가 없다). 저장소에는 해시만 있다.
    let (_, tokens) = env.cluster.list::<JoinTokenSpec>().unwrap();
    assert_eq!(tokens.len(), 1);
    assert!(!url.token.contains(&tokens[0].spec.secret_hash));
}

fn join_request(token: &str, name: &str) -> (JoinRequest, Uuid) {
    let node = Uuid::now_v7();
    let (_key, csr_pem) = node_key_and_csr().unwrap();
    (
        JoinRequest {
            token: token.to_string(),
            node,
            name: name.to_string(),
            address: "10.0.0.7:8006".into(),
            cluster_address: "10.0.0.7:8007".into(),
            csr_pem,
        },
        node,
    )
}

async fn new_token(env: &Env) -> String {
    let (status, created) = env
        .admin()
        .await
        .post("/cluster/join-tokens", json!({}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created["url"]
        .as_str()
        .unwrap()
        .parse::<JoinUrl>()
        .unwrap()
        .token
}

#[tokio::test]
async fn a_node_joins_once_per_token() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let token = new_token(&env).await;

    // 이름이 겹치면 토큰을 쓰지 않고 거부한다.
    let (req, _) = join_request(&token, "node1");
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "name_taken");

    let (req, node) = join_request(&token, "node2");
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cluster"], "test");
    let cert = first_cert(body["cert_pem"].as_str().unwrap()).unwrap();
    assert_eq!(node_id_of(&cert), Some(node));
    assert_eq!(body["via"], NodeId(env.node).to_string());
    let members = env.cluster.members();
    assert_eq!(
        members.learners.get(&NodeId(node)).unwrap(),
        "10.0.0.7:8007"
    );
    let joined = env.cluster.get::<NodeSpec>(node).unwrap().unwrap();
    assert_eq!(joined.name, "node2");
    assert_eq!(joined.spec.address, "10.0.0.7:8006");

    // 같은 토큰은 다시 쓸 수 없다.
    let (req, _) = join_request(&token, "node3");
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "invalid_token");
    // 합류하다 멈춘 노드(learner)는 새 토큰으로 다시 합류할 수 있다.
    let mut req = join_request(&new_token(&env).await, "node2").0;
    req.node = node;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(env.cluster.members().learners.len(), 1);
    // 투표 멤버는 다시 합류할 수 없다.
    let mut req = join_request(&new_token(&env).await, "node9").0;
    req.node = env.node;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "already_member");
}

#[tokio::test]
async fn bad_or_expired_tokens_are_refused() {
    let env = Env::start().await;
    let anon = env.anonymous();
    for token in [
        "",
        "garbage",
        "oplj_nothex_x",
        &format!("oplj_{}_x", Uuid::now_v7().simple()),
    ] {
        let (req, _) = join_request(token, "node2");
        let (status, body) = anon.post("/cluster/join", json!(req)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{token}: {body}");
    }
    let token = new_token(&env).await;
    let (id, _) = token
        .strip_prefix("oplj_")
        .unwrap()
        .split_once('_')
        .unwrap();
    let id = Uuid::try_parse(id).unwrap();
    // 비밀 값이 틀리면 거부한다.
    let (req, _) = join_request(&format!("oplj_{}_wrong", id.simple()), "node2");
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::UNAUTHORIZED
    );
    // 만료된 토큰도 거부한다.
    let mut stored = env.cluster.get::<JoinTokenSpec>(id).unwrap().unwrap();
    stored.spec.expires_at = 1;
    env.cluster
        .update_spec(id, None, stored.labels.clone(), &stored.spec)
        .await
        .unwrap();
    let (req, _) = join_request(&token, "node2");
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::UNAUTHORIZED
    );
    // CSR이 깨졌으면 토큰을 쓰지 않는다.
    let token = new_token(&env).await;
    let (mut req, _) = join_request(&token, "node2");
    req.csr_pem = "garbage".into();
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::BAD_REQUEST
    );
    let (req, _) = join_request(&token, "node2");
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn only_offline_nodes_are_removed() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (req, node) = join_request(&new_token(&env).await, "node2");
    assert_eq!(
        env.anonymous().post("/cluster/join", json!(req)).await.0,
        StatusCode::OK
    );

    // 이 노드와 온라인 노드는 뺄 수 없다.
    let (status, body) = admin.delete("/nodes/node1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_online");
    let online = NodeStatus {
        online: true,
        ..Default::default()
    };
    env.cluster
        .update_status::<NodeSpec>(node, &online)
        .await
        .unwrap();
    assert_eq!(admin.delete("/nodes/node2").await.0, StatusCode::CONFLICT);
    env.cluster
        .update_status::<NodeSpec>(node, &NodeStatus::default())
        .await
        .unwrap();

    let operator = env.user("op", "operator").await;
    assert_eq!(
        operator.delete("/nodes/node2").await.0,
        StatusCode::FORBIDDEN
    );
    let (status, body) = admin.delete("/nodes/node2").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!env.cluster.members().contains(NodeId(node)));
    assert_eq!(admin.get("/nodes/node2").await.0, StatusCode::NOT_FOUND);
}
```

Run: `cargo test -p opl-api --test cluster`
Expected: 컴파일 실패 — `opl_api::cluster`가 없다

- [ ] **Step 3: 구현**

`crates/opl-api/src/auth.rs`:
- `RoleAuthorizer::check`의 `let account = matches!(kind, Kind::User | Kind::ApiToken);`를 `let account = matches!(kind, Kind::User | Kind::ApiToken | Kind::JoinToken);`로 바꾼다.
- `fn constant_time_eq`를 `pub(crate) fn constant_time_eq`로 바꾼다.

`crates/opl-api/src/cluster.rs`:

```rust
//! 클러스터 정보, 노드 합류(조인 토큰, CSR 서명), 노드 제거.
//!
//! 새 노드는 조인 URL을 받아 이 노드의 `POST /cluster/join`을 부른다. 토큰을 지워 한 번만 쓰게 하고,
//! 노드의 CSR에 클러스터 CA로 서명하고, Node 리소스를 만들고, 노드를 learner로 더한다. 새 노드는
//! 받은 인증서로 노드 간 서버를 띄우고 로그를 따라잡은 뒤 스스로 투표 멤버 승격을 요청한다.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use opl_cluster::ClusterError;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_peer::pki::{ClusterCa, fingerprint};
use opl_types::cluster::{ClusterInfo, ClusterSpec, JoinToken, JoinTokenSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::{Resource, ResourceSpec};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal, constant_time_eq, random_hex, sha256_hex};
use crate::error::{ApiError, Json};
use crate::resources::{check_name, lookup};
use crate::state::AppState;

/// 조인 토큰의 기본 유효 시간
pub const DEFAULT_JOIN_TTL: Duration = Duration::from_secs(3600);
/// 조인 토큰의 최대 유효 시간
const MAX_JOIN_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const JOIN_TOKEN_PREFIX: &str = "oplj_";
const JOIN_SCHEME: &str = "opl-join://";

/// `opl-join://<API 주소>?token=<토큰>&ca=<CA 지문>`. `oplctl cluster join`에 그대로 넘긴다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinUrl {
    /// 합류 요청을 받을 노드의 API 주소(`host:port`)
    pub address: String,
    pub token: String,
    /// 클러스터 CA 인증서의 지문(`sha256:<16진수>`). 새 노드는 이것으로 상대를 확인한다.
    pub ca_fingerprint: String,
}

impl fmt::Display for JoinUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{JOIN_SCHEME}{}?token={}&ca={}",
            self.address, self.token, self.ca_fingerprint
        )
    }
}

impl FromStr for JoinUrl {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || {
            format!(
                "invalid join URL {s:?}: expected {JOIN_SCHEME}<host:port>?token=...&ca=sha256:..."
            )
        };
        let rest = s.trim().strip_prefix(JOIN_SCHEME).ok_or_else(bad)?;
        let (address, query) = rest.split_once('?').ok_or_else(bad)?;
        let (mut token, mut ca) = (None, None);
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("token", v)) => token = Some(v.to_string()),
                Some(("ca", v)) => ca = Some(v.to_string()),
                _ => return Err(bad()),
            }
        }
        if host_of(address).is_none() {
            return Err(bad());
        }
        Ok(JoinUrl {
            address: address.to_string(),
            token: token.ok_or_else(bad)?,
            ca_fingerprint: ca.filter(|c| c.starts_with("sha256:")).ok_or_else(bad)?,
        })
    }
}

/// `host:port`나 `[v6]:port`의 호스트. 포트가 숫자가 아니면 `None`이다.
pub fn host_of(address: &str) -> Option<&str> {
    let (host, port) = address.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 클러스터 정보 리소스. 클러스터를 만들 때 하나 생긴다.
fn cluster_info(state: &AppState) -> Result<ClusterInfo, ApiError> {
    state
        .cluster
        .list::<ClusterSpec>()?
        .1
        .into_iter()
        .next()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "cluster_not_initialized",
                "the cluster has not been initialized",
            )
        })
}

fn ca_fingerprint(info: &ClusterInfo) -> Result<String, ApiError> {
    fingerprint(&info.spec.ca_cert_pem).map_err(ApiError::internal)
}

/// 클러스터 멤버 한 노드
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MemberView {
    pub id: Uuid,
    /// Node 리소스 이름. 아직 없으면(합류 중) 비어 있다.
    pub name: Option<String>,
    /// API 주소
    pub address: Option<String>,
    /// 노드 간 통신 주소
    pub cluster_address: String,
    /// 투표 멤버인지(아니면 learner)
    pub voter: bool,
    pub online: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ClusterView {
    pub name: String,
    /// 클러스터 CA 인증서의 지문. `oplctl login --ca-fingerprint`에 쓴다.
    pub ca_fingerprint: String,
    pub leader: Option<Uuid>,
    pub members: Vec<MemberView>,
}

/// 클러스터 이름, CA 지문, 멤버(이 노드가 아는 Raft 멤버십)
#[utoipa::path(get, path = "/cluster", tag = "cluster",
    responses((status = 200, body = ClusterView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_cluster(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<ClusterView>, ApiError> {
    state.authorize(&principal, Action::Read, ClusterSpec::KIND)?;
    let info = cluster_info(&state)?;
    let members = state.cluster.members();
    let nodes = state.cluster.list::<NodeSpec>()?.1;
    let view = |id: NodeId, addr: &String, voter: bool| {
        let node = nodes.iter().find(|n| n.id == id.0);
        MemberView {
            id: id.0,
            name: node.map(|n| n.name.clone()),
            address: node.map(|n| n.spec.address.clone()),
            cluster_address: addr.clone(),
            voter,
            online: node.is_some_and(|n| n.status.online),
        }
    };
    let mut list: Vec<MemberView> = members
        .voters
        .iter()
        .map(|(id, addr)| view(*id, addr, true))
        .collect();
    list.extend(
        members
            .learners
            .iter()
            .map(|(id, addr)| view(*id, addr, false)),
    );
    Ok(Json(ClusterView {
        name: info.name.clone(),
        ca_fingerprint: ca_fingerprint(&info)?,
        leader: members.leader.map(|id| id.0),
        members: list,
    }))
}

/// 클러스터 CA 인증서(PEM). 인증 없이 읽는다. 받는 쪽은 반드시 지문을 확인해야 한다.
#[utoipa::path(get, path = "/cluster/ca", tag = "cluster", security(()),
    responses((status = 200, description = "CA certificate", content_type = "application/x-pem-file", body = String),
              (status = "5XX", body = crate::error::ErrorBody)))]
pub async fn get_ca(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let info = cluster_info(&state)?;
    Ok((
        [(CONTENT_TYPE, "application/x-pem-file")],
        info.spec.ca_cert_pem,
    ))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinTokenRequest {
    /// 유효 시간(초). 기본 1시간, 최대 7일
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreatedJoinToken {
    /// 새 노드에서 `oplctl cluster join`에 넘길 URL. 다시 볼 수 없다.
    pub url: String,
    /// 만료 시각(유닉스 초)
    pub expires_at: u64,
}

/// 새 토큰과 그 조인 URL(이 노드의 API 주소). 토큰은 한 번만 쓸 수 있다.
pub async fn issue_join_token(
    state: &AppState,
    ttl: Duration,
) -> Result<CreatedJoinToken, ApiError> {
    let info = cluster_info(state)?;
    let me = state
        .cluster
        .get::<NodeSpec>(state.node)?
        .ok_or_else(|| ApiError::internal("this node has no Node resource"))?;
    let secret = random_hex(32);
    let expires_at = now_secs() + ttl.as_secs();
    let mut token: JoinToken = Resource::new(
        "",
        JoinTokenSpec {
            secret_hash: sha256_hex(&secret),
            expires_at,
        },
    );
    token.name = token.id.to_string();
    state.cluster.create(&token).await?;
    let url = JoinUrl {
        address: me.spec.address,
        token: format!("{JOIN_TOKEN_PREFIX}{}_{secret}", token.id.simple()),
        ca_fingerprint: ca_fingerprint(&info)?,
    };
    Ok(CreatedJoinToken {
        url: url.to_string(),
        expires_at,
    })
}

#[utoipa::path(post, path = "/cluster/join-tokens", tag = "cluster",
    request_body = JoinTokenRequest,
    responses((status = 201, body = CreatedJoinToken), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_join_token(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<JoinTokenRequest>,
) -> Result<(StatusCode, Json<CreatedJoinToken>), ApiError> {
    state.authorize(&principal, Action::Write, JoinTokenSpec::KIND)?;
    let ttl = req.ttl_secs.map_or(DEFAULT_JOIN_TTL, Duration::from_secs);
    if ttl.is_zero() || ttl > MAX_JOIN_TTL {
        return Err(ApiError::bad_request(
            "ttl_secs must be between 1 and 604800",
        ));
    }
    Ok((
        StatusCode::CREATED,
        Json(issue_join_token(&state, ttl).await?),
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinRequest {
    /// 조인 URL의 토큰
    pub token: String,
    /// 새 노드의 id(Raft 노드 id, Node 리소스 id)
    pub node: Uuid,
    /// 새 노드의 이름
    pub name: String,
    /// 새 노드의 API 주소(`host:port`)
    pub address: String,
    /// 새 노드의 노드 간 통신 주소(`host:port`)
    pub cluster_address: String,
    /// 새 노드가 만든 키의 CSR(PEM)
    pub csr_pem: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JoinResponse {
    pub cluster: String,
    pub ca_pem: String,
    /// 새 노드의 인증서(PEM)
    pub cert_pem: String,
    /// 이 노드의 노드 간 통신 주소. 새 노드는 여기로 승격을 요청한다.
    pub via: String,
}

fn invalid_token() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "invalid_token",
        "invalid or expired join token",
    )
}

/// 새 노드의 합류 요청. 조인 토큰으로 인증한다(사용자 인증 없음).
#[utoipa::path(post, path = "/cluster/join", tag = "cluster", security(()),
    request_body = JoinRequest,
    responses((status = 200, body = JoinResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn join(
    State(state): State<Arc<AppState>>,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, ApiError> {
    // 토큰부터 확인해, 토큰 없는 요청에는 클러스터에 대해 아무것도 알려 주지 않는다.
    let (id, secret) = req
        .token
        .strip_prefix(JOIN_TOKEN_PREFIX)
        .and_then(|rest| rest.split_once('_'))
        .and_then(|(id, secret)| Some((Uuid::try_parse(id).ok()?, secret)))
        .ok_or_else(invalid_token)?;
    let token = state
        .cluster
        .get::<JoinTokenSpec>(id)?
        .ok_or_else(invalid_token)?;
    if !constant_time_eq(&sha256_hex(secret), &token.spec.secret_hash)
        || token.spec.expires_at <= now_secs()
    {
        return Err(invalid_token());
    }

    check_name(&req.name)?;
    let (Some(api_host), Some(cluster_host)) =
        (host_of(&req.address), host_of(&req.cluster_address))
    else {
        return Err(ApiError::bad_request(
            "address and cluster_address must be host:port",
        ));
    };
    if req.node.is_nil() {
        return Err(ApiError::bad_request("node id must not be nil"));
    }
    // learner이면 합류하다 멈춘 노드다. 새 토큰으로 다시 합류할 수 있다.
    if state
        .cluster
        .members()
        .voters
        .contains_key(&NodeId(req.node))
    {
        return Err(ApiError::conflict(
            "already_member",
            format!("node {} is already a cluster member", req.node),
        ));
    }
    if let Some(other) = state.cluster.get_by_name::<NodeSpec>(&req.name)?
        && other.id != req.node
    {
        return Err(ApiError::conflict(
            "name_taken",
            format!("node name {:?} is already taken", req.name),
        ));
    }
    let info = cluster_info(&state)?;
    let me = state
        .cluster
        .members()
        .addr(NodeId(state.node))
        .map(str::to_string)
        .ok_or_else(|| ApiError::internal("this node is not in the membership it knows"))?;

    let mut sans = vec![api_host.to_string()];
    if cluster_host != api_host {
        sans.push(cluster_host.to_string());
    }
    sans.extend(["localhost".into(), "127.0.0.1".into(), "::1".into()]);
    let ca = ClusterCa {
        cert_pem: info.spec.ca_cert_pem.clone(),
        key_pem: info.spec.ca_key_pem.clone(),
    };
    let cert_pem = ca
        .sign_node(&req.csr_pem, req.node, &sans)
        .map_err(|e| ApiError::bad_request(format!("cannot sign the CSR: {e}")))?;

    // 토큰을 지운다. 동시에 같은 토큰을 쓴 요청은 하나만 성공한다.
    match state
        .cluster
        .delete::<JoinTokenSpec>(token.id, Some(token.revision))
        .await
    {
        Ok(_) => {}
        Err(ClusterError::Rejected(_)) => return Err(invalid_token()),
        Err(e) => return Err(e.into()),
    }

    // 앞서 합류하다 실패한 같은 노드면 Node 리소스를 다시 쓴다.
    let spec = NodeSpec {
        address: req.address.clone(),
        maintenance: false,
    };
    match state.cluster.get::<NodeSpec>(req.node)? {
        Some(node) => {
            state
                .cluster
                .update_spec(node.id, None, node.labels, &spec)
                .await?;
        }
        None => {
            let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(req.name.clone(), spec);
            node.id = req.node;
            state.cluster.create(&node).await?;
        }
    }
    state
        .cluster
        .request(LeaderRequest::AddLearner {
            id: NodeId(req.node),
            addr: req.cluster_address.clone(),
        })
        .await?;
    tracing::info!(node = %req.name, id = %req.node, "node joined as learner");
    Ok(Json(JoinResponse {
        cluster: info.name,
        ca_pem: info.spec.ca_cert_pem,
        cert_pem,
        via: me,
    }))
}

/// 죽은 노드를 클러스터에서 뺀다. 살아 있는 노드는 그 노드에서 `oplctl cluster leave`로 뺀다.
/// 그 노드에 배정된 VM은 그대로 남는다.
#[utoipa::path(delete, path = "/nodes/{key}", tag = "nodes",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_node(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    if node.id == state.node || node.status.online {
        return Err(ApiError::conflict(
            "node_online",
            format!(
                "node {:?} is online; run `oplctl cluster leave` on it instead",
                node.name
            ),
        ));
    }
    state
        .cluster
        .request(LeaderRequest::Remove {
            id: NodeId(node.id),
        })
        .await?;
    match state.cluster.delete::<NodeSpec>(node.id, None).await {
        Ok(_) | Err(ClusterError::Rejected(_)) => {}
        Err(e) => return Err(e.into()),
    }
    tracing::info!(node = %node.name, "node removed from the cluster");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_url_round_trips() {
        let url = JoinUrl {
            address: "[::1]:8006".into(),
            token: "oplj_abc_def".into(),
            ca_fingerprint: "sha256:00ff".into(),
        };
        let s = url.to_string();
        assert_eq!(s, "opl-join://[::1]:8006?token=oplj_abc_def&ca=sha256:00ff");
        assert_eq!(s.parse::<JoinUrl>().unwrap(), url);
        for bad in [
            "https://h:1?token=a&ca=sha256:00",
            "opl-join://h?token=a&ca=sha256:00",
            "opl-join://h:1?token=a",
            "opl-join://h:1?token=a&ca=md5:00",
            "opl-join://h:1?token=a&ca=sha256:00&x=1",
        ] {
            assert!(bad.parse::<JoinUrl>().is_err(), "{bad}");
        }
    }

    #[test]
    fn host_of_handles_v6_and_names() {
        assert_eq!(host_of("10.0.0.5:8006"), Some("10.0.0.5"));
        assert_eq!(host_of("[::1]:8006"), Some("::1"));
        assert_eq!(host_of("node1.lan:8007"), Some("node1.lan"));
        assert_eq!(host_of("node1.lan"), None);
        assert_eq!(host_of(":8006"), None);
        assert_eq!(host_of("h:port"), None);
    }
}
```

`crates/opl-api/src/lib.rs`: `pub mod auth;` 다음 줄에 `pub mod cluster;`를 더하고, 라우터의 `.routes(routes!(r::get_node, r::update_node))`를 다음으로 바꾼다:

```rust
        .routes(routes!(r::get_node, r::update_node, cluster::delete_node))
        .routes(routes!(cluster::get_cluster))
        .routes(routes!(cluster::get_ca))
        .routes(routes!(cluster::create_join_token))
        .routes(routes!(cluster::join))
```

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p opl-api --all-features`
Expected: PASS. 새 테스트는 `cluster_view_and_ca`, `join_tokens_are_for_admins`, `a_node_joins_once_per_token`, `bad_or_expired_tokens_are_refused`, `only_offline_nodes_are_removed`이다. OpenAPI 문서 테스트도 그대로 통과한다(새 경로가 문서에 들어간다).

- [ ] **Step 5: 커밋**

```bash
git add crates/opl-api
git commit -m "feat(api): 클러스터 정보, 조인 토큰, 노드 합류와 제거"
```

---
### Task 9: `oplctl`: HTTPS, CA 지문, 클러스터 명령

- **HTTPS 로그인:** 처음 로그인할 때 서버의 CA를 인증서 검증 없이 받는다.
  - `--ca-fingerprint`와 맞으면 그 CA를 설정 파일에 저장하고, 그 뒤로는 그 CA로만 서버를 확인한다.
  - 지문을 주지 않았으면 서버의 지문을 알려 주고 실패한다.
  - 같은 서버에 다시 로그인할 때는 저장한 CA를 쓴다. HTTP 서버면 CA가 없다.
- **TLS 구현:** 클라이언트 TLS는 OS 구현(native-tls)이다. reqwest의 rustls 경로가 CDLA-Permissive-2.0 라이선스를 끌어오기 때문이다. 콘솔도 같은 CA로 WSS를 연다.
- **제어 소켓 명령:** `cluster init|join|leave`는 이 기계의 opld 제어 소켓(`--socket`, 기본 `/run/opl/opld.sock`, `OPL_SOCKET`)으로 보내므로 로그인이 필요 없다.
- **API 명령:** `cluster status|join-token|remove-node`는 API로 보낸다. `remove-node`는 `--force`가 있어야 한다.
- **2노드 경고:** `cluster status`는 투표 멤버가 2개면 경고한다.
- **Task 기다리기:** 15분 안에 끝나지 않으면 Task 상태와 노드를 알려 주고 그만둔다.

**Files:**
- Modify: `Cargo.toml`, `crates/oplctl/Cargo.toml`, `crates/oplctl/src/{config,client,console,cli}.rs`

- [ ] **Step 1: 의존성**

루트 `Cargo.toml`의 `reqwest`에 `native-tls` 기능을 더하고(`features = ["json", "stream", "query", "native-tls"]`), `[workspace.dependencies]` 끝에 `native-tls = "0.2.18"`을 더한다.

`crates/oplctl/Cargo.toml` 전체:

```toml
[package]
name = "oplctl"
description = "Command-line client for opLite"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
anyhow.workspace = true
clap.workspace = true
futures-util.workspace = true
hex.workspace = true
reqwest.workspace = true
rpassword.workspace = true
native-tls.workspace = true
rustls-pki-types.workspace = true
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true
tokio.workspace = true
tokio-tungstenite = { workspace = true, features = ["native-tls"] }
tokio-util.workspace = true
uuid.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 2: 설정에 CA 저장 (테스트 먼저)**

`crates/oplctl/src/config.rs`의 `saves_privately_and_loads_back` 테스트에서 `token: Some("opl_x_y".into()),` 다음에 `ca_pem: Some("CA".into()),`를 더한다. `CliConfig`에 필드를 더한다:

```rust
    /// 서버의 클러스터 CA 인증서(PEM). HTTPS 서버는 이 CA로만 확인한다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_pem: Option<String>,
```

Run: `cargo test -p oplctl config`
Expected: PASS

- [ ] **Step 3: 클라이언트**

`crates/oplctl/src/client.rs` 전체:

```rust
//! opLite REST API 클라이언트.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde_json::Value;

/// Task가 이보다 오래 끝나지 않으면 기다리기를 그만둔다. Task 실행기의 제한 시간(10분)보다 길다.
const TASK_WAIT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone)]
pub struct Client {
    pub base: String,
    pub(crate) http: reqwest::Client,
    pub token: Option<String>,
    /// 서버의 클러스터 CA(PEM). 있으면 서버 인증서를 이 CA로만 확인한다(콘솔 WebSocket도).
    pub ca_pem: Option<String>,
}

impl Client {
    pub fn new(base: &str, token: Option<String>, ca_pem: Option<String>) -> anyhow::Result<Self> {
        let mut http = reqwest::Client::builder();
        if let Some(pem) = &ca_pem {
            http = http.tls_certs_only([reqwest::Certificate::from_pem(pem.as_bytes())?]);
        }
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            http: http.build()?,
            token,
            ca_pem,
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base)
    }

    pub fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let r = self.http.request(method, self.url(path));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    /// 성공하면 본문(JSON, 없으면 null), 실패하면 API 오류의 메시지와 코드
    pub async fn send(&self, r: RequestBuilder) -> anyhow::Result<Value> {
        let resp = r
            .send()
            .await
            .with_context(|| format!("cannot reach {}", self.base))?;
        let status = resp.status();
        let text = resp.text().await?;
        let body: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        if status.is_success() {
            return Ok(body);
        }
        if status == StatusCode::UNAUTHORIZED && body["code"] == "unauthorized" {
            bail!("not logged in or the token is no longer valid; run `oplctl login`");
        }
        match (body["message"].as_str(), body["code"].as_str()) {
            (Some(message), Some(code)) => bail!("{message} ({code})"),
            _ => bail!("{status}: {body}"),
        }
    }

    pub async fn get(&self, path: &str) -> anyhow::Result<Value> {
        self.send(self.request(Method::GET, path)).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        self.send(self.request(Method::POST, path).json(body)).await
    }

    pub async fn put(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        self.send(self.request(Method::PUT, path).json(body)).await
    }

    pub async fn delete(&self, path: &str) -> anyhow::Result<Value> {
        self.send(self.request(Method::DELETE, path)).await
    }

    /// 파일을 읽어 가며 본문으로 보낸다(이미지는 수 GB일 수 있다).
    pub async fn upload(
        &self,
        path: &str,
        query: &[(&str, &str)],
        file: &Path,
    ) -> anyhow::Result<Value> {
        let f = tokio::fs::File::open(file)
            .await
            .with_context(|| format!("cannot open {}", file.display()))?;
        let len = f.metadata().await?.len();
        let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(f));
        let r = self
            .request(Method::POST, path)
            .query(query)
            .header(reqwest::header::CONTENT_LENGTH, len)
            .body(body);
        self.send(r).await
    }

    /// Task가 끝날 때까지 기다린다. 실패했으면 그 오류를 돌려준다.
    pub async fn wait_task(&self, task: &Value) -> anyhow::Result<Value> {
        let id = task["id"].as_str().context("response has no task id")?;
        let deadline = Instant::now() + TASK_WAIT;
        loop {
            let task = self.get(&format!("/tasks/{id}")).await?;
            match task["status"]["state"].as_str() {
                Some("succeeded") => return Ok(task),
                Some("failed") => bail!(
                    "task {id} failed: {}",
                    task["status"]["error"].as_str().unwrap_or("unknown error")
                ),
                state if Instant::now() > deadline => bail!(
                    "task {id} did not finish in {} minutes (state: {}); is opld running on node {}?",
                    TASK_WAIT.as_secs() / 60,
                    state.unwrap_or("unknown"),
                    task["spec"]["node"].as_str().unwrap_or("?")
                ),
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }
}

/// 서버의 CA 인증서(PEM)를 인증서 검증 없이 받는다. 받는 쪽은 지문을 확인해야 한다.
pub async fn fetch_ca(server: &str) -> anyhow::Result<String> {
    let http = reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .build()?;
    let url = format!("{}/api/v1/cluster/ca", server.trim_end_matches('/'));
    let resp = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    if !resp.status().is_success() {
        bail!(
            "cannot read the cluster CA from {server}: {}",
            resp.status()
        );
    }
    Ok(resp.text().await?)
}

/// 인증서(PEM)의 SHA-256 지문(`sha256:<16진수>`)
pub fn fingerprint(pem: &str) -> anyhow::Result<String> {
    use rustls_pki_types::CertificateDer;
    use rustls_pki_types::pem::PemObject;
    use sha2::{Digest, Sha256};
    let der = CertificateDer::from_pem_slice(pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("invalid certificate: {e}"))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(&der))))
}

/// 이 기계의 opld 제어 소켓으로 보내는 요청(`oplctl cluster init|join|leave`)
pub async fn local(socket: &Path, path: &str, body: &Value) -> anyhow::Result<Value> {
    let http = reqwest::Client::builder().unix_socket(socket).build()?;
    let resp = http
        .post(format!("http://opld/v1{path}"))
        .json(body)
        .send()
        .await
        .with_context(|| format!("cannot reach opld at {}; is it running?", socket.display()))?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or_default();
    if !status.is_success() {
        bail!("{}", body["message"].as_str().unwrap_or("request failed"));
    }
    Ok(body)
}
```

- [ ] **Step 4: WSS 콘솔**

`crates/oplctl/src/console.rs` 전체:

```rust
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
```

- [ ] **Step 5: 명령**

`crates/oplctl/src/cli.rs` 전체:

```rust
//! 명령 정의와 실행.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{Client, fetch_ca, fingerprint, local};
use crate::config::{CliConfig, default_path};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Parser)]
#[command(name = "oplctl", version, about = "opLite command-line client")]
struct Cli {
    /// 로그인 정보 파일 (기본: ~/.config/opl/oplctl.json)
    #[arg(long, global = true, env = "OPL_CONFIG")]
    config: Option<PathBuf>,
    /// API 주소. 로그인할 때 저장한 주소 대신 쓴다.
    #[arg(long, global = true, env = "OPL_SERVER")]
    server: Option<String>,
    #[arg(short, long, global = true, value_enum, default_value = "table")]
    output: Output,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Output {
    Table,
    Json,
}

#[derive(Subcommand)]
enum Cmd {
    /// 로그인해 API 토큰을 만들고 저장한다.
    Login(LoginArgs),
    /// 저장한 API 토큰을 지운다(서버에서도).
    Logout,
    /// 클러스터 만들기, 노드 합류와 탈퇴, 상태
    Cluster {
        #[command(subcommand)]
        cmd: ClusterCmd,
    },
    Node {
        #[command(subcommand)]
        cmd: NodeCmd,
    },
    Pool {
        #[command(subcommand)]
        cmd: PoolCmd,
    },
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
}

#[derive(Args)]
struct LoginArgs {
    #[arg(long, default_value = "admin")]
    user: String,
    /// 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
    /// HTTPS 서버의 클러스터 CA 지문(`sha256:...`). 처음 로그인할 때 필요하다.
    /// `oplctl cluster init`의 출력이나 다른 노드의 `oplctl cluster status`에서 확인한다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
}

/// 이 기계의 opld에 제어 소켓으로 보내는 명령의 공통 인자
#[derive(Args)]
struct LocalArgs {
    /// opld 제어 소켓
    #[arg(long, env = "OPL_SOCKET", default_value = "/run/opl/opld.sock")]
    socket: PathBuf,
}

#[derive(Subcommand)]
enum ClusterCmd {
    /// 이 노드 하나로 새 클러스터를 만든다. 관리자 비밀번호와 다음 노드의 조인 URL을 출력한다.
    Init {
        /// 클러스터 이름
        #[arg(long)]
        name: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 조인 URL의 클러스터에 더한다.
    Join {
        url: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 클러스터에서 뺀다. 이 노드에 배정된 VM이 없어야 한다.
    Leave {
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 클러스터 이름, CA 지문, 멤버
    Status,
    /// 새 노드를 더할 조인 URL을 만든다. 한 번만 쓸 수 있다.
    JoinToken {
        /// 유효 시간(초)
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
    },
    /// 죽은 노드를 클러스터에서 뺀다. 되돌릴 수 없다.
    RemoveNode {
        node: String,
        /// 확인
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum NodeCmd {
    List,
    Get { node: String },
}

#[derive(Subcommand)]
enum PoolCmd {
    List,
    Get {
        pool: String,
    },
    /// 로컬 디렉터리 풀을 만든다.
    Create {
        name: String,
        /// 풀 디렉터리(절대 경로)
        #[arg(long)]
        path: PathBuf,
        /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
        #[arg(long = "node")]
        nodes: Vec<String>,
    },
    Delete {
        pool: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Qcow2,
    Raw,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Subcommand)]
enum VolumeCmd {
    List,
    Get {
        volume: String,
    },
    Create {
        name: String,
        #[arg(long)]
        pool: String,
        /// 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
        #[arg(long, value_enum, default_value = "qcow2")]
        format: Format,
    },
    /// 이미지 파일(VMDK 등)을 올려 새 볼륨으로 가져온다.
    Import {
        name: String,
        #[arg(long)]
        pool: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, value_enum, default_value = "vmdk")]
        source_format: SourceFormat,
        #[arg(long, value_enum, default_value = "qcow2")]
        format: Format,
    },
    Delete {
        volume: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Machine {
    Virt,
    Q35,
}

#[derive(Clone, Copy, ValueEnum)]
enum Firmware {
    Uefi,
    Bios,
}

#[derive(Clone, Copy, ValueEnum)]
enum Restart {
    Never,
    OnFailure,
}

#[derive(Subcommand)]
enum VmCmd {
    List,
    Get {
        vm: String,
    },
    Create(Box<VmCreate>),
    Start {
        vm: String,
    },
    Stop {
        vm: String,
        /// ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
    Reboot {
        vm: String,
    },
    Delete {
        vm: String,
    },
    /// 로컬 포트에서 VNC 연결을 받아 VM 콘솔로 잇는다. Ctrl-C로 끝낸다.
    Console {
        vm: String,
        /// 0이면 비어 있는 포트를 고른다.
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}

#[derive(Args)]
struct VmCreate {
    name: String,
    #[arg(long, default_value_t = 1)]
    sockets: u16,
    #[arg(long, default_value_t = 1)]
    cores: u16,
    /// 메모리(MiB)
    #[arg(long, default_value_t = 1024)]
    memory: u32,
    /// 기본값은 호스트에 맞춘다(Apple Silicon: virt, x86_64: q35).
    #[arg(long, value_enum)]
    machine: Option<Machine>,
    #[arg(long, value_enum, default_value = "uefi")]
    firmware: Firmware,
    /// 붙일 볼륨 이름. 여러 번 줄 수 있고 첫 디스크로 부팅한다.
    #[arg(long = "disk")]
    disks: Vec<String>,
    /// 연결할 네트워크 이름(지금은 user만). 여러 번 줄 수 있다.
    #[arg(long = "nic")]
    nics: Vec<String>,
    #[arg(long, value_enum, default_value = "never")]
    restart: Restart,
    /// 노드 이름이나 id. 생략하면 시작할 때 스케줄러가 고른다.
    #[arg(long)]
    node: Option<String>,
    /// 만든 뒤 바로 시작하고 실행될 때까지 기다린다.
    #[arg(long)]
    start: bool,
}

#[derive(Subcommand)]
enum TaskCmd {
    List,
    Get { task: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
    Admin,
    Operator,
    Viewer,
}

#[derive(Subcommand)]
enum UserCmd {
    List,
    Create {
        name: String,
        #[arg(long, value_enum)]
        role: Role,
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    /// 비밀번호를 바꾼다.
    Passwd {
        name: String,
        #[arg(long)]
        password_file: Option<PathBuf>,
        /// 자기 비밀번호를 바꿀 때 현재 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
        #[arg(long)]
        current_password_file: Option<PathBuf>,
    },
    Delete {
        name: String,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    List,
    /// 새 토큰을 만들어 출력한다. 다시 볼 수 없다.
    Create {
        name: String,
    },
    Delete {
        token: String,
    },
}

/// `1024`, `512M`, `10G`처럼 쓴다.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, shift) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 10),
        Some('M' | 'm') => (&s[..s.len() - 1], 20),
        Some('G' | 'g') => (&s[..s.len() - 1], 30),
        Some('T' | 't') => (&s[..s.len() - 1], 40),
        _ => (s, 0),
    };
    let n: u64 = num.parse().map_err(|_| format!("invalid size {s:?}"))?;
    n.checked_mul(1 << shift)
        .ok_or_else(|| format!("size {s:?} is too large"))
}

fn read_password(file: Option<&PathBuf>, prompt: &str) -> anyhow::Result<String> {
    match file {
        Some(path) => Ok(std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .trim_end_matches(['\r', '\n'])
            .to_string()),
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}

fn value_name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().unwrap().get_name().to_string()
}

/// 명령을 실행하고 결과를 `out`에 쓴다. 인자 오류는 `clap::Error`로 돌려준다.
pub async fn run<I, T>(args: I, out: &mut (dyn Write + Send)) -> anyhow::Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::try_parse_from(args)?;
    let config_path = cli.config.clone().unwrap_or_else(default_path);
    let mut config = CliConfig::load(&config_path)?;
    let server = cli.server.clone().or(config.server.clone());

    if let Cmd::Login(args) = &cli.cmd {
        let server = server.context("--server is required for the first login")?;
        let same_server = config.server.as_deref().map(|s| s.trim_end_matches('/'))
            == Some(server.trim_end_matches('/'));
        let ca_pem = server_ca(
            &server,
            args,
            same_server.then(|| config.ca_pem.clone()).flatten(),
        )
        .await?;
        let token = login(&server, args, ca_pem.clone()).await?;
        let previous = config.token.take().zip(config.server.take());
        config.server = Some(server.clone());
        config.token = Some(token.clone());
        config.ca_pem = ca_pem.clone();
        config.save(&config_path)?;
        // 새 토큰을 저장한 뒤, 같은 서버에 다시 로그인했으면 이전 토큰은 서버에서도 지운다(실패해도 무시).
        if let Some((old, old_server)) = previous
            && old_server.trim_end_matches('/') == server.trim_end_matches('/')
            && let Some(id) = token_id(&old)
        {
            let client = Client::new(&server, Some(token), ca_pem)?;
            let _ = client.delete(&format!("/tokens/{id}")).await;
        }
        writeln!(out, "logged in to {server} as {}", args.user)?;
        return Ok(());
    }

    if let Cmd::Cluster { cmd } = &cli.cmd
        && let Some(done) = cluster_local(cmd, out).await
    {
        return done;
    }

    let server = server.context("not logged in; run `oplctl login --server <url>`")?;
    let client = Client::new(&server, config.token.clone(), config.ca_pem.clone())?;
    let json_out = cli.output == Output::Json;
    match cli.cmd {
        Cmd::Login(_) => unreachable!(),
        Cmd::Cluster { cmd } => cluster(&client, cmd, json_out, out).await?,
        Cmd::Logout => {
            if let Some(id) = config.token.as_deref().and_then(token_id) {
                // 이미 지워진 토큰이어도 로컬에서는 지운다.
                let _ = client.delete(&format!("/tokens/{id}")).await;
            }
            config.token = None;
            config.save(&config_path)?;
            writeln!(out, "logged out")?;
        }
        Cmd::Node { cmd } => match cmd {
            NodeCmd::List => {
                let list = client.get("/nodes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &[
                        "NAME",
                        "ID",
                        "ADDRESS",
                        "ONLINE",
                        "CPUS",
                        "MEMORY",
                        "MAINTENANCE",
                    ],
                    |n| {
                        vec![
                            cell(&n["name"]),
                            cell(&n["id"]),
                            cell(&n["spec"]["address"]),
                            cell(&n["status"]["online"]),
                            cell(&n["status"]["cpus"]),
                            bytes(&n["status"]["memory_mib"].as_u64().map(|m| m << 20).into()),
                            cell(&n["spec"]["maintenance"]),
                        ]
                    },
                )?;
            }
            NodeCmd::Get { node } => {
                print_json(out, &client.get(&format!("/nodes/{node}")).await?)?
            }
        },
        Cmd::Pool { cmd } => match cmd {
            PoolCmd::List => {
                let list = client.get("/storage-pools").await?;
                print_list(out, json_out, &list, &["NAME", "PATH", "NODES"], |p| {
                    let nodes = p["spec"]["nodes"].as_array().map_or(0, Vec::len);
                    vec![
                        cell(&p["name"]),
                        cell(&p["spec"]["backend"]["path"]),
                        if nodes == 0 {
                            "all".into()
                        } else {
                            nodes.to_string()
                        },
                    ]
                })?;
            }
            PoolCmd::Get { pool } => {
                print_json(out, &client.get(&format!("/storage-pools/{pool}")).await?)?
            }
            PoolCmd::Create { name, path, nodes } => {
                let body = json!({
                    "name": name,
                    "spec": { "backend": { "kind": "dir", "path": path }, "nodes": nodes }
                });
                client.post("/storage-pools", &body).await?;
                writeln!(out, "storage pool {name} created")?;
            }
            PoolCmd::Delete { pool } => {
                client.delete(&format!("/storage-pools/{pool}")).await?;
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Volume { cmd } => match cmd {
            VolumeCmd::List => {
                let list = client.get("/volumes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["NAME", "POOL", "SIZE", "FORMAT", "ALLOCATED"],
                    |v| {
                        vec![
                            cell(&v["name"]),
                            cell(&v["spec"]["pool"]),
                            bytes(&v["spec"]["size_bytes"]),
                            cell(&v["spec"]["format"]),
                            bytes(&v["status"]["allocated_bytes"]),
                        ]
                    },
                )?;
            }
            VolumeCmd::Get { volume } => {
                print_json(out, &client.get(&format!("/volumes/{volume}")).await?)?
            }
            VolumeCmd::Create {
                name,
                pool,
                size,
                format,
            } => {
                let body = json!({
                    "name": name,
                    "spec": { "pool": pool, "size_bytes": size, "format": value_name(format) }
                });
                let task = client.post("/volumes", &body).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} created")?;
            }
            VolumeCmd::Import {
                name,
                pool,
                file,
                source_format,
                format,
            } => {
                let (src, fmt) = (value_name(source_format), value_name(format));
                let query = [
                    ("name", name.as_str()),
                    ("pool", pool.as_str()),
                    ("source_format", src.as_str()),
                    ("format", fmt.as_str()),
                ];
                let task = client.upload("/volumes/import", &query, &file).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} imported")?;
            }
            VolumeCmd::Delete { volume } => {
                let task = client.delete(&format!("/volumes/{volume}")).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} deleted")?;
            }
        },
        Cmd::Vm { cmd } => vm(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List => {
                let list = client.get("/tasks").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["ID", "OP", "TARGET", "STATE", "ERROR"],
                    |t| {
                        vec![
                            cell(&t["id"]),
                            cell(&t["spec"]["op"]["kind"]),
                            cell(&t["spec"]["target"]),
                            cell(&t["status"]["state"]),
                            cell(&t["status"]["error"]),
                        ]
                    },
                )?;
            }
            TaskCmd::Get { task } => {
                print_json(out, &client.get(&format!("/tasks/{task}")).await?)?
            }
        },
        Cmd::User { cmd } => match cmd {
            UserCmd::List => {
                let list = client.get("/users").await?;
                print_list(out, json_out, &list, &["NAME", "ROLE"], |u| {
                    vec![cell(&u["name"]), cell(&u["role"])]
                })?;
            }
            UserCmd::Create {
                name,
                role,
                password_file,
            } => {
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "name": name, "password": password, "role": value_name(role) });
                client.post("/users", &body).await?;
                writeln!(out, "user {name} created")?;
            }
            UserCmd::Passwd {
                name,
                password_file,
                current_password_file,
            } => {
                let me = client.get("/auth/whoami").await?;
                let current = if me["name"] == name.as_str() {
                    Some(read_password(
                        current_password_file.as_ref(),
                        "Current password: ",
                    )?)
                } else {
                    None
                };
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "password": password, "current_password": current });
                client.put(&format!("/users/{name}"), &body).await?;
                writeln!(out, "password of {name} changed")?;
            }
            UserCmd::Delete { name } => {
                client.delete(&format!("/users/{name}")).await?;
                writeln!(out, "user {name} deleted")?;
            }
        },
        Cmd::Token { cmd } => match cmd {
            TokenCmd::List => {
                let list = client.get("/tokens").await?;
                print_list(out, json_out, &list, &["NAME", "ID", "USER"], |t| {
                    vec![cell(&t["name"]), cell(&t["id"]), cell(&t["user"])]
                })?;
            }
            TokenCmd::Create { name } => {
                let created = client.post("/tokens", &json!({ "name": name })).await?;
                writeln!(out, "{}", cell(&created["secret"]))?;
            }
            TokenCmd::Delete { token } => {
                client.delete(&format!("/tokens/{token}")).await?;
                writeln!(out, "token {token} deleted")?;
            }
        },
    }
    Ok(())
}

/// 이 기계의 opld에 보내는 클러스터 명령. 로그인이 필요 없다. 다른 명령이면 `None`이다.
async fn cluster_local(
    cmd: &ClusterCmd,
    out: &mut (dyn Write + Send),
) -> Option<anyhow::Result<()>> {
    let run = async {
        match cmd {
            ClusterCmd::Init { name, local: l } => {
                let r = local(&l.socket, "/init", &json!({ "cluster_name": name })).await?;
                writeln!(out, "cluster {name} created")?;
                writeln!(
                    out,
                    "admin password (shown only once): {}",
                    cell(&r["admin_password"])
                )?;
                writeln!(out, "CA fingerprint: {}", cell(&r["ca_fingerprint"]))?;
                writeln!(
                    out,
                    "join URL for the next node (single use, valid for 1 hour):"
                )?;
                writeln!(out, "  {}", cell(&r["join_url"]))?;
                writeln!(out, "log in with:")?;
                writeln!(
                    out,
                    "  oplctl login --server https://{} --ca-fingerprint {}",
                    cell(&r["api_address"]),
                    cell(&r["ca_fingerprint"])
                )?;
            }
            ClusterCmd::Join { url, local: l } => {
                let r = local(&l.socket, "/join", &json!({ "url": url })).await?;
                writeln!(
                    out,
                    "node {} joined cluster {}",
                    cell(&r["node"]),
                    cell(&r["cluster"])
                )?;
            }
            ClusterCmd::Leave { local: l } => {
                local(&l.socket, "/leave", &json!({})).await?;
                writeln!(out, "this node left the cluster")?;
            }
            _ => return Ok(false),
        }
        anyhow::Ok(true)
    };
    match run.await {
        Ok(false) => None,
        Ok(true) => Some(Ok(())),
        Err(e) => Some(Err(e)),
    }
}

/// 로그인한 서버의 API로 보내는 클러스터 명령
async fn cluster(
    client: &Client,
    cmd: ClusterCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ClusterCmd::Init { .. } | ClusterCmd::Join { .. } | ClusterCmd::Leave { .. } => {
            unreachable!("handled by cluster_local")
        }
        ClusterCmd::Status => {
            let view = client.get("/cluster").await?;
            if json_out {
                print_json(out, &view)?;
                return Ok(());
            }
            writeln!(out, "cluster: {}", cell(&view["name"]))?;
            writeln!(out, "CA fingerprint: {}", cell(&view["ca_fingerprint"]))?;
            let leader = view["leader"].clone();
            let rows: Vec<Vec<String>> = view["members"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|m| {
                    vec![
                        cell(&m["name"]),
                        cell(&m["id"]),
                        cell(&m["address"]),
                        if m["voter"] == true {
                            "voter"
                        } else {
                            "learner"
                        }
                        .into(),
                        cell(&m["online"]),
                        (m["id"] == leader).to_string(),
                    ]
                })
                .collect();
            table(
                out,
                &["NAME", "ID", "ADDRESS", "ROLE", "ONLINE", "LEADER"],
                &rows,
            )?;
            let voters = view["members"]
                .as_array()
                .map_or(0, |m| m.iter().filter(|m| m["voter"] == true).count());
            if voters == 2 {
                writeln!(
                    out,
                    "warning: a two-node cluster stops accepting changes when either node fails; add a third node"
                )?;
            }
        }
        ClusterCmd::JoinToken { ttl } => {
            let created = client
                .post("/cluster/join-tokens", &json!({ "ttl_secs": ttl }))
                .await?;
            writeln!(out, "{}", cell(&created["url"]))?;
        }
        ClusterCmd::RemoveNode { node, force } => {
            if !force {
                bail!("removing a node cannot be undone; pass --force to confirm");
            }
            client.delete(&format!("/nodes/{node}")).await?;
            writeln!(out, "node {node} removed from the cluster")?;
        }
    }
    Ok(())
}

async fn vm(
    client: &Client,
    cmd: VmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        VmCmd::List => {
            let list = client.get("/vms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "POWER", "PHASE", "NODE", "PID", "ERROR"],
                |v| {
                    vec![
                        cell(&v["name"]),
                        cell(&v["spec"]["power"]),
                        cell(&v["status"]["phase"]),
                        cell(&v["status"]["node"]),
                        cell(&v["status"]["pid"]),
                        cell(&v["status"]["last_error"]),
                    ]
                },
            )?;
        }
        VmCmd::Get { vm } => print_json(out, &client.get(&format!("/vms/{vm}")).await?)?,
        VmCmd::Create(args) => {
            let machine = args.machine.unwrap_or(if cfg!(target_arch = "aarch64") {
                Machine::Virt
            } else {
                Machine::Q35
            });
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{key}")).await?["id"].clone()),
                None => None,
            };
            let disks: Vec<Value> = args
                .disks
                .iter()
                .enumerate()
                .map(|(i, volume)| {
                    let boot = (i == 0).then_some(1);
                    json!({ "volume": volume, "bus": "virtio-blk", "boot_order": boot })
                })
                .collect();
            let nics: Vec<Value> = args
                .nics
                .iter()
                .map(|network| json!({ "network": network, "model": "virtio" }))
                .collect();
            let body = json!({
                "name": args.name,
                "spec": {
                    "cpu": { "sockets": args.sockets, "cores": args.cores },
                    "memory_mib": args.memory,
                    "machine": value_name(machine),
                    "firmware": value_name(args.firmware),
                    "disks": disks,
                    "nics": nics,
                    "restart": value_name(args.restart),
                    "node": node,
                }
            });
            client.post("/vms", &body).await?;
            writeln!(out, "vm {} created", args.name)?;
            if args.start {
                act(client, &args.name, "start", out).await?;
            }
        }
        VmCmd::Start { vm } => act(client, &vm, "start", out).await?,
        VmCmd::Stop { vm, force } => {
            act(
                client,
                &vm,
                if force { "stop?mode=force" } else { "stop" },
                out,
            )
            .await?
        }
        VmCmd::Reboot { vm } => act(client, &vm, "reboot", out).await?,
        VmCmd::Delete { vm } => {
            client.delete(&format!("/vms/{vm}")).await?;
            writeln!(out, "vm {vm} deleted")?;
        }
        VmCmd::Console { vm, port } => {
            // 콘솔을 열 수 있는지 먼저 확인한다(없는 VM, 권한).
            client.get(&format!("/vms/{vm}")).await?;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("cannot listen on 127.0.0.1:{port}"))?;
            let port = listener.local_addr()?.port();
            writeln!(
                out,
                "console of {vm}: vnc://127.0.0.1:{port} (Ctrl-C to quit)"
            )?;
            out.flush()?;
            tokio::select! {
                result = crate::console::serve(client.clone(), vm, listener) => result?,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }
    Ok(())
}

/// VM 동작을 요청하고 Task가 끝날 때까지 기다린다.
async fn act(
    client: &Client,
    vm: &str,
    action: &str,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .send(client.request(Method::POST, &format!("/vms/{vm}/actions/{action}")))
        .await?;
    client.wait_task(&task).await?;
    let verb = action.split('?').next().unwrap_or(action);
    writeln!(out, "vm {vm}: {verb} done")?;
    Ok(())
}

fn print_list(
    out: &mut (dyn Write + Send),
    json_out: bool,
    list: &Value,
    headers: &[&str],
    row: impl Fn(&Value) -> Vec<String>,
) -> anyhow::Result<()> {
    let items = list["items"].as_array().cloned().unwrap_or_default();
    if json_out {
        print_json(out, &Value::Array(items))?;
    } else {
        let rows: Vec<_> = items.iter().map(row).collect();
        table(out, headers, &rows)?;
    }
    Ok(())
}

/// 토큰 문자열 `opl_<id>_<secret>`의 id
fn token_id(token: &str) -> Option<uuid::Uuid> {
    let rest = token.strip_prefix("opl_")?;
    uuid::Uuid::try_parse(rest.split_once('_')?.0).ok()
}

/// HTTPS 서버의 클러스터 CA. 처음이면 받아서 `--ca-fingerprint`와 맞는지 확인한다. 같은 서버에
/// 다시 로그인하면 저장한 CA를 그대로 쓴다. HTTP 서버면 없다.
async fn server_ca(
    server: &str,
    args: &LoginArgs,
    saved: Option<String>,
) -> anyhow::Result<Option<String>> {
    if !server.starts_with("https://") {
        return Ok(None);
    }
    if args.ca_fingerprint.is_none()
        && let Some(saved) = saved
    {
        return Ok(Some(saved));
    }
    let pem = fetch_ca(server).await?;
    let actual = fingerprint(&pem)?;
    match &args.ca_fingerprint {
        Some(expected) if expected.eq_ignore_ascii_case(&actual) => Ok(Some(pem)),
        Some(expected) => {
            bail!("the server's cluster CA does not match: expected {expected}, got {actual}")
        }
        None => bail!(
            "the server's cluster CA fingerprint is {actual}; check it (from `oplctl cluster init` \
             or `oplctl cluster status` on a node you trust) and log in again with --ca-fingerprint {actual}"
        ),
    }
}

/// 세션으로 로그인해 이 CLI가 쓸 API 토큰을 만든다. 세션은 바로 닫는다.
async fn login(server: &str, args: &LoginArgs, ca_pem: Option<String>) -> anyhow::Result<String> {
    let password = read_password(args.password_file.as_ref(), "Password: ")?;
    let anon = Client::new(server, None, ca_pem)?;
    let resp = anon
        .http
        .post(anon.url("/auth/login"))
        .json(&json!({ "username": args.user, "password": password }))
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    if !resp.status().is_success() {
        let body: Value = resp.json().await.unwrap_or_default();
        bail!("login failed: {}", cell(&body["message"]));
    }
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .context("server did not return a session")?
        .to_string();
    let name = format!("oplctl-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let with_cookie = |r: reqwest::RequestBuilder| r.header(reqwest::header::COOKIE, &cookie);
    let created = anon
        .send(with_cookie(anon.request(Method::POST, "/tokens")).json(&json!({ "name": name })))
        .await?;
    let _ = anon
        .send(with_cookie(anon.request(Method::POST, "/auth/logout")))
        .await;
    created["secret"]
        .as_str()
        .map(str::to_string)
        .context("server did not return a token")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("512M"), Ok(512 << 20));
        assert_eq!(parse_size("10g"), Ok(10 << 30));
        assert!(parse_size("ten").is_err());
        assert!(parse_size("99999999999T").is_err());
    }

    #[test]
    fn extracts_token_id() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(token_id(&format!("opl_{}_abc", id.simple())), Some(id));
        assert_eq!(token_id("garbage"), None);
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
```

- [ ] **Step 6: 확인**

Run: `cargo test -p oplctl && cargo test -p opld && cargo deny check licenses bans sources`
Expected: PASS. `cli_definition_is_valid`이 새 명령 정의를 확인한다. opld는 아직 M5 흐름(HTTP)이라, HTTP 서버에는 CA 없이 로그인하는 기존 smoke 테스트가 그대로 통과한다. `licenses ok`. Linux에서는 `libssl-dev`와 `pkg-config`가 필요하다(Task 12에서 CI에 더한다).

- [ ] **Step 7: 커밋**

```bash
git add Cargo.toml Cargo.lock crates/oplctl
git commit -m "feat(oplctl): HTTPS와 CA 지문 확인, 클러스터 명령, Task 기다리기 제한 시간"
```

---
### Task 10: `opld`: 미초기화 ↔ 멤버, 제어 소켓, HTTPS

`opld`의 두 상태는 다음과 같다.

- **미초기화:** 노드 인증서(`{data_dir}/pki`)가 없으면 이 상태다. 제어 소켓만 열고 `init`/`join`을 기다린다. 이때 API와 노드 간 포트는 열지 않는다.
- **멤버:** 다음 순서로 띄운다.
  1. 노드 간 gRPC 클라이언트로 저장소를 연다.
  2. 포트 두 개를 묶는다. 이미 쓰이고 있으면 아무것도 띄우기 전에 실패한다.
  3. 하이퍼바이저를 준비한다.
  4. 노드 간 서버를 띄운다.
  5. 처음 멤버가 되는 경우 init/join 기록을 하고 인증서를 저장한다.
  6. reconciler, Task 실행기(고아 정리 뒤), 노드 감시, HTTPS API를 띄운다.

동작 규칙은 다음과 같다.

- 다시 시작한 노드는 과반수가 없어도 VM을 계속 다스려야 하므로, Node 리소스를 뒤에서 될 때까지 기록한다.
- 쓰기 제한 시간은 10초다(선거가 리더 임기 때문에 제한 시간의 두 배쯤 걸린다).
- `leave`가 성공하면 저장소와 인증서를 지우고 미초기화로 돌아간다.
- init/join이 실패하면 남은 클러스터 저장소를 지운다.
- 설정에 `address`가 필수가 되고, `cluster_listen`과 `control_socket`이 생기며, 루프백 제한은 없어진다.
- 첫 관리자 비밀번호 파일은 없어지고, `init`의 출력으로 대신한다.

**Files:**
- Create: `crates/opld/src/{member,join,control}.rs`
- Modify: `crates/opld/Cargo.toml`, `crates/opld/src/{main,config,bootstrap}.rs`, `crates/opld/tests/{common/mod.rs,smoke.rs,e2e.rs}`

- [ ] **Step 1: 하네스와 실패하는 테스트**

`crates/opld/tests/common/mod.rs` 전체. 데몬마다 포트 두 개와 짧은 경로의 제어 소켓을 쓰고, `init`의 출력에서 비밀번호, CA 지문, 조인 URL을 꺼내고, 로그인은 CA 지문으로 한다:

```rust
//! opld 프로세스를 띄우고 oplctl(라이브러리)로 조작하는 하네스.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use tempfile::TempDir;

pub const WAIT: Duration = Duration::from_secs(30);

pub struct Daemon {
    pub name: String,
    pub dir: TempDir,
    pub port: u16,
    pub cluster_port: u16,
    child: Option<Child>,
    /// `cluster init`이 출력한 것
    pub admin_password: Option<String>,
    pub ca_fingerprint: Option<String>,
    pub join_url: Option<String>,
}

/// 비어 있는 포트를 하나 고른다.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Daemon {
    /// 유닉스 소켓 경로 길이 제한 때문에 /tmp 아래 짧은 경로를 쓴다.
    pub fn new(name: &str, extra_config: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("opl")
            .tempdir_in("/tmp")
            .unwrap();
        let port = free_port();
        let cluster_port = free_port();
        let share =
            std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
        let config = format!(
            "name = \"{name}\"\naddress = \"127.0.0.1\"\nlisten = \"127.0.0.1:{port}\"\n\
             cluster_listen = \"127.0.0.1:{cluster_port}\"\ncontrol_socket = \"{sock}\"\n\
             data_dir = \"{data}\"\nrun_dir = \"{run}\"\nqemu_share_dir = \"{share}\"\n{extra_config}",
            sock = dir.path().join("opld.sock").display(),
            data = dir.path().join("data").display(),
            run = dir.path().join("run").display(),
        );
        std::fs::write(dir.path().join("opld.toml"), config).unwrap();
        Self {
            name: name.into(),
            dir,
            port,
            cluster_port,
            child: None,
            admin_password: None,
            ca_fingerprint: None,
            join_url: None,
        }
    }

    pub fn server(&self) -> String {
        format!("https://127.0.0.1:{}", self.port)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    pub fn socket(&self) -> String {
        self.path("opld.sock").display().to_string()
    }

    /// opld를 띄우고 제어 소켓이 응답할 때까지 기다린다. 멤버였던 노드면 API까지 기다린다.
    pub async fn start(&mut self) {
        assert!(self.child.is_none());
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("opld.log"))
            .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_opld"))
            .arg("--config")
            .arg(self.path("opld.toml"))
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        self.child = Some(child);
        let member = self.data_dir().join("pki").exists();
        self.wait_until(|status| status.is_some_and(|s| !member || s["state"] == "member"))
            .await;
        if member {
            self.wait_api().await;
        }
    }

    /// 제어 소켓의 상태(`GET /v1/status`)가 `ready`를 만족할 때까지 기다린다.
    async fn wait_until(&mut self, ready: impl Fn(Option<&serde_json::Value>) -> bool) {
        let http = reqwest::Client::builder()
            .unix_socket(self.path("opld.sock"))
            .build()
            .unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            let status = match http.get("http://opld/v1/status").send().await {
                Ok(resp) => resp.json::<serde_json::Value>().await.ok(),
                Err(_) => None,
            };
            if ready(status.as_ref()) {
                return;
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                panic!("opld exited with {status}:\n{}", self.log());
            }
            assert!(
                Instant::now() < deadline,
                "opld did not get ready:\n{}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// API가 HTTPS로 응답할 때까지 기다린다.
    async fn wait_api(&mut self) {
        let http = reqwest::Client::builder()
            .tls_danger_accept_invalid_certs(true)
            .build()
            .unwrap();
        let url = format!("{}/api/v1/auth/whoami", self.server());
        let deadline = Instant::now() + WAIT;
        loop {
            if let Ok(resp) = http.get(&url).send().await
                && resp.status() == reqwest::StatusCode::UNAUTHORIZED
            {
                return;
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                panic!("opld exited with {status}:\n{}", self.log());
            }
            assert!(
                Instant::now() < deadline,
                "API did not start:\n{}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// `oplctl cluster init`으로 새 클러스터를 만들고 출력(비밀번호, CA 지문, 조인 URL)을 기억한다.
    pub async fn init(&mut self, cluster: &str) -> String {
        let socket = self.socket();
        let out = self
            .ctl(&["cluster", "init", "--name", cluster, "--socket", &socket])
            .await
            .unwrap();
        let field = |prefix: &str| {
            out.lines()
                .find_map(|l| l.trim().strip_prefix(prefix).map(str::to_string))
                .unwrap_or_else(|| panic!("no {prefix:?} in {out}"))
        };
        self.admin_password = Some(field("admin password (shown only once): "));
        self.ca_fingerprint = Some(field("CA fingerprint: "));
        self.join_url = Some(format!("opl-join://{}", field("opl-join://")));
        self.wait_api().await;
        out
    }

    /// `oplctl cluster join`
    pub async fn join(&mut self, url: &str) -> anyhow::Result<String> {
        let socket = self.socket();
        let out = self
            .ctl(&["cluster", "join", url, "--socket", &socket])
            .await?;
        self.wait_api().await;
        Ok(out)
    }

    /// SIGTERM을 보내고 끝나기를 기다린다.
    pub fn stop(&mut self) {
        let mut child = self.child.take().unwrap();
        rustix::process::kill_process(
            rustix::process::Pid::from_child(&child),
            rustix::process::Signal::TERM,
        )
        .unwrap();
        let status = child.wait().unwrap();
        assert!(
            status.success(),
            "opld exited with {status}:\n{}",
            self.log()
        );
    }

    /// kill -9
    pub fn kill(&mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.path("opld.log")).unwrap_or_default()
    }

    /// oplctl 명령을 실행한다. 로그인 정보는 이 데몬의 임시 디렉터리에 둔다.
    pub async fn ctl(&self, args: &[&str]) -> anyhow::Result<String> {
        let config = self.path("oplctl.json");
        let mut argv = vec!["oplctl", "--config", config.to_str().unwrap()];
        argv.extend_from_slice(args);
        let mut out = Vec::new();
        oplctl::run(argv, &mut out).await?;
        Ok(String::from_utf8(out).unwrap())
    }

    /// 이 데몬의 API에 admin으로 로그인한다. 비밀번호와 CA 지문은 `init`한 데몬의 것을 쓴다.
    pub async fn login_with(&self, password: &str, ca_fingerprint: &str) {
        let file = self.path("admin-password");
        std::fs::write(&file, password).unwrap();
        let server = self.server();
        self.ctl(&[
            "login",
            "--server",
            &server,
            "--user",
            "admin",
            "--password-file",
            file.to_str().unwrap(),
            "--ca-fingerprint",
            ca_fingerprint,
        ])
        .await
        .unwrap();
    }

    pub async fn login(&self) {
        self.login_with(
            self.admin_password.as_deref().unwrap(),
            self.ca_fingerprint.as_deref().unwrap(),
        )
        .await;
    }

    /// `oplctl -o json`의 결과
    pub async fn ctl_json(&self, args: &[&str]) -> serde_json::Value {
        let mut argv = vec!["-o", "json"];
        argv.extend_from_slice(args);
        serde_json::from_str(&self.ctl(&argv).await.unwrap()).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 조건이 맞을 때까지 다시 확인한다.
pub async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + WAIT;
    while !check().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
```

`crates/opld/tests/smoke.rs` 전체:

```rust
//! QEMU 없이 opld 하나를 띄워 클러스터를 만들고, HTTPS로 CLI를 쓰고, 다시 띄워도 상태가 남는지 본다.

mod common;

use common::Daemon;

#[tokio::test]
async fn single_node_cluster_serves_cli_over_https_and_survives_restarts() {
    let mut d = Daemon::new("node1", "");
    d.start().await;
    // 아직 클러스터가 없으면 API를 열지 않는다.
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", d.port)).is_err(),
        "API is open before the cluster exists"
    );
    let socket = d.socket();
    let err = d
        .ctl(&["cluster", "leave", "--socket", &socket])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a member"), "{err}");

    let out = d.init("prod").await;
    assert!(out.contains("cluster prod created"), "{out}");
    assert!(
        out.contains("oplctl login --server https://127.0.0.1:"),
        "{out}"
    );
    let err = d
        .ctl(&["cluster", "init", "--name", "other", "--socket", &socket])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already a member"), "{err}");
    // 노드 키는 주인만 읽는다.
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(d.data_dir().join("pki/node.key"))
            .unwrap()
            .permissions(),
    );
    assert_eq!(mode & 0o777, 0o600);

    // CA 지문 없이는 처음 로그인할 수 없다(지문을 알려 준다).
    let password = d.path("admin-password");
    std::fs::write(&password, d.admin_password.as_deref().unwrap()).unwrap();
    let server = d.server();
    let err = d
        .ctl(&[
            "login",
            "--server",
            &server,
            "--password-file",
            password.to_str().unwrap(),
        ])
        .await
        .unwrap_err();
    let fp = d.ca_fingerprint.clone().unwrap();
    assert!(err.to_string().contains(&fp), "{err}");
    let err = d
        .ctl(&[
            "login",
            "--server",
            &server,
            "--password-file",
            password.to_str().unwrap(),
            "--ca-fingerprint",
            "sha256:00",
        ])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("does not match"), "{err}");
    d.login().await;
    // 같은 서버에 다시 로그인하면 저장한 CA를 쓰고, 이전 토큰은 지워진다.
    d.ctl(&[
        "login",
        "--server",
        &server,
        "--password-file",
        password.to_str().unwrap(),
    ])
    .await
    .unwrap();
    let tokens = d.ctl_json(&["token", "list"]).await;
    assert_eq!(tokens.as_array().unwrap().len(), 1, "{tokens}");

    let status = d.ctl(&["cluster", "status"]).await.unwrap();
    assert!(status.contains("cluster: prod"), "{status}");
    assert!(status.contains(&fp), "{status}");
    let nodes = d.ctl_json(&["node", "list"]).await;
    assert_eq!(nodes[0]["name"], "node1");
    assert_eq!(nodes[0]["status"]["online"], true);
    assert_eq!(nodes[0]["spec"]["address"], format!("127.0.0.1:{}", d.port));

    let pool = d.path("pool");
    std::fs::create_dir(&pool).unwrap();
    d.ctl(&["pool", "create", "local", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    d.ctl(&["vm", "create", "web", "--memory", "256", "--nic", "user"])
        .await
        .unwrap();
    let err = d
        .ctl(&["vm", "create", "web", "--memory", "256"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("name_taken"), "{err}");

    d.stop();
    d.start().await;
    // 다시 시작하면 멤버로 바로 뜨고, 저장한 토큰을 계속 쓸 수 있다.
    let vms = d.ctl_json(&["vm", "list"]).await;
    assert_eq!(vms[0]["name"], "web");
    let pools = d.ctl(&["pool", "list"]).await.unwrap();
    assert!(pools.contains("local"), "{pools}");
    // 유일한 투표 멤버는 떠날 수 없다.
    let err = d
        .ctl(&["cluster", "leave", "--socket", &socket])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("only voting member"), "{err}");

    d.ctl(&["logout"]).await.unwrap();
    let err = d.ctl(&["vm", "list"]).await.unwrap_err();
    assert!(err.to_string().contains("oplctl login"), "{err}");
    d.stop();
}

#[tokio::test]
async fn opld_fails_on_taken_port_before_starting_workers() {
    let mut d = Daemon::new("node1", "");
    d.start().await;
    d.init("prod").await;
    d.stop();
    // 하이퍼바이저 드라이버는 run_dir/vm을 만든다. 포트를 먼저 열면 거기까지 가지 않는다.
    std::fs::remove_dir_all(d.path("run/vm")).unwrap();
    let _taken = std::net::TcpListener::bind(("127.0.0.1", d.port)).unwrap();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_opld"))
        .arg("--config")
        .arg(d.path("opld.toml"))
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert!(!d.path("run/vm").exists());
}

#[tokio::test]
async fn a_second_opld_on_the_same_socket_refuses_to_start() {
    let mut d = Daemon::new("node1", "");
    d.start().await;
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_opld"))
        .arg("--config")
        .arg(d.path("opld.toml"))
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    // 처음 opld는 그대로 응답한다.
    d.init("prod").await;
    d.stop();
}
```

`crates/opld/tests/e2e.rs`에서 맨 위 설명의 첫 줄 다음에 `//! API는 HTTPS, 콘솔은 WSS다.`를 더하고, 데몬을 띄우는 부분을 다음으로 바꾼다:

```rust
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("e2e").await;
    d.login().await;
```

Run: `cargo test -p opld --test smoke`
Expected: 실패 — 설정에 모르는 키(`address`)가 있어 opld가 시작하지 못한다

- [ ] **Step 2: 설정**

`crates/opld/Cargo.toml` 전체:

```toml
[package]
name = "opld"
description = "opLite node daemon"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[features]
# 실제 QEMU로 CLI부터 콘솔, kill -9 뒤 재연결까지 확인하는 E2E 테스트. Apple Silicon Mac + brew install qemu가 필요하다.
qemu-it = []

[dependencies]
anyhow.workspace = true
axum.workspace = true
clap.workspace = true
openraft.workspace = true
opl-api.workspace = true
opl-cluster.workspace = true
opl-driver.workspace = true
opl-peer.workspace = true
opl-qemu.workspace = true
opl-reconciler.workspace = true
opl-types.workspace = true
reqwest.workspace = true
serde.workspace = true
serde_json.workspace = true
tokio.workspace = true
toml.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
uuid.workspace = true

[dev-dependencies]
oplctl.workspace = true
reqwest.workspace = true
rustix.workspace = true
serde_json.workspace = true
tempfile.workspace = true
```

`crates/opld/src/config.rs` 전체:

```rust
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// `opld --config <path>`로 읽는 TOML 설정.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 노드 이름. 클러스터 안에서 유일해야 한다.
    pub name: String,
    /// 다른 노드와 클라이언트가 이 노드에 닿는 호스트(IP나 DNS 이름). 노드 인증서에 들어가고,
    /// 클러스터에 알리는 주소(`address:포트`)가 된다. 나중에 바꿀 수 없다.
    pub address: String,
    /// API(HTTPS)를 받을 소켓 주소
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// 노드 간 통신(gRPC over mTLS)을 받을 소켓 주소
    #[serde(default = "default_cluster_listen")]
    pub cluster_listen: SocketAddr,
    /// `oplctl cluster init|join|leave`를 받는 유닉스 소켓. 기본은 `{run_dir}/opld.sock`이다.
    /// opld를 실행한 사용자만 쓸 수 있다(0600).
    pub control_socket: Option<PathBuf>,
    /// 클러스터 저장소, 노드 인증서, VM별 영구 파일, 올린 이미지를 둔다.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// QEMU 소켓과 pidfile을 두는 휘발성 디렉터리. 유닉스 소켓 경로 길이 제한(104바이트) 때문에 짧아야 한다.
    #[serde(default = "default_run_dir")]
    pub run_dir: PathBuf,
    /// Homebrew QEMU의 펌웨어 디렉터리. 지금은 Apple Silicon(hvf)만 지원한다(Linux/KVM은 M7).
    #[serde(default = "default_qemu_share_dir")]
    pub qemu_share_dir: PathBuf,
    #[serde(default = "default_qemu_img")]
    pub qemu_img: PathBuf,
    #[serde(default)]
    pub reconciler: ReconcilerSection,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcilerSection {
    /// ACPI 종료를 요청한 뒤 강제 종료하기까지 기다리는 시간(초)
    #[serde(default = "default_acpi_timeout")]
    pub acpi_timeout_secs: u64,
}

impl Default for ReconcilerSection {
    fn default() -> Self {
        Self {
            acpi_timeout_secs: default_acpi_timeout(),
        }
    }
}

impl ReconcilerSection {
    pub fn acpi_timeout(&self) -> Duration {
        Duration::from_secs(self.acpi_timeout_secs)
    }
}

fn default_listen() -> SocketAddr {
    "0.0.0.0:8006".parse().unwrap()
}
fn default_cluster_listen() -> SocketAddr {
    "0.0.0.0:8007".parse().unwrap()
}
fn default_data_dir() -> PathBuf {
    "/var/lib/opl".into()
}
fn default_run_dir() -> PathBuf {
    "/run/opl".into()
}
fn default_qemu_share_dir() -> PathBuf {
    "/opt/homebrew/share/qemu".into()
}
fn default_qemu_img() -> PathBuf {
    "qemu-img".into()
}
fn default_acpi_timeout() -> u64 {
    60
}

/// `host:port`. IPv6 주소는 대괄호로 감싼다.
fn with_port(host: &str, port: u16) -> String {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(_)) => format!("[{host}]:{port}"),
        _ => format!("{host}:{port}"),
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        let config: Self = toml::from_str(&text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        opl_types::resource::validate_name(&self.name)?;
        let address = &self.address;
        let ok = !address.is_empty()
            && (address.parse::<IpAddr>().is_ok()
                || address
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.'));
        if !ok {
            anyhow::bail!(
                "address must be an IP address or a host name without a port (got {address:?})"
            );
        }
        Ok(())
    }

    /// 클러스터에 알리는 API 주소
    pub fn api_address(&self) -> String {
        with_port(&self.address, self.listen.port())
    }

    /// 클러스터에 알리는 노드 간 통신 주소(Raft 멤버십의 노드 주소)
    pub fn cluster_address(&self) -> String {
        with_port(&self.address, self.cluster_listen.port())
    }

    /// 노드 인증서의 이름: 알리는 호스트와 같은 기계에서 쓰는 이름
    pub fn cert_names(&self) -> Vec<String> {
        let mut names = vec![self.address.clone()];
        for local in ["localhost", "127.0.0.1", "::1"] {
            if self.address != local {
                names.push(local.into());
            }
        }
        names
    }

    pub fn control_socket(&self) -> PathBuf {
        self.control_socket
            .clone()
            .unwrap_or_else(|| self.run_dir.join("opld.sock"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Config {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn fills_defaults() {
        let c = parse("name = \"node1\"\naddress = \"10.0.0.5\"");
        assert_eq!(c.listen, "0.0.0.0:8006".parse().unwrap());
        assert_eq!(c.api_address(), "10.0.0.5:8006");
        assert_eq!(c.cluster_address(), "10.0.0.5:8007");
        assert_eq!(c.data_dir, Path::new("/var/lib/opl"));
        assert_eq!(c.control_socket(), Path::new("/run/opl/opld.sock"));
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(60));
        assert_eq!(
            c.cert_names(),
            ["10.0.0.5", "localhost", "127.0.0.1", "::1"]
        );
    }

    #[test]
    fn rejects_unknown_keys_and_missing_address() {
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\nlisten_addr = \"x\"").is_err()
        );
        assert!(toml::from_str::<Config>("name = \"n\"").is_err());
        let c = parse("name = \"n\"\naddress = \"h\"\n[reconciler]\nacpi_timeout_secs = 5");
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(5));
    }

    #[test]
    fn validates_name_and_address() {
        let with = |name: &str, address: &str| Config {
            name: name.into(),
            address: address.into(),
            ..parse("name = \"n\"\naddress = \"h\"")
        };
        assert!(with("node1", "10.0.0.5").validate().is_ok());
        assert!(with("node1", "node1.lan").validate().is_ok());
        assert!(with("node1", "::1").validate().is_ok());
        assert!(with("Node_1", "10.0.0.5").validate().is_err());
        for bad in ["", "10.0.0.5:8006", "a b", "[::1]"] {
            assert!(with("node1", bad).validate().is_err(), "{bad}");
        }
        assert_eq!(with("n", "::1").api_address(), "[::1]:8006");
    }
}
```

- [ ] **Step 3: 첫 기록과 Node 등록**

`crates/opld/src/bootstrap.rs` 전체(`start_cluster`와 `ensure_admin`이 없어지고 `create_cluster`가 생긴다):

```rust
//! 노드 id, 새 클러스터의 첫 기록(클러스터 정보, 관리자), Node 리소스.

use std::io::Write;
use std::path::Path;

use anyhow::Context;
use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_peer::pki::ClusterCa;
use opl_types::auth::{Role, UserSpec};
use opl_types::cluster::ClusterSpec;
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use uuid::Uuid;

use crate::config::Config;

pub const NODE_ID_FILE: &str = "node-id";
const NODE_ID_TMP_FILE: &str = "node-id.tmp";

/// 처음 시작할 때 만든 노드 id를 계속 쓴다. Raft 노드 id이자 Node 리소스 id다.
pub fn node_id(data_dir: &Path) -> anyhow::Result<Uuid> {
    let path = data_dir.join(NODE_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(s) => Uuid::try_parse(s.trim()).with_context(|| format!("bad {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let id = Uuid::now_v7();
            // 임시 파일에 쓰고 바꿔 넣어, 도중에 죽어도 반쯤 쓴 id가 남지 않게 한다.
            let tmp = data_dir.join(NODE_ID_TMP_FILE);
            let mut file = std::fs::File::create(&tmp)?;
            writeln!(file, "{id}")?;
            file.sync_all()?;
            std::fs::rename(&tmp, &path)?;
            Ok(id)
        }
        Err(e) => Err(e.into()),
    }
}

/// 새 클러스터의 첫 기록: 클러스터 정보(이름, CA), 이 노드, `admin` 사용자. 돌려준 비밀번호는
/// 저장하지 않는다(해시만 남는다).
pub async fn create_cluster(
    cluster: &Cluster,
    config: &Config,
    name: &str,
    ca: &ClusterCa,
) -> anyhow::Result<String> {
    let info = Resource::new(
        name,
        ClusterSpec {
            ca_cert_pem: ca.cert_pem.clone(),
            ca_key_pem: ca.key_pem.clone(),
        },
    );
    cluster.create(&info).await?;
    register_node(cluster, config).await?;
    let password = Uuid::new_v4().simple().to_string();
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: opl_api::hash_password(password.clone())
                .await
                .map_err(|e| anyhow::anyhow!(e.message))?,
            role: Role::Admin,
        },
    );
    cluster.create(&admin).await?;
    Ok(password)
}

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량을 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let address = config.api_address();
    match cluster.get::<NodeSpec>(id)? {
        None => {
            let mut node = Resource::new(
                config.name.clone(),
                NodeSpec {
                    address,
                    maintenance: false,
                },
            );
            node.id = id;
            cluster.create(&node).await?;
        }
        Some(node) if node.spec.address != address => {
            let spec = NodeSpec {
                address,
                ..node.spec
            };
            cluster.update_spec(id, None, node.labels, &spec).await?;
        }
        Some(_) => {}
    }
    let status = NodeStatus {
        online: true,
        cpus: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
        memory_mib: crate::host::memory_mib(),
        hypervisor: "qemu-hvf".into(),
    };
    cluster.update_status::<NodeSpec>(id, &status).await?;
    Ok(())
}

/// 정상 종료할 때 오프라인으로 표시한다. 실패해도 종료는 계속한다.
pub async fn mark_offline(cluster: &Cluster) {
    let NodeId(id) = cluster.id();
    let Ok(Some(node)) = cluster.get::<NodeSpec>(id) else {
        return;
    };
    let status = NodeStatus {
        online: false,
        ..node.status
    };
    if let Err(e) = cluster.update_status::<NodeSpec>(id, &status).await {
        tracing::warn!(error = %e, "cannot mark node offline");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let id = node_id(dir.path()).unwrap();
        assert_eq!(node_id(dir.path()).unwrap(), id);
        std::fs::write(dir.path().join(NODE_ID_FILE), "garbage").unwrap();
        assert!(node_id(dir.path()).is_err());
    }
}
```

- [ ] **Step 4: 멤버로 도는 노드**

`crates/opld/src/member.rs`:

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use openraft::BasicNode;
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{AppState, TaskRunner, TaskRunnerConfig};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{CATCH_UP_TIMEOUT, Cluster, ClusterConfig, ClusterError};
use opl_driver::{DirStorage, Hypervisor, QemuConfig, QemuHypervisor, Storage};
use opl_peer::pki::{ClusterCa, NodeTls, PKI_DIR, fingerprint};
use opl_peer::tls::{TlsListener, api_server_config};
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_qemu::HostProfile;
use opl_reconciler::{Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
use opl_types::vm::VmSpec;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::bootstrap;
use crate::config::Config;

/// 데이터 디렉터리 아래 클러스터 저장소(Raft 로그와 상태)를 두는 디렉터리
pub const CLUSTER_DIR: &str = "cluster";
/// 종료 신호를 받은 뒤 열린 연결(watch, 콘솔)이 끝나기를 기다리는 시간
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// 쓰기가 커밋되기를 기다리는 시간. 리더를 잃으면 새 리더를 뽑는 데 보통 선거 제한 시간의 두 배쯤
/// 걸리므로(openraft는 리더 임기 동안 다른 노드에 투표하지 않는다) 그보다 넉넉히 둔다.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// 다시 시작한 노드가 Node 리소스를 다시 기록하는 간격(과반수가 없으면 될 때까지)
const REGISTER_RETRY: Duration = Duration::from_secs(2);

pub fn pki_dir(config: &Config) -> PathBuf {
    config.data_dir.join(PKI_DIR)
}

pub fn cluster_dir(config: &Config) -> PathBuf {
    config.data_dir.join(CLUSTER_DIR)
}

/// 멤버가 되는 방법
pub enum Bootstrap {
    /// 이미 멤버다(다시 시작).
    Restart,
    /// 이 노드 하나로 새 클러스터를 만든다.
    Init { name: String, ca: ClusterCa },
    /// 합류가 받아들여졌다. `via`(받아 준 노드의 노드 간 주소)로 투표 멤버 승격을 요청한다.
    Join { via: String },
}

/// 새 클러스터를 만든 결과. 사용자에게 한 번만 보여 준다.
pub struct Created {
    pub admin_password: String,
    pub join_url: String,
    pub ca_fingerprint: String,
}

/// 노드에서 노드 간 통신에 쓰는 Raft 설정. 스냅샷 조각은 JSON으로 가므로 작게 둔다.
fn raft_config() -> openraft::Config {
    openraft::Config {
        cluster_name: "opl".into(),
        heartbeat_interval: 200,
        election_timeout_min: 1000,
        election_timeout_max: 2000,
        snapshot_max_chunk_size: 1 << 20,
        ..Default::default()
    }
}

struct Server<E> {
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<(), E>>,
}

pub struct Member {
    config: Config,
    cluster: Arc<Cluster>,
    peer: Server<opl_peer::ServeError>,
    api: Server<std::io::Error>,
    reconciler: Reconciler,
    runner: TaskRunner,
    monitor: NodeMonitor,
    register: JoinHandle<()>,
    /// 서버 태스크가 끝나면 그 이유가 온다.
    exited: mpsc::Receiver<String>,
}

impl Member {
    /// 멤버로서 시작한다. 처음 멤버가 되는 경우(`Init`, `Join`)는 마지막에 노드 인증서를 저장한다.
    /// 인증서가 있으면 멤버로 보므로, 도중에 실패하면 이 노드는 계속 미초기화 상태다.
    pub async fn start(
        config: &Config,
        node: Uuid,
        tls: NodeTls,
        bootstrap: Bootstrap,
    ) -> anyhow::Result<(Self, Option<Created>)> {
        let id = NodeId(node);
        let peers = Arc::new(Peers::new(&tls));
        let mut cluster_config = ClusterConfig::new(id, cluster_dir(config));
        cluster_config.raft = raft_config();
        cluster_config.write_timeout = WRITE_TIMEOUT;
        let cluster = Arc::new(
            Cluster::open(cluster_config, GrpcNetwork::new(peers.clone()))
                .await?
                .with_forwarder(peers.clone()),
        );
        // 포트가 이미 쓰이고 있으면 아무것도 띄우기 전에 실패한다.
        let ports = async {
            let api = TcpListener::bind(config.listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.listen))?;
            let peer = TcpListener::bind(config.cluster_listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.cluster_listen))?;
            anyhow::Ok((api, peer))
        };
        let (api_listener, peer_listener) = match ports.await {
            Ok(listeners) => listeners,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        let hv: Arc<dyn Hypervisor> = match QemuHypervisor::new(QemuConfig {
            host: HostProfile::homebrew_hvf(&config.qemu_share_dir),
            run_dir: config.run_dir.clone(),
            state_dir: config.data_dir.clone(),
        })
        .await
        {
            Ok(hv) => Arc::new(hv),
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e.into());
            }
        };
        let (exited_tx, exited) = mpsc::channel(2);
        let (stop, rx) = oneshot::channel::<()>();
        let service = PeerService::new(cluster.clone(), Some(hv.clone()));
        let peer_tls = tls.clone();
        let tx = exited_tx.clone();
        let task = tokio::spawn(async move {
            let r = opl_peer::serve(peer_listener, &peer_tls, service, async {
                let _ = rx.await;
            })
            .await;
            let _ = tx.send(format!("cluster server stopped: {r:?}")).await;
            r
        });
        let peer = Server { stop, task };

        // 여기서 실패하면 노드 간 서버와 저장소를 닫는다.
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let prepared = async {
            let mut password = None;
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { name, ca } => {
                    let me = BasicNode::new(config.cluster_address());
                    cluster.initialize(BTreeMap::from([(id, me)])).await?;
                    password = Some(bootstrap::create_cluster(&cluster, config, &name, &ca).await?);
                }
                Bootstrap::Join { via } => {
                    promote(&cluster, &peers, &via).await?;
                    bootstrap::register_node(&cluster, config).await?;
                }
            }
            let mut state = AppState::new(
                cluster.clone(),
                node,
                hv.clone(),
                config.data_dir.join("uploads"),
            );
            state.peers = Some(peers.clone());
            state.secure_cookies = true;
            let created = match password {
                Some(admin_password) => {
                    let token = issue_join_token(&state, DEFAULT_JOIN_TTL)
                        .await
                        .map_err(|e| anyhow::anyhow!(e.message))?;
                    Some(Created {
                        admin_password,
                        join_url: token.url,
                        ca_fingerprint: fingerprint(&tls.ca_pem)?,
                    })
                }
                None => None,
            };
            let api_tls = api_server_config(&tls)?;
            if first_time {
                tls.save(&pki_dir(config))?;
            }
            anyhow::Ok((state, created, api_tls))
        };
        let (state, created, api_tls) = match prepared.await {
            Ok(prepared) => prepared,
            Err(e) => {
                let _ = peer.stop.send(());
                let _ = peer.task.await;
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };

        // 다시 시작한 노드는 과반수가 없어도 VM을 계속 다스려야 하므로, Node 리소스는 뒤에서 기록한다.
        let register = if first_time {
            tokio::spawn(async {})
        } else {
            let (cluster, config) = (cluster.clone(), config.clone());
            tokio::spawn(async move {
                while let Err(e) = bootstrap::register_node(&cluster, &config).await {
                    tracing::warn!(error = %e, "cannot register this node; will retry");
                    tokio::time::sleep(REGISTER_RETRY).await;
                }
            })
        };
        let mut reconciler_config = ReconcilerConfig::new(node);
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        let reconciler = Reconciler::spawn(cluster.clone(), hv, reconciler_config);
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img));
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
        let mut runner_config = TaskRunnerConfig::new(node);
        runner_config.upload_dir = Some(config.data_dir.join("uploads"));
        let runner =
            TaskRunner::spawn(cluster.clone(), storage, reconciler.handle(), runner_config).await;
        let monitor = NodeMonitor::spawn(cluster.clone(), peers, NodeMonitorConfig::default());

        let listener = TlsListener::new(api_listener, api_tls)?;
        tracing::info!(
            node = %node,
            api = %config.listen,
            cluster = %config.cluster_listen,
            "opld is ready"
        );
        let (stop, rx) = oneshot::channel::<()>();
        let app = opl_api::router(Arc::new(state));
        let task = tokio::spawn(async move {
            let r = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
            let _ = exited_tx.send(format!("API server stopped: {r:?}")).await;
            r
        });
        let member = Member {
            config: config.clone(),
            cluster,
            peer,
            api: Server { stop, task },
            reconciler,
            runner,
            monitor,
            register,
            exited,
        };
        Ok((member, created))
    }

    pub fn cluster(&self) -> &Cluster {
        &self.cluster
    }

    /// API 서버나 노드 간 서버가 스스로 멈추면 끝난다.
    pub async fn failed(&mut self) -> anyhow::Error {
        match self.exited.recv().await {
            Some(why) => anyhow::anyhow!(why),
            None => std::future::pending().await,
        }
    }

    /// 클러스터에서 빠진다. 이 노드에 배정된 VM이 있거나 이 노드가 유일한 투표 멤버면 거부한다.
    /// 성공하면 `close(true)`로 닫는다.
    pub async fn leave(&self) -> anyhow::Result<()> {
        let me = self.cluster.id();
        let members = self.cluster.members();
        if members.voters.len() == 1 && members.voters.contains_key(&me) {
            anyhow::bail!(
                "this node is the only voting member; stop opld and remove its data directory instead"
            );
        }
        let (_, vms) = self.cluster.list::<VmSpec>()?;
        if let Some(vm) = vms
            .iter()
            .find(|vm| vm.spec.node == Some(me.0) || vm.status.node == Some(me.0))
        {
            anyhow::bail!(
                "VM {:?} is still assigned to this node; delete it or move it to another node first",
                vm.name
            );
        }
        // 멤버십에서 빠지면 다른 노드가 이 노드의 요청을 받지 않으므로 Node 리소스부터 지운다.
        match self.cluster.delete::<NodeSpec>(me.0, None).await {
            Ok(_) | Err(ClusterError::Rejected(_)) => {}
            Err(e) => return Err(e.into()),
        }
        self.cluster
            .request(LeaderRequest::Remove { id: me })
            .await?;
        Ok(())
    }

    /// 모든 것을 멈춘다. `left`면 클러스터에서 빠진 것이므로 클러스터 저장소와 노드 인증서를 지운다.
    /// 아니면 오프라인으로 기록한다. 실행 중인 VM은 그대로 둔다.
    pub async fn close(mut self, left: bool) -> anyhow::Result<()> {
        let _ = self.api.stop.send(());
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.api.task)
            .await
            .is_err()
        {
            self.api.task.abort();
        }
        self.monitor.shutdown().await;
        self.runner.shutdown().await;
        self.reconciler.shutdown().await;
        self.register.abort();
        if !left {
            bootstrap::mark_offline(&self.cluster).await;
        }
        let _ = self.peer.stop.send(());
        let _ = self.peer.task.await;
        self.cluster.shutdown().await?;
        if left {
            std::fs::remove_dir_all(pki_dir(&self.config))?;
            remove_cluster_data(&self.config)?;
        }
        Ok(())
    }
}

/// 클러스터 저장소(Raft 로그와 상태)를 지운다. 없으면 그대로 성공한다.
pub fn remove_cluster_data(config: &Config) -> std::io::Result<()> {
    let dir = cluster_dir(config);
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 합류한 노드가 로그를 따라잡으면 투표 멤버로 올려 달라고 리더에게 요청하고, 이 노드도 그것을
/// 적용할 때까지 기다린다. 요청을 받은 노드가 리더가 아니면 알려 준 리더로 다시 보낸다.
async fn promote(cluster: &Cluster, peers: &Peers, via: &str) -> anyhow::Result<()> {
    let id = cluster.id();
    let deadline = tokio::time::Instant::now() + CATCH_UP_TIMEOUT + Duration::from_secs(30);
    let mut addr = via.to_string();
    loop {
        match peers
            .leader_request(&addr, LeaderRequest::AddVoter { id })
            .await
        {
            Ok(_) => break,
            Err(ClusterError::NotLeader {
                leader_addr: Some(leader),
                ..
            }) => addr = leader,
            Err(e) if tokio::time::Instant::now() < deadline => {
                tracing::info!(error = %e, "waiting to become a voting member");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => return Err(e).context("cannot become a voting member"),
        }
    }
    cluster
        .raft()
        .wait(Some(Duration::from_secs(30)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .voter_ids()
                    .any(|v| v == id)
            },
            "this node is a voter",
        )
        .await?;
    Ok(())
}
```

- [ ] **Step 5: 합류 요청과 제어 소켓**

`crates/opld/src/join.rs`:

```rust
//! 새 노드가 클러스터에 합류를 요청한다.

use anyhow::{Context, bail};
use opl_api::cluster::{JoinRequest, JoinResponse, JoinUrl};
use opl_peer::pki::{NodeTls, fingerprint, node_key_and_csr};
use uuid::Uuid;

use crate::config::Config;

/// 받아들여진 합류
pub struct Joined {
    pub cluster: String,
    pub tls: NodeTls,
    /// 받아 준 노드의 노드 간 주소. 여기로 투표 멤버 승격을 요청한다.
    pub via: String,
}

/// 조인 URL의 노드에 합류를 요청한다. 그 노드의 CA는 인증서 검증 없이 받아 URL의 지문과 맞는지
/// 확인하고, 그 뒤로는 그 CA만 믿는다. 키는 이 노드에서 만들고 CSR만 보낸다.
pub async fn request(config: &Config, node: Uuid, url: &JoinUrl) -> anyhow::Result<Joined> {
    let base = format!("https://{}/api/v1", url.address);
    let insecure = reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .build()?;
    let ca_pem = insecure
        .get(format!("{base}/cluster/ca"))
        .send()
        .await
        .with_context(|| format!("cannot reach {}", url.address))?
        .error_for_status()?
        .text()
        .await?;
    if fingerprint(&ca_pem)? != url.ca_fingerprint {
        bail!(
            "the cluster CA of {} does not match the join URL; check the URL",
            url.address
        );
    }
    let client = reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(ca_pem.as_bytes())?])
        .build()?;
    let (key_pem, csr_pem) = node_key_and_csr()?;
    let resp = client
        .post(format!("{base}/cluster/join"))
        .json(&JoinRequest {
            token: url.token.clone(),
            node,
            name: config.name.clone(),
            address: config.api_address(),
            cluster_address: config.cluster_address(),
            csr_pem,
        })
        .send()
        .await
        .with_context(|| format!("cannot reach {}", url.address))?;
    if !resp.status().is_success() {
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        bail!(
            "join refused: {}",
            body["message"].as_str().unwrap_or("unknown error")
        );
    }
    let joined: JoinResponse = resp.json().await?;
    Ok(Joined {
        cluster: joined.cluster,
        tls: NodeTls {
            ca_pem: joined.ca_pem,
            cert_pem: joined.cert_pem,
            key_pem,
        },
        via: joined.via,
    })
}
```

`crates/opld/src/control.rs`:

```rust
//! 로컬 제어 소켓. `oplctl cluster init|join|leave`가 이 노드의 opld에 요청한다. 소켓은 opld를
//! 실행한 사용자만 쓸 수 있으므로(0600) 따로 인증하지 않는다.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::bail;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

pub type Reply = oneshot::Sender<Result<Value, ControlError>>;

/// 제어 요청. 노드의 상태를 바꾸므로 main 루프가 하나씩 처리한다.
pub enum Request {
    Init { cluster_name: String, reply: Reply },
    Join { url: String, reply: Reply },
    Leave { reply: Reply },
}

#[derive(Debug)]
pub struct ControlError {
    pub status: StatusCode,
    pub message: String,
}

impl ControlError {
    /// 노드의 지금 상태에서는 할 수 없는 요청
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    pub fn failed(e: anyhow::Error) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: format!("{e:#}"),
        }
    }
}

/// `GET /v1/status`의 내용
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub node: Uuid,
    pub name: String,
    /// `uninitialized` | `member`
    pub state: &'static str,
    pub cluster: Option<String>,
}

#[derive(Clone)]
struct Ctx {
    requests: mpsc::Sender<Request>,
    status: Arc<Mutex<Status>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitBody {
    cluster_name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinBody {
    url: String,
}

type Answer = Result<Json<Value>, (StatusCode, Json<Value>)>;

async fn ask(ctx: &Ctx, make: impl FnOnce(Reply) -> Request) -> Answer {
    let (reply, rx) = oneshot::channel();
    let gone = || {
        let body = json!({ "message": "opld is shutting down" });
        (StatusCode::SERVICE_UNAVAILABLE, Json(body))
    };
    ctx.requests.send(make(reply)).await.map_err(|_| gone())?;
    match rx.await.map_err(|_| gone())? {
        Ok(v) => Ok(Json(v)),
        Err(e) => Err((e.status, Json(json!({ "message": e.message })))),
    }
}

fn router(ctx: Ctx) -> Router {
    Router::new()
        .route(
            "/v1/status",
            get(|State(ctx): State<Ctx>| async move {
                Json(serde_json::to_value(ctx.status.lock().unwrap().clone()).unwrap())
            }),
        )
        .route(
            "/v1/init",
            post(
                |State(ctx): State<Ctx>, Json(b): Json<InitBody>| async move {
                    ask(&ctx, |reply| Request::Init {
                        cluster_name: b.cluster_name,
                        reply,
                    })
                    .await
                },
            ),
        )
        .route(
            "/v1/join",
            post(
                |State(ctx): State<Ctx>, Json(b): Json<JoinBody>| async move {
                    ask(&ctx, |reply| Request::Join { url: b.url, reply }).await
                },
            ),
        )
        .route(
            "/v1/leave",
            post(|State(ctx): State<Ctx>| async move {
                ask(&ctx, |reply| Request::Leave { reply }).await
            }),
        )
        .with_state(ctx)
}

/// 제어 소켓을 연다. 다른 opld가 이미 쓰고 있으면 실패한다. 이전 opld가 남긴 소켓 파일은 지운다.
pub async fn serve(
    path: &Path,
    requests: mpsc::Sender<Request>,
    status: Arc<Mutex<Status>>,
) -> anyhow::Result<JoinHandle<()>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        if tokio::net::UnixStream::connect(path).await.is_ok() {
            bail!("{} is in use; is another opld running?", path.display());
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let app = router(Ctx { requests, status });
    Ok(tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "control socket stopped");
        }
    }))
}
```

- [ ] **Step 6: 조립**

`crates/opld/src/main.rs` 전체:

```rust
//! opLite 노드 데몬. 클러스터 저장소, 노드 간 통신, reconciler, Task 실행기, REST API를 한 프로세스로
//! 묶는다.
//!
//! 노드 인증서(`{data_dir}/pki`)가 없으면 아직 클러스터에 속하지 않은 노드다. 이때는 제어 소켓만 열고
//! `oplctl cluster init`(새 클러스터)이나 `oplctl cluster join`(합류)을 기다린다. 멤버가 되면 모든
//! 것을 띄우고, `oplctl cluster leave`로 빠지면 다시 기다린다.
//!
//! QEMU는 `-daemonize`로 떠 있으므로 opld가 멈추거나 죽어도 VM은 계속 돈다. 다시 시작하면
//! 하이퍼바이저 드라이버가 살아 있는 VM에 다시 붙고 reconciler가 이어서 맞춘다.

mod bootstrap;
mod config;
mod control;
mod host;
mod join;
mod member;

use std::path::PathBuf;
use std::pin::pin;
use std::sync::{Arc, Mutex};

use clap::Parser;
use opl_api::cluster::JoinUrl;
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use serde_json::json;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use crate::config::Config;
use crate::control::{ControlError, Request, Status};
use crate::member::{Bootstrap, Member};

#[derive(Parser)]
#[command(version, about = "opLite node daemon")]
struct Args {
    #[arg(long, default_value = "/etc/opl/opld.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,openraft=warn")),
        )
        .init();
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    run(config).await
}

async fn run(config: Config) -> anyhow::Result<()> {
    std::fs::create_dir_all(&config.data_dir)?;
    let node = bootstrap::node_id(&config.data_dir)?;
    let status = Arc::new(Mutex::new(Status {
        node,
        name: config.name.clone(),
        state: "uninitialized",
        cluster: None,
    }));
    let (requests, mut control) = mpsc::channel(4);
    let control_task = control::serve(&config.control_socket(), requests, status.clone()).await?;
    let mut shutdown = pin!(shutdown_signal());

    let result = async {
        loop {
            // 멤버가 아니면 init이나 join을 기다린다.
            let mut member = match NodeTls::load(&member::pki_dir(&config))? {
                Some(tls) => Member::start(&config, node, tls, Bootstrap::Restart).await?.0,
                None => {
                    tracing::info!(
                        socket = %config.control_socket().display(),
                        "this node is not in a cluster; waiting for `oplctl cluster init` or `oplctl cluster join`"
                    );
                    loop {
                        let req = tokio::select! {
                            () = &mut shutdown => return anyhow::Ok(()),
                            req = control.recv() => req,
                        };
                        let Some(req) = req else { return Ok(()) };
                        if let Some(member) = become_member(&config, node, req).await {
                            break member;
                        }
                    }
                }
            };
            let cluster_name = member
                .cluster()
                .list::<opl_types::cluster::ClusterSpec>()
                .ok()
                .and_then(|(_, l)| l.into_iter().next())
                .map(|c| c.name);
            {
                let mut s = status.lock().unwrap();
                s.state = "member";
                s.cluster = cluster_name.clone();
            }
            // 멤버로 돈다.
            let left = loop {
                tokio::select! {
                    () = &mut shutdown => {
                        tracing::info!("shutting down; running VMs keep running");
                        member.close(false).await?;
                        return Ok(());
                    }
                    e = member.failed() => {
                        let _ = member.close(false).await;
                        return Err(e);
                    }
                    req = control.recv() => match req {
                        Some(Request::Leave { reply }) => match member.leave().await {
                            Ok(()) => break reply,
                            Err(e) => {
                                let _ = reply.send(Err(ControlError::failed(e)));
                            }
                        },
                        Some(Request::Init { reply, .. } | Request::Join { reply, .. }) => {
                            let message = format!(
                                "this node is already a member of cluster {:?}",
                                cluster_name.as_deref().unwrap_or("?")
                            );
                            let _ = reply.send(Err(ControlError::conflict(message)));
                        }
                        None => {
                            member.close(false).await?;
                            return Ok(());
                        }
                    },
                }
            };
            let closed = member.close(true).await;
            {
                let mut s = status.lock().unwrap();
                s.state = "uninitialized";
                s.cluster = None;
            }
            match closed {
                Ok(()) => {
                    tracing::info!("left the cluster");
                    let _ = left.send(Ok(json!({})));
                }
                Err(e) => {
                    let _ = left.send(Err(ControlError::failed(e)));
                }
            }
        }
    }
    .await;
    control_task.abort();
    let _ = std::fs::remove_file(config.control_socket());
    result
}

/// 미초기화 노드가 받은 요청을 처리한다. 멤버가 되면 그 멤버를 돌려준다.
async fn become_member(config: &Config, node: Uuid, req: Request) -> Option<Member> {
    let (bootstrap, tls, reply, cluster) = match req {
        Request::Leave { reply } => {
            let _ = reply.send(Err(ControlError::conflict(
                "this node is not a member of a cluster",
            )));
            return None;
        }
        Request::Init {
            cluster_name,
            reply,
        } => match new_cluster_tls(config, node, &cluster_name) {
            Ok((ca, tls)) => (
                Bootstrap::Init {
                    name: cluster_name.clone(),
                    ca,
                },
                tls,
                reply,
                cluster_name,
            ),
            Err(e) => {
                let _ = reply.send(Err(ControlError::failed(e)));
                return None;
            }
        },
        Request::Join { url, reply } => {
            let joined = async {
                let url: JoinUrl = url.parse().map_err(anyhow::Error::msg)?;
                join::request(config, node, &url).await
            };
            match joined.await {
                Ok(joined) => (
                    Bootstrap::Join { via: joined.via },
                    joined.tls,
                    reply,
                    joined.cluster,
                ),
                Err(e) => {
                    let _ = reply.send(Err(ControlError::failed(e)));
                    return None;
                }
            }
        }
    };
    // 인증서가 없는데 클러스터 저장소가 있으면 끝나지 않은 init/join이 남긴 것이다.
    if member::cluster_dir(config).exists() {
        tracing::warn!("removing cluster data left by an unfinished init or join");
        if let Err(e) = member::remove_cluster_data(config) {
            let _ = reply.send(Err(ControlError::failed(e.into())));
            return None;
        }
    }
    match Member::start(config, node, tls, bootstrap).await {
        Ok((member, created)) => {
            let mut answer = json!({ "cluster": cluster, "node": config.name });
            if let Some(c) = created {
                answer["admin_password"] = json!(c.admin_password);
                answer["ca_fingerprint"] = json!(c.ca_fingerprint);
                answer["join_url"] = json!(c.join_url);
                answer["api_address"] = json!(config.api_address());
            }
            let _ = reply.send(Ok(answer));
            Some(member)
        }
        Err(e) => {
            tracing::error!(error = format!("{e:#}"), "cannot become a cluster member");
            let _ = member::remove_cluster_data(config);
            let _ = reply.send(Err(ControlError::failed(e)));
            None
        }
    }
}

/// 새 클러스터의 CA와 이 노드의 인증서
fn new_cluster_tls(
    config: &Config,
    node: Uuid,
    name: &str,
) -> anyhow::Result<(ClusterCa, NodeTls)> {
    opl_types::resource::validate_name(name)?;
    let ca = ClusterCa::generate(name)?;
    let (key_pem, csr) = node_key_and_csr()?;
    let cert_pem = ca.sign_node(&csr, node, &config.cert_names())?;
    let tls = NodeTls {
        ca_pem: ca.cert_pem.clone(),
        cert_pem,
        key_pem,
    };
    Ok((ca, tls))
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}
```

- [ ] **Step 7: 통과 확인**

Run: `cargo test -p opld`
Expected: PASS (설정 단위 테스트, `single_node_cluster_serves_cli_over_https_and_survives_restarts`, `opld_fails_on_taken_port_before_starting_workers`, `a_second_opld_on_the_same_socket_refuses_to_start`)

Run (Apple Silicon + `brew install qemu`): `cargo test -p opld --features qemu-it --test e2e`
Expected: PASS. HTTPS와 WSS 콘솔로 M5 기준을 다시 확인한다. 남은 `qemu-system` 프로세스가 없다.

- [ ] **Step 8: 커밋**

```bash
git add crates/opld
git commit -m "feat(opld): 클러스터 init/join/leave, 제어 소켓, HTTPS API와 노드 간 서버"
```

---
### Task 11: E2E: opld 프로세스 3개로 클러스터 (M6 완료 기준)

QEMU 없이 opld 프로세스 3개를 띄워 다음을 차례로 확인한다(CI에서도 돈다).

1. `cluster init`으로 클러스터를 만든다.
2. 두 노드를 합류시킨다. 하나는 init이 준 URL, 다른 하나는 `cluster join-token`으로 새로 만든 URL을 쓴다. 쓴 URL은 다시 쓸 수 없다.
3. 세 노드 모두 투표 멤버이고 온라인이다.
4. 세 노드 각각을 통해 쓴 것을 한 노드에서 읽는다.
5. 리더를 `kill -9` 한 뒤 남은 노드로 쓴다. 다른 노드에는 조금 늦게 반영될 수 있으므로 기다렸다가 읽는다.
6. `--force` 없는 `remove-node`가 거부되는지 본다.
7. 죽은 노드가 오프라인이 되면 그 노드를 뺀다. 남은 2노드 클러스터에서 상태 경고가 나온다.
8. 남은 노드 하나가 떠난다. 노드 인증서가 지워지고 1인 클러스터가 된다.
9. 떠난 노드가 새 토큰으로 다시 합류하고, 그동안의 기록을 받는다.

**Files:**
- Create: `crates/opld/tests/cluster.rs`

- [ ] **Step 1: 테스트 작성**

```rust
//! M6 완료 기준: opld 프로세스 3개로 클러스터를 만들고, 아무 노드로나 쓰고, 리더가 죽어도 쓰기가
//! 이어지고, 죽은 노드를 빼고, 노드가 떠났다가 다시 합류한다. QEMU는 쓰지 않는다.

mod common;

use common::{Daemon, eventually};
use serde_json::Value;

/// `oplctl cluster status`의 멤버
async fn members(d: &Daemon) -> Vec<Value> {
    d.ctl_json(&["cluster", "status"]).await["members"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

async fn voters(d: &Daemon) -> usize {
    members(d)
        .await
        .iter()
        .filter(|m| m["voter"] == true)
        .count()
}

#[tokio::test]
async fn three_processes_form_a_cluster_that_survives_losing_a_node() {
    let mut nodes = vec![
        Daemon::new("node1", ""),
        Daemon::new("node2", ""),
        Daemon::new("node3", ""),
    ];
    for d in &mut nodes {
        d.start().await;
    }
    nodes[0].init("prod").await;
    let password = nodes[0].admin_password.clone().unwrap();
    let fp = nodes[0].ca_fingerprint.clone().unwrap();
    nodes[0].login().await;

    // node2는 init이 준 URL로, node3은 새로 만든 URL로 합류한다.
    let url = nodes[0].join_url.clone().unwrap();
    let out = nodes[1].join(&url).await.unwrap();
    assert!(out.contains("node node2 joined cluster prod"), "{out}");
    // 쓴 조인 URL은 다시 쓸 수 없다.
    let err = nodes[2].join(&url).await.unwrap_err();
    assert!(err.to_string().contains("invalid or expired"), "{err}");
    let url = nodes[0].ctl(&["cluster", "join-token"]).await.unwrap();
    nodes[2].join(url.trim()).await.unwrap();
    for d in &nodes[1..] {
        d.login_with(&password, &fp).await;
    }
    assert_eq!(voters(&nodes[0]).await, 3);
    eventually("all nodes online", || async {
        members(&nodes[0]).await.iter().all(|m| m["online"] == true)
    })
    .await;

    // 아무 노드로나 쓰고 다른 노드에서 읽는다(팔로워는 리더로 전달한다).
    for (i, d) in nodes.iter().enumerate() {
        let pool = d.path("pool");
        std::fs::create_dir_all(&pool).unwrap();
        d.ctl(&[
            "pool",
            "create",
            &format!("pool{i}"),
            "--path",
            pool.to_str().unwrap(),
        ])
        .await
        .unwrap();
    }
    let pools = nodes[2].ctl_json(&["pool", "list"]).await;
    assert_eq!(pools.as_array().unwrap().len(), 3, "{pools}");

    // 리더를 kill -9 해도 남은 두 노드로 쓰기가 이어진다.
    let view = nodes[0].ctl_json(&["cluster", "status"]).await;
    let leader = view["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == view["leader"])
        .unwrap()["name"]
        .as_str()
        .unwrap()
        .to_string();
    let dead = nodes.iter().position(|d| d.name == leader).unwrap();
    nodes[dead].kill();
    let alive: Vec<usize> = (0..3).filter(|i| *i != dead).collect();
    let (a, b) = (alive[0], alive[1]);
    let pool = nodes[a].path("pool-after");
    std::fs::create_dir_all(&pool).unwrap();
    nodes[a]
        .ctl(&["pool", "create", "after", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    // 다른 노드에는 조금 늦게 반영될 수 있다(각 노드는 자기 상태에서 읽는다).
    eventually("node b sees the pool", || async {
        nodes[b].ctl(&["pool", "get", "after"]).await.is_ok()
    })
    .await;

    // 죽은 노드는 오프라인이 되고, 그 뒤에야 뺄 수 있다.
    let err = nodes[a]
        .ctl(&["cluster", "remove-node", &leader])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("--force"), "{err}");
    eventually("the killed node is offline", || async {
        members(&nodes[a])
            .await
            .iter()
            .any(|m| m["name"] == leader.as_str() && m["online"] == false)
    })
    .await;
    nodes[a]
        .ctl(&["cluster", "remove-node", &leader, "--force"])
        .await
        .unwrap();
    assert_eq!(voters(&nodes[a]).await, 2);
    let status = nodes[a].ctl(&["cluster", "status"]).await.unwrap();
    assert!(status.contains("two-node cluster"), "{status}");

    // 남은 노드 하나가 떠났다가 새 토큰으로 다시 합류한다.
    let socket = nodes[b].socket();
    let out = nodes[b]
        .ctl(&["cluster", "leave", "--socket", &socket])
        .await
        .unwrap();
    assert!(out.contains("left the cluster"), "{out}");
    assert_eq!(voters(&nodes[a]).await, 1);
    assert!(!nodes[b].data_dir().join("pki").exists());
    let url = nodes[a].ctl(&["cluster", "join-token"]).await.unwrap();
    nodes[b].join(url.trim()).await.unwrap();
    assert_eq!(voters(&nodes[a]).await, 2);
    // 다시 합류한 노드도 그동안의 기록을 받는다.
    nodes[b].login_with(&password, &fp).await;
    nodes[b].ctl(&["pool", "get", "after"]).await.unwrap();

    nodes[a].stop();
    nodes[b].stop();
}
```

- [ ] **Step 2: 통과 확인**

Run: `cargo test -p opld --test cluster`
Expected: PASS (30초 안팎. 죽은 노드가 오프라인이 되기까지 10초 남짓 걸린다). 남은 opld 프로세스가 없다(`pgrep opld`).

- [ ] **Step 3: 커밋**

```bash
git add crates/opld/tests/cluster.rs
git commit -m "test(opld): 프로세스 3개로 클러스터 구성, 리더 장애, 제거, 탈퇴·재합류 (M6)"
```

---

### Task 12: 문서, CI

**Files:**
- Modify: `README.md`, `.github/workflows/ci.yml`, `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`

- [ ] **Step 1: CI**

`.github/workflows/ci.yml`의 `check` 작업에서 `- uses: Swatinem/rust-cache@v2` 다음에 더한다(native-tls가 Linux에서 OpenSSL을 쓴다):

```yaml
      - name: OpenSSL headers (Linux; native-tls)
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libssl-dev pkg-config
```

- [ ] **Step 2: README**

`README.md` 전체:

````markdown
# opLite

Rust로 작성하는 온프레미스 가상화·클라우드 플랫폼 (Proxmox VE / VMware vSphere 대안).

- 아키텍처: 대칭형 노드 + 내장 Raft, QEMU/KVM을 QMP로 직접 제어
- 라이선스: Apache-2.0
- 상태: 개발 초기 (M6)

## 실행 (macOS, Apple Silicon)

```bash
brew install qemu
mkdir -p /tmp/opl
cat > /tmp/opl/node1.toml <<'TOML'
name = "node1"
address = "127.0.0.1"          # 다른 노드와 클라이언트가 이 노드에 닿는 호스트
data_dir = "/tmp/opl/node1"
run_dir = "/tmp/opl/run1"
TOML
cargo run -p opld -- --config /tmp/opl/node1.toml &       # 제어 소켓: /tmp/opl/run1/opld.sock
cargo run -p oplctl -- cluster init --name dev --socket /tmp/opl/run1/opld.sock
# 출력된 admin 비밀번호와 CA 지문으로 로그인한다(API: https://127.0.0.1:8006).
cargo run -p oplctl -- login --server https://127.0.0.1:8006 --ca-fingerprint sha256:...
mkdir -p /tmp/opl/pool
cargo run -p oplctl -- pool create local --path /tmp/opl/pool
cargo run -p oplctl -- volume create root --pool local --size 1G
cargo run -p oplctl -- vm create web --memory 512 --disk root --nic user --start
cargo run -p oplctl -- vm console web            # 출력된 vnc:// 주소를 VNC 뷰어로 연다
```

같은 Mac에 노드를 더하려면 포트와 디렉터리를 바꿔 opld를 하나 더 띄우고, 조인 URL로 합류한다.

```bash
cat > /tmp/opl/node2.toml <<'TOML'
name = "node2"
address = "127.0.0.1"
listen = "127.0.0.1:8016"
cluster_listen = "127.0.0.1:8017"
data_dir = "/tmp/opl/node2"
run_dir = "/tmp/opl/run2"
TOML
cargo run -p opld -- --config /tmp/opl/node2.toml &
cargo run -p oplctl -- cluster join "$(cargo run -q -p oplctl -- cluster join-token)" \
    --socket /tmp/opl/run2/opld.sock
cargo run -p oplctl -- cluster status
```

API 문서(OpenAPI)는 `https://127.0.0.1:8006/api/v1/openapi.json`에 있다.

## 개발

```bash
cargo test --workspace                                              # 단위 테스트
brew install qemu                                                   # macOS 통합 테스트용 (Linux: apt install qemu-utils)
cargo test -p opl-driver --features storage-it --test dir_storage   # qemu-img 스토리지 테스트
cargo test -p opl-qemu --features qemu-it --test qemu_boot          # 실제 QEMU 기동 (Apple Silicon)
cargo test -p opl-driver --features qemu-it --test qemu_driver      # 드라이버: 기동, 재부착, 크래시 감지
cargo test -p opl-reconciler --features qemu-it --test qemu_restart # reconciler: opld 재시작 후 재연결
cargo test -p opl-api --features storage-it --test volumes          # API: 볼륨 생성·가져오기 (qemu-img)
cargo test -p opld --features qemu-it --test e2e                    # E2E: CLI → VM → 콘솔 → kill -9 → 재연결
cargo test -p opld --test cluster                                   # E2E: opld 프로세스 3개로 클러스터 구성
```

## 문서

- [1차 하위 프로젝트 설계: 노드 코어](docs/superpowers/specs/2026-10-20-opl-node-core-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
- [계획 2: 하이퍼바이저·스토리지 드라이버](docs/superpowers/plans/2026-10-20-plan2-opl-driver.md)
- [계획 3: 클러스터 저장소](docs/superpowers/plans/2026-10-21-plan3-opl-cluster.md)
- [계획 4: reconciler](docs/superpowers/plans/2026-10-21-plan4-opl-reconciler.md)
- [계획 5: API, opld, oplctl](docs/superpowers/plans/2026-10-21-plan5-api-opld-oplctl.md)
- [계획 6: 클러스터 부트스트랩, 노드 간 통신](docs/superpowers/plans/2026-10-22-plan6-cluster-bootstrap.md)
````

- [ ] **Step 3: 설계 문서**

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`:

1. 2.2절 크레이트 표에서 `opl-cluster` 행을 바꾸고 그 다음에 `opl-peer` 행을 더한다:

```markdown
| `opl-cluster` | openraft 상태 머신, 리소스 저장소, watch 스트림, 리더 전달 | 공개 |
| `opl-peer` | 노드 간 통신(gRPC over mTLS: Raft RPC, 리더 전달, 생존 확인, 콘솔 중계), 클러스터 PKI | 공개 |
```

2. 3.2절 표에서 `Volume` 행의 status를 `준비 상태(Pending/Ready), 할당 크기, 파일을 만드는 노드`로 바꾸고, `User`, `ApiToken` 행 다음에 더한다:

```markdown
| `Cluster` | 클러스터 이름(리소스 이름), CA 인증서와 개인키(API로 내보내지 않는다) | — |
| `JoinToken` | 비밀 값의 SHA-256, 만료 시각 | — |

VM이 붙인 볼륨과 NIC의 MAC 주소는 클러스터 안에서 VM 하나만 쓸 수 있다. 여러 노드의 API가 동시에 써도 겹치지 않도록 상태 머신이 커밋 순서대로 확인한다(`Rejection::InUse`, API는 409).
```

3. 4절에서 콘솔 항목을 다음으로 바꾼다(그 뒤 두 항목은 새로 더한다):

```markdown
- 콘솔: `GET /api/v1/vms/{id}/console` (WebSocket). 실행 노드의 QEMU VNC로 프록시한다. 다른 노드에서 요청하면 노드 간 gRPC로 중계한다.
- 클러스터: `GET /api/v1/cluster`(이름, CA 지문, 리더, 멤버), `GET /api/v1/cluster/ca`(인증 없음), `POST /api/v1/cluster/join-tokens`(관리자), `POST /api/v1/cluster/join`(조인 토큰으로 인증), `DELETE /api/v1/nodes/{id}`(오프라인 노드만).
- 팔로워가 받은 쓰기는 리더로 전달된다. 응답은 그 쓰기가 이 노드에도 적용된 뒤에 오므로, 같은 노드에서 곧바로 읽으면 결과가 보인다.
```

4. 8절 전체를 다음으로 바꾼다:

```markdown
## 8. 클러스터 부트스트랩과 보안

```
node1$ oplctl cluster init --name prod          # 이 노드의 opld에 제어 소켓으로 요청한다
       → 클러스터 CA 생성, node1 인증서 발급, 1인 Raft 클러스터 시작, admin 계정 생성
       → admin 비밀번호(한 번만), CA 지문, 조인 URL 출력: opl-join://node1:8006?token=…&ca=sha256:…

node2$ oplctl cluster join 'opl-join://…'
       → CA 지문 검증 → 토큰으로 CSR 서명 받기 → learner로 합류 → 로그 따라잡은 뒤 voter로 승격
```

- 노드 인증서(`{data_dir}/pki`)가 없는 opld는 클러스터에 속하지 않은 노드다. 제어 소켓(`{run_dir}/opld.sock`, 0600)만 열고 `init`이나 `join`을 기다린다. 인증서는 init/join의 마지막에 저장하므로, 도중에 실패하면 노드는 미초기화로 남고 남은 클러스터 저장소는 다음 시도에서 지운다.
- 조인 토큰은 일회용이고 유효 시간이 정해져 있다(기본 1시간, 최대 7일). Raft에는 해시만 저장한다. 더 필요하면 관리자가 `oplctl cluster join-token`으로 만든다. 합류를 받는 노드는 토큰을 지워 한 번만 쓰게 하고, CSR에서 공개키만 써서 서명한다(이름, 용도, 기간은 서명하는 쪽이 정한다). 노드 인증서의 CN은 노드 id다.
- CA 개인키는 Raft에 복제한다(`Cluster` 리소스). 어느 노드든 합류를 받을 수 있고, 첫 노드가 죽어도 노드를 더할 수 있다. Proxmox(pmxcfs)와 같은 방식이다.
- 노드 간 통신(기본 8007)은 gRPC(tonic) over mTLS(rustls)다. `.proto` 대신 Rust 타입을 serde JSON으로 보내므로 `protoc`가 필요 없다. 서비스는 Raft RPC, 리더 전달, 생존 확인(Ping), 콘솔 중계다. 서버는 상대 인증서의 노드 id가 지금 멤버인지 확인해 제거된 노드를 막는다.
- 외부 API(기본 8006)는 클러스터 CA가 서명한 노드 인증서로 HTTPS를 연다. `oplctl`은 처음 로그인할 때 CA 지문(`--ca-fingerprint`)으로 서버를 확인하고 CA를 저장한다. 클라이언트 쪽 TLS는 OS 구현(native-tls)이다. macOS는 유효 기간이 825일을 넘는 서버 인증서를 거부하므로 노드 인증서는 824일짜리이고, 만료 전에 갱신해야 한다. 사용자 인증서로 교체하는 것은 나중에 한다. 세션 쿠키에는 `Secure`를 붙인다.
- 리더는 2초마다 다른 노드에 Ping을 보내 10초 동안 응답이 없으면 `NodeStatus.online`을 false로 기록한다(바뀔 때만 기록한다). 스케줄러가 이 값을 본다.
- `oplctl cluster leave`(정상 탈퇴)는 이 노드에 배정된 VM이 있거나 이 노드가 유일한 투표 멤버면 거부한다. Node 리소스를 지우고 멤버십에서 빠진 뒤, 클러스터 저장소와 노드 인증서를 지우고 미초기화 상태로 돌아간다. `oplctl cluster remove-node --force`(죽은 노드 제거)는 오프라인 노드만 뺀다. 멤버십 변경은 openraft의 joint consensus로 처리한다.
- 2노드 클러스터는 만들 수 있지만 `oplctl cluster status`가 경고를 표시한다. 위트니스는 하위 프로젝트 5에서 다룬다.
```

5. 10절 표에서 M5 E2E 행 다음에 더한다:

```markdown
| E2E | oplctl로 클러스터 구성 → 아무 노드로 쓰기 → 리더 kill -9 → 노드 제거·탈퇴·재합류 (opld 프로세스 3개) | `cargo test` | macOS, CI (M6) |
```

6. 12절 표에서 노드 간 RPC를 `` `tonic`(수동 서비스 정의, serde JSON 메시지) ``로, TLS/PKI를 `` `rustls`(ring), `rcgen`, 클라이언트는 `native-tls` ``로 바꾼다.

- [ ] **Step 4: 전체 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features && cargo deny check`
Expected: 모두 통과. `--all-features`이므로 실제 QEMU 테스트까지 돈다(Apple Silicon + `brew install qemu`). 남은 `qemu-system`이나 `opld` 프로세스가 없다.

- [ ] **Step 5: 커밋**

```bash
git add README.md .github/workflows/ci.yml docs/superpowers/specs/2026-10-20-opl-node-core-design.md
git commit -m "docs: M6 클러스터 부트스트랩 반영"
```

---
## 실행 (macOS, Apple Silicon)

```bash
brew install qemu
cargo run -p opld -- --config /tmp/opl/node1.toml &        # README의 설정 예
cargo run -p oplctl -- cluster init --name dev --socket /tmp/opl/run1/opld.sock
cargo run -p oplctl -- login --server https://127.0.0.1:8006 --ca-fingerprint sha256:...
cargo run -p oplctl -- cluster join-token                   # 다음 노드의 조인 URL
cargo run -p oplctl -- cluster join '<URL>' --socket /tmp/opl/run2/opld.sock
cargo run -p oplctl -- cluster status
```

## 개발

```bash
cargo test -p opl-peer                                   # 실제 mTLS로 잇는 노드들
cargo test -p opl-api --test two_nodes                   # 다른 노드의 콘솔 중계
cargo test -p opld --test cluster                        # M6: opld 프로세스 3개
cargo test -p opld --features qemu-it --test e2e         # M5 기준을 HTTPS/WSS로
```

## 완료 기준 (M6)

- macOS에서 opld 프로세스 3개로 클러스터를 만들고(init, 합류 둘), 아무 노드로 쓰고, 리더를 `kill -9` 해도 쓰기가 이어지고, 죽은 노드를 빼고, 노드가 떠났다가 다시 합류한다 (`cargo test -p opld --test cluster`).
- 노드 간 통신은 mTLS다. 다른 CA의 노드와 제거된 노드는 연결하거나 부를 수 없다.
- 외부 API는 HTTPS이고, `oplctl`은 CA 지문으로 서버를 확인한다. 실제 QEMU E2E가 HTTPS/WSS로 통과한다.
- `cargo fmt`, `clippy -D warnings`, `cargo test --workspace --all-features`, `cargo deny`가 통과한다.

## 알고 받아들인 제한

- **인증서:**
  - 노드 인증서는 824일 뒤 만료되고, 갱신은 아직 없다.
  - 외부 API를 사용자 인증서로 바꿀 수 없다.
  - 인증서 폐기 목록은 없다. 제거된 노드는 멤버십 확인으로만 막는다.
- **노드 주소:** 노드 주소(`address`, 포트)는 합류 뒤 바꿀 수 없다. 인증서 이름과 Raft 멤버십 주소에 들어간다.
- **전달 재시도:** 리더에 닿지 못해 전달을 다시 보낼 때, 앞 요청이 실제로는 적용되었을 수 있다. 그러면 두 번째 요청이 거부된다(예: `already_exists`).
- **스냅샷 크기:** 스냅샷 조각은 JSON(바이트가 숫자 배열)이라 크다. 설정만 담는 저장소라 받아들인다.
- **CA 개인키:**
  - CA 개인키는 모든 노드의 저장소에 있다. 노드 하나가 털리면 CA도 털린 것으로 본다(Proxmox와 같다).
  - 제어 소켓의 인증은 파일 권한(0600)뿐이다.
- **빠진 노드의 처리:**
  - 제거된 노드에 배정된 VM은 그대로 남는다(HA는 하위 프로젝트 5).
  - 멤버십에서 빠진 노드가 예전 인증서로 다시 시작하면 다른 노드가 받지 않는다. 데이터 디렉터리를 지우고 다시 합류해야 한다.
- **시각과 반영 지연:**
  - 조인 토큰의 만료는 받는 노드의 시계로 본다.
  - 노드 생존은 리더가 보므로, 리더가 바뀌면 처음 10초 동안은 예전 값을 유지한다.
  - 한 노드에 쓴 것은 다른 노드에 조금 늦게 보일 수 있다(각 노드는 자기 상태에서 읽는다). 같은 노드에서는 곧바로 보인다.
- **노드 간 경합:** 볼륨 삭제와 VM에 붙이기의 노드 간 경합은 막지 않는다. 상태 머신은 VM 사이의 중복만 본다.
- **M5 데이터:** M5에서 만든 데이터 디렉터리는 쓸 수 없다. 노드 인증서가 없으므로 `init`이 남은 저장소를 지운다.
- **2노드 경고:** 2노드 경고는 `cluster status`에만 나온다.
- **채널 정리:** 제거된 노드의 gRPC 채널은 프로세스가 끝날 때까지 남는다(주소마다 하나).
