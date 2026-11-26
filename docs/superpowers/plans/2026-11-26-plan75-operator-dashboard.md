# Orbivisor 계획 75: 운영자 대시보드 — 홈 + 2.5D 인프라 지도 + 물리·가상 관계 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 운영자가 홈 한 화면으로 클러스터를 본다 — 2.5D 지도(포트 줄·네트워크 레일), 노드별 CPU·RAM·스토리지 할당·사용 표, 고른 노드의 vCPU ↔ 스레드(NUMA·고정·오버커밋),
메모리·스토리지 관계, 물리 포트 → 본딩 → VLAN → 브리지 → 네트워크 → VM 사슬과 랜 포트·본딩 장애의 영향, 링크·본딩 경보.

**Architecture:** `orbivisor-types::physical`·`capacity` 순수 함수(사슬·영향·레일·용량) → `GET /topology/physical`·`GET /capacity`(기능 `operator-dashboard`), 노드 수집에 VLAN 아래 장치·MTU·LLDP(lldpd 있으면),
표본에 인터페이스별 rx/tx·오류, 경보 `NodeLinkDown`·`BondDegraded`(기능 `link-alerts`). 웹 홈은 지도 중심 + 자세히 판 + 할당·사용 표, 지도 네트워크 층에 포트 줄·레일·장애 초점.

**Tech Stack:** Rust(orbivisor-types·orbivisor-driver·orbivisord·orbivisor-api, utoipa), React 19·TypeScript·TanStack Query, 지도 SVG(계획 70 iso), vitest·msw·axe, Playwright(web-it). 새 의존성 없음.

**설계 문서:** `docs/superpowers/specs/2026-11-26-operator-dashboard-design.md`
**선행 계획:** 계획 1~74 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| 수집: VLAN 아래 장치·VLAN id·MTU·LLDP 이웃, 인터페이스별 rx/tx·오류·버림(실시간) | 인터페이스별 이력, SNMP, 직접 LLDP 수신 |
| `GET /topology/physical`(사슬·영향·레일), `GET /capacity`(CPU·메모리·NUMA·스토리지 할당·사용·오버커밋·씬 프로비전) | 포트 설정 바꾸기 |
| 경보 `NodeLinkDown`·`BondDegraded`(권하는 묶음 포함) | |
| 웹 홈: 지도 + 자세히 판(CPU 스레드 격자·메모리·스토리지·네트워크 사슬도) + 노드별 할당·사용 표, VM 경로 | 새 사이드바 항목, WebGL |
| 지도: 포트 줄·네트워크 레일·장애 초점(`?focus=port:`)·예산 | 노드 사이 실제 경로 추적 |

## 실행 환경
- **이 Mac:** Rust 전부, 웹 전부, web-it.
- **svc01:** 일반 시험 묶음 전체.
- **GitHub 탐침:** 모든 작업(`linux-kvm`에서 수집 시험 — 실제 bond/VLAN을 만들 수 있으면 `net-it`).
- **실험실:** 배포 뒤 dummy 인터페이스 둘로 bond(실험실 전용)를 만들어 한 포트를 내리고 지도·판·경보·영향 VM 수를 확인, 끝나면 지움.

## Task

| 단계 | 내용 | 의존 |
|---|---|---|
| 1 | 타입·순수 함수: `NodeInterface` 새 칸, `LldpNeighbor`, `IfaceSample`, `physical::build`(사슬·영향·레일), `capacity::build`, `AlertCondition` 두 변형, 기능 상수 | — |
| 2 | 노드 수집: `ip -j -d` link/vlan/mtu, lldpcli, 표본기 인터페이스별 | 1 |
| 3 | API: 두 끝점(캐시·권한·기능), 경보 엔진 두 조건·메시지·권하는 묶음, OpenAPI·`gen:api` | 1 |
| 4 | 웹 홈: 레이아웃·할당·사용 표·자세히 판 네 칸·VM 경로·권한·기능 없음 | 3 |
| 5 | 웹 지도: 포트 줄·레일·장애 배지·초점·LOD·예산 | 3 |
| 6 | 문서: `docs/guide`(대시보드 안내·lldpd), architecture | 4·5 |

## 완료 기준 (계획 75)
- [ ] 이 Mac 전부, svc01 전부, GitHub 탐침 전부
- [ ] 리뷰 두 번(세 갈래) 고침
- [ ] 실험실: bond 포트 내림 → 지도 포트 빨강·레일·판 영향·경보 firing/resolved
- [ ] main CI 통과

