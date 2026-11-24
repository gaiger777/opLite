# Orbivisor 계획 70: 2.5D 클러스터 지도 — 장애 영역 판 위 노드 블록과 VM 타일, 부하 색·상태, 레이어, 라이브 마이그레이션 움직임, 큰 클러스터의 자세히 보기 단계 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 노드 표·VM 표·최적 배치·경보를 오가며 머리로 합치던 "어느 노드가 뜨겁고, 어디가 꺼졌고, 무엇이 옮겨 가나"를 **3D처럼 보이는 2D 지도 한 장**으로 보인다. 인프라 › 노드 탭의 보기 **"지도"**(`/cluster/map` — 노드 | 지도 | 클러스터, 사이드바 항목 없음): 노드 레이블 `zone`(·`rack`)마다 바닥 판, 그 위 노드 아이소메트릭 블록(윗면 부하 열 색, 옆면 메모리·CPU 채움), 노드 위에 쌓인 VM 타일(상태 색·HA), 오프라인·유지보수·비우는 중·얼림·증인·리더·경보 수, 원본 → 대상으로 흐르는 라이브 마이그레이션 화살표와 진행률, 켜고 끄는 레이어(부하·스토리지·네트워크·HA·추천), 가리키기 카드와 누르면 지금 있는 미리 보기 서랍·동작. WebGL·three.js·캔버스 3D 엔진은 쓰지 않는다.

**Architecture:**
- **그리기는 SVG 한 장 + 계획 67 그림 조각**(`web/src/components/iso/` — `iso()` 투영, `shade()` 면 밝기, `byDepth`, `IsoBlock`·`IsoSlab`·`IsoTag`·`IsoLink`, `useIsoViewport`). 새로 더하는 것은 일반 함수: `boxesPath`·`facesPath`(같은 층·색·면의 다각형을 `<path>` 하나로 — 계획 67 §18이 미룬 최적화), `screenToWorld`(역투영), `IsoGauge`(채움 면).
- **순수 파이프라인**(`web/src/clusterMap/`): 질의 → `toModel` → `layoutCluster`(판·격자·타일 칸, 같은 입력 같은 그림, 타일 자리는 이름 순 고정) → `lodFor`(요약·타일·자세히, 화면 픽셀 크기와 이력) → `scenePaths`(합친 경로) → `hitTest`(화면 점 → 노드·층·칸, 위 층 먼저)·`neighbor`(두 단계 키보드). 가리키기·초점·끌기는 블록을 다시 그리지 않는다(`memo`, 밝히기는 `<style>` 한 줄, 초점 타일은 테두리 원소 하나).
- **서버 변경 없음**: `useList(node·vm·storage-pool)`(watch), `GET /placement`(부하 0.1% 단위·추천, 최적 배치 화면과 같은 질의 열쇠 15초, 503이면 "부하 모름"), `GET /cluster`(리더, 403이면 없이), `useAlerts()`, Task 진행률. 첫 그림은 노드·VM 목록만으로, 나머지는 오는 대로 얹는다. watch 사건은 1초 모아 다시 배치.
- **다루기는 토폴로지 규칙 그대로**: 탭 차례 하나, 화살표 이웃, `]`·`[`로 노드 안 VM 단계에 들고 남(지도에 초점이 있을 때만 — 그림 밖 `[`는 사이드바), `Enter` 서랍, `Shift+Enter` 상세, `aria-label` + `aria-live` 줄, "그림 | 목록" 전환(나무 목록), 동작 줄이기면 움직임 없음, 768px 미만은 "보기 설정" 서랍 하나와 요약 단계.
- **동작은 지금 것을 다시 쓴다**: 노드 서랍 `NodePeek` + 그 노드 VM 목록 + `useNodeActions`, VM 서랍 `VmPeek` + 전원·**옮기기…**(`VmMigrateDialog`) + `VmMenu`, 추천 화살표의 "적용"(`POST /placement/recommendations/{id}/apply`). **끌어 놓아 옮기기는 계획 71**(끌어 놓기 VM 만들기)의 끌기 틀과 함께 — 이번에는 넣지 않는다.

