# opLite 계획 22: 위트니스 노드 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** VM을 돌리지 않는 Raft 투표 멤버(위트니스)를 더해, 일반 노드 둘과 위트니스 하나로 한 노드가 죽어도 과반수를 지키는 클러스터를 만든다.

**Architecture:** 위트니스는 opld 설정 `witness = true`로 정하고 합류할 때 Node의 `spec.witness`에 기록한다. 위트니스 opld는 클러스터 저장소·노드 간 서버·승격·Node 등록만 띄우고(QEMU·HTTPS API 없음), openraft `enable_elect = false`로 투표만 하고 리더가 되지 않는다. 스케줄러(`node_fits`)와 풀 규칙(`pool_usable_on`)이 위트니스를 빼므로 VM·볼륨·Task가 가지 않는다. oplctl·웹은 역할을 보이고, 일반 노드가 하나뿐인 클러스터에 경고한다.

**Tech Stack:** Rust 2024(MSRV 1.88), openraft 0.9(`Config::enable_elect`), axum 0.8, utoipa, clap; 웹은 계획 8~21과 같다. 새 외부 의존성은 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md` (§2와 §6·§7의 계획 22 부분, "구현:" 메모 포함)
**선행 계획:** 계획 1~21 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `NodeSpec.witness`, 합류 요청의 역할, 배치·풀·Task 노드 선택에서 빼기 | HA 리스와 자기 펜싱 (계획 23) |
| opld 위트니스 멤버(축소 서비스, `enable_elect = false`), init 거부, 역할 불일치·탈퇴 검사 | 리더의 HA 복구 (계획 24) |
| oplctl `WITNESS` 열과 경고, 웹 배지, 실제 시험(리더를 잃은 뒤 남은 일반 노드가 리더) | 노드 하나에 여러 클러스터의 위트니스, 위트니스의 API |

## 이전 계획에서 이어받는 사항

- 계획 1·2: 클러스터 부트스트랩(init·join·learner 승격·leave), 노드 감시(`NodeMonitor`의 Ping), 제어 소켓, opld 실제 시험 하네스(`crates/opld/tests/common`).
- 계획 15~20: 풀 규칙(`pool_usable_on`, 볼륨 Task 노드 선택), 스케줄러 `node_fits`, 마이그레이션 대상 검사.

## 실행 환경

계획 21과 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상), Node.js 22.12 이상, pnpm 10.33.2. 위트니스 실제 시험은 QEMU가 필요 없어 `cargo test --workspace`에서 돈다. 새 CI 단계는 없다.

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `web/src/api/schema.d.ts`(`pnpm -C web gen:api`).

## 설계 결정

- **위트니스는 Raft 투표 멤버다**(외부 중재자가 아니다). 선거에 나서지 않으므로 리더만 하는 일(감시·정리·예약 스냅샷·HA 복구)은 늘 일반 노드가 한다.
- **역할은 설정에서 오고 합류 때 고정된다.** 다시 시작할 때 저장된 역할과 설정이 다르면 시작하지 않는다(탈퇴 뒤 다시 합류).
- **위트니스는 QEMU도 HTTPS API도 없다.** 노드 간 포트만 열고, 그 노드에서는 제어 소켓 명령(`join`, `leave`)만 쓴다.
- **빼는 규칙은 두 곳에 모은다:** `node_fits`(배치·마이그레이션)와 `pool_usable_on`(디스크·볼륨 Task·복제·풀 검사).
- **리더가 될 수 없는 클러스터를 만들지 않는다:** 남는 투표 멤버가 모두 위트니스가 되는 탈퇴는 거부한다.

## 파일 구조

```
crates/opl-types/src/{node.rs, storage.rs}                          # witness, pool_usable_on
crates/opl-api/src/{cluster.rs, scheduler.rs, vms.rs, resources.rs}  # 합류·배치·409·멤버 표시
crates/opl-cluster/tests/{common/mod.rs, witness.rs}                 # enable_elect = false 시험
crates/opld/src/{config.rs, main.rs, member.rs, bootstrap.rs, join.rs}   # 위트니스 멤버
crates/opld/tests/{common/mod.rs, cluster.rs}                        # 실제 시험
crates/oplctl/src/cli.rs, web/src/{cluster, nodes, pages, storage, vms}/..., web/src/i18n/{ko,en}.json
README.md, docs/superpowers/specs/{2026-11-02-ha-fencing-witness-design.md, 2026-10-20-opl-node-core-design.md}
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 위트니스 노드 타입과 배치에서 빼기

`NodeSpec.witness: bool`(`serde(default)`, OpenAPI에서 필수 아님)과 합류 요청 `JoinRequest.witness`(받는 쪽이 Node에 기록, 다시 합류하면 새 요청의 값). `PUT /nodes/{key}`는 저장된 역할을 유지한다. `scheduler::node_fits`는 위트니스를 맨 먼저 "node {name:?} is a witness node"로 거절하고, `opl_types::storage::pool_usable_on`이 위트니스에 false(`pool_error_on` "witness nodes do not use storage pools")라 스케줄러의 디스크 규칙·볼륨 Task 노드 선택·복제·풀 검사가 함께 위트니스를 뺀다. VM 생성·수정·마이그레이션 대상으로 위트니스를 주면 409 `node_unavailable`(`check_not_witness`). oplctl·웹의 풀 노드 목록, 웹 VM 폼의 노드 목록·기본 machine, 마이그레이션 대상도 위트니스를 뺀다. 모든 `NodeSpec` 리터럴에 `witness: false`.

**Files:**
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/src/janitor.rs`
- Modify: `crates/opl-api/src/resources.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/tests/cluster.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/incoming.rs`
- Modify: `crates/opl-api/tests/janitor.rs`
- Modify: `crates/opl-api/tests/migrate.rs`
- Modify: `crates/opl-api/tests/pools.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-api/tests/snapshots.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-api/tests/vms.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`
- Modify: `crates/opl-types/src/node.rs`
- Modify: `crates/opl-types/src/storage.rs`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/join.rs`
- Modify: `crates/opld/src/pools.rs`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/storage/pools.test.ts`
- Modify: `web/src/storage/pools.ts`
- Modify: `web/src/vms/VmFormPage.test.tsx`
- Modify: `web/src/vms/VmMigrateDialog.test.tsx`
- Modify: `web/src/vms/VmMigrateDialog.tsx`
- Modify: `web/src/vms/form.test.ts`
- Modify: `web/src/vms/form.ts`
- Modify: `web/src/vms/form/GeneralSection.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/cluster.rs`에서

```rust
    pub csr_pem: String,
}
```

을 다음으로 바꾼다:

```rust
    pub csr_pem: String,
    /// 새 노드가 위트니스(투표만 하고 VM을 돌리지 않는 노드)인지
    #[serde(default)]
    pub witness: bool,
}
```

`crates/opl-api/src/cluster.rs`에서

```rust
        maintenance: false,
    };
```

을 다음으로 바꾼다:

```rust
        maintenance: false,
        witness: req.witness,
    };
```

`crates/opl-api/src/janitor.rs`에서

```rust
                maintenance: false,
            },
```

을 다음으로 바꾼다:

```rust
                maintenance: false,
                witness: false,
            },
```

`crates/opl-api/src/resources.rs`에서

```rust
/// 노드의 주소와 유지보수 모드를 바꾼다. 노드 리소스는 opld가 시작할 때 스스로 만든다.
#[utoipa::path(put, path = "/nodes/{key}", tag = "nodes",
```

을 다음으로 바꾼다:

```rust
/// 노드의 주소와 유지보수 모드를 바꾼다. 노드 리소스는 opld가 시작할 때 스스로 만든다.
/// 역할(`witness`)은 합류할 때 정해지므로 요청의 값을 무시하고 그대로 둔다.
#[utoipa::path(put, path = "/nodes/{key}", tag = "nodes",
```

`crates/opl-api/src/resources.rs`에서

```rust
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    state
        .cluster
        .update_spec(node.id, if_match(&headers)?, req.labels, &req.spec)
        .await?;
```

을 다음으로 바꾼다:

```rust
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    let spec = NodeSpec {
        witness: node.spec.witness,
        ..req.spec
    };
    state
        .cluster
        .update_spec(node.id, if_match(&headers)?, req.labels, &spec)
        .await?;
```

`crates/opl-api/src/scheduler.rs`에서

```rust
#[error(
    "no node can run this VM (needs an online node that is not in maintenance, has the architecture of its machine type, can reach its storage pools and has the bridges of its networks)"
)]
```

을 다음으로 바꾼다:

```rust
#[error(
    "no node can run this VM (needs an online node that is not a witness or in maintenance, has the architecture of its machine type, can reach its storage pools and has the bridges of its networks)"
)]
```

`crates/opl-api/src/scheduler.rs`에서

```rust

/// 온라인이고 유지보수 중이 아니며 머신 타입의 아키텍처이고 디스크의 풀을 쓸 수 있고 네트워크의 브리지가 있는 노드
/// 가운데 미할당 메모리가 가장 많은 노드를 고른다. 같으면 id가 작은 노드다. 메모리가 모자라도 고른다(오버커밋 정책은
```

을 다음으로 바꾼다:

```rust

/// 위트니스가 아니고 온라인이고 유지보수 중이 아니며 머신 타입의 아키텍처이고 디스크의 풀을 쓸 수 있고 네트워크의 브리지가 있는 노드
/// 가운데 미할당 메모리가 가장 많은 노드를 고른다. 같으면 id가 작은 노드다. 메모리가 모자라도 고른다(오버커밋 정책은
```

`crates/opl-api/src/scheduler.rs`에서

```rust
    let name = &node.name;
    if !node.status.online {
```

을 다음으로 바꾼다:

```rust
    let name = &node.name;
    // 위트니스는 투표만 하는 노드다. 다른 조건과 상관없이 VM을 돌리지 않는다.
    if node.spec.witness {
        return Err(format!("node {name:?} is a witness node"));
    }
    if !node.status.online {
```

`crates/opl-api/src/scheduler.rs`에서

```rust
                maintenance: false,
            },
```

을 다음으로 바꾼다:

```rust
                maintenance: false,
                witness: false,
            },
```

`crates/opl-api/src/scheduler.rs`에서

```rust
        assert_eq!(schedule(Uuid::now_v7(), &me, &empty), Err(NoNode));
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(schedule(Uuid::now_v7(), &me, &empty), Err(NoNode));
    }

    #[test]
    fn skips_witness_nodes() {
        // 위트니스는 메모리가 많고 다른 조건을 갖춰도 VM을 돌리지 않는다.
        let mut witness = node(64000);
        witness.spec.witness = true;
        let small = node(1024);
        let mut inv = Inventory {
            nodes: vec![witness, small.clone()],
            ..Default::default()
        };
        let me = spec(512, None, PowerState::Running);
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(small.id));
        inv.nodes.pop();
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Err(NoNode));
    }
```

`crates/opl-api/src/scheduler.rs`에서

```rust
        );
        let mut x86 = ready.clone();
```

을 다음으로 바꾼다:

```rust
        );
        // 위트니스는 다른 조건보다 먼저 거절한다.
        let mut witness = offline.clone();
        witness.spec.witness = true;
        assert_eq!(
            node_fits(&me, &inv, &witness),
            Err("node \"n\" is a witness node".into())
        );
        let mut x86 = ready.clone();
```

`crates/opl-api/src/vms.rs`에서

```rust
        };
        check_node_arch(node, spec)?;
```

을 다음으로 바꾼다:

```rust
        };
        check_not_witness(node)?;
        check_node_arch(node, spec)?;
```

`crates/opl-api/src/vms.rs`에서

```rust
        );
    }
```

을 다음으로 바꾼다:

```rust
        );
    }
    Ok(())
}

/// 위트니스 노드면 409 `node_unavailable`. 위트니스는 VM을 돌리지 않는다.
fn check_not_witness(node: &Node) -> Result<(), ApiError> {
    if node.spec.witness {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!("node {:?} is a witness node and cannot run VMs", node.name),
        ));
    }
```

`crates/opl-api/src/vms.rs`에서

```rust
/// 한다. 디스크가 모두 공유 풀에 있어야 하고, HVF(macOS) 노드는 원본도 대상도 될 수 없다(409
/// `migration_unsupported`). 대상 노드가 오프라인이거나 유지보수 중이면 409 `node_unavailable`,
/// 스케줄러가 고를 수 없는 노드(풀, 네트워크, 남은 메모리)면 409 `no_capacity`다.
```

을 다음으로 바꾼다:

```rust
/// 한다. 디스크가 모두 공유 풀에 있어야 하고, HVF(macOS) 노드는 원본도 대상도 될 수 없다(409
/// `migration_unsupported`). 대상 노드가 위트니스이거나 오프라인이거나 유지보수 중이면 409 `node_unavailable`,
/// 스케줄러가 고를 수 없는 노드(풀, 네트워크, 남은 메모리)면 409 `no_capacity`다.
```

`crates/opl-api/src/vms.rs`에서

```rust
    }
    if !target.status.online || target.spec.maintenance {
```

을 다음으로 바꾼다:

```rust
    }
    check_not_witness(&target)?;
    if !target.status.online || target.spec.maintenance {
```

`crates/opl-api/tests/cluster.rs`에서

```rust
            csr_pem,
        },
```

을 다음으로 바꾼다:

```rust
            csr_pem,
            witness: false,
        },
```

`crates/opl-api/tests/cluster.rs`에서

```rust
    assert!(joined.status.online);
}
```

을 다음으로 바꾼다:

```rust
    assert!(joined.status.online);
}

/// 합류 요청의 `witness`가 Node의 역할이 된다. 적지 않으면 일반 노드다. 역할은 노드를 고쳐도
/// 바뀌지 않는다.
#[tokio::test]
async fn a_witness_joins_as_a_witness() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let (mut req, witness) = join_request(&new_token(&env).await, "w");
    req.witness = true;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let joined = env.cluster.get::<NodeSpec>(witness).unwrap().unwrap();
    assert!(joined.spec.witness);
    assert!(joined.status.online);

    let (req, node) = join_request(&new_token(&env).await, "node2");
    let mut body = json!(req);
    body.as_object_mut().unwrap().remove("witness");
    let (status, body) = anon.post("/cluster/join", body).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !env.cluster
            .get::<NodeSpec>(node)
            .unwrap()
            .unwrap()
            .spec
            .witness
    );

    let admin = env.admin().await;
    for spec in [
        json!({ "address": "10.0.0.7:8006", "maintenance": true }),
        json!({ "address": "10.0.0.7:8006", "witness": false }),
    ] {
        let (status, body) = admin
            .put(&format!("/nodes/{witness}"), json!({ "spec": spec }), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["spec"]["witness"], true, "{body}");
    }
    let (status, body) = admin
        .put(
            &format!("/nodes/{node}"),
            json!({ "spec": { "address": "10.0.0.7:8006", "witness": true } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["witness"], false, "{body}");
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
                maintenance: false,
            },
```

을 다음으로 바꾼다:

```rust
                maintenance: false,
                witness: false,
            },
```

`crates/opl-api/tests/incoming.rs`에서

```rust
            maintenance: false,
        },
```

을 다음으로 바꾼다:

```rust
            maintenance: false,
            witness: false,
        },
```

`crates/opl-api/tests/janitor.rs`에서

```rust
            maintenance: false,
        },
```

을 다음으로 바꾼다:

```rust
            maintenance: false,
            witness: false,
        },
```

`crates/opl-api/tests/migrate.rs`에서

```rust
            maintenance: false,
        },
```

을 다음으로 바꾼다:

```rust
            maintenance: false,
            witness: false,
        },
```

`crates/opl-api/tests/migrate.rs`에서

```rust
    edit_node(&env, node2, |n| n.spec.maintenance = false).await;
```

을 다음으로 바꾼다:

```rust
    edit_node(&env, node2, |n| n.spec.maintenance = false).await;
    // 위트니스는 VM을 받지 않는다.
    edit_node(&env, node2, |n| n.spec.witness = true).await;
    let (status, body) = migrate(&admin, "web", "node2").await;
    assert_eq!(
        body["message"], "node \"node2\" is a witness node and cannot run VMs",
        "{body}"
    );
    assert_conflict((status, body), "node_unavailable");
    edit_node(&env, node2, |n| n.spec.witness = false).await;
```

`crates/opl-api/tests/pools.rs`에서

```rust
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
        },
    );
    low.id = Uuid::nil();
```

을 다음으로 바꾼다:

```rust
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
        },
    );
    low.id = Uuid::nil();
```

`crates/opl-api/tests/pools.rs`에서

```rust
    assert_eq!(task["spec"]["node"], low, "{task}");
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(task["spec"]["node"], low, "{task}");
}

/// 위트니스는 볼륨 Task를 맡지 않는다. id가 가장 작고 풀이 준비됐다고 알렸어도, 상태를 알리지 않아도
/// 되는 디렉터리 풀이라도 고르지 않는다.
#[tokio::test]
async fn volume_tasks_never_go_to_a_witness() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let backends = [
        ("ceph", json!({ "kind": "rbd", "pool": "rbd" })),
        ("local", json!({ "kind": "dir", "path": env.dir.path() })),
    ];
    for (name, backend) in &backends {
        let (status, body) = admin
            .post("/storage-pools", pool(name, backend.clone()))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    env.pool_ready("ceph").await;
    let mut witness: Node = Resource::new(
        "w",
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: true,
        },
    );
    witness.id = Uuid::nil();
    env.cluster.create(&witness).await.unwrap();
    witness.status.online = true;
    env.cluster
        .update_status::<NodeSpec>(witness.id, &witness.status)
        .await
        .unwrap();
    common::mark_pool_ready(&env.cluster, witness.id, "ceph").await;

    let create = |name: &str, pool: &str| json!({ "name": name, "spec": { "pool": pool, "size_bytes": 1 << 20, "format": "raw" } });
    let (status, task) = admin.post("/volumes", create("r", "ceph")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string(), "{task}");

    // 이 노드에서 디렉터리 풀이 준비되지 않았으면 맡을 노드가 없다.
    let state = PoolState {
        ready: false,
        error: Some("no such directory".into()),
        ..Default::default()
    };
    common::set_pool_state(&env.cluster, env.node, "local", state).await;
    let (status, body) = admin.post("/volumes", create("d", "local")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_unavailable");
}
```

`crates/opl-api/tests/pools.rs`에서

```rust
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
        },
    );
    node.status.online = online;
```

을 다음으로 바꾼다:

```rust
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
        },
    );
    node.status.online = online;
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust
            maintenance: false,
        },
```

을 다음으로 바꾼다:

```rust
            maintenance: false,
            witness: false,
        },
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
            maintenance: false,
        },
```

을 다음으로 바꾼다:

```rust
            maintenance: false,
            witness: false,
        },
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
                maintenance: false,
            },
```

을 다음으로 바꾼다:

```rust
                maintenance: false,
                witness: false,
            },
```

`crates/opl-api/tests/vms.rs`에서

```rust
use opl_driver::DriverError;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
use opl_driver::DriverError;
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Resource;
```

`crates/opl-api/tests/vms.rs`에서

```rust
}

#[tokio::test]
async fn update_checks_if_match_revision() {
```

을 다음으로 바꾼다:

```rust
}

/// 위트니스는 VM을 돌리지 않는다. VM의 노드로 지정하면 만들 때도 고칠 때도 409다.
#[tokio::test]
async fn a_witness_cannot_hold_vms() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut witness: Node = Resource::new(
        "w",
        NodeSpec {
            address: "127.0.0.2:0".into(),
            maintenance: false,
            witness: true,
        },
    );
    witness.status.online = true;
    witness.status.arch = Some(Arch::Aarch64);
    env.cluster.create(&witness).await.unwrap();

    let mut req = vm_request("web");
    req["spec"]["node"] = json!(witness.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");
    assert_eq!(
        body["message"],
        "node \"w\" is a witness node and cannot run VMs"
    );

    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let mut spec = vm["spec"].clone();
    spec["node"] = json!(witness.id);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");

    // 노드를 정하지 않고 켜면 위트니스가 아닌 이 노드로 간다.
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
}

#[tokio::test]
async fn update_checks_if_match_revision() {
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
            },
        );
        r.id = node.id().0;
        // 하나는 오프라인으로 시작해, 응답하면 온라인이 되는지 본다.
```

을 다음으로 바꾼다:

```rust
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
            },
        );
        r.id = node.id().0;
        // 하나는 오프라인으로 시작해, 응답하면 온라인이 되는지 본다.
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
            },
        );
        r.id = node.id().0;
        r.status.online = true;
