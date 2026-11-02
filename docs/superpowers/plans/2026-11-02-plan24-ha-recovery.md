# opLite 계획 24: HA 복구 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 노드가 죽거나 과반수와 끊겨 리스가 끝나면, 리더가 그 노드의 HA VM을 다른 노드에서 켠다. 이로써 하위 프로젝트 5(HA·펜싱·위트니스)를 마친다.

**Architecture:** 리더의 `HaManager`가 1초마다 리스 표(계획 23)를 보고, 노드가 알린 `recover_after_secs`가 지나도록 갱신하지 않은 노드를 펜싱 중으로 표시한 뒤(`begin_fence` — 그 노드의 갱신을 거부) 그 노드의 HA VM을 revision 검사 쓰기로 다른 노드에 배정한다. 대상 노드의 reconciler는 평소처럼 켜고, 죽은 노드가 쥐었던 디스크 잠금이 풀릴 때까지 실패로 세지 않고 기다린다. 노드가 끊긴 사이 HA가 켜지는 일이 없도록 HA는 멈춘 VM에서만 켤 수 있다.

**Tech Stack:** Rust 2024(MSRV 1.88), openraft 0.9, axum 0.8, utoipa, clap; 웹은 계획 8~23과 같다. 새 외부 의존성은 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md` (§4와 §3.1·§5~§7의 계획 24 부분, "구현(계획 24)" 메모 포함)
**선행 계획:** 계획 1~23 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| 리더의 `HaManager`(펜싱할 노드 고르기, 대상 고르기, status → spec 쓰기, Task 실패, 기록 Task `vm-ha-recover`) | HA 그룹·우선순위·반친화성, 복구 뒤 원래 노드로 되돌리기 |
| 결과를 모르는 쓰기 뒤 펜싱 유지, 리더가 바뀔 때의 남은 단계 | HA 아닌 VM의 자동 복구, 노드 유지보수 때 모두 옮기기 |
| `ImageLocked`(디스크 잠금 기다리기), HA는 멈춘 VM에서만 켜기 | 죽은 노드에만 있던 UEFI 변수 살리기, RBD 배타 잠금·blocklist |
| oplctl·웹 표시, 실제 시험 `ha-it`(opld 셋, 실제 QEMU), README | 외부 펜싱 장치 |

## 이전 계획에서 이어받는 사항

- 계획 23: 리스 표(`Cluster::leases()`, `begin_fence`/`end_fence`, `Rejection::Fenced`), 노드의 `NodeHa.recover_after_secs`, `HaGuard`와 워커의 켜기 문턱·자기 펜싱, `ha_capable`.
- 계획 22: 위트니스(리더가 되지 않으므로 `HaManager`는 늘 일반 노드에서 돈다), opld 실제 시험 하네스의 위트니스.
- 계획 20·21: `two_nodes`·`migration.rs` 하네스, 공유 디렉터리 풀로 한 호스트에서 opld 둘을 돌리는 방식, `status.migration`.

## 실행 환경

계획 23과 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상), Node.js 22.12 이상, pnpm 10.33.2. 실제 시험(`opld`의 `ha-it`)은 이 Mac에서 `[ha] fencing = "process"`로 돈다(진짜 watchdog 장치를 열지 않는다). CI `linux-kvm`에 `opld/ha-it`를 더한다.

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `web/src/api/schema.d.ts`(`pnpm -C web gen:api`).

## 설계 결정

- **복구는 리더의 메모리 리스 표로 판단한다.** 새 리더는 리더가 된 때부터 센다. 노드는 리스를 잃은 지 `fence_after` 안에 HA VM을 끄고, watchdog가 그 뒤 제한 시간 안에 노드를 재시작하므로 `recover_after`가 지나 옮긴 VM은 겹치지 않는다.
- **펜싱 중 표시와 결과를 모르는 쓰기:** 쓰기의 결과를 모르면(시간 초과) 그 항목이 나중에 커밋될 수 있으므로, 같은 임기에서는 같은 쓰기를 다시 보내 결과를 알 때까지 그 노드의 갱신을 거부한다.
- **status를 먼저, `spec.node`를 나중에** 쓴다. 사이에 리더가 바뀌어도 VM은 아직 죽은 노드에 배정돼 있어 새 리더가 다시 한다.
- **HA는 멈춘 VM에서만 켠다.** 끊긴 노드가 모르는 사이에 실행 중인 VM이 HA가 되면 그 노드는 펜싱하지 않는다.
- **디스크 잠금은 기다린다.** 죽은 노드의 NFS 잠금·sanlock 임대가 풀릴 때까지 실패로 세지 않는다(HA 아닌 VM에도).

## 파일 구조

```
crates/opl-api/src/{ha.rs, vms.rs, tasks.rs, lib.rs}, tests/{ha.rs, vms.rs, migrate.rs}   # HaManager, 규칙
crates/opl-types/src/task.rs                                         # TaskOp::VmHaRecover
crates/opl-driver/src/{error.rs, qemu.rs, mock.rs}, tests/qemu_driver.rs   # ImageLocked
crates/opl-reconciler/src/worker.rs, tests/reconcile.rs              # 잠금 기다리기, status 쓰기
crates/opld/src/member.rs, crates/opld/{Cargo.toml, tests/ha.rs}, .github/workflows/ci.yml   # 연결, 실제 시험
crates/oplctl/src/cli.rs, web/src/{vms, tasks}/..., web/src/i18n/{ko,en}.json
README.md, docs/superpowers/specs/{2026-11-02-ha-fencing-witness-design.md, 2026-10-20-opl-node-core-design.md}
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: HA는 멈춘 VM에서만 켠다

VM 수정에서 `ha`를 false → true로 바꾸는 것은 VM이 멈춰 있을 때만(`is_stopped`) — 아니면 409 `vm_running`("cannot turn on HA for VM … while it is running; turn on HA while the VM is stopped"). 끄기와 생성 때의 `ha: true`는 그대로 된다. 노드가 과반수와 끊긴 동안 다수 쪽이 실행 중인 VM의 HA를 켜면 그 노드는 HA인 줄 모르고 펜싱하지 않는데 리더는 복구해 둘이 함께 돌기 때문이다. oplctl `vm set --ha` 도움말, 웹 HA 스위치는 실행 중이고 HA가 꺼진 VM이면 이유(`vmForm.haRunning`)와 함께 막는다(`haLocked`).

**Files:**
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/tests/migrate.rs`
- Modify: `crates/opl-api/tests/vms.rs`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/vms/VmFormPage.test.tsx`
- Modify: `web/src/vms/VmFormPage.tsx`
- Modify: `web/src/vms/form/GeneralSection.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/vms.rs`에서

```rust

/// spec 전체를 바꾼다. 실행 중인 VM의 `node`와 `disks`는 바꿀 수 없다. 다른 변경은 다음 시작부터
/// 적용된다. `If-Match`가 없으면 검증한 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 검증한다.
#[utoipa::path(put, path = "/vms/{key}", tag = "vms",
```

을 다음으로 바꾼다:

```rust

/// spec 전체를 바꾼다. 실행 중인 VM의 `node`와 `disks`는 바꿀 수 없고 `ha`를 켤 수 없다. 다른
/// 변경은 다음 시작부터 적용된다. `If-Match`가 없으면 검증한 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 검증한다.
#[utoipa::path(put, path = "/vms/{key}", tag = "vms",
```

`crates/opl-api/src/vms.rs`에서

```rust
            return Err(vm_running(&current, "change the disks of"));
        }
```

을 다음으로 바꾼다:

```rust
            return Err(vm_running(&current, "change the disks of"));
        }
        // 노드가 과반수와 끊긴 동안 켜면 그 노드는 HA인 줄 모르고 펜싱하지 않는다. 끄기는 언제든.
        if spec.ha && !current.spec.ha && !is_stopped(&current) {
            return Err(ApiError::conflict(
                "vm_running",
                format!(
                    "cannot turn on HA for VM {:?} while it is running; turn on HA while the VM is stopped",
                    current.name
                ),
            ));
        }
```

`crates/opl-api/tests/migrate.rs`에서

```rust
    edit_node(&env, env.node, |n| n.status.ha = ha).await;
    // 실행 중인 VM도 HA로 바꿀 수 있다.
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["ha"] = json!(true);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
```

을 다음으로 바꾼다:

```rust
    edit_node(&env, env.node, |n| n.status.ha = ha).await;
    // API는 실행 중인 VM의 HA를 켜지 않으므로 저장소에 바로 쓴다.
    let mut vm = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    vm.spec.ha = true;
    env.cluster
        .update_spec(vm.id, None, vm.labels, &vm.spec)
        .await
        .unwrap();
```

`crates/opl-api/tests/vms.rs`에서

```rust

/// HA VM은 HA 가능 노드(펜싱이 켜진 노드)에서만 돈다. 실행 중에도 `ha`를 바꿀 수 있다.
#[tokio::test]
```

을 다음으로 바꾼다:

```rust

/// HA VM은 HA 가능 노드(펜싱이 켜진 노드)에서만 돈다. HA는 멈춘 VM에서만 켤 수 있다(끄기는 언제든).
#[tokio::test]
```

`crates/opl-api/tests/vms.rs`에서

```rust

    // 실행 중인 VM의 `ha`는 바꿀 수 있다.
    for ha in [false, true] {
        let mut spec = vm["spec"].clone();
        spec["ha"] = json!(ha);
        let (code, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        assert_eq!(body["spec"]["ha"], ha);
    }
    // 노드의 펜싱이 꺼진 뒤에는 HA로 바꿀 수 없다.
```

을 다음으로 바꾼다:

```rust

    // 실행 중인 VM의 HA는 끌 수 있지만 켤 수는 없다. 멈춘 뒤에는 켤 수 있다.
    let mut spec = vm["spec"].clone();
    spec["ha"] = json!(false);
    let (code, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["ha"], false);
    let mut spec = body["spec"].clone();
    spec["ha"] = json!(true);
    let (code, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running");
    assert_eq!(
        body["message"],
        "cannot turn on HA for VM \"web\" while it is running; turn on HA while the VM is stopped"
    );
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["ha"] = json!(true);
    let (code, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["ha"], true);
    // 노드의 펜싱이 꺼진 뒤에는 HA로 바꿀 수 없다.
```

`crates/oplctl/src/cli.rs`에서

```rust
    vm: String,
    /// HA VM으로 만든다. VM의 노드가 펜싱이 켜진 노드여야 한다.
    #[arg(long)]
```

을 다음으로 바꾼다:

```rust
    vm: String,
    /// HA VM으로 만든다. VM이 멈춰 있어야 하고 VM의 노드가 펜싱이 켜진 노드여야 한다.
    #[arg(long)]
```

`crates/oplctl/src/cli.rs`에서

```rust
        assert_eq!(r.next("VM web not found (not_found)"), None);
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(r.next("VM web not found (not_found)"), None);
        // 실행 중인 VM의 HA 켜기는 서버의 409 문구를 그대로 보인다.
        assert_eq!(
            r.next("cannot turn on HA for VM \"web\" while it is running; turn on HA while the VM is stopped (vm_running)"),
            None
        );
    }
```

`web/src/i18n/en.json`에서

```json
    "haNodeOff": "{{node}} has fencing off and cannot run HA VMs.",
    "haNoNode": "No node has fencing on, so HA VMs cannot run."
  },
```

을 다음으로 바꾼다:

```json
    "haNodeOff": "{{node}} has fencing off and cannot run HA VMs.",
    "haNoNode": "No node has fencing on, so HA VMs cannot run.",
    "haRunning": "Turn on HA while the VM is stopped."
  },
```

`web/src/i18n/ko.json`에서

```json
    "haNodeOff": "{{node}}의 펜싱이 꺼져 있어 HA VM을 돌릴 수 없습니다.",
    "haNoNode": "펜싱이 켜진 노드가 없어 HA VM을 돌릴 수 없습니다."
  },
```

을 다음으로 바꾼다:

```json
    "haNodeOff": "{{node}}의 펜싱이 꺼져 있어 HA VM을 돌릴 수 없습니다.",
    "haNoNode": "펜싱이 켜진 노드가 없어 HA VM을 돌릴 수 없습니다.",
    "haRunning": "HA는 VM이 멈춰 있을 때 켤 수 있습니다."
  },
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
  const ha = await screen.findByRole("switch", { name: "High availability (HA)" });
  // 실행 중이어도 HA는 바꿀 수 있고, 켜진 HA는 끌 수 있다.
  expect(ha).toBeChecked();
```

을 다음으로 바꾼다:

```tsx
  const ha = await screen.findByRole("switch", { name: "High availability (HA)" });
  // 실행 중이어도 켜진 HA는 끌 수 있다.
  expect(ha).toBeChecked();
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx

it("says when no node has fencing on", async () => {
```

을 다음으로 바꾼다:

```tsx

it("turns HA on only while the VM is stopped and says why not", async () => {
  mockLists({ vm: [vm("app", "running")], node: [FENCED] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/app/edit" });
  const ha = await screen.findByRole("switch", { name: "High availability (HA)" });
  expect(ha).not.toBeChecked();
  expect(ha).toBeDisabled();
  expect(screen.getByText("Turn on HA while the VM is stopped.")).toBeInTheDocument();
});

it("turns HA on for a stopped VM", async () => {
  mockLists({ vm: [vm("app", "stopped")], node: [FENCED] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/app/edit" });
  const ha = await screen.findByRole("switch", { name: "High availability (HA)" });
  expect(ha).toBeEnabled();
  expect(screen.queryByText(/while the VM is stopped/)).not.toBeInTheDocument();
});

it("says when no node has fencing on", async () => {
```

`web/src/vms/VmFormPage.tsx`에서

```tsx
          locked={locked}
          autoMachine={creating && !machineChosen}
```

을 다음으로 바꾼다:

```tsx
          locked={locked}
          haLocked={locked && !vm?.spec.ha}
          autoMachine={creating && !machineChosen}
```

`web/src/vms/form/GeneralSection.tsx`에서

```tsx
  locked,
  autoMachine,
```

을 다음으로 바꾼다:

```tsx
  locked,
  haLocked,
  autoMachine,
```

`web/src/vms/form/GeneralSection.tsx`에서

```tsx
  locked: boolean;
  /** 노드를 바꾸면 그 노드에 맞게 머신 기본값을 다시 정한다. */
```

을 다음으로 바꾼다:

```tsx
  locked: boolean;
  /** 실행 중이고 HA가 꺼진 VM은 HA를 켜지 못한다(서버의 409). */
  haLocked: boolean;
  /** 노드를 바꾸면 그 노드에 맞게 머신 기본값을 다시 정한다. */
```

`web/src/vms/form/GeneralSection.tsx`에서

```tsx
  const { t } = useTranslation();
  const noHa = haUnavailable(nodes, values.node);
  return (
```

을 다음으로 바꾼다:

```tsx
  const { t } = useTranslation();
  const noHa = haLocked ? { key: "vmForm.haRunning" } : haUnavailable(nodes, values.node);
  return (
```

`web/src/vms/form/GeneralSection.tsx`에서

```tsx
          <div className="flex items-center gap-2">
            {/* 켤 수 없는 노드여도 이미 켜진 HA는 끌 수 있다(실행 중이어도 바꿀 수 있다). */}
            <Switch
```

을 다음으로 바꾼다:

```tsx
          <div className="flex items-center gap-2">
            {/* 켤 수 없어도 이미 켜진 HA는 끌 수 있다(실행 중이어도). */}
            <Switch
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 818개, 실패 0

Run: `cargo test -p opl-api --test vms --test migrate`
Expected: PASS

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 323개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat: HA는 멈춘 VM에서만 켠다 — 실행 중 켜기 409, 웹 HA 스위치가 이유와 함께 막음"
```

---

### Task 2: 이미지 잠금 기다리기

QEMU 시작 stderr에 `"write" lock`이나 `Is another process using the image`가 있으면 `DriverError::ImageLocked`(일시 오류). 워커는 이것을 실패로 세지 않고 `retry_interval`마다 다시 켠다(status `Starting`, "disk image is locked by another process; retrying" — 스냅샷 복원 요청이 있으면 기존대로 멈춘다). 죽은 노드가 쥐고 있던 NFS 잠금이 풀릴 때까지 HA 복구가 기다리게 한다. 워커의 status 쓰기 재시도는 다시 읽은 VM이 다른 노드로 갔으면 멈춘다(다시 시작한 노드가 옮겨 간 VM의 status를 덮어쓰지 않게). MockHypervisor `fail_next_starts`, 시작 문(`hold_next_start`·`start_held`·`release_start`). 시험: driver(가짜 qemu, `qemu-it`의 두 QEMU 실제 잠금), reconciler.

**Files:**
- Modify: `crates/opl-driver/src/error.rs`
- Modify: `crates/opl-driver/src/mock.rs`
- Modify: `crates/opl-driver/src/qemu.rs`
- Modify: `crates/opl-driver/tests/qemu_driver.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/ha.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/src/error.rs`에서

```rust
    StartFailed(String),
    #[error("volume {0} not found")]
```

을 다음으로 바꾼다:

```rust
    StartFailed(String),
    /// 다른 프로세스가 디스크 이미지의 잠금을 쥐고 있어 QEMU가 시작하지 못했다(죽은 노드의 NFS
    /// 임대가 아직 남음 등). 내용은 QEMU의 stderr다. 잠금이 풀리면 시작할 수 있다.
    #[error("disk image is locked by another process: {0}")]
    ImageLocked(String),
    #[error("volume {0} not found")]
```

`crates/opl-driver/src/error.rs`에서

```rust
            | DriverError::PoolNotResponding(_)
            | DriverError::VolumeNotActive(_) => true,
            DriverError::NotRunning(_)
```

을 다음으로 바꾼다:

```rust
            | DriverError::PoolNotResponding(_)
            | DriverError::VolumeNotActive(_)
            | DriverError::ImageLocked(_) => true,
            DriverError::NotRunning(_)
```

`crates/opl-driver/src/error.rs`에서

```rust
        assert!(DriverError::VolumeNotActive("/dev/vg0/opl-x".into()).is_transient());
        assert!(DriverError::Qmp(QmpError::Disconnected).is_transient());
```

을 다음으로 바꾼다:

```rust
        assert!(DriverError::VolumeNotActive("/dev/vg0/opl-x".into()).is_transient());
        // 잠금을 쥔 프로세스가 끝나거나 NFS 임대가 끝나면 풀린다.
        assert!(DriverError::ImageLocked("Failed to get \"write\" lock".into()).is_transient());
        assert!(DriverError::Qmp(QmpError::Disconnected).is_transient());
```

`crates/opl-driver/src/mock.rs` 전체:

```rust
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use opl_qemu::QmpError;
use opl_types::runtime::VmRuntimeSpec;
use tokio::sync::{Notify, broadcast};
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};
use crate::migration::{MigrateOptions, MigrateTarget};

/// 따로 정하지 않았을 때 `qemu_version`이 돌려주는 버전
pub const MOCK_QEMU_VERSION: &str = "8.2.2";

/// 메모리 안에서만 동작하는 하이퍼바이저. reconciler 테스트에서 장애를 주입하는 데 쓴다.
pub struct MockHypervisor {
    state: Mutex<MockState>,
    events: broadcast::Sender<HypervisorEvent>,
    /// 멈춘 `snapshot_save`가 들어왔다
    save_reached: Notify,
    /// 멈춘 `start`가 들어왔다
    start_reached: Notify,
    /// 멈춘 `start`를 놓아준다
    start_released: Notify,
}

#[derive(Default)]
struct MockState {
    vms: HashMap<Uuid, u32>,
    next_pid: u32,
    fail_next_start: Option<DriverError>,
    /// 이후의 `start`를 차례로 실패시킬 오류들(`fail_next_start` 다음에 쓴다)
    fail_starts: VecDeque<DriverError>,
    /// `start`가 이만큼 기다린 뒤에 동작한다.
    start_delay: Option<Duration>,
    /// 다음 `start` 한 번을 `release_start`까지 멈춘다.
    hold_next_start: bool,
    /// 다음 `snapshot_save`·`snapshot_delete` 한 번을 실패시킬 오류
    fail_next_snapshot: Option<DriverError>,
    /// 다음 `snapshot_save` 한 번을 VM이 꺼질 때까지 멈춘다.
    hang_next_save: bool,
    /// 살아 있지만 QMP로 다룰 수 없는 VM
    unresponsive: HashSet<Uuid>,
    console_dir: Option<PathBuf>,
    /// 새로 띄우려 한 VM과 불러오려 한 스냅샷 태그 (실패한 시도 포함)
    launches: Vec<(Uuid, Option<String>)>,
    /// VM별 내부 스냅샷 (태그, 메모리 포함)
    snapshots: HashMap<Uuid, Vec<(String, bool)>>,
    /// `block_resize`로 바꾼 (VM, 디스크 번호)별 크기
    disk_sizes: HashMap<(Uuid, usize), u64>,
    /// `pause`로 멈추고 아직 `resume`하지 않은 VM
    paused: HashSet<Uuid>,
    /// 멈춘 채 남은 표시(`paused-by-opl`)가 있는 VM(이전 opld가 멈춘 채 죽었다)
    marked: HashSet<Uuid>,
    /// `pause`·`resume` 호출 (VM, 이름)
    pauses: Vec<(Uuid, &'static str)>,
    /// 조회에 드러낼 상태(QEMU가 스스로 멈춘 상태 등)
    statuses: HashMap<Uuid, RuntimeStatus>,
    /// `set_qemu_version`으로 정한 QEMU 버전
    qemu_version: Option<String>,
    /// 받는 중 표시가 있는 VM(`start_incoming`으로 띄우고 아직 `finish_incoming`하지 않음)과 그
    /// 마이그레이션 Task
    incoming: HashMap<Uuid, Uuid>,
    /// 마이그레이션이 만든 상태: 받는 쪽 `inmigrate`·받은 뒤 멈춤, 보낸 원본 `postmigrate`.
    /// VM이 꺼지면 사라진다.
    migration_status: HashMap<Uuid, RuntimeStatus>,
    /// 다음 `migrate` 한 번을 실패시킬 오류
    fail_next_migrate: Option<DriverError>,
    /// 다음 `incoming_wait` 한 번을 실패시킬 오류. 실제 QEMU처럼 받는 QEMU는 끝난다.
    fail_next_incoming: Option<DriverError>,
    /// `migrate` 호출 (VM, 보낸 곳). 실패한 호출 포함
    migrations: Vec<(Uuid, MigrateTarget)>,
    /// 보내는 중인 마이그레이션이 있는 VM(누구도 끝을 기다리지 않는다). 원본은 돈다.
    outgoing: HashSet<Uuid>,
    /// 다음 `migrate` 한 번을 이 오류로 실패시키되 마이그레이션은 보내는 중으로 남긴다.
    fail_next_migrate_unfinished: Option<DriverError>,
    /// `cancel_migration`이 취소하는 사이에 마이그레이션이 끝난다(원본은 `postmigrate`).
    complete_on_cancel: bool,
    /// `migrate`가 이만큼 기다린 뒤에 동작한다.
    migrate_delay: Option<Duration>,
}

impl Default for MockHypervisor {
    fn default() -> Self {
        Self {
            state: Mutex::new(MockState {
                next_pid: 1000,
                ..Default::default()
            }),
            events: broadcast::channel(64).0,
            save_reached: Notify::new(),
            start_reached: Notify::new(),
            start_released: Notify::new(),
        }
    }
}

impl MockHypervisor {
    /// 다음 `start` 한 번을 이 오류로 실패시킨다. 이미 실행 중인 VM에 대한 `start`에는 쓰이지 않는다.
    pub fn fail_next_start(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_start = Some(err);
    }

    /// 이후의 `start`를 차례로 이 오류들로 실패시킨다(한 번에 하나씩). `fail_next_start`처럼 이미
    /// 실행 중인 VM에 대한 `start`에는 쓰이지 않는다.
    pub fn fail_next_starts(&self, errs: impl IntoIterator<Item = DriverError>) {
        self.state.lock().unwrap().fail_starts.extend(errs);
    }

    /// 이후의 `start`가 `delay`만큼 기다린 뒤에 동작하게 한다. 시작 도중에 일어나는 일을 흉내 낸다.
    pub fn delay_starts(&self, delay: Duration) {
        self.state.lock().unwrap().start_delay = Some(delay);
    }

    /// 다음 `start` 한 번을 `release_start`까지 멈춘다. 시작 도중에 일어나는 일을 시간에 기대지 않고
    /// 흉내 낸다.
    pub fn hold_next_start(&self) {
        self.state.lock().unwrap().hold_next_start = true;
    }

    /// 멈춘 `start`가 들어올 때까지 기다린다.
    pub async fn start_held(&self) {
        self.start_reached.notified().await;
    }

    /// 멈춘 `start`를 놓아준다.
    pub fn release_start(&self) {
        self.start_released.notify_one();
    }

    /// 다음 `snapshot_save`나 `snapshot_delete` 한 번을 이 오류로 실패시킨다. `snapshot_save`는
    /// 태그를 남긴 채 실패한다(일부 디스크에 만들었거나, 시간 초과로 작업이 아직 돌고 있는 것처럼).
    pub fn fail_next_snapshot(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_snapshot = Some(err);
    }

    /// 다음 `snapshot_save` 한 번이 VM이 꺼질 때까지 끝나지 않게 한다. 실제 QEMU의 메모리 저장처럼
    /// 그동안 VM을 강제로 끄면 QMP가 끊긴 오류로 실패하고 태그를 남기지 않는다.
    pub fn hang_next_snapshot_save(&self) {
        self.state.lock().unwrap().hang_next_save = true;
    }

    /// 멈춘 `snapshot_save`가 들어올 때까지 기다린다.
    pub async fn snapshot_save_reached(&self) {
        self.save_reached.notified().await;
    }

    /// 게스트 OS 안에서 전원을 끈 것처럼 만든다. `stop(Acpi)` 요청에 게스트가 응한 경우도 이것으로 흉내 낸다.
    pub fn guest_shutdown(&self, id: Uuid) {
        self.remove(
            id,
            EventKind::Stopped {
                guest_initiated: true,
            },
        );
    }

    /// 프로세스가 비정상 종료한 것처럼 만든다.
    pub fn crash(&self, id: Uuid) {
        self.remove(id, EventKind::Crashed);
    }

    /// 실행 중인 VM을 살아 있지만 QMP로 다룰 수 없는 VM으로 만들거나(true) 되돌린다(false).
    /// 실제 드라이버처럼 조회에는 `unresponsive`로 드러나고, `start`와 `stop(Acpi)`는 다시 붙지
    /// 못해 실패하며, `stop(Force)`는 프로세스를 죽이므로 종료 이벤트 없이 사라진다.
    pub fn set_unresponsive(&self, id: Uuid, unresponsive: bool) {
        let mut st = self.state.lock().unwrap();
        if unresponsive {
            st.unresponsive.insert(id);
        } else {
            st.unresponsive.remove(&id);
        }
    }

    /// `console_endpoint`가 `{dir}/{id}.vnc`를 돌려주게 한다. 테스트가 그 경로에 가짜 VNC 서버를 띄운다.
    pub fn set_console_dir(&self, dir: impl Into<PathBuf>) {
        self.state.lock().unwrap().console_dir = Some(dir.into());
    }

    /// 새로 띄우려 한 VM과 `start`에 준 `restore` 태그를 순서대로 돌려준다. 이미 실행 중이어서
    /// 띄우지 않은 `start`는 빠지고, 주입한 오류로 실패한 시도는 들어간다.
    pub fn launches(&self) -> Vec<(Uuid, Option<String>)> {
        self.state.lock().unwrap().launches.clone()
    }

    /// 실행 중에 `snapshot_save`로 만들고 지우지 않은 스냅샷 (태그, 메모리 포함). VM이 꺼져도 남는다.
    pub fn snapshots(&self, id: Uuid) -> Vec<(String, bool)> {
        let st = self.state.lock().unwrap();
        st.snapshots.get(&id).cloned().unwrap_or_default()
    }

    /// `block_resize`로 마지막에 바꾼 크기
    pub fn disk_size(&self, id: Uuid, disk: usize) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .disk_sizes
            .get(&(id, disk))
            .copied()
    }

    /// `pause`로 멈추고 아직 `resume`하지 않았는지
    pub fn is_paused(&self, id: Uuid) -> bool {
        self.state.lock().unwrap().paused.contains(&id)
    }

    /// 이전 opld가 `pause`한 채 죽은 것처럼 VM을 표시와 함께 멈춘다. `resume_left_stopped`가
    /// 이어서 실행한다(지금 `pause`해 둔 VM은 표시가 없는 것으로 본다).
    pub fn leave_paused(&self, id: Uuid) {
        let mut st = self.state.lock().unwrap();
        st.paused.insert(id);
        st.marked.insert(id);
    }

    /// VM에 부른 `pause`·`resume`(실패한 호출 포함)을 순서대로 돌려준다.
    pub fn pause_calls(&self, id: Uuid) -> Vec<&'static str> {
        let st = self.state.lock().unwrap();
        st.pauses
            .iter()
            .filter(|(vm, _)| *vm == id)
            .map(|(_, call)| *call)
            .collect()
    }

    /// 조회(`inspect`·`list`)가 VM의 상태를 `status`로 알리게 한다(`None`이면 되돌린다). QEMU가 스스로
    /// 멈춘 상태(`io-error` 등)를 흉내 낸다. QMP로 다룰 수 없는 VM은 그대로 `unresponsive`다.
    pub fn set_status(&self, id: Uuid, status: Option<RuntimeStatus>) {
        let mut st = self.state.lock().unwrap();
        match status {
            Some(status) => st.statuses.insert(id, status),
            None => st.statuses.remove(&id),
        };
    }

    /// `qemu_version`·`running_qemu_version`이 돌려줄 버전(기본 [`MOCK_QEMU_VERSION`])
    pub fn set_qemu_version(&self, version: &str) {
        self.state.lock().unwrap().qemu_version = Some(version.to_string());
    }

    /// 다음 `migrate` 한 번을 이 오류로 실패시킨다. 원본은 그대로 돈다.
    pub fn fail_next_migrate(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_migrate = Some(err);
    }

    /// 다음 `migrate` 한 번을 이 오류로 실패시킨다. QMP 연결이 끊긴 것처럼 마이그레이션은 보내는
    /// 중으로 남는다([`complete_outgoing`](Self::complete_outgoing)이 끝낸다).
    pub fn fail_next_migrate_unfinished(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_migrate_unfinished = Some(err);
    }

    /// 이후 `migrate`가 이만큼 기다린 뒤에 동작한다(보내는 동안 다른 일이 끼어드는 시험).
    pub fn delay_migrations(&self, delay: Duration) {
        self.state.lock().unwrap().migrate_delay = Some(delay);
    }

    /// 도는 VM이 보내는 중인 마이그레이션을 남긴다(opld가 보내다 죽었다).
    pub fn start_outgoing(&self, id: Uuid) {
        self.state.lock().unwrap().outgoing.insert(id);
    }

    /// 보내는 중인 마이그레이션이 있는지
    pub fn is_outgoing(&self, id: Uuid) -> bool {
        self.state.lock().unwrap().outgoing.contains(&id)
    }

    /// 보내는 중인 마이그레이션이 있으면 끝낸다(원본은 `postmigrate`로 멈춘다).
    pub fn complete_outgoing(&self, id: Uuid) {
        let mut st = self.state.lock().unwrap();
        if st.outgoing.remove(&id) {
            st.migration_status.insert(id, postmigrate());
        }
    }

    /// 이후 `cancel_migration`은 취소하는 사이에 마이그레이션이 끝난 것으로 한다.
    pub fn complete_on_cancel(&self) {
        self.state.lock().unwrap().complete_on_cancel = true;
    }

    /// 다음 `incoming_wait` 한 번을 이 오류로 실패시킨다. 받는 QEMU는 크래시로 끝난다.
    pub fn fail_next_incoming_wait(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_incoming = Some(err);
    }

    /// `migrate` 호출(실패 포함)을 순서대로 돌려준다.
    pub fn migrations(&self) -> Vec<(Uuid, MigrateTarget)> {
        self.state.lock().unwrap().migrations.clone()
    }

    /// 받는 중 표시가 있는지
    pub fn is_incoming(&self, id: Uuid) -> bool {
        self.state.lock().unwrap().incoming.contains_key(&id)
    }

    fn version(st: &MockState) -> String {
        st.qemu_version
            .clone()
            .unwrap_or_else(|| MOCK_QEMU_VERSION.to_string())
    }

    /// 실제 드라이버처럼 실행 중인 VM만 다룬다. 살아 있지만 QMP로 다룰 수 없으면 다시 시도할 오류다.
    fn controllable(st: &MockState, id: Uuid) -> Result<(), DriverError> {
        if !st.vms.contains_key(&id) {
            Err(DriverError::NotRunning(id))
        } else if st.unresponsive.contains(&id) {
            Err(DriverError::NotAttached(id))
        } else {
            Ok(())
        }
    }

    fn remove(&self, id: Uuid, kind: EventKind) {
        let mut st = self.state.lock().unwrap();
        st.unresponsive.remove(&id);
        st.paused.remove(&id);
        st.marked.remove(&id);
        st.incoming.remove(&id);
        st.migration_status.remove(&id);
        st.outgoing.remove(&id);
        if st.vms.remove(&id).is_some() {
            drop(st);
            let _ = self.events.send(HypervisorEvent { id, kind });
        }
    }

    fn state_of(st: &MockState, id: Uuid, pid: u32) -> ActualState {
        let status = if st.unresponsive.contains(&id) {
            RuntimeStatus::Other("unresponsive".into())
        } else if let Some(status) = st.statuses.get(&id) {
            status.clone()
        } else if let Some(status) = st.migration_status.get(&id) {
            status.clone()
        } else if st.paused.contains(&id) {
            RuntimeStatus::Paused
        } else {
            RuntimeStatus::Running
        };
        ActualState { id, pid, status }
    }
}

#[async_trait]
impl Hypervisor for MockHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec, restore: Option<&str>) -> Result<(), DriverError> {
        let (delay, hold) = {
            let mut st = self.state.lock().unwrap();
            (st.start_delay, std::mem::take(&mut st.hold_next_start))
        };
        if hold {
            self.start_reached.notify_one();
            self.start_released.notified().await;
        }
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        {
            let mut st = self.state.lock().unwrap();
            // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 주입한 오류도 쓰지 않는다.
            if st.vms.contains_key(&vm.id) {
                if st.unresponsive.contains(&vm.id) {
                    return Err(DriverError::Timeout("reattach to running QEMU"));
                }
                return Ok(());
            }
            st.launches.push((vm.id, restore.map(str::to_string)));
            if let Some(err) = st
                .fail_next_start
                .take()
                .or_else(|| st.fail_starts.pop_front())
            {
                return Err(err);
            }
            let pid = st.next_pid;
            st.next_pid += 1;
            st.vms.insert(vm.id, pid);
        }
        let _ = self.events.send(HypervisorEvent {
            id: vm.id,
            kind: EventKind::Started,
        });
        Ok(())
    }

    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError> {
        {
            let mut st = self.state.lock().unwrap();
            if st.vms.contains_key(&id) && st.unresponsive.contains(&id) {
                // 실제 드라이버는 다시 붙지 못한 VM에 ACPI 요청을 보낼 수 없고, 강제 종료는
                // 프로세스를 죽이므로 종료 이벤트가 없다.
                return match mode {
                    StopMode::Acpi => Err(DriverError::Timeout("reattach to running QEMU")),
                    StopMode::Force => {
                        st.unresponsive.remove(&id);
                        st.paused.remove(&id);
                        st.incoming.remove(&id);
                        st.migration_status.remove(&id);
                        st.outgoing.remove(&id);
                        st.vms.remove(&id);
                        Ok(())
                    }
                };
            }
        }
        match mode {
            // 실제 QEMU처럼 요청만 보낸다. 게스트는 무시할 수 있으므로 실제 종료는
            // 테스트가 `guest_shutdown`으로 일으킨다.
            StopMode::Acpi => {}
            StopMode::Force => self.remove(
                id,
                EventKind::Stopped {
                    guest_initiated: false,
                },
            ),
        }
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st.vms.get(&id).map(|&pid| Self::state_of(&st, id, pid)))
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .vms
            .iter()
            .map(|(&id, &pid)| Self::state_of(&st, id, pid))
            .collect())
    }

    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError> {
        let st = self.state.lock().unwrap();
        if st.vms.contains_key(&id) {
            Ok(match &st.console_dir {
                Some(dir) => dir.join(format!("{id}.vnc")),
                None => PathBuf::from(format!("/mock/{id}/vnc.sock")),
            })
        } else {
            Err(DriverError::NotRunning(id))
        }
    }

    async fn snapshot_save(&self, id: Uuid, tag: &str, memory: bool) -> Result<(), DriverError> {
        let hang = {
            let mut st = self.state.lock().unwrap();
            Self::controllable(&st, id)?;
            std::mem::take(&mut st.hang_next_save)
        };
        if hang {
            self.save_reached.notify_one();
            while self.state.lock().unwrap().vms.contains_key(&id) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            return Err(DriverError::Qmp(QmpError::Disconnected));
        }
        let mut st = self.state.lock().unwrap();
        st.snapshots
            .entry(id)
            .or_default()
            .push((tag.to_string(), memory));
        match st.fail_next_snapshot.take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if let Some(err) = st.fail_next_snapshot.take() {
            return Err(err);
        }
        if let Some(snapshots) = st.snapshots.get_mut(&id) {
            snapshots.retain(|(t, _)| t != tag);
        }
        Ok(())
    }

    async fn block_resize(
        &self,
        id: Uuid,
        disk: usize,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        st.disk_sizes.insert((id, disk), size_bytes);
        Ok(())
    }

    async fn pause(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        st.pauses.push((id, "pause"));
        Self::controllable(&st, id)?;
        st.paused.insert(id);
        Ok(())
    }

    async fn resume(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        st.pauses.push((id, "resume"));
        Self::controllable(&st, id)?;
        st.paused.remove(&id);
        st.marked.remove(&id);
        // 마이그레이션을 보낸 원본도 `cont`로 다시 돈다.
        if st.migration_status.get(&id) == Some(&postmigrate()) {
            st.migration_status.remove(&id);
        }
        Ok(())
    }

    async fn resume_left_stopped(&self, id: Uuid) -> Result<bool, DriverError> {
        let mut st = self.state.lock().unwrap();
        if Self::controllable(&st, id).is_err() {
            return Ok(false);
        }
        if st.marked.remove(&id) {
            st.paused.remove(&id);
        }
        Ok(true)
    }

    async fn qemu_version(&self) -> Result<String, DriverError> {
        Ok(Self::version(&self.state.lock().unwrap()))
    }

    async fn running_qemu_version(&self, id: Uuid) -> Result<String, DriverError> {
        let st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        Ok(Self::version(&st))
    }

    /// 범위의 첫 포트를 돌려준다. `fail_next_start`로 주입한 오류로 실패할 수 있다.
    async fn start_incoming(
        &self,
        vm: &VmRuntimeSpec,
        task: Uuid,
        _listen_host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError> {
        {
            let mut st = self.state.lock().unwrap();
            if st.vms.contains_key(&vm.id) {
                return Err(DriverError::Migration(format!(
                    "VM {} is already running on this node",
                    vm.id
                )));
            }
            if let Some(err) = st.fail_next_start.take() {
                return Err(err);
            }
            let pid = st.next_pid;
            st.next_pid += 1;
            st.vms.insert(vm.id, pid);
            st.incoming.insert(vm.id, task);
            st.migration_status.insert(vm.id, inmigrate());
        }
        let _ = self.events.send(HypervisorEvent {
            id: vm.id,
            kind: EventKind::Started,
        });
        Ok(*ports.start())
    }

    /// 주입한 실패가 없으면 받기를 마친 것으로 본다(멈춘 채).
    async fn incoming_wait(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if !st.incoming.contains_key(&id) {
            return Err(DriverError::Migration(format!(
                "VM {id} is not receiving a migration"
            )));
        }
        if let Some(err) = st.fail_next_incoming.take() {
            drop(st);
            self.remove(id, EventKind::Crashed);
            return Err(err);
        }
        st.migration_status.insert(id, RuntimeStatus::Paused);
        Ok(())
    }

    async fn finish_incoming(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if !st.incoming.contains_key(&id) {
            return Ok(());
        }
        if st.migration_status.get(&id) == Some(&inmigrate()) {
            return Err(DriverError::Migration(format!(
                "incoming migration of VM {id} has not completed"
            )));
        }
        st.incoming.remove(&id);
        st.migration_status.remove(&id);
        Ok(())
    }

    async fn list_incoming(&self) -> Result<Vec<(Uuid, Uuid)>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .incoming
            .iter()
            .filter(|(id, _)| st.vms.contains_key(id))
            .map(|(&id, &task)| (id, task))
            .collect())
    }

    /// 주입한 실패가 없으면 원본을 `postmigrate`로 멈춘다.
    async fn migrate(
        &self,
        id: Uuid,
        target: &MigrateTarget,
        _opts: &MigrateOptions,
    ) -> Result<(), DriverError> {
        let delay = self.state.lock().unwrap().migrate_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let mut st = self.state.lock().unwrap();
        st.migrations.push((id, target.clone()));
        Self::controllable(&st, id)?;
        if let Some(err) = st.fail_next_migrate.take() {
            return Err(err);
        }
        if let Some(err) = st.fail_next_migrate_unfinished.take() {
            st.outgoing.insert(id);
            return Err(err);
        }
        st.migration_status.insert(id, postmigrate());
        Ok(())
    }

    async fn stop_migrated(&self, id: Uuid) -> Result<bool, DriverError> {
        let migrated = {
            let st = self.state.lock().unwrap();
            st.vms.contains_key(&id) && st.migration_status.get(&id) == Some(&postmigrate())
        };
        if migrated {
            self.remove(
                id,
                EventKind::Stopped {
                    guest_initiated: false,
                },
            );
        }
        Ok(migrated)
    }

    async fn cancel_migration(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        if st.outgoing.remove(&id) && st.complete_on_cancel {
            st.migration_status.insert(id, postmigrate());
        }
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

fn inmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("inmigrate".into())
}

fn postmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("postmigrate".into())
}

#[cfg(test)]
mod tests {
    use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType};

    use super::*;

    fn spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::now_v7(),
            name: "t".into(),
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: CpuModel::Host,
            },
            memory_mib: 256,
            firmware: Firmware::Uefi,
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
        }
    }

    #[tokio::test]
    async fn start_is_idempotent_and_emits_one_event() {
        let hv = MockHypervisor::default();
        let mut rx = hv.subscribe();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: vm.id,
                kind: EventKind::Started
            }
        );
        assert!(rx.try_recv().is_err());
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(st.status, RuntimeStatus::Running);
        assert_eq!(hv.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stop_removes_vm_and_is_idempotent() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::Stopped {
                guest_initiated: false
            }
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(matches!(
            hv.console_endpoint(vm.id).await,
            Err(DriverError::NotRunning(_))
        ));
    }

    #[tokio::test]
    async fn console_dir_sets_endpoint() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.set_console_dir("/tmp/consoles");
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.console_endpoint(vm.id).await.unwrap(),
            PathBuf::from(format!("/tmp/consoles/{}.vnc", vm.id))
        );
    }

    #[tokio::test]
    async fn injected_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
        hv.start(&vm, None).await.unwrap();
    }

    #[tokio::test]
    async fn injected_failure_does_not_apply_to_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 실패하지 않는다.
        hv.start(&vm, None).await.unwrap();
        // 주입한 오류는 소비되지 않고 다음 실제 기동에 적용된다.
        hv.crash(vm.id);
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
    }

    #[tokio::test]
    async fn acpi_stop_only_requests_shutdown() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.stop(vm.id, StopMode::Acpi).await.unwrap();
        // 게스트가 요청을 무시할 수 있으므로 VM은 그대로 돌고 이벤트도 없다.
        assert!(rx.try_recv().is_err());
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(st.status, RuntimeStatus::Running);
        assert_eq!(hv.list().await.unwrap().len(), 1);

        hv.guest_shutdown(vm.id);
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::Stopped {
                guest_initiated: true
            }
        );
    }

    #[tokio::test]
    async fn unresponsive_vm_cannot_be_controlled_but_can_be_killed() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
        let mut rx = hv.subscribe();
        hv.set_unresponsive(vm.id, true);

        let unresponsive = ActualState {
            id: vm.id,
            pid,
            status: RuntimeStatus::Other("unresponsive".into()),
        };
        assert_eq!(hv.inspect(vm.id).await.unwrap(), Some(unresponsive.clone()));
        assert_eq!(hv.list().await.unwrap(), vec![unresponsive]);
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::Timeout(_))
        ));
        assert!(matches!(
            hv.stop(vm.id, StopMode::Acpi).await,
            Err(DriverError::Timeout(_))
        ));

        // 다시 붙을 수 있게 되면 그대로 돌고 있다.
        hv.set_unresponsive(vm.id, false);
        hv.start(&vm, None).await.unwrap();
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!((st.pid, st.status), (pid, RuntimeStatus::Running));

        // 강제 종료는 프로세스를 죽이므로 종료 이벤트가 없다.
        hv.set_unresponsive(vm.id, true);
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn records_restore_tag_of_each_launch() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, Some("s1")).await.unwrap();
        // 이미 실행 중이면 새로 띄우지 않으므로 기록하지 않는다.
        hv.start(&vm, None).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.launches(),
            vec![(vm.id, Some("s1".to_string())), (vm.id, None)]
        );
    }

    #[tokio::test]
    async fn snapshot_ops_need_a_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        for res in [
            hv.snapshot_save(vm.id, "s", true).await,
            hv.snapshot_delete(vm.id, "s").await,
            hv.block_resize(vm.id, 0, 1 << 30).await,
            hv.pause(vm.id).await,
            hv.resume(vm.id).await,
        ] {
            assert!(matches!(res, Err(DriverError::NotRunning(_))), "{res:?}");
        }

        hv.start(&vm, None).await.unwrap();
        hv.snapshot_save(vm.id, "mem", true).await.unwrap();
        hv.snapshot_save(vm.id, "disk", false).await.unwrap();
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        // 없는 태그를 지워도 성공한다.
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        assert_eq!(hv.snapshots(vm.id), vec![("disk".to_string(), false)]);
        hv.block_resize(vm.id, 1, 2 << 30).await.unwrap();
        assert_eq!(hv.disk_size(vm.id, 1), Some(2 << 30));
        assert_eq!(hv.disk_size(vm.id, 0), None);
        hv.pause(vm.id).await.unwrap();
        assert!(hv.is_paused(vm.id));
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Paused);
        hv.resume(vm.id).await.unwrap();
        assert!(!hv.is_paused(vm.id));
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);

        // QMP로 다룰 수 없는 VM은 다시 시도할 수 있는 오류로 끝난다.
        hv.set_unresponsive(vm.id, true);
        let err = hv.snapshot_save(vm.id, "x", false).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
    }

    #[tokio::test]
    async fn injected_snapshot_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        // 저장은 태그를 남긴 채 실패한다.
        hv.fail_next_snapshot(DriverError::StartFailed("boom".into()));
        assert!(hv.snapshot_save(vm.id, "s", true).await.is_err());
        assert_eq!(hv.snapshots(vm.id), vec![("s".to_string(), true)]);
        hv.fail_next_snapshot(DriverError::StartFailed("boom".into()));
        assert!(hv.snapshot_delete(vm.id, "s").await.is_err());
        assert_eq!(hv.snapshots(vm.id).len(), 1);
        hv.snapshot_delete(vm.id, "s").await.unwrap();
        assert!(hv.snapshots(vm.id).is_empty());
    }

    fn target() -> MigrateTarget {
        MigrateTarget {
            host: "10.0.0.2".into(),
            port: 60000,
            tls_hostname: "10.0.0.2".into(),
        }
    }

    fn opts() -> MigrateOptions {
        MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(60),
        }
    }

    /// 원본 노드와 대상 노드의 하이퍼바이저로 한 번의 마이그레이션을 흉내 낸다.
    #[tokio::test]
    async fn migration_between_two_mocks() {
        let (src, dst) = (MockHypervisor::default(), MockHypervisor::default());
        let vm = spec();
        let task = Uuid::now_v7();
        assert_eq!(src.qemu_version().await.unwrap(), MOCK_QEMU_VERSION);
        assert!(matches!(
            src.running_qemu_version(vm.id).await,
            Err(DriverError::NotRunning(_))
        ));
        src.start(&vm, None).await.unwrap();
        assert_eq!(
            src.running_qemu_version(vm.id).await.unwrap(),
            dst.qemu_version().await.unwrap()
        );

        let mut events = dst.subscribe();
        let port = dst
            .start_incoming(&vm, task, "10.0.0.2", 60000..=60099)
            .await
            .unwrap();
        assert_eq!(port, 60000);
        assert_eq!(events.recv().await.unwrap().kind, EventKind::Started);
        assert!(dst.is_incoming(vm.id));
        assert_eq!(dst.list_incoming().await.unwrap(), vec![(vm.id, task)]);
        let status = |hv: &MockHypervisor| {
            let st = hv.state.lock().unwrap();
            st.vms
                .get(&vm.id)
                .map(|&pid| MockHypervisor::state_of(&st, vm.id, pid).status)
        };
        assert_eq!(status(&dst), Some(inmigrate()));
        // 아직 받는 중이면 넘겨받을 수 없다.
        assert!(matches!(
            dst.finish_incoming(vm.id).await,
            Err(DriverError::Migration(_))
        ));
        // 이미 돌고 있는 VM은 받을 수 없다.
        assert!(matches!(
            src.start_incoming(&vm, task, "10.0.0.1", 60000..=60099)
                .await,
            Err(DriverError::Migration(_))
        ));

        src.migrate(vm.id, &target(), &opts()).await.unwrap();
        assert_eq!(src.migrations(), vec![(vm.id, target())]);
        assert_eq!(status(&src), Some(postmigrate()));
        dst.incoming_wait(vm.id).await.unwrap();
        assert_eq!(status(&dst), Some(RuntimeStatus::Paused));
        dst.finish_incoming(vm.id).await.unwrap();
        assert_eq!(status(&dst), Some(RuntimeStatus::Running));
        assert!(!dst.is_incoming(vm.id));
        assert!(dst.list_incoming().await.unwrap().is_empty());
        // 두 번째 넘겨받기는 아무것도 하지 않는다.
        dst.finish_incoming(vm.id).await.unwrap();
        // 받는 중 표시가 없으면 기다릴 것이 없다.
        assert!(matches!(
            dst.incoming_wait(vm.id).await,
            Err(DriverError::Migration(_))
        ));

        // 돌고 있는 VM은 끄지 않는다.
        assert!(!dst.stop_migrated(vm.id).await.unwrap());
        assert!(src.stop_migrated(vm.id).await.unwrap());
        assert_eq!(src.inspect(vm.id).await.unwrap(), None);
        assert!(!src.stop_migrated(vm.id).await.unwrap());
    }

    #[tokio::test]
    async fn failed_migration_keeps_source_and_receiver_exits() {
        let (src, dst) = (MockHypervisor::default(), MockHypervisor::default());
        let vm = spec();
        let task = Uuid::now_v7();
        src.start(&vm, None).await.unwrap();
        src.fail_next_migrate(DriverError::Migration("Connection refused".into()));
        assert!(matches!(
            src.migrate(vm.id, &target(), &opts()).await,
            Err(DriverError::Migration(_))
        ));
        let state = src.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);

        // 받기에 실패한 QEMU는 크래시로 끝난다.
        dst.start_incoming(&vm, task, "10.0.0.2", 60000..=60099)
            .await
            .unwrap();
        let mut events = dst.subscribe();
        dst.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
        assert!(dst.incoming_wait(vm.id).await.is_err());
        assert_eq!(events.recv().await.unwrap().kind, EventKind::Crashed);
        assert!(dst.list_incoming().await.unwrap().is_empty());

        // 시작 실패를 주입하면 받기도 실패한다.
        dst.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            dst.start_incoming(&vm, task, "10.0.0.2", 60000..=60099)
                .await,
            Err(DriverError::StartFailed(_))
        ));
        assert_eq!(dst.inspect(vm.id).await.unwrap(), None);

        // 보낸 뒤 확정 전에 실패하면 원본을 이어서 실행한다.
        src.migrate(vm.id, &target(), &opts()).await.unwrap();
        assert_eq!(
            src.inspect(vm.id).await.unwrap().unwrap().status,
            postmigrate()
        );
        src.resume(vm.id).await.unwrap();
        assert_eq!(
            src.inspect(vm.id).await.unwrap().unwrap().status,
            RuntimeStatus::Running
        );
        assert!(!src.stop_migrated(vm.id).await.unwrap());

        // 받는 쪽에서 버전을 다르게 보이게 할 수 있다.
        dst.set_qemu_version("9.0.0");
        assert_eq!(dst.qemu_version().await.unwrap(), "9.0.0");
    }

    #[tokio::test]
    async fn cancelled_migration_does_not_complete_later() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let status = |hv: &MockHypervisor| {
            let st = hv.state.lock().unwrap();
            MockHypervisor::state_of(&st, vm.id, st.vms[&vm.id]).status
        };
        // 끝을 기다리지 못한 마이그레이션은 보내는 중으로 남고, 나중에 끝나면 원본이 멈춘다.
        hv.fail_next_migrate_unfinished(DriverError::Timeout("QMP query-migrate"));
        assert!(hv.migrate(vm.id, &target(), &opts()).await.is_err());
        assert!(hv.is_outgoing(vm.id));
        assert_eq!(status(&hv), RuntimeStatus::Running);
        hv.complete_outgoing(vm.id);
        assert_eq!(status(&hv), postmigrate());
        hv.resume(vm.id).await.unwrap();

        // 취소하면 끝나지 않는다.
        hv.start_outgoing(vm.id);
        hv.cancel_migration(vm.id).await.unwrap();
        assert!(!hv.is_outgoing(vm.id));
        hv.complete_outgoing(vm.id);
        assert_eq!(status(&hv), RuntimeStatus::Running);
        // 보내는 중이 아니거나 없는 VM이면 아무것도 하지 않는다.
        hv.cancel_migration(vm.id).await.unwrap();
        hv.cancel_migration(Uuid::now_v7()).await.unwrap();

        // 취소하는 사이에 끝날 수 있다.
        hv.complete_on_cancel();
        hv.start_outgoing(vm.id);
        hv.cancel_migration(vm.id).await.unwrap();
        assert_eq!(status(&hv), postmigrate());
    }

    #[tokio::test]
    async fn crash_and_guest_shutdown_emit_events() {
        let hv = MockHypervisor::default();
        let (a, b) = (spec(), spec());
        hv.start(&a, None).await.unwrap();
        hv.start(&b, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.crash(a.id);
        hv.guest_shutdown(b.id);
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: a.id,
                kind: EventKind::Crashed
            }
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: b.id,
                kind: EventKind::Stopped {
                    guest_initiated: true
                }
            }
        );
        assert!(hv.list().await.unwrap().is_empty());
    }
}
```

`crates/opl-driver/src/qemu.rs`에서

```rust
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(DriverError::StartFailed(stderr));
```

을 다음으로 바꾼다:

```rust
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if image_locked(&stderr) {
                return Err(DriverError::ImageLocked(stderr));
            }
            return Err(DriverError::StartFailed(stderr));
```

`crates/opl-driver/src/qemu.rs`에서

```rust

/// 끝난 마이그레이션의 결과
```

을 다음으로 바꾼다:

```rust

/// QEMU 시작 stderr가 다른 프로세스가 쥔 이미지 잠금 때문인지. QEMU(file-posix, 8.2와 11 같음)는
/// `Failed to get "write" lock`이나 `Failed to get shared "write" lock` 뒤에
/// `Is another process using the image [경로]?`를 붙인다.
fn image_locked(stderr: &str) -> bool {
    stderr.contains("\"write\" lock") || stderr.contains("Is another process using the image")
}

/// 끝난 마이그레이션의 결과
```

`crates/opl-driver/src/qemu.rs`에서

```rust
        assert_eq!(std::fs::read(vars).unwrap(), b"vars");
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(std::fs::read(vars).unwrap(), b"vars");
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    }

    /// 다른 프로세스(죽은 노드의 QEMU, NFS 임대가 아직 남은 잠금)가 이미지를 쥐고 있으면 QEMU는
    /// 이 문구로 끝난다(QEMU 8.2·11 같음). 잠시 뒤 다시 시도할 오류로 구분한다.
    #[tokio::test]
    async fn locked_image_is_a_transient_start_error() {
        let tmp = short_tempdir();
        let fake = tmp.path().join("fake-qemu");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             echo 'qemu-system-x86_64: -blockdev driver=qcow2,file=disk0-file,node-name=disk0: Failed to get \"write\" lock' >&2\n\
             echo 'Is another process using the image [/pool/d.qcow2]?' >&2\n\
             exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hv = QemuHypervisor::new(config(tmp.path(), fake)).await.unwrap();
        let vm = spec();

        let err = hv.start(&vm, None).await.unwrap_err();
        match &err {
            DriverError::ImageLocked(msg) => {
                assert!(msg.contains("Failed to get \"write\" lock"), "{msg}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(err.is_transient());
        assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
    }

    #[test]
    fn classifies_image_lock_messages() {
        // 쓰기를 원하는 쪽과 남이 쓰기를 공유하지 않는 쪽 모두
        assert!(image_locked(
            "qemu: -blockdev x: Failed to get \"write\" lock\nIs another process using the image [/d]?"
        ));
        assert!(image_locked(
            "qemu: -blockdev x: Failed to get shared \"write\" lock\nIs another process using the image [/d]?"
        ));
        assert!(!image_locked("qemu: something broke"));
        assert!(!image_locked(
            "qemu: Could not open '/d': No such file or directory"
        ));
    }
```

`crates/opl-driver/tests/qemu_driver.rs`에서

```rust

/// `OPL_TEST_SYSTEMD=1`(systemd가 있는 Linux)일 때만 확인한다: QEMU가 VM 전용 스코프에서 돈다.
```

을 다음으로 바꾼다:

```rust

/// 다른 QEMU(다른 노드)가 쓰는 이미지로 켜면 잠금 오류로 구분된다(실제 QEMU 문구 확인).
#[tokio::test]
async fn image_locked_by_another_qemu_is_transient() {
    let tmp = short_tempdir();
    let disk = tmp.path().join("disk0.qcow2");
    let vm = with_qcow2_disk(spec(), &disk);
    let a = QemuHypervisor::new(config(&tmp.path().join("a")))
        .await
        .unwrap();
    a.start(&vm, None).await.unwrap();

    // 같은 디스크를 쓰는 다른 VM(systemd 스코프 이름이 겹치지 않게 다른 id)
    let other = VmRuntimeSpec {
        id: Uuid::now_v7(),
        ..vm.clone()
    };
    let b = QemuHypervisor::new(config(&tmp.path().join("b")))
        .await
        .unwrap();
    let err = b.start(&other, None).await.unwrap_err();
    assert!(
        matches!(err, opl_driver::DriverError::ImageLocked(_)),
        "{err:?}"
    );
    assert!(err.is_transient());
    assert_eq!(b.inspect(other.id).await.unwrap(), None);

    // 잠금이 풀리면 켤 수 있다.
    a.stop(vm.id, StopMode::Force).await.unwrap();
    b.start(&other, None).await.unwrap();
    b.stop(other.id, StopMode::Force).await.unwrap();
}

/// `OPL_TEST_SYSTEMD=1`(systemd가 있는 Linux)일 때만 확인한다: QEMU가 VM 전용 스코프에서 돈다.
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
const HA_UNAVAILABLE: &str = "HA is not available on this node";
```

을 다음으로 바꾼다:

```rust
const HA_UNAVAILABLE: &str = "HA is not available on this node";
/// 다른 프로세스가 디스크 이미지를 잠가 두어 다시 시도하는 동안의 status
const IMAGE_LOCKED: &str = "disk image is locked by another process; retrying";
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
            }
            (Err(e), None) if e.is_transient() => {
```

을 다음으로 바꾼다:

```rust
            }
            // 다른 프로세스가 이미지를 잠가 두었다(죽은 노드의 NFS 임대 등, 설계 §4.3). 풀리면 켤 수
            // 있으므로 실패로 세지 않고 잠시 뒤 다시 시도한다. 사용자는 정지로 멈출 수 있다.
            (Err(e @ DriverError::ImageLocked(_)), None) => {
                tracing::warn!(vm = %self.id, error = %e, "disk image is locked; will retry");
                self.backoff = Some((
                    Instant::now() + self.config.retry_interval,
                    IMAGE_LOCKED.into(),
                ));
                Ok(Start::NotStarted)
            }
            (Err(e), None) if e.is_transient() => {
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
                    match self.cluster.get::<VmSpec>(self.id)? {
                        Some(current) => vm = current,
                        None => return Ok(()),
                    }
```

