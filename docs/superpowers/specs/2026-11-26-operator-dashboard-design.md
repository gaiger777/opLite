# 운영자 대시보드 — 홈 + 2.5D 인프라 지도 + 물리·가상 관계 (설계)

상위 설계: `2026-11-24-cluster-view-design.md`(계획 70 지도), `2026-11-18-ui-ux-redesign-design.md`(계획 61 UI 규칙),
`2026-11-19-numa-pinning-design.md`(계획 64), `2026-11-20-sdn-ovn-design.md`(계획 65~67).

사용자 요청(2026-11-25): "인프라 지도와 홈에 대시보드를 엮어서 보여줄 필요가 있을 듯해. 2.5d로 표현하고 가상 네트워크와 물리 네트워크와의
연관관계, 실제코어와 가상코어와의 연관관계, 메모리, 스토리지도 마찬가지고, 물리네트워크는 랜포트(본딩) 장애났을 경우 어디서 문제인지 한번에
확인할 수 있는.. 그리고 노드별 cpu 할당량, 사용량, 램 할당량, 사용량, 스토리지도 할당량, 사용량.. 운영자가 정말 이 화면만 보고 클라우드가
어떻게 돌아가는지 확인할 수 있게."

목표: **홈 한 화면**에서 (1) 클러스터 전체가 2.5D 지도로 보이고, (2) 노드를 고르면 그 노드의 CPU·메모리·스토리지·네트워크가 "가상 ↔ 물리" 관계로
펼쳐지고, (3) 랜 포트·본딩 장애가 지도 위 그 포트에 빨갛게 찍히고 "무엇이 영향받는지"(브리지·네트워크·VM)가 바로 보이며, (4) 노드마다 할당·사용이
같은 막대로 비교된다.

## 1. 지금 있는 것(조사 2026-11-26)

- 홈(`web/src/pages/DashboardPage.tsx`): 상태 띠, 경보, 용량(클라이언트 계산 vCPU·메모리 할당), 풀 사용, 클러스터 차트, 노드 띠, 상위 VM, 최근 Task, 즐겨찾기.
- 지도(`web/src/clusterMap/`, 계획 70): 존·랙 판, 노드 블록(열 = max(cpu, mem), 옆면 게이지, 메모리 오버커밋 띠), VM 타일, 이주 화살표, 층(부하·스토리지·
  네트워크 업링크 띠 하나·HA·권고), 100노드 × 50VM 예산(배치+경로 < 30ms, 요소 < 4000), 칸 18.6KB/20KB.
- 노드 상태: `NodeStatus.interfaces[NodeInterface{name, kind, state, carrier, speed_mbps, mac, master, addresses, bond: BondStatus{mode, state, ports[BondPort{name,
  state(up/down/lacp_mismatch/speed_mismatch), speed_mbps, aggregator}]}}]`(30초, `hostcheck.rs`), `bridges`, `underlay`, `sdn.physnets`, `pools`, `topology`, `checks`.
- 부하: `GET /placement`(`NodeLoadView` cpu%·mem used/allocated/total). NUMA: `GET /nodes/{key}/topology`(호스트 NUMA 노드별 cpu·메모리·전용·hugepage, VM별 고정 pCPU),
  `GET /topology/usage`(노드별 고정·전용·hugepage 요약). 메트릭: `/metrics/nodes`(노드 합계 net rx/tx — 인터페이스별 없음).
- 없는 것: 링크·본딩 경보 조건, VLAN 인터페이스의 아래 장치, MTU, 인터페이스별 트래픽·오류, LLDP, 풀의 "할당(프로비전)" 합, vCPU 오버커밋 비율(서버).

## 2. 고른 것과 견준 길