```

을 다음으로 바꾼다:

```rust
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
            },
        );
        r.id = node.id().0;
        r.status.online = true;
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
        "n1",
        NodeSpec {
            address: "a".into(),
            maintenance: false,
        },
    );
    me.id = env.node;
    let unsupported = PoolState {
```

을 다음으로 바꾼다:

```rust
        "n1",
        NodeSpec {
            address: "a".into(),
            maintenance: false,
            witness: false,
        },
    );
    me.id = env.node;
    let unsupported = PoolState {
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
        "n1",
        NodeSpec {
            address: "a".into(),
            maintenance: false,
        },
    );
    me.id = env.node;
    let down = PoolState {
```

을 다음으로 바꾼다:

```rust
        "n1",
        NodeSpec {
            address: "a".into(),
            maintenance: false,
            witness: false,
        },
    );
    me.id = env.node;
    let down = PoolState {
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
            maintenance: false,
        },
```

을 다음으로 바꾼다:

```rust
            maintenance: false,
            witness: false,
        },
```

`crates/opl-types/src/node.rs`에서

```rust
    pub maintenance: bool,
}
```

을 다음으로 바꾼다:

```rust
    pub maintenance: bool,
    /// 위트니스 노드: Raft 투표만 하고 VM을 돌리지 않는다. 합류할 때 정해지고 바뀌지 않는다.
    #[serde(default)]
    pub witness: bool,
}
```

`crates/opl-types/src/node.rs`에서

```rust
        assert!(!spec.maintenance);
        let status: NodeStatus = serde_json::from_value(serde_json::json!({})).unwrap();
```

을 다음으로 바꾼다:

```rust
        assert!(!spec.maintenance);
        assert!(!spec.witness);
        let witness: NodeSpec = serde_json::from_value(
            serde_json::json!({ "address": "10.0.0.1:8006", "witness": true }),
        )
        .unwrap();
        assert!(witness.witness);
        let status: NodeStatus = serde_json::from_value(serde_json::json!({})).unwrap();
```

`crates/opl-types/src/storage.rs` 전체:

```rust
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::node::{Node, PoolState};
use crate::resource::{Kind, Resource, ResourceSpec};
use crate::runtime::ImageFormat;

pub type StoragePool = Resource<StoragePoolSpec, StoragePoolStatus>;
pub type Volume = Resource<VolumeSpec, VolumeStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct StoragePoolSpec {
    pub backend: PoolBackend,
    /// 이 풀을 쓸 수 있는 노드. 비어 있으면 모든 노드.
    #[serde(default)]
    pub nodes: Vec<Uuid>,
    /// 모든 노드가 같은 데이터를 보는 공유 스토리지인지. NFS·RBD·공유 LVM 풀은 늘 공유다(`is_shared`).
    #[serde(default)]
    pub shared: bool,
}

impl ResourceSpec for StoragePoolSpec {
    const KIND: Kind = Kind::StoragePool;
    type Status = StoragePoolStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum PoolBackend {
    /// 로컬 디렉터리에 이미지 파일을 둔다.
    Dir {
        #[cfg_attr(feature = "openapi", schema(value_type = String))]
        path: PathBuf,
    },
    /// NFS 내보내기를 노드마다 마운트하고 그 안에 이미지 파일을 둔다. 늘 공유 풀이다.
    Nfs {
        /// 호스트 이름 또는 IP. IPv6는 `[...]` 없이 적는다.
        server: String,
        /// 서버의 내보내기 경로(절대 경로)
        export: String,
        /// `mount -o`에 넘길 쉼표 목록(예: `vers=4.2,hard`). 늘 붙는 `nosuid,nodev,noexec,retry=0` 뒤에
        /// 붙고, 마운트의 성격을 바꾸는 옵션(`bind`, `remount`, `exec` 등)은 받지 않는다. 없으면 커널
        /// 기본값.
        #[serde(default)]
        options: Option<String>,
    },
    /// 노드의 LVM thin pool에 볼륨마다 thin LV(`opl-{id}`, raw)를 둔다. 노드 로컬 풀이고, VG와 thin
    /// pool은 관리자가 노드마다 같은 이름으로 만들어 둔다.
    LvmThin {
        /// 볼륨 그룹 이름
        vg: String,
        /// VG 안의 thin pool LV 이름
        thinpool: String,
    },
    /// 노드의 ZFS 데이터셋 아래에 볼륨마다 sparse zvol(`{dataset}/opl-{id}`, raw)을 둔다. 노드 로컬
    /// 풀이고, 데이터셋은 관리자가 노드마다 같은 이름으로 만들어 둔다.
    Zfs {
        /// zvol을 둘 데이터셋 이름(예: `tank/opl`)
        dataset: String,
    },
    /// Ceph RBD 풀에 볼륨마다 raw 이미지(`opl-{id}`)를 둔다. 모든 노드가 같은 이미지를 보는 공유
    /// 풀이고, Ceph 클러스터와 노드의 ceph.conf·키링은 관리자가 준비해 둔다.
    Rbd {
        /// Ceph 풀 이름
        pool: String,
        /// 풀 안의 RBD 네임스페이스. 없으면 기본 네임스페이스.
        #[serde(default)]
        namespace: Option<String>,
        /// Ceph 클라이언트 id(`client.` 없이). 없으면 `admin`.
        #[serde(default)]
        user: Option<String>,
        /// ceph.conf 절대 경로. 없으면 Ceph 기본(`/etc/ceph/ceph.conf`).
        #[serde(default)]
        conf: Option<String>,
    },
    /// 여러 노드가 함께 보는 LUN 위의 공유 VG(lvmlockd)에 볼륨마다 두꺼운 LV(`opl-{id}`, raw)를 둔다.
    /// 늘 공유 풀이고, 공유 VG와 lvmlockd·sanlock은 관리자가 준비해 둔다.
    LvmShared {
        /// 공유 볼륨 그룹 이름
        vg: String,
        /// VG가 놓인 iSCSI LUN. 있으면 노드가 그 타깃에 로그인한다. 없으면 LUN이 이미 보인다(FC 등).
        #[serde(default)]
        iscsi: Option<IscsiTarget>,
    },
}

/// 노드가 로그인할 iSCSI 타깃
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct IscsiTarget {
    /// `호스트[:포트]` 또는 `[IPv6][:포트]`
    pub portal: String,
    /// 타깃 이름(`iqn.`·`eui.`·`naa.`로 시작)
    pub target: String,
}

impl StoragePoolSpec {
    /// 백엔드별 값 검사. API가 풀을 만들 때 부른다.
    pub fn validate(&self) -> Result<(), PoolError> {
        match &self.backend {
            PoolBackend::Dir { path } => {
                if !path.is_absolute() {
                    return Err(PoolError::RelativePath(path.clone()));
                }
            }
            PoolBackend::Nfs {
                server,
                export,
                options,
            } => {
                if !valid_server(server) {
                    return Err(PoolError::BadServer(server.clone()));
                }
                if !export.starts_with('/') || export.chars().any(char::is_control) {
                    return Err(PoolError::BadExport(export.clone()));
                }
                if let Some(options) = options
                    && (options.is_empty()
                        || options.chars().any(|c| c.is_whitespace() || c.is_control())
                        || options.split(',').any(denied_option))
                {
                    return Err(PoolError::BadOptions(options.clone()));
                }
            }
            PoolBackend::LvmThin { vg, thinpool } => {
                for name in [vg, thinpool] {
                    if !valid_lvm_name(name) {
                        return Err(PoolError::BadLvmName(name.clone()));
                    }
                }
            }
            PoolBackend::Zfs { dataset } => {
                if !valid_zfs_dataset(dataset) {
                    return Err(PoolError::BadZfsDataset(dataset.clone()));
                }
            }
            PoolBackend::Rbd {
                pool,
                namespace,
                user,
                conf,
            } => {
                for name in std::iter::once(pool).chain(namespace).chain(user) {
                    if !valid_ceph_name(name) {
                        return Err(PoolError::BadRbdName(name.clone()));
                    }
                }
                // `rbd --id`와 QEMU `user`는 `client.` 없는 id를 받는다.
                if let Some(user) = user
                    && user.starts_with("client.")
                {
                    return Err(PoolError::RbdUserPrefix(user.clone()));
                }
                if let Some(conf) = conf
                    && (!conf.starts_with('/')
                        || conf.chars().any(|c| c.is_control() || ",:=".contains(c)))
                {
                    return Err(PoolError::BadRbdConf(conf.clone()));
                }
            }
            PoolBackend::LvmShared { vg, iscsi } => {
                if !valid_lvm_name(vg) {
                    return Err(PoolError::BadLvmName(vg.clone()));
                }
                if let Some(IscsiTarget { portal, target }) = iscsi {
                    if !valid_portal(portal) {
                        return Err(PoolError::BadIscsiPortal(portal.clone()));
                    }
                    if !valid_iscsi_target(target) {
                        return Err(PoolError::BadIscsiTarget(target.clone()));
                    }
                }
            }
        }
        Ok(())
    }
}

/// NFS 서버: IPv6 주소이거나, 공백·`:`·`,`·`/`·`[`·`]` 없는 ASCII 이름·IPv4.
/// `mount`의 인자 `server:export`가 옵션으로 읽히지 않게 `-`로 시작하지 않는다.
fn valid_server(server: &str) -> bool {
    if server.parse::<std::net::Ipv6Addr>().is_ok() {
        return true;
    }
    !server.is_empty()
        && !server.starts_with('-')
        && server
            .bytes()
            .all(|c| c.is_ascii_graphic() && !b":,/[]".contains(&c))
}

/// iSCSI 포털: `호스트[:포트]` 또는 `[IPv6][:포트]`. 호스트는 NFS 서버와 같은 규칙이되 IPv6는
/// 포트와 헷갈리지 않게 `[...]`로 감싼다. 포트는 1–65535.
fn valid_portal(portal: &str) -> bool {
    let (host_ok, port) = match portal.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((addr, port)) => (addr.parse::<std::net::Ipv6Addr>().is_ok(), port),
            None => return false,
        },
        None => match portal.find(':') {
            Some(i) => (valid_server(&portal[..i]), &portal[i..]),
            None => (valid_server(portal), ""),
        },
    };
    host_ok
        && (port.is_empty()
            || port.strip_prefix(':').is_some_and(|p| {
                !p.is_empty()
                    && p.bytes().all(|c| c.is_ascii_digit())
                    && p.parse::<u16>().is_ok_and(|n| n > 0)
            }))
}

/// iSCSI 타깃 이름: `iqn.`·`eui.`·`naa.`로 시작하고 소문자·숫자·`.`·`:`·`-`로만 된 223바이트 이하
fn valid_iscsi_target(target: &str) -> bool {
    target.len() <= 223
        && ["iqn.", "eui.", "naa."]
            .iter()
            .any(|p| target.len() > p.len() && target.starts_with(p))
        && target
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b".:-".contains(&c))
}

/// LVM 이름(VG, LV): `A-Za-z0-9+_.-`의 1–127자이고 `-`로 시작하지 않는다(명령 옵션으로 읽히지 않게).
/// LVM이 받지 않는 이름(`.`·`..`, `snapshot`·`pvmove`로 시작, [`LVM_RESERVED`]를 담은 것)도 거절한다.
fn valid_lvm_name(name: &str) -> bool {
    (1..=127).contains(&name.len())
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"+_.-".contains(&c))
        && name != "."
        && name != ".."
        && !name.starts_with("snapshot")
        && !name.starts_with("pvmove")
        && !LVM_RESERVED.iter().any(|r| name.contains(r))
}

/// ZFS 데이터셋 이름: 177자 이하이고(zvol 스냅샷 `{dataset}/opl-{uuid}@{uuid}`가 ZFS 이름 상한
/// 255자 안에 들게) `/`로 나뉜 구성 요소가 비지 않는다. 구성 요소는
/// `A-Za-z0-9_.:-`로만 되고 `-`로 시작하지 않으며(명령 옵션으로 읽히지 않게) `.`·`..`이 아니다.
/// 첫 구성 요소(풀 이름)는 문자로 시작한다.
fn valid_zfs_dataset(name: &str) -> bool {
    name.len() <= 177
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('-')
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
        })
}

/// Ceph 이름(풀, 네임스페이스, 클라이언트 id): `A-Za-z0-9_.-`의 1–63자이고 `-`로 시작하지 않는다
/// (명령 옵션으로 읽히지 않게). QEMU 옵션 문자열에도 그대로 들어간다.
fn valid_ceph_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}

/// LVM이 내부 LV 이름에 쓰므로 LV 이름에 담을 수 없는 문자열
const LVM_RESERVED: [&str; 12] = [
    "_cdata", "_cmeta", "_corig", "_mimage", "_mlog", "_pmspare", "_rimage", "_rmeta", "_tdata",
    "_tmeta", "_vorigin", "_vdata",
];

/// 받지 않는 마운트 옵션(`=` 앞 이름, 대소문자 무시). 다른 경로를 연결하거나(`bind`, `rbind`,
/// `move`), 기존 마운트를 바꾸거나(`remount`), 장치·setuid·실행을 허용하거나(`dev`, `suid`, `exec`),
/// 다른 프로그램을 부르는(`helper`, `x-*`, 그중 `X-mount.*`) 옵션, 마운트하지 못해도 뒤에서 다시
/// 시도하며 성공으로 돌아오는(`bg`) 옵션이다. opld는 늘 `nosuid,nodev,noexec,retry=0`을 붙인다.
fn denied_option(option: &str) -> bool {
    let name = option
        .split_once('=')
        .map_or(option, |(name, _)| name)
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "bind" | "rbind" | "remount" | "move" | "suid" | "dev" | "exec" | "helper" | "bg"
    ) || name.starts_with("x-")
}

/// 모든 노드가 같은 데이터를 보는 풀인지. NFS·RBD·공유 LVM 풀은 `shared` 값과 상관없이 공유이고,
/// LVM-thin·ZFS 풀은 늘 노드 로컬이다.
pub fn is_shared(pool: &StoragePool) -> bool {
    match pool.spec.backend {
        PoolBackend::Dir { .. } => pool.spec.shared,
        PoolBackend::Nfs { .. } | PoolBackend::Rbd { .. } | PoolBackend::LvmShared { .. } => true,
        PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } => false,
    }
}

/// 이 백엔드의 `format` 볼륨이 스냅샷을 담을 수 있는지. 파일 풀(dir, nfs)은 qcow2 내부 스냅샷,
/// LVM-thin 풀은 raw 볼륨의 thin 스냅샷, ZFS 풀은 raw zvol의 ZFS 스냅샷, RBD 풀은 raw 이미지의 RBD
/// 스냅샷이다. 공유 LVM 풀은 스냅샷을 담지 못한다.
pub fn pool_kind_supports_snapshots(backend: &PoolBackend, format: ImageFormat) -> bool {
    match backend {
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => format == ImageFormat::Qcow2,
        PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } | PoolBackend::Rbd { .. } => {
            format == ImageFormat::Raw
        }
        PoolBackend::LvmShared { .. } => false,
    }
}

/// 풀의 노드 목록이 이 노드를 허용하는지(`nodes`가 비었거나 포함)
pub fn pool_listed_on(pool: &StoragePool, node: Uuid) -> bool {
    pool.spec.nodes.is_empty() || pool.spec.nodes.contains(&node)
}

/// 노드가 알린 이 풀의 상태. 상태의 `pool_id`가 풀 id와 다르면(같은 이름으로 다시 만든 풀, 옛 상태)
/// 알리지 않은 것으로 본다.
pub fn pool_state_on<'a>(pool: &StoragePool, node: &'a Node) -> Option<&'a PoolState> {
    node.status
        .pools
        .get(&pool.name)
        .filter(|s| s.pool_id == pool.id)
}

/// 이 노드에서 풀을 쓸 수 있는지: 노드 목록이 허용하고, 노드가 알린 그 풀의 상태가 준비됨.
/// 디렉터리 풀은 노드가 상태를 아직 알리지 않았어도(옛 노드, 첫 새로 고침 전) 쓸 수 있다고 보고
/// 준비되지 않았다고 알렸을 때만 막는다. NFS 풀은 마운트됐다고, LVM-thin 풀은 thin pool이, ZFS 풀은
/// 데이터셋이 있다고, RBD 풀은 Ceph 풀에 닿는다고, 공유 LVM 풀은 공유 VG의 잠금이 시작됐다고 알려야
/// 한다. 위트니스 노드는 풀을 쓰지 않는다.
pub fn pool_usable_on(pool: &StoragePool, node: &Node) -> bool {
    !node.spec.witness
        && pool_listed_on(pool, node.id)
        && match pool_state_on(pool, node) {
            Some(state) => state.ready,
            None => matches!(pool.spec.backend, PoolBackend::Dir { .. }),
        }
}

