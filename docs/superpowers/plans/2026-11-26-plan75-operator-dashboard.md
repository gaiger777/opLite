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
(실행 뒤 채움)
