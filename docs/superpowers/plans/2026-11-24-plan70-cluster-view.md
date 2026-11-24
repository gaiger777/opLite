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

- [x] **Task 1: 클러스터 지도 배치 — 노드 묶기(zone·rack·레이블)·노드 블록·VM 타일 쌓기·자세히 보기 세 단계·부하 색·합친 경로·적중 검사·키보드 이웃(순수)(web)** — `web/src/clusterMap/{model,layout,lod,paths,hit,keys,labels}.ts`와 시험(`toModel` 템플릿·배치 안 됨·알 수 없는 노드·증인·마이그레이션·볼 수 없는 VM 수·0.1% 단위·부하 모름, `layoutCluster` 결정성·묶기 넷·"영역 없음"·자리 고정·"+N"·줄 간격·깊이, `lodFor` 문턱·이력, `scenePaths` 경로 수, `hitTest` 모든 칸 왕복·위 층 먼저, `neighbor` 격자·판 넘기·`]`·`[`·`Home`·`End`, `heatTone`, `migrationLinks`, 화면 읽기 이름 en·ko), `web/src/components/iso/`의 `boxesPath`·`facesPath`·`screenToWorld`·`IsoGauge`와 시험, `ClusterMapScene.perf.test.tsx`(100 × 50 배치 + 경로 30 ms 밑, 단계마다 원소 수 예산 — 장면 셈), i18n `clusterMap.label.*`.
- [x] **Task 2: 클러스터 지도 화면 — 인프라 › 노드의 보기 "지도", 2.5D 그림·가리키기 카드·서랍과 그 자리의 동작·목록 보기·레이어(부하·스토리지·네트워크·HA·추천)·라이브 마이그레이션 움직임·휴대폰(web)** — `nav.ts` 보기(노드 | 지도 | 클러스터)·`App.tsx` 늦은 경로 `cluster/map`, `web/src/clusterMap/{data.ts,ClusterMapPage,MapScene,MapCard,MapSheet,MapList,Legend}.tsx`(질의 합치기·1초 모으기, 상태 모양·무늬, 레이어, 화살표·유령 타일·진행률, 서랍 동작, `?focus=`, 기억 `clusterMap.{mode,group,layers,filter}`), `lib/shortcuts.ts`에 지도 안 `]`·`[`, 팔레트 별칭, i18n en·ko, vitest(상태 모양·503·403·서랍·옮기기 대화상자·추천 적용 본문·키보드·그림 ↔ 목록·동작 줄이기·768px 미만·axe·대비 두 테마·`nav.test`·`routes.test`·단축키), 성능 시험에 가리키기·끌기 다시 그리기 0, VoiceOver 손 확인.
- [x] **Task 3: 클러스터 지도 브라우저 E2E — 노드 보기 전환·키보드 서랍·목록 보기·휴대폰 폭, 번들 크기 기록(web·opld)** — `web/e2e/vm.spec.ts` 한 줄기(넓은 화면: 보기 "지도" → 노드 블록·VM 타일 → 키보드 `]`·`Enter` 서랍 → 목록 → 보기 "노드" → 인프라 다시 누르면 기억한 보기; 390 × 844: 가로 스크롤 없음·"보기 설정"), 필요하면 `crates/opld/tests/web.rs`, `eager.sh` 앞뒤·지도 조각 gzip을 §18에.
- [x] **Task 4: 웹 가이드의 클러스터 지도, 설계 문서 스테이지 보고, 계획 70 실행 기록(docs)** — `docs/guide/web-ui.md`(영역 표의 노드 보기, 새 절 "클러스터 지도 화면", 단축키 표의 지도 안 키, 한계), `docs/guide/cluster.md`·`ha.md`(zone·rack 레이블이 지도의 판 — 한 줄씩), `docs/index.md`, README 한 줄(화면 목록이 있으면), 이 문서.

## 완료 기준 (계획 70)

