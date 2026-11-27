# Orbivisor 계획 77: 웹 UI/UX 전면 개편 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 오류가 다 보이고 원인·해결을 알며, 메뉴는 2단 사이드바 하나, 모든 설정에 설명·그림, VM 만들기는 스크롤 없는 그림 한 화면, 해상도에서 안 깨지고, 선언 묶음·기록이 사람 말, 지도 VM 100개 이상·업로드 진행.
**설계:** `docs/superpowers/specs/2026-11-27-ux-overhaul-design.md`, 진단 `docs/superpowers/specs/2026-11-27-ux-audit.md`.
**선행:** 계획 1~76.

## 단계 (이 차례로, 단계마다 리뷰·스크린샷 비교 후 main에)
| 단계 | 내용 | 서버 |
|---|---|---|
| 77-1 | 오류가 보이게: Task 오류 잘림 제거, Task 상세 서랍, 대상 이름, API 오류 설명 함께 | `requested_by` 이름·`started_at`·`finished_at`·`target_name` |
| 77-3 | 2단 사이드바·메뉴 재편(묶음 9 + 관리), 영역 탭·보기 전환 제거, 옛 경로 넘김 | 없음 |
| 77-4 | 도움말 체계(help·ⓘ 패널·그림)·쉬운 말·칸 240개 설명·빈 상태 | 없음 |
| 77-5 | VM 만들기 그림 한 화면(폼·빌더 합침), VM 상세 하드웨어 같은 그림 | 배치 미리보기 |
| 77-2 | 실패 원인·해결: `TaskFailure`, 상위 20경로 code, 웹 사전 | 큼 |
| 77-6 | 해상도·밀도(표·행 단추·홈·노드·설정·역할·VM 목록 페이지·차트 시각) | 없음 |
| 77-7 | 선언 묶음·변경 기록·감사 쉬운 화면 | 감사 동작 키 |
| 77-8 | 지도 100+ VM LOD·칸 예산, 조각 업로드·진행 | 조각 업로드 API |

## 첫 화면 묶음 예산
- 첫 화면 묶음(index.html의 module script + modulepreload, gzip -9) + 고른 언어의 첫 화면 문구 조각 하나가 계획 76 끝(479 KB)을 넘지 않는다. 첫 화면 문구(`web/src/i18n/{ko,en}.json`)는 언어마다 늦게 받는 조각이고 다른 언어는 바꿀 때 받는다(77-4 리뷰). 77-4 리뷰 고침 뒤: ko 427 KB, en 420 KB(77-4 처음 509 KB).
- 단계마다 새 문구가 첫 화면 묶음을 3 KB 넘게 늘리면 그 화면 조각이나 모듈 묶음(`<모듈>/strings.ts`)으로 늦게 받는다.

## 완료 기준
- [ ] 단계마다 수용 기준(설계 "목표와 수용 기준")·스크린샷 비교·리뷰
- [ ] 이 Mac·svc01·GitHub 탐침, main CI
- [ ] 실험실 배포 뒤 화면 확인

## 실행 후 기록
(실행 뒤 채움)