- **홈 = 지도 중심(채택) vs 홈에 작은 지도 미리보기 위젯 vs 지도 화면에 대시보드 붙이기.** 계획 70은 홈 위젯을 뺐지만(§15) 사용자가 "홈에 엮어"를 직접 요청했다.
  미리보기 위젯은 "이 화면만 보고"에 못 미치고, 지도 화면에 붙이면 홈이 둘이 된다. 홈의 위쪽 큰 칸을 지도로 두고 오른쪽(좁은 화면은 아래)에 **고른 것의 자세히 판**,
  그 아래에 지금 위젯들(경보·Task·차트)을 그대로 둔다. 지도 컴포넌트는 Infra › 노드 › 지도와 **같은 것**(같은 칸 — 홈에서 늦게 읽음). 새 사이드바 항목 없음(계획 61 규칙).
- **물리 관계 계산: 서버 끝점 하나 `GET /topology/physical`(채택) vs 웹이 노드·네트워크·VM을 엮음.** 같은 계산(포트 → 본딩 → VLAN → 브리지 → 네트워크 → VM NIC)을 경보 문구
  ("eno1 down: bond0 degraded — vmbr0, 네트워크 3개, VM 12개 영향")와 화면이 함께 쓴다. 한 곳(`orbivisor-types::physical` 순수 함수)에 두고 서버가 쓴다. VM 목록을 다 내려받지
  않아도 되어 큰 클러스터에서 가볍다.
- **용량 요약: 서버 끝점 `GET /capacity`(채택) vs 지금처럼 클라이언트 합산.** 풀의 프로비전 합(볼륨 `size_bytes` 합)은 볼륨이 수천 개면 웹이 셀 수 없다. vCPU 할당·전용·
  예약·오버커밋 비율도 서버가 한 번에 낸다(지금 `CapacityWidget`의 계산을 서버로 옮기고 같은 값인지 시험).
- **인터페이스 트래픽: 실시간 표본에만(채택) vs 이력까지.** `/metrics/nodes`·`/nodes/{key}/metrics` 표본에 인터페이스별 rx/tx bps·오류·버림 증가를 더한다(물리·본딩·브리지만,
  노드당 64개 상한). 이력(RRD)은 노드 합계 그대로 — 인터페이스별 이력은 범위 밖(디스크·메모리).
- **LLDP: lldpd가 있으면 읽음(채택) vs 직접 LLDP 수신.** `lldpcli -f json show neighbors`(30초, 인터페이스 수집과 함께)로 이웃 스위치 이름·포트를 붙인다. lldpd가 없으면 칸이
  비고 안내("lldpd를 설치하면 스위치 포트가 보입니다"). 직접 수신(raw socket)은 권한·보안 면이 넓다.
- **경보: 새 조건 `NodeLinkDown`·`BondDegraded`(채택) vs HA 검사 결과를 경보로.** HA 검사는 사람이 정한 인터페이스만 보고 Task로 남는다. 새 조건은 상태의 `interfaces`를 리더의
  경보 엔진이 보고(이미 있는 이벤트 조건 방식), **쓰이는 포트만**(브리지·본딩에 속한 ethernet, underlay를 가진 인터페이스, 전역 주소를 가진
  물리 포트 — 리뷰 1) 기본으로 본다. 기능 `link-alerts`.
- **2.5D 표현: 지도의 네트워크 층을 "포트 줄 + 네트워크 레일"로(채택) vs 노드 사이 선 그리기.** 노드 블록 앞면 아래에 물리 포트를 작은 블록 줄로(본딩은 묶음 테두리), 지도 밑에
  네트워크 레일(브리지/physnet/VXLAN underlay별 가로 띠)을 깔고 노드의 업링크 포트에서 레일로 짧은 세로 선. 포트가 죽으면 그 포트와 선이 빨갛고, 레일에서 떨어진 노드가 보인다.
  노드끼리 선을 그으면 100노드에서 선이 수천 개가 되어 예산을 넘는다(계획 70 §16의 "노드 사이 선"을 이 방식으로 푼다).

## 3. 서버

### 3.1 수집(노드)
- `NodeInterface`에 `mtu: Option<u32>`, `parent: Option<String>`(VLAN·macvlan의 아래 장치 — `ip -j -d`의 `link`), `vlan_id: Option<u16>`, `lldp: Option<LldpNeighbor{chassis_name,
  chassis_id, port_id, port_descr, mgmt_ip}>`를 더한다(serde default·skip_if_none — 옛 노드 상태와 호환).