/// 노드에서 풀이 준비되지 않은 이유. 노드가 알린 오류이고, 상태를 아직 알리지 않았으면 그렇게 적는다.
pub fn pool_error_on(pool: &StoragePool, node: &Node) -> String {
    if node.spec.witness {
        return "witness nodes do not use storage pools".into();
    }
    match pool_state_on(pool, node) {
        Some(state) => state.error.clone().unwrap_or_else(|| "not ready".into()),
        None => "no status reported yet".into(),
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PoolError {
    #[error("pool path must be absolute: {0:?}")]
    RelativePath(PathBuf),
    #[error("invalid nfs server {0:?}: use a host name or IP address without spaces, ':' or ','")]
    BadServer(String),
    #[error("invalid nfs export {0:?}: use an absolute path")]
    BadExport(String),
    #[error(
        "invalid nfs options {0:?}: use a comma-separated list without spaces, \
         and without bind, rbind, remount, move, suid, dev, exec, helper, bg or x-* options"
    )]
    BadOptions(String),
    #[error(
        "invalid lvm name {0:?}: use 1-127 characters from A-Z, a-z, 0-9, '+', '_', '.' and '-', \
         not starting with '-', 'snapshot' or 'pvmove', other than '.' and '..', and without \
         names lvm reserves such as '_tdata' or '_tmeta'"
    )]
    BadLvmName(String),
    #[error(
        "invalid zfs dataset {0:?}: use at most 177 characters in '/'-separated components of \
         A-Z, a-z, 0-9, '_', '.', ':' and '-', none empty, '.', '..' or starting with '-', \
         and the first (the pool name) starting with a letter"
    )]
    BadZfsDataset(String),
    #[error(
        "invalid ceph name {0:?}: use 1-63 characters from A-Z, a-z, 0-9, '_', '.' and '-', \
         not starting with '-'"
    )]
    BadRbdName(String),
    #[error("invalid ceph user {0:?}: write the Ceph user without client.")]
    RbdUserPrefix(String),
    #[error(
        "invalid ceph conf path {0:?}: use an absolute path without control characters, ',', ':' or '='"
    )]
    BadRbdConf(String),
    #[error(
        "invalid iscsi portal {0:?}: use host[:port] or [ipv6][:port] with a port from 1 to 65535"
    )]
    BadIscsiPortal(String),
    #[error(
        "invalid iscsi target {0:?}: use at most 223 characters from a-z, 0-9, '.', ':' and '-', \
         starting with iqn., eui. or naa."
    )]
    BadIscsiTarget(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct StoragePoolStatus {
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VolumeSpec {
    /// StoragePool 리소스 이름
    pub pool: String,
    pub size_bytes: u64,
    pub format: ImageFormat,
}

impl ResourceSpec for VolumeSpec {
    const KIND: Kind = Kind::Volume;
    type Status = VolumeStatus;
}

/// 가져올 원본 이미지 형식
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct VolumeStatus {
    pub phase: VolumePhase,
    /// 실제로 디스크에 할당된 크기
    pub allocated_bytes: Option<u64>,
    pub attached_to: Option<Uuid>,
    /// 볼륨을 만든 Task를 실행하는 노드. 그 노드만 시작할 때 남은 고아 볼륨을 정리한다.
    pub node: Option<Uuid>,
}

/// 볼륨 파일이 준비됐는지. VM은 준비된 볼륨만 붙일 수 있다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VolumePhase {
    /// 파일을 만들거나 가져오는 중
    #[default]
    Pending,
    Ready,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_pool_deserializes_with_defaults() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "dir", "path": "/var/lib/opl/pools/local" }
        }))
        .unwrap();
        assert_eq!(
            spec.backend,
            PoolBackend::Dir {
                path: "/var/lib/opl/pools/local".into()
            }
        );
        assert!(spec.nodes.is_empty());
        assert!(!spec.shared);
    }

    fn nfs(server: &str, export: &str, options: Option<&str>) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::Nfs {
                server: server.into(),
                export: export.into(),
                options: options.map(Into::into),
            },
            nodes: vec![],
            shared: false,
        }
    }

    #[test]
    fn nfs_pool_round_trips_and_options_are_optional() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "nfs", "server": "nas.lan", "export": "/srv/vm" }
        }))
        .unwrap();
        assert_eq!(spec, nfs("nas.lan", "/srv/vm", None));
        let with = nfs("10.0.0.5", "/srv/vm", Some("vers=4.2,hard"));
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["backend"]["kind"], "nfs");
        assert_eq!(json["backend"]["options"], "vers=4.2,hard");
        assert_eq!(
            serde_json::from_value::<StoragePoolSpec>(json).unwrap(),
            with
        );
        let typo = serde_json::from_value::<StoragePoolSpec>(serde_json::json!({
            "backend": { "kind": "nfs", "server": "nas", "export": "/x", "opts": "hard" }
        }));
        assert!(typo.is_err());
    }

    #[test]
    fn validates_backend_values() {
        let dir = |path: &str| StoragePoolSpec {
            backend: PoolBackend::Dir { path: path.into() },
            nodes: vec![],
            shared: false,
        };
        assert_eq!(dir("/srv/pool").validate(), Ok(()));
        assert_eq!(
            dir("pool").validate(),
            Err(PoolError::RelativePath("pool".into()))
        );
        for server in ["nas.lan", "10.0.0.5", "fd00::5", "::1", "NAS-01"] {
            assert_eq!(nfs(server, "/srv/vm", None).validate(), Ok(()), "{server}");
        }
        for bad in [
            "",
            "nas lan",
            "nas:2049",
            "a,b",
            "[fd00::5]",
            "-oremount",
            "a/b",
        ] {
            assert_eq!(
                nfs(bad, "/srv/vm", None).validate(),
                Err(PoolError::BadServer(bad.into())),
                "{bad}"
            );
        }
        for bad in ["", "srv/vm", "/srv/\nvm"] {
            assert_eq!(
                nfs("nas", bad, None).validate(),
                Err(PoolError::BadExport(bad.into()))
            );
        }
        assert_eq!(
            nfs("nas", "/srv/vm", Some("vers=4.2,hard")).validate(),
            Ok(())
        );
        for bad in ["", "vers=4.2, hard", "hard\t"] {
            assert_eq!(
                nfs("nas", "/srv/vm", Some(bad)).validate(),
                Err(PoolError::BadOptions(bad.into()))
            );
        }
        // 마운트의 성격을 바꾸는 옵션(다른 경로 연결, 기존 마운트 변경, 실행 허용 등)은 받지 않는다.
        for bad in [
            "bind",
            "hard,rbind",
            "remount,rw",
            "move",
            "suid",
            "DEV",
            "exec",
            "helper=foo",
            "x-systemd.automount",
            "X-mount.mkdir",
            "vers=4.2,Exec",
            "bg",
            "hard,BG",
        ] {
            assert_eq!(
                nfs("nas", "/srv/vm", Some(bad)).validate(),
                Err(PoolError::BadOptions(bad.into())),
                "{bad}"
            );
        }
        for ok in [
            "nosuid,nodev,noexec",
            "soft,timeo=600",
            "nconnect=4",
            "sec=krb5",
        ] {
            assert_eq!(nfs("nas", "/srv/vm", Some(ok)).validate(), Ok(()), "{ok}");
        }
    }

    #[test]
    fn nfs_pools_are_always_shared() {
        let mut pool = StoragePool::new(
            "p",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: "/srv/p".into(),
                },
                nodes: vec![],
                shared: false,
            },
        );
        assert!(!is_shared(&pool));
        pool.spec.shared = true;
        assert!(is_shared(&pool));
        pool.spec = nfs("nas", "/srv/vm", None);
        assert!(is_shared(&pool));
    }

    #[test]
    fn usable_on_needs_the_node_list_and_a_ready_state() {
        use crate::node::NodeSpec;

        let mut node: Node = Resource::new(
            "n1",
            NodeSpec {
                address: "a".into(),
                maintenance: false,
                witness: false,
            },
        );
        let mut pool = StoragePool::new("p", nfs("nas", "/srv/vm", None));
        // 상태를 알리지 않은 노드에서는 NFS 풀을 쓸 수 없다. 디렉터리 풀은 쓸 수 있다.
        assert!(pool_listed_on(&pool, node.id));
        assert!(!pool_usable_on(&pool, &node));
        assert_eq!(pool_error_on(&pool, &node), "no status reported yet");
        let mut dir = StoragePool::new(
            "d",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: "/srv/d".into(),
                },
                nodes: vec![],
                shared: false,
            },
        );
        assert!(pool_usable_on(&dir, &node));
        // 디렉터리 풀도 준비되지 않았다고 알렸거나 노드 목록에 없으면 쓸 수 없다.
        let down = PoolState {
            pool_id: dir.id,
            ready: false,
            error: Some("no such directory".into()),
            ..Default::default()
        };
        node.status.pools.insert("d".into(), down);
        assert!(!pool_usable_on(&dir, &node));
        node.status.pools.get_mut("d").unwrap().pool_id = Uuid::now_v7();
        assert!(pool_usable_on(&dir, &node));
        dir.spec.nodes = vec![Uuid::now_v7()];
        assert!(!pool_usable_on(&dir, &node));
        let failed = PoolState {
            pool_id: pool.id,
            ready: false,
            error: Some("mount.nfs: access denied".into()),
            ..Default::default()
        };
        node.status.pools.insert("p".into(), failed);
        assert!(!pool_usable_on(&pool, &node));
        assert_eq!(pool_error_on(&pool, &node), "mount.nfs: access denied");
        node.status.pools.get_mut("p").unwrap().ready = true;
        assert!(pool_usable_on(&pool, &node));
        // 같은 이름의 다른 풀(지우고 다시 만든 풀)이나 pool_id 전의 상태는 알리지 않은 것으로 본다.
        for other in [Uuid::now_v7(), Uuid::nil()] {
            node.status.pools.get_mut("p").unwrap().pool_id = other;
            assert_eq!(pool_state_on(&pool, &node), None);
            assert!(!pool_usable_on(&pool, &node));
            assert_eq!(pool_error_on(&pool, &node), "no status reported yet");
        }
        node.status.pools.get_mut("p").unwrap().pool_id = pool.id;
        assert!(pool_state_on(&pool, &node).is_some_and(|s| s.ready));
        // 노드 목록에 없으면 준비됐다고 알려도 쓸 수 없다.
        pool.spec.nodes = vec![Uuid::now_v7()];
        assert!(!pool_listed_on(&pool, node.id));
        assert!(!pool_usable_on(&pool, &node));
        pool.spec.nodes.push(node.id);
        assert!(pool_usable_on(&pool, &node));
        // 위트니스에서는 준비됐다고 알렸거나 상태가 없어도 되는 디렉터리 풀이라도 쓸 수 없다.
        node.spec.witness = true;
        assert!(!pool_usable_on(&pool, &node));
        assert_eq!(
            pool_error_on(&pool, &node),
            "witness nodes do not use storage pools"
        );
        dir.spec.nodes.clear();
        node.status.pools.remove("d");
        assert!(!pool_usable_on(&dir, &node));
    }

    fn lvm(vg: &str, thinpool: &str) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::LvmThin {
                vg: vg.into(),
                thinpool: thinpool.into(),
            },
            nodes: vec![],
            shared: false,
        }
    }

    #[test]
    fn lvm_thin_pool_round_trips() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" }
        }))
        .unwrap();
        assert_eq!(spec, lvm("vg0", "tp"));
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["backend"]["kind"], "lvm-thin");
        assert_eq!(json["backend"]["thinpool"], "tp");
        let typo = serde_json::from_value::<StoragePoolSpec>(serde_json::json!({
            "backend": { "kind": "lvm-thin", "vg": "vg0", "pool": "tp" }
        }));
        assert!(typo.is_err());
    }

    #[test]
    fn validates_lvm_names() {
        let long = "a".repeat(127);
        for ok in [
            "vg0",
            "opl_vg",
            "a+b.c-d",
            "VG",
            long.as_str(),
            "...",
            "my_snapshot",
            "tdata",
        ] {
            assert_eq!(lvm(ok, "tp").validate(), Ok(()), "{ok}");
            assert_eq!(lvm("vg", ok).validate(), Ok(()), "{ok}");
        }
        let too_long = "a".repeat(128);
        for bad in [
            "",
            "-vg",
            "vg 0",
            "vg/tp",
            "vg:0",
            "그룹",
            too_long.as_str(),
            // LVM이 받지 않는 이름: `.`·`..`, `snapshot`·`pvmove`로 시작, 내부 LV의 접미어 포함
            ".",
            "..",
            "snapshot1",
            "pvmove0",
            "data_tdata",
            "x_tmeta",
            "a_cdata",
            "a_cmeta",
            "a_corig",
            "a_mimage_0",
            "a_mlog",
            "a_pmspare",
            "a_rimage",
            "a_rmeta",
            "a_vorigin",
            "a_vdata",
        ] {
            assert_eq!(
                lvm(bad, "tp").validate(),
                Err(PoolError::BadLvmName(bad.into())),
                "{bad}"
            );
            assert_eq!(
                lvm("vg", bad).validate(),
                Err(PoolError::BadLvmName(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn lvm_thin_pools_are_node_local_and_need_a_ready_state() {
        use crate::node::NodeSpec;

        let mut pool = StoragePool::new("vg", lvm("vg0", "tp"));
        assert!(!is_shared(&pool));
        pool.spec.shared = true;
        assert!(!is_shared(&pool));
        let mut node: Node = Resource::new(
            "n1",
            NodeSpec {
                address: "a".into(),
                maintenance: false,
                witness: false,
            },
        );
        // 디렉터리 풀과 달리 상태를 알리기 전에는 쓸 수 없다.
        assert!(!pool_usable_on(&pool, &node));
        let state = PoolState {
            pool_id: pool.id,
            ready: true,
            ..Default::default()
        };
        node.status.pools.insert("vg".into(), state);
        assert!(pool_usable_on(&pool, &node));
    }

    #[test]
    fn snapshot_support_depends_on_backend_and_format() {
        let dir = PoolBackend::Dir {
            path: "/srv/p".into(),
        };
        let nfs = nfs("nas", "/srv/vm", None).backend;
        let lvm = lvm("vg0", "tp").backend;
        let zfs = zfs("tank/opl").backend;
        for (backend, qcow2, raw) in [
            (&dir, true, false),
            (&nfs, true, false),
            (&lvm, false, true),
            (&zfs, false, true),
            (&rbd("rbd").backend, false, true),
            (&lvm_shared("vg0", None).backend, false, false),
        ] {
            assert_eq!(
                pool_kind_supports_snapshots(backend, ImageFormat::Qcow2),
                qcow2,
                "{backend:?}"
            );
            assert_eq!(
                pool_kind_supports_snapshots(backend, ImageFormat::Raw),
                raw,
                "{backend:?}"
            );
        }
    }

    fn zfs(dataset: &str) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::Zfs {
                dataset: dataset.into(),
            },
            nodes: vec![],
            shared: false,
        }
    }

    #[test]
    fn zfs_pool_round_trips() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "zfs", "dataset": "tank/opl" }
        }))
        .unwrap();
        assert_eq!(spec, zfs("tank/opl"));
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["backend"]["kind"], "zfs");
        assert_eq!(json["backend"]["dataset"], "tank/opl");
        let typo = serde_json::from_value::<StoragePoolSpec>(serde_json::json!({
            "backend": { "kind": "zfs", "pool": "tank/opl" }
        }));
        assert!(typo.is_err());
    }

    #[test]
    fn validates_zfs_dataset_names() {
        // zvol 스냅샷 `{dataset}/opl-{uuid}@{uuid}`가 ZFS 이름 상한 255자를 넘지 않게 177자까지다.
        let long = format!("t/{}", "a".repeat(175));
        for ok in [
            "tank",
            "tank/opl",
            "Tank0/a_b.c:d-e/f",
            "t/x-",
            "t/.x",
            "t/9",
            long.as_str(),
        ] {
            assert_eq!(zfs(ok).validate(), Ok(()), "{ok}");
        }
        let too_long = format!("t/{}", "a".repeat(176));
        for bad in [
            "",
            "/tank",
            "tank/",
            "tank//opl",
            // 풀 이름은 문자로 시작한다.
            "0tank",
            "_tank",
            "-tank",
            ".tank",
            // `-`로 시작하는 구성 요소는 명령 옵션으로 읽힐 수 있다.
            "tank/-opl",
            "tank/opl@snap",
            "tank/opl#bm",
            "tank/my opl",
            "tank/opl+",
            "tank/데이터",
            "tank/.",
            "tank/..",
            too_long.as_str(),
        ] {
            assert_eq!(
                zfs(bad).validate(),
                Err(PoolError::BadZfsDataset(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn zfs_pools_are_node_local_and_need_a_ready_state() {
        use crate::node::NodeSpec;

        let mut pool = StoragePool::new("z", zfs("tank/opl"));
        assert!(!is_shared(&pool));
        pool.spec.shared = true;
        assert!(!is_shared(&pool));
        let mut node: Node = Resource::new(
            "n1",
            NodeSpec {
                address: "a".into(),
                maintenance: false,
                witness: false,
            },
        );
        assert!(!pool_usable_on(&pool, &node));
        let state = PoolState {
            pool_id: pool.id,
            ready: true,
            ..Default::default()
        };
        node.status.pools.insert("z".into(), state);
        assert!(pool_usable_on(&pool, &node));
    }

    fn rbd(pool: &str) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::Rbd {
                pool: pool.into(),
                namespace: None,
                user: None,
                conf: None,
            },
            nodes: vec![],
            shared: true,
        }
    }

    /// `rbd` 풀의 `namespace`·`user`·`conf`의 값을 바꾼 spec
    fn rbd_with(
        namespace: Option<&str>,
        user: Option<&str>,
        conf: Option<&str>,
    ) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::Rbd {
                pool: "rbd".into(),
                namespace: namespace.map(Into::into),
                user: user.map(Into::into),
                conf: conf.map(Into::into),
            },
            ..rbd("rbd")
        }
    }

    #[test]
    fn rbd_pool_round_trips_with_optional_fields() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "rbd", "pool": "rbd" }, "shared": true
        }))
        .unwrap();
        assert_eq!(spec, rbd("rbd"));
        let full = rbd_with(Some("opl"), Some("opl"), Some("/etc/ceph/opl.conf"));
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(
            json["backend"],
            serde_json::json!({
                "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl",
                "conf": "/etc/ceph/opl.conf"
            })
        );
        assert_eq!(
            serde_json::from_value::<StoragePoolSpec>(json).unwrap(),
            full
        );
        let typo = serde_json::from_value::<StoragePoolSpec>(serde_json::json!({
            "backend": { "kind": "rbd", "pool": "rbd", "id": "admin" }
        }));
        assert!(typo.is_err());
    }

    #[test]
    fn validates_rbd_names_and_conf() {
        let long = "a".repeat(63);
        for ok in ["rbd", "Opl_vm.1-a", "9", long.as_str()] {
            assert_eq!(rbd(ok).validate(), Ok(()), "{ok}");
            assert_eq!(
                rbd_with(Some(ok), Some(ok), None).validate(),
                Ok(()),
                "{ok}"
            );
        }
        let too_long = "a".repeat(64);
        for bad in [
            "",
            // `-`로 시작하면 명령 옵션으로 읽힐 수 있다.
            "-rbd",
            "rbd/opl",
            "rbd@s",
            "a,b",
            "a:b",
            "a=b",
            "my rbd",
            "데이터",
            too_long.as_str(),
        ] {
            let err = Err(PoolError::BadRbdName(bad.into()));
            assert_eq!(rbd(bad).validate(), err, "pool {bad}");
            assert_eq!(rbd_with(Some(bad), None, None).validate(), err, "ns {bad}");
            assert_eq!(
                rbd_with(None, Some(bad), None).validate(),
                err,
                "user {bad}"
            );
        }
        // `rbd --id`와 QEMU `user`는 `client.` 없는 id를 받는다.
        for user in ["client.admin", "client.opl"] {
            let err = rbd_with(None, Some(user), None).validate().unwrap_err();
            assert_eq!(err, PoolError::RbdUserPrefix(user.into()));
            assert!(
                err.to_string()
                    .contains("write the Ceph user without client."),
                "{err}"
            );
        }
        assert_eq!(
            rbd_with(Some("client.a"), Some("clients"), None).validate(),
            Ok(())
        );
        assert_eq!(
            rbd_with(None, None, Some("/etc/ceph/my ceph.conf")).validate(),
            Ok(())
        );
        // conf는 QEMU 옵션 문자열(`conf=...`)과 `rbd:` 이미지 이름에 들어간다.
        for bad in [
            "",
            "ceph.conf",
            "/etc/ceph/a,b.conf",
            "/etc/ceph/a:b.conf",
            "/etc/ceph/a=b.conf",
            "/etc/ceph/\nceph.conf",
        ] {
            assert_eq!(
                rbd_with(None, None, Some(bad)).validate(),
                Err(PoolError::BadRbdConf(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn rbd_pools_are_always_shared() {
        let mut pool = StoragePool::new("r", rbd("rbd"));
        assert!(is_shared(&pool));
        pool.spec.shared = false;
        assert!(is_shared(&pool));
        let mut node: Node = Resource::new(
            "n1",
            crate::node::NodeSpec {
                address: "a".into(),
                maintenance: false,
                witness: false,
            },
        );
        // 디렉터리 풀과 달리 준비됐다는 상태가 있어야 쓸 수 있다.
        assert!(!pool_usable_on(&pool, &node));
        let state = PoolState {
            pool_id: pool.id,
            ready: true,
            ..Default::default()
        };
        node.status.pools.insert("r".into(), state);
        assert!(pool_usable_on(&pool, &node));
    }

    fn lvm_shared(vg: &str, iscsi: Option<(&str, &str)>) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::LvmShared {
                vg: vg.into(),
                iscsi: iscsi.map(|(portal, target)| IscsiTarget {
                    portal: portal.into(),
                    target: target.into(),
                }),
            },
            nodes: vec![],
            shared: true,
        }
    }

    const IQN: &str = "iqn.2003-01.org.linux-iscsi.san:opl";

    #[test]
    fn lvm_shared_pool_round_trips() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "lvm-shared", "vg": "san" }, "shared": true
        }))
        .unwrap();
        assert_eq!(spec, lvm_shared("san", None));
        let full = lvm_shared("san", Some(("10.0.0.5:3260", IQN)));
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(
            json["backend"],
            serde_json::json!({
                "kind": "lvm-shared", "vg": "san",
                "iscsi": { "portal": "10.0.0.5:3260", "target": IQN }
            })
        );
        assert_eq!(
            serde_json::from_value::<StoragePoolSpec>(json).unwrap(),
            full
        );
        for typo in [
            serde_json::json!({ "kind": "lvm-shared", "vg": "san", "thinpool": "tp" }),
            serde_json::json!({
                "kind": "lvm-shared", "vg": "san",
                "iscsi": { "portal": "nas", "target": IQN, "chap": "secret" }
            }),
            serde_json::json!({ "kind": "lvm-shared", "vg": "san", "iscsi": { "portal": "nas" } }),
        ] {
            let res =
                serde_json::from_value::<StoragePoolSpec>(serde_json::json!({ "backend": typo }));
            assert!(res.is_err(), "{typo}");
        }
    }

    #[test]
    fn validates_lvm_shared_vg_portal_and_target() {
        assert_eq!(lvm_shared("san", None).validate(), Ok(()));
        // VG 이름은 LVM-thin과 같은 규칙이다.
        for bad in ["", "-vg", "a/b", "snapshot"] {
            assert_eq!(
                lvm_shared(bad, None).validate(),
                Err(PoolError::BadLvmName(bad.into())),
                "{bad}"
            );
        }
        for ok in [
            "san.lan",
            "san.lan:3260",
            "10.0.0.5",
            "10.0.0.5:1",
            "NAS-01:65535",
            "[fd00::5]",
            "[::1]:3260",
        ] {
            assert_eq!(
                lvm_shared("san", Some((ok, IQN))).validate(),
                Ok(()),
                "{ok}"
            );
        }
        for bad in [
            "",
            "san lan",
            "-oremount",
            "a,b",
            "a/b",
            "san:",
            "san:0",
            "san:65536",
            "san:+80",
            "san:32a",
            "san:3260:1",
            // IPv6는 포트와 헷갈리지 않게 `[...]`로 감싼다.
            "fd00::5",
            "[fd00::5",
            "[nas]:3260",
            "[fd00::5]3260",
            "[fd00::5]:",
            ":3260",
        ] {
            assert_eq!(
                lvm_shared("san", Some((bad, IQN))).validate(),
                Err(PoolError::BadIscsiPortal(bad.into())),
                "{bad}"
            );
        }
        let long = format!("iqn.{}", "a".repeat(219));
        for ok in [
            IQN,
            "eui.02004567a425678d",
            "naa.52004567ba64678d",
            long.as_str(),
        ] {
            assert_eq!(
                lvm_shared("san", Some(("san", ok))).validate(),
                Ok(()),
                "{ok}"
            );
        }
        let too_long = format!("iqn.{}", "a".repeat(220));
        for bad in [
            "",
            "iqn.",
            "opl",
            "IQN.2003-01.org:opl",
            "iqn.2003-01.org:OPL",
            "iqn.2003-01.org:my opl",
            "iqn.2003-01.org:a,b",
            "iqn.2003-01.org:a_b",
            "-iqn.2003",
            too_long.as_str(),
        ] {
            assert_eq!(
                lvm_shared("san", Some(("san", bad))).validate(),
                Err(PoolError::BadIscsiTarget(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn lvm_shared_pools_are_always_shared_and_need_a_ready_state() {
        let mut pool = StoragePool::new("s", lvm_shared("san", None));
        assert!(is_shared(&pool));
        pool.spec.shared = false;
        assert!(is_shared(&pool));
        let mut node: Node = Resource::new(
            "n1",
            crate::node::NodeSpec {
                address: "a".into(),
                maintenance: false,
                witness: false,
            },
        );
        assert!(!pool_usable_on(&pool, &node));
        let state = PoolState {
            pool_id: pool.id,
            ready: true,
            ..Default::default()
        };
        node.status.pools.insert("s".into(), state);
        assert!(pool_usable_on(&pool, &node));
    }

    #[test]
    fn rejects_unknown_backend_fields() {
        let res = serde_json::from_value::<StoragePoolSpec>(serde_json::json!({
            "backend": { "kind": "dir", "path": "/p", "pth": "/typo" }
        }));
        assert!(res.is_err());
    }

    #[test]
    fn volume_spec_round_trips() {
        let spec = VolumeSpec {
            pool: "local".into(),
            size_bytes: 10 << 30,
            format: ImageFormat::Qcow2,
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["format"], "qcow2");
        assert_eq!(serde_json::from_value::<VolumeSpec>(json).unwrap(), spec);
    }

    #[test]
    fn statuses_default_missing_fields() {
        let p: StoragePoolStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(p, StoragePoolStatus::default());
        let v: VolumeStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(v, VolumeStatus::default());
    }
}
```

`crates/oplctl/src/cli.rs`에서

```rust

/// 풀을 쓸 수 있는 노드. 노드 목록이 비었으면 모든 노드이고, 아니면 목록의 노드를 그 순서대로
/// 본다. 목록에 있지만 없는 노드는 그 id(`Err`)다.
fn pool_nodes<'a>(pool: &'a Value, nodes: &'a [Value]) -> Vec<Result<&'a Value, &'a Value>> {
```

을 다음으로 바꾼다:

```rust

/// 풀을 쓸 수 있는 노드. 노드 목록이 비었으면 위트니스가 아닌 모든 노드이고, 아니면 목록의 노드를
/// 그 순서대로 본다. 목록에 있지만 없는 노드는 그 id(`Err`)다.
fn pool_nodes<'a>(pool: &'a Value, nodes: &'a [Value]) -> Vec<Result<&'a Value, &'a Value>> {
```

`crates/oplctl/src/cli.rs`에서

```rust
            .collect(),
        _ => nodes.iter().map(Ok).collect(),
    }
```

을 다음으로 바꾼다:

```rust
            .collect(),
        _ => nodes
            .iter()
            .filter(|n| n["spec"]["witness"] != true)
            .map(Ok)
            .collect(),
    }
```

`crates/oplctl/src/cli.rs`에서

```rust
            ),
        ];