**Tech Stack:** React 19·TypeScript·Vite(늦게 읽는 경로 조각), SVG·CSS(`color-mix`, `stroke-dashoffset` 키프레임, `prefers-reduced-motion`), TanStack Query·지금 watch(`LiveProvider`), react-i18next(en·ko), vitest + msw + axe-core + `test/contrast.ts`, Playwright(opld `web-it`), `eager.sh`(첫 화면 묶음 gzip). 새 의존성 없음.

**설계 문서:** `docs/superpowers/specs/2026-11-24-cluster-view-design.md`(상위: `2026-11-18-ui-ux-redesign-design.md`, `2026-11-21-sdn-ovn-plan67-design.md` §12, `2026-11-18-dashboard-metrics-notifications-design.md`, `2026-11-11-availability-scale-design.md`)
**선행 계획:** 계획 1~69 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| 인프라 › 노드 탭의 보기 "지도" `/cluster/map`, 늦게 읽는 조각, 마지막 보기 기억 | 새 사이드바 항목·영역·홈 위젯 |
| 묶기 `zone`(기본)·`rack`·고른 레이블 키·없음, "영역 없음" 판, 증인 기둥, "배치 안 됨" 판 | 랙 안 U 자리·평면도 |
| 노드 블록(부하 열 색·채움·과다 할당·상태 꼬리표·경보 수), VM 타일(상태 색·HA·규칙 어김·"+N"·"+N 볼 수 없음") | VM 크기를 타일 크기로 |
| 레이어 부하·스토리지·네트워크·HA·추천, 라이브 마이그레이션·비우기 움직임과 진행률 | 노드 사이 네트워크 선, 시간 되감기 |
| 가리키기 카드, 서랍과 지금 동작(전원·옮기기 대화상자·노드 동작·추천 적용), `?focus=` | 끌어 놓아 옮기기(계획 71), 여러 개 고르기 |
| 자세히 보기 세 단계·합친 경로·적중 셈, 100 노드 × 50 VM 예산 | Web Worker·culling |
| 키보드·화면 읽기·목록 보기·휴대폰·밝음/어둠, vitest·axe·대비·Playwright·번들 | 서버 변경 |

## 실행 환경

- **이 Mac(aarch64):** 모든 웹 변경, `pnpm typecheck`·`lint`·`test`·`build`, 성능 시험 기록(배치 시간·원소 수), `eager.sh` 번들 재기, opld `web-it` 브라우저 E2E(Homebrew QEMU HVF — 한 노드), VoiceOver 손 확인.
- **GitHub:** 웹만 바뀌므로 `check`·`web` 작업이 통과하면 된다. `linux-kvm`·`ldap`·`package`는 바뀐 것이 없어 따로 탐침하지 않는다(도는 그대로 통과만 본다).
- **배포 서버 svc01(Rocky 10 aarch64, `qemu-tcg`):** 일반 시험 묶음 전체(웹 묶음을 넣은 빌드).
- **테스트 VM:** svc01·svc02에 배포해 지도에서 두 노드(판 하나, `zone` 레이블을 주면 판 둘)·test-vm 타일·리더 꼬리표를 보고, test-vm을 svc01 ↔ svc02로 라이브 마이그레이션해 화살표·진행률·유령 타일을 손으로 확인(TCG라 느려 보기 좋다), 한 노드 유지보수 시작·끝의 줄무늬, 휴대폰 폭.

## 설계 결정