을 다음으로 바꾼다:

```rust
                    match self.cluster.get::<VmSpec>(self.id)? {
                        Some(current) if current.spec.node == Some(self.config.node) => {
                            vm = current
                        }
                        // 지워졌거나 그 사이 다른 노드로 옮겨졌다(HA 복구 등). 그 노드의 status를
                        // 덮지 않는다.
                        _ => return Ok(()),
                    }
```

`crates/opl-reconciler/tests/ha.rs` 전체:

```rust
//! HA VM과 리스(설계 §3.3 워커 규칙): 켜기·`cont` 문턱, 리스를 잃으면 강제 종료, 펜싱이 꺼진 노드,
//! 도는 HA VM 알림(`set_running`).

mod common;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use async_trait::async_trait;
use common::{Env, vm};
use opl_cluster::ClusterError;
use opl_driver::{
    Capacity, DriverError, Hypervisor, RuntimeStatus, SourceFormat, Storage, VolumeInfo,
    VolumeSource,
};
use opl_reconciler::ha::{LeaseRenewer, Watchdog, WatchdogOpener};
use opl_reconciler::{FirmwareError, FirmwareFetcher, HaConfig, HaGuard, ReconcilerConfig};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{
    DiskBus, DiskSpec, MigrationStatus, PowerState, RestartPolicy, Vm, VmPhase, VmSpec,
};
use uuid::Uuid;

const WAITING: &str = "waiting for the cluster majority before starting an HA VM";
const FENCED: &str = "fenced: lost the cluster majority";
const UNAVAILABLE: &str = "HA is not available on this node";

/// 정해 둔 대로 곧바로 갱신에 성공하거나 실패한다.
struct FakeRenewer(Mutex<bool>);

impl FakeRenewer {
    fn set_ok(&self, ok: bool) {
        *self.0.lock().unwrap() = ok;
    }
}

#[async_trait]
impl LeaseRenewer for FakeRenewer {
    async fn renew_lease(&self, _timeout: Duration) -> Result<(), ClusterError> {
        if *self.0.lock().unwrap() {
            Ok(())
        } else {
            Err(ClusterError::Timeout)
        }
    }
}

/// 장치 대신 한 일을 적는다.
#[derive(Default)]
struct FakeWatchdog {
    log: Arc<Mutex<Vec<&'static str>>>,
    fail_open: Mutex<bool>,
}

impl FakeWatchdog {
    fn count(&self, what: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|e| **e == what)
            .count()
    }
}

struct FakeDevice(Arc<Mutex<Vec<&'static str>>>);

impl Watchdog for FakeDevice {
    fn ping(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().push("ping");
        Ok(())
    }

    fn magic_close(self: Box<Self>) -> io::Result<()> {
        self.0.lock().unwrap().push("magic_close");
        Ok(())
    }
}

impl WatchdogOpener for FakeWatchdog {
    fn open(&self) -> io::Result<Box<dyn Watchdog>> {
        if *self.fail_open.lock().unwrap() {
            return Err(io::Error::new(io::ErrorKind::ResourceBusy, "busy"));
        }
        self.log.lock().unwrap().push("open");
        Ok(Box::new(FakeDevice(self.log.clone())))
    }
}

/// 첫 받아 오기에서 멈춘다: 켜기 문턱을 지난 뒤 `hv.start` 전이다.
#[derive(Default)]
struct GateFetcher {
    calls: Mutex<usize>,
    reached: Notify,
    release: Notify,
}

#[async_trait]
impl FirmwareFetcher for GateFetcher {
    async fn fetch(&self, _vm: Uuid, _node: Uuid) -> Result<(), FirmwareError> {
        let first = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls == 1
        };
        if first {
            self.reached.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

/// `gate`가 켜져 있으면 다음 볼륨 켜기에서 멈춘다: 도는 VM에 다시 붙으며 디스크를 해석하는 중이다.
#[derive(Default)]
struct GateStorage {
    gate: Mutex<bool>,
    reached: Notify,
    release: Notify,
}

#[async_trait]
impl Storage for GateStorage {
    async fn activate_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        let gated = std::mem::take(&mut *self.gate.lock().unwrap());
        if gated {
            self.reached.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
    async fn create_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: u64,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        unreachable!()
    }
    async fn import_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: &Path,
        _: SourceFormat,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        unreachable!()
    }
    async fn check_import_source(&self, _: &Path, _: SourceFormat) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn delete_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn volume_info(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        unreachable!()
    }
    async fn capacity(&self, _: &StoragePool) -> Result<Capacity, DriverError> {
        unreachable!()
    }
    fn supports_snapshots(&self, _: &StoragePool, _: ImageFormat) -> bool {
        unreachable!()
    }
    async fn snapshot_create(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_revert(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_delete(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_tags(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        unreachable!()
    }
    async fn resize_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: u64,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn volume_source(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        unreachable!()
    }
    async fn release_source(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn copy_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
        _: bool,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
}

struct Ha {
    renewer: Arc<FakeRenewer>,
    watchdog: Arc<FakeWatchdog>,
    guard: Arc<HaGuard>,
}

const FENCE_AFTER: Duration = Duration::from_millis(300);

/// 가짜 갱신과 가짜 watchdog로 guard를 만들어 reconciler에 넣는다. 리스는 300ms 뒤에 끝난다.
/// 부하로 런타임이 그만큼 멈추면 갱신에 성공하는 동안에도 리스가 잠깐 끝나(guard의 시계로는 맞다) 도는
/// HA VM을 끄고 갱신 뒤 다시 켠다. 리스가 끝나야 하는 시험만 쓰고, 그 시험은 그런 끄기·켜기에 기대지 않는다.
async fn start(ok: bool) -> (Env, Ha) {
    start_with(ok, FENCE_AFTER, |_| {}).await
}

async fn start_with(
    ok: bool,
    fence_after: Duration,
    tweak: impl FnOnce(&FakeWatchdog),
) -> (Env, Ha) {
    start_full(ok, fence_after, tweak, |_| {}).await
}

/// `config`로 reconciler 설정도 바꾼다.
async fn start_full(
    ok: bool,
    fence_after: Duration,
    tweak: impl FnOnce(&FakeWatchdog),
    config: impl FnOnce(&mut ReconcilerConfig),
) -> (Env, Ha) {
    let renewer = Arc::new(FakeRenewer(Mutex::new(ok)));
    let watchdog = Arc::new(FakeWatchdog::default());
    tweak(&watchdog);
    let guard = HaGuard::spawn(
        renewer.clone(),
        HaConfig {
            fence_after,
            renew_interval: Duration::from_millis(20),
            watchdog: Some(watchdog.clone()),
        },
    );
    let g = guard.clone();
    let env = Env::start_with(move |c: &mut ReconcilerConfig| {
        c.ha = Some(g);
        config(c);
    })
    .await;
    (
        env,
        Ha {
            renewer,
            watchdog,
            guard,
        },
    )
}

fn ha_vm(name: &str, node: Uuid) -> Vm {
    let mut vm = vm(name, node);
    vm.spec.ha = true;
    vm
}

fn running(v: &Vm) -> bool {
    v.status.phase == VmPhase::Running
}

fn error_is(v: &Vm, msg: &str) -> bool {
    v.status.last_error.as_deref() == Some(msg)
}

/// 리스를 갱신하기 전에는 HA VM을 켜지 않고(실패로 세지 않는다) 기다린다. 갱신되면 watchdog를 연 뒤 켠다.
#[tokio::test]
async fn ha_vm_waits_for_the_lease_before_starting() {
    // 시험하는 동안 stale이 되지 않게 리스를 길게 둔다.
    let (env, ha) = start_with(false, Duration::from_secs(60), |_| {}).await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let got = env.wait(vm.id, "waiting", |v| error_is(v, WAITING)).await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    // 기다리는 동안 다시 보아도 켜지 않고 원하는 상태도 그대로다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
    assert_eq!(ha.watchdog.count("open"), 0);
    assert_eq!(env.get(vm.id).spec.power, PowerState::Running);

    ha.renewer.set_ok(true);
    let got = env.wait(vm.id, "running", running).await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.hv.launches().len(), 1);
    assert_eq!(ha.watchdog.count("open"), 1);
}

/// 켜기 문턱을 지난 뒤 시작하기 전에 리스가 끊기면 켜지 않고 기다린다. 시작하는 동안에는 도는 HA VM으로
/// 알려 두어, 리스가 없으면 watchdog를 쓰다듬지 않는다.
#[tokio::test]
async fn ha_vm_is_not_started_if_the_lease_ends_while_starting() {
    let fetcher = Arc::new(GateFetcher::default());
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    let (env, ha) = start_full(true, FENCE_AFTER, |_| {}, |c| c.firmware = Some(f)).await;
    // 다른 노드의 UEFI 변수를 받아 온 뒤 켠다.
    let mut vm = ha_vm("web", env.node);
    vm.status.firmware_node = Some(Uuid::now_v7());
    env.create(&vm).await;
    fetcher.reached.notified().await;
    assert_eq!(ha.watchdog.count("open"), 1);

    ha.renewer.set_ok(false);
    ha.guard.subscribe().wait_for(|s| s.stale).await.unwrap();
    let pinged = ha.watchdog.count("ping");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(ha.watchdog.count("ping"), pinged, "pinged while starting");

    fetcher.release.notify_one();
    let got = env.wait(vm.id, "waiting", |v| error_is(v, WAITING)).await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    assert_eq!(got.spec.power, PowerState::Running);
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
    // 켜지 않았으므로 도는 HA VM이 아니다: 다시 쓰다듬는다.
    let deadline = std::time::Instant::now() + common::WAIT;
    while ha.watchdog.count("ping") == pinged {
        assert!(std::time::Instant::now() < deadline, "not pinging again");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // 리스가 돌아오면 켠다. 켠 뒤 런타임이 멈추면 다시 끄고 켤 수 있으므로 횟수는 보지 않는다.
    ha.renewer.set_ok(true);
    let got = env.wait(vm.id, "running", running).await;
    assert_eq!(got.status.last_error, None);
}

/// HA가 아닌 VM은 리스와 상관없이 켠다.
#[tokio::test]
async fn non_ha_vm_starts_without_the_lease() {
    let (env, ha) = start(false).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(ha.watchdog.count("open"), 0);
}

/// watchdog를 열지 못하면 HA VM을 켜지 않고 오류를 남긴다.
#[tokio::test]
async fn ha_vm_does_not_start_when_the_watchdog_cannot_be_opened() {
    let (env, ha) = start_with(true, FENCE_AFTER, |w| *w.fail_open.lock().unwrap() = true).await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "watchdog error", |v| {
            v.status
                .last_error
                .as_deref()
                .is_some_and(|e| e.starts_with("cannot open the watchdog device"))
        })
        .await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    assert!(env.hv.launches().is_empty());

    *ha.watchdog.fail_open.lock().unwrap() = false;
    env.wait(vm.id, "running", running).await;
}

/// 리스를 잃으면 도는 HA VM을 강제로 끈다. 우리가 요청한 종료이므로 실패로 세지 않고(`restart = never`여도
/// 원하는 상태가 그대로다) 리스가 돌아오면 다시 켠다. HA가 아닌 VM은 그대로 둔다.
#[tokio::test]
async fn lost_lease_force_stops_ha_vms_and_restarts_them_when_it_returns() {
    let (env, ha) = start(true).await;
    let mut vm = ha_vm("web", env.node);
    vm.spec.restart = RestartPolicy::Never;
    let plain = common::vm("plain", env.node);
    env.create(&vm).await;
    env.create(&plain).await;
    env.wait(vm.id, "running", running).await;
    let plain_pid = env.wait(plain.id, "running", running).await.status.pid;

    ha.renewer.set_ok(false);
    let got = env.wait(vm.id, "fenced", |v| error_is(v, FENCED)).await;
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.phase, VmPhase::Starting);
    assert_eq!(got.status.pid, None);
    assert_eq!(env.pid(vm.id).await, None);
    // set_ok(false) 전에 런타임이 멈춰 리스가 잠깐 끝났으면 web을 이미 한 번 끄고 다시 켰을 수 있다(그때는
    // 갱신 뒤라 맞다). 그래서 펜싱을 본 뒤의 횟수에서 센다: 리스가 없는 동안에는 켜지 않는다.
    let fenced_web = launches_of(&env, vm.id);
    // 이벤트를 처리한 뒤에도 실패로 남지 않는다.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let got = env.get(vm.id);
    assert_eq!(got.spec.power, PowerState::Running);
    assert_ne!(got.status.phase, VmPhase::Failed);
    assert_eq!(
        launches_of(&env, vm.id),
        fenced_web,
        "web launched while fenced"
    );
    assert_eq!(launches_of(&env, plain.id), 1, "plain launched again");
    assert_eq!(env.get(plain.id).status.pid, plain_pid);
    assert_eq!(env.pid(plain.id).await, plain_pid);

    ha.renewer.set_ok(true);
    let got = env.wait(vm.id, "running again", running).await;
    assert_eq!(got.status.last_error, None);
    assert!(launches_of(&env, vm.id) > fenced_web);
    assert_eq!(launches_of(&env, plain.id), 1, "plain launched again");
}

/// `id`를 켠 횟수
fn launches_of(env: &Env, id: Uuid) -> usize {
    env.hv.launches().iter().filter(|(v, _)| *v == id).count()
}

/// 펜싱이 꺼진 노드(guard 없음)에서는 HA VM을 켜지 않는다.
#[tokio::test]
async fn ha_vm_does_not_start_on_a_node_without_fencing() {
    let env = Env::start().await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "unavailable", |v| error_is(v, UNAVAILABLE))
        .await;
    assert_ne!(got.status.phase, VmPhase::Running);
    assert!(env.hv.launches().is_empty());
}

/// 펜싱이 꺼진 노드에서 이미 도는 VM을 HA로 바꿔도 끄지 않고 그 사실만 남긴다.
#[tokio::test]
async fn running_ha_vm_on_a_node_without_fencing_is_left_running() {
    let env = Env::start().await;
    let mut vm = vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    let current = env.get(vm.id);
    vm.spec.ha = true;
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    let got = env
        .wait(vm.id, "unavailable", |v| error_is(v, UNAVAILABLE))
        .await;
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.pid, pid);
    assert_eq!(env.pid(vm.id).await, pid);
}

/// reconciler가 멈춰도 도는 HA VM은 도는 것으로 남는다: 정상 종료 때 watchdog를 그대로 둔다.
#[tokio::test]
async fn shutdown_keeps_running_ha_vms_armed() {
    // 시험하는 동안 stale이 되지 않게 리스를 길게 둔다.
    let (mut env, ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 0);
}

/// 멈춘 HA VM은 도는 것에서 빠진다: 정상 종료 때 watchdog를 멈춘다.
#[tokio::test]
async fn stopped_ha_vm_is_no_longer_running() {
    let (mut env, ha) = start(true).await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 1);
}

/// 다른 노드로 옮긴 HA VM은 작업자가 물러나며 도는 것에서 뺀다.
#[tokio::test]
async fn ha_vm_moved_away_is_no_longer_running() {
    let (mut env, ha) = start(true).await;
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    env.hv
        .stop(vm.id, opl_driver::StopMode::Force)
        .await
        .unwrap();
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 1);
}

/// 이미 도는 VM이 HA가 되면 리스가 살아 있을 때 watchdog를 연다. 그 전에는 오류 없이 기다렸다가
/// 리스가 살아나면 연다.
#[tokio::test]
async fn running_ha_vm_arms_the_watchdog_once_the_lease_is_fresh() {
    // 시험하는 동안 stale이 되지 않게 리스를 길게 둔다.
    let (env, ha) = start_with(false, Duration::from_secs(60), |_| {}).await;
    let mut vm = common::vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    let current = env.get(vm.id);
    vm.spec.ha = true;
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let got = env.get(vm.id);
    assert!(got.spec.ha);
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.last_error, None);
    assert_eq!(ha.watchdog.count("open"), 0);

    ha.renewer.set_ok(true);
    let deadline = std::time::Instant::now() + common::WAIT;
    while ha.watchdog.count("open") == 0 {
        assert!(std::time::Instant::now() < deadline, "watchdog not armed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(env.pid(vm.id).await, pid);
    assert_eq!(env.get(vm.id).status.last_error, None);
}

/// 다른 노드로 옮겨졌어도 QEMU가 아직 돌면 도는 HA VM으로 남긴다: 정상 종료 때 watchdog를 그대로 둔다.
#[tokio::test]
async fn ha_vm_moved_away_while_running_stays_running() {
    // 시험하는 동안 stale이 되지 않게 리스를 길게 둔다.
    let (mut env, ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.pid(vm.id).await, pid);
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 0);
}

/// HA VM을 다른 노드로 옮긴 마이그레이션이 성공했다: 작업자는 보류 중에 물러나고(원본 QEMU가 아직
/// 있다) 원본은 그 뒤에 멈춘다. 작업자 없이 온 그 종료 이벤트로 도는 HA VM에서 빼므로 정상 종료 때
/// watchdog를 멈춘다.
#[tokio::test]
async fn ha_vm_migrated_away_is_dropped_when_its_source_stops() {
    let (mut env, ha) = start(true).await;
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    let handle = env.handle();
    handle.begin_migration(vm.id).await;
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // 마이그레이션 Task가 원본을 끄고 보류를 푼다.
    env.hv
        .stop(vm.id, opl_driver::StopMode::Force)
        .await
        .unwrap();
    handle.end_migration(vm.id);
    tokio::time::sleep(Duration::from_millis(300)).await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 1);
}

/// 옮겨 간 HA VM의 QEMU가 이벤트 없이 사라졌어도 주기 재동기화가 도는 HA VM에서 뺀다.
#[tokio::test]
async fn resync_drops_ha_vms_no_longer_on_this_node() {
    let (mut env, ha) = start_full(
        true,
        FENCE_AFTER,
        |_| {},
        |c| c.resync_interval = Duration::from_millis(200),
    )
    .await;
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // QMP로 다룰 수 없는 VM의 강제 종료는 이벤트 없이 사라진다.
    env.hv.set_unresponsive(vm.id, true);
    env.hv
        .stop(vm.id, opl_driver::StopMode::Force)
        .await
        .unwrap();
    assert_eq!(env.pid(vm.id).await, None);
    tokio::time::sleep(Duration::from_millis(600)).await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 1);
}

/// 다른 노드로 배정을 바꾼다.
async fn reassign(env: &Env, id: Uuid) {
    let current = env.get(id);
    let mut spec = current.spec.clone();
    spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(id, Some(current.revision), current.labels, &spec)
        .await
        .unwrap();
}

/// 켜기 문턱을 지난 뒤 시작하기 전에 다른 노드로 배정됐으면(리스는 살아 있다: 과반수 쪽의 복구 쓰기가
/// 이미 적용됐다) 켜지 않고 status도 덮어쓰지 않는다.
#[tokio::test]
async fn ha_vm_reassigned_while_starting_is_not_started() {
    let fetcher = Arc::new(GateFetcher::default());
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    let (env, _ha) = start_full(
        true,
        Duration::from_secs(60),
        |_| {},
        |c| c.firmware = Some(f),
    )
    .await;
    let mut vm = ha_vm("web", env.node);
    vm.status.firmware_node = Some(Uuid::now_v7());
    env.create(&vm).await;
    fetcher.reached.notified().await;
    reassign(&env, vm.id).await;
    fetcher.release.notify_one();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
    assert_eq!(env.get(vm.id).status.node, None);
}

/// 켜기 문턱을 지난 뒤 시작하기 전에 원하는 상태가 stopped로 바뀌었으면 켜지 않는다.
#[tokio::test]
async fn ha_vm_stopped_while_starting_is_not_started() {
    let fetcher = Arc::new(GateFetcher::default());
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    let (env, _ha) = start_full(
        true,
        Duration::from_secs(60),
        |_| {},
        |c| c.firmware = Some(f),
    )
    .await;
    let mut vm = ha_vm("web", env.node);
    vm.status.firmware_node = Some(Uuid::now_v7());
    env.create(&vm).await;
    fetcher.reached.notified().await;
    env.set_power(vm.id, PowerState::Stopped).await;
    fetcher.release.notify_one();
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
}

/// 다룰 수 없는 HA VM에 다시 붙는 동안(디스크 해석) 다른 노드로 배정되고 QEMU가 사라졌으면 새로 띄우지
/// 않는다.
#[tokio::test]
async fn ha_vm_reassigned_while_reattaching_is_not_started() {
    let storage = Arc::new(GateStorage::default());
    let s: Arc<dyn Storage> = storage.clone();
    let (env, _ha) = start_full(true, Duration::from_secs(60), |_| {}, |c| c.storage = s).await;
    let pool: StoragePool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/pools/local".into(),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    let mut vm = ha_vm("web", env.node);
    vm.spec.disks.push(DiskSpec {
        volume: "root".into(),
        bus: DiskBus::VirtioBlk,
        boot_order: Some(1),
    });
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(env.hv.launches().len(), 1);

    // QMP로 다룰 수 없게 되면 작업자가 다시 붙는다(`start`).
    env.hv.set_unresponsive(vm.id, true);
    *storage.gate.lock().unwrap() = true;
    env.handle().end_migration(vm.id);
    storage.reached.notified().await;
    reassign(&env, vm.id).await;
    // 이벤트 없이 사라진다.
    env.hv
        .stop(vm.id, opl_driver::StopMode::Force)
        .await
        .unwrap();
    storage.release.notify_one();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.hv.launches().len(), 1, "{:?}", env.hv.launches());
    assert_eq!(env.pid(vm.id).await, None);
}

/// 켜는 동안 리스가 끝나고 다른 노드로 옮겨졌으면, 켜진 QEMU를 강제로 끄고 도는 HA VM에서 뺀다.
#[tokio::test]
async fn ha_vm_moved_away_after_the_lease_ended_is_force_stopped() {
    let (env, ha) = start(true).await;
    env.hv.delay_starts(Duration::from_secs(1));
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    // 켜기 문턱을 지나 hv.start 안에 있다.
    tokio::time::sleep(Duration::from_millis(100)).await;
    ha.renewer.set_ok(false);
    ha.guard.subscribe().wait_for(|s| s.stale).await.unwrap();
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();

    let deadline = std::time::Instant::now() + common::WAIT;
    while env.hv.launches().is_empty() || env.pid(vm.id).await.is_some() {
        assert!(std::time::Instant::now() < deadline, "still running");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // 끈 뒤에는 도는 HA VM이 없으므로 리스가 없어도 다시 쓰다듬는다.
    let pinged = ha.watchdog.count("ping");
    while ha.watchdog.count("ping") == pinged {
        assert!(std::time::Instant::now() < deadline, "not pinging again");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 이 노드로 확정된 HA VM의 마이그레이션은 리스가 살아 있어야 이어서 실행한다(`cont`).
#[tokio::test]
async fn received_ha_vm_is_continued_only_with_the_lease() {
    // 시험하는 동안 stale이 되지 않게 리스를 길게 둔다.
    let (env, ha) = start_with(false, Duration::from_secs(60), |_| {}).await;
    let source = Uuid::now_v7();
    let mut vm = ha_vm("web", source);
    env.create(&vm).await;
    let mut status = env.get(vm.id).status;
    status.phase = VmPhase::Migrating;
    status.migration = Some(MigrationStatus {
        node: env.node,
        task: Uuid::now_v7(),
    });
    env.cluster
        .update_status::<VmSpec>(vm.id, &status)
        .await
        .unwrap();
    let spec = VmRuntimeSpec {
        id: vm.id,
        name: vm.name.clone(),
        cpu: vm.spec.cpu.clone(),
        memory_mib: vm.spec.memory_mib,
        firmware: vm.spec.firmware,
        machine: vm.spec.machine,
        disks: vec![],
        nics: vec![],
    };
    env.hv
        .start_incoming(&spec, Uuid::now_v7(), "127.0.0.1", 60000..=60099)
        .await
        .unwrap();
    env.hv.incoming_wait(vm.id).await.unwrap();

    // 원본이 확정한다. 리스를 갱신하지 못했으므로(아직 stale은 아니다) 멈춘 채 기다린다.
    let current = env.get(vm.id);
    vm.spec.node = Some(env.node);
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    let got = env.wait(vm.id, "waiting", |v| error_is(v, WAITING)).await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Paused)
    );
    assert!(env.hv.is_incoming(vm.id));

    ha.renewer.set_ok(true);
    let got = env
        .wait(vm.id, "taken over", |v| {
            running(v) && v.status.migration.is_none() && v.status.last_error.is_none()
        })
        .await;
    assert!(got.status.pid.is_some());
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.hv.is_incoming(vm.id) {
        assert!(std::time::Instant::now() < deadline, "still incoming");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Running)
    );
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
    assert_eq!(ha.watchdog.count("open"), 1);
}

/// opld가 HA VM의 마이그레이션을 보내다 확정 전에 죽고 다시 떴다. 다시 뜬 opld의 guard는 아직 갱신하지
/// 못했다: 남은 표시를 지우고 `postmigrate`로 멈춘 원본을 리스를 기다리며(Starting) 그대로 둔다.
/// VM과 원본의 pid, 새 guard의 갱신을 돌려준다.
async fn ha_source_left_by_a_restart(env: &mut Env) -> (Vm, Option<u32>, Arc<FakeRenewer>) {
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    env.stop_reconciler().await;
    let mut status = env.get(vm.id).status;
    status.phase = VmPhase::Migrating;
    status.migration = Some(MigrationStatus {
        node: Uuid::now_v7(),
        task: Uuid::now_v7(),
    });
    env.cluster
        .update_status::<VmSpec>(vm.id, &status)
        .await
        .unwrap();
    let target = opl_driver::MigrateTarget {
        host: "127.0.0.2".into(),
        port: 60000,
        tls_hostname: "127.0.0.2".into(),
    };
    let opts = opl_driver::MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(60),
    };
    env.hv.migrate(vm.id, &target, &opts).await.unwrap();
    let renewer = Arc::new(FakeRenewer(Mutex::new(false)));
    env.config.ha = Some(HaGuard::spawn(
        renewer.clone(),
        HaConfig {
            fence_after: Duration::from_secs(60),
            renew_interval: Duration::from_millis(20),
            watchdog: None,
        },
    ));
    env.start_reconciler();

    let got = env
        .wait(vm.id, "waiting", |v| {
            error_is(v, WAITING) && v.status.migration.is_none()
        })
        .await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let postmigrate = RuntimeStatus::Other("postmigrate".into());
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(postmigrate)
    );
    (vm, pid, renewer)
}

/// 남은 마이그레이션의 HA 원본은 리스를 갱신한 뒤에 이어서 실행한다.
#[tokio::test]
async fn ha_source_left_by_a_restart_is_resumed_only_with_the_lease() {
    let (mut env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let (vm, pid, renewer) = ha_source_left_by_a_restart(&mut env).await;

    renewer.set_ok(true);
    let got = env
        .wait(vm.id, "resumed", |v| {
            running(v) && v.status.last_error.is_none()
        })
        .await;
    assert_eq!(got.status.pid, pid);
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Running)
    );
    assert_eq!(env.hv.launches().len(), 1);
}

/// 리스를 기다리는 원본의 VM이 HA가 아니게 되어도 낡은 원본으로 보고 끄지 않고 이어서 실행한다.
#[tokio::test]
async fn waiting_ha_source_is_resumed_when_ha_is_turned_off() {
    let (mut env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let (vm, pid, _renewer) = ha_source_left_by_a_restart(&mut env).await;

    let current = env.get(vm.id);
    let mut spec = current.spec.clone();
    spec.ha = false;
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &spec)
        .await
        .unwrap();
    let got = env
        .wait(vm.id, "resumed", |v| {
            running(v) && v.status.last_error.is_none()
        })
        .await;
    assert_eq!(got.status.pid, pid);
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Running)
    );
    assert_eq!(env.hv.launches().len(), 1, "{:?}", env.hv.launches());
}
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
use common::{Env, vm};
use opl_driver::{
```

을 다음으로 바꾼다:

```rust
use common::{Env, vm};
use opl_cluster::watch::WatchEvent;
use opl_driver::{
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
```

을 다음으로 바꾼다:

```rust
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::{Kind, Resource};
use opl_types::runtime::ImageFormat;
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
}

#[tokio::test]
async fn unresolvable_volume_marks_failed() {
```

을 다음으로 바꾼다:

```rust
}

/// 다른 프로세스가 디스크 이미지를 잠가 둔 동안(죽은 노드의 NFS 임대 등)은 실패로 세지 않고
/// 다시 시도한다. 하네스는 3번 실패하면 포기하지만 그보다 많이 잠겨 있어도 결국 켠다.
#[tokio::test]
async fn locked_image_is_retried_without_counting_failures() {
    let env = Env::start().await;
    env.hv.fail_next_starts(
        (0..4).map(|_| DriverError::ImageLocked("Failed to get \"write\" lock".into())),
    );
    let (since, _) = env.cluster.list::<VmSpec>().unwrap();
    let mut watch = env.cluster.watch(since).unwrap();
    let vm = vm("web", env.node);
    env.create(&vm).await;

    let got = env.wait(vm.id, "running", running).await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(env.hv.launches().len(), 5);
    // 잠긴 동안의 status는 Starting과 잠금 문구이고 Failed가 되지 않는다.
    let mut locked = 0;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), watch.next())
            .await
            .unwrap()
            .unwrap();
        let WatchEvent::Put {
            kind: Kind::Vm,
            resource,
        } = event
        else {
            continue;
        };
        if resource.id != vm.id {
            continue;
        }
        let status = &resource.status;
        assert_ne!(status["phase"], "failed");
        if status["last_error"] == "disk image is locked by another process; retrying" {
            assert_eq!(status["phase"], "starting");
            locked += 1;
        }
        if status["phase"] == "running" {
            break;
        }
    }
    assert!(locked >= 1);
}

#[tokio::test]
async fn unresolvable_volume_marks_failed() {
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
    assert_eq!(env.get(vm.id).status.schedule_error, Some(busy()));
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(env.get(vm.id).status.schedule_error, Some(busy()));
}

/// status를 쓰다 revision 불일치로 다시 읽었을 때 그 사이 다른 노드로 옮겨졌으면 쓰지 않는다(낡은
/// 로컬 저장소로 다시 시작한 노드가 HA로 옮겨 간 VM의 status를 덮지 않게).
#[tokio::test]
async fn status_is_not_written_after_the_vm_moved_away_meanwhile() {
    let env = Env::start().await;
    let vm = stopped_vm(&env).await;

    // 작업자가 VM을 읽고 시작하는 사이에 VM을 다른 노드로 옮긴다.
    env.hv.hold_next_start();
    env.set_power(vm.id, PowerState::Running).await;
    env.hv.start_held().await;
    let current = env.get(vm.id);
    assert_ne!(current.status.phase, VmPhase::Running);
    let mut spec = current.spec.clone();
    spec.node = Some(Uuid::now_v7());
    let revision = env
        .cluster
        .update_spec(vm.id, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();
    env.hv.release_start();

    // 시작이 끝난 뒤 Running status를 쓰려다 revision 불일치로 다시 읽고 멈춘다.
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.pid(vm.id).await.is_none() {
        assert!(std::time::Instant::now() < deadline, "not started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let now = env.get(vm.id);
    assert_eq!(now.revision, revision);
    assert_eq!(now.status, current.status);
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 822개, 실패 0

Run: `cargo test -p opl-driver -p opl-reconciler`
Expected: PASS

Run: `cargo test -p opl-driver --features qemu-it --test qemu_driver`
Expected: macOS에서 PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat: 이미지 잠금 기다리기 — QEMU 잠금 오류를 ImageLocked로 구분, 워커가 세지 않고 다시"
```