- 인터페이스별 표본: 표본기(`orbivisord/src/metrics/sampler.rs`)가 `/sys/class/net/<if>/statistics/{rx,tx}_{bytes,errors,dropped}`를 읽어 직전 값과의 차로 `IfaceSample{name, rx_bps,
  tx_bps, rx_errors, tx_errors, rx_dropped, tx_dropped}`(errors·dropped는 표본 간 증가). 대상: kind ethernet·bond·vlan·bridge, veth·tap 제외, 64개 상한. `NodeSample.interfaces`(새 칸).
- LLDP: `lldpcli`가 있으면 30초마다(시간 상한 3초), 출력 파싱은 순수 함수 + 고정물 시험. 없으면 `None`.

### 3.2 물리 관계 `GET /topology/physical` (권한 `cluster.read`, 기능 `operator-dashboard`)
순수 함수 `orbivisor_types::physical::build(nodes, networks, vms) -> PhysicalTopology`(풀은 보지 않는다 — 스토리지망은 주소를 가진 포트의
역할 `service`와 쓰임으로 보인다):
```
PhysicalTopology { nodes: [NodePhysical {
  node, online,
  ports: [Port{name, kind(ethernet|bond|vlan|bridge|other), state, carrier, speed_mbps, mtu, mac, parent, vlan_id, lldp,
               bond: Option<{mode, state, members[]}>, role(uplink|underlay|service|unused|other), uses[cluster|management], health}],
  bridges: [Bridge{name, uplinks[port], vlan_filtering, networks[network id], vm_nics: u32}],
  underlay: Option<{address, port}>,
  physnets: [{name, bridge, ready, error}],
  impact: [Impact{port, severity(down|degraded), bond, bridges[], underlay, uses[], networks[], vms: u32, vm_ids (최대 50)}],
  unreachable: [{network, vms, vm_ids}],
}], networks: [NetRail{id, name, kind(bridge|vlan|vxlan|sdn-overlay|sdn-provider), carrier(bridge 이름|physnet|underlay), vlan, nodes[{node, state(up|degraded|down|absent|unknown)}]}] }
```
- 사슬: VM NIC → 네트워크 → (bridge·vlan → 노드 브리지 | provider physnet → 노드 physnet 브리지 | vxlan·overlay → underlay 주소를 가진 인터페이스) → 브리지 업링크(`master == bridge`)
  → VLAN이면 `parent` → 본딩이면 구성 포트.
- 역할(리뷰 1): 브리지 아래·본딩과 그 구성 포트 = `uplink`, underlay 주소의 인터페이스와 그 아래 = `underlay`, 그 밖에 전역 주소(IPv4·IPv6,
  링크 로컬 빼고)를 가진 물리 포트(ethernet·bond·vlan·other)와 그 아래 = `service`(스토리지·복제·관리·클러스터 링크망 — 노드의
  `cluster_addresses`·API 주소면 `uses`에 `cluster`·`management`), 묶이지 않고 주소도 없는 ethernet = `unused`, 나머지 = `other`.
- 영향: 포트 down(carrier 없음)·본딩 down·관리자가 내린 브리지(`admin_up` 거짓) → 그 위 브리지·네트워크·이 노드의 VM 수 "down", 본딩 degraded·
  포트 down이지만 본딩은 up → "degraded"(이중화 잃음). `uplink`·`underlay`·`service` 포트만 영향을 낸다. 도는 VM의 NIC 네트워크를 이 노드에서 쓸
  수단이 없으면 `unreachable`.
- underlay 주인: 주소를 가진 인터페이스, 없으면(NetworkManager가 carrier를 잃은 장치의 주소를 뗌) 노드가 마지막으로 본 이름(`NodeStatus.underlay_ports`).
  그 이름이 목록에서도 사라졌으면 끊김, 이름도 없으면 레일은 `unknown`.