- **SVG + 합친 경로, WebGL·Canvas 아님:** 사용자가 진짜 3D를 막았고, Canvas는 원소마다 초점·`aria-label`·테마 토큰을 잃는다. 토폴로지 방식(타일마다 `<g>` + 다각형 셋)이면 5000 VM에서 약 2만 원소 — 층·색·면마다 경로 하나로 합쳐 노드당 수십 개(타일 단계 3,000개 밑), 적중은 순수 셈.
- **장애 영역 판 + 결정적 격자:** 노드 레이블 `zone`(`[node] zone`이 등록 때 레이블로 — HA·배치 규칙이 이미 장애 영역으로 씀)이 판이 된다. 같은 입력 같은 그림, 타일 자리는 이름 순 고정이라 상태가 바뀌어도 흔들리지 않는다. 힘 배치·트리맵은 버림.
- **부하는 `/placement` 요약:** 리더가 리스로 모은 노드마다 한 줄(질의 한 번, 최적 배치 화면과 캐시 공유). `/metrics/nodes`는 노드마다 퍼져 묻는다. `age_secs`가 없으면 대체값을 부하처럼 보이지 않고 "부하 모름" 빗금.
- **서버 묶음 응답을 만들지 않음:** 노드·VM은 이미 watch로 살아 있고, 묶음 응답은 권한·감추기·버전 섞임 규칙을 하나 더 만든다. 첫 그림은 목록 둘이면 된다.
- **자리는 노드 탭의 보기 `/cluster/map`:** 계획 61 규칙(새 사이드바 항목 없음). `/nodes/map`은 노드 이름 `map`의 상세와 겹친다. 보기 전환이 곧 "표 ↔ 지도" 전환이고 영역이 마지막 보기를 기억한다.
- **두 단계 키보드(`]`·`[`):** 화살표를 노드 사이와 타일 사이에 함께 쓰면 다툰다. 나무 목록의 펼치기·접기와 같은 뜻의 키를 지도에 초점이 있을 때만 받는다(전역 `[` 사이드바 접기는 그림 밖에서 그대로).
- **끌어 놓아 옮기기는 계획 71로:** 그림 끌기는 보기 움직이기이고(터치에서 실수), 놓을 자리 판정·키보드 대체·확인을 계획 71 끌기 틀과 한 번에 만든다. 이번에는 VM 서랍의 "옮기기…"가 같은 `VmMigrateDialog`·같은 API다.
- **움직임은 CSS만, 동작 줄이기 존중:** `stroke-dashoffset` 흐름·맥박은 그리기만 하고 레이아웃을 다시 하지 않는다. 지도 전용 모양(키프레임·무늬)은 조각 안 `<style>`·`<defs>`에 — 첫 화면 CSS에 넣지 않는다.

## Task

각 Task(스테이지)는 하나의 커밋이며, 앞 Task의 커밋 위에서 순서대로 한다. 스테이지마다 혼자 컴파일되고 시험된다(`pnpm typecheck`·`lint`·`test`). 화면은 스테이지 2에서 열린다. 설계와 다른 것·결정은 설계 문서 §18에 스테이지마다 남긴다.