- 서버·API·OpenAPI·`schema.d.ts`가 바뀌지 않는다(브랜치 diff에 `crates/`·`schema.d.ts` 변경 없음 — 시험 환경 값 말고).
  - **결과:** 브랜치 diff(`main...HEAD`)의 `crates/`는 `crates/opld/tests/web.rs` 머리 설명 한 줄뿐이고 `web/src/api/schema.d.ts`·OpenAPI·서버 코드는 그대로다. 지도는 이미 있는 `useList(node·vm·storage-pool)`·`/placement`·`/cluster`·`/alerts`·Task 진행을 읽는다.
- 인프라 › 노드 탭 보기가 노드 | 지도 | 클러스터이고 사이드바·영역이 그대로다. `/cluster/map`은 늦게 읽는 조각이고 이동 경로 `인프라 › 지도`, 영역이 마지막 보기를 기억하며 팔레트로 간다. 노드 읽기 권한이 없으면 보기가 숨는다.
  - **결과:** `nav.test`(보기 차례 `/nodes`·`/cluster/map`·`/cluster`, `areaOf("/cluster/map")` = 인프라, 이동 경로 `Infrastructure › Map`, 역할별 보이는 탭 표 — 보기는 탭의 `kind: "node"`를 따라 노드 읽기가 없으면 탭과 함께 숨음), `routes.test`(늦게 읽는 보기, 그림 밖 `[`는 사이드바를 접고 그림 안 `[`·`]`는 그림이 받음), `CommandPalette.test`(별칭 "map·topology·3d"·"지도·클러스터 보기"), 브라우저 E2E가 보기 "Map" → 보기 "Nodes" → 사이드바 인프라를 다시 누르면 기억한 보기를 본다. 사이드바·영역 표는 그대로.
- 지도가 `zone`·`rack`·고른 레이블·없음으로 판을 나누고, 노드 블록의 부하 열 색·채움·과다 할당, 오프라인·유지보수·비우는 중·얼림·증인·리더·경보 수, VM 타일의 상태 색·HA·규칙 어김·"+N"·"+N 볼 수 없음", "배치 안 됨" 판을 보인다. `/placement` 503·`age_secs` 없음은 "부하 모름", `/cluster` 403은 리더 없이 그린다.
  - **결과:** 순수 시험(`model`·`layout`·`labels` — 묶기 넷·"영역 없음"·레이블이 없으면 판 하나·증인·배치 안 됨·템플릿 뺌·"+N"·"+N 볼 수 없음"·0.1% 단위·`age_secs` 없음 → 부하 모름)과 화면 시험 `draws node blocks with their state and the VMs on them`(오프라인·유지보수·얼림·증인·리더·경보 수·과다 할당), `shows unknown load and blocks recommendations when /placement is unavailable`(503 + `/cluster` 403 → 리더 없이), `blocks the recommendations layer when the balancer is off`.
- 라이브 마이그레이션은 원본 → 대상 화살표·진행률·유령 타일로 보이고 끝나면 대상 자리로 들어가며, 동작 줄이기면 움직임이 없다. 추천 레이어의 "적용"이 최적 배치 화면과 같은 요청이다.
  - **결과:** `model.test`·`labels.test`(원본 → 대상 화살표, 진행률·단계 글, 추천과 진행 중이 겹치면 하나, 유령 타일 칸), 화면 시험 `animates the migration arrow otherwise`·`stops the migration animation when the user asks for reduced motion`, 브라우저 E2E의 `emulateMedia` 동작 줄이기(그림 `<style>`에 `@keyframes` 없음), `applies a placement recommendation from its arrow after confirming`(최적 배치 화면과 같은 `POST /placement/recommendations/{id}/apply` 본문). 끝난 타일의 0.2초 나타남은 하지 않았다(§18 스테이지 2). 실제 두 노드 움직임은 svc01·svc02 배포 확인에서 본다(아래 배포).