---

### Task 3: 리더의 HaManager

opl-api `ha.rs`, 리더만 1초마다: 임기마다 기준점 b, 노드의 리스 시각 r은 `leases()`. 펜싱할 노드는 자기가 아니고 `status.ha`가 있고 `now − max(b, r) ≥ recover_after_secs`이고 복구할 VM(`spec.node == N`, `spec.ha`, power Running)이 있는 노드. `begin_fence(N)` 뒤 리스를 다시 읽어 아직 만료인지 확인하고, VM마다(이름 순) 대상(`node_fits` + 리스가 살아 있음, N과 받던 마이그레이션 노드 제외, 같은 틱에 옮긴 메모리를 셈)을 골라 status(revision 검사: Stopped, node·pid·migration 없음, "recovering: node {N} failed", `firmware_node` 유지) → `spec.node = T`(revision 검사) → N의 끝나지 않은 Task 실패(다시 읽어서) → 기록 Task `VmHaRecover { from }`(종류 `vm-ha-recover`, 처음부터 Succeeded). 대상이 없으면 "HA cannot find a node for this VM". 쓰기는 리더에서만(`RecoveryWriter`). 펜싱 해제는 guard가 모든 경로에서 — 단 결과를 모르는 쓰기(Timeout 등) 뒤에는 같은 임기에서 같은 쓰기를 Ok·Rejected까지 다시 보내고, 임기가 바뀌면 풀고(spec 쓰기였으면 남은 단계는 다음에 리더일 때), 종료로 취소된 쓰기 중이면 풀지 않는다. opld가 일반 노드에서 띄운다.

**Files:**
- Create: `crates/opl-api/src/ha.rs`
- Create: `crates/opl-api/tests/ha.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-types/src/task.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/ha.rs`를 만든다:

```rust
//! 리더의 HA 복구(설계 2026-11-02-ha-fencing-witness-design.md §4.1·§4.2). 리더만 `tick`마다 노드의
//! 리스를 보고, 리스가 `recover_after`보다 오래 끝난 노드의 HA VM(실행하기로 된 것)을 다른 HA 가능
//! 노드로 옮긴다. 그 노드는 그사이 HA VM을 스스로 껐다(펜싱). 옮기는 동안 그 노드를 펜싱 중으로
//! 표시해 갱신을 거부한다.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::types::{Command, LeaderRequest, NodeId};
use opl_cluster::{Cluster, ClusterError, Leases, Typed};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::scheduler::{Inventory, node_fits};
use crate::state::AppState;

/// 옮길 노드를 찾지 못한 HA VM의 `last_error`
const NO_NODE: &str = "HA cannot find a node for this VM";

/// 실행 중인 다른 쪽(그 노드의 실행기, 정리 작업)과 엇갈려 Task를 실패로 끝내지 못했을 때 다시 읽는 횟수
const TASK_FAIL_ATTEMPTS: usize = 3;

#[derive(Debug, Clone)]
pub struct HaManagerConfig {
    /// 리스를 확인하는 간격
    pub tick: Duration,
}

impl Default for HaManagerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(1),
        }
    }
}

/// HA 복구 반복 작업. 모든 노드(위트니스 빼고)에서 띄우고, 리더인 노드에서만 일한다.
pub struct HaManager {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl HaManager {
    pub fn spawn(state: Arc<AppState>, config: HaManagerConfig) -> Self {
        Self::spawn_with_writer(state, config, Arc::new(LeaderWriter))
    }

    /// 복구 쓰기 경로를 바꿔 끼워 띄운다(시험).
    pub fn spawn_with_writer(
        state: Arc<AppState>,
        config: HaManagerConfig,
        writer: Arc<dyn RecoveryWriter>,
    ) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, config, writer, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// 리더로 본 임기와 그 임기에서 리더인 것을 처음 본 시각(기준점 b). 리스를 받은 적 없는 노드의
/// 리스는 b에 끝난 것으로 본다(리더가 바뀌면 리스 표가 비므로).
type Base = Option<(u64, Instant)>;

/// 결과를 모르는 복구 쓰기(`Timeout` 등). 리더 로그에 남아 나중에 커밋될 수 있으므로, 같은 임기에서는
/// 결과를 알 때까지 그 노드의 펜싱 표시를 지우지 않고 같은 쓰기(revision 검사라 다시 해도 된다)를 다시
/// 보낸다. 임기가 바뀌면 새 임기의 리더가 커밋한 인덱스가 살아남은 옛 쓰기를 포함하므로 지워도 된다
/// (spec 쓰기였으면 남은 단계는 `Moved`로 이어 간다).
struct Unknown {
    /// 쓰기가 실패로 돌아온 뒤 본 임기(쓰기의 임기보다 작지 않다)
    term: u64,
    vm: Uuid,
    command: Command,
    /// spec 쓰기였으면 옮긴 노드(결과를 안 뒤 남은 단계를 한다)
    moved_to: Option<Uuid>,
}

/// 결과를 모르는 쓰기가 있는 노드
type Unknowns = BTreeMap<Uuid, Unknown>;

/// 결과를 모른 채 임기 변경으로 정리한 spec 쓰기. 이 노드가 리더일 때 VM이 `to`로 가 있으면 남은 단계를
/// 한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Moved {
    vm: Uuid,
    from: Uuid,
    to: Uuid,
}

async fn run(
    state: Arc<AppState>,
    config: HaManagerConfig,
    writer: Arc<dyn RecoveryWriter>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut base: Base = None;
    let mut unknown = Unknowns::new();
    let mut moved: Vec<Moved> = vec![];
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 선거 중에는 기록이 오래 걸릴 수 있으므로 종료 요청과 겨룬다. 멈추면 펜싱 표시는
        // `Fence`가 지운다. 쓰는 중이었거나 결과를 모르는 쓰기가 있으면 지우지 않는다(리더 자리는 곧
        // 없어지고 새 리더는 빈 표에서 다시 센다).
        let ctx = Ctx {
            cluster: &state.cluster,
            writer: writer.as_ref(),
        };
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(ctx, &mut base, &mut unknown, &mut moved) => {}
        }
    }
}

async fn tick(ctx: Ctx<'_>, base: &mut Base, unknown: &mut Unknowns, moved: &mut Vec<Moved>) {
    let cluster = ctx.cluster;
    for node in settled_by_term(unknown, ctx.writer.term(cluster), moved) {
        cluster.end_fence(NodeId(node));
    }
    if cluster.members().leader != Some(cluster.id()) {
        *base = None;
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let leases = cluster.leases();
    let now = Instant::now();
    let b = base_at(base, leases.term, now);
    finish_moved(ctx, moved).await;
    resend(ctx, unknown).await;
    let Ok(inv) = Inventory::load(cluster) else {
        return;
    };
    let me = cluster.id().0;
    let failed: Vec<Uuid> = inv
        .nodes
        .iter()
        .filter(|n| n.id != me && !unknown.contains_key(&n.id))
        .filter(|n| expired(now, b, &leases, n))
        .filter(|n| inv.vms.iter().any(|vm| recoverable(vm, n.id)))
        .map(|n| n.id)
        .collect();
    for node in failed {
        if let Some(u) = recover_node(ctx, leases.term, b, node).await {
            unknown.insert(node, u);
        }
    }
}

/// 임기 `term`의 기준점 b. 그 임기에서 처음이면(임기가 바뀌었거나 리더가 아니었으면) `now`로 잡는다.
fn base_at(base: &mut Base, term: u64, now: Instant) -> Instant {
    match *base {
        Some((t, b)) if t == term => b,
        _ => {
            *base = Some((term, now));
            now
        }
    }
}

/// 임기가 바뀌어 펜싱을 풀어도 되는 노드를 `unknown`에서 빼 돌려준다. spec 쓰기였으면 남은 단계를 하도록
/// `moved`에 옮긴다(그 쓰기는 커밋됐을 수도 버려졌을 수도 있다).
fn settled_by_term(unknown: &mut Unknowns, term: u64, moved: &mut Vec<Moved>) -> Vec<Uuid> {
    let nodes: Vec<Uuid> = unknown
        .iter()
        .filter(|(_, u)| u.term != term)
        .map(|(node, _)| *node)
        .collect();
    for node in &nodes {
        if let Some(Unknown {
            vm,
            moved_to: Some(to),
            ..
        }) = unknown.remove(node)
        {
            moved.push(Moved {
                vm,
                from: *node,
                to,
            });
        }
    }
    nodes
}

/// 임기 변경으로 정리한 spec 쓰기의 남은 단계. 장벽 뒤라 그 쓰기의 결과가 정해져 있다. VM이 그 노드로 가
/// 있으면 남은 단계를 하고, 아니면(쓰기가 버려졌거나, 다른 리더가 끝냈거나, 사용자가 옮겼거나, VM이 없다)
/// 그냥 지운다. 버려졌으면 VM은 아직 죽은 노드에 있어 평소대로 다시 옮긴다.
async fn finish_moved(ctx: Ctx<'_>, moved: &mut Vec<Moved>) {
    let mut kept = vec![];
    for m in moved.drain(..) {
        let (vm, failed) = match (
            ctx.cluster.get::<VmSpec>(m.vm),
            ctx.cluster.get::<NodeSpec>(m.from),
        ) {
            (Ok(vm), Ok(failed)) => (vm, failed),
            // 읽지 못하면 다음 tick에 다시 본다.
            _ => {
                kept.push(m);
                continue;
            }
        };
        if let (Some(vm), Some(failed)) = (vm, failed)
            && vm.spec.node == Some(m.to)
        {
            finish(ctx, &vm, &failed, m.to).await;
        }
    }
    *moved = kept;
}

/// 결과를 모르는 쓰기를 다시 보낸다. 결과를 알면(이번에 적용됐거나, 앞의 것이 커밋됐거나 다른 쓰기가
/// 이겨 revision이 다르다) 펜싱을 풀고, spec 쓰기였으면 남은 단계를 한다. 나머지는 다음 tick에 평소대로
/// VM을 다시 읽어 이어 간다.
async fn resend(ctx: Ctx<'_>, unknown: &mut Unknowns) {
    let nodes: Vec<Uuid> = unknown.keys().copied().collect();
    for node in nodes {
        let command = unknown[&node].command.clone();
        let result = ctx.writer.write(ctx.cluster, command).await;
        if !settled(&result) {
            tracing::warn!(node = %node, result = ?result, "HA recovery write has no result yet; keeping the node fenced");
            if let Some(u) = unknown.get_mut(&node) {
                u.term = ctx.writer.term(ctx.cluster);
            }
            continue;
        }
        let Some(u) = unknown.remove(&node) else {
            continue;
        };
        if let Some(target) = u.moved_to {
            let vm = ctx.cluster.get::<VmSpec>(u.vm);
            let failed = ctx.cluster.get::<NodeSpec>(node);
            if let (Ok(Some(vm)), Ok(Some(failed))) = (vm, failed)
                && vm.spec.node == Some(target)
            {
                finish(ctx, &vm, &failed, target).await;
            }
        }
        ctx.cluster.end_fence(NodeId(node));
    }
}

/// 쓰기의 결과를 안다(펜싱을 풀어도 된다): 적용됐거나(`Ok`), 상태 머신이 거부했거나(`Rejected`), 리더를
/// 잃었다(`NotLeader`). `NotLeader`여도 항목은 이미 로그에 들어갔을 수 있다. 하지만 리더를 잃었다는 것은
/// 임기가 바뀐다는 뜻이고, 다음 임기의 리더는 자기 임기의 첫 항목을 커밋한 뒤에야 — 곧 그 항목을 커밋하거나
/// 버린 뒤에야 — 갱신을 확인해 주며 그 인덱스를 돌려준다. 죽은 노드는 그것을 적용한 뒤에야 fresh가 되므로
/// 그 결과를 본 뒤에 VM을 켠다. 그 밖(`Timeout` 등)은 같은 임기의 리더 로그에 남아 이 리더가 나중에 커밋할 수
/// 있으므로, 그 사이 이 리더가 확인해 준 갱신이 그 항목을 포함하지 않을 수 있다.
fn settled(result: &Result<u64, ClusterError>) -> bool {
    matches!(
        result,
        Ok(_) | Err(ClusterError::Rejected(_) | ClusterError::NotLeader { .. })
    )
}

/// 노드 `node`를 펜싱 중으로 표시하고, 리스가 여전히 끝났으면 그 HA VM을 이름 순서로 옮긴다.
/// 결과를 모르는 쓰기가 생기면 펜싱 표시를 지우지 않고 그 쓰기를 돌려준다.
async fn recover_node(ctx: Ctx<'_>, term: u64, b: Instant, node: Uuid) -> Option<Unknown> {
    let cluster = ctx.cluster;
    let mut fence = Fence::begin(cluster, node);
    // 표시하기 직전에 받은 갱신이 있을 수 있으므로 표시한 뒤 다시 본다.
    let leases = cluster.leases();
    if leases.term != term {
        return None;
    }
    let Ok(mut inv) = Inventory::load(cluster) else {
        return None;
    };
    let failed = inv.nodes.iter().find(|n| n.id == node).cloned()?;
    if !expired(Instant::now(), b, &leases, &failed) {
        return None;
    }
    let mut vms: Vec<Vm> = inv
        .vms
        .iter()
        .filter(|vm| recoverable(vm, node))
        .cloned()
        .collect();
    vms.sort_by(|a, b| a.name.cmp(&b.name));
    let names: Vec<&str> = vms.iter().map(|vm| vm.name.as_str()).collect();
    tracing::warn!(node = %failed.name, vms = ?names, "recovering HA VMs from a failed node");
    for vm in &vms {
        let result = recover_vm(ctx, &mut fence, &mut inv, &leases, b, &failed, vm).await;
        if let Some((command, moved_to)) = fence.unknown.take() {
            tracing::warn!(vm = %vm.name, result = ?result, "HA recovery write has no result; keeping the node fenced");
            return Some(Unknown {
                term: ctx.writer.term(cluster),
                vm: vm.id,
                command,
                moved_to,
            });
        }
        match result {
            Ok(()) => {}
            // 그사이 바뀌었으면 다음 tick에 다시 본다.
            Err(ClusterError::Rejected(e)) => {
                tracing::info!(vm = %vm.name, error = %e, "HA recovery not applied; will retry");
            }
            // 리더를 잃었거나 쓰지 못한다. 나머지도 다음 tick(이나 새 리더)에 다시 한다.
            Err(e) => {
                tracing::warn!(vm = %vm.name, error = %e, "cannot recover HA VM; will retry");
                return None;
            }
        }
    }
    None
}

/// VM 하나를 옮긴다: status를 먼저 쓰고 spec의 노드를 바꾼 뒤, 죽은 노드의 그 VM Task를 실패로
/// 끝내고 기록 Task를 남긴다. 대상이 없으면 그 이유만 status에 남긴다.
async fn recover_vm(
    ctx: Ctx<'_>,
    fence: &mut Fence<'_>,
    inv: &mut Inventory,
    leases: &Leases,
    b: Instant,
    failed: &Node,
    vm: &Vm,
) -> Result<(), ClusterError> {
    let migrating = vm.status.migration.map(|m| m.node);
    let now = Instant::now();
    // 스케줄러처럼 미할당 메모리가 가장 많은 노드, 같으면 id가 작은 노드다. 죽은 노드와 마이그레이션을
    // 받던 노드(같은 실행 디렉터리를 쓰는 QEMU가 남아 있을 수 있다)는 빼고, 리스가 살아 있어야 한다.
    let target = inv
        .nodes
        .iter()
        .filter(|n| n.id != failed.id && Some(n.id) != migrating)
        .filter(|n| alive(now, b, leases, n) && node_fits(&vm.spec, inv, n).is_ok())
        .map(|n| (inv.free_memory_mib(n, vm.id), n.id))
        .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
        .map(|(_, id)| id);
    let Some(target) = target else {
        if vm.status.last_error.as_deref() != Some(NO_NODE) {
            let status = VmStatus {
                last_error: Some(NO_NODE.into()),
                ..vm.status.clone()
            };
            update_status_checked::<VmSpec>(ctx, vm.id, vm.revision, &status).await?;
            tracing::warn!(vm = %vm.name, "HA cannot find a node for the VM");
        }
        return Ok(());
    };
    // 노드를 바꾸기 전에 실행 상태를 지운다. 사이에 리더가 바뀌면 새 리더가 다시 한다(아직 그 노드).
    let status = VmStatus {
        phase: VmPhase::Stopped,
        node: None,
        pid: None,
        migration: None,
        last_error: Some(format!("recovering: node {} failed", failed.name)),
        ..vm.status.clone()
    };
    let command = status_command::<VmSpec>(vm.id, vm.revision, &status)?;
    let revision = fence.write(ctx, command, None).await?;
    let mut spec = vm.spec.clone();
    spec.node = Some(target);
    let command = Command::UpdateSpec {
        kind: VmSpec::KIND,
        id: vm.id,
        expected_revision: Some(revision),
        labels: vm.labels.clone(),
        spec: serde_json::to_value(&spec)?,
    };
    fence.write(ctx, command, Some(target)).await?;
    // 다음 VM의 미할당 메모리에 센다.
    if let Some(moved) = inv.vms.iter_mut().find(|v| v.id == vm.id) {
        moved.spec.node = Some(target);
    }
    finish(ctx, vm, failed, target).await;
    Ok(())
}

/// 옮긴 VM의 남은 일: 죽은 노드의 그 VM Task를 실패로 끝내고 기록 Task를 남긴다.
async fn finish(ctx: Ctx<'_>, vm: &Vm, failed: &Node, target: Uuid) {
    // 그 노드의 끝나지 않은 Task가 남으면 VM 조작이 막힌다(`vm_busy`). spec을 쓴 뒤에 읽어야 그
    // 사이에 만든 Task도 본다.
    let error = format!("node {} failed; the VM was recovered by HA", failed.name);
    match ctx.cluster.list::<TaskSpec>() {
        Ok((_, tasks)) => {
            for task in tasks.iter().filter(|t| {
                t.spec.node == failed.id && t.spec.target == vm.id && !t.status.state.is_finished()
            }) {
                fail_task(ctx, task, &error).await;
            }
        }
        Err(e) => tracing::warn!(vm = %vm.name, error = %e, "cannot list tasks of a failed node"),
    }
    let mut record: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmHaRecover { from: failed.id },
            target: vm.id,
            node: target,
        },
    );
    record.name = record.id.to_string();
    // 실행기는 끝난 Task를 실행하지 않는다.
    record.status.state = TaskState::Succeeded;
    if let Err(e) = create(ctx, &record).await {
        tracing::warn!(vm = %vm.name, error = %e, "cannot record HA recovery task");
    }
}

/// Task를 실패로 끝낸다. 그사이 바뀌었으면 다시 읽어 끝나지 않았을 때만 다시 쓴다.
async fn fail_task(ctx: Ctx<'_>, task: &Task, error: &str) {
    let status = TaskStatus {
        state: TaskState::Failed,
        error: Some(error.into()),
    };
    let mut revision = task.revision;
    for _ in 0..TASK_FAIL_ATTEMPTS {
        match update_status_checked::<TaskSpec>(ctx, task.id, revision, &status).await {
            Ok(_) => return,
            Err(e) => {
                tracing::info!(task = %task.id, error = %e, "cannot fail task of a failed node");
                match ctx.cluster.get::<TaskSpec>(task.id) {
                    Ok(Some(t)) if !t.status.state.is_finished() => revision = t.revision,
                    _ => return,
                }
            }
        }
    }
}

/// 노드 `node`에 배정된 HA VM으로 실행하기로 된 것
fn recoverable(vm: &Vm, node: Uuid) -> bool {
    vm.spec.node == Some(node) && vm.spec.ha && vm.spec.power == PowerState::Running
}

/// 노드의 마지막 리스 갱신(없으면 기준점 b) 뒤 지난 시간
fn lease_age(now: Instant, b: Instant, leases: &Leases, node: Uuid) -> Duration {
    let last = leases
        .renewed
        .get(&NodeId(node))
        .map_or(b, |renewed| (*renewed).max(b));
    now.saturating_duration_since(last)
}

/// HA 가능 노드이고 리스가 `recover_after`보다 오래 끝났다.
fn expired(now: Instant, b: Instant, leases: &Leases, node: &Node) -> bool {
    node.status.ha.is_some_and(|ha| {
        lease_age(now, b, leases, node.id) >= Duration::from_secs(ha.recover_after_secs)
    })
}

/// HA 가능 노드이고 리스가 살아 있다.
fn alive(now: Instant, b: Instant, leases: &Leases, node: &Node) -> bool {
    node.status.ha.is_some_and(|ha| {
        lease_age(now, b, leases, node.id) < Duration::from_secs(ha.recover_after_secs)
    })
}

/// 펜싱 중 표시. 버려질 때(오류, 리더를 잃음, 종료로 멈춤 포함) 지운다. 표시는 리스 표에 있어 임기가
/// 바뀌어도 남으므로 꼭 지워야 한다. 단, 복구 쓰기의 결과를 모르면(쓰는 중 멈춤, `Timeout` 등) 지우지
/// 않는다. 그 쓰기가 나중에 커밋될 수 있기 때문이다.
struct Fence<'a> {
    cluster: &'a Cluster,
    node: NodeId,
    /// 결과를 모르는 쓰기가 있다(쓰는 중이거나 결과를 모름).
    hold: bool,
    /// 결과를 모르는 쓰기와 그것이 spec 쓰기였으면 옮긴 노드
    unknown: Option<(Command, Option<Uuid>)>,
}

impl<'a> Fence<'a> {
    fn begin(cluster: &'a Cluster, node: Uuid) -> Self {
        let node = NodeId(node);
        cluster.begin_fence(node);
        Self {
            cluster,
            node,
            hold: false,
            unknown: None,
        }
    }

    /// 복구 쓰기. 결과를 알 때까지 표시를 쥔다.
    async fn write(
        &mut self,
        ctx: Ctx<'_>,
        command: Command,
        moved_to: Option<Uuid>,
    ) -> Result<u64, ClusterError> {
        self.hold = true;
        let result = ctx.writer.write(ctx.cluster, command.clone()).await;
        if settled(&result) {
            self.hold = false;
        } else {
            self.unknown = Some((command, moved_to));
        }
        result
    }
}

impl Drop for Fence<'_> {
    fn drop(&mut self) {
        if !self.hold {
            self.cluster.end_fence(self.node);
        }
    }
}

/// 복구 쓰기를 적용한다. 시험에서 결과를 모르는 쓰기(`Timeout`)와 임기 변경을 흉내 내려고 바꿔 끼운다.
#[async_trait]
pub trait RecoveryWriter: Send + Sync {
    async fn write(&self, cluster: &Cluster, command: Command) -> Result<u64, ClusterError>;

    /// 결과를 모르는 쓰기를 판정하는 임기(이 임기가 바뀌면 그 쓰기를 정리한다)
    fn term(&self, cluster: &Cluster) -> u64 {
        cluster.leases().term
    }
}

/// 복구 쓰기는 이 노드가 리더일 때만 한다. 다른 노드로 전달하지 않으므로 리더를 잃었으면 `NotLeader`로
/// 끝난다. 펜싱 표시는 이 노드의 리스 표에만 있어, 새 리더에 전달된 쓰기는 그 노드의 갱신(새 리더는
/// 거부하지 않는다)과 엇갈릴 수 있다.
struct LeaderWriter;

#[async_trait]
impl RecoveryWriter for LeaderWriter {
    async fn write(&self, cluster: &Cluster, command: Command) -> Result<u64, ClusterError> {
        cluster
            .execute_as_leader(LeaderRequest::Write { command })
            .await
    }
}

/// 복구에 쓰는 것: 클러스터와 쓰기 경로
#[derive(Clone, Copy)]
struct Ctx<'a> {
    cluster: &'a Cluster,
    writer: &'a dyn RecoveryWriter,
}

async fn write(ctx: Ctx<'_>, command: Command) -> Result<u64, ClusterError> {
    ctx.writer.write(ctx.cluster, command).await
}

fn status_command<S: ResourceSpec>(
    id: Uuid,
    expected_revision: u64,
    status: &S::Status,
) -> Result<Command, ClusterError> {
    Ok(Command::UpdateStatusIf {
        kind: S::KIND,
        id,
        expected_revision,
        status: serde_json::to_value(status)?,
    })
}

async fn update_status_checked<S: ResourceSpec>(
    ctx: Ctx<'_>,
    id: Uuid,
    expected_revision: u64,
    status: &S::Status,
) -> Result<u64, ClusterError> {
    write(ctx, status_command::<S>(id, expected_revision, status)?).await
}

async fn create<S: ResourceSpec>(ctx: Ctx<'_>, resource: &Typed<S>) -> Result<u64, ClusterError> {
    let command = Command::Create {
        kind: S::KIND,
        resource: serde_json::from_value(serde_json::to_value(resource)?)?,
    };
    write(ctx, command).await
}

#[cfg(test)]
mod tests {
    use opl_cluster::types::Rejection;

    use super::*;

    fn unknown_at(term: u64) -> Unknown {
        Unknown {
            term,
            vm: Uuid::now_v7(),
            command: Command::Delete {
                kind: VmSpec::KIND,
                id: Uuid::now_v7(),
                expected_revision: None,
            },
            moved_to: None,
        }
    }

    #[test]
    fn the_base_is_taken_again_when_the_term_changes() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(5);
        let mut base: Base = None;
        assert_eq!(base_at(&mut base, 3, t0), t0);
        // 같은 임기에서는 처음 본 시각 그대로다.
        assert_eq!(base_at(&mut base, 3, later), t0);
        // 임기가 바뀌면 다시 잡는다.
        let next = later + Duration::from_secs(5);
        assert_eq!(base_at(&mut base, 4, next), next);
        assert_eq!(base, Some((4, next)));
    }

    #[test]
    fn unknown_writes_of_another_term_are_settled() {
        let (old, same) = (Uuid::now_v7(), Uuid::now_v7());
        let mut unknown = Unknowns::from([(old, unknown_at(3)), (same, unknown_at(4))]);
        let mut moved = vec![];
        assert_eq!(settled_by_term(&mut unknown, 4, &mut moved), vec![old]);
        assert_eq!(unknown.keys().copied().collect::<Vec<_>>(), vec![same]);
        assert!(settled_by_term(&mut unknown, 4, &mut moved).is_empty());
        // 이 노드가 리더가 아니어도 임기가 바뀌면 푼다.
        assert_eq!(settled_by_term(&mut unknown, 5, &mut moved), vec![same]);
        assert!(unknown.is_empty());
        // status 쓰기였으면 남은 단계가 없다.
        assert!(moved.is_empty());
    }

    #[test]
    fn a_spec_write_settled_by_term_keeps_its_remaining_steps() {
        let (node, to) = (Uuid::now_v7(), Uuid::now_v7());
        let u = Unknown {
            moved_to: Some(to),
            ..unknown_at(3)
        };
        let vm = u.vm;
        let mut unknown = Unknowns::from([(node, u)]);
        let mut moved = vec![];
        assert_eq!(settled_by_term(&mut unknown, 4, &mut moved), vec![node]);
        assert!(unknown.is_empty());
        assert_eq!(moved, vec![Moved { vm, from: node, to }]);
    }

    #[test]
    fn only_ok_rejected_and_not_leader_are_settled() {
        assert!(settled(&Ok(1)));
        assert!(settled(&Err(ClusterError::Rejected(
            Rejection::RevisionMismatch {
                expected: 1,
                actual: 2
            }
        ))));
        assert!(settled(&Err(ClusterError::NotLeader {
            leader_id: None,
            leader_addr: None
        })));
        assert!(!settled(&Err(ClusterError::Timeout)));
        assert!(!settled(&Err(ClusterError::Raft("shutting down".into()))));
        assert!(!settled(&Err(ClusterError::Unreachable("x".into()))));
    }
}
```

`crates/opl-api/src/lib.rs`에서

```rust
mod firmware;
mod incoming;
```

을 다음으로 바꾼다:

```rust
mod firmware;
mod ha;
mod incoming;
```

`crates/opl-api/src/lib.rs`에서

```rust
pub use firmware::UefiVarsFetcher;
pub use incoming::{IncomingConfig, IncomingMigrations};
```

을 다음으로 바꾼다:

```rust
pub use firmware::UefiVarsFetcher;
pub use ha::{HaManager, HaManagerConfig, RecoveryWriter};
pub use incoming::{IncomingConfig, IncomingMigrations};
```

`crates/opl-api/src/tasks.rs`에서

```rust
        TaskOp::VmMigrate { node } => migrate::migrate(ctx, task, *node).await,
    }
```

을 다음으로 바꾼다:

```rust
        TaskOp::VmMigrate { node } => migrate::migrate(ctx, task, *node).await,
        // 기록 Task는 끝난 상태로 만들어지므로 실행되지 않는다.
        TaskOp::VmHaRecover { .. } => Err("an HA recovery task is a record and is not run".into()),
    }
```

`crates/opl-api/tests/ha.rs`를 만든다:

```rust
//! 리더의 HA 복구(`HaManager`): 단일 노드 클러스터의 저장소에 다른 노드를 직접 기록한다(멤버십에는
//! 없고 리스를 갱신하지 않는다). 리스가 끝난 노드의 HA VM만 이름 순서로 옮기는지, 대상 고르기,
//! status를 spec보다 먼저 쓰는지, 그 노드의 Task를 실패로 끝내고 기록 Task를 남기는지, 대상이 없을
//! 때, 리스가 살아 있는 노드는 옮기지 않는지, 펜싱 표시가 늘 지워지는지, Task 목록을 spec 쓰기 뒤에
//! 읽는지, 결과를 모르는 복구 쓰기 뒤에는 결과를 알 때까지 펜싱을 쥐는지(임기가 바뀌면 풀고 남은 단계를
//! 하는지), 쓰기 오류 뒤에는 펜싱을 푸는지.

mod common;

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, WAIT, vm_request};
use opl_api::{HaManager, HaManagerConfig, RecoveryWriter};
use opl_cluster::types::{Command, LeaderRequest, NodeId, Rejection};
use opl_cluster::watch::WatchEvent;
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::{Fencing, NodeHa, NodeSpec, NodeStatus};
use opl_types::resource::{Kind, Resource};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{Arch, MigrationStatus, PowerState, Vm, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;

fn fast_manager(env: &Env) -> HaManager {
    HaManager::spawn(
        env.state.clone(),
        HaManagerConfig {
            tick: Duration::from_millis(100),
        },
    )
}

fn manager_with(env: &Env, writer: Arc<dyn RecoveryWriter>) -> HaManager {
    HaManager::spawn_with_writer(
        env.state.clone(),
        HaManagerConfig {
            tick: Duration::from_millis(100),
        },
        writer,
    )
}

/// 리더로서 쓴다(`HaManager`의 쓰기 경로).
async fn leader_write(cluster: &Cluster, command: Command) -> Result<u64, ClusterError> {
    cluster
        .execute_as_leader(LeaderRequest::Write { command })
        .await
}

/// 노드 `name`을 저장소에 직접 만든다. `recover_after`가 있으면 HA 가능 노드다.
async fn node(env: &Env, name: &str, memory_mib: u64, recover_after: Option<u64>) -> Uuid {
    node_with(env, name, memory_mib, recover_after, |_| {}).await
}

async fn node_with(
    env: &Env,
    name: &str,
    memory_mib: u64,
    recover_after: Option<u64>,
    tweak: impl FnOnce(&mut Resource<NodeSpec, NodeStatus>),
) -> Uuid {
    let mut node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
        },
    );
    node.status = NodeStatus {
        online: true,
        cpus: 4,
        memory_mib,
        hypervisor: "mock".into(),
        bridges: vec![],
        arch: Some(Arch::Aarch64),
        pools: Default::default(),
        ha: recover_after.map(|secs| NodeHa {
            fencing: Fencing::Process,
            recover_after_secs: secs,
        }),
    };
    tweak(&mut node);
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 노드 `node`에 배정된 VM을 직접 만든다. 실행 중이면 status도 그 노드에서 도는 것으로 둔다.
async fn vm(env: &Env, name: &str, node: Uuid, ha: bool, power: PowerState) -> Vm {
    let mut spec: VmSpec = serde_json::from_value(vm_request(name)["spec"].clone()).unwrap();
    spec.memory_mib = 2048;
    spec.nics.clear();
    spec.node = Some(node);
    spec.ha = ha;
    spec.power = power;
    let mut vm: Vm = Resource::new(name, spec);
    if power == PowerState::Running {
        vm.status = VmStatus {
            phase: VmPhase::Running,
            node: Some(node),
            pid: Some(4242),
            firmware_node: Some(node),
            ..Default::default()
        };
    }
    vm.revision = env.cluster.create(&vm).await.unwrap();
    vm
}

fn get(env: &Env, id: Uuid) -> Vm {
    env.cluster.get::<VmSpec>(id).unwrap().unwrap()
}

/// 이 노드의 이름으로 리더에게 리스를 갱신한다.
async fn renew(cluster: &Cluster, node: Uuid) -> Result<u64, ClusterError> {
    cluster
        .execute_as_leader(LeaderRequest::RenewLease { node: NodeId(node) })
        .await
}

/// 노드 `node`에서 VM `target`을 켜는 끝나지 않은 Task를 만든다.
async fn pending_task(cluster: &Cluster, target: Uuid, node: Uuid) -> Uuid {
    let mut t: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmStart,
            target,
            node,
        },
    );
    t.name = t.id.to_string();
    cluster.create(&t).await.unwrap();
    t.id
}

/// 기록 Task들(대상 VM별)
fn recover_tasks(env: &Env) -> Vec<Task> {
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    tasks
        .into_iter()
        .filter(|t| matches!(t.spec.op, TaskOp::VmHaRecover { .. }))
        .collect()
}

async fn eventually(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + WAIT;
    while !ok() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn recovers_ha_vms_of_a_failed_node_in_name_order() {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    // 대상 후보: 메모리가 많은 쪽부터 고른다. 옮긴 VM의 메모리를 세어 두 번째 VM은 다른 노드로 간다.
    let small = node(&env, "small", 4096, Some(60)).await;
    let big = node(&env, "big", 5000, Some(60)).await;
    // 후보가 아닌 노드: HA 불가, 리스가 끝남, 유지보수, 위트니스, 마이그레이션 대상
    node(&env, "no-ha", 64000, None).await;
    node(&env, "expired", 64000, Some(1)).await;
    node_with(&env, "maint", 64000, Some(60), |n| {
        n.spec.maintenance = true
    })
    .await;
    node_with(&env, "wit", 64000, Some(60), |n| n.spec.witness = true).await;
    let receiving = node(&env, "receiving", 4500, Some(60)).await;

    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let b = vm(&env, "b-vm", failed, true, PowerState::Running).await;
    let off = vm(&env, "off", failed, true, PowerState::Stopped).await;
    let plain = vm(&env, "plain", failed, false, PowerState::Running).await;
    // b-vm은 receiving으로 마이그레이션하던 중이었다.
    let mut status = b.status.clone();
    status.migration = Some(MigrationStatus {
        node: receiving,
        task: Uuid::now_v7(),
    });
    env.cluster
        .update_status::<VmSpec>(b.id, &status)
        .await
        .unwrap();
    // 죽은 노드의 끝나지 않은 Task(대상 VM의 것만 실패로 끝낸다)와 다른 노드의 Task
    let mut tasks = vec![];
    for (target, on, state) in [
        (a.id, failed, TaskState::Pending),
        (b.id, failed, TaskState::Running),
        (plain.id, failed, TaskState::Pending),
        (a.id, small, TaskState::Pending),
    ] {
        let mut t: Task = Resource::new(
            "",
            TaskSpec {
                op: TaskOp::VmStart,
                target,
                node: on,
            },
        );
        t.name = t.id.to_string();
        t.status.state = state;
        env.cluster.create(&t).await.unwrap();
        tasks.push(t.id);
    }

    let (since, _) = env.cluster.list::<VmSpec>().unwrap();
    let mut watch = env.cluster.watch(since).unwrap();
    let manager = fast_manager(&env);
    eventually("HA VMs moved", || {
        get(&env, a.id).spec.node != Some(failed) && get(&env, b.id).spec.node != Some(failed)
    })
    .await;
    eventually("record tasks", || recover_tasks(&env).len() == 2).await;

    // 이름 순서: a-vm이 먼저 메모리가 많은 big으로, 그 뒤 b-vm은 남은 메모리가 많은 small로 간다
    // (receiving은 b-vm의 마이그레이션 대상이라 빠진다).
    let (a_now, b_now) = (get(&env, a.id), get(&env, b.id));
    assert_eq!(a_now.spec.node, Some(big));
    assert_eq!(b_now.spec.node, Some(small));
    for vm in [&a_now, &b_now] {
        assert_eq!(vm.spec.power, PowerState::Running);
        assert_eq!(vm.status.phase, VmPhase::Stopped);
        assert_eq!(vm.status.node, None);
        assert_eq!(vm.status.pid, None);
        assert_eq!(vm.status.migration, None);
        assert_eq!(vm.status.firmware_node, Some(failed));
        assert_eq!(
            vm.status.last_error.as_deref(),
            Some("recovering: node failed failed")
        );
    }
    // HA가 아니거나 꺼진 VM은 그대로다.
    assert_eq!(get(&env, off.id).spec.node, Some(failed));
    assert_eq!(get(&env, plain.id), plain);

    // 죽은 노드의 대상 VM Task만 실패로 끝난다.
    let task = |id: Uuid| env.cluster.get::<TaskSpec>(id).unwrap().unwrap();
    for id in &tasks[..2] {
        assert_eq!(
            task(*id).status,
            TaskStatus {
                state: TaskState::Failed,
                error: Some("node failed failed; the VM was recovered by HA".into()),
            }
        );
    }
    assert_eq!(task(tasks[2]).status.state, TaskState::Pending);
    assert_eq!(task(tasks[3]).status.state, TaskState::Pending);

    // 기록 Task: 대상 VM, 받은 노드, 처음부터 Succeeded
    let mut records: Vec<(Uuid, Uuid, TaskOp, TaskStatus)> = recover_tasks(&env)
        .into_iter()
        .map(|t| (t.spec.target, t.spec.node, t.spec.op, t.status))
        .collect();
    records.sort_by_key(|r| (r.0 != a.id, r.0));
    let done = TaskStatus {
        state: TaskState::Succeeded,
        error: None,
    };
    let op = TaskOp::VmHaRecover { from: failed };
    assert_eq!(
        records,
        vec![
            (a.id, big, op.clone(), done.clone()),
            (b.id, small, op, done)
        ]
    );

    // VM마다 status(아직 죽은 노드에 배정)를 쓴 뒤에 spec을 바꾼다.
    for id in [a.id, b.id] {
        let mut writes = vec![];
        while writes.len() < 2 {
            let event = tokio::time::timeout(WAIT, watch.next())
                .await
                .unwrap()
                .unwrap();
            if let WatchEvent::Put {
                kind: Kind::Vm,
                resource,
            } = event
                && resource.id == id
            {
                writes.push(resource);
            }
        }
        assert_eq!(writes[0].spec["node"], failed.to_string());
        assert_eq!(writes[0].status["phase"], "stopped");
        assert_ne!(writes[1].spec["node"], failed.to_string());
    }

    // 펜싱 표시가 지워져 죽은 노드가 다시 리스를 받을 수 있다.
    renew(&env.cluster, failed).await.unwrap();
    manager.shutdown().await;
}

#[tokio::test]
async fn leaves_vms_of_a_node_whose_lease_is_alive() {
    let env = Env::start().await;
    let alive = node(&env, "alive", 8192, Some(1)).await;
    node(&env, "spare", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", alive, true, PowerState::Running).await;
    // 갱신 결과는 채널로 받아 단언한다(뒤에서 조용히 죽지 않게).
    let (results, mut renewed) = tokio::sync::mpsc::unbounded_channel();
    let renewer = {
        let cluster = env.cluster.clone();
        tokio::spawn(async move {
            loop {
                if results.send(renew(&cluster, alive).await).is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    };
    let manager = fast_manager(&env);
    // recover_after(1초)를 여러 번 넘기도록 기다린다.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(get(&env, a.id), a);
    assert!(recover_tasks(&env).is_empty());
    manager.shutdown().await;
    renewer.abort();
    let mut count = 0;
    while let Ok(result) = renewed.try_recv() {
        result.unwrap();
        count += 1;
    }
    assert!(count >= 10, "renewed {count} times");
}

#[tokio::test]
async fn records_the_error_once_when_no_node_can_take_the_vm() {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    // 유일한 다른 HA 노드도 리스가 끝났다.
    node(&env, "expired", 8192, Some(1)).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let manager = fast_manager(&env);
    eventually("error recorded", || {
        get(&env, a.id).status.last_error.as_deref() == Some("HA cannot find a node for this VM")
    })
    .await;
    let recorded = get(&env, a.id);
    assert_eq!(recorded.spec, a.spec);
    assert_eq!(
        recorded.status,
        VmStatus {
            last_error: Some("HA cannot find a node for this VM".into()),
            ..a.status.clone()
        }
    );
    // 같은 오류는 다시 쓰지 않는다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(get(&env, a.id).revision, recorded.revision);
    assert!(recover_tasks(&env).is_empty());
    // 대상이 없어도 펜싱 표시는 지운다.
    renew(&env.cluster, failed).await.unwrap();
    manager.shutdown().await;
}

#[tokio::test]
async fn a_node_is_not_fenced_before_recover_after_from_becoming_leader() {
    // 리더가 된 뒤(기준점)부터 recover_after가 지나야 펜싱한다. 그 전에는 갱신이 거부되지 않는다.
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(3)).await;
    node(&env, "spare", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let manager = fast_manager(&env);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(get(&env, a.id), a);
    eventually("HA VM moved", || get(&env, a.id).spec.node != Some(failed)).await;
    manager.shutdown().await;
}

#[tokio::test]
async fn the_task_runner_does_not_run_a_record_task() {
    let env = Env::start().await;
    let a = vm(&env, "a-vm", env.node, true, PowerState::Stopped).await;
    let mut t: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmHaRecover {
                from: Uuid::now_v7(),
            },
            target: a.id,
            node: env.node,
        },
    );
    t.name = t.id.to_string();
    t.status.state = TaskState::Succeeded;
    let revision = env.cluster.create(&t).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let now = env.cluster.get::<TaskSpec>(t.id).unwrap().unwrap();
    assert_eq!(now.revision, revision);
    assert_eq!(now.status.state, TaskState::Succeeded);
}

/// spec 쓰기 바로 앞에 죽은 노드의 그 VM Task를 하나 만든다.
struct TaskBeforeSpec {
    failed: Uuid,
    created: OnceLock<Uuid>,
}

#[async_trait]
impl RecoveryWriter for TaskBeforeSpec {
    async fn write(&self, cluster: &Cluster, command: Command) -> Result<u64, ClusterError> {
        if let Command::UpdateSpec { id, .. } = &command
            && self.created.get().is_none()
        {
            let task = pending_task(cluster, *id, self.failed).await;
            self.created.set(task).unwrap();
        }
        leader_write(cluster, command).await
    }
}

#[tokio::test]
async fn fails_a_task_of_the_failed_node_created_before_the_spec_write() {
    // Task 목록은 spec 쓰기 뒤에 읽는다. 그 전에 읽으면 그 사이에 만든 Task가 남아 VM 조작이 막힌다.
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    node(&env, "spare", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let writer = Arc::new(TaskBeforeSpec {
        failed,
        created: OnceLock::new(),
    });
    let manager = manager_with(&env, writer.clone());
    eventually("record task", || recover_tasks(&env).len() == 1).await;
    assert_ne!(get(&env, a.id).spec.node, Some(failed));
    let task = *writer.created.get().unwrap();
    assert_eq!(
        env.cluster.get::<TaskSpec>(task).unwrap().unwrap().status,
        TaskStatus {
            state: TaskState::Failed,
            error: Some("node failed failed; the VM was recovered by HA".into()),
        }
    );
    manager.shutdown().await;
}

/// `pick`이 처음 고른 쓰기의 결과를 모르게 한다(`applied`면 적용한 뒤 `Timeout`, 아니면 적용하지 않고
/// `Timeout`). 그 뒤의 쓰기는 `release`까지 적용하지 않고 `Timeout`이다.
struct Unsure {
    pick: fn(&Command) -> bool,
    applied: bool,
    hit: AtomicBool,
    resent: AtomicUsize,
    release: AtomicBool,
}

#[async_trait]
impl RecoveryWriter for Unsure {
    async fn write(&self, cluster: &Cluster, command: Command) -> Result<u64, ClusterError> {
        if self.release.load(Ordering::SeqCst) {
            return leader_write(cluster, command).await;
        }
        if self.hit.load(Ordering::SeqCst) {
            self.resent.fetch_add(1, Ordering::SeqCst);
            return Err(ClusterError::Timeout);
        }
        if (self.pick)(&command) {
            if self.applied {
                leader_write(cluster, command).await?;
            }
            self.hit.store(true, Ordering::SeqCst);
            return Err(ClusterError::Timeout);
        }
        leader_write(cluster, command).await
    }
}

fn is_status_write(command: &Command) -> bool {
    matches!(command, Command::UpdateStatusIf { kind: Kind::Vm, .. })
}

fn is_spec_write(command: &Command) -> bool {
    matches!(command, Command::UpdateSpec { kind: Kind::Vm, .. })
}

fn is_fenced(result: &Result<u64, ClusterError>) -> bool {
    matches!(
        result,
        Err(ClusterError::Rejected(Rejection::Fenced { .. }))
    )
}

/// 결과가 나올 때까지 갱신을 다시 시도한다(펜싱이 풀리기를 기다린다).
async fn renew_eventually(cluster: &Cluster, node: Uuid) {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        match renew(cluster, node).await {
            Ok(_) => return,
            Err(e) => assert!(std::time::Instant::now() < deadline, "renew: {e}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 고른 복구 쓰기가 `Timeout`으로 끝나면 결과를 알 때까지 죽은 노드의 갱신을 거부하고, 결과를 안 뒤
/// 남은 단계를 이어 한다.
async fn holds_the_fence_until_an_unknown_write_settles(
    pick: fn(&Command) -> bool,
    applied: bool,
    moved_meanwhile: bool,
) {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    let spare = node(&env, "spare", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let task = pending_task(&env.cluster, a.id, failed).await;
    let writer = Arc::new(Unsure {
        pick,
        applied,
        hit: AtomicBool::new(false),
        resent: AtomicUsize::new(0),
        release: AtomicBool::new(false),
    });
    let manager = manager_with(&env, writer.clone());
    eventually("write resent", || writer.resent.load(Ordering::SeqCst) >= 2).await;
    // 결과를 모르는 동안(같은 임기) 펜싱 표시를 지우지 않는다.
    for _ in 0..5 {
        let result = renew(&env.cluster, failed).await;
        assert!(is_fenced(&result), "renew while unknown: {result:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let expected = if moved_meanwhile { spare } else { failed };
    assert_eq!(get(&env, a.id).spec.node, Some(expected));
    assert!(recover_tasks(&env).is_empty());

    // 다시 보낸 쓰기가 결과를 내면 남은 단계를 하고 펜싱을 푼다.
    writer.release.store(true, Ordering::SeqCst);
    eventually("record task", || recover_tasks(&env).len() == 1).await;
    renew_eventually(&env.cluster, failed).await;
    let moved = get(&env, a.id);
    assert_eq!(moved.spec.node, Some(spare));
    assert_eq!(moved.status.phase, VmPhase::Stopped);
    assert_eq!(
        moved.status.last_error.as_deref(),
        Some("recovering: node failed failed")
    );
    assert_eq!(
        env.cluster
            .get::<TaskSpec>(task)
            .unwrap()
            .unwrap()
            .status
            .state,
        TaskState::Failed
    );
    let records = recover_tasks(&env);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].spec.node, spare);
    manager.shutdown().await;
}

#[tokio::test]
async fn holds_the_fence_after_an_applied_status_write_times_out() {
    holds_the_fence_until_an_unknown_write_settles(is_status_write, true, false).await;
}

#[tokio::test]
async fn holds_the_fence_after_a_dropped_status_write_times_out() {
    holds_the_fence_until_an_unknown_write_settles(is_status_write, false, false).await;
}

#[tokio::test]
async fn holds_the_fence_after_an_applied_spec_write_times_out() {
    holds_the_fence_until_an_unknown_write_settles(is_spec_write, true, true).await;
}

#[tokio::test]
async fn holds_the_fence_after_a_dropped_spec_write_times_out() {
    // 적용되지 않은 spec 쓰기: 다시 보낸 쓰기가 Ok로 적용된 뒤 남은 단계를 한다.
    holds_the_fence_until_an_unknown_write_settles(is_spec_write, false, false).await;
}

/// 처음 spec 쓰기는 적용한 뒤 `Timeout`, 그 뒤의 spec 쓰기는 적용하지 않고 `Timeout`이다. 다른 쓰기는
/// 그대로 한다. 결과를 모르는 쓰기를 판정하는 임기는 실제 임기에 `bump`를 더한 것이다(단일 노드에서는
/// 임기를 바꿀 수 없어 임기 변경을 흉내 낸다).
struct SpecUnknownThenNewTerm {
    hit: AtomicBool,
    resent: AtomicUsize,
    bump: AtomicU64,
}

#[async_trait]
impl RecoveryWriter for SpecUnknownThenNewTerm {
    async fn write(&self, cluster: &Cluster, command: Command) -> Result<u64, ClusterError> {
        if !is_spec_write(&command) {
            return leader_write(cluster, command).await;
        }
        if self.hit.swap(true, Ordering::SeqCst) {
            self.resent.fetch_add(1, Ordering::SeqCst);
        } else {
            leader_write(cluster, command).await?;
        }
        Err(ClusterError::Timeout)
    }

    fn term(&self, cluster: &Cluster) -> u64 {
        cluster.leases().term + self.bump.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn finishes_a_spec_write_settled_by_a_new_term() {
    // 결과를 모르는 spec 쓰기가 임기 변경으로 정리되면 펜싱을 풀고, 다시 리더일 때 VM이 그 노드로 가
    // 있으면 남은 단계(Task 실패, 기록 Task)를 한 번 한다.
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    let spare = node(&env, "spare", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let task = pending_task(&env.cluster, a.id, failed).await;
    let writer = Arc::new(SpecUnknownThenNewTerm {
        hit: AtomicBool::new(false),
        resent: AtomicUsize::new(0),
        bump: AtomicU64::new(0),
    });
    let manager = manager_with(&env, writer.clone());
    eventually("spec write resent", || {
        writer.resent.load(Ordering::SeqCst) >= 1
    })
    .await;
    assert!(is_fenced(&renew(&env.cluster, failed).await));
    assert_eq!(get(&env, a.id).spec.node, Some(spare));
    assert!(recover_tasks(&env).is_empty());

    writer.bump.store(1, Ordering::SeqCst);
    eventually("record task", || recover_tasks(&env).len() == 1).await;
    renew_eventually(&env.cluster, failed).await;
    let records = recover_tasks(&env);
    assert_eq!(records[0].spec.target, a.id);
    assert_eq!(records[0].spec.node, spare);
    assert_eq!(
        env.cluster.get::<TaskSpec>(task).unwrap().unwrap().status,
        TaskStatus {
            state: TaskState::Failed,
            error: Some("node failed failed; the VM was recovered by HA".into()),
        }
    );
    // 한 번만 한다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(recover_tasks(&env).len(), 1);
    manager.shutdown().await;
}

/// 처음 쓰기를 끝내지 않는다.
struct Stuck {
    started: AtomicBool,
}

#[async_trait]
impl RecoveryWriter for Stuck {
    async fn write(&self, _: &Cluster, _: Command) -> Result<u64, ClusterError> {
        self.started.store(true, Ordering::SeqCst);
        std::future::pending().await
    }
}

#[tokio::test]
async fn keeps_the_fence_when_stopped_during_a_write() {
    // 쓰는 중에 멈추면 그 쓰기가 나중에 커밋될 수 있으므로 펜싱 표시를 지우지 않는다.
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    node(&env, "spare", 8192, Some(60)).await;
    vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let writer = Arc::new(Stuck {
        started: AtomicBool::new(false),
    });
    let manager = manager_with(&env, writer.clone());
    eventually("write started", || writer.started.load(Ordering::SeqCst)).await;
    manager.shutdown().await;
    let result = renew(&env.cluster, failed).await;
    assert!(is_fenced(&result), "renew after stop: {result:?}");
}

/// 모든 쓰기가 `error`로 끝난다(적용하지 않는다).
struct Failing {
    error: fn() -> ClusterError,
    writes: AtomicUsize,
}

#[async_trait]
impl RecoveryWriter for Failing {
    async fn write(&self, _: &Cluster, _: Command) -> Result<u64, ClusterError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Err((self.error)())
    }
}

/// 복구 쓰기가 결과를 아는 오류로 끝나면 펜싱 표시를 지워 그 노드가 다시 리스를 받을 수 있다.
async fn ends_the_fence_after_a_write_error(error: fn() -> ClusterError) {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    node(&env, "spare", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let writer = Arc::new(Failing {
        error,
        writes: AtomicUsize::new(0),
    });
    let manager = manager_with(&env, writer.clone());
    eventually("write tried", || writer.writes.load(Ordering::SeqCst) >= 1).await;
    renew_eventually(&env.cluster, failed).await;
    assert_eq!(get(&env, a.id), a);
    manager.shutdown().await;
}

#[tokio::test]
async fn ends_the_fence_after_losing_leadership_during_a_write() {
    ends_the_fence_after_a_write_error(|| ClusterError::NotLeader {
        leader_id: None,
        leader_addr: None,
    })
    .await;
}

#[tokio::test]
async fn ends_the_fence_after_a_rejected_write() {
    ends_the_fence_after_a_write_error(|| {
        ClusterError::Rejected(Rejection::RevisionMismatch {
            expected: 1,
            actual: 2,
        })
    })
    .await;
}
```

`crates/opl-types/src/task.rs`에서

```rust
        node: Uuid,
    },
```

을 다음으로 바꾼다:

```rust
        node: Uuid,
    },
    /// 리더의 HA 복구 기록: 노드 `from`이 죽어 대상 VM을 Task의 `node`로 옮겼다. 처음부터 끝난
    /// 상태(Succeeded)로 만들고 실행기는 실행하지 않는다.
    VmHaRecover {
        from: Uuid,
    },
```

`crates/opl-types/src/task.rs`에서

```rust
    #[test]
    fn new_task_is_pending() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn ha_recover_op_is_tagged_by_kind() {
        let from = Uuid::now_v7();
        let op = TaskOp::VmHaRecover { from };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "vm-ha-recover", "from": from })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
    }

    #[test]
    fn new_task_is_pending() {
```

`crates/opld/src/member.rs`에서

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리, 노드별 풀 상태. 위트니스는 노드 간 서버와 클러스터
//! 저장소, 승격, Node 등록만 띄운다.
```

을 다음으로 바꾼다:

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리, HA 복구, 노드별 풀 상태. 위트니스는 노드 간 서버와 클러스터
//! 저장소, 승격, Node 등록만 띄운다.
```

`crates/opld/src/member.rs`에서

```rust
use opl_api::{
    AppState, ClusterJanitor, IncomingConfig, IncomingMigrations, SnapshotScheduler, TaskRunner,
    TaskRunnerConfig, UefiVarsFetcher,
};
```

을 다음으로 바꾼다:

```rust
use opl_api::{
    AppState, ClusterJanitor, HaManager, HaManagerConfig, IncomingConfig, IncomingMigrations,
    SnapshotScheduler, TaskRunner, TaskRunnerConfig, UefiVarsFetcher,
};
```

`crates/opld/src/member.rs`에서

```rust
    janitor: ClusterJanitor,
    pools: PoolMounter,
```

을 다음으로 바꾼다:

```rust
    janitor: ClusterJanitor,
    /// 리더가 리스가 끝난 노드의 HA VM을 옮긴다(이 노드의 펜싱 설정과 상관없다).
    recovery: HaManager,
    pools: PoolMounter,
```

`crates/opld/src/member.rs`에서

```rust
        self.janitor.shutdown().await;
        self.pools.shutdown().await;
```

을 다음으로 바꾼다:

```rust
        self.janitor.shutdown().await;
        self.recovery.shutdown().await;
        self.pools.shutdown().await;
```

`crates/opld/src/member.rs`에서

```rust
        let janitor = ClusterJanitor::spawn(state.clone(), config.janitor.janitor_config());
        let app = opl_api::router(state).merge(crate::web::router());
```

을 다음으로 바꾼다:

```rust
        let janitor = ClusterJanitor::spawn(state.clone(), config.janitor.janitor_config());
        let recovery = HaManager::spawn(state.clone(), HaManagerConfig::default());
        let app = opl_api::router(state).merge(crate::web::router());
```

`crates/opld/src/member.rs`에서

```rust
                janitor,
                pools,
```

을 다음으로 바꾼다:

```rust
                janitor,
                recovery,
                pools,
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 841개, 실패 0

Run: `cargo test -p opl-api --test ha`
Expected: PASS

Run: `cargo test -p opl-api --lib ha`
Expected: PASS

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(opl-api): 리더의 HaManager — 리스가 끝난 노드의 HA VM을 다른 노드로 옮긴다"
```

---

### Task 4: HA 복구 Task 표시

oplctl `task list`의 OP에 `vm-ha-recover (from <노드 id>)`(`task_row`), 웹 Task 목록 "{{target}} HA 복구 (노드 {{from}}에서)"(노드 이름, 없으면 id — `describeTask`의 노드 이름 함수). en·ko.

**Files:**
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/tasks/TasksPage.test.tsx`
- Modify: `web/src/tasks/TasksPage.tsx`
- Modify: `web/src/tasks/tasks.test.tsx`
- Modify: `web/src/tasks/tasks.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs`에서

```rust
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
```

을 다음으로 바꾼다:

```rust
                    &["ID", "OP", "TARGET", "STATE", "ERROR"],
                    task_row,
                )?;
```

`crates/oplctl/src/cli.rs`에서

```rust
        }
    }
    Ok(())
}

/// `vm list`의 한 행. 마이그레이션 중이면 NODE에 대상 노드를 붙인다(`A -> B`).
```

을 다음으로 바꾼다:

```rust
        }
    }
    Ok(())
}

/// `task list`의 한 행. HA 복구면 OP에 죽은 노드를 붙인다(`vm-ha-recover (from A)`).
fn task_row(t: &Value) -> Vec<String> {
    let op = &t["spec"]["op"];
    let mut kind = cell(&op["kind"]);
    if op["kind"] == "vm-ha-recover" {
        kind = format!("{kind} (from {})", cell(&op["from"]));
    }
    vec![
        cell(&t["id"]),
        kind,
        cell(&t["spec"]["target"]),
        cell(&t["status"]["state"]),
        cell(&t["status"]["error"]),
    ]
}

/// `vm list`의 한 행. 마이그레이션 중이면 NODE에 대상 노드를 붙인다(`A -> B`).
```

`crates/oplctl/src/cli.rs`에서

```rust
    #[test]
    fn delete_with_snapshots_turns_a_schedule_off() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn task_row_shows_where_ha_moved_the_vm_from() {
        let task = json!({
            "id": "t1",
            "spec": { "op": { "kind": "vm-ha-recover", "from": "id-a" }, "target": "vm-1", "node": "id-b" },
            "status": { "state": "succeeded", "error": null },
        });
        assert_eq!(
            task_row(&task),
            ["t1", "vm-ha-recover (from id-a)", "vm-1", "succeeded", "-"]
        );
        // 다른 종류는 이름만
        let mut task = task;
        task["spec"]["op"] = json!({ "kind": "vm-start" });
        assert_eq!(task_row(&task)[1], "vm-start");
    }

    #[test]
    fn delete_with_snapshots_turns_a_schedule_off() {
```

`web/src/i18n/en.json`에서

```json
      "vm-clone": "Clone {{target}}",
      "vm-migrate": "Migrate {{target}}"
    }