```

을 다음으로 바꾼다:

```rust
            ),
            // 위트니스는 풀을 쓰지 않으므로 "모든 노드"에 들지 않는다.
            json!({
                "id": "id-w",
                "name": "w",
                "spec": { "witness": true },
                "status": { "online": true, "pools": {} },
            }),
        ];
```

`crates/opld/src/bootstrap.rs`에서

```rust
                    maintenance: false,
                },
```

을 다음으로 바꾼다:

```rust
                    maintenance: false,
                    witness: false,
                },
```

`crates/opld/src/join.rs`에서

```rust
            csr_pem,
        })
```

을 다음으로 바꾼다:

```rust
            csr_pem,
            witness: false,
        })
```

`crates/opld/src/pools.rs`에서

```rust
                maintenance: false,
            },
```

을 다음으로 바꾼다:

```rust
                maintenance: false,
                witness: false,
            },
```

`web/src/i18n/en.json`에서

```json
    "migration_unsupported": "This VM cannot be migrated: {{message}}",
    "node_unavailable": "The target node is offline or in maintenance.",
    "no_capacity": "The target node cannot take this VM: {{message}}",
```

을 다음으로 바꾼다:

```json
    "migration_unsupported": "This VM cannot be migrated: {{message}}",
    "node_unavailable": "The target node is a witness, offline or in maintenance.",
    "no_capacity": "The target node cannot take this VM: {{message}}",
```

`web/src/i18n/ko.json`에서

```json
    "migration_unsupported": "이 VM은 마이그레이션할 수 없습니다: {{message}}",
    "node_unavailable": "대상 노드가 오프라인이거나 유지보수 중입니다.",
    "no_capacity": "대상 노드가 이 VM을 받을 수 없습니다: {{message}}",
```

을 다음으로 바꾼다:

```json
    "migration_unsupported": "이 VM은 마이그레이션할 수 없습니다: {{message}}",
    "node_unavailable": "대상 노드가 위트니스이거나 오프라인이거나 유지보수 중입니다.",
    "no_capacity": "대상 노드가 이 VM을 받을 수 없습니다: {{message}}",
```

`web/src/storage/pools.test.ts`에서

```ts
  ]);
  // 꺼진 노드는 오프라인으로 보인다.
```

을 다음으로 바꾼다:

```ts
  ]);
  // 노드 목록이 비었으면 위트니스는 빼고 본다. 위트니스는 풀을 쓰지 않는다.
  const witness = { ...b, id: "n9", name: "node-w", spec: { ...NODE.spec, witness: true } };
  expect(poolNodes(pool("local"), [NODE, witness]).map((n) => n.name)).toEqual(["node-a"]);
  // 꺼진 노드는 오프라인으로 보인다.
```

`web/src/storage/pools.test.ts`에서

```ts
  expect(poolsUsableOn(pools, [ready], "n9")).toEqual([]);
```

을 다음으로 바꾼다:

```ts
  expect(poolsUsableOn(pools, [ready], "n9")).toEqual([]);
  // 위트니스에서는 준비됐다고 알려도 아무 풀도 쓸 수 없다.
  const witness = { ...ready, spec: { ...NODE.spec, witness: true } };
  expect(poolsUsableOn(pools, [witness], "n1")).toEqual([]);
```

`web/src/storage/pools.ts`에서

```ts
/**
 * 이 노드에서 풀을 쓸 수 있는지. 서버의 `pool_usable_on`(노드 목록이 허용하고 준비됐다고 알림 —
 * 디렉터리 풀은 알리지 않았어도 됨)에 노드가 온라인일 것을 더한다.
 */
function usableOn(pool: StoragePool, node: Node): boolean {
  const listed = pool.spec.nodes ?? [];
  if (!node.status.online || (listed.length > 0 && !listed.includes(node.id))) {
    return false;
```

을 다음으로 바꾼다:

```ts
/**
 * 이 노드에서 풀을 쓸 수 있는지. 서버의 `pool_usable_on`(위트니스가 아니고 노드 목록이 허용하고
 * 준비됐다고 알림 — 디렉터리 풀은 알리지 않았어도 됨)에 노드가 온라인일 것을 더한다.
 */
function usableOn(pool: StoragePool, node: Node): boolean {
  const listed = pool.spec.nodes ?? [];
  if (
    node.spec.witness ||
    !node.status.online ||
    (listed.length > 0 && !listed.includes(node.id))
  ) {
    return false;
```

`web/src/storage/pools.ts`에서

```ts
/**
 * 풀을 쓸 수 있는 노드와 그 노드가 알린 풀 상태. 노드 목록이 비었으면 모든 노드다. 꺼진 노드는
 * `offline`이다.
 */
```

을 다음으로 바꾼다:

```ts
/**
 * 풀을 쓸 수 있는 노드와 그 노드가 알린 풀 상태. 노드 목록이 비었으면 위트니스가 아닌 모든 노드다.
 * 꺼진 노드는 `offline`이다.
 */
```

`web/src/storage/pools.ts`에서

```ts
  const listed = pool.spec.nodes ?? [];
  const ids = listed.length === 0 ? nodes.map((n) => n.id) : listed;
  return ids.map((id) => {
```

을 다음으로 바꾼다:

```ts
  const listed = pool.spec.nodes ?? [];
  const ids = listed.length === 0 ? nodes.filter((n) => !n.spec.witness).map((n) => n.id) : listed;
  return ids.map((id) => {
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
  await user.selectOptions(screen.getByLabelText("Node"), "node-b");
  expect(machine).toHaveValue("virt");
});
```

을 다음으로 바꾼다:

```tsx
  await user.selectOptions(screen.getByLabelText("Node"), "node-b");
  expect(machine).toHaveValue("virt");
});

it("does not offer witness nodes", async () => {
  const arm = { ...NODE, status: { ...NODE.status, arch: "aarch64" as const } };
  const witness = {
    ...NODE,
    id: "n2",
    name: "node-w",
    spec: { ...NODE.spec, witness: true },
    status: { ...NODE.status, arch: "x86_64" as const },
  };
  mockLists({ node: [arm, witness] });
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  // 위트니스의 아키텍처는 머신 기본값에 섞이지 않는다.
  expect(await screen.findByLabelText("Machine type")).toHaveValue("virt");
  expect(screen.getByRole("option", { name: "node-a" })).toBeInTheDocument();
  expect(screen.queryByRole("option", { name: "node-w" })).not.toBeInTheDocument();
});
```

`web/src/vms/VmMigrateDialog.test.tsx`에서

```tsx
  expect(migrationTargets(q35, NODES).map((n) => n.name)).toEqual(["node-e"]);
});
```

을 다음으로 바꾼다:

```tsx
  expect(migrationTargets(q35, NODES).map((n) => n.name)).toEqual(["node-e"]);
  // 위트니스는 VM을 받지 않는다.
  const witness = { ...node("n8", "node-w"), spec: { ...NODE.spec, witness: true } };
  expect(migrationTargets(web, [...NODES, witness]).map((n) => n.name)).toEqual([
    "node-b",
    "node-g",
  ]);
});
```

`web/src/vms/VmMigrateDialog.tsx`에서

```tsx
/**
 * 옮겨 갈 수 있어 보이는 노드: 온라인이고 유지보수 중이 아니며 VM 머신의 아키텍처인, 지금 노드가 아닌 노드.
 * 풀·브리지·메모리는 서버가 본다(409로 알린다).
```

을 다음으로 바꾼다:

```tsx
/**
 * 옮겨 갈 수 있어 보이는 노드: 위트니스가 아니고 온라인이고 유지보수 중이 아니며 VM 머신의 아키텍처인,
 * 지금 노드가 아닌 노드.
 * 풀·브리지·메모리는 서버가 본다(409로 알린다).
```

`web/src/vms/VmMigrateDialog.tsx`에서

```tsx
  return nodes.filter(
    (n) => n.id !== here && n.status.online && !n.spec.maintenance && n.status.arch === arch,
  );
```

을 다음으로 바꾼다:

```tsx
  return nodes.filter(
    (n) =>
      n.id !== here &&
      !n.spec.witness &&
      n.status.online &&
      !n.spec.maintenance &&
      n.status.arch === arch,
  );
```

`web/src/vms/form.test.ts`에서

```ts
  expect(defaultMachine([])).toBe("q35");
});
```

을 다음으로 바꾼다:

```ts
  expect(defaultMachine([])).toBe("q35");
  // 위트니스는 VM을 돌리지 않으므로 그 아키텍처는 보지 않는다.
  const witness = { ...x86, id: "n4", spec: { ...NODE.spec, witness: true } };
  expect(defaultMachine([arm, witness])).toBe("virt");
});
```

`web/src/vms/form.ts`에서

```ts
/**
 * 머신 기본값: 지정 노드(`selectedNode`, 빈 문자열이면 자동)의 아키텍처, 아니면 아키텍처를 아는 노드가 모두
 * 같을 때 그 아키텍처의 머신, 그 밖에는 `q35`다.
 */
```

을 다음으로 바꾼다:

```ts
/**
 * 머신 기본값: 지정 노드(`selectedNode`, 빈 문자열이면 자동)의 아키텍처, 아니면 아키텍처를 아는 노드(위트니스
 * 제외)가 모두 같을 때 그 아키텍처의 머신, 그 밖에는 `q35`다.
 */
```

`web/src/vms/form.ts`에서

```ts
  }
  const [only, ...others] = new Set(nodes.flatMap((n) => (n.status.arch ? [n.status.arch] : [])));
  return only && others.length === 0 ? MACHINE_OF[only] : "q35";
```

을 다음으로 바꾼다:

```ts
  }
  const [only, ...others] = new Set(
    nodes.flatMap((n) => (n.status.arch && !n.spec.witness ? [n.status.arch] : [])),
  );
  return only && others.length === 0 ? MACHINE_OF[only] : "q35";