- 가리키기·초점 카드, 누르면 노드·VM 서랍과 지금 동작(전원·옮기기 대화상자·노드 동작), `Shift+Enter`·더블클릭 상세, 서랍을 닫으면 초점이 그 원소로 돌아온다.
  - **결과:** 화면 시험 `opens the node drawer with its VMs and node actions, then a VM drawer with Migrate`(노드 서랍 = `NodePeek` + 노드 동작 + VM 목록, VM 서랍 = "옮기기…" → `VmMigrateDialog`), 키보드 시험의 `Shift+Enter` 주소, 브라우저 E2E(진짜 가리키기 카드·타일 누르기 → VM 서랍, `Esc`로 닫으면 초점이 노드 `<g>`로). 더블클릭·Ctrl/⌘+누르기는 같은 `onDetail`.
- 키보드만으로 모든 노드·VM에 닿고(화살표·`]`·`[`·`Home`·`End`·`Enter`), 그림 밖 `[`는 사이드바를 접는다. 원소마다 화면 읽기 이름과 `aria-live` 줄, "목록으로 보기"가 같은 정보를 준다. axe 위반 없음, 상태 색·글 대비가 두 테마에서 3:1·4.5:1 이상.
  - **결과:** `keys.test`(무작위 클러스터에서 모든 노드·타일에 닿음, 판 넘기·`]`·`[`·`Home`·`End`), 화면 시험 `moves with the keyboard: …`·`focuses the element named in ?focus=`·`switches to the list view and remembers …`, `MapScene.dom.test`의 요약 단계 `]` → VM 목록 서랍, `routes.test`의 그림 밖 `[` = 사이드바, `labels.test`(en·ko 이름), 브라우저 E2E(`]` → aria-live "VM web, Running, node node1"). axe는 그림·목록·서랍과 두 테마 레이어 모두(`has no axe violations in the %s theme with every layer on`), `colors.test`가 상태 색·뜨거운 윗면·받침·업링크·화살표를 두 테마의 `--card`·`--background` 위 3:1로 잰다(글은 지금 토큰이라 `tokens.test`의 4.5:1). VoiceOver 손 확인은 아직이다(배포 확인 때).
- 100 노드 × 50 VM에서 배치 + 경로 30 ms 밑, SVG 원소가 요약 1,500·타일 3,000·레이어 모두 4,000개 밑이고, 가리키기·끌기·초점 이동이 블록을 다시 그리지 않는다.
  - **결과:** `ClusterMapScene.perf.test`: 100 × 50 `toModel` + `layoutCluster` + `scenePaths` 이 Mac 12~15 ms(예산 30 ms), 원소 셈 요약 1,172·타일 2,251·레이어 모두 2,751(스테이지 2에서 그리는 것과 꼭 같게 다시 센 값 요약 1,279(레이어 모두 1,499)·타일 2,358(2,578)), 20 × 20 자세히 794. `MapScene.dom.test`가 그린 SVG 원소 수 = 셈을 보고 `does not redraw node blocks on hover, keyboard moves or drags`가 블록 다시 그리기 0을 본다.
- 390px 폭에서 가로 스크롤이 없고 도구 띠가 "보기 설정" 하나로 접히며 요약 단계로 그린다.
  - **결과:** 화면 시험 `folds the toolbar into one View settings sheet on a phone`, 브라우저 E2E 390 × 844(레이어 칸이 도구 띠에 없고 "View settings" 서랍에 있음, 문서 `scrollWidth ≤ clientWidth`). 요약 단계는 맞추기 배율(타일 6px 밑)로 저절로 — 휴대폰을 따로 고르지 않는다.
- 지도 조각 gzip 20 KB 밑, 첫 화면 묶음 증가 3 KB 밑(`eager.sh`).
  - **결과:** `eager.sh`(gzip -9): 계획 69 끝 476,367 → 478,317 B(**+1,950 B**, 목표 3 KB 밑 — 지도 문구를 늦게 받는 조각으로 옮김, §18 스테이지 2). 지도 조각 18,750 B(목표 20 KB 밑), 서랍 2,516 B, 문구 en 1,731 B·ko 2,101 B, 불러오기 308 B.