```

을 다음으로 바꾼다:

```json
      "vm-clone": "Clone {{target}}",
      "vm-migrate": "Migrate {{target}}",
      "vm-ha-recover": "HA recovery of {{target}} (from node {{from}})"
    }
```

`web/src/i18n/ko.json`에서

```json
      "vm-clone": "{{target}} 복제",
      "vm-migrate": "{{target}} 마이그레이션"
    }
```

을 다음으로 바꾼다:

```json
      "vm-clone": "{{target}} 복제",
      "vm-migrate": "{{target}} 마이그레이션",
      "vm-ha-recover": "{{target}} HA 복구 (노드 {{from}}에서)"
    }
```

`web/src/tasks/TasksPage.test.tsx`에서

```tsx

function many(n: number, state: "failed" | "succeeded" = "succeeded") {
```

을 다음으로 바꾼다:

```tsx

it("shows HA recoveries with the failed node's name", async () => {
  mockLists({
    vm: [{ id: VM, name: "web" }],
    node: [
      { id: "n1", name: "node-a" },
      { id: "n2", name: "node-b" },
    ],
    task: [
      {
        id: "019a0f3c-8a10-7000-8000-000000000004",
        spec: { op: { kind: "vm-ha-recover", from: "n1" }, target: VM, node: "n2" },
        status: { state: "succeeded", error: null },
      },
    ],
  });
  renderPage(<TasksPage />);
  const row = (
    await screen.findByRole("link", { name: "HA recovery of web (from node node-a)" })
  ).closest("tr")!;
  expect(row).toHaveTextContent("node-b");
});

function many(n: number, state: "failed" | "succeeded" = "succeeded") {
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
          const time = uuidTime(task.id);
          return (
```

을 다음으로 바꾼다:

```tsx
          const time = uuidTime(task.id);
          const label = describeTask(task, t, name, (id) => nodeById.get(id)?.name);
          return (
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
                  <Link to={href} className="underline-offset-4 hover:underline">
                    {describeTask(task, t, name)}
                  </Link>
                ) : (
                  describeTask(task, t, name)
                )}
```

을 다음으로 바꾼다:

```tsx
                  <Link to={href} className="underline-offset-4 hover:underline">
                    {label}
                  </Link>
                ) : (
                  label
                )}
```

`web/src/tasks/tasks.test.tsx`에서

```tsx