- 본딩 구성 포트가 사라짐(PCIe 장애·unbind·핫플러그): 노드가 본딩마다 본 적 있는 구성 포트를 기억해 `missing`으로 남기고 본딩을 degraded로 본다.
- 레일 상태: 노드마다 그 네트워크의 사슬이 끝까지 up이면 up, 본딩 degraded면 degraded, 끊기면 down, 그 노드에 그 네트워크를 쓸 수단이 없으면 absent,
  노드가 오프라인이거나 underlay 주인을 모르면 unknown(낡은 상태·모르는 것을 끊김·정상으로 그리지 않는다). 오프라인 노드의 `impact`는 마지막으로
  본 값이고 화면은 그렇게 적는다(배지 수에는 넣지 않는다).
- watch·캐시: 노드·네트워크·VM 리비전이 같으면 이전 결과(서버 메모리 캐시 하나). 웹은 15초 갱신 + 노드 watch에서 바뀌면 다시.

### 3.3 용량 `GET /capacity` (권한 `cluster.read`, 같은 기능)
```
Capacity { nodes: [NodeCapacity{ node, online,
  cpu: {threads, reserved, dedicated_total, dedicated_used, vcpus_allocated, vcpus_running, vcpus_shared, overcommit (vcpus_shared / (threads - reserved - dedicated_total)), use_pct (placement 부하 — 노드 전체)},
  memory: {total_mib, reserved_mib, allocated_mib, allocated_hugepages_mib, used_mib, hugepages: [{size_kib, total, used}], overcommit ((allocated_mib - allocated_hugepages_mib) / (total_mib - reserved_mib))},
  numa: [{id, cpus, memory_mib, dedicated_used, pinned_vcpus}] (topology 있으면),
  storage: [{pool, total_bytes, used_bytes, provisioned_bytes, volumes}] (그 노드의 로컬 풀) }],
  shared_pools: [{pool, total, used, provisioned, volumes, nodes_ready, nodes}], totals: {...같은 모양 합} }
```
- `vcpus_allocated` = 그 노드에 자리 잡은 VM의 vCPU 합 — QEMU가 떠 있는 단계(시작·실행·정지 중·옮기는 중, 상태 node 기준 — 멈춘·실패한 VM은
  세지 않고, 옮기는 VM은 원본 노드에만 센다), `vcpus_running` = 실행 중만, `vcpus_shared` = 그 가운데 공유 정책. `CapacityWidget.usage`와 같은
  규칙이다(서버 값과 같은지 시험). 오버커밋은 공유 vCPU ÷ 공유 스레드다(전용 VM은 자기 스레드 위에서 돈다). 표는 공유 할당(공유 vCPU / 공유 스레드)과
  전용 할당(잡힌 전용 / 전용)을 따로 보이고, 사용률은 노드 전체 %를 글로 보인다(공유 풀 막대에 얹지 않는다 — 리뷰 1). "문제만"은 공유 vCPU
  오버커밋 > 4(설정 한도가 생기면 그 값)·메모리 오버커밋 > 1·사용 > 90%·링크 장애·없는 네트워크다.
- `provisioned_bytes` = 그 풀 볼륨 `size_bytes` 합(씬 할당), `used_bytes` = `total - available`.

### 3.4 경보
- `AlertCondition::NodeLinkDown{ ports: Option<String>(글롭), role: uplink|any }`, `AlertCondition::BondDegraded{}`. 리더 경보 엔진의 이벤트 조건 방식(노드 상태 바뀜을 봄), 메시지는
  `physical::describe`로 "eno1 down on svc01: bond0 degraded — vmbr0, 3 networks, 12 VMs". 해소되면 resolved. 권하는 묶음은 SDN 묶음과 따로
  `link_default_rules`(`POST /alert-rules/link-defaults`, 심각도 critical/warning — SDN 없는 클러스터에서도 켤 수 있게).
- 본딩(리뷰 1): 구성 포트가 끊기고 본딩은 흐르면 `bond_degraded`(warning) 하나만, 본딩이 끊기면 본딩 이름으로 `node_link_down`(critical) 하나 —
  구성 포트는 따로 울리지 않는다. 저하 → 끊김은 새 알림(`node_link_down`)이고 저하 경보는 풀린다. `role: any`는 이더넷·VLAN과 묶인 포트까지
  보되 어디에도 묶이지 않은 가상 장치(`ovs-system`·`br-int`)는 보지 않는다.
