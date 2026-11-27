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
(실행 뒤 채움)
