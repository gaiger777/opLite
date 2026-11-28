# Orbivisor 계획 76: DR 오케스트레이션 — 복구 계획·부팅 그룹·네트워크/IP 바꾸기·격리 시험·RPO/RTO 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** DR 클러스터의 복구 계획 하나로 지키는 VM의 RPO를 늘 보고, 격리망 시험 장애 조치로 RTO를 재고 뒷정리하며, 실제 장애 때 그룹 순서대로 복원·시작하고
네트워크·IP를 DR에 맞춘다(계획된 이전은 운영 쪽 정지·마지막 백업·복제 뒤 넘김).

**Architecture:** `orbivisor-types::dr` 순수 함수(계획 검사·보호 상태·실행 상태 기계) → `/dr-plans`·`/dr-runs`(기능 `dr-plans`), 리더 스케줄러 `dr_scheduler.rs`(VMware 이주
계획 방식 — 진행은 Raft, 새 리더가 이어 감)가 지금 Task(`backup-restore`·`vm-start`)를 부름. 복원 API에 네트워크·보안 그룹·cloud-init 맵(기능 `restore-remap`).
경보 `dr_rpo_exceeded`·`dr_run_failed`. 웹 데이터 보호 › DR.

**Tech Stack:** Rust(orbivisor-types·orbivisor-api·orbivisorctl, utoipa), React 19·TypeScript·TanStack Query, vitest·msw. 새 의존성 없음.

**설계 문서:** `docs/superpowers/specs/2026-11-27-dr-orchestration-design.md`
**선행 계획:** 계획 1~75 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| 복구 계획(그룹·기다림·지연·시간 초과·네트워크/보안 그룹/시험 네트워크 맵·VM별 IP) | 연속·동기 복제 |
| 시험 장애 조치(SDN 거품 네트워크)·뒷정리·취소, 장애 조치 disaster·planned | 자동 장애 조치(사이트 감시) |
| 복원 API 맵(네트워크·보안 그룹·cloud-init 네트워크) | 게스트 안 스크립트, Windows IP |
| 보호 상태(RPO)·보고·RTO, 경보 둘·권하는 묶음, reprotect(되돌리기용 백업 작업) | 자동 되돌리기, PDF |
| CLI `dr …`, 웹 DR 화면 | |

## 실행 환경
- **이 Mac:** Rust 전부, 웹 전부, web-it.
- **svc01:** 일반 시험 묶음 전체.
- **GitHub 탐침:** 모든 작업.
- **실험실:** svc01·svc02는 한 클러스터라 서로를 원격으로 등록할 수 없다(`remote_is_local`). 배포 뒤 svc02에 따로 데이터 디렉터리·포트·소켓으로 한 노드 클러스터를
  하나 더 띄워(실험실 전용, VM 스코프·브리지가 겹치지 않게 VM은 그쪽에서만 작게) DR 쪽으로 쓰고, 운영 쪽 svc01 클러스터의 작은 VM 백업을 복제해 시험 장애 조치 →
  뒷정리 → disaster 장애 조치를 확인한다. 안 되면 원격 쪽은 API 시험(가짜 원격)이 맡고 그 까닭을 기록한다.

## Task

| 단계 | 내용 | 의존 |
|---|---|---|
| 1 | 타입·순수 함수: `RecoveryPlan{Spec,Status}`·검사, 보호 상태 계산, `DrRun`·단계 상태 기계(그룹 차례·기다림·시간 초과·on_timeout·취소·RTO), `RestoreRequest` 새 칸, 경보 조건 둘, 기능 상수 둘 | — |
| 2 | 복원 맵: 복원 API·Task가 네트워크·보안 그룹 맵과 cloud-init 네트워크를 씀(SDN IPAM 고정 주소), CLI `backup restore --network/--security-group/--ip`, OpenAPI | 1 |
| 3 | DR API: 계획 CRUD·권한·기능, 보호 상태 갱신(리더 1분), 보고, 경보 엔진 둘·`dr-defaults`, reprotect | 1 |
| 4 | DR 엔진: `dr_scheduler`(test·failover disaster/planned·cleanup·cancel), 거품 네트워크, 기다림(running·guest·ip), 실행 기록, 원격 프록시 부름 | 2·3 |
| 5 | CLI `dr …`·`alert rule add-dr-defaults` | 3·4 |
| 6 | 웹: 데이터 보호 › DR(목록·계획 편집·실행·확인), `gen:api` | 3·4 |
| 7 | 문서: `docs/guide/backup.md` DR 절·되돌리기 순서, architecture, index | 4~6 |

## 완료 기준 (계획 76)
- [ ] 이 Mac 전부, svc01 전부, GitHub 탐침 전부
- [ ] 리뷰 두 번(세 갈래) 고침
- [ ] 실험실: 시험 장애 조치 → 거품 네트워크에서 그룹 순서로 시작·RTO → 뒷정리, disaster 장애 조치
- [ ] main CI 통과