- 경보가 난 포트가 노드의 인터페이스 목록에서 사라지면 풀지 않고 "사라짐"으로 둔다(다시 나타나 살아 있으면 풀림). 오프라인·유지보수 노드의 링크
  경보는 새로 내지도·풀지도·되풀이하지도 않는다. 알림 본문에 지도 경로 `/cluster/map?focus=port:<노드>/<포트>`를 붙인다.
- 기능 `link-alerts` — 섞인 판에서 새 조건 만들기는 409 `cluster_upgrading`.

## 4. 웹

### 4.1 홈
```
┌ 상태 띠 (지금) ──────────────────────────────────────────────────────────────┐
│ ┌ 인프라 지도 (2.5D, 층 토글: 부하·네트워크·스토리지·HA) ─┐ ┌ 자세히 판 ───────┐ │
│ │  존/랙 · 노드 블록 · VM 타일 · 포트 줄 · 네트워크 레일    │ │ 고른 것 없음:     │ │
│ │  (장애 포트는 빨강 + 깜빡임 없는 표시, 레일 끊김)        │ │  클러스터 요약     │ │
│ └──────────────────────────────────────────────────────────┘ │ 노드: CPU·메모리· │ │
│ ┌ 노드별 할당·사용 표 (CPU / RAM / 스토리지 — 같은 막대) ─┐ │  스토리지·네트워크│ │
│ └──────────────────────────────────────────────────────────┘ └──────────────────┘ │
│ 경보 · 최근 Task · 클러스터 차트 · 상위 VM (지금 위젯)                           │
└──────────────────────────────────────────────────────────────────────────────┘
```
- 지도는 `ClusterMapScene`을 그대로(늦게 읽는 칸), 홈에서는 "홈 모드"(높이 고정, 범례 접힘). 장애가 있으면 지도 위에 "물리 장애 n" 배지 → 누르면 그 포트로 초점.
- **노드별 할당·사용 표**: 행 = 노드, 열 = CPU(할당 vCPU / 가용 스레드 막대 + 사용% 표시선 + 오버커밋 ×n), RAM(할당 / 총 + 사용 표시선), 스토리지(프로비전 / 총 + 사용 표시선).
  한 막대 모양: 바탕 = 총, 진한 = 사용, 테두리 칸 = 할당(넘치면 빨간 넘침 꼬리). 정렬·필터(문제만). 좁은 화면은 카드.
- **자세히 판(노드)** 네 탭이 아니라 한 열의 네 칸(스크롤):
  1. **CPU**: 물리 스레드 격자(NUMA 노드별 묶음, 칸 = 스레드: 예약 회색·전용(고정된 VM 이름)·공유), 그 아래 "공유 풀 vCPU n개가 스레드 m개 위 — 오버커밋 ×k", 사용% 막대.
     VM에 마우스를 올리면 그 VM의 vCPU → 스레드가 강조(고정이면 칸, 공유면 공유 영역 전체).
  2. **메모리**: NUMA 노드별 막대(총·할당·사용·hugepage 풀), VM 상위 몇 개의 할당, 오버커밋 띠.
  3. **스토리지**: 로컬 풀·공유 풀별 막대(총·프로비전·사용), 씬 프로비전 비율, 풀 상태.
  4. **네트워크**: 물리 → 가상 그림(작은 2.5D가 아니라 평면 사슬도 — 왼쪽 물리 포트(LLDP 스위치·포트, 속도, carrier) → 본딩 → VLAN → 브리지/physnet/underlay → 네트워크 → VM 수).
     장애 포트에서 오른쪽으로 영향 경로가 빨갛게. 인터페이스 rx/tx·오류(실시간 표본).