```

`web/src/vms/form/GeneralSection.tsx`에서

```tsx
            <NativeSelectOption value="">{t("vmForm.nodeAuto")}</NativeSelectOption>
            {nodes?.map((n) => (
              <NativeSelectOption key={n.id} value={n.id}>
                {n.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
```

을 다음으로 바꾼다:

```tsx
            <NativeSelectOption value="">{t("vmForm.nodeAuto")}</NativeSelectOption>
            {/* 위트니스는 VM을 돌리지 않는다. */}
            {nodes
              ?.filter((n) => !n.spec.witness)
              .map((n) => (
                <NativeSelectOption key={n.id} value={n.id}>
                  {n.name}
                </NativeSelectOption>
              ))}
          </NativeSelect>
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 751개, 실패 0

Run: `cargo test -p opl-types -p opl-api`
Expected: PASS

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 311개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat: 위트니스 노드 타입과 배치에서 빼기"
```

---

### Task 2: opld 위트니스 멤버

opld 설정 최상위 `witness`(기본 false). 위트니스는 `cluster init`을 409(`WITNESS_CANNOT_INIT`)로 거부하고, 합류 요청에 `witness: true`를 싣고, 합류 전 포트 검사는 노드 간 포트만 본다. 멤버로 돌 때 클러스터 저장소·노드 간 서버(`PeerService::new(cluster, None)`)·승격·Node 등록(`register_witness`: online과 arch만)만 띄운다 — HTTPS API, reconciler, Task 실행기, 풀 마운터, 노드 감시, 예약 스냅샷, 정리, 브리지 새로 고침, 마이그레이션 TLS 파일이 없고 `Host`·QEMU를 찾지 않는다. Raft 설정 `enable_elect: !witness`(투표만 하고 후보가 되지 않는다). 다시 시작할 때 저장된 `spec.witness`와 설정이 다르면 시작하지 않는다(`check_role`). 탈퇴는 남는 투표 멤버가 모두 위트니스(또는 없음)면 거부한다(`check_leave`). 시험: opl-cluster memnet(`tests/witness.rs`, `TestCluster::start_with_witnesses`), opld 단위.

**Files:**
- Create: `crates/opl-cluster/tests/witness.rs`
- Modify: `crates/opl-cluster/tests/common/mod.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/join.rs`
- Modify: `crates/opld/src/main.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-cluster/tests/common/mod.rs`에서

```rust

use std::collections::BTreeMap;
use std::time::{Duration, Instant};
```

을 다음으로 바꾼다:

```rust

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};
```

`crates/opl-cluster/tests/common/mod.rs`에서

```rust
    pub forward: bool,
    root: TempDir,
```

을 다음으로 바꾼다:

```rust
    pub forward: bool,
    /// 선거에 나서지 않는 노드(`enable_elect = false`, 위트니스)
    pub witnesses: BTreeSet<NodeId>,
    root: TempDir,
```

`crates/opl-cluster/tests/common/mod.rs`에서

```rust
    pub async fn start_with(n: u64, raft: openraft::Config, write_timeout: Duration) -> Self {
        Self::build(n, raft, write_timeout, false).await
    }
```

을 다음으로 바꾼다:

```rust
    pub async fn start_with(n: u64, raft: openraft::Config, write_timeout: Duration) -> Self {
        Self::build(n, raft, write_timeout, false, &[]).await
    }
```

`crates/opl-cluster/tests/common/mod.rs`에서

```rust
    pub async fn start_forwarding(n: u64) -> Self {
        Self::build(n, raft_config(), Duration::from_secs(10), true).await
    }

    async fn build(n: u64, raft: openraft::Config, write_timeout: Duration, forward: bool) -> Self {
        let mut tc = TestCluster {
```

을 다음으로 바꾼다:

```rust
    pub async fn start_forwarding(n: u64) -> Self {
        Self::build(n, raft_config(), Duration::from_secs(10), true, &[]).await
    }

    /// `witnesses`가 선거에 나서지 않는 클러스터. 노드 1에서 초기화하므로 1은 넣지 않는다.
    pub async fn start_with_witnesses(n: u64, witnesses: &[u64]) -> Self {
        Self::build(n, raft_config(), Duration::from_secs(5), false, witnesses).await
    }

    async fn build(
        n: u64,
        raft: openraft::Config,
        write_timeout: Duration,
        forward: bool,
        witnesses: &[u64],
    ) -> Self {
        let mut tc = TestCluster {
```

`crates/opl-cluster/tests/common/mod.rs`에서

```rust
            forward,
            root: TempDir::new().unwrap(),
```

을 다음으로 바꾼다:

```rust
            forward,
            witnesses: witnesses.iter().map(|&i| node(i)).collect(),
            root: TempDir::new().unwrap(),
```

`crates/opl-cluster/tests/common/mod.rs`에서

```rust
        let mut config = ClusterConfig::new(id, self.root.path().join(id.to_string()));
        config.raft = self.raft.clone();
        config.write_timeout = self.write_timeout;
```

을 다음으로 바꾼다:

```rust
        let mut config = ClusterConfig::new(id, self.root.path().join(id.to_string()));
        config.raft = openraft::Config {
            enable_elect: !self.witnesses.contains(&id),
            ..self.raft.clone()
        };
        config.write_timeout = self.write_timeout;
```

`crates/opl-cluster/tests/witness.rs`를 만든다:

```rust
//! 선거에 나서지 않는 투표 멤버(위트니스, `enable_elect = false`).

mod common;

use std::time::{Duration, Instant};

use common::{TestCluster, node, volume};
use openraft::ServerState;
use opl_types::storage::VolumeSpec;

/// 위트니스만 남으면 과반수여도 리더가 생기지 않는다. 일반 노드가 돌아오면 다시 리더가 된다.
#[tokio::test]
async fn witnesses_never_become_leader() {
    let mut tc = TestCluster::start_with_witnesses(3, &[2, 3]).await;
    assert_eq!(tc.leader().await, node(1));

    tc.stop(node(1)).await;
    // 선거 제한 시간(최대 2초)의 두 배 넘게 지켜본다.
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        for id in [node(2), node(3)] {
            let state = tc.get(id).raft().metrics().borrow().state;
            assert_eq!(state, ServerState::Follower, "{id} stood for election");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tc.open(node(1)).await;
    assert_eq!(tc.leader().await, node(1));
}

/// 일반 노드 둘과 위트니스에서 리더를 멈추면 남은 일반 노드가 리더가 되고 쓰기가 된다.
#[tokio::test]
async fn the_other_node_takes_over_when_the_leader_stops() {
    let mut tc = TestCluster::start_with_witnesses(3, &[3]).await;
    let old = tc.leader().await;
    assert_ne!(old, node(3));

    tc.stop(old).await;
    let rest = [node(1), node(2), node(3)]
        .into_iter()
        .filter(|id| *id != old);
    let new = tc.leader_among(&rest.collect::<Vec<_>>()).await;
    assert_ne!(new, node(3), "the witness became leader");

    let v = volume("after");
    let rev = tc.get(new).create(&v).await.unwrap();
    tc.wait_applied(node(3), rev).await;
    assert!(tc.get(node(3)).get::<VolumeSpec>(v.id).unwrap().is_some());
}
```

`crates/opld/src/bootstrap.rs` 전체:

```rust
//! 노드 id, 새 클러스터의 첫 기록(클러스터 정보, 관리자), Node 리소스.

use std::io::Write;
use std::path::Path;

use anyhow::Context;
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError};
use opl_peer::pki::ClusterCa;
use opl_types::auth::{Role, UserSpec};
use opl_types::cluster::ClusterSpec;
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::vm::Arch;
use uuid::Uuid;

use crate::config::Config;
use crate::host::Host;

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
    host: &Host,
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
    register_node(cluster, config, host).await?;
    let password = Uuid::new_v4().simple().to_string();
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: opl_api::hash_password(password.clone())
                .await
                .map_err(|e| anyhow::anyhow!(e.message))?,
            role: Role::Admin,
            session_epoch: 0,
        },
    );
    cluster.create(&admin).await?;
    Ok(password)
}

/// 다시 시작한 노드의 역할(합류할 때 정해진 Node `spec.witness`)이 설정의 `witness`와 같은지 본다.
/// 로컬 저장소만 읽는다. Node 리소스가 아직 없으면 그대로 둔다.
pub fn check_role(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let Some(node) = cluster.get::<NodeSpec>(id)? else {
        return Ok(());
    };
    match (node.spec.witness, config.witness) {
        (true, false) => anyhow::bail!(
            "this node joined as a witness but opld.toml says witness = false; leave the cluster \
             and join again to change the role"
        ),
        (false, true) => anyhow::bail!(
            "this node joined as a non-witness node but opld.toml says witness = true; leave the \
             cluster and join again to change the role"
        ),
        _ => Ok(()),
    }
}

/// 이 노드의 Node 리소스가 없으면 만들고(역할은 설정의 `witness`), 있으면 주소만 맞춘다.
async fn ensure_node(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let address = config.api_address();
    match cluster.get::<NodeSpec>(id)? {
        None => {
            let mut node = Resource::new(
                config.name.clone(),
                NodeSpec {
                    address,
                    maintenance: false,
                    witness: config.witness,
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
    Ok(())
}

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
    ensure_node(cluster, config).await?;
    let bridges = host.bridges().await;
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let memory_mib = crate::host::memory_mib();
    update_node_status(cluster, |status| {
        *status = NodeStatus {
            online: true,
            cpus,
            memory_mib,
            hypervisor: host.hypervisor_name(),
            // 브리지 목록을 얻지 못하면 기록된 목록을 그대로 둔다.
            bridges: bridges.clone().unwrap_or_else(|| status.bridges.clone()),
            arch: Some(host.profile.arch),
            // 풀 상태는 풀 새로 고침이 쓴다. 이미 적힌 것을 지우지 않는다.
            pools: std::mem::take(&mut status.pools),
        };
        true
    })
    .await
}

/// 위트니스의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 아키텍처만 기록한다. VM을 돌리지
/// 않으므로 용량·하이퍼바이저·브리지·풀은 비운다.
pub async fn register_witness(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    ensure_node(cluster, config).await?;
    update_node_status(cluster, |status| {
        *status = NodeStatus {
            online: true,
            arch: Some(Arch::native()),
            ..NodeStatus::default()
        };
        true
    })
    .await
}

/// 브리지가 바뀌었으면(관리자가 만들거나 지움) 기록한다. 바뀌지 않았거나 목록을 얻지 못하면
/// 쓰지 않는다.
pub async fn refresh_bridges(cluster: &Cluster, host: &Host) -> anyhow::Result<()> {
    let Some(bridges) = host.bridges().await else {
        return Ok(());
    };
    update_node_status(cluster, |status| {
        if status.bridges == bridges {
            return false;
        }
        tracing::info!(?bridges, "bridges changed");
        status.bridges = bridges.clone();
        true
    })
    .await
}

/// 정상 종료할 때 오프라인으로 표시한다. 실패해도 종료는 계속한다.
pub async fn mark_offline(cluster: &Cluster) {
    let result = update_node_status(cluster, |status| {
        status.online = false;
        true
    })
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "cannot mark node offline");
    }
}

/// 이 노드의 status를 `change`로 고쳐 쓴다(`false`면 쓰지 않는다). status의 다른 필드는 다른
/// 작업(풀 새로 고침)도 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 다시 고친다.
async fn update_node_status(
    cluster: &Cluster,
    mut change: impl FnMut(&mut NodeStatus) -> bool,
) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    loop {
        let Some(node) = cluster.get::<NodeSpec>(id)? else {
            return Ok(());
        };
        let mut status = node.status;
        if !change(&mut status) {
            return Ok(());
        }
        match cluster
            .update_status_checked::<NodeSpec>(id, node.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use async_trait::async_trait;
    use openraft::{BasicNode, ServerState};
    use opl_cluster::ClusterConfig;
    use opl_cluster::memnet::MemRouter;
    use opl_driver::{DriverError, HostNet, Tap};
    use opl_types::node::{BridgeInfo, PoolState};

    /// 정해 둔 브리지를 돌려준다. `None`이면 `ip`가 실패한 것처럼 오류를 낸다.
    #[derive(Debug)]
    struct FakeNet(Option<Vec<BridgeInfo>>);

    #[async_trait]
    impl HostNet for FakeNet {
        async fn bridges(&self) -> Result<Vec<BridgeInfo>, DriverError> {
            self.0.clone().ok_or(DriverError::ToolFailed {
                tool: "ip",
                stderr: "boom".into(),
            })
        }
        async fn create_tap(&self, _: &Tap) -> Result<(), DriverError> {
            unimplemented!()
        }
        async fn delete_vm_taps(&self, _: Uuid) -> Result<(), DriverError> {
            unimplemented!()
        }
    }

    #[test]
    fn node_id_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let id = node_id(dir.path()).unwrap();
        assert_eq!(node_id(dir.path()).unwrap(), id);
        std::fs::write(dir.path().join(NODE_ID_FILE), "garbage").unwrap();
        assert!(node_id(dir.path()).is_err());
    }

    /// 멤버가 이 노드 하나인 클러스터. 리더가 될 때까지 기다린다.
    pub(crate) async fn single_node(dir: &Path) -> (Cluster, NodeId) {
        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir), router.network(id))
            .await
            .unwrap();
        router.register(id, cluster.raft().clone());
        let members = BTreeMap::from([(id, BasicNode::new(id.to_string()))]);
        cluster.initialize(members).await.unwrap();
        cluster
            .raft()
            .wait(Some(Duration::from_secs(10)))
            .metrics(|m| m.state == ServerState::Leader, "leader")
            .await
            .unwrap();
        (cluster, id)
    }

    fn test_config() -> Config {
        toml::from_str(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn registration_records_the_host_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        // 컴파일된 아키텍처가 아니라 HostProfile의 것을 기록한다.
        let mut host = Host::from_config(&config).unwrap();
        host.net = None;
        for arch in [Arch::X86_64, Arch::Aarch64] {
            host.profile.arch = arch;
            register_node(&cluster, &config, &host).await.unwrap();
            let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
            assert_eq!(node.status.arch, Some(arch));
        }
        cluster.shutdown().await.unwrap();
    }

    /// 위트니스는 역할과 온라인, 아키텍처만 기록하고 VM을 돌리는 데 쓰는 값은 비운다.
    #[tokio::test]
    async fn witness_registration_records_only_the_role_and_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config: Config =
            toml::from_str("name = \"w\"\naddress = \"h\"\nwitness = true").unwrap();
        register_witness(&cluster, &config).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.name, "w");
        assert!(node.spec.witness);
        assert_eq!(node.spec.address, "h:8006");
        assert_eq!(
            node.status,
            NodeStatus {
                online: true,
                arch: Some(Arch::native()),
                ..NodeStatus::default()
            }
        );

        mark_offline(&cluster).await;
        register_witness(&cluster, &config).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(node.spec.witness);
        assert!(node.status.online);
        cluster.shutdown().await.unwrap();
    }

    /// 합류할 때 정한 역할과 설정의 `witness`가 다르면 시작하지 않는다. Node 리소스가 없으면 본다.
    #[tokio::test]
    async fn a_role_change_in_the_config_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let witness: Config =
            toml::from_str("name = \"w\"\naddress = \"h\"\nwitness = true").unwrap();
        let normal = test_config();
        check_role(&cluster, &witness).unwrap();
        check_role(&cluster, &normal).unwrap();

        register_witness(&cluster, &witness).await.unwrap();
        check_role(&cluster, &witness).unwrap();
        let err = check_role(&cluster, &normal).unwrap_err().to_string();
        assert!(
            err.contains("joined as a witness but opld.toml says witness = false"),
            "{err}"
        );

        let NodeId(id) = cluster.id();
        let node = cluster.get::<NodeSpec>(id).unwrap().unwrap();
        let spec = NodeSpec {
            witness: false,
            ..node.spec
        };
        cluster
            .update_spec(id, None, node.labels, &spec)
            .await
            .unwrap();
        check_role(&cluster, &normal).unwrap();
        let err = check_role(&cluster, &witness).unwrap_err().to_string();
        assert!(
            err.contains("joined as a non-witness node but opld.toml says witness = true"),
            "{err}"
        );
        cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_bridge_listing_keeps_the_recorded_bridges() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let host = |bridges: Option<Vec<BridgeInfo>>| Host {
            net: Some(Arc::new(FakeNet(bridges))),
            ..Host::from_config(&config).unwrap()
        };
        let br0 = vec![BridgeInfo {
            name: "br0".into(),
            vlan_filtering: false,
        }];
        let (ok, failing) = (host(Some(br0.clone())), host(None));
        let recorded = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .bridges
        };

        // 새 노드는 목록을 얻지 못하면 브리지가 없다.
        register_node(&cluster, &config, &failing).await.unwrap();
        assert_eq!(recorded(), vec![]);
        register_node(&cluster, &config, &ok).await.unwrap();
        assert_eq!(recorded(), br0);
        // 이미 기록된 브리지는 목록을 얻지 못해도 지우지 않는다.
        refresh_bridges(&cluster, &failing).await.unwrap();
        assert_eq!(recorded(), br0);
        register_node(&cluster, &config, &failing).await.unwrap();
        assert_eq!(recorded(), br0);
        refresh_bridges(&cluster, &host(Some(vec![])))
            .await
            .unwrap();
        assert_eq!(recorded(), vec![]);
        cluster.shutdown().await.unwrap();
    }

    /// 풀 새로 고침이 쓴 `pools`를 브리지 기록과 오프라인 표시가 지우지 않는다. 읽은 뒤 쓰기 전에 다른
    /// 쪽이 status를 써도 그 값을 지키며 다시 쓴다.
    #[tokio::test(flavor = "multi_thread")]
    async fn node_status_writes_keep_pools_written_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config).unwrap();
        host.net = None;
        register_node(&cluster, &config, &host).await.unwrap();
        let pools = BTreeMap::from([("local".to_string(), PoolState::default())]);
        // 처음 고칠 때 풀 새로 고침이 끼어든다.
        let mut first = true;
        update_node_status(&cluster, |status| {
            if std::mem::take(&mut first) {
                let mut meanwhile = status.clone();
                meanwhile.pools = pools.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(cluster.update_status::<NodeSpec>(id.0, &meanwhile))
                })
                .unwrap();
            }
            status.bridges = vec![BridgeInfo {
                name: "br0".into(),
                vlan_filtering: false,
            }];
            true
        })
        .await
        .unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.pools, pools);
        assert_eq!(node.status.bridges.len(), 1);

        mark_offline(&cluster).await;
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(!node.status.online);
        assert_eq!(node.status.pools, pools);

        // 다시 시작해 등록해도 풀 새로 고침이 먼저 쓴 pools를 지우지 않는다.
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(node.status.online);
        assert_eq!(node.status.pools, pools);
        cluster.shutdown().await.unwrap();
    }
}
```

`crates/opld/src/config.rs`에서

```rust
    pub address: String,
    /// API(HTTPS)를 받을 소켓 주소
```

을 다음으로 바꾼다:

```rust
    pub address: String,
    /// 위트니스 노드: VM을 돌리지 않고 선거에 나서지 않는 Raft 투표 멤버. 합류만 할 수 있고, HTTPS
    /// API를 열지 않으며 `[hypervisor]`·`[pools]`·`[migration]`을 쓰지 않는다(QEMU가 없어도 된다).
    #[serde(default)]
    pub witness: bool,
    /// API(HTTPS)를 받을 소켓 주소
```

`crates/opld/src/config.rs`에서

```rust
        let c = parse("name = \"node1\"\naddress = \"10.0.0.5\"");
        assert_eq!(c.listen, "0.0.0.0:8006".parse().unwrap());
```

을 다음으로 바꾼다:

```rust
        let c = parse("name = \"node1\"\naddress = \"10.0.0.5\"");
        assert!(!c.witness);
        assert_eq!(c.listen, "0.0.0.0:8006".parse().unwrap());
```

`crates/opld/src/config.rs`에서

```rust
            ["10.0.0.5", "localhost", "127.0.0.1", "::1"]
        );
    }
```

을 다음으로 바꾼다:

```rust
            ["10.0.0.5", "localhost", "127.0.0.1", "::1"]
        );
    }

    #[test]
    fn witness_is_a_top_level_flag() {
        let c = parse(
            "name = \"w\"\naddress = \"h\"\nwitness = true\n[hypervisor]\nqemu_binary = \"/nonexistent/qemu\"",
        );
        assert!(c.witness);
        c.validate().unwrap();
    }
```

`crates/opld/src/join.rs`에서

```rust
            csr_pem,
            witness: false,
        })
```

을 다음으로 바꾼다:

```rust
            csr_pem,
            witness: config.witness,
        })