- `pnpm typecheck`·`lint`·`test`·`build`, 로컬 브라우저 E2E가 통과하고 svc01 일반 시험과 GitHub `check`·`web`을 포함한 모든 작업이 통과한다.
  - **결과:** 이 Mac에서 스테이지마다 `pnpm typecheck`·`lint`·`test`·`build`와 `cargo test -p opld --features web-it --test web`(셋 통과, 파일 복원 줄기는 환경 값이 없어 건너뜀)이 통과했다. 스테이지 4 커밋 앞 vitest 1,717개(168 파일). svc01·GitHub은 아래 "svc01 일반 시험"·"GitHub"에 채운다.

## 알고 받아들인 제한

- **부하는 리더의 요약:** `/placement`를 15초마다(최적 배치 화면과 같은 캐시) 읽는다. 리더에 묻지 못하면(503) 옛 값을 쓰지 않고 모든 블록이 "부하 모름" 빗금이고 추천 레이어가 막힌다. 노드의 리스 요약이 없으면(`age_secs` 없음) 그 노드만 "부하 모름". 리더가 모은 값이라 몇 초 늦다.
- **끌어 놓아 옮기기 없음:** 그림 끌기는 보기 움직이기다. 타일 → 노드 끌기는 계획 71(끌어 놓기 VM 만들기)의 끌기 틀(놓을 자리 판정·키보드 대체·확인)과 함께 넣는다. 지금은 VM 서랍의 "옮기기…"가 같은 `VmMigrateDialog`·같은 API다.
- **큰 클러스터:** 노드 200을 넘으면 판 하나 보기를 권하는 띠. Web Worker 배치·화면 밖 판 그리지 않기(culling)는 없다. 100 × 50에서 배치 + 경로 12~15 ms, 원소 요약 1,279·타일 2,358·레이어 모두 2,578(그린 것). 보이는 VM이 3,000을 넘으면 늘 요약 단계.
- **타일 초점:** 타일은 층·색·면마다 합친 경로 조각이라 DOM 초점이 없다. 화면 초점은 노드 `<g>`에 두고 초점·가리킨 타일 테두리를 덧그리며, 화면 읽기는 `aria-live` 줄이 타일 이름을 읽는다(처음 초점은 노드의 `aria-label`). 끝난 마이그레이션 타일의 나타남 움직임도 같은 까닭으로 없다.
- **레이어:** 노드 사이 네트워크 선은 그리지 않는다(노드 서랍의 "네트워크 토폴로지에서 보기"). 인터페이스·펜싱 정보가 없는 옛 노드·macOS 노드·증인은 업링크 띠·펜싱 점선을 그리지 않는다(빈 칸을 "나쁨"으로 보이지 않음). 꽂지 않은 남는 포트는 업링크로 세지 않는다. 카드의 로컬 풀은 막대가 아니라 글.
- **움직임:** 진행률은 마이그레이션 Task의 `progress`(`done_bytes`/`total_bytes`)가 있을 때만이고 없으면 단계 이름. 한 노드 브라우저 E2E에는 마이그레이션이 없어 vitest와 배포 확인이 맡는다. "비우는 중 n/m"의 n은 지금 나가는 것만(이미 나간 수는 기록이 없음).

## 실행 후 기록 (2026-11-24)