- 자세히 판(VM): 지금 `VmPeek` + "이 VM의 경로": vCPU → 스레드, 메모리 → NUMA, 디스크 → 풀, NIC → 네트워크 → 브리지 → 포트(상태).
- 권한: `cluster.read` 없는 사용자는 지금처럼(지도·표·판 숨김, 자기 VM·경보만).

### 4.2 지도(홈·Infra 둘 다)
- **포트 줄**(네트워크 층): 노드 블록 앞면 아래에 업링크·underlay 포트를 작은 블록(최대 8, 넘으면 +n), 본딩은 묶음 테두리, 색 = up/degraded/down(색만이 아니라 down은 ×,
  degraded는 빗금), LOD summary에서는 지금 업링크 띠 하나로 접힘.
- **네트워크 레일**: 지도 아래 바닥 판에 레일(브리지·physnet·underlay별, 최대 8 + "+n"), 노드의 업링크 포트 → 레일 세로 선. 레일 위에 그 네트워크 수. down이면 선이 끊김 표시.
  레일을 고르면 그 레일을 쓰는 노드·네트워크·VM이 강조.
- **장애 초점**: `?focus=port:<node>/<port>` — 지도가 그 노드로 이동·포트 강조, 판은 네트워크 칸.
- 예산(100노드 × 50VM): 배치+경로 < 30ms 그대로, 요소: 네트워크 층 켜면 < 4,500(포트 줄 merged path, 레일 선 merged path — 색·층마다 path 하나), 칸 크기 < 24 KiB(24,576바이트) gzip(예산 올림 — 까닭 기록),
  홈 첫 로드 늘어남 < 3KB(지도·판은 늦게 읽음).

## 5. 섞인 판·호환
- 새 상태 칸은 옛 노드에 없음 → 물리 끝점은 그 노드의 `parent`·`mtu`·`lldp`를 비우고 VLAN 아래 장치는 이름 규칙(`eno1.100`)으로 추정, 표시 "옛 판 — 일부 정보 없음".
- 기능 `operator-dashboard`를 온라인 노드가 모두 알지 않으면 두 끝점이 409이고 웹은 지금 홈(위젯만)을 보이고 "업그레이드 뒤 대시보드" 안내. 꺼진
  옛 노드는 막지 않는다(리뷰 1 — 지우지 않은 죽은 노드 하나가 대시보드를 영영 막지 않게): 그 노드는 위의 `legacy` 표시로 마지막 상태를 보인다.
  새 경보 조건은 `link-alerts` 게이트(모든 노드).

## 6. 시험
- 수집: `ip -j -d` 고정물(VLAN `link`, MTU), sysfs 통계 차(되감김·없어진 인터페이스), lldpcli JSON 고정물(이웃 없음·여럿).
- `physical::build`: 사슬 경우(브리지 직결·본딩·본딩 위 VLAN·VLAN 위 브리지·provider physnet·VXLAN underlay·SDN overlay), 영향(포트 down·본딩 degraded·본딩 down·브리지 없음), 레일 상태, 옛 노드 상태.
- `capacity`: 위젯 계산과 같은 값, 예약·전용·hugepage, 씬 프로비전, 공유 풀.
- 경보: 포트 down → firing·메시지, 다시 up → resolved, 역할 필터, 기능 게이트.
- 웹: 홈 레이아웃(넓음·좁음·권한 없음·기능 없음), 표 정렬·넘침, 판 네 칸(고정·공유·NUMA 없음), 네트워크 사슬도 장애 강조, 지도 포트 줄·레일·초점, perf 예산, axe, 색 대비(두 테마).
- 실제(실험실): svc01·svc02에서 본딩이 없으므로 — dummy 인터페이스 둘로 bond를 만들고(실험실 전용) 하나를 `ip link set down`해 지도·판·경보가 그 포트를 짚는지, VM이 있는 브리지 업링크를 내려
  영향 VM 수. 끝나면 지움.

## 7. 범위 밖
- 인터페이스별 이력, 스위치 쪽 SNMP, 직접 LLDP 수신, 포트 설정 바꾸기(본딩 만들기 등 — 운영 도구가 아님), 노드 사이 실제 경로 추적(traceroute), 3D(WebGL).