```

`crates/opld/src/main.rs`에서

```rust
//! 것을 띄우고, `oplctl cluster leave`로 빠지면 다시 기다린다.
//!
```

을 다음으로 바꾼다:

```rust
//! 것을 띄우고, `oplctl cluster leave`로 빠지면 다시 기다린다.
//!
//! 위트니스(설정 `witness = true`)는 VM을 돌리지 않는 Raft 투표 멤버다. 합류만 할 수 있고, 멤버가
//! 되면 클러스터 저장소와 노드 간 서버만 띄운다.
//!
```

`crates/opld/src/main.rs`에서

```rust
        }
        Request::Init {
```

을 다음으로 바꾼다:

```rust
        }
        Request::Init { reply, .. } if config.witness => {
            let _ = reply.send(Err(ControlError::conflict(member::WITNESS_CANNOT_INIT)));
            return None;
        }
        Request::Init {
```

`crates/opld/src/main.rs`에서

```rust
/// API와 노드 간 포트를 잠깐 묶어 본다. 쓰이고 있으면 init/join을 시작하기 전에 실패한다.
fn check_ports(config: &Config) -> anyhow::Result<()> {
    for addr in [config.listen, config.cluster_listen] {
        std::net::TcpListener::bind(addr).with_context(|| format!("cannot listen on {addr}"))?;
```

을 다음으로 바꾼다:

```rust
/// API와 노드 간 포트를 잠깐 묶어 본다. 쓰이고 있으면 init/join을 시작하기 전에 실패한다.
/// 위트니스는 API를 열지 않는다.
fn check_ports(config: &Config) -> anyhow::Result<()> {
    let api = (!config.witness).then_some(config.listen);
    for addr in api.into_iter().chain([config.cluster_listen]) {
        std::net::TcpListener::bind(addr).with_context(|| format!("cannot listen on {addr}"))?;
```

`crates/opld/src/main.rs`에서

```rust
        _ = tokio::signal::ctrl_c() => {}
    }
}
```

을 다음으로 바꾼다:

```rust
        _ = tokio::signal::ctrl_c() => {}
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use tokio::sync::oneshot;

    use super::*;

    #[tokio::test]
    async fn a_witness_refuses_to_start_a_cluster() {
        let dir = tempfile::tempdir().unwrap();
        let mut config: Config =
            toml::from_str("name = \"w\"\naddress = \"127.0.0.1\"\nwitness = true").unwrap();
        config.data_dir = dir.path().to_path_buf();
        let (reply, rx) = oneshot::channel();
        let req = Request::Init {
            cluster_name: "c".into(),
            reply,
        };
        assert!(become_member(&config, Uuid::now_v7(), req).await.is_none());
        let e = rx.await.unwrap().unwrap_err();
        assert_eq!(e.status, StatusCode::CONFLICT);
        assert_eq!(
            e.message,
            "a witness cannot start a cluster; join an existing cluster instead"
        );
        assert!(!member::pki_dir(&config).exists());
    }
}
```

`crates/opld/src/member.rs` 전체:

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리, 노드별 풀 상태. 위트니스는 노드 간 서버와 클러스터
//! 저장소, 승격, Node 등록만 띄운다.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::serve::ListenerExt;
use openraft::BasicNode;
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{
    AppState, ClusterJanitor, IncomingConfig, IncomingMigrations, SnapshotScheduler, TaskRunner,
    TaskRunnerConfig, UefiVarsFetcher,
};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{CATCH_UP_TIMEOUT, Cluster, ClusterConfig, ClusterError, Typed};
use opl_driver::{
    DirStorage, Hypervisor, LvmSharedStorage, LvmThinStorage, QemuHypervisor, RbdStorage, Storage,
    Storages, SystemLvm, SystemRbd, SystemZfs, ZfsStorage, migration_tls_dir, write_migration_tls,
};
use opl_peer::pki::{ClusterCa, NodeTls, PKI_DIR, fingerprint};
use opl_peer::tls::{TlsListener, api_server_config};
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_reconciler::{FirmwareFetcher, Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
use opl_types::vm::VmSpec;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::bootstrap;
use crate::config::Config;
use crate::host::Host;
use crate::pools::PoolMounter;

/// 데이터 디렉터리 아래 클러스터 저장소(Raft 로그와 상태)를 두는 디렉터리
pub const CLUSTER_DIR: &str = "cluster";
/// 종료 신호를 받은 뒤 열린 연결(watch, 콘솔)이 끝나기를 기다리는 시간
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// 쓰기가 커밋되기를 기다리는 시간. 리더를 잃으면 새 리더를 뽑는 데 보통 선거 제한 시간의 두 배쯤
/// 걸리므로(openraft는 리더 임기 동안 다른 노드에 투표하지 않는다) 그보다 넉넉히 둔다.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// 다시 시작한 노드가 Node 리소스를 다시 기록하는 간격(과반수가 없으면 될 때까지)
const REGISTER_RETRY: Duration = Duration::from_secs(2);
/// 노드의 브리지 목록을 다시 확인하는 간격. 관리자가 브리지를 만들면 이만큼 뒤에 스케줄러가 안다.
const BRIDGE_REFRESH: Duration = Duration::from_secs(60);
/// 다시 시작한 learner가 승격을 다시 요청하는 간격. 실패할 때마다 두 배로 늘려 최대 시간까지 둔다.
const PROMOTE_RETRY_MIN: Duration = Duration::from_secs(5);
const PROMOTE_RETRY_MAX: Duration = Duration::from_secs(60);

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
    /// 합류가 받아들여졌다(이 노드는 이미 learner다). 인증서를 저장한 뒤 `via`(받아 준 노드의
    /// 노드 간 주소)로 투표 멤버 승격을 요청한다.
    Join { via: String },
}

/// 새 클러스터를 만든 결과. 사용자에게 한 번만 보여 준다.
pub struct Created {
    pub admin_password: String,
    pub join_url: String,
    pub ca_fingerprint: String,
}

/// 위트니스가 `oplctl cluster init`을 받았을 때의 답
pub const WITNESS_CANNOT_INIT: &str =
    "a witness cannot start a cluster; join an existing cluster instead";

/// 노드에서 노드 간 통신에 쓰는 Raft 설정. 스냅샷 조각은 JSON으로 가므로 작게 둔다.
fn raft_config(witness: bool) -> openraft::Config {
    openraft::Config {
        cluster_name: "opl".into(),
        // 위트니스는 투표는 하지만 후보가 되지 않는다. 리더 일은 늘 일반 노드가 한다.
        enable_elect: !witness,
        heartbeat_interval: 200,
        election_timeout_min: 1000,
        election_timeout_max: 2000,
        snapshot_max_chunk_size: 1 << 20,
        // 스냅샷 조각마다 걸리는 제한 시간(밀리초). 기본 200ms로는 JSON으로 부푼 1MiB 조각(3.5MiB쯤)이
        // 바쁜 노드에서 끝나지 못할 수 있다.
        install_snapshot_timeout: 10_000,
        ..Default::default()
    }
}

struct Server<E> {
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<(), E>>,
}

/// 위트니스가 띄우지 않는 것: HTTPS API와 VM·풀을 다스리는 일, 리더의 일
struct Services {
    api: Server<std::io::Error>,
    reconciler: Reconciler,
    runner: TaskRunner,
    monitor: NodeMonitor,
    schedule: SnapshotScheduler,
    janitor: ClusterJanitor,
    pools: PoolMounter,
}

impl Services {
    async fn shutdown(mut self) {
        let _ = self.api.stop.send(());
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.api.task)
            .await
            .is_err()
        {
            self.api.task.abort();
        }
        self.monitor.shutdown().await;
        self.schedule.shutdown().await;
        self.janitor.shutdown().await;
        self.pools.shutdown().await;
        self.runner.shutdown().await;
        self.reconciler.shutdown().await;
    }
}

pub struct Member {
    config: Config,
    cluster: Arc<Cluster>,
    peer: Server<opl_peer::ServeError>,
    /// 위트니스면 없다.
    services: Option<Services>,
    register: JoinHandle<()>,
    promotion: JoinHandle<()>,
    /// 서버 태스크가 끝나면 그 이유가 온다.
    exited: mpsc::Receiver<String>,
}

impl Member {
    /// 멤버로서 시작한다. 인증서가 있으면 멤버로 보므로, 처음 멤버가 되는 경우 `Init`은 마지막에
    /// 노드 인증서를 저장해 도중에 실패하면 이 노드는 계속 미초기화 상태다. `Join`은 이미 learner로
    /// 더해졌으므로 승격을 요청하기 전에 저장한다. 그 뒤에 실패하거나 죽어도 클러스터가 투표 멤버로
    /// 세는 노드가 미초기화로 남지 않고, 다시 시작하면(`Restart`) 승격을 다시 요청한다.
    pub async fn start(
        config: &Config,
        node: Uuid,
        tls: NodeTls,
        bootstrap: Bootstrap,
    ) -> anyhow::Result<(Self, Option<Created>)> {
        let id = NodeId(node);
        let peers = Arc::new(Peers::new(&tls));
        let mut cluster_config = ClusterConfig::new(id, cluster_dir(config));
        cluster_config.raft = raft_config(config.witness);
        cluster_config.write_timeout = WRITE_TIMEOUT;
        let cluster = Arc::new(
            Cluster::open(cluster_config, GrpcNetwork::new(peers.clone()))
                .await?
                .with_forwarder(peers.clone()),
        );
        // 역할은 합류할 때 정해진다. 설정만 바꿔 다시 시작하면 아무것도 띄우지 않는다.
        if matches!(bootstrap, Bootstrap::Restart)
            && let Err(e) = bootstrap::check_role(&cluster, config)
        {
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        if config.witness {
            let member = Self::start_witness(config, cluster, peers, tls, bootstrap).await?;
            return Ok((member, None));
        }
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
        let host = match Host::from_config(config) {
            Ok(host) => host,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        let hv: Arc<dyn Hypervisor> = match QemuHypervisor::new(host.qemu_config(config)).await {
            Ok(hv) => Arc::new(hv),
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e.into());
            }
        };
        // QEMU가 마이그레이션 TLS에 쓰는 노드 인증서. 드라이버의 state_dir은 데이터 디렉터리다(host.rs).
        let tls_dir = migration_tls_dir(&config.data_dir);
        if let Err(e) = write_migration_tls(&tls_dir, &tls.ca_pem, &tls.cert_pem, &tls.key_pem) {
            let _ = cluster.shutdown().await;
            return Err(
                anyhow::Error::new(e).context(format!("cannot write {}", tls_dir.display()))
            );
        }
        // 계획 20 전에 받은 노드 인증서는 keyEncipherment가 없어 QEMU 8.2 이하의 마이그레이션 TLS가
        // 실패한다. 노드가 클러스터를 나갔다 다시 들어오면 새 인증서를 받는다.
        if let Ok(false) = opl_peer::pki::allows_key_encipherment(&tls.cert_pem) {
            tracing::warn!(
                "this node's certificate lacks the keyEncipherment key usage; live migration over TLS \
                 will fail with QEMU 8.2 or older until the node leaves and rejoins the cluster"
            );
        }
        // NFS 풀의 마운트 지점은 `{data_dir}/pools/{풀 id}`다.
        let mount_root = config.data_dir.join("pools");
        // 풀 백엔드 종류로 나눠 보내는 저장소 하나를 reconciler와 Task 실행기가 함께 쓴다.
        let files: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img, &mount_root));
        let lvm = LvmThinStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let zfs = ZfsStorage::new(&config.qemu_img, Arc::new(SystemZfs));
        let rbd = RbdStorage::new(&config.qemu_img, Arc::new(SystemRbd));
        let lvm_shared = LvmSharedStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let storage: Arc<dyn Storage> = Arc::new(
            Storages::new(&config.qemu_img, files)
                .with_lvm_thin(Arc::new(lvm))
                .with_zfs(Arc::new(zfs))
                .with_rbd(Arc::new(rbd))
                .with_lvm_shared(Arc::new(lvm_shared)),
        );
        let (exited_tx, exited) = mpsc::channel(2);
        // 받는 VM은 원본 노드에 배정된 채이므로 reconciler 없이 받는다.
        let incoming = IncomingMigrations::new(
            cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node,
                mount_root: mount_root.clone(),
                storage: storage.clone(),
                listen_host: config.migration_listen_host(),
                ports: config.migration.ports.clone(),
            },
        );
        // UEFI 변수 파일은 드라이버의 state_dir(데이터 디렉터리)에 있다.
        let service = PeerService::new(cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming))
            .with_uefi_vars(config.data_dir.clone());
        let peer = serve_peer(peer_listener, &tls, service, exited_tx.clone());

        // 여기서 실패하면 노드 간 서버와 저장소를 닫는다.
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let init = matches!(bootstrap, Bootstrap::Init { .. });
        let prepared = async {
            let mut password = None;
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { name, ca } => {
                    let me = BasicNode::new(config.cluster_address());
                    cluster.initialize(BTreeMap::from([(id, me)])).await?;
                    password =
                        Some(bootstrap::create_cluster(&cluster, config, &host, &name, &ca).await?);
                }
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_node(&cluster, config, &host).await?;
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
            if init {
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
        let promotion = spawn_promotion(first_time, &cluster, &peers);
        // 풀 마운터가 QEMU의 rbd 블록 드라이버를 확인한다.
        let qemu_binary = host.profile.qemu_binary.clone();
        // 받은 UEFI 변수 파일의 크기를 이 노드의 템플릿과 견준다.
        let uefi_vars_template = host.profile.uefi_vars_template.clone();
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            tokio::spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_node(&cluster, &config, &host).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
                loop {
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_bridges(&cluster, &host).await {
                        tracing::warn!(error = %e, "cannot record bridges");
                    }
                }
            })
        };
        let mut reconciler_config = ReconcilerConfig::new(node, &mount_root, storage.clone());
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        // 다른 노드에 있는 UEFI 변수 파일을 노드 사이 RPC로 받아 온다(시작, 스냅샷, 복제).
        let firmware: Arc<dyn FirmwareFetcher> = Arc::new(UefiVarsFetcher::new(
            cluster.clone(),
            peers.clone(),
            config.data_dir.clone(),
            uefi_vars_template,
        ));
        reconciler_config.firmware = Some(firmware.clone());
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
        let pools = PoolMounter::spawn(
            cluster.clone(),
            mount_root.clone(),
            qemu_binary,
            config.pools.refresh(),
        );
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
        let mut runner_config = TaskRunnerConfig::new(node);
        runner_config.upload_dir = Some(config.data_dir.join("uploads"));
        // QEMU 드라이버의 state_dir과 같다(host.rs).
        runner_config.state_dir = Some(config.data_dir.clone());
        // 마이그레이션을 보낼 때 대상 노드를 부른다.
        runner_config.peers = Some(peers.clone());
        runner_config.firmware = Some(firmware);
        runner_config.migration_bandwidth =
            (config.migration.bandwidth_mib > 0).then_some(config.migration.bandwidth_mib << 20);
        runner_config.migration_timeout = config.migration.timeout;
        let runner = TaskRunner::spawn(
            cluster.clone(),
            storage,
            hv,
            reconciler.handle(),
            runner_config,
        )
        .await;
        let monitor = NodeMonitor::spawn(cluster.clone(), peers, NodeMonitorConfig::default());

        let listener = TlsListener::new(api_listener, api_tls)?;
        tracing::info!(
            node = %node,
            api = %config.listen,
            cluster = %config.cluster_listen,
            "opld is ready"
        );
        let (stop, rx) = oneshot::channel::<()>();
        let state = Arc::new(state);
        let schedule = SnapshotScheduler::spawn(state.clone(), config.schedule.scheduler_config());
        let janitor = ClusterJanitor::spawn(state.clone(), config.janitor.janitor_config());
        let app = opl_api::router(state).merge(crate::web::router());
        let task = tokio::spawn(async move {
            // 연결 주소(클라이언트 IP)를 시도 제한에 넘긴다. axum은 `TapIo`로 감싼 리스너에
            // `ConnectInfo<SocketAddr>`를 만들어 주므로 아무 일도 하지 않는 tap을 붙인다.
            let listener = listener.tap_io(|_| {});
            let r = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
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
            services: Some(Services {
                api: Server { stop, task },
                reconciler,
                runner,
                monitor,
                schedule,
                janitor,
                pools,
            }),
            register,
            promotion,
            exited,
        };
        Ok((member, created))
    }

    /// 위트니스로 시작한다. 노드 간 서버(하이퍼바이저 없음)와 승격, Node 등록만 띄우고 QEMU를 찾지
    /// 않는다. 위트니스는 클러스터를 만들 수 없다(`Init`은 main이 먼저 거부한다).
    async fn start_witness(
        config: &Config,
        cluster: Arc<Cluster>,
        peers: Arc<Peers>,
        tls: NodeTls,
        bootstrap: Bootstrap,
    ) -> anyhow::Result<Self> {
        let peer_listener = match TcpListener::bind(config.cluster_listen)
            .await
            .with_context(|| format!("cannot listen on {}", config.cluster_listen))
        {
            Ok(listener) => listener,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        let (exited_tx, exited) = mpsc::channel(2);
        // 마이그레이션과 UEFI 변수는 받지 않는다.
        let service = PeerService::new(cluster.clone(), None);
        let peer = serve_peer(peer_listener, &tls, service, exited_tx);
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let prepared = async {
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { .. } => anyhow::bail!(WITNESS_CANNOT_INIT),
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_witness(&cluster, config).await?;
                }
            }
            anyhow::Ok(())
        };
        if let Err(e) = prepared.await {
            let _ = peer.stop.send(());
            let _ = peer.task.await;
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        let promotion = spawn_promotion(first_time, &cluster, &peers);
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            tokio::spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_witness(&cluster, &config).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
            })
        };
        tracing::info!(
            node = %cluster.id(),
            cluster = %config.cluster_listen,
            "opld is ready as a witness"
        );
        Ok(Member {
            config: config.clone(),
            cluster,
            peer,
            services: None,
            register,
            promotion,
            exited,
        })
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

    /// 클러스터에서 빠진다. 이 노드에 배정된 VM이 있거나, 남는 투표 멤버가 없거나 모두 위트니스면
    /// 거부한다.
    /// 성공하면 `close(true)`로 닫는다.
    pub async fn leave(&self) -> anyhow::Result<()> {
        let me = self.cluster.id();
        let (_, nodes) = self.cluster.list::<NodeSpec>()?;
        check_leave(me, &self.cluster.members().voters, &nodes)?;
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
        if let Some(services) = self.services.take() {
            services.shutdown().await;
        }
        self.register.abort();
        self.promotion.abort();
        if !left {
            bootstrap::mark_offline(&self.cluster).await;
        }
        let _ = self.peer.stop.send(());
        // 오래 가는 콘솔 중계 스트림이 있으면 서버가 끝나지 않으므로 기다리는 시간을 제한한다.
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.peer.task)
            .await
            .is_err()
        {
            self.peer.task.abort();
        }
        let shutdown = self.cluster.shutdown().await;
        if left {
            // 이미 클러스터에서 빠졌으므로 저장소를 닫지 못했어도 지운다. 남기면 빠진 멤버로 다시 뜬다.
            if let Err(e) = &shutdown {
                tracing::warn!(error = %e, "cannot shut down the cluster store; removing it anyway");
            }
            match std::fs::remove_dir_all(pki_dir(&self.config)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                other => other?,
            }
            remove_cluster_data(&self.config)?;
        }
        shutdown?;
        Ok(())
    }
}

/// 이 노드가 빠진 뒤에도 리더가 될 수 있는 투표 멤버가 남는지 본다. 위트니스는 후보가 되지 않으므로
/// 위트니스만 남으면 클러스터에 리더가 영영 생기지 않는다. Node 리소스가 없는 투표 멤버는 일반 노드로
/// 본다(`get_cluster`처럼).
fn check_leave(
    me: NodeId,
    voters: &BTreeMap<NodeId, String>,
    nodes: &[Typed<NodeSpec>],
) -> anyhow::Result<()> {
    let mut rest = voters.keys().filter(|&&id| id != me).peekable();
    if rest.peek().is_none() {
        anyhow::bail!(
            "this node is the only voting member; stop opld and remove its data directory instead"
        );
    }
    let witness = |id: &NodeId| nodes.iter().any(|n| n.id == id.0 && n.spec.witness);
    if rest.all(witness) {
        anyhow::bail!(
            "the remaining voting members are all witnesses, which cannot lead; add another \
             non-witness node first"
        );
    }
    Ok(())
}

/// 노드 간 서버를 띄운다. 서버가 스스로 멈추면 `exited`로 알린다.
fn serve_peer(
    listener: TcpListener,
    tls: &NodeTls,
    service: PeerService,
    exited: mpsc::Sender<String>,
) -> Server<opl_peer::ServeError> {
    let (stop, rx) = oneshot::channel::<()>();
    let tls = tls.clone();
    let task = tokio::spawn(async move {
        let r = opl_peer::serve(listener, &tls, service, async {
            let _ = rx.await;
        })
        .await;
        let _ = exited.send(format!("cluster server stopped: {r:?}")).await;
        r
    });
    Server { stop, task }
}

/// 다시 시작한 노드는 투표 멤버가 될 때까지 승격을 다시 요청한다. 처음 멤버가 된 노드는 이미 했다.
fn spawn_promotion(first_time: bool, cluster: &Arc<Cluster>, peers: &Arc<Peers>) -> JoinHandle<()> {
    if first_time {
        return tokio::spawn(async {});
    }
    let (cluster, peers) = (cluster.clone(), peers.clone());
    tokio::spawn(async move { promote_until_voter(&cluster, &peers).await })
}

/// 클러스터 저장소(Raft 로그와 상태)를 지운다. 없으면 그대로 성공한다.
pub fn remove_cluster_data(config: &Config) -> std::io::Result<()> {
    let dir = cluster_dir(config);
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 합류하다 투표 멤버가 되기 전에 멈춘 노드는 learner로 남아 있다. 다시 시작하면 투표 멤버가 될
/// 때까지, 또는 클러스터에서 빠진 것을 알 때까지 승격을 다시 요청한다.
async fn promote_until_voter(cluster: &Cluster, peers: &Peers) {
    let id = cluster.id();
    // 저장한 멤버십이 지표에 반영되기를 기다린다. 합류 직후 죽었으면 리더가 보내 줄 때까지 모른다.
    while cluster
        .raft()
        .wait(Some(Duration::from_secs(10)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .nodes()
                    .any(|(n, _)| *n == id)
            },
            "this node is in the membership",
        )
        .await
        .is_err()
    {
        tracing::info!("this node is not in the cluster membership it knows yet; still waiting");
    }
    let mut retry = PROMOTE_RETRY_MIN;
    let mut asked = false;
    loop {
        let members = cluster.members();
        if members.voters.contains_key(&id) {
            if asked {
                tracing::info!("this node became a voting member");
            } else {
                tracing::info!("this node is a voting member");
            }
            return;
        }
        if !members.learners.contains_key(&id) {
            tracing::info!("this node was removed from the cluster; not asking to be promoted");
            return;
        }
        tracing::info!("this node is not a voting member yet; asking to be promoted");
        asked = true;
        match promote(cluster, peers, None).await {
            Ok(()) => {}
            Err(e) if matches!(e.downcast_ref(), Some(ClusterError::Rejected(_))) => {
                tracing::info!(
                    error = format!("{e:#}"),
                    "this node was removed from the cluster; not asking to be promoted"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    error = format!("{e:#}"),
                    "cannot become a voting member; will retry"
                );
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(PROMOTE_RETRY_MAX);
            }
        }
    }
}

/// 합류한 노드가 로그를 따라잡으면 투표 멤버로 올려 달라고 리더에게 요청하고, 이 노드도 그것을
/// 적용할 때까지 기다린다. 요청을 받은 노드가 리더가 아니면 알려 준 리더로 다시 보낸다. `via`가
/// 없으면(다시 시작한 노드) 이 노드가 아는 리더나 투표 멤버에게 보낸다.
async fn promote(cluster: &Cluster, peers: &Peers, via: Option<&str>) -> anyhow::Result<()> {
    let id = cluster.id();
    let deadline = tokio::time::Instant::now() + CATCH_UP_TIMEOUT + Duration::from_secs(30);
    let mut redirect: Option<String> = None;
    loop {
        // 다시 시작한 직후에는 리더를 아직 모를 수 있다. 알 때까지 기다린다.
        let Some(addr) = redirect
            .take()
            .or_else(|| via.map(str::to_string))
            .or_else(|| voter_addr(cluster))
        else {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("cannot become a voting member: no voting member is known");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        };
        match peers
            .leader_request(&addr, LeaderRequest::AddVoter { id })
            .await
        {
            Ok(_) => break,
            Err(ClusterError::NotLeader {
                leader_addr: Some(leader),
                ..
            }) => redirect = Some(leader),
            // 리더의 멤버십에 learner로 없다(클러스터에서 빠졌다). 다시 시도해도 소용없다.
            Err(e @ ClusterError::Rejected(_)) => {
                return Err(e).context("this node is not in the cluster membership");
            }
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

/// 이 노드가 아는 리더의 주소, 모르면 아무 투표 멤버의 주소
fn voter_addr(cluster: &Cluster) -> Option<String> {
    let members = cluster.members();
    members
        .leader
        .and_then(|leader| members.voters.get(&leader))
        .or_else(|| members.voters.values().next())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::resource::Resource;

    #[test]
    fn a_witness_does_not_stand_for_election() {
        assert!(raft_config(false).enable_elect);
        assert!(!raft_config(true).enable_elect);
    }

    /// 남는 투표 멤버가 없거나 모두 위트니스면(리더가 될 수 없다) 빠지지 않는다.
    #[test]
    fn leaving_keeps_a_voter_that_can_lead() {
        let node = |witness: bool| {
            let mut node = Resource::new(
                "n",
                NodeSpec {
                    address: "h:8006".into(),
                    maintenance: false,
                    witness,
                },
            );
            node.id = Uuid::now_v7();
            node
        };
        let (a, b, w) = (node(false), node(false), node(true));
        let id = |n: &Typed<NodeSpec>| NodeId(n.id);
        let voters = |nodes: &[&Typed<NodeSpec>]| {
            nodes
                .iter()
                .map(|n| (id(n), String::new()))
                .collect::<BTreeMap<_, _>>()
        };
        let nodes = [a.clone(), b.clone(), w.clone()];

        // 혼자 남은 투표 멤버
        let only = check_leave(id(&a), &voters(&[&a]), &nodes).unwrap_err();
        assert!(only.to_string().contains("only voting member"), "{only}");
        // 위트니스만 남는다.
        let err = check_leave(id(&a), &voters(&[&a, &w]), &nodes).unwrap_err();
        assert!(err.to_string().contains("all witnesses"), "{err}");
        // 일반 노드가 남으면 누구든 빠질 수 있다. 위트니스도.
        check_leave(id(&a), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &w]), &nodes).unwrap();
        // Node 리소스가 없는 투표 멤버는 일반 노드로 본다(get_cluster처럼).
        check_leave(id(&a), &voters(&[&a, &b, &w]), &[a.clone(), w.clone()]).unwrap();
    }
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 759개, 실패 0

Run: `cargo test -p opl-cluster --test witness`
Expected: PASS 2개

Run: `cargo test -p opld`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(opld): 위트니스 멤버 — 설정 witness, 선거에 나서지 않는 축소 멤버"
```

---

### Task 3: 위트니스 표시: oplctl 열과 HA 경고, 웹 배지

`GET /cluster`의 멤버에 `witness`(Node의 `spec.witness`, Node가 없으면 false). oplctl `node list`·`cluster status`에 `WITNESS` 열, 투표 멤버가 3 이상인데 위트니스가 아닌 투표 멤버가 둘보다 적으면 `warning: HA needs at least two non-witness nodes`(2노드 경고는 그대로). 웹: 클러스터 페이지 멤버 행과 대시보드 노드 표에 위트니스 배지, 멤버 설명 문구. en·ko.

**Files:**
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/tests/cluster.rs`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `web/src/cluster/ClusterPage.test.tsx`
- Modify: `web/src/cluster/ClusterPage.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/nodes/NodesPage.test.tsx`
- Modify: `web/src/nodes/NodesPage.tsx`
- Modify: `web/src/pages/DashboardPage.test.tsx`
- Modify: `web/src/pages/DashboardPage.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/cluster.rs`에서

```rust
    pub online: bool,
}
```

을 다음으로 바꾼다:

```rust
    pub online: bool,
    /// 위트니스 노드인지(VM을 돌리지 않는 투표 멤버)
    pub witness: bool,
}
```

`crates/opl-api/src/cluster.rs`에서

```rust
            online: node.is_some_and(|n| n.status.online),
        }