## 알고 받아들인 제한
설계 §7.

## 실행 후 기록

- **단계**(2026-11-27): 1 타입·상태 기계 → 2 복원 맵·3 DR API(동시) → 4 엔진(`dr/scheduler.rs`, 리더 5초, 진행은 DrRun 리소스) → 5 CLI·6 웹(동시) → 7 문서·작은 서버 고침.
  1단계에서 정한 것: IP는 복원 요청에 함께(라이브 복원), 실행은 리소스(계획 사본), 라벨 `orbivisor.io/dr`·`dr-test`·`dr-run`, 권한은 기본 규칙(읽기 cluster.read, 쓰기 cluster.admin).
- **리뷰 1**(backend·web·operator): 심각 1 — 계획된 이전이 사전 검사 없이 운영 VM부터 끔. 중요 — 리더 바뀜에 복원 두 번, 이름 겹침으로 시험 영구 막힘, 라벨 한 번만, 라이브 복원이 RTO에 무용,
  복구 지점 없는 VM, 시험 중 진짜 장애, 시험망 격리, 재해 뒤 두 벌, 걸린 실행. 고침: 사전 검사·되돌림, 실행 id 시각 기준 멱등, 라벨·뒷정리는 엔진이 만든 VM만, 라이브 복원은 켜지면 다음,
  `skip_vms`·`point_in_time`, 시험 끊기(preempt), fence-source·`dr_source_returned`, stalled, 웹 저장 충돌·확인 대화상자.
- **리뷰 2**: 심각 2 — 계획된 이전 사전 검사가 DR 쪽을 안 봄, fence가 이름만 보고 끔. 중요 — 앱이 두 사이트로 갈림, 두 계획이 같은 VM, 오래된 사본 숨김, 다시 돌린 장애 조치, 시험 끊기 경합,
  운영 VM 다시 켜기 한 번뿐. 고침: DR 쪽 준비·이름 겹침도 사전 검사, 준비는 전부 아니면 전무, fence는 운영 쪽 VM id·CA, `clear-fence`, 한 VM 한 계획, 만들 때 라벨, `lost_secs`·`partial`, 다시 켜기 30분.
- **이 Mac**: fmt·clippy·Rust 4681·웹 2481·Go vet·이름 시험·web-it 6 통과. **svc01**: 13단계 4820개 통과. **GitHub 탐침**: 21작업 모두 통과(한 번에). main CI 18작업 통과.
- **배포**: svc02 → svc01(업그레이드 길), 두 노드 `dr-plans`·`restore-remap` 알림, svc01 test-vm 그대로.
- **실험실 DR 시험**(2026-11-28, 계획 77 뒤): svc02에 두 번째 데몬(dr1 — 설정 `/etc/orbivisor-dr`, 포트 8016/8017, 데이터·run 디렉터리 따로, HA 펜싱 끔, systemd-run 임시 유닛)으로
  한 노드 클러스터 dr을 띄우고 lab(prod)과 서로 원격 등록(prod→dr operator 토큰, dr→prod viewer 토큰). test-vm 백업 → dr로 복제(출처 CA 지문 맞음) → 계획 `lab-app`(RPO ok).
  - 시험 장애 조치: 브리지 시험망은 `dr_test_network_not_isolated`로 먼저 막힘(업링크 없는 `drtest0`이라 `allow_routed_test_networks`로 진행) → RTO 11초, 시험 VM `test-vm-drt`는 새 MAC·시험망·dr에 없는 ISO는 빈 CD, prod 그대로. 뒷정리로 VM·볼륨·tap 모두 지워짐.
  - 재해 장애 조치: RTO 14초, dr-lan에 test-vm. prod가 살아 있어 `dr_source_returned`(critical)가 뜸, fence-source는 viewer 토큰이라 `dr_fence_preflight`로 분명히 거절, `clear-fence`로 경보 풀림.
  - prod에서 원본 백업을 지우자 dr 사본도 지워짐. 끝나고 dr 데몬·디렉터리·브리지·방화벽 규칙·원격·사용자를 걷어 냄.
  - **찾은 것**: 새 DR 클러스터에는 경보 규칙이 없어서 `alert rule add-dr-defaults`(알림 대상 1개 이상 필요)를 하기 전에는 `dr_source_returned`가 뜨지 않는다 — 두 벌이 도는 위험을 알리는 경보가 기본으로 없다. 계획 만들 때 DR 경보 규칙이 없으면 알리는 것을 계획 79(신뢰성)에.

## 다음 계획으로 넘길 사항

- 실험실 DR 시험(위).
- cloud-init: 복원한 VM은 새 instance-id라 첫 부팅 모듈이 다시 돔 — 원본 instance-id를 유지할 칸을 만들지 검토.
- 계획된 이전 준비의 동시성 제한, 운영 쪽 `GET /backups`를 VM마다 부르는 것.