- 4개 스테이지를 단계 커밋으로 구현했다(설계·계획 `f5e77d14`, `dd1879e2` 순수 배치, `381c46f6` 화면, `5a1eb5ed` 브라우저 E2E, 이 문서 커밋). 설계와 다른 것·결정은 설계 문서 §18에 스테이지마다 남겼다. 주요한 것: 타일 20 × 20 × 6·간격 6(높이 ≤ 간격이라 한 층 안 타일이 겹치지 않아 합친 경로 차례가 정확), 노드마다가 아니라 **띠(열 + 줄이 같은 노드) · 층 · 색 · 면**마다 경로, 옆면 둘을 한 경로로 + 오른쪽 면 검은 반투명 그늘, 줄 간격은 앞 줄의 층 수로, 키보드 화살표는 판 격자 축(화면 방향 그대로면 닿지 못하는 노드가 생김), **지도 문구를 첫 화면 묶음 밖**(`web/src/clusterMap/i18n/{en,ko}.json`, 경로를 열 때 `addResourceBundle`), 서랍은 늦게 받는 조각 하나 더(`MapSheet`), `PLACEMENT_KEY`를 `placement/balance.ts`로, 원소 셈을 그리는 것과 꼭 같게(`nodeMarks`·`FRAME_ELEMENTS` 19), 이름·꼬리표는 블록 앞 면 위, 꺼진 VM 색은 `--muted-foreground` 그대로(옅은 섞음은 3:1이 안 됨), 적중은 `getScreenCTM()`의 역, 단축키 묶음 `map`(도움말에만), 끝난 타일 나타남 없음.
- 새 시험(브랜치 diff의 `it(`·`test(` 수, `it.each` 한 줄은 하나로): 스테이지 1 53(`clusterMap/{model,layout,lod,paths,hit,keys,labels}.test.ts`·`ClusterMapScene.perf.test.tsx`·`components/iso` 시험), 스테이지 2 24(`ClusterMapPage.test.tsx` 12·`MapScene.dom.test.tsx` 5·`colors.test.ts` 2·`strings.test.ts`·`paths.test.ts`·`nav.test`·`CommandPalette.test`·`routes.test` 하나씩 — `shortcuts.test`·`i18n.test`는 있던 시험에 지도 묶음·문구 파일을 더함), 스테이지 3 Playwright 줄기 하나(`web/e2e/vm.spec.ts`의 `clusterMap(page, "web")` — opld `web-it`가 돌림). `src/clusterMap`·`src/components/iso` vitest 114개(14 파일), 웹 vitest 전체 1,717개(168 파일 — 계획 69 기록 1,586개·155 파일).
- **Mac 시험**(aarch64 HVF, Homebrew QEMU): 스테이지마다 `pnpm -C web typecheck`·`lint`·`test`·`build` 통과. 스테이지 3에서 `cargo test -p opld --features web-it --test web` 셋 통과(큰 줄기 약 50초, 셋 합 약 72초 — 파일 복원 줄기는 도우미 이미지 환경 값이 없어 건너뜀). 진짜 브라우저(Chromium, 가짜 API — 넓은 화면 밝음·어둠·390px)에서 그림·가리키기 카드·서랍·확대 단계 바뀜을 손으로 봤다(스테이지 2).
- **스테이지 4 확인:** 가이드의 키·문구·주소를 코드에서 읽어 맞췄다(`MapScene.tsx` `onKey` — 화살표·`]`·`[`·`Esc`(VM 단계만)·`Home`·`End`·`Enter`·`Space`·`Shift+Enter`·`+`·`=`·`-`·`0`, 더블클릭·Ctrl/⌘+누르기 = 상세, 4px 넘게 끌면 열지 않음; `lib/shortcuts.ts` 묶음 `map`; `clusterMap/i18n/ko.json`의 도구 띠·단계·범례·서랍 문구; `ClusterMapPage.tsx`의 기억 `opl.<사용자>.clusterMap.{mode,group,layers,filter}`·기본 레이어 `load`·노드 200 띠·`?focus=`; `lod.ts` 문턱; `data.ts` 15초·30초). `web-ui.md`에 영역 표·단축키 표·새 절·한계, `cluster.md`·`ha.md`에 한 줄씩, `development.md`에 시험 명령·E2E 줄기·늦게 받는 문구 규약, `docs/index.md`, README 한 줄. `docs/architecture.md`는 화면을 하나씩 적지 않아 그대로 두었다.
- **웹 번들(eager, gzip -9):** 계획 69 끝 476,367 → 스테이지 1 477,319 → 스테이지 2·3 478,317 B(계획 70 합 +1,950 B, 목표 3 KB 밑). 늦은 조각: 지도 18,750 B(목표 20 KB 밑)·서랍 2,516 B·문구 en 1,731 B·ko 2,101 B·불러오기 308 B. 스테이지 4는 문서만이라 같다.
- 리뷰: 웹 두 번. 1차(중간 2·중하 2·낮음 6·제안 2, 거짓 경보 없음 → `fix(web): 리뷰 1`) — 메모리 단계에 100% 진행률(디스크 복사 바이트) → 디스크 복사 단계만 퍼센트·`tasks.ts`와 한 규칙, 관리자에게 "볼 수 없는 VM +1" 유령 타일, 지도 포커스 때 Ctrl/⌘ +·−·0을 삼킴, 화살표(마이그레이션·추천)가 마우스로만 → 노드 서랍·목록 보기의 "마이그레이션과 추천", 포커스 돌려주기, 한글 꼬리표 폭, 사라진 기억 설정, HA 칩·서랍 배치 규칙, 마이그레이션 Task만 다시 그리기, 그 VM을 옮길 수 있을 때만 적용. 2차(1차 수정 13개 모두 맞음, 낮음 1·시험 제안 1 → `fix(web): 리뷰 2`) — 목록 보기에서 적용·마이그레이션 끝난 뒤 포커스가 body로 → 화살표 키로 기억, 폴더 운영자의 적용 시험.
- svc01 일반 시험(리뷰 1 커밋, Rust 바뀜 없음): fmt·clippy·test·starve·scale·storage-dir·storage-api 모두 통과, 4277개 통과·0 실패.
- GitHub 탐침(`38e79562`, main 위로 다시 쌓은 뒤, `web` 작업 포함): 13개 작업 모두 통과(web·package·file-restore·linux-kvm·ldap·ceph·ovn·sdn-multinode·backup-s3·install-media·iso-e2e 둘·catalog-links).
- 배포(2026-11-24 20:11, `c35c2508`): svc01에서 빌드한 `orbivisord`(sha256 `2d6628c0…`)를 svc02 → svc01에 설치, 두 노드 active·경고 없음, test-vm(PID 40553) 그대로. 확인(Playwright 화면, 어두운 테마 1400×900): 인프라 › 노드 › 지도에 판 "lab · 2 nodes", svc01·svc02 블록(CPU·메모리, 리더 꼬리표), test-vm 타일. 라이브 마이그레이션은 test-vm이 노드 로컬 ISO를 꽂아 막혀(`cdrom_local` — 맞는 보호) 확인용 VM `map-demo`(256 MiB 디스크)로 했다 — **처음엔 실패**: 랩의 `/opt/qemu`(직접 빌드 QEMU 10.1.2)에 GnuTLS가 없어 TLS 마이그레이션이 처음부터 안 되던 랩 문제(`TLS credentials support requires GNUTLS`). QEMU를 `--enable-gnutls --enable-seccomp`로 다시 빌드해 두 노드에 넣고 opld를 다시 띄운 뒤 svc01 → svc02 → svc01 둘 다 성공(각 15초 안). 지도에 점선 흐르는 화살표 `map-demo → svc01`, 원본 테두리 타일·대상 점선 유령 타일, 끝나면 타일이 대상 블록으로 옮겨짐을 1초 간격 화면으로 확인. `map-demo`는 지웠다.

## 다음 계획으로 넘길 사항

- **로드맵 다음**: **계획 71 — 끌어 놓기 VM 만들기**(이 지도에 타일 → 노드 끌어 옮기기를 함께, 설계 §16).
- 다른 화면의 "지도에서 보기"(`?focus=`), 홈의 작은 지도 위젯, 시간 되감기, 노드 사이 네트워크 선, 랙 U 자리, 원격 클러스터 지도, 그림에서 여러 개 고르기, 노드 500 넘는 클러스터(Worker·culling).