```

을 다음으로 바꾼다:

```rust
            online: node.is_some_and(|n| n.status.online),
            witness: node.is_some_and(|n| n.spec.witness),
        }
```

`crates/opl-api/tests/cluster.rs`에서

```rust
    assert!(joined.spec.witness);
    assert!(joined.status.online);

    let (req, node) = join_request(&new_token(&env).await, "node2");
```

을 다음으로 바꾼다:

```rust
    assert!(joined.spec.witness);
    assert!(joined.status.online);
    // 클러스터 멤버 목록에도 위트니스인지가 보인다.
    let (_, view) = env.admin().await.get("/cluster").await;
    let witness_of = |id: Uuid| {
        view["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id.to_string())
            .unwrap()["witness"]
            .clone()
    };
    assert_eq!(witness_of(witness), true, "{view}");
    assert_eq!(witness_of(env.node), false, "{view}");

    let (req, node) = join_request(&new_token(&env).await, "node2");
```

`crates/oplctl/src/cli.rs`에서

```rust
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
```

을 다음으로 바꾼다:

```rust
                let list = client.get("/nodes").await?;
                print_list(out, json_out, &list, NODE_HEADERS, node_row)?;
            }
```

`crates/oplctl/src/cli.rs`에서

```rust
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
```

을 다음으로 바꾼다:

```rust
            }
            print_cluster_status(out, &view)?;
        }
```

`crates/oplctl/src/cli.rs`에서

```rust
        table(out, headers, &rows)?;
    }
```

을 다음으로 바꾼다:

```rust
        table(out, headers, &rows)?;
    }
    Ok(())
}

/// `node list`의 열
const NODE_HEADERS: &[&str] = &[
    "NAME",
    "ID",
    "ADDRESS",
    "ONLINE",
    "CPUS",
    "MEMORY",
    "MAINTENANCE",
    "WITNESS",
];

/// `node list`의 한 행
fn node_row(n: &Value) -> Vec<String> {
    vec![
        cell(&n["name"]),
        cell(&n["id"]),
        cell(&n["spec"]["address"]),
        cell(&n["status"]["online"]),
        cell(&n["status"]["cpus"]),
        bytes(&n["status"]["memory_mib"].as_u64().map(|m| m << 20).into()),
        cell(&n["spec"]["maintenance"]),
        // 옛 서버는 보내지 않는다 — 일반 노드다.
        (n["spec"]["witness"] == true).to_string(),
    ]
}

/// `cluster status`의 표와 경고
fn print_cluster_status(out: &mut (dyn Write + Send), view: &Value) -> anyhow::Result<()> {
    writeln!(out, "cluster: {}", cell(&view["name"]))?;
    writeln!(out, "CA fingerprint: {}", cell(&view["ca_fingerprint"]))?;
    let leader = &view["leader"];
    let members = view["members"].as_array().cloned().unwrap_or_default();
    let rows: Vec<Vec<String>> = members
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
                (&m["id"] == leader).to_string(),
                (m["witness"] == true).to_string(),
            ]
        })
        .collect();
    table(
        out,
        &[
            "NAME", "ID", "ADDRESS", "ROLE", "ONLINE", "LEADER", "WITNESS",
        ],
        &rows,
    )?;
    let voters: Vec<&Value> = members.iter().filter(|m| m["voter"] == true).collect();
    if voters.len() == 2 {
        writeln!(
            out,
            "warning: a two-node cluster stops accepting changes when either node fails; add a third node"
        )?;
    } else if voters.len() >= 3 && voters.iter().filter(|m| m["witness"] != true).count() < 2 {
        // 위트니스는 VM을 돌리지 않으므로 VM을 옮겨 받을 일반 노드가 둘 이상이어야 한다.
        writeln!(out, "warning: HA needs at least two non-witness nodes")?;
    }
```

`crates/oplctl/src/cli.rs`에서

```rust
    #[test]
    fn cli_definition_is_valid() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn node_list_shows_witnesses() {
        let node = |name: &str, witness: Option<bool>| {
            let mut spec = json!({ "address": "10.0.0.1:8006", "maintenance": false });
            if let Some(w) = witness {
                spec["witness"] = json!(w);
            }
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "spec": spec,
                "status": { "online": true, "cpus": 4, "memory_mib": 1024 },
            })
        };
        let rows: Vec<_> = [
            node("a", Some(false)),
            node("w", Some(true)),
            node("old", None),
        ]
        .iter()
        .map(node_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, NODE_HEADERS, &rows).unwrap();
        // 옛 서버는 witness를 보내지 않는다 — 일반 노드다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  ID      ADDRESS        ONLINE  CPUS  MEMORY  MAINTENANCE  WITNESS\n\
             a     id-a    10.0.0.1:8006  true    4     1.0GiB  false        false\n\
             w     id-w    10.0.0.1:8006  true    4     1.0GiB  false        true\n\
             old   id-old  10.0.0.1:8006  true    4     1.0GiB  false        false\n"
        );
    }

    #[test]
    fn cluster_status_shows_witnesses_and_warns() {
        let member = |name: &str, voter: bool, witness: bool| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": voter,
                "online": true,
                "witness": witness,
            })
        };
        let status = |members: Vec<Value>| {
            let view = json!({
                "name": "lab",
                "ca_fingerprint": "ab:cd",
                "leader": "id-a",
                "node": "id-a",
                "members": members,
            });
            let mut out = Vec::new();
            print_cluster_status(&mut out, &view).unwrap();
            String::from_utf8(out).unwrap()
        };
        // 일반 노드 둘과 위트니스면 투표 멤버가 셋이라 경고가 없다.
        assert_eq!(
            status(vec![
                member("a", true, false),
                member("b", true, false),
                member("w", true, true),
            ]),
            "cluster: lab\nCA fingerprint: ab:cd\n\
             NAME  ID    ADDRESS  ROLE   ONLINE  LEADER  WITNESS\n\
             a     id-a  a:8006   voter  true    true    false\n\
             b     id-b  b:8006   voter  true    false   false\n\
             w     id-w  w:8006   voter  true    false   true\n"
        );
        const TWO: &str = "warning: a two-node cluster";
        const HA: &str = "warning: HA needs at least two non-witness nodes";
        // 투표 멤버 둘이면 위트니스가 있어도 2노드 경고다.
        let out = status(vec![member("a", true, false), member("w", true, true)]);
        assert!(out.contains(TWO) && !out.contains(HA), "{out}");
        // 투표 멤버가 셋 이상이어도 일반 노드가 하나뿐이면 HA 경고다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
        ]);
        assert!(out.contains(HA) && !out.contains(TWO), "{out}");
        // learner는 투표 멤버로 세지 않는다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
            member("b", false, false),
        ]);
        assert!(out.contains(HA), "{out}");
        // 옛 서버는 witness를 보내지 않는다 — 모두 일반 노드로 본다.
        let mut old = member("c", true, false);
        old.as_object_mut().unwrap().remove("witness");
        let out = status(vec![
            member("a", true, false),
            member("b", true, false),
            old,
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    #[test]
    fn cli_definition_is_valid() {
```

`web/src/cluster/ClusterPage.test.tsx`에서

```tsx
  online: true,
  ...extra,
```

을 다음으로 바꾼다:

```tsx
  online: true,
  witness: false,
  ...extra,
```

`web/src/cluster/ClusterPage.test.tsx`에서

```tsx
          member("n3", null, { voter: false, online: false, address: null }),
        ],
```

을 다음으로 바꾼다:

```tsx
          member("n3", null, { voter: false, online: false, address: null }),
          member("n4", "witness-w", { witness: true }),
        ],
```

`web/src/cluster/ClusterPage.test.tsx`에서

```tsx
  expect(joining).toHaveTextContent("Offline");
  expect(screen.queryByRole("button", { name: "Issue a join token" })).not.toBeInTheDocument();
```

을 다음으로 바꾼다:

```tsx
  expect(joining).toHaveTextContent("Offline");
  // 위트니스는 역할 옆에 배지가 붙는다.
  expect(within(table).getByText("witness-w").closest("tr")).toHaveTextContent("VoterWitness");
  expect(within(table).queryAllByText("Witness")).toHaveLength(1);
  expect(screen.queryByRole("button", { name: "Issue a join token" })).not.toBeInTheDocument();
```

`web/src/cluster/ClusterPage.tsx`에서

```tsx
                    : t(m.voter ? "cluster.voter" : "cluster.learner")}
                </TableCell>
```

을 다음으로 바꾼다:

```tsx
                    : t(m.voter ? "cluster.voter" : "cluster.learner")}
                  {m.witness && (
                    <Badge variant="outline" className="ml-2">
                      {t("dashboard.witness")}
                    </Badge>
                  )}
                </TableCell>
```

`web/src/i18n/en.json`에서

```json
    "members": "Members",
    "membersNote": "Voters keep the cluster state. A learner is a node that is still joining.",
    "refreshFailed": "Could not refresh the cluster. {{error}}",
```

을 다음으로 바꾼다:

```json
    "members": "Members",
    "membersNote": "Voters keep the cluster state. A learner is a node that is still joining. A witness votes but runs no VMs.",
    "refreshFailed": "Could not refresh the cluster. {{error}}",
```

`web/src/i18n/en.json`에서

```json
    "maintenance": "Maintenance",
    "failedVms": "Failed virtual machines",
```

을 다음으로 바꾼다:

```json
    "maintenance": "Maintenance",
    "witness": "Witness",
    "failedVms": "Failed virtual machines",
```

`web/src/i18n/ko.json`에서

```json
    "members": "멤버",
    "membersNote": "투표 멤버가 클러스터 상태를 지킵니다. 학습자는 아직 합류 중인 노드입니다.",
    "refreshFailed": "클러스터를 다시 읽지 못했습니다. {{error}}",
```

을 다음으로 바꾼다:

```json
    "members": "멤버",
    "membersNote": "투표 멤버가 클러스터 상태를 지킵니다. 학습자는 아직 합류 중인 노드입니다. 위트니스는 투표만 하고 VM을 돌리지 않습니다.",
    "refreshFailed": "클러스터를 다시 읽지 못했습니다. {{error}}",
```

`web/src/i18n/ko.json`에서

```json
    "maintenance": "유지보수",
    "failedVms": "실패한 가상 머신",
```

을 다음으로 바꾼다:

```json
    "maintenance": "유지보수",
    "witness": "위트니스",
    "failedVms": "실패한 가상 머신",
```

`web/src/nodes/NodesPage.test.tsx`에서

```tsx
  expect(within(a).getAllByRole("cell")[archColumn]).toHaveTextContent("—");
});
```

을 다음으로 바꾼다:

```tsx
  expect(within(a).getAllByRole("cell")[archColumn]).toHaveTextContent("—");
});

it("marks witness nodes", async () => {
  cluster();
  const witness = { ...NODE, id: "n3", name: "node-w", spec: { ...NODE.spec, witness: true } };
  mockLists({ node: [NODE, witness] });
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const w = (await within(table).findByText("node-w")).closest("tr")!;
  expect(within(w).getByText("Witness")).toBeInTheDocument();
  const a = within(table).getByText("node-a").closest("tr")!;
  expect(within(a).queryByText("Witness")).not.toBeInTheDocument();
});
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
                  )}
                </div>
```

을 다음으로 바꾼다:

```tsx
                  )}
                  {node.spec.witness && <Badge variant="outline">{t("dashboard.witness")}</Badge>}
                </div>
```

`web/src/pages/DashboardPage.test.tsx`에서

```tsx

it("shows loading instead of zeros, then errors", async () => {
```

을 다음으로 바꾼다:

```tsx

it("marks witness nodes", async () => {
  const witness = { ...NODE, id: "n3", name: "node-w", spec: { ...NODE.spec, witness: true } };
  mockLists({ node: [NODE, witness], vm: [] });
  server.use(cluster("n1"));
  renderPage(<DashboardPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const w = (await within(table).findByText("node-w")).closest("tr")!;
  expect(within(w).getByText("Witness")).toBeInTheDocument();
  expect(within(table).getAllByText("Witness")).toHaveLength(1);
});

it("shows loading instead of zeros, then errors", async () => {
```

`web/src/pages/DashboardPage.tsx`에서

```tsx
                    )}
                  </TableCell>
```

을 다음으로 바꾼다:

```tsx
                    )}
                    {node.spec.witness && (
                      <Badge variant="outline" className="ml-1">
                        {t("dashboard.witness")}
                      </Badge>
                    )}
                  </TableCell>
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 761개, 실패 0

Run: `cargo test -p opl-api --test cluster`
Expected: PASS

Run: `cargo test -p oplctl`
Expected: PASS

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 313개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat: 위트니스 표시 — oplctl 열과 HA 경고, 웹 배지"
```

---

### Task 4: 실제 시험: 위트니스 클러스터

opld 시험 하네스에 `Daemon::witness(name)`(설정 `witness = true`, 없는 `qemu_binary`) — 위트니스는 API가 없으므로 합류·시작은 제어 소켓의 `state == "member"`를 기다린다. `crates/opld/tests/cluster.rs`의 `a_witness_votes_but_never_leads_or_runs_vms`: 위트니스의 init 거부, 합류 뒤 API 포트가 닫혀 있음, 투표 멤버 3·경고 없음·`spec.witness`, `vm create --node wit`는 `node_unavailable`, 리더를 `kill -9`하면 남은 일반 노드가 리더가 되고 쓰기가 된다, 위트니스를 다시 시작해도 위트니스 멤버, 죽은 노드를 `remove-node --force`로 뺀 뒤 마지막 일반 노드의 탈퇴는 거부, 위트니스의 탈퇴는 된다. QEMU가 필요 없어 이 Mac과 CI에서 기본 시험으로 돈다.

**Files:**
- Modify: `crates/opld/tests/cluster.rs`
- Modify: `crates/opld/tests/common/mod.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opld/tests/cluster.rs`에서

```rust
//! 이어지고, 죽은 노드를 빼고, 노드가 떠났다가 다시 합류한다. QEMU는 쓰지 않는다.
```

을 다음으로 바꾼다:

```rust
//! 이어지고, 죽은 노드를 빼고, 노드가 떠났다가 다시 합류한다. QEMU는 쓰지 않는다.
//! 계획 22: 일반 노드 둘과 위트니스로 만든 클러스터는 리더를 잃어도 남은 일반 노드가 리더가 된다.
//! 위트니스는 다시 시작해도 위트니스이고, 위트니스만 남기는 탈퇴는 거부한다.
```

`crates/opld/tests/cluster.rs`에서

```rust
    nodes[b].stop();
}
```

을 다음으로 바꾼다:

```rust
    nodes[b].stop();
}

/// `oplctl cluster status`에서 리더의 이름. 리더가 없거나 답하지 못하면 `None`
async fn leader_name(d: &Daemon) -> Option<String> {
    let out = d.ctl(&["-o", "json", "cluster", "status"]).await.ok()?;
    let view: Value = serde_json::from_str(&out).ok()?;
    view["members"]
        .as_array()?
        .iter()
        .find(|m| m["id"] == view["leader"])?["name"]
        .as_str()
        .map(str::to_string)
}