## 알고 받아들인 제한
설계 §7.

## 실행 후 기록

- **단계**(2026-11-26): 1 물리·용량 타입(`orbivisor_types::physical`·`capacity`), 2 노드 수집(VLAN·MTU·`admin_up`·LLDP — lldpcli는 따로 작업·5분마다 다시 찾기, 인터페이스별 표본),
  3 API(`/topology/physical`·`/capacity`, 기능 `operator-dashboard`·`link-alerts`)와 링크 경보, 4 지도(포트 줄·레일·`?focus=port:`·키보드), 5 홈(지도 + 자세히 판 + 용량 표), 6 CLI·문서.
- **리뷰 1**(backend·web·operator): 중요 7을 모두 고침 — 주소만 가진 NIC(스토리지·관리)를 새 역할 `service`로 감시, 본딩 한 포트 down은 `bond_degraded` 하나만, 사라진 NIC를 down으로,
  CPU 열(노드 사용%·공유 할당·전용 할당 따로), underlay 주소가 사라지면 레일 `unknown`, 사슬도가 `other` 포트를 버리던 것, 물리 자료가 없는데 "장애 없음"을 보이던 것.
- **리뷰 2**: 장애 순간 포트가 사라지거나 주소를 잃으면 역할이 바뀌어 감시 밖으로 빠지는 것이 중요 항목 대부분의 뿌리라 땜질 대신 "기억하는 포트" 모델로 —
  노드 상태 `known_ports`(재시작에도 남음), 사라짐은 `missing`, `POST /nodes/{key}/forget-ports`(CLI `node forget-ports`, 웹 "잊기"). 본딩 down이면 `bond_degraded`를 풀지 않고
  `node_link_down`(critical)을 함께. 링크 경보 묶음은 SDN 묶음에서 떼어 `POST /alert-rules/link-defaults`로.
- **이 Mac**: fmt·clippy·Rust 4584·웹 2394·Go vet·이름 시험·web-it 6 통과(`8277432e`). 지도 칸 InfraMap 약 24.43 kB(예산 24 KiB 안), perf 약 15 ms(예산 30 ms).
- **svc01**: 일반 시험 묶음 13단계 전부 통과, 4723개.
- **GitHub 탐침**: 세 번 돌림. 첫째는 `linux-kvm` — 새 러너 이미지(20261122)에서 `zfs version`이 root를 요구해 `sudo`를 붙임(`1ff794d2`). 둘째는 `check (ubuntu-24.04)`만 실패 —
  시험 빌드가 쌓여 러너 디스크가 참(링커 Bus error, 남은 80 MB) → check 작업은 디버그 정보 없이 빌드(`8277432e`). 셋째 21작업 모두 통과.
- **배포**: svc01에서 릴리스 빌드(웹 포함) → svc02 → svc01, 계획 74의 업그레이드 길(`postinst configure`) 그대로. 두 노드 active·두 유닛 이름 하나, 기능 `operator-dashboard`·`link-alerts` 알림,
  svc01 test-vm(PID 40553) 그대로 `running`.
- **실험실 장애 시험**(svc02, dummy 둘 → active-backup 본딩 `bondt` → 브리지 `brt`, 웹훅 수신기로 알림 확인, 끝나고 모두 지움):
  d2 down → 물리 위상에서 `bondt degraded`·d2 down·영향 `brt`, 구성 포트에 `node_link_down` 없음, 60초 뒤 `bond-degraded` firing 알림
  "bondt degraded on svc02 (d2 down) — brt"(지도 `/cluster/map?focus=port:svc02/bondt`). d1도 down → `node-link-down` critical firing
  "bondt down on svc02 (d1 down, d2 down) — brt", `bond-degraded`는 풀리지 않고 firing 그대로. d1 up → `node-link-down`만 resolved.
  d2 삭제(`ip link del`) → 구성 포트 `missing`으로 기억, `bond-degraded` 유지. `forget-ports d2` → 기억에서 빠지고 본딩 up, `bond-degraded` resolved.

## 다음 계획으로 넘길 사항

- resolved 알림에는 덧말(`detail`)이 없다(모든 경보가 원래 그렇다 — 제목에 대상·포트가 있음). 풀린 까닭("d2 forgotten", "port back up")을 붙이면 좋다.
- CI 러너 디스크 여유가 작다(check 작업이 약 100 GB 근처). 시험이 더 늘면 단계 사이 정리나 시험 묶음 나누기가 필요하다.