- [ ] **Task 1: 클러스터 지도 배치 — 노드 묶기(zone·rack·레이블)·노드 블록·VM 타일 쌓기·자세히 보기 세 단계·부하 색·합친 경로·적중 검사·키보드 이웃(순수)(web)** — `web/src/clusterMap/{model,layout,lod,paths,hit,keys,labels}.ts`와 시험(`toModel` 템플릿·배치 안 됨·알 수 없는 노드·증인·마이그레이션·볼 수 없는 VM 수·0.1% 단위·부하 모름, `layoutCluster` 결정성·묶기 넷·"영역 없음"·자리 고정·"+N"·줄 간격·깊이, `lodFor` 문턱·이력, `scenePaths` 경로 수, `hitTest` 모든 칸 왕복·위 층 먼저, `neighbor` 격자·판 넘기·`]`·`[`·`Home`·`End`, `heatTone`, `migrationLinks`, 화면 읽기 이름 en·ko), `web/src/components/iso/`의 `boxesPath`·`facesPath`·`screenToWorld`·`IsoGauge`와 시험, `ClusterMapScene.perf.test.tsx`(100 × 50 배치 + 경로 30 ms 밑, 단계마다 원소 수 예산 — 장면 셈), i18n `clusterMap.label.*`.
- [ ] **Task 2: 클러스터 지도 화면 — 인프라 › 노드의 보기 "지도", 2.5D 그림·가리키기 카드·서랍과 그 자리의 동작·목록 보기·레이어(부하·스토리지·네트워크·HA·추천)·라이브 마이그레이션 움직임·휴대폰(web)** — `nav.ts` 보기(노드 | 지도 | 클러스터)·`App.tsx` 늦은 경로 `cluster/map`, `web/src/clusterMap/{data.ts,ClusterMapPage,MapScene,MapCard,MapSheet,MapList,Legend}.tsx`(질의 합치기·1초 모으기, 상태 모양·무늬, 레이어, 화살표·유령 타일·진행률, 서랍 동작, `?focus=`, 기억 `clusterMap.{mode,group,layers,filter}`), `lib/shortcuts.ts`에 지도 안 `]`·`[`, 팔레트 별칭, i18n en·ko, vitest(상태 모양·503·403·서랍·옮기기 대화상자·추천 적용 본문·키보드·그림 ↔ 목록·동작 줄이기·768px 미만·axe·대비 두 테마·`nav.test`·`routes.test`·단축키), 성능 시험에 가리키기·끌기 다시 그리기 0, VoiceOver 손 확인.
- [ ] **Task 3: 클러스터 지도 브라우저 E2E — 노드 보기 전환·키보드 서랍·목록 보기·휴대폰 폭, 번들 크기 기록(web·opld)** — `web/e2e/vm.spec.ts` 한 줄기(넓은 화면: 보기 "지도" → 노드 블록·VM 타일 → 키보드 `]`·`Enter` 서랍 → 목록 → 보기 "노드" → 인프라 다시 누르면 기억한 보기; 390 × 844: 가로 스크롤 없음·"보기 설정"), 필요하면 `crates/opld/tests/web.rs`, `eager.sh` 앞뒤·지도 조각 gzip을 §18에.
- [ ] **Task 4: 웹 가이드의 클러스터 지도, 설계 문서 스테이지 보고, 계획 70 실행 기록(docs)** — `docs/guide/web-ui.md`(영역 표의 노드 보기, 새 절 "클러스터 지도 화면", 단축키 표의 지도 안 키, 한계), `docs/guide/cluster.md`·`ha.md`(zone·rack 레이블이 지도의 판 — 한 줄씩), `docs/index.md`, README 한 줄(화면 목록이 있으면), 이 문서.

## 완료 기준 (계획 70)

- 서버·API·OpenAPI·`schema.d.ts`가 바뀌지 않는다(브랜치 diff에 `crates/`·`schema.d.ts` 변경 없음 — 시험 환경 값 말고).
  - **결과:** (채움)
- 인프라 › 노드 탭 보기가 노드 | 지도 | 클러스터이고 사이드바·영역이 그대로다. `/cluster/map`은 늦게 읽는 조각이고 이동 경로 `인프라 › 지도`, 영역이 마지막 보기를 기억하며 팔레트로 간다. 노드 읽기 권한이 없으면 보기가 숨는다.
  - **결과:** (채움)
- 지도가 `zone`·`rack`·고른 레이블·없음으로 판을 나누고, 노드 블록의 부하 열 색·채움·과다 할당, 오프라인·유지보수·비우는 중·얼림·증인·리더·경보 수, VM 타일의 상태 색·HA·규칙 어김·"+N"·"+N 볼 수 없음", "배치 안 됨" 판을 보인다. `/placement` 503·`age_secs` 없음은 "부하 모름", `/cluster` 403은 리더 없이 그린다.
  - **결과:** (채움)