#[tokio::test]
async fn a_witness_votes_but_never_leads_or_runs_vms() {
    let mut nodes = vec![Daemon::new("node-a", ""), Daemon::new("node-b", "")];
    // 위트니스의 qemu_binary는 없는 파일이다 — QEMU 없이 돈다.
    let mut w = Daemon::witness("wit");
    for d in &mut nodes {
        d.start().await;
    }
    w.start().await;

    // 위트니스는 클러스터를 만들 수 없다.
    let socket = w.socket();
    let err = w
        .ctl(&["cluster", "init", "--name", "prod", "--socket", &socket])
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("a witness cannot start a cluster"),
        "{err}"
    );

    nodes[0].init("prod").await;
    let password = nodes[0].admin_password.clone().unwrap();
    let fp = nodes[0].ca_fingerprint.clone().unwrap();
    nodes[0].login().await;
    let url = nodes[0].join_url.clone().unwrap();
    nodes[1].join(&url).await.unwrap();
    nodes[1].login_with(&password, &fp).await;
    let url = nodes[0].ctl(&["cluster", "join-token"]).await.unwrap();
    let out = w.join(url.trim()).await.unwrap();
    assert!(out.contains("node wit joined cluster prod"), "{out}");
    // 위트니스는 HTTPS API를 열지 않는다.
    assert!(std::net::TcpStream::connect(("127.0.0.1", w.port)).is_err());

    // 투표 멤버 셋 중 하나가 위트니스다. 일반 노드가 둘이니 경고가 없다.
    assert_eq!(voters(&nodes[0]).await, 3);
    eventually("all members online", || async {
        members(&nodes[0]).await.iter().all(|m| m["online"] == true)
    })
    .await;
    let roles: Vec<(String, bool)> = members(&nodes[0])
        .await
        .iter()
        .map(|m| {
            (
                m["name"].as_str().unwrap().to_string(),
                m["witness"] == true,
            )
        })
        .collect();
    assert!(roles.contains(&("wit".into(), true)), "{roles:?}");
    assert_eq!(roles.iter().filter(|(_, w)| *w).count(), 1, "{roles:?}");
    let status = nodes[0].ctl(&["cluster", "status"]).await.unwrap();
    assert!(!status.contains("warning"), "{status}");
    let node = nodes[0].ctl_json(&["node", "get", "wit"]).await;
    assert_eq!(node["spec"]["witness"], true, "{node}");

    // 위트니스에 VM을 둘 수 없다.
    let err = nodes[0]
        .ctl(&["vm", "create", "web", "--memory", "256", "--node", "wit"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("node_unavailable"), "{err}");

    // 리더(일반 노드)를 kill -9 하면 남은 일반 노드가 리더가 되고 쓰기가 이어진다.
    let leader = leader_name(&nodes[0]).await.unwrap();
    assert_ne!(leader, "wit");
    let dead = nodes.iter().position(|d| d.name == leader).unwrap();
    nodes[dead].kill();
    let alive = &nodes[1 - dead];
    eventually("the other node leads", || async {
        leader_name(alive).await.as_deref() == Some(alive.name.as_str())
    })
    .await;
    let pool = alive.path("pool-after");
    std::fs::create_dir_all(&pool).unwrap();
    alive
        .ctl(&["pool", "create", "after", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();

    // 다시 시작한 위트니스는 위트니스 멤버로 돌아와 온라인이 된다(멈춘 동안은 과반수가 없다).
    w.stop();
    w.start().await;
    let witness_online = || async {
        members(alive).await.iter().any(|m| {
            m["name"] == "wit" && m["voter"] == true && m["witness"] == true && m["online"] == true
        })
    };
    eventually("the restarted witness is online", witness_online).await;

    // 죽은 노드를 빼면 투표 멤버는 일반 노드 하나와 위트니스다. 일반 노드는 빠질 수 없다(위트니스만
    // 남으면 리더가 생기지 않는다). 위트니스는 빠질 수 있다.
    eventually("the killed node is offline", || async {
        members(alive)
            .await
            .iter()
            .any(|m| m["name"] == leader.as_str() && m["online"] == false)
    })
    .await;
    alive
        .ctl(&["cluster", "remove-node", &leader, "--force"])
        .await
        .unwrap();
    assert_eq!(voters(alive).await, 2);
    let socket = alive.socket();
    let err = alive
        .ctl(&["cluster", "leave", "--socket", &socket])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("all witnesses"), "{err}");
    let socket = w.socket();
    let out = w
        .ctl(&["cluster", "leave", "--socket", &socket])
        .await
        .unwrap();
    assert!(out.contains("left the cluster"), "{out}");
    assert_eq!(voters(alive).await, 1);

    nodes[1 - dead].stop();
    w.stop();
}
```

`crates/opld/tests/common/mod.rs` 전체:

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
    /// 위트니스(설정 `witness = true`)는 HTTPS API를 열지 않는다.
    pub witness: bool,
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
        // systemd가 있는 Linux(CI)에서만 `OPL_TEST_SYSTEMD=1`로 QEMU를 스코프로 감싼다.
        let systemd_scope = std::env::var_os("OPL_TEST_SYSTEMD").is_some();
        Self::with_hypervisor(
            name,
            extra_config,
            &format!("systemd_scope = {systemd_scope}"),
        )
    }

    /// `[hypervisor]` 절의 내용을 정해 만든다.
    pub fn with_hypervisor(name: &str, extra_config: &str, hypervisor: &str) -> Self {
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
             data_dir = \"{data}\"\nrun_dir = \"{run}\"\nqemu_share_dir = \"{share}\"\n{extra_config}\n\
             [hypervisor]\n{hypervisor}\n",
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
            witness: false,
            child: None,
            admin_password: None,
            ca_fingerprint: None,
            join_url: None,
        }
    }

    /// 위트니스. QEMU가 없어도 돌아야 하므로 `qemu_binary`는 없는 파일을 가리킨다.
    pub fn witness(name: &str) -> Self {
        let mut d = Self::with_hypervisor(
            name,
            "witness = true",
            "qemu_binary = \"/nonexistent/qemu-system\"",
        );
        d.witness = true;
        d
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

    /// opld를 띄우고 제어 소켓이 응답할 때까지 기다린다. 멤버였던 노드면 API까지 기다린다(위트니스는
    /// 멤버가 될 때까지만).
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
        if member && !self.witness {
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

    /// `oplctl cluster join`. 위트니스는 API가 없으니 제어 소켓이 멤버라고 알릴 때까지 기다린다.
    pub async fn join(&mut self, url: &str) -> anyhow::Result<String> {
        let socket = self.socket();
        let out = self
            .ctl(&["cluster", "join", url, "--socket", &socket])
            .await?;
        if self.witness {
            self.wait_until(|status| status.is_some_and(|s| s["state"] == "member"))
                .await;
        } else {
            self.wait_api().await;
        }
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

    /// `start`한 뒤 아직 멈추지 않았는지
    pub fn is_running(&self) -> bool {
        self.child.is_some()
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

    /// 이 노드가 풀 `pool`을 준비됐다고 알릴 때까지 기다린다. 볼륨 만들기와 배치는 준비된 노드만 고른다.
    pub async fn wait_pool_ready(&self, pool: &str) {
        eventually(&format!("pool {pool} ready on {}", self.name), || async {
            let nodes = self.ctl_json(&["node", "list"]).await;
            nodes.as_array().unwrap().iter().any(|n| {
                n["name"] == self.name.as_str() && n["status"]["pools"][pool]["ready"] == true
            })
        })
        .await;
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // 테스트가 중간에 실패해도 이 데몬이 띄운 QEMU(데몬과 따로 돈다)를 남기지 않는다.
        // 명령줄에 이 임시 디렉터리의 경로가 든 프로세스만 맞는다.
        let _ = Command::new("pkill")
            .arg("-KILL")
            .arg("-f")
            .arg(self.path("run").join("vm").display().to_string())
            .status();
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

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 762개, 실패 0

Run: `cargo test -p opld --test cluster`
Expected: PASS 2개

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "test(opld): 위트니스 클러스터 실제 시험 — 리더를 잃으면 남은 일반 노드가 리더"
```

---

### Task 5: 문서

README에 위트니스 노드 절(설치·`witness = true`·합류, 도는 것과 돌지 않는 것, API 없음, 역할 바꾸기는 탈퇴 뒤 다시 합류, 표시와 경고, 2노드+위트니스 권장), 설계 문서 §2·§6·§7의 "구현:" 메모, 처음 분해 문서 §1.1·§8.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션, 위트니스 노드)
```

`README.md`에서

```markdown

## 웹 UI
```

을 다음으로 바꾼다:

````markdown

## 위트니스 노드

2노드 클러스터는 한 노드만 죽어도 과반수를 잃어 아무것도 바꿀 수 없다. 위트니스는 VM을 돌리지 않고 Raft 투표만
하는 opld다(vSAN 위트니스, corosync-qnetd와 같은 자리). **일반 노드 둘과 위트니스 하나**를 권한다. 투표 멤버가
셋이므로 어느 한 노드가 죽어도 클러스터가 계속 돈다. QEMU가 필요 없으므로 작은 기계나 VM(이 클러스터 밖)에 둔다.

```bash
sudo tee /etc/opl/opld.toml >/dev/null <<'TOML'
name = "wit"
address = "10.0.0.13"
witness = true
TOML
sudo opld --config /etc/opl/opld.toml &
oplctl cluster join-token                        # 일반 노드의 API에 로그인한 곳에서
sudo oplctl cluster join 'opl-join://…'          # 위트니스에서
oplctl cluster status                            # WITNESS 열이 true, 경고 없음
```

- **합류만 된다.** 위트니스에서 `cluster init`은 거절된다("a witness cannot start a cluster; join an existing
  cluster instead"). 역할은 합류할 때 정해지고 바뀌지 않는다. 바꾸려면 `cluster leave` 뒤 설정을 고치고 opld를
  다시 띄워 다시 합류한다. 멤버인 채 `witness` 값만 바꿔 다시 띄우면 opld는 시작하지 않는다("this node joined
  as a witness but opld.toml says witness = false; leave the cluster and join again to change the role", 반대도
  같다).
- **탈퇴.** 탈퇴·`remove-node`는 일반 노드와 같다. 다만 빠진 뒤 남는 투표 멤버가 모두 위트니스면 위트니스는
  리더가 될 수 없어 클러스터가 멈추므로 `cluster leave`를 거절한다("the remaining voting members are all
  witnesses, which cannot lead; add another non-witness node first"). 위트니스 자신은 일반 노드가 남으면 빠질
  수 있다.
- **도는 것과 돌지 않는 것.** 클러스터 저장소와 노드 사이 연결(`cluster_listen`, 기본 8007)만 돈다. HTTPS API(8006)·
  웹 UI를 열지 않고, VM·Task·스토리지 풀·마이그레이션·예약 스냅샷·정리를 하지 않는다. 그래서 위트니스에서는
  `cluster join`·`leave`(제어 소켓)만 쓰고, 상태는 일반 노드의 API로 본다. 방화벽은 노드 사이에 8007만 연다.
- **설정.** `[hypervisor]`·`[pools]`·`[migration]`은 쓰지 않는다. 다만 `[hypervisor]` 값의 검사는 그대로 거치므로
  (예: macOS에서 `accel = "kvm"`, `systemd_scope = true`) 잘못된 값이 있으면 시작하지 않는다. `qemu_binary`는
  찾지 않는다.
- **리더가 되지 않는다.** 투표는 하지만 선거에 나서지 않는다. 리더의 일(노드 감시, 정리, 예약 스냅샷)은 늘 일반
  노드가 한다. 리더가 죽으면 남은 일반 노드가 위트니스의 표를 받아 리더가 된다.
- **배치에서 빠진다.** 스케줄러, 작업 노드 선택, 마이그레이션 대상, 풀의 노드 목록이 위트니스를 세지 않는다. VM의
  노드나 마이그레이션 대상으로 위트니스를 지정하면 **409** `node_unavailable`이다.
- **표시.** `oplctl node list`·`cluster status`에 `WITNESS` 열(`true`/`false`), `node get`에 `spec.witness`가
  있다. 웹은 노드 목록·클러스터 멤버·대시보드에 "위트니스" 배지를 붙인다. `cluster status`는 투표 멤버가 둘이면
  2노드 경고("warning: a two-node cluster stops accepting changes when either node fails; add a third node")를,
  투표 멤버가 셋 이상인데 위트니스가 아닌 노드가 하나뿐이면 "warning: HA needs at least two non-witness
  nodes"를 보인다.
  위트니스의 `ADDRESS`(`node list`·`cluster status`)는 설정의 API 주소(`address:listen 포트`)이지만 위트니스는
  그 포트를 열지 않는다.

## 웹 UI
````

`README.md`에서

```markdown
- [라이브 마이그레이션 설계 (하위 프로젝트 4: 계획 20·21)](docs/superpowers/specs/2026-10-30-live-migration-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

을 다음으로 바꾼다:

```markdown
- [라이브 마이그레이션 설계 (하위 프로젝트 4: 계획 20·21)](docs/superpowers/specs/2026-10-30-live-migration-design.md)
- [HA·펜싱·위트니스 설계 (하위 프로젝트 5: 계획 22·23·24)](docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown
구현: 하위 프로젝트 4는 계획 20(공유 풀 위의 라이브 마이그레이션, QEMU TLS 마이그레이션·노드 사이 RPC·`VmMigrate` Task)과 계획 21(UEFI 변수가 멈춘 채 옮긴 VM을 따라감, 디렉터리 풀의 `shared` 바꾸기)로 나눴다(`2026-10-30-live-migration-design.md`). 디스크까지 복사하는 마이그레이션과 macOS(HVF) 노드는 넣지 않았다.
```

을 다음으로 바꾼다:

```markdown
구현: 하위 프로젝트 4는 계획 20(공유 풀 위의 라이브 마이그레이션, QEMU TLS 마이그레이션·노드 사이 RPC·`VmMigrate` Task)과 계획 21(UEFI 변수가 멈춘 채 옮긴 VM을 따라감, 디렉터리 풀의 `shared` 바꾸기)로 나눴다(`2026-10-30-live-migration-design.md`). 디스크까지 복사하는 마이그레이션과 macOS(HVF) 노드는 넣지 않았다.

구현: 하위 프로젝트 5는 계획 22(위트니스 노드: VM을 돌리지 않는 Raft 투표 멤버, 완료), 계획 23(HA 리스와 watchdog 자기 펜싱), 계획 24(HA VM 복구)로 나눴다(`2026-11-02-ha-fencing-witness-design.md`). 23·24는 아직이다.
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown
- `oplctl cluster leave`(정상 탈퇴)는 이 노드에 배정된 VM이 있거나 이 노드가 유일한 투표 멤버면 거부한다. Node 리소스를 지우고 멤버십에서 빠진 뒤, 클러스터 저장소와 노드 인증서를 지우고 미초기화 상태로 돌아간다. `oplctl cluster remove-node --force`(죽은 노드 제거)는 오프라인 노드만 뺀다. 멤버십 변경은 openraft의 joint consensus로 처리한다.
- 2노드 클러스터는 만들 수 있지만 `oplctl cluster status`가 경고를 표시한다. 위트니스는 하위 프로젝트 5에서 다룬다.
```

을 다음으로 바꾼다:

```markdown
- `oplctl cluster leave`(정상 탈퇴)는 이 노드에 배정된 VM이 있거나 이 노드가 유일한 투표 멤버면 거부한다. Node 리소스를 지우고 멤버십에서 빠진 뒤, 클러스터 저장소와 노드 인증서를 지우고 미초기화 상태로 돌아간다. `oplctl cluster remove-node --force`(죽은 노드 제거)는 오프라인 노드만 뺀다. 멤버십 변경은 openraft의 joint consensus로 처리한다.
- 2노드 클러스터는 만들 수 있지만 `oplctl cluster status`가 경고를 표시한다. 위트니스는 하위 프로젝트 5에서 다룬다. 구현: 계획 22로 위트니스 노드(설정 `witness = true`, VM을 돌리지 않고 선거에 나서지 않는 투표 멤버)를 더했다. 일반 노드 둘과 위트니스면 투표 멤버가 셋이라 경고가 없고, 한 노드가 죽어도 과반수가 남는다(`2026-11-02-ha-fencing-witness-design.md` §2).
```

`docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md`에서

```markdown
- 탈퇴·제거는 일반 노드와 같다(배정된 VM이 없다).
```

을 다음으로 바꾼다:

```markdown
- 탈퇴·제거는 일반 노드와 같다(배정된 VM이 없다).

구현: 설정은 opld `Config.witness`다. 쓰지 않는 `[hypervisor]`도 읽고 검사는 그대로 거친다(`accel`·`systemd_scope`의 운영체제 규칙 — 잘못된 값이면 시작하지 않는다. `qemu_binary`는 찾지 않으므로 없는 경로여도 된다). `cluster init`은 제어 소켓에서 409 "a witness cannot start a cluster; join an existing cluster instead"(`member::WITNESS_CANNOT_INIT`)이고, init/join 전 포트 검사는 `cluster_listen`만 본다. 멤버로 돌 때 `Member`의 `services`(HTTPS API, reconciler, Task 실행기, 노드 감시, 예약 스냅샷, 정리, 풀 마운터)가 없고 `Host::from_config`·`QemuHypervisor::new`를 부르지 않는다. 노드 간 서버는 `PeerService::new(cluster, None)`이라 마이그레이션·UEFI 변수를 받지 않고, 브리지 새로 고침과 마이그레이션 TLS 파일 쓰기도 없다. Raft 설정은 `raft_config(witness)`의 `enable_elect: !witness`다. Node 등록은 `bootstrap::register_witness`(`online`, `arch`만)이고, 다시 시작했을 때는 일반 노드처럼 뒤에서 될 때까지 다시 해 본다. `spec.address`는 일반 노드처럼 `address:listen 포트`이지만 그 포트는 열리지 않는다(`node list`·`cluster status`의 `ADDRESS`도 이 값이다). 다시 시작할 때 로컬 저장소의 이 노드 Node `spec.witness`가 설정의 `witness`와 다르면 아무것도 띄우지 않고 실패한다(`bootstrap::check_role`). 탈퇴는 빠진 뒤 남는 투표 멤버가 없거나 모두 위트니스면 거절한다(`member::check_leave`, "the remaining voting members are all witnesses, which cannot lead; add another non-witness node first") — 위트니스만 남으면 리더가 생기지 않는다. 역할은 합류 요청의 `witness`로만 정해지고 `PUT /nodes/{key}`는 저장된 값을 그대로 둔다(요청 값 무시). 배치에서 빼기는 `node_fits`("node {name:?} is a witness node")와 `opl_types::storage::pool_usable_on`(위트니스면 false, 이유 "witness nodes do not use storage pools")에 모았다 — 스케줄러, Task 노드 선택, 복제, 공유 풀 확인이 함께 막힌다. VM 생성·수정의 `spec.node`와 마이그레이션 대상은 `vms.rs`의 `check_not_witness`가 409 `node_unavailable`로 거절한다. 위트니스를 이름으로 적은 풀 `spec.nodes`는 받아들인다(쓰이지 않을 뿐 해가 없다). 웹·oplctl의 풀 노드 목록, 웹 VM 폼의 노드 목록·기본 머신 종류, 마이그레이션 대상 목록도 위트니스를 뺀다. 표시: `GET /cluster`의 members에 `witness`, oplctl `node list`·`cluster status`에 `WITNESS` 열(`true`/`false`), `node get`은 JSON의 `spec.witness`다. HA 경고는 투표 멤버가 3 이상이고 위트니스가 아닌 투표 멤버가 2 미만일 때다. 웹은 노드 목록, 클러스터 멤버, 대시보드 노드 표에 "위트니스" 배지를 붙이고, 대시보드의 노드 수는 위트니스도 센다.
```

`docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md`에서

```markdown
  - CI `linux-kvm`에 `opld/ha-it`를 더한다.
- watchdog 장치를 실제로 쓰는 시험은 CI에 두지 않는다. 터지면 러너가 재시작되고, CI의 `softdog`는 sanlock `wdmd`가 쥐고 있다. 대신 수동 시험 절차를 문서에 남긴다(`softdog soft_noboot=1`).
```

을 다음으로 바꾼다:

```markdown
  - CI `linux-kvm`에 `opld/ha-it`를 더한다.

구현(계획 22): memnet 시험은 `opl-cluster/tests/witness.rs`(헬퍼 `TestCluster::start_with_witnesses(n, &[ids])`)로, 위트니스가 리더가 되지 않는 것과 일반 노드 둘·위트니스에서 리더를 멈추면 남은 일반 노드가 리더가 되는 것을 본다. 실제 시험은 `crates/opld/tests/cluster.rs`의 `a_witness_votes_but_never_leads_or_runs_vms`다. 위트니스(`Daemon::witness`, 없는 `qemu_binary`)는 API가 없으므로 하네스는 합류·시작 뒤 HTTPS API 대신 제어 소켓의 `state == "member"`를 기다린다. init 거부, API 포트가 닫혔는지, 투표 멤버 셋·경고 없음, `vm create --node wit`의 `node_unavailable`, 리더 `kill -9` 뒤 남은 일반 노드가 리더가 되고 `pool create`가 되는지, 위트니스를 다시 시작하면 온라인 위트니스 멤버로 돌아오는지, 죽은 노드를 `remove-node --force`로 뺀 뒤 일반 노드의 `cluster leave`는 거절되고 위트니스의 탈퇴는 되는지 본다. QEMU가 필요 없어 이 Mac에서도 돈다.

- watchdog 장치를 실제로 쓰는 시험은 CI에 두지 않는다. 터지면 러너가 재시작되고, CI의 `softdog`는 sanlock `wdmd`가 쥐고 있다. 대신 수동 시험 절차를 문서에 남긴다(`softdog soft_noboot=1`).
```

`docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md`에서

```markdown
- HA 아닌 VM은 죽은 노드에 묶여 있다(지금과 같다).
- 위트니스는 한 클러스터에만 속하고, 위트니스 하나로는 클러스터를 만들 수 없다.
```

을 다음으로 바꾼다:

```markdown
- HA 아닌 VM은 죽은 노드에 묶여 있다(지금과 같다).
- 위트니스는 한 클러스터에만 속하고, 위트니스 하나로는 클러스터를 만들 수 없다. 구현: 위트니스는 HTTPS API를 열지 않으므로 그 노드에서는 제어 소켓 명령(`cluster join`·`leave`)만 되고, 상태는 일반 노드의 API로 본다. 역할은 합류할 때 정해지므로 바꾸려면 탈퇴한 뒤 설정을 고치고 opld를 다시 띄워 다시 합류한다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 762개, 실패 0

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 313개

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: 위트니스 노드 — README 설치·역할, 설계 구현 메모"
```

---

## 완료 기준 (계획 22)

- 위트니스 opld가 QEMU 없이 합류해 투표 멤버가 되고, 리더가 되지 않으며, VM·볼륨·Task가 가지 않는다(VM 노드로 지정하면 409 `node_unavailable`).
- 일반 노드 둘과 위트니스로 된 클러스터에서 리더를 잃으면 남은 일반 노드가 리더가 되고 쓰기가 된다.
- `oplctl cluster status`·`node list`와 웹이 역할을 보이고, 일반 노드가 하나뿐인 클러스터에 경고한다.
- 위트니스의 init, 역할이 바뀐 설정으로 다시 시작, 리더가 될 수 없게 하는 탈퇴는 거부된다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **위트니스에는 HTTPS API가 없다.** 노드 목록의 주소(설정의 API 주소)로 접속할 수 없다.
- **위트니스 하나로는 클러스터를 만들 수 없고**, 노드 하나는 한 클러스터의 위트니스만 된다.
- **위트니스의 설정도 `[hypervisor]` 값의 검사는 거친다**(QEMU를 찾지는 않는다).
- 그 밖의 제한은 설계 문서 §7에 있다.
