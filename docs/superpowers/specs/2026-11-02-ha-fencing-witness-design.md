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