- 라이브 마이그레이션은 원본 → 대상 화살표·진행률·유령 타일로 보이고 끝나면 대상 자리로 들어가며, 동작 줄이기면 움직임이 없다. 추천 레이어의 "적용"이 최적 배치 화면과 같은 요청이다.
  - **결과:** (채움)
- 가리키기·초점 카드, 누르면 노드·VM 서랍과 지금 동작(전원·옮기기 대화상자·노드 동작), `Shift+Enter`·더블클릭 상세, 서랍을 닫으면 초점이 그 원소로 돌아온다.
  - **결과:** (채움)
- 키보드만으로 모든 노드·VM에 닿고(화살표·`]`·`[`·`Home`·`End`·`Enter`), 그림 밖 `[`는 사이드바를 접는다. 원소마다 화면 읽기 이름과 `aria-live` 줄, "목록으로 보기"가 같은 정보를 준다. axe 위반 없음, 상태 색·글 대비가 두 테마에서 3:1·4.5:1 이상.
  - **결과:** (채움)
- 100 노드 × 50 VM에서 배치 + 경로 30 ms 밑, SVG 원소가 요약 1,500·타일 3,000·레이어 모두 4,000개 밑이고, 가리키기·끌기·초점 이동이 블록을 다시 그리지 않는다.
  - **결과:** (채움)
- 390px 폭에서 가로 스크롤이 없고 도구 띠가 "보기 설정" 하나로 접히며 요약 단계로 그린다.
  - **결과:** (채움)
- 지도 조각 gzip 20 KB 밑, 첫 화면 묶음 증가 3 KB 밑(`eager.sh`).
  - **결과:** (채움)
- `pnpm typecheck`·`lint`·`test`·`build`, 로컬 브라우저 E2E가 통과하고 svc01 일반 시험과 GitHub `check`·`web`을 포함한 모든 작업이 통과한다.
  - **결과:** (채움)

## 알고 받아들인 제한

- **부하는 리더의 요약:** (채움 — `/placement` 15초 주기, 리더에 못 물으면 "부하 모름", 리스 요약의 나이)
- **끌어 놓아 옮기기 없음:** (채움 — 계획 71, VM 서랍의 "옮기기…"로)
- **큰 클러스터:** (채움 — 노드 200 넘으면 판 하나 보기 권함, culling·Worker 없음, 측정값)
- **타일 초점:** (채움 — 합친 경로라 타일마다 DOM 초점이 없고 테두리를 덧그림, 화면 읽기는 `aria-live` 줄)
- **레이어:** (채움 — 노드 사이 네트워크 선 없음, 옛 노드의 빈 칸은 그리지 않음)
- **움직임:** (채움 — 진행률은 Task `progress`가 있을 때만, 한 노드 E2E에는 마이그레이션 없음)

## 실행 후 기록

- 리뷰: (채움)
- svc01 일반 시험: (채움)
- GitHub: (채움)
- 웹 번들(eager, gzip -9): 계획 69 끝 476,367 → 스테이지 1 477,319 → 스테이지 2·3 478,317 B(계획 70 합 +1,950 B, 목표 3 KB 밑). 늦은 조각: 지도 18,750 B(목표 20 KB 밑)·서랍 2,516 B·문구 en 1,731 B·ko 2,101 B·불러오기 308 B.
- 배포: (배포 후 채움)

## 다음 계획으로 넘길 사항

- **로드맵 다음**: **계획 71 — 끌어 놓기 VM 만들기**(이 지도에 타일 → 노드 끌어 옮기기를 함께, 설계 §16).
- 다른 화면의 "지도에서 보기"(`?focus=`), 홈의 작은 지도 위젯, 시간 되감기, 노드 사이 네트워크 선, 랙 U 자리, 원격 클러스터 지도, 그림에서 여러 개 고르기, 노드 500 넘는 클러스터(Worker·culling).