it("waits for a task and rejects with its error when it fails", async () => {
```

을 다음으로 바꾼다:

```tsx

it("names the failed node of an HA recovery", () => {
  const recover = {
    ...task("4", "succeeded"),
    spec: { op: { kind: "vm-ha-recover", from: "id-a" }, target: "x", node: "id-b" },
  } as Task;
  // 노드 이름을 모르면 id로
  expect(describeTask(recover, i18n.t, "web")).toBe("HA recovery of web (from node id-a)");
  const name = (id: string) => (id === "id-a" ? "node-a" : undefined);
  expect(describeTask(recover, i18n.t, "web", name)).toBe("HA recovery of web (from node node-a)");
});

it("waits for a task and rejects with its error when it fails", async () => {
```

`web/src/tasks/tasks.ts`에서

```ts

/** Task를 사람이 읽을 문장으로: "web-01 시작" */
export function describeTask(task: Task, t: TFunction, target: string): string {
  const kind = task.spec.op.kind;
  // 모르는 작업(새 서버)은 번역 키 대신 종류를 그대로 보여 준다.
  return t(`tasks.op.${kind}`, { target, defaultValue: `${kind} ${target}` });
}
```

을 다음으로 바꾼다:

```ts

/**
 * Task를 사람이 읽을 문장으로: "web-01 시작". `nodeName`은 노드 id의 이름(HA 복구의 죽은 노드)이고,
 * 모르면 id를 보인다.
 */
export function describeTask(
  task: Task,
  t: TFunction,
  target: string,
  nodeName: (id: string) => string | undefined = () => undefined,
): string {
  const op = task.spec.op;
  const from = op.kind === "vm-ha-recover" ? (nodeName(op.from) ?? op.from) : undefined;
  // 모르는 작업(새 서버)은 번역 키 대신 종류를 그대로 보여 준다.
  return t(`tasks.op.${op.kind}`, { target, from, defaultValue: `${op.kind} ${target}` });
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 842개, 실패 0

Run: `cargo test -p oplctl`
Expected: PASS

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 325개

- [ ] **Step 3: 커밋**

```bash
git add crates web
git commit -m "feat: HA 복구 Task 표시 — oplctl task list에 죽은 노드, 웹 "HA 복구 (노드 {from}에서)""
```

---

### Task 5: 실제 시험: 죽은 노드의 HA VM을 다른 노드에서 켠다

opld 기능 `ha-it`, `crates/opld/tests/ha.rs`: 노드 A·B(`[ha] fencing = "process"`, `fence_after_secs = 2`, `systemd_scope = false`)와 위트니스. 공유 디렉터리 풀 위 디스크를 가진 HA VM `db`(`--ha --restart on-failure`)와 디스크 없는 `plain`을 A에서 켠다. A의 opld를 `kill -9`한 뒤 두 QEMU를 pid로 죽이면 180초 안에 `db`가 B에서 새 pid로 Running, `vm-ha-recover` Task 하나(from A, 노드 B, Succeeded), `plain`은 A에 배정된 채. A를 다시 띄워도 10초 동안 `db`는 B에서 같은 pid로 돈다. CI `linux-kvm`의 기능 목록에 `opld/ha-it`.

**Files:**
- Create: `crates/opld/tests/ha.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `crates/opld/Cargo.toml`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml
        env:
          FEATURES: opl-qemu/qemu-it,opl-driver/qemu-it,opl-driver/net-it,opl-reconciler/qemu-it,opld/qemu-it,opld/net-it,opld/web-it,opld/nfs-it,opl-driver/lvm-it,opld/lvm-it,opl-driver/zfs-it,opld/zfs-it,opl-driver/rbd-it,opld/rbd-it,opl-driver/lvm-shared-it,opld/lvm-shared-it,opld/migration-it
          OPL_TEST_NFS_EXPORT: /srv/opl-nfs
```

을 다음으로 바꾼다:

```yaml
        env:
          FEATURES: opl-qemu/qemu-it,opl-driver/qemu-it,opl-driver/net-it,opl-reconciler/qemu-it,opld/qemu-it,opld/net-it,opld/web-it,opld/nfs-it,opl-driver/lvm-it,opld/lvm-it,opl-driver/zfs-it,opld/zfs-it,opl-driver/rbd-it,opld/rbd-it,opl-driver/lvm-shared-it,opld/lvm-shared-it,opld/migration-it,opld/ha-it
          OPL_TEST_NFS_EXPORT: /srv/opl-nfs
```

`crates/opld/Cargo.toml`에서

```toml
migration-it = []
```

을 다음으로 바꾼다:

```toml
migration-it = []
# 한 호스트의 opld 둘(프로세스 펜싱)과 위트니스로 죽은 노드의 HA VM을 리더가 다른 노드에서 켜는지 보는
# E2E. qemu-it의 준비물과 qemu-img가 필요하다.
ha-it = []
```

`crates/opld/tests/ha.rs`를 만든다:

```rust
//! 실제 HA 복구: 한 호스트의 opld 둘(`[ha] fencing = "process"`)과 위트니스로 클러스터를 만들고, 노드 A에서
//! 도는 HA VM과 HA 아닌 VM을 둔 채 A의 opld와 그 QEMU들을 kill -9 한다. 리더가 A의 리스가 끝난 것을 보고
//! HA VM만 노드 B로 옮겨 켜는지(기록 Task `vm-ha-recover`), A를 다시 띄워도 그 VM을 건드리지 않는지 본다.
//! `cargo test -p opld --features ha-it --test ha`
//! QEMU(`qemu-it`와 같다)와 qemu-img가 필요하다. 이 Mac에서도 돈다.
//!
//! 한 호스트의 두 노드는 systemd 스코프 이름(`opl-vm-{id}.scope`)이 겹치므로 스코프를 끈다(`migration.rs`).
#![cfg(feature = "ha-it")]

mod common;

use std::future::Future;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Daemon, eventually};
use oplctl::Client;
use rustix::process::{Pid, Signal, kill_process, test_kill_process};
use serde_json::Value;

/// 복구를 기다리는 시간. A의 recover_after(fence_after 2초 + 여유 5초 = 7초), 리더 선거, 대상 노드의
/// QEMU 시작(CI의 KVM, 이 Mac의 HVF·TCG)을 넉넉히 덮는다.
const RECOVER_WAIT: Duration = Duration::from_secs(180);

/// 두 노드의 설정: 프로세스 펜싱, 리스가 2초 끊기면 HA VM을 끈다. UEFI만 있는 게스트는 ACPI 종료에
/// 응하지 않을 수 있다.
const CONFIG: &str =
    "[reconciler]\nacpi_timeout_secs = 5\n[ha]\nfencing = \"process\"\nfence_after_secs = 2\n";

/// `oplctl login`이 저장한 로그인 정보로 API 클라이언트를 만든다.
fn client(d: &Daemon) -> Client {
    let config: Value =
        serde_json::from_slice(&std::fs::read(d.path("oplctl.json")).unwrap()).unwrap();
    Client::new(
        config["server"].as_str().unwrap(),
        config["token"].as_str().map(str::to_string),
        config["ca_pem"].as_str().map(str::to_string),
    )
    .unwrap()
}

/// 조건이 맞을 때까지 `wait` 동안 다시 확인한다.
async fn eventually_within<F, Fut>(what: &str, wait: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + wait;
    while !check().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// 프로세스가 살아 있는지
fn alive(pid: u64) -> bool {
    test_kill_process(Pid::from_raw(pid as i32).unwrap()).is_ok()
}

/// 명령줄에 `pattern`이 든 프로세스가 있는지(그 노드의 VM 실행 디렉터리로 그 노드의 QEMU를 찾는다)
fn process_with(pattern: &Path) -> bool {
    Command::new("pgrep")
        .arg("-f")
        .arg(pattern)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run pgrep")
        .success()
}

/// 이 시험이 띄운 QEMU 하나를 pid로 kill -9 하고 사라질 때까지 기다린다.
async fn kill_qemu(pid: u64) {
    let _ = kill_process(Pid::from_raw(pid as i32).unwrap(), Signal::KILL);
    eventually(&format!("QEMU {pid} gone"), || async { !alive(pid) }).await;
}

/// 노드 이름 → id
async fn node_id(c: &Client, name: &str) -> String {
    c.get(&format!("/nodes/{name}")).await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// 노드 `node`에서 도는 VM의 QEMU pid
async fn running_pid(c: &Client, vm: &str, node: &str) -> u64 {
    let deadline = Instant::now() + RECOVER_WAIT;
    loop {
        if let Ok(v) = c.get(&format!("/vms/{vm}")).await
            && v["status"]["phase"] == "running"
            && v["status"]["node"] == node
            && let Some(pid) = v["status"]["pid"].as_u64()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for vm {vm} running on {node}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test]
async fn the_leader_restarts_an_ha_vm_of_a_dead_node_on_another_node() {
    let daemon = |name: &str| Daemon::with_hypervisor(name, CONFIG, "systemd_scope = false");
    let mut a = daemon("node-a");
    let mut b = daemon("node-b");
    let mut w = Daemon::witness("wit");
    a.start().await;
    b.start().await;
    w.start().await;
    a.init("ha").await;
    a.login().await;
    let url = a.join_url.clone().unwrap();
    b.join(&url).await.unwrap();
    b.login_with(
        a.admin_password.as_deref().unwrap(),
        a.ca_fingerprint.as_deref().unwrap(),
    )
    .await;
    let url = a.ctl(&["cluster", "join-token"]).await.unwrap();
    w.join(url.trim()).await.unwrap();
    let (ca, cb) = (client(&a), client(&b));
    // 두 일반 노드가 온라인이고 HA를 돌릴 수 있다(펜싱 설정을 등록했다).
    eventually("both nodes online and HA capable", || async {
        let nodes = ca.get("/nodes").await.unwrap();
        let nodes = nodes["items"].as_array().unwrap().clone();
        nodes.len() == 3
            && nodes.iter().all(|n| n["status"]["online"] == true)
            && nodes
                .iter()
                .filter(|n| n["spec"]["witness"] != true)
                .all(|n| n["status"]["ha"]["fencing"] == "process")
    })
    .await;
    let (id_a, id_b) = (node_id(&ca, "node-a").await, node_id(&ca, "node-b").await);

    // 두 노드가 같은 경로로 여는 공유 디렉터리 풀과 그 위의 디스크
    let shared = tempfile::Builder::new()
        .prefix("oplha")
        .tempdir_in("/tmp")
        .unwrap();
    a.ctl(&[
        "pool",
        "create",
        "shared",
        "--path",
        shared.path().to_str().unwrap(),
        "--shared",
    ])
    .await
    .unwrap();
    a.wait_pool_ready("shared").await;
    b.wait_pool_ready("shared").await;
    a.ctl(&[
        "volume", "create", "db-root", "--pool", "shared", "--size", "64M",
    ])
    .await
    .unwrap();

    // 실행 중인 VM의 HA는 켤 수 없으므로 만들 때 켠다. HA 아닌 VM은 디스크 없이 둔다.
    a.ctl(&[
        "vm",
        "create",
        "db",
        "--memory",
        "256",
        "--disk",
        "db-root",
        "--node",
        "node-a",
        "--ha",
        "--restart",
        "on-failure",
        "--start",
    ])
    .await
    .unwrap();
    a.ctl(&[
        "vm", "create", "plain", "--memory", "256", "--node", "node-a", "--start",
    ])
    .await
    .unwrap();
    let db_pid = running_pid(&ca, "db", &id_a).await;
    let plain_pid = running_pid(&ca, "plain", &id_a).await;

    // 노드 A가 죽는다: opld를 먼저(QEMU를 다시 띄우지 않게), 그 다음 이 시험이 띄운 A의 QEMU들
    a.kill();
    kill_qemu(db_pid).await;
    kill_qemu(plain_pid).await;

    // 리더(B, 위트니스와 과반수)가 A의 리스가 끝난 것을 보고 HA VM을 B로 옮겨 켠다.
    eventually_within("HA VM running on node B", RECOVER_WAIT, || async {
        let Ok(vm) = cb.get("/vms/db").await else {
            return false;
        };
        vm["spec"]["node"] == id_b.as_str()
            && vm["status"]["phase"] == "running"
            && vm["status"]["node"] == id_b.as_str()
    })
    .await;
    let recovered_pid = running_pid(&cb, "db", &id_b).await;
    assert_ne!(recovered_pid, db_pid);
    assert!(alive(recovered_pid));

    // 기록 Task: A에서 B로, 처음부터 성공
    let vm_db = cb.get("/vms/db").await.unwrap();
    let db_id = vm_db["id"].as_str().unwrap().to_string();
    let tasks = cb.get("/tasks").await.unwrap();
    let records: Vec<&Value> = tasks["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["spec"]["op"]["kind"] == "vm-ha-recover")
        .collect();
    assert_eq!(records.len(), 1, "{tasks}");
    let record = records[0];
    assert_eq!(record["spec"]["op"]["from"], id_a.as_str(), "{record}");
    assert_eq!(record["spec"]["target"], vm_db["id"], "{record}");
    assert_eq!(record["spec"]["node"], id_b.as_str(), "{record}");
    assert_eq!(record["status"]["state"], "succeeded", "{record}");

    // HA 아닌 VM은 A에 남는다.
    let plain = cb.get("/vms/plain").await.unwrap();
    assert_eq!(plain["spec"]["node"], id_a.as_str(), "{plain}");

    // A를 다시 띄워도 HA VM은 B에서 그대로 돈다(A는 그 VM의 QEMU를 띄우지 않는다).
    a.start().await;
    eventually_within("node A back online", RECOVER_WAIT, || async {
        let Ok(node) = cb.get("/nodes/node-a").await else {
            return false;
        };
        node["status"]["online"] == true
    })
    .await;
    // A의 워커가 자리를 잡을 동안 몇 번 본다. A가 옛 상태로 잠깐 쓴 status("waiting for the cluster
    // majority …", 노드 A)는 B의 워커가 곧 다시 쓰므로 status는 끝에 한 번 본다.
    for _ in 0..20 {
        let vm = cb.get("/vms/db").await.unwrap();
        assert_eq!(vm["spec"]["node"], id_b.as_str(), "{vm}");
        assert!(alive(recovered_pid));
        assert!(!process_with(&a.path("run").join("vm").join(&db_id)));
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(running_pid(&cb, "db", &id_b).await, recovered_pid);
    let plain = cb.get("/vms/plain").await.unwrap();
    assert_eq!(plain["spec"]["node"], id_a.as_str(), "{plain}");

    b.ctl(&["vm", "stop", "db", "--force"]).await.unwrap();
    b.ctl(&["vm", "stop", "plain", "--force"]).await.unwrap();
    b.ctl(&["vm", "delete", "db"]).await.unwrap();
    b.ctl(&["vm", "delete", "plain"]).await.unwrap();
    b.ctl(&["volume", "delete", "db-root"]).await.unwrap();
    b.ctl(&["pool", "delete", "shared"]).await.unwrap();
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 842개, 실패 0

Run: `cargo test -p opld --features ha-it --test ha`
Expected: macOS에서 PASS 1개(약 30초)

- [ ] **Step 3: 커밋**

```bash
git add .github crates
git commit -m "test(opld): 실제 HA 복구 E2E ha-it — 죽은 노드의 HA VM을 리더가 다른 노드에서 켠다"
```

---

### Task 6: 문서

README HA 절에 복구(걸리는 시간, 기록 Task, HA는 멈춘 VM에서 켬, 이미지 잠금 기다림, 죽은 노드가 돌아오면, 한계), 설계 문서 §3.1의 새 규칙과 §4~§7의 "구현(계획 24)" 메모, 처음 분해 문서 §1.1(하위 프로젝트 5 완료).

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

````markdown
노드의 HA VM을 강제로 끈다. opld 자신이 멈추거나 멈춰 서도 끄도록 Linux watchdog 장치를 쥐고 있다가, 리스를 잃은
채 HA VM이 돌면 쓰다듬기를 멈춰 노드를 재시작시킨다. **죽은 노드의 HA VM을 다른 노드에서 다시 켜는 복구는 계획
24에서 더한다.** 지금은 펜싱된 HA VM이 그 노드에 남아 있다가 리스가 돌아오면 그 노드에서 다시 켜진다.

```bash
oplctl vm create web --memory 1024 --disk root --ha --restart on-failure --start
oplctl vm set web --no-ha            # 끄기(--ha로 다시 켠다). 실행 중이어도 바꿀 수 있다.
oplctl vm list                       # HA 열(true/false)
oplctl node list                     # FENCING 열: watchdog · process · -(꺼짐)
oplctl cluster status                # 멤버마다 FENCING 열
```

웹은 VM 만들기·편집의 "고가용성(HA)" 스위치(그 노드의 펜싱이 꺼져 있으면 이유와 함께 막는다), VM 목록·상세의
HA 배지, 노드·클러스터·대시보드 노드 표의 "펜싱" 열이다. API는 VM `spec.ha`(생성·`PUT /api/v1/vms/{key}`),
Node `status.ha`(`{"fencing": "watchdog"|"process", "recover_after_secs": …}`, 펜싱이 꺼져 있으면 없다),
````

을 다음으로 바꾼다:

````markdown
노드의 HA VM을 강제로 끈다. opld 자신이 멈추거나 멈춰 서도 끄도록 Linux watchdog 장치를 쥐고 있다가, 리스를 잃은
채 HA VM이 돌면 쓰다듬기를 멈춰 노드를 재시작시킨다. 리더는 노드가 스스로 펜싱을 마쳤을 시간(`recover_after_secs`)이
지나도록 리스가 돌아오지 않으면 그 노드의 HA VM을 다른 노드로 옮겨 켠다(**복구**, 아래).

```bash
oplctl vm create web --memory 1024 --disk root --ha --restart on-failure --start
oplctl vm set web --no-ha            # 끄기. 실행 중이어도 된다.
oplctl vm set web --ha               # 켜기. VM이 멈춰 있을 때만 된다(실행 중이면 409 vm_running).
oplctl vm list                       # HA 열(true/false)
oplctl node list                     # FENCING 열: watchdog · process · -(꺼짐)
oplctl cluster status                # 멤버마다 FENCING 열
oplctl task list                     # 복구 기록: OP "vm-ha-recover (from <죽은 노드 id>)"
```

웹은 VM 만들기·편집의 "고가용성(HA)" 스위치(그 노드의 펜싱이 꺼져 있거나, 실행 중이고 HA가 꺼진 VM이면 이유와
함께 켜기를 막는다), VM 목록·상세의 HA 배지, 노드·클러스터·대시보드 노드 표의 "펜싱" 열, 작업 목록의
"web HA 복구 (노드 node-a에서)"다. API는 VM `spec.ha`(생성·`PUT /api/v1/vms/{key}`),
Node `status.ha`(`{"fencing": "watchdog"|"process", "recover_after_secs": …}`, 펜싱이 꺼져 있으면 없다),
````

`README.md`에서

```markdown
  노드만 고른다(없으면 **409** `no_node`). 노드는 등록할 때마다 `[ha]`를 `status.ha`에 쓴다. `recover_after_secs`는
  `fence_after_secs + watchdog_timeout_secs(watchdog일 때) + 5`(기본 65초)로, 계획 24의 리더가 이만큼 기다린 뒤 옮긴다.
- **리스.** 갱신 간격은 `fence_after_secs`의 10분의 1(0.2초 이상 2초 이하, 기본 2초)이다. 리더는 아직 과반수의
```

을 다음으로 바꾼다:

```markdown
  노드만 고른다(없으면 **409** `no_node`). 노드는 등록할 때마다 `[ha]`를 `status.ha`에 쓴다. `recover_after_secs`는
  `fence_after_secs + watchdog_timeout_secs(watchdog일 때) + 5`(기본 65초)로, 리더가 이만큼 기다린 뒤 옮긴다.
- **HA는 멈춘 VM에서만 켠다.** 만들 때 `--ha`는 그대로 되지만, 이미 있는 VM의 HA를 켜는 수정(`vm set --ha`, 웹
  편집, `PUT`의 `ha` false → true)은 VM이 멈춰 있을 때만 된다(원하는 상태가 stopped이고 phase가 stopped·failed).
  아니면 **409** `vm_running`("cannot turn on HA for VM "web" while it is running; turn on HA while the VM is
  stopped"). 끄기는 언제든 된다. 노드가 과반수와 끊긴 동안 실행 중인 VM의 HA를 켜면 그 노드는 HA인 줄 모르고
  끄지 않는데 리더는 그 VM을 다른 노드에서 켜 둘이 함께 돌 수 있기 때문이다. 실행 중인 VM은 정지하고 HA를 켠 뒤
  다시 시작한다.
- **리스.** 갱신 간격은 `fence_after_secs`의 10분의 1(0.2초 이상 2초 이하, 기본 2초)이다. 리더는 아직 과반수의
```

`README.md`에서

```markdown
- **리스를 잃으면.** `fence_after_secs`가 지나면 그 노드의 HA VM을 강제로 끈다(실패로 세지 않고 원하는 상태는
  running 그대로, status "fenced: lost the cluster majority"). HA 아닌 VM은 그대로 돈다. 리스가 돌아오면 그 노드에서
  다시 켠다. 리스 없이는 HA VM을 켜지도, 멈춘 채 받은 마이그레이션을 이어서 실행하지도 않는다("waiting for the
  cluster majority before starting an HA VM", 실패로 세지 않고 다시 본다). 켜기 바로 앞에서 리스와 VM을 다시 보아,
  그 사이 다른 노드로 배정됐거나 멈추라고 했으면 켜지 않는다. 짧은 네트워크 장애라도 `fence_after_secs`
```

을 다음으로 바꾼다:

```markdown
- **리스를 잃으면.** `fence_after_secs`가 지나면 그 노드의 HA VM을 강제로 끈다(실패로 세지 않고 원하는 상태는
  running 그대로, status "fenced: lost the cluster majority"). HA 아닌 VM은 그대로 돈다. 리더가 옮기기 전에 리스가
  돌아오면 그 노드에서 다시 켠다. 리스 없이는 HA VM을 켜지도, 멈춘 채 받은 마이그레이션을 이어서 실행하지도
  않는다("waiting for the cluster majority before starting an HA VM", 실패로 세지 않고 다시 본다). 켜기 바로 앞에서 리스와 VM을 다시 보아,
  그 사이 다른 노드로 배정됐거나 멈추라고 했으면 켜지 않는다. 짧은 네트워크 장애라도 `fence_after_secs`
```

`README.md`에서

```markdown
  끄고 다시 켠다).
```

을 다음으로 바꾼다:

```markdown
  끄고 다시 켠다).

**복구.** 리더(위트니스가 아닌 노드)가 1초마다 노드의 리스를 보고 다음 노드의 HA VM을 옮긴다.

- **언제.** 리스를 마지막으로 갱신한 뒤 그 노드의 `recover_after_secs`(기본 65초)가 지난 노드. 리스는 리더의
  메모리에만 있으므로 리더가 바뀌면 새 리더는 리더가 된 때부터 다시 센다. 그래서 노드가 죽고 VM이 다른 노드에서
  켜지기까지는 대략 `recover_after_secs` + 1초에 부팅 시간이다. 리더도 함께 죽었으면 선거 시간이 더해지고, 디스크
  잠금이 남아 있으면(아래) 그만큼 더 걸린다. `fencing = "process"`, `fence_after_secs = 2`(recover_after 7초)인
  자동 시험에서는 수십 초다. 펜싱이 꺼진 노드(`status.ha` 없음)의 VM은 옮기지 않는다.
- **무엇을.** 그 노드에 배정되고(`spec.node`) HA이고 원하는 상태가 running인 VM을 이름 순서로 옮긴다. 게스트가
  스스로 끄거나 사용자가 정지한 VM, HA 아닌 VM은 그 노드에 남는다. 옮기는 동안 리더는 그 노드의 리스 갱신을
  거부한다(펜싱 중 표시) — 그 노드가 돌아와 낡은 배정을 보고 VM을 켜지 않게 하려는 것이다. 복구 기록의 결과를
  모르면(시간 초과 — 나중에 커밋될 수 있다) 결과를 알 때까지 같은 기록을 다시 보내며 거부를 이어 가고, 리더가
  바뀌면 푼다.
- **어디로.** 스케줄러와 같은 규칙(온라인, 유지보수 아님, 위트니스 아님, 아키텍처·디스크 풀·브리지, 미할당 메모리가
  가장 많은 노드)에 더해 HA 가능 노드이고 리스가 살아 있는 노드다. 죽은 노드와 진행 중이던 마이그레이션의 대상
  노드는 뺀다. 여러 VM을 옮기면 앞서 옮긴 VM의 메모리도 센다. 그런 노드가 없으면 VM은 그대로 두고 status에
  "HA cannot find a node for this VM"만 남긴 뒤 1초마다 다시 본다.
- **어떻게.** status를 먼저 비우고(phase stopped, "recovering: node node-a failed") 그 다음 `spec.node`를 대상
  노드로 바꾼다(둘 다 revision 검사, 그 사이 바뀌었으면 1초 뒤 다시). 대상 노드의 reconciler가 평소처럼 새로
  켠다. 죽은 노드가 맡았던 그 VM의 끝나지 않은 작업은 "node node-a failed; the VM was recovered by HA"로 실패
  처리하고(그래야 `vm_busy`가 풀린다), 기록 작업 `vm-ha-recover`(대상 VM, 노드 = 옮긴 노드, `from` = 죽은 노드)를
  처음부터 성공 상태로 남긴다. 작업 실행기는 이것을 실행하지 않는다.
- **이미지 잠금 기다리기.** 죽은 노드의 QEMU가 쥐었던 디스크 잠금은 곧바로 풀리지 않을 수 있다(NFSv4 임대 약 90초,
  공유 LVM의 sanlock 임대). QEMU 시작이 이미지 잠금 오류(`Failed to get "write" lock` / `Is another process using
  the image`)로 실패하면 실패로 세지 않고 2초마다 다시 켠다(phase starting, "disk image is locked by
  another process; retrying"). **이 규칙은 HA 아닌 VM에도 적용된다** — 다른 프로세스가 이미지를 쥔 동안에는 실패
  (`failed`)로 끝나지 않고 계속 다시 시도하므로, 멈추려면 VM을 정지한다. 공유 LVM은 전부터 다시 시도하는 오류였다.
- **죽은 노드가 돌아오면.** 펜싱 중 표시가 풀린 뒤의 첫 리스 갱신이 복구 기록까지 적용된 뒤에야 HA VM을 켜므로,
  옮겨 간 VM은 이미 다른 노드의 것으로 보고 켜지 않는다(낡은 기록으로 status를 덮지도 않는다). 그 노드의 실행기는
  실패 처리된 작업을 다시 하지 않는다. 그 노드에 남은 HA 아닌 VM은 평소처럼 그 노드가 다시 맞춘다. 옮긴 VM은
  원래 노드로 되돌아가지 않는다(필요하면 마이그레이션한다).
- **한계.** 죽은 노드에만 있던 UEFI 변수는 잃는다 — 대상 노드는 변수를 그 노드에서 받으려다 실패하면 경고를 남기고
  자기 노드의 옛 파일(없으면 템플릿)로 켠다. HA 아닌 VM은 노드가 돌아올 때까지 그 노드에 묶여 있다. Ceph RBD에는
  디스크 잠금이 없어 펜싱만 믿는다(opLite는 RBD 배타 잠금·blocklist를 쓰지 않는다). NFSv3에서는 죽은 노드의 잠금이
  그 노드가 돌아올 때까지 남을 수 있고, 그동안 VM은 "disk image is locked …; retrying"으로 기다린다. `process`
  펜싱 노드는 opld만 멈추면(QEMU는 남음) HA VM을 끄지 못하므로 복구가 VM을 둘 돌게 할 수 있다(시험·개발용).
  복구 도중 리더가 다른 노드로 바뀌면 죽은 노드에 남은 그 VM의 작업 실패 처리와 기록 작업 `vm-ha-recover`가 빠질 수
  있다(VM은 옮겨진다). 끝나지 않은 작업은 그 노드가 돌아오거나 제거될 때 정리된다.
```

`README.md`에서

````markdown
cargo test -p opld --features migration-it --test migration
```
````

을 다음으로 바꾼다:

````markdown
cargo test -p opld --features migration-it --test migration
# 실제 HA 복구: 한 호스트의 opld 둘(process 펜싱)과 위트니스, 공유 디렉터리 풀(macOS는 TCG로 돈다).
cargo test -p opld --features ha-it --test ha
```
````

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown

구현: 하위 프로젝트 5는 계획 22(위트니스 노드: VM을 돌리지 않는 Raft 투표 멤버, 완료), 계획 23(HA 리스와 watchdog 자기 펜싱: `VmSpec.ha`, 리더의 리스 표, 노드의 `HaGuard`, 리스를 잃으면 HA VM을 끔, 완료), 계획 24(HA VM 복구)로 나눴다(`2026-11-02-ha-fencing-witness-design.md`). 24는 아직이다.
```

을 다음으로 바꾼다:

```markdown

구현: 하위 프로젝트 5는 계획 22(위트니스 노드: VM을 돌리지 않는 Raft 투표 멤버, 완료), 계획 23(HA 리스와 watchdog 자기 펜싱: `VmSpec.ha`, 리더의 리스 표, 노드의 `HaGuard`, 리스를 잃으면 HA VM을 끔, 완료), 계획 24(리더의 HA VM 복구: `HaManager`가 리스가 끝난 노드의 HA VM을 다른 노드로 옮기고 기록 Task `vm-ha-recover`를 남김, 이미지 잠금 기다리기, HA는 멈춘 VM에서만 켬, 실제 시험 `ha-it`, 완료)로 나눴다(`2026-11-02-ha-fencing-witness-design.md`). 하위 프로젝트 5는 끝났다. 외부 펜싱 장치, 유지보수 때 모두 옮기기, HA 그룹·우선순위, HA 아닌 VM의 자동 복구는 넣지 않았다.
```

`docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md` 전체:

```markdown
# opLite HA·펜싱·위트니스 설계 (하위 프로젝트 5: 계획 22·23·24)

작성일: 2026-11-02
상위 문서: `2026-10-20-opl-node-core-design.md`(하위 프로젝트 분해, 노드 감시 §8, reconciler 규칙 §5.3), `2026-10-30-live-migration-design.md`(revision 검사 확정, UEFI 변수 위치), `2026-10-30-shared-storage-iscsi-lvm-design.md`(sanlock 잠금)

## 1. 목표와 범위

처음 분해에서 하위 프로젝트 5는 HA와 펜싱, 2노드 위트니스(ha-manager·vSphere HA)다. 지금은 노드가 죽으면 그 노드의 VM이 멈춘 채 남는다. `spec.node`는 죽은 노드를 가리키고, status는 낡은 `Running`이며, API도 실행 중으로 보여 노드를 바꾸지 못한다. 2노드 클러스터는 한 노드만 죽어도 과반수를 잃는다.

| 계획 | 내용 |
|---|---|
| 22 | 위트니스 노드: VM을 돌리지 않는 Raft 투표 멤버(QEMU 불필요, 선거에 나서지 않음), 합류·상태 표시, 배치에서 빼기 |
| 23 | HA 리스와 자기 펜싱: `VmSpec.ha`, 노드의 펜싱 설정(watchdog·프로세스), 리더가 확인하는 리스, 리스를 잃은 노드는 HA VM을 끈다 |
| 24 | 복구: 리더가 리스가 끝난 노드의 HA VM을 다른 노드로 옮겨 켠다. 이미지 잠금 기다리기, 기록 Task, 실제 시험 |

넣지 않는 것: IPMI·BMC 같은 외부 펜싱 장치, 노드 유지보수 때 모두 옮기기(유지보수 모드는 지금처럼 배치만 막는다), HA 그룹·노드 선호·우선순위·반친화성, 복구 뒤 원래 노드로 되돌리기, HA 아닌 VM의 자동 복구, 죽은 노드에만 있던 UEFI 변수 살리기, RBD 배타 잠금·blocklist, sanlock `wdmd`와 watchdog 나눠 쓰기, 노드 하나에 여러 클러스터의 위트니스.

### 견준 길

- **펜싱: 외부 장치(IPMI, STONITH) vs watchdog 자기 펜싱(채택).** 외부 장치는 확실하지만 하드웨어와 비밀번호 설정이 필요하고 목표 고객(중소 규모)이 늘 갖추고 있지 않다. Proxmox ha-manager처럼 각 노드가 리스를 갱신하지 못하면 스스로 HA VM을 끄고, 그러지 못하면(opld가 멈춤) watchdog가 노드를 재시작한다. 리더는 노드가 스스로 펜싱을 마쳤을 시간이 지난 뒤에만 VM을 옮긴다.
- **리스: Raft에 주기적으로 기록 vs 리더 메모리(채택).** 기록하면 노드 수만큼 초당 쓰기가 생기고 모든 watch가 깨어난다. 노드가 리더에게 갱신을 요청하면 리더가 `ensure_linearizable`로 자기가 과반수의 리더임을 확인한 뒤 받은 시각을 메모리에 적는다. 리더가 바뀌면 새 리더는 기록이 없으므로 리더가 된 때부터 센다(더 늦게 옮길 뿐 안전하다).
- **위트니스: 외부 중재자(corosync-qnetd) vs Raft 투표 멤버(채택).** Raft가 이미 있으므로 VM을 돌리지 않는 opld를 투표 멤버로 두면 따로 만들 프로토콜이 없다(vSAN 위트니스와 같은 모양). 선거에 나서지 않게 해(openraft `enable_elect = false`) 리더 일(감시, 정리, HA 복구)은 늘 일반 노드가 한다.
- **복구 지휘: 리더 루프(채택) vs 대상 노드의 Task.** 펜싱 판단(리스 표)과 배치가 리더에 있으므로 리더가 revision 검사로 `spec.node`를 바꾼다. 마이그레이션 확정과 같은 방식이다.

## 2. 위트니스 (계획 22)

- 설정 `witness = true`(opld.toml 최상위, 기본 false)인 opld는 위트니스다. `[hypervisor]`·`[pools]`·`[migration]`·`[ha]`는 쓰지 않고 QEMU를 찾지 않는다.
- **합류만 된다.** `oplctl cluster init`은 위트니스에서 거부한다("a witness cannot start a cluster"). `oplctl cluster join`은 지금과 같고, 합류 요청에 `witness: true`가 실린다. 받는 쪽은 Node를 `spec.witness = true`로 만든다. 승격까지 일반 노드와 같다(learner → voter).
- **선거에 나서지 않는다.** 위트니스의 Raft 설정은 `enable_elect = false`다. 투표는 하지만 후보가 되지 않으므로 리더가 되지 않는다. 그래서 리더만 하는 일(노드 감시, 정리, 예약 스냅샷, HA 복구)을 띄우지 않는다.
- **도는 것:** 클러스터 저장소, 노드 간 서버(Raft RPC, 리더 전달, Ping — 하이퍼바이저 없음), 승격, Node 등록. 이 노드는 HTTPS API, reconciler, Task 실행기, 풀 마운터, 받는 마이그레이션을 띄우지 않는다. Node 등록은 `online = true`와 아키텍처만 기록하고, cpus·메모리·하이퍼바이저·브리지·풀은 비운다.
- **배치에서 빠진다.** `node_fits`는 위트니스를 "witness node"로 거부한다. 따라서 생성·시작·마이그레이션 대상·Task 노드 선택·HA 복구 대상에서 빠지고, API가 VM의 노드로 위트니스를 받으면 409 `node_unavailable`이다. 풀의 노드 목록과 공유 풀 확인(모든 노드에서 준비됐는지)도 위트니스를 세지 않는다.
- **상태 표시:** `oplctl cluster status`와 `oplctl node list`, 웹 노드 목록에 역할(위트니스)을 보인다. 2노드 경고는 지금처럼 투표 멤버 수가 2일 때다. 일반 노드 둘과 위트니스면 3이므로 경고가 없다. 투표 멤버가 3 이상이어도 일반 노드가 하나뿐이면 "HA needs at least two non-witness nodes"를 보인다.
- 탈퇴·제거는 일반 노드와 같다(배정된 VM이 없다).

구현: 설정은 opld `Config.witness`다. 쓰지 않는 `[hypervisor]`도 읽고 검사는 그대로 거친다(`accel`·`systemd_scope`의 운영체제 규칙 — 잘못된 값이면 시작하지 않는다. `qemu_binary`는 찾지 않으므로 없는 경로여도 된다). `cluster init`은 제어 소켓에서 409 "a witness cannot start a cluster; join an existing cluster instead"(`member::WITNESS_CANNOT_INIT`)이고, init/join 전 포트 검사는 `cluster_listen`만 본다. 멤버로 돌 때 `Member`의 `services`(HTTPS API, reconciler, Task 실행기, 노드 감시, 예약 스냅샷, 정리, 풀 마운터)가 없고 `Host::from_config`·`QemuHypervisor::new`를 부르지 않는다. 노드 간 서버는 `PeerService::new(cluster, None)`이라 마이그레이션·UEFI 변수를 받지 않고, 브리지 새로 고침과 마이그레이션 TLS 파일 쓰기도 없다. Raft 설정은 `raft_config(witness)`의 `enable_elect: !witness`다. Node 등록은 `bootstrap::register_witness`(`online`, `arch`만)이고, 다시 시작했을 때는 일반 노드처럼 뒤에서 될 때까지 다시 해 본다. `spec.address`는 일반 노드처럼 `address:listen 포트`이지만 그 포트는 열리지 않는다(`node list`·`cluster status`의 `ADDRESS`도 이 값이다). 다시 시작할 때 로컬 저장소의 이 노드 Node `spec.witness`가 설정의 `witness`와 다르면 아무것도 띄우지 않고 실패한다(`bootstrap::check_role`). 탈퇴는 빠진 뒤 남는 투표 멤버가 없거나 모두 위트니스면 거절한다(`member::check_leave`, "the remaining voting members are all witnesses, which cannot lead; add another non-witness node first") — 위트니스만 남으면 리더가 생기지 않는다. 역할은 합류 요청의 `witness`로만 정해지고 `PUT /nodes/{key}`는 저장된 값을 그대로 둔다(요청 값 무시). 배치에서 빼기는 `node_fits`("node {name:?} is a witness node")와 `opl_types::storage::pool_usable_on`(위트니스면 false, 이유 "witness nodes do not use storage pools")에 모았다 — 스케줄러, Task 노드 선택, 복제, 공유 풀 확인이 함께 막힌다. VM 생성·수정의 `spec.node`와 마이그레이션 대상은 `vms.rs`의 `check_not_witness`가 409 `node_unavailable`로 거절한다. 위트니스를 이름으로 적은 풀 `spec.nodes`는 받아들인다(쓰이지 않을 뿐 해가 없다). 웹·oplctl의 풀 노드 목록, 웹 VM 폼의 노드 목록·기본 머신 종류, 마이그레이션 대상 목록도 위트니스를 뺀다. 표시: `GET /cluster`의 members에 `witness`, oplctl `node list`·`cluster status`에 `WITNESS` 열(`true`/`false`), `node get`은 JSON의 `spec.witness`다. HA 경고는 투표 멤버가 3 이상이고 위트니스가 아닌 투표 멤버가 2 미만일 때다. 웹은 노드 목록, 클러스터 멤버, 대시보드 노드 표에 "위트니스" 배지를 붙이고, 대시보드의 노드 수는 위트니스도 센다.

## 3. HA 리스와 자기 펜싱 (계획 23)

### 3.1 VM과 노드의 HA 설정

- `VmSpec.ha: bool`(기본 false). HA VM이 원하는 상태가 running인데 그 노드가 펜싱됐으면 다른 노드에서 켠다(§4). 같은 노드에서의 크래시 재시작은 지금처럼 `restart`가 정한다(HA VM에는 `on-failure`를 권한다). 게스트가 스스로 끈 VM은 원하는 상태가 stopped가 되므로 복구하지 않는다.
- 노드 설정 `[ha]`:

| 키 | 기본 | 뜻 |
|---|---|---|
| `fencing` | Linux `watchdog`, macOS `off` | `watchdog`(아래 둘 다), `process`(opld가 HA VM을 끄는 것만 — watchdog 없음, 시험·개발용), `off`(이 노드는 HA VM을 돌리지 않는다) |
| `watchdog_device` | `/dev/watchdog` | Linux watchdog 장치(하드웨어 또는 `softdog`) |
| `watchdog_timeout_secs` | 30 | 장치에 정하는 제한 시간(`WDIOC_SETTIMEOUT`) |
| `fence_after_secs` | 30 | 마지막으로 성공한 리스 갱신 뒤 이만큼 지나면 HA VM을 끈다 |

- 노드는 등록할 때(`register_node`) `NodeStatus.ha: Option<NodeHa>`를 쓴다. `fencing`이 `off`면 None이다. `NodeHa { fencing: Watchdog | Process, recover_after_secs }`이고, `recover_after_secs = fence_after + (watchdog면 watchdog_timeout, 아니면 0) + 5`(여유)다. 리더는 이 노드의 값을 써서 옮길 때를 정한다.
- 갱신 간격은 `fence_after / 10`이다(0.2초 이상 2초 이하).
- **HA 가능 노드** = 위트니스가 아니고 `status.ha`가 있는 노드. API 검사:
  - HA VM을 만들거나 바꿀 때 `spec.node`가 HA 가능 노드가 아니면 409 `ha_unsupported`.
  - 노드가 없는 HA VM을 시작할 때는 스케줄러가 HA 가능 노드만 고른다(없으면 409 `no_capacity`).
  - HA VM의 마이그레이션 대상이 HA 가능 노드가 아니면 409 `ha_unsupported`.
  - **HA는 멈춘 VM에서만 켠다**(계획 24에서 더한 규칙 — 처음에는 "실행 중인 VM의 `ha`는 바꿀 수 있다"였다). 수정에서 `ha`를 false → true로 바꾸는 것은 VM이 멈춰 있을 때만이다(원하는 상태 stopped이고 phase `Stopped`·`Failed`). 아니면 409 `vm_running`. 끄기는 언제든, 생성 때 `ha: true`는 그대로 된다. 이유: 노드 N이 과반수와 끊긴 동안 다수 쪽이 N에서 실행 중인 VM의 HA를 켜면 N은 그 기록을 받지 못해 HA인 줄 모르고 펜싱하지 않는데, 리더는 N의 리스가 끝난 뒤 그 VM을 복구해 둘이 함께 돈다. 멈춘 VM이면 N이 그것을 켜는 기록(`power = Running`)은 HA 기록 뒤에 있으므로, N은 둘을 함께 보거나(HA VM으로 리스를 보고 켠다) 둘 다 보지 못한다(켜지 않는다).

구현(계획 23): 타입은 `opl_types::node`의 `NodeHa`, `Fencing { Watchdog, Process }`(`snake_case`), `ha_capable(&Node)`이고 `NodeStatus.ha`는 없으면 빼고 쓴다(OpenAPI에서도 선택 필드). opld 설정은 `Config.ha: HaSection`(`FencingSetting { Watchdog, Process, Off }`, `deny_unknown_fields`)이고, Linux가 아니면 `watchdog`는 설정 오류다. 검증은 `fence_after_secs ≥ 1`(펜싱이 꺼져 있어도), watchdog면 `watchdog_timeout_secs`가 **5..=3600**이다(1초마다 쓰다듬으므로 너무 짧으면 한 번만 늦어도 재시작된다). `HaSection::node_ha()`·`renew_interval()`·`guard_config()`가 등록 값·갱신 간격·`HaGuard` 설정을 만든다. API 검사는 `vms::check_ha_capable`(생성·수정의 `spec.node`, 마이그레이션 대상 — 마이그레이션은 `node_fits`보다 먼저)이고, 스케줄러는 `node_fits`가 HA VM에 "node {name:?} does not support HA (fencing is off)"로 HA 가능 노드만 남긴다. **HA 가능 노드가 없을 때의 오류는 설계의 `no_capacity`가 아니라 기존 스케줄러 오류인 409 `no_node`다**(`no_capacity`는 마이그레이션 대상 검사의 코드다). 실행 중 VM의 `ha` 수정은 원래 막는 필드가 아니라 바꿀 것이 없었다. 스냅샷 되돌리기는 예약처럼 지금 VM의 `ha`를 지킨다(`write_reverted_spec`).

구현(계획 24, 새 규칙): `vms::update_vm`이 `prepare`와 노드·디스크 검사 뒤 `spec.ha && !current.spec.ha && !is_stopped(&current)`면 409 `vm_running`("cannot turn on HA for VM \"web\" while it is running; turn on HA while the VM is stopped")이다. 멈춤 판정은 노드·디스크 변경과 같은 `is_stopped`다. oplctl `vm set --ha`는 서버 문구를 그대로 보이고(다시 시도하지 않음) 도움말에 "VM이 멈춰 있어야" 한다고 적었다. 웹 VM 편집은 실행 중이고 HA가 꺼진 VM이면 HA 스위치의 켜기를 막고 이유(`vmForm.haRunning`, "HA는 VM이 멈춰 있을 때 켤 수 있습니다.")를 보인다(`GeneralSection`의 `haLocked`). 시험에서 실행 중인 HA VM이 필요하면 만들 때 `ha: true`로 하거나 저장소에 직접 쓴다(`migrate.rs`).

### 3.2 리스

- **갱신(노드):** HA 가능 노드의 `HaGuard`가 갱신 간격마다 `LeaderRequest::RenewLease { node }`를 보낸다(리더면 자기에게, 아니면 기존 전달 경로). 한 번의 제한은 갱신 간격의 두 배다. 보내기 직전의 시각 s를 재고, 성공하면 `last_renewed = s`. 응답은 리더가 적용한 로그 인덱스이고, 노드는 그 인덱스까지 자기 저장소에 적용된 뒤에 성공으로 친다. 그러면 리스가 다시 살아난 노드는 그 사이 리더가 옮긴 VM을 이미 다른 노드의 것으로 본다(§4.2).
- **처리(리더, opl-cluster):** `ensure_linearizable`로 아직 과반수의 리더인지 확인한다. 그 노드가 펜싱 중(§4.2)이면 `Rejection::Fenced`로 거부한다. 아니면 리스 표에 (현재 임기, 받은 시각 r)을 적고 적용한 인덱스를 돌려준다. 리스 표는 메모리에만 있고 임기가 바뀌면 비운다.
- **안전 조건:** 갱신이 성공했다면 s ≤ r이다(노드는 보내기 전에 쟀다). 노드는 s + fence_after 안에 HA VM을 끄고, 그러지 못해도 watchdog가 마지막 쓰다듬기 + watchdog_timeout 안에 노드를 재시작한다. 노드는 리스가 살아 있을 때(now − s < fence_after)만 쓰다듬는다. 리더는 r + recover_after가 지난 뒤에만 옮긴다. 따라서 옮긴 VM이 켜질 때 원래 노드의 VM은 꺼져 있다. 새 리더는 리더가 된 것을 처음 본 시각 b부터 센다(max(b, r)). 이전 리더가 확인해 준 갱신은 모두 b 전에 일어났다. 새 임기의 과반수가 투표한 뒤에는 이전 리더의 `ensure_linearizable`가 과반수의 응답을 받을 수 없기 때문이다.

구현(계획 23): 리스 표는 `opl-cluster`의 `lease.rs`(`LeaseTable`, `Cluster` 안의 메모리 `Mutex`)다. `LeaderRequest::RenewLease { node }`(전송 형식 `renew-lease`)는 로그에 아무것도 쓰지 않는다. 리더는 `ensure_linearizable`를 **두 번** 한다. 첫 번째로 과반수의 리더인지 확인하고, 같은 잠금 안에서 임기가 그대로인지·펜싱 중인지 본 뒤 (임기, `tokio::time::Instant`)를 적고, 두 번째 확인의 커밋 인덱스를 돌려준다(펜싱 표시를 지우기 전에 끝난 복구 쓰기가 모두 들어 있다. 지표의 적용 인덱스는 쓰기 응답보다 늦게 바뀔 수 있어 쓰지 않는다). 노드 쪽은 `Cluster::renew_lease(timeout)`이고 그 인덱스가 로컬에 적용돼야 성공이다. 리더 쪽 조회는 `Cluster::leases() -> Leases { term, renewed }`(리더에서만 의미가 있고, 표가 더 낮은 임기의 것이면 비운다)다. `Rejection::Fenced { node }`는 opl-api에서 409 `node_fenced`이지만 API 요청으로는 생기지 않는다. 시험 `opl-cluster/tests/lease.rs`(`MemRouter::register(id, &Cluster)`, `MemRouter::block_raft(id)`).

### 3.3 노드 쪽 펜싱 (`HaGuard`, opl-reconciler)

- **상태:**
  - 프로세스 시작 시각을 기준점으로 둔다. 이 덕분에 opld를 다시 시작한 직후 바로 끄지 않는다.
  - `renewed`: 이 프로세스에서 한 번이라도 갱신에 성공했는지.
  - `fresh()`: `renewed`이고 now − last_renewed < fence_after.
  - `stale()`: now − max(시작, last_renewed) ≥ fence_after.
  - fresh/stale이 바뀌면 `watch`로 알린다.
- **워커 규칙**(HA VM만, 원하는 상태가 running일 때):
  - 켜기(`start()`)와 넘겨받은 마이그레이션의 `cont` 전에 `fresh()`이고 watchdog가 켜져 있어야 한다(`arm()`). 아니면 실패로 세지 않고 `retry_interval` 뒤 다시 본다. status: `Starting`, "waiting for the cluster majority before starting an HA VM". 이 노드에 펜싱이 없으면(`fencing = off`) 켜지 않는다. status: "HA is not available on this node".
  - 실행 중인데 `stale()`이면 강제 종료한다. 우리가 요청한 종료로 표시하므로 실패로 세지 않고 원하는 상태도 바꾸지 않는다. status: "fenced: lost the cluster majority". 리스가 다시 살아나고 `spec.node`가 여전히 이 노드면 다시 켠다.
  - reconciler는 stale로 바뀌면 모든 워커를 깨운다.
  - 펜싱이 꺼진 노드에서 이미 도는 HA VM(설정을 바꾸고 다시 시작한 경우)은 끄지 않는다. status에 그 사실을 남기고, 리더도 그 노드에서는 옮기지 않는다(`status.ha`가 없다).
- **watchdog**(`fencing = watchdog`):
  - 이 노드에서 HA VM을 켜거나 이미 도는 HA VM에 붙을 때 처음 연다(`arm()`). `renewed` 전에는 열지 않는다. 장치를 열면 타이머가 다시 시작되므로, 이전 프로세스가 켜 둔 타이머를 새 리스 없이 늘리면 안 되기 때문이다.
  - 연 뒤에는 `WDIOC_SETTIMEOUT`을 하고 1초마다 쓰다듬는다. 단, 리스가 살아 있거나 이 노드에 도는 HA VM이 없을 때만이다. 워커가 도는 HA VM을 알린다.
  - 닫지 않고 opld가 끝날 때까지 쥔다. 정상 종료 때 도는 HA VM이 없으면 `V`를 쓰고 닫는다(magic close). 있으면 그대로 두어 opld가 watchdog_timeout 안에 돌아오지 않으면 노드가 재시작된다(경고 로그).
  - 장치를 열지 못하면(없음, 다른 프로세스가 쥠 — 예: sanlock `wdmd`) HA VM을 켜지 않는다. status에 오류를 남긴다.
  - 커널이 `nowayout`이면 magic close가 듣지 않는다. 그런 장치는 쓰지 말라고 문서에 적는다.
- **`process`:** watchdog 없이 워커가 끄는 것만 한다. opld가 멈추거나 죽으면 아무것도 끄지 않으므로 안전하지 않다. 시험과 개발에만 쓴다. 등록하는 `NodeHa.fencing`이 `Process`라 상태 표시에 드러난다.

구현(계획 23): `opl-reconciler`의 `ha.rs`. `HaGuard::spawn(renewer: Arc<dyn LeaseRenewer>, HaConfig { fence_after, renew_interval, watchdog })`의 spawn 시각이 기준점이고, 갱신 루프(한 번은 간격의 두 배까지)와 상태 알림·쓰다듬기 루프가 뒤에서 돈다. `state() -> LeaseState { fresh, stale }`, `fresh()`, `stale()`, `subscribe()`(`watch`), `arm()`, `set_running(vm, bool)`, `retain_running(ids)`(그 밖의 VM을 뺀다), `close()`. `arm()`은 **같은 잠금 안에서 지금 fresh인지 확인한다**(`renewed`만이 아니다. 아니면 `ArmError::NotFresh`, 장치를 열지 못하면 `ArmError::Open`, 쓸 수 없는 장치면 `ArmError::Rejected`) — `process`도 리스를 본다. watchdog는 trait(`Watchdog { ping, magic_close, rejected }`, `WatchdogOpener`)이고 장치 구현은 `DeviceWatchdog { path, timeout_secs }`다(rustix로 `O_WRONLY|O_CLOEXEC`, `WDIOC_SETTIMEOUT`, 쓰다듬기는 `write(b"\0")`, Linux가 아니면 열기가 `Unsupported`). `WDIOC_SETTIMEOUT`이 실패하면 `WDIOC_GETTIMEOUT`으로 지금 값을 읽어 요청 이하면 받아들이고, 장치가 요청보다 긴 값을 고르거나 받아들일 수 없으면(`accept_timeout`) **fd를 닫지 않고** "쓸 수 없는 장치"(`rejected()`가 이유)로 돌려준다. 열면 타이머가 돌므로 그냥 닫으면 나중에 노드가 재시작되기 때문이다. guard는 그 장치를 쥔 채 평소 규칙대로 쓰다듬고, `arm()`은 다시 열지 않고 `Rejected`로 실패하며(HA VM을 켜지 않는다), `close()`는 도는 HA VM이 없으면 magic close한다. 처음 열기 자체가 fresh일 때만이므로 이전 프로세스의 타이머를 리스 없이 멈추는 일은 없다. 짧은 값은 경고만 남긴다. `LeaseRenewer`는 `Cluster`가 구현한다. opld는 일반 노드에서 reconciler 앞에서 guard를 만들어 `ReconcilerConfig.ha`에 넣고(`off`·위트니스는 None), 정상 종료 때 reconciler가 멈춘 뒤 `close()`한다.

구현(계획 23, 워커): 켜기 문턱은 `fresh()`+`arm()`이고 멈춘 QEMU를 이어서 실행하는 모든 경로(받은 마이그레이션, 멈춘 채 남은 VM, `postmigrate` 원본)에 같다. 아니면 실패로 세지 않고 `retry_interval` 뒤 다시(status `Starting`, "waiting for the cluster majority before starting an HA VM", 펜싱 뒤면 "fenced: lost the cluster majority", 열기 오류면 그 메시지). 문턱과 실제 시작 사이의 틈을 막으려고 HA VM은 느린 시작 과정(Starting 기록, 디스크 해석·활성화, UEFI 변수 받기) 전에 `set_running(true)`(못 켜면 false로 되돌린다)로 쓰다듬기를 막고, `hv.start`(새로 켜기와 다룰 수 없는 QEMU에 다시 붙는 `reattach`) 바로 앞에서 `fresh()`와 로컬 저장소의 VM을 다시 본다(`ha_recheck`: `spec.node`가 이 노드이고 원하는 상태가 running). 리스가 살아 있으면 그 갱신 인덱스까지 적용되어 있으므로 `process`에서 문턱 뒤 리스를 잃었다 되찾은 사이의 복구 쓰기를 본다. 아니면 켜지 않고(디스크를 끈다) status를 쓰지 않은 채 다시 맞춘다. 저장된 phase가 Running인 채 다시 켤 때 HA VM은 Starting 기록이 실패하면 "restarting anyway"하지 않고 다시 시도하며(과반수가 없으면 계획 24의 리더가 다른 노드에서 켤 수 있다), 기록 뒤 다시 읽은 VM이 다른 노드로 배정됐으면(HA가 아니어도) 켜지 않는다. stale이면 강제 종료(우리가 요청한 종료, 세지 않음, 원하는 상태가 stopped여도). 이미 도는 HA VM에 붙을 때 `arm()`하고 `NotFresh`는 조용히 넘긴다. 배정되지 않은 VM(지워짐·다른 노드로 옮김)과 물러나는 워커는 `hv.inspect`로 QEMU가 없을 때만 목록에서 빼고, 목록에 있던 VM이 stale이면 끈다. reconciler 종료 때는 목록을 그대로 둔다(`close`가 본다). 옮겨 간 HA VM의 원본처럼 워커가 물러난 뒤 멈춘 QEMU는 디스패처가 정리한다: 작업자 없는 VM의 종료 이벤트에 `set_running(false)`, 주기 재동기화 때 하이퍼바이저 목록·작업자에 없는 VM을 `retain_running`으로 뺀다. 낡은 `postmigrate` 원본 판단의 예외(status.node가 이 노드이고 phase가 Running·Migrating·Starting)는 HA 여부를 보지 않는다(리스를 기다리는 동안 HA를 꺼도 원본을 끄지 않는다). 디스패처는 guard의 바뀜 알림에 모든 워커를 깨운다. 펜싱이 꺼진 노드에서 멈춘 HA VM은 phase `Stopped`, "HA is not available on this node"로 켜지 않는다. 마이그레이션 되돌리기(`tasks/migrate.rs`)는 HA VM 원본을 직접 `resume`하지 않고 보류가 풀린 뒤 워커가 문턱을 거쳐 이어서 실행한다. 시험 `opl-reconciler/tests/ha.rs`, `src/ha.rs` 단위 시험(멈춘 시계).

## 4. 복구 (계획 24)

### 4.1 리더의 `HaManager` (opl-api, 리더만)

1초마다:

1. 리더가 아니면 리스 표와 기준점을 잊는다. 리더면 `read_barrier`를 한다.
2. 리더가 된 뒤 처음이면 기준점 b = 지금으로 둔다.
3. 펜싱할 노드를 고른다. 아래를 모두 만족하는 노드 N이다.
   - 이 노드가 아니다.
   - `status.ha`가 있다.
   - now − max(b, r_N) ≥ recover_after_N.
   - `spec.node == N`, `spec.ha`, `spec.power == Running`인 VM이 있다.
4. N마다 리스 표에 펜싱 중 표시를 한다(§4.2). 그 VM들을 이름 순서로 처리한다.
   - **대상 고르기:** 스케줄러로 고른다. 후보는 HA 가능 노드이고, 리스가 살아 있고(now − max(b, r) < recover_after), 유지보수가 아니고, `node_fits`를 통과하며, N과 `status.migration.node`가 아닌 노드다. 받는 쪽 QEMU와 같은 실행 디렉터리를 쓰지 않게 하려는 것이다.
   - **status를 먼저 쓴다**(revision 검사). `phase: Stopped`, `node: None`, `pid: None`, `migration: None`, `last_error: "recovering: node {N} failed"`로 쓰고 `firmware_node`는 그대로 둔다.
   - **그 다음 `spec.node = T`를 쓴다**(revision 검사). 사이에 리더가 바뀌면 새 리더가 다시 한다(아직 `spec.node == N`).
   - 그 VM을 대상으로 하는 N의 끝나지 않은 Task를 실패로 기록한다("node {N} failed; the VM was recovered by HA"). 그러지 않으면 `vm_busy`가 풀리지 않는다.
   - 기록 Task `VmHaRecover { from: N }`(대상 VM, 노드 T)를 끝난 상태(Succeeded)로 만든다. 실행기는 이것을 실행하지 않는다.
   - 대상이 없으면 status(`last_error: "HA cannot find a node for this VM"`)만 쓰고 다음 틱에 다시 본다.
5. 펜싱 중 표시를 지운다.

T의 reconciler는 `spec.node == T`가 되면 평소처럼 새로 켠다(UEFI 변수는 계획 21대로 N에서 받아 보고, 안 되면 T의 파일이나 템플릿).

구현(계획 24): `opl-api`의 `ha.rs`. `HaManager::spawn(state, HaManagerConfig { tick })`(기본 1초)·`shutdown()`이고, opld가 위트니스가 아닌 모든 노드에서 `ClusterJanitor`·`SnapshotScheduler`처럼 띄운다(`Services.recovery`, 정리 작업 뒤에 멈춘다. 이 노드의 펜싱 설정과 상관없다). 틱마다 리더가 아니면 기준점을 잊고, 리더면 `read_barrier` 뒤 `leases()`를 읽는다. 기준점 b는 `base_at`(순수 함수 — (임기, 처음 본 시각)을 쥐고 임기가 바뀌면 다시 잡는다)이고 노드의 리스 나이는 now − max(b, r)(리스가 없으면 b). 펜싱할 노드마다 `begin_fence` → 같은 임기인지와 `leases()`로 여전히 만료인지 다시 확인 → VM을 이름 순서로 `recover_vm`. 대상은 `node_fits`(유지보수·오프라인·위트니스·HA 가능·아키텍처·풀·브리지) + 리스가 살아 있음 + N과 `status.migration.node`가 아님이고, 스케줄러처럼 미할당 메모리가 가장 많은(같으면 id가 작은) 노드다. **같은 틱에 앞서 옮긴 VM의 메모리를 센다**(메모리의 `Inventory`를 고친다). 쓰기는 status(`UpdateStatusIf`, 읽은 revision) → spec(`UpdateSpec`, status 쓰기가 돌려준 revision) 순서이고, **Task 실패는 spec 쓰기가 성공한 뒤 Task 목록을 다시 읽어** 한다(그 사이 만든 N의 Task도 본다). Task 실패 쓰기가 다른 쪽과 엇갈리면 다시 읽어 끝나지 않았을 때만 3번까지 다시 하고, 그래도 안 되면 로그만 남긴다. 기록 Task는 이름이 id이고 상태가 처음부터 `Succeeded`다. 대상이 없으면 `last_error`가 이미 그 문구가 아닐 때만 status를 쓴다. revision 불일치(`Rejected`)는 그 VM만 넘기고 다음 틱에 다시 보며, 그 밖의 오류는 그 노드의 나머지를 다음 틱으로 미룬다. 로그는 `tracing::warn!`("recovering HA VMs from a failed node", VM 이름 목록).

- **쓰기 경로:** 복구 쓰기는 `Cluster::execute_as_leader`로만 한다(다른 노드로 전달하지 않는다 — 펜싱 표시는 이 노드의 리스 표에만 있기 때문이다. 리더를 잃으면 `NotLeader`). 시험에서 바꿔 끼우도록 `RecoveryWriter` trait(`write(&Cluster, Command) -> Result<u64, ClusterError>`, 결과를 모르는 쓰기를 판정하는 임기 `term(&Cluster)` — 기본 `leases().term`, 시험이 임기 변경을 흉내 낸다)로 감싸고 `HaManager::spawn_with_writer`가 받는다.
- **펜싱 표시 해제:** `Fence` guard가 Drop에서 `end_fence`한다(오류, 리더를 잃음, `?`로 빠져나감 포함). 예외는 **결과를 모르는 쓰기**다. `Ok`·`Rejected`·`NotLeader`가 아닌 결과(`Timeout` 등)는 리더 로그에 남아 나중에 커밋될 수 있으므로 펜싱을 풀지 않고 그 노드를 `Unknown { term, vm, command, moved_to }`로 기억한다. 다음 틱부터 임기가 바뀌었으면(이 노드가 여전히 리더든 아니든) 푼다 — 새 임기 리더의 커밋 인덱스는 살아남은 옛 항목을 포함하고 노드의 첫 갱신은 그 인덱스까지 적용한 뒤에 성공한다. spec 쓰기였으면 남은 단계 목록(`Moved`)으로 옮겨, 이 노드가 리더인 틱마다(장벽 뒤) VM을 다시 읽어 `spec.node`가 그 대상이면 남은 단계(Task 실패, 기록 Task)를 하고 지우며, 다른 값이거나 VM이 없으면(쓰기가 버려졌거나 다른 리더가 끝냈거나 사용자가 옮김) 그냥 지운다. `NotLeader`는 결과를 아는 것으로 친다 — 항목이 로그에 들어갔을 수 있지만 리더를 잃었다는 것은 임기가 바뀐다는 뜻이고, 다음 임기의 리더는 그 항목을 커밋하거나 버린 뒤에야 갱신을 확인해 주므로 N은 그 결과를 본 뒤에야 fresh가 된다. 같은 임기면 같은 revision 검사 쓰기를 다시 보내(멱등이다. `Ok`면 이번에 적용됐고 `Rejected`면 앞의 것이 커밋됐거나 다른 쓰기가 이겼다) 결과를 알 때까지 펜싱을 쥐고, 알면 spec 쓰기였을 때 VM이 그 노드로 가 있으면 남은 단계(Task 실패, 기록 Task)를 한 뒤 푼다. 그사이 그 노드는 새 복구 대상에서 빠진다. 루프가 종료로 쓰기 도중에 취소되면 Drop에서 풀지 않는다(리더 자리는 곧 없어지고 새 리더는 빈 표에서 다시 센다).
- **설계와 다른 점:** 리스 표를 잊는 것은 `Cluster::leases()`가 한다(임기가 다른 표는 비운다). `HaManager`는 기준점만 잊는다.

### 4.2 펜싱 중 표시

리더가 N을 펜싱하기로 정한 뒤 쓰기가 끝나기 전에 N의 갱신이 도착하면 N이 낡은 `spec.node`를 보고 VM을 켤 수 있다. 그래서 리스 표에 `fencing: set<NodeId>`를 두고, 그 사이 N의 갱신을 `Fenced`로 거부한다. 표시를 지운 뒤의 갱신은 복구 쓰기를 포함하는 인덱스를 돌려준다(§3.2). N은 그것을 적용한 뒤에야 fresh가 되므로 옮겨진 VM을 켜지 않는다. 다른 임기에서 쓴 복구도 새 리더의 적용 인덱스에 들어 있다.

구현(계획 24): 위의 펜싱 표시를 `HaManager`가 쓴다(§4.1 구현 — 결과를 모르는 쓰기가 있으면 풀지 않는다).

구현(계획 23): 펜싱 표시는 `Cluster::begin_fence(node)`·`end_fence(node)`로 리스 표의 펜싱 집합에 넣고 뺀다(API와 시험만, 쓰는 것은 계획 24의 `HaManager`). 펜싱 검사와 갱신 기록이 한 잠금 안이라 `begin_fence` 뒤에는 그 노드의 새 갱신이 기록되지 않는다. 집합은 **임기가 바뀌어도 지우지 않는다**(지우면 이전 임기에 시작한 복구 쓰기가 끝나기 전에 그 노드가 리스를 받을 수 있다). 그래서 `HaManager`는 모든 경로에서 `end_fence`해야 하고, `begin_fence` 뒤 `leases()`를 다시 읽어 아직 만료인지 확인해야 한다.

### 4.3 이미지와 디스크 잠금

죽은 노드가 쥐고 있던 잠금은 곧바로 풀리지 않을 수 있다.

- **공유 LVM(sanlock 임대가 끝날 때까지):** 이미 일시 오류(`PoolUnavailable`)라 세지 않고 다시 시도한다.
- **NFS(QEMU의 OFD 잠금, NFSv4 임대 90초):** QEMU 시작이 `Failed to get "write" lock`으로 실패하면 새 `DriverError::ImageLocked`로 구분한다(QEMU stderr). 워커는 이것을 실패로 세지 않고 `retry_interval`마다 다시 시도한다. status: "disk image is locked by another process; retrying". 이 규칙은 HA가 아닌 VM에도 적용된다. 전에는 곧바로 `Failed`였다. 사용자는 정지로 멈출 수 있다.

구현(계획 24): `DriverError::ImageLocked(String)`(내용은 QEMU stderr, 표시 "disk image is locked by another process: …", `is_transient()`가 true). `qemu.rs`의 `image_locked(stderr)`가 시작 실패 stderr에 `"write" lock`(`Failed to get "write" lock`·`Failed to get shared "write" lock`) 또는 `Is another process using the image`가 있으면 그렇게 본다 — QEMU 8.2와 11의 file-posix 문구가 같다(실제 QEMU 두 개로 같은 qcow2를 여는 `qemu-it` 시험으로 확인). 워커는 스냅샷 복원으로 켜는 중이 아닐 때 `ImageLocked`를 `record_failure` 없이 `retry_interval`(기본 2초) 백오프로 두고 status `Starting`, "disk image is locked by another process; retrying"을 쓴다(복원 시작의 오류는 지금처럼 복원 실패로 끝낸다). 원하는 상태가 running인 동안 끝없이 다시 시도한다. 시험용으로 `MockHypervisor::fail_next_starts(errs)`(다음 시작들을 차례로 그 오류로 실패시킨다)를 더했다.

구현(계획 24, 워커 보강 — 실제 시험에서 찾음): 낡은 로컬 저장소로 다시 시작한 N의 워커가 옮겨 간 VM의 status를 쓰려다 revision 불일치로 다시 읽으면, 전에는 배정을 보지 않고 다시 썼다. 이제 `write_status`의 재시도는 다시 읽은 VM의 `spec.node`가 이 노드가 아니면(지워진 경우처럼) 쓰지 않고 멈춘다.

### 4.4 그 밖의 경우

- **N이 돌아온 경우:** reconciler는 배정되지 않은 VM을 지금처럼 건드리지 않는다. 펜싱됐다면 그 VM은 이미 꺼져 있다. N의 Task 실행기는 실패로 기록된 Task를 다시 실행하지 않는다.
- **HA 아닌 VM:** 그대로 둔다. 지금처럼 N에 묶여 있다.

구현(계획 24): 실제 시험에서 N(A)을 다시 띄우면 HA VM은 B에 배정된 채 같은 QEMU로 계속 돌고, A는 그 VM을 켜지 않으며, HA 아닌 VM은 A에 배정된 채다. 끝나지 않은 N의 Task 가운데 복구한 VM을 대상으로 하지 않는 것은 건드리지 않는다.
- **마이그레이션 중:** N이 원본이면 확정 전에는 `spec.node == N`이고 위대로 복구한다. 받던 노드는 `status.migration`이 지워졌으므로 다음 정리에서 받은 QEMU를 끈다(계획 20 규칙). N이 대상이면 원본의 Task가 실패를 처리한다.

## 5. API·oplctl·웹

- **API**
  - `VmSpec.ha`: 생성·수정에 쓰고 OpenAPI에 보인다.
  - 오류 `ha_unsupported`(409).
  - `NodeSpec.witness`, `NodeStatus.ha`.
  - Task 종류 `vm_ha_recover`. (구현: 선 위 이름은 다른 Task 종류처럼 케밥 `vm-ha-recover`다.)
- **oplctl**
  - `vm create --ha`, `vm set VM --ha|--no-ha`(기존 VM 수정 API).
  - `vm list`·`vm get`에 HA 표시.
  - `node list`·`cluster status`에 역할(위트니스)과 펜싱(`watchdog`·`process`·`-`)을 보인다.
- **웹**
  - VM 만들기·설정에 HA 스위치를 두고, 그 노드가 HA 가능하지 않으면 이유와 함께 막는다.
  - VM 목록과 상세에 HA 표시.
  - 노드 목록에 위트니스 표시와 펜싱 열.
  - Task 목록에 "HA 복구 (노드 {from}에서)".
  - 문구는 한국어와 영어 둘 다 둔다.

구현(계획 23): `GET /cluster`의 멤버에 `fencing`(`watchdog`·`process`, 꺼졌거나 Node가 아직 없으면 null)을 더했다. oplctl `vm set VM --ha|--no-ha`는 VM을 읽어 그 revision으로(`If-Match`) spec 전체를 다시 쓴다(예약 스냅샷 바꾸기와 같은 `put_spec_field`). `vm list`의 HA 열은 `true`/`false`, `vm get`은 JSON의 `spec.ha`다. 웹은 VM 폼의 HA 스위치(`haUnavailable` — 노드가 HA 가능하지 않으면 켜기만 막고 이미 켜진 HA는 끌 수 있다), VM 목록·상세의 HA 배지, 노드·클러스터·대시보드 노드 표의 "펜싱" 열, `errors.ha_unsupported`다. 마이그레이션 대화상자는 HA 불가 대상을 거르지 않는다(서버가 409). `vm_ha_recover`와 Task 문구는 계획 24다.

구현(계획 24): `TaskOp::VmHaRecover { from: Uuid }`이고 JSON은 `{"kind": "vm-ha-recover", "from": …}`이다(설계의 `vm_ha_recover`가 아니다 — `TaskOp`는 `kebab-case`로 태그된다). OpenAPI와 `web/src/api/schema.d.ts`에 보인다. Task의 `node`는 옮긴 노드, `target`은 VM이다. oplctl `task list`의 OP 열은 `vm-ha-recover (from <죽은 노드 id>)`(`task_row`), 웹 Task 목록은 `describeTask(task, t, target, nodeName)`으로 `tasks.op.vm-ha-recover` "{{target}} HA 복구 (노드 {{from}}에서)"/"HA recovery of {{target}} (from node {{from}})"이고 노드 이름을 모르면 id를 보인다. HA 켜기 규칙의 API·oplctl·웹은 §3.1 구현이다.

## 6. 테스트

- **단위**
  - 리스 표: 임기, 펜싱 중 거부, 기준점.
  - `HaGuard`의 fresh·stale 경계. 가짜 시계와 가짜 클러스터를 쓴다.
  - watchdog: 가짜 장치를 쓴다. 쓰다듬기, magic close, 리스가 죽고 HA VM이 돌 때 쓰다듬지 않는지.
  - 워커: 가짜 guard로 HA VM이 리스 없이 켜지지 않는지, stale에서 꺼지고 실패로 세지 않는지, 다시 fresh가 되면 켜지는지, `ImageLocked`를 세지 않는지.
  - `HaManager`: 단일 노드 하네스에 노드를 직접 기록해, 대상 고르기·status/spec 순서·Task 실패와 기록·대상 없음을 본다.
  - 위트니스: Raft 설정, 스케줄러가 빼는지, `cluster status` 경고.
- **클러스터(opl-cluster `memnet`)**
  - 셋 중 하나를 고립시키면 그 노드의 갱신이 실패하고 리더의 표에서 나이가 늘어나는지 본다.
  - 리더를 고립시키면 그 노드 자신도 갱신하지 못하는지 본다.
  - 위트니스(`enable_elect = false`)가 리더가 되지 않는지 본다. 일반 노드 둘과 위트니스에서 리더를 죽이면 남은 일반 노드가 리더가 되는지 본다.
- **실제 프로세스(opld 통합)**
  - **계획 22:** 노드 A, B와 위트니스 W(QEMU 경로가 없는 설정)로 클러스터를 만든다. 경고가 없는지, W가 배치에서 빠지는지(409) 본다. 리더를 `kill -9`하면 남은 일반 노드가 리더가 되고 쓰기가 되는지 본다.
  - **계획 24, 기능 `ha-it`:** A, B는 `fencing = process`, `fence_after_secs = 2`로 둔다. W도 함께 둔다. 공유 디렉터리 풀 위에 HA VM(실제 QEMU, macOS는 TCG, CI는 KVM)과 HA 아닌 VM을 A에서 켠다. A의 opld와 QEMU를 `kill -9`하면 HA VM만 B에서 켜지는지 본다. 기록 Task와, A를 다시 띄웠을 때 VM을 건드리지 않는지도 본다.
  - CI `linux-kvm`에 `opld/ha-it`를 더한다.

구현(계획 22): memnet 시험은 `opl-cluster/tests/witness.rs`(헬퍼 `TestCluster::start_with_witnesses(n, &[ids])`)로, 위트니스가 리더가 되지 않는 것과 일반 노드 둘·위트니스에서 리더를 멈추면 남은 일반 노드가 리더가 되는 것을 본다. 실제 시험은 `crates/opld/tests/cluster.rs`의 `a_witness_votes_but_never_leads_or_runs_vms`다. 위트니스(`Daemon::witness`, 없는 `qemu_binary`)는 API가 없으므로 하네스는 합류·시작 뒤 HTTPS API 대신 제어 소켓의 `state == "member"`를 기다린다. init 거부, API 포트가 닫혔는지, 투표 멤버 셋·경고 없음, `vm create --node wit`의 `node_unavailable`, 리더 `kill -9` 뒤 남은 일반 노드가 리더가 되고 `pool create`가 되는지, 위트니스를 다시 시작하면 온라인 위트니스 멤버로 돌아오는지, 죽은 노드를 `remove-node --force`로 뺀 뒤 일반 노드의 `cluster leave`는 거절되고 위트니스의 탈퇴는 되는지 본다. QEMU가 필요 없어 이 Mac에서도 돈다.

- watchdog 장치를 실제로 쓰는 시험은 CI에 두지 않는다. 터지면 러너가 재시작되고, CI의 `softdog`는 sanlock `wdmd`가 쥐고 있다. 대신 수동 시험 절차를 문서에 남긴다(`softdog soft_noboot=1`).

구현(계획 23): 리스 표 시험은 `opl-cluster/tests/lease.rs`(고립된 노드와 고립된 리더의 갱신 실패, 펜싱 중 거부, 임기가 바뀌면 표가 비는지 등), `HaGuard`는 `ha.rs` 단위 시험(`start_paused`: fence_after 직전·직후, 시작 기준점, 리스 없이 `arm` 실패와 장치를 열지 않는지, 쓰다듬기 규칙, magic close 조건, `accept_timeout`), 워커는 `opl-reconciler/tests/ha.rs`(가짜 guard·MockHypervisor), opld 설정·등록은 `config.rs`·`bootstrap.rs` 단위 시험, API는 `opl-api/tests/vms.rs`·`two_nodes.rs`·`migrate.rs`·`snapshot_tasks.rs`다. 실제 프로세스 시험(`ha-it`)은 계획 24다. 수동 watchdog 시험 절차는 README "HA VM과 자기 펜싱"에 있다. Linux opld(CI 하네스 포함)는 기본으로 watchdog 펜싱을 등록하지만 HA VM이 있을 때만 장치를 연다.

구현(계획 24): `HaManager`는 `opl-api/tests/ha.rs`(단일 노드 하네스에 Node를 직접 기록 — 작은 `recover_after_secs`, 갱신하지 않는 노드, 빠른 틱)로 이름 순서·대상 고르기(메모리, 죽은 노드 제외)·status/spec 순서·N의 Task 실패와 기록 Task, 리스가 살아 있는 노드는 옮기지 않음, 대상이 없을 때 오류를 한 번만 씀, 리더가 된 뒤 recover_after 전에는 옮기지 않음, 실행기가 기록 Task를 실행하지 않음, spec 쓰기 바로 앞에 만든 N의 Task도 실패로, 결과를 모르는 쓰기(가짜 `RecoveryWriter`로 status·spec 쓰기를 `Timeout` — 실제로 적용되거나 안 되거나) 동안 N의 갱신이 `Fenced`이고 결과를 알면 풀림, 쓰기 도중 종료하면 펜싱을 쥠, 쓰기 도중 리더를 잃거나 쓰기가 거부되면 펜싱이 풀림을 본다(12개). `base_at`·임기에 따른 해제·`settled`는 `ha.rs` 단위 시험이다(단일 노드에서 임기를 바꾸기 어렵다). 이미지 잠금은 `opl-driver`의 가짜 qemu 단위 시험(stderr 분류)과 `qemu-it` 실제 잠금 시험, `opl-reconciler/tests/reconcile.rs`(`fail_next_starts`로 잠금 4번 — 실패로 세지 않고 결국 Running, 그동안 status가 `Starting`과 잠금 문구, 그리고 status 쓰기 재시도 중 다른 노드로 옮겨지면 쓰지 않음)다. 실제 시험은 `crates/opld/tests/ha.rs`의 `the_leader_restarts_an_ha_vm_of_a_dead_node_on_another_node`(기능 `ha-it`): A·B(`fencing = "process"`, `fence_after_secs = 2`)와 위트니스, 공유 디렉터리 풀 디스크의 HA VM `db`(`--ha --restart on-failure`)와 디스크 없는 HA 아닌 VM `plain`을 A에서 켜고, A의 opld를 `kill -9`한 뒤 status의 pid로 QEMU들도 죽인다. 180초 안에 `db`가 B에서 Running(새 QEMU)이고 `vm-ha-recover` Task가 하나(from A, 노드 B, succeeded), `plain`은 A에 배정된 채인지, A를 다시 띄운 뒤 10초 동안 `db`가 B에서 같은 pid로 돌고 A가 켜지 않는지 본다. 한 호스트의 두 노드라 `systemd_scope = false`(`migration.rs`). 이 Mac에서 약 30초에 돈다. CI `linux-kvm` 작업의 기능 목록에 `opld/ha-it`를 더했다.

## 7. 알려진 한계

- 펜싱은 노드의 watchdog를 믿는다. `process`는 opld가 멈추면 지켜 주지 못한다. sanlock(공유 LVM)을 쓰는 노드는 `wdmd`가 기본 watchdog를 쥐므로 다른 watchdog 장치가 있어야 한다.
- 노드가 죽고 VM을 다른 노드에서 켜기까지 걸리는 시간은 대략 `fence_after + watchdog_timeout + 5`초(기본 65초)에 부팅 시간을 더한 것이다. 리더도 함께 죽었으면 새 리더가 된 때부터 그만큼이다.
- 리스를 갱신하지 못하는 노드(과반수와 끊김)는 HA VM을 끈다. 짧은 네트워크 장애가 `fence_after`보다 길면 HA VM이 다른 노드에서 다시 부팅된다.
- 죽은 노드에만 있던 UEFI 변수는 잃는다. 다른 노드의 낡은 복사본이나 템플릿으로 켠다(계획 21과 같다).
- RBD는 디스크 잠금이 없어 펜싱만 믿는다. NFSv3에서는 죽은 노드의 잠금이 그 노드가 돌아올 때까지 남을 수 있다. 그동안 VM은 "locked"로 기다린다.
- HA 아닌 VM은 죽은 노드에 묶여 있다(지금과 같다).
- 구현(계획 23 — 계획 24에서 풀림): 리더의 복구가 아직 없으므로 펜싱된 HA VM은 그 노드에 남아 있다가 리스가 돌아오면 그 노드에서 다시 켜진다. `recover_after_secs`는 등록만 한다. 계획 24에서 리더가 복구한다. 리더가 옮기기 전(`recover_after` 안)에 리스가 돌아오면 여전히 그 노드에서 다시 켜진다.
- 구현(계획 24): 이미지 잠금은 다른 프로세스가 쥔 동안 끝없이 기다린다(HA 아닌 VM도). 잠금을 쥔 프로세스가 사라지지 않으면 사용자가 정지해야 멈춘다.
- 구현(계획 24): 결과를 모르는 복구 쓰기가 있으면 결과를 알거나 임기가 바뀔 때까지 그 노드의 리스 갱신을 계속 거부한다(그 노드의 HA VM은 그동안 켜지지 않는다).
- 구현(계획 24): 복구 도중 리더가 다른 노드로 바뀌면 죽은 노드에 남은 그 VM의 Task 실패와 기록 Task가 빠질 수 있다(옛 리더가 다시 리더가 되면 한다). 끝나지 않은 Task는 그 노드가 돌아오거나 제거될 때 정리된다.
- 구현(계획 24): 실행 중인 VM의 HA는 켤 수 없다(§3.1). 켜려면 정지했다가 다시 시작해야 한다.
- 구현(계획 23): 라이브 마이그레이션이 보류 중인 HA VM은 보류가 풀릴 때까지 강제 종료하지 않는다(`process`에서 긴 마이그레이션이 `fence_after`를 넘길 수 있다. `watchdog`면 쓰다듬기가 멈춰 노드가 재시작된다). 실패한 마이그레이션이 원본을 그냥 `Paused`로 남기면 HA VM은 이어서 실행되지 않는다(드묾).
- 구현(계획 23): 정상 종료 때 도는 HA VM이 있으면 watchdog를 그대로 두므로 opld를 `watchdog_timeout` 안에 다시 띄우지 않으면 노드가 재시작된다.
- 위트니스는 한 클러스터에만 속하고, 위트니스 하나로는 클러스터를 만들 수 없다. 구현: 위트니스는 HTTPS API를 열지 않으므로 그 노드에서는 제어 소켓 명령(`cluster join`·`leave`)만 되고, 상태는 일반 노드의 API로 본다. 역할은 합류할 때 정해지므로 바꾸려면 탈퇴한 뒤 설정을 고치고 opld를 다시 띄워 다시 합류한다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 842개, 실패 0

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 325개

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: HA 복구 — README 복구 절, 설계 "구현(계획 24)" 메모와 HA 켜기 규칙, 하위 프로젝트 5 완료"
```

---

## 완료 기준 (계획 24)

- 노드가 죽으면 리더가 `recover_after_secs` 뒤 그 노드의 HA VM을 다른 HA 가능 노드에서 켜고, `vm-ha-recover` 기록 Task를 남기며, HA 아닌 VM은 그대로 둔다.
- 리스가 살아 있는 노드의 VM은 옮기지 않고, 펜싱 중인 노드의 갱신은 거부되며, 결과를 모르는 쓰기 뒤에는 결과를 알 때까지 펜싱을 쥔다.
- 디스크 이미지가 잠겨 있으면 실패로 세지 않고 기다린다. 실행 중인 VM의 HA는 켤 수 없다(409).
- 이 Mac에서 `ha-it`(opld 셋, 실제 QEMU)이, Linux CI(`linux-kvm`)에서 같은 시험이 통과한다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **죽은 노드에만 있던 UEFI 변수는 잃는다**(다른 노드의 복사본이나 템플릿으로 켠다).
- **HA 아닌 VM은 죽은 노드에 묶여 있다.**
- **RBD는 디스크 잠금이 없어 펜싱만 믿고, NFSv3의 잠금은 죽은 노드가 돌아올 때까지 남을 수 있다**(그동안 "locked"로 기다린다).
- **복구 도중 리더가 다른 노드로 바뀌면** 죽은 노드에 남은 그 VM의 Task 실패와 기록 Task가 빠질 수 있다(드물게 기록이 둘이 될 수도 있다).
- 그 밖의 제한은 설계 문서 §7과 README HA 절에 있다.

## 실행 후 기록 (2026-11-02)

6개 Task는 계획대로 적용했고, Task마다 검증된 단계와 트리가 같았다(생성 파일 `schema.d.ts` 포함). Task 2의 첫 적용은 도중에 멈춰 남은 편집을 되돌리고 처음부터 다시 적용했다. 계획 문서를 만들기 전에 스테이지 작업에서 리뷰를 두 차례 돌려 고쳤다(위 Task의 코드에 이미 들어 있다).

- 1차: 결과를 모르는 복구 쓰기(시간 초과) 뒤 펜싱을 풀어 이중 실행이 날 수 있던 것(같은 임기에서 같은 쓰기를 결과를 알 때까지 다시 보내고 그동안 펜싱을 쥔다), Task 목록을 쓰기 전에 읽던 것, 오류 경로의 펜싱 해제와 기준점 시험, `ImageLocked` 시험의 시간 의존. 실제 시험이 찾은 것: 다시 시작한 노드가 옮겨 간 VM의 status를 덮어쓰던 것(워커 status 쓰기 재시도가 배정을 확인).
- 2차: 임기 변경으로 정리된 spec 쓰기의 남은 단계(Task 실패, 기록 Task)를 다음에 리더일 때 마저 한다, `NotLeader` 주석, 결정적 시작 문.
- CI 확인용 브랜치에서 `linux-kvm`이 snap 다운로드 연결 끊김으로 한 번 실패했고(다시 돌림), 계획 23의 `opl-reconciler` `tests/ha.rs` 시험 하나가 부하에서 가끔 실패했다 — 단일 스레드 런타임이 리스 시간(300ms)보다 오래 멈추면 리스가 실제로 끝나 VM이 다시 켜지는 시험의 경쟁이었다(제품은 리스가 살아 있을 때만 켠다). 리스가 끝날 필요가 없는 시험은 60초 리스, VM별 실행 횟수로 고쳤다(Task 2에 들어 있다). 그 뒤 CI 5개 작업이 모두 통과했다(`ha-it`이 KVM에서 처음 통과).

테스트: macOS `cargo test --workspace` 842개, `opl-driver` `qemu-it` 10개, `opld` `ha-it`(opld 셋, 실제 QEMU), 웹 Vitest 325개, `fmt`·`clippy -D warnings`(모든 기능).

## 다음 계획으로 넘길 사항

- 하위 프로젝트 5(HA·펜싱·위트니스)를 마쳤다. 다음은 처음 분해(`2026-10-20-opl-node-core-design.md` §1.1)의 하위 프로젝트 6(네트워크와 SDN)이다.
- 남은 HA 과제: 죽은 노드에만 있던 UEFI 변수 지키기, HA 아닌 VM을 죽은 노드에서 옮기는 관리자 명령, HA 그룹·우선순위, 노드 유지보수 때 모두 옮기기, RBD 배타 잠금·blocklist, sanlock `wdmd`와 watchdog 나눠 쓰기, 복구 도중 리더가 바뀔 때 빠질 수 있는 Task 정리와 기록.
