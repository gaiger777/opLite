# Orbivisor 계획 71: 끌어 놓기 VM 빌더 — 부품 상자·VM 틀·살아 있는 확인·JSON/CLI 미리 보기, 서버 미리 확인 API, 클러스터 지도에서 끌어 옮기기 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 긴 VM 폼을 내려가며 이름을 고르고 만들기를 눌러야 서버 규칙을 알던 것을, **부품을 VM 틀에 끌어 놓아 조립하는 화면**으로 보인다. VM 탭 만들기 화면의 보기 **"빌더"**(`/vms/-new/builder` — 폼 | 빌더, 사이드바·탭 없음): 부품 상자(크기·OS 프리셋, CPU 모델, 풀 → 새 디스크, 남는 볼륨, ISO, 템플릿, 네트워크·보안 그룹, cloud-init 조각, 노드·장애 영역, HA, 폴더), 칸이 정해진 VM 틀(계산 밀대·디스크·CD·NIC·cloud-init·배치·보호·폴더), 살피기, **살아 있는 확인**(브라우저 즉시 + 0.6초 뒤 서버가 만들기와 같은 검사를 그대로 도는 `POST /vm-checks`), 보낼 요청 그대로의 **JSON·CLI 미리 보기**, 폼과 같은 요청의 **만들기**. 모든 끌기에 누르기·키보드·메뉴 대체가 있고 휴대폰에서도 된다. 그리고 계획 70이 미룬 **지도 끌어 옮기기**(켜진 VM 타일 → 노드, 놓을 수 있는 노드 밝히기와 까닭, 키보드 `m`, 지금 옮기기 대화상자).

**Architecture:**
- **빌더 = 폼 값의 다른 편집기**: 상태는 `VmFormValues` + `base`(빈 VM | 템플릿)·`zone`(만들 배치 규칙)·`autoNames`. 검사 `validate()`, 요청 `toSpec`·`createVolume`·`createVm`, 프리셋 `applyPreset`, 템플릿 배포 본문을 폼과 함께 쓴다. 순수 모델 `web/src/vmBuilder/model/`: `parts(ctx)` → `slots(state)` → `accepts(part, slot, ctx)`(종류·개수·권한·자리 막음, 용량은 경고) → `drop` → `problems`(칸 id = 폼 오류 열쇠 꼴 `disks.0`·`nics.1` — 브라우저·서버 문제가 같은 칸에) → `requests`(JSON·만들기 공용) → `cliText`(실제 CLI와 본문이 같은지 Rust 교차 시험).
- **끌기 틀은 직접 짠다**(`web/src/dnd/`, gzip 4 KB 밑, 새 의존성 없음): 포인터 사건 하나로 마우스·펜(4px 문턱)·터치(350 ms 길게 누르기), 누르기로 들고 누르기로 놓기, 키보드 들기(`Space`·`Enter`)·놓을 칸만 탭 차례·`Esc`, `aria-live` 알림 줄, 가장자리 자동 스크롤, 동작 줄이기. 칸 찾기는 DOM 사각형 또는 꽂는 찾기 함수(지도는 계획 70 `hitTest`). 순수 상태 기계라 사건열로 시험한다.
- **서버는 미리 확인 하나**: `POST /vm-checks`(`VmCheckRequest{name, labels, spec, volumes, rules}` → 200 `VmCheck{ok, problems[{at, code, message}], node, machine, cpu_model, ips}`) — `create_vm`의 검사를 쓰지 않고 돈다. 독립 검사(이름·첨부 권한·CPU·NUMA 권한·만들 볼륨·만들 규칙·할당량)는 모으고, `prepare_with(Planned{volumes, rules})`(만들 것을 얹은 목록 — 지금 `prepare`는 빈 `Planned`)는 첫 오류를 `area_of(code)`로 칸에. 폴더 `vm.create`가 없으면 403, 기능 `vm-check` 없는 노드가 있으면 409 `cluster_upgrading`, 잠그지 않고 감사 로그에 적지 않는다. `orbivisorctl vm create … --dry-run`. 지금 타입은 그대로(바이트 호환), 새 타입만 OpenAPI·`schema.d.ts`에.
- **지도 끌기는 타일에서 시작한 누르기만**: 켜진 VM·`vm.migrate`면 끌기, 아니면 지금 보기 움직이기. 끄는 동안 노드 밝히기는 `<style>` 한 줄(블록 다시 그리기 0). 판정 `clusterMap/drop.ts`(같은 노드·위트니스·오프라인·유지보수·얼림·아키텍처·CPU·NUMA·hard 규칙 예상 `placement/hard.ts`·메모리 경고·로컬 디스크 복사 안내). 놓으면 `VmActionHost.open({which: "migrate", node})` — 바로 옮기지 않고 지금 대화상자·API.

**Tech Stack:** Rust(axum·utoipa·serde, `opl-api`·`opl-types`·`oplctl`), React 19·TypeScript·Vite(늦게 읽는 경로·공유 조각), Pointer Events·`setPointerCapture`·`requestAnimationFrame`, TanStack Query·watch(`LiveProvider`), react-i18next(늦게 받는 문구 en·ko), Radix(서랍·메뉴), vitest + msw + axe-core + `test/contrast.ts`, Playwright(opld `web-it`), `eager.sh`. 새 의존성 없음.

**설계 문서:** `docs/superpowers/specs/2026-11-25-dnd-builder-design.md`(상위: `2026-11-18-ui-ux-redesign-design.md`, `2026-11-24-cluster-view-design.md` §10·§15·§16, `2026-11-18-folders-permissions-quotas-2fa-design.md`, `2026-11-11-availability-scale-design.md`)
**선행 계획:** 계획 1~70 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| 만들기 화면 보기 전환 폼 \| 빌더(`/vms/-new/builder`), 마지막 보기 기억, 값 넘겨받기(평문 비밀번호는 기록에 두지 않음) | 새 사이드바·탭·영역, VM 고치기 빌더 |
| 부품 상자(크기·OS·CPU 모델·풀·볼륨·ISO·템플릿·네트워크·보안 그룹·cloud-init 조각·노드·영역·HA·폴더), VM 틀 칸, 순서 바꾸기·빼기·되돌리기, 살피기(밀대) | 자유 그림판, 이미지 라이브러리에서 받으며 만들기, 스냅샷 예약·복제·NUMA 나누기 부품(폼에서) |
| 템플릿 바탕(배포 `clone` 덮어쓰기) | 템플릿 배포의 서버 미리 확인 |
| 살아 있는 확인: 브라우저 즉시 + `POST /vm-checks`(만들 볼륨·규칙 얹음, 문제마다 칸), JSON·CLI 미리 보기, CLI 교차 시험 | 확인 결과 예약(잡아 두기), 선언형 파일 내보내기 |
| 끌기 틀: 포인터·길게 누르기·누르기·키보드·알림·자동 스크롤·동작 줄이기 | DnD 라이브러리, 폴더 끌어 놓기 바꾸기 |
| 지도 끌어 옮기기(켜진 VM 타일 → 노드, 판정·까닭, `m`, 지금 대화상자) | 꺼진 VM·여러 VM·영역 판으로 끌기 |
| `vm create --dry-run`, 기능 `vm-check`, OpenAPI·`schema.d.ts` | 다른 리소스의 미리 확인 |

## 실행 환경

- **이 Mac(aarch64):** Rust(`cargo test -p opl-api -p opl-types -p oplctl`·`clippy`·`fmt`), 모든 웹 변경(`pnpm typecheck`·`lint`·`test`·`build`·`gen:api`), `eager.sh` 번들 재기, opld `web-it` 브라우저 E2E(Homebrew QEMU HVF — 한 노드), VoiceOver 손 확인.
- **GitHub:** 서버(opl-api·oplctl)와 웹이 바뀌므로 `check`·`web`이 통과해야 한다. `linux-kvm`·`ldap`·`package`는 바뀐 것이 없어 따로 탐침하지 않는다(도는 그대로 통과만 본다).
- **배포 서버 svc01(Rocky 10 aarch64, `qemu-tcg`):** 일반 시험 묶음 전체(웹 묶음을 넣은 빌드 — opl-api `vm_checks`·oplctl 교차 시험 포함).
- **테스트 VM:** svc01·svc02에 배포해 빌더로 VM 하나(풀 끌기·네트워크 끌기·노드 svc02 놓기 → 서버 확인 "통과, 노드 svc02" → 만들기)와 영역 놓기(노드에 `zone` 레이블을 주고 hard 규칙 → 그 영역 노드에 켜짐), 지도에서 test-vm 타일을 svc01 → svc02로 끌어 대화상자(로컬 디스크 복사 안내)·옮기기, `m` 키 길, 휴대폰 폭.

## 설계 결정

- **빌더는 폼 값의 다른 편집기:** 따로 된 모델이면 폼에 칸이 생길 때마다 늦는다. 같은 `VmFormValues`로 검사·요청·프리셋·폼 ↔ 빌더를 그대로 얻고, 빌더가 그리지 않는 칸은 "폼에서 고친 칸"으로 들고 다닌다(열쇠마다 지키는 시험).
- **칸이 정해진 틀, 자유 그림판 아님:** VM 스펙은 나무 모양(디스크·NIC 목록)이라 선 잇기가 뜻을 더하지 않고 키보드·화면 읽기·휴대폰이 어렵다. 칸 id를 폼 오류 열쇠 꼴로 두어 브라우저·서버 문제가 같은 칸에 붙는다.
- **서버 미리 확인을 더함(이번 계획의 유일한 서버 변경):** 할당량 사슬·스케줄러 점수·CPU 카탈로그·IPAM·HA 용량·hard 규칙을 웹에 다시 짜면 갈라진다. 같은 함수를 쓰는 확인은 갈라질 수 없고 CLI `--dry-run`도 얻는다. 경로는 `/vm-checks`(`?dry_run`은 옛 노드가 진짜로 만들고, `/vms/check`는 VM 이름과 겹침). 문제 목록은 200, 확인할 수 없을 때만 4XX. 저장하는 것이 없어 지금 타입은 그대로다.
- **직접 짠 포인터 끌기 틀:** dnd-kit(13~16 KB)은 DOM 사각형 충돌 판정이라 지도의 합친 경로에 맞지 않고, HTML5 DnD는 터치·키보드·SVG 타일이 안 되며, React Aria는 25 KB를 넘는다. 쓰는 기능이 좁아 4 KB 밑으로 두 화면이 같은 규칙(문턱·길게 누르기·누르기 들기·키보드 들기·알림)을 쓴다.
- **끌지 않는 길이 셋:** 누르기 들기/놓기(휴대폰 기본), 키보드 들기·놓을 칸만 탭·놓기, 부품 "⋯ 더하기"·블록 "위로·아래로·빼기" 메뉴(WCAG 2.2 SC 2.5.7). `aria-grabbed`는 폐기라 쓰지 않고 `aria-roledescription` + 알림 줄.
- **영역에 놓기 = hard node-affinity 규칙을 VM보다 먼저 만듦:** 노드 하나를 고르면 영역의 뜻이 사라진다. 규칙은 없는 VM 이름을 받고 스케줄러가 켤 때 따른다. admin만(규칙 쓰기), 실패 때 남은 것 알림.
- **지도 끌기는 타일에서 시작한 누르기만, 놓으면 대화상자:** 보기 움직이기 손버릇을 지키고, 바로 옮기지 않아 실수가 싸다. 판정은 "예상"이고 서버(대화상자의 409·admin 규칙 넘기)가 최종이다.
- **첫 화면 묶음은 거의 그대로:** 빌더·끌기 틀·지도 끌기 문구는 늦게 받는 조각(계획 70 규약), 첫 화면에는 보기 전환 두 낱말·팔레트 별칭·`m` 단축키 글만(+1 KB 밑).

## Task

각 Task(스테이지)는 하나의 커밋이며, 앞 Task의 커밋 위에서 순서대로 한다. 스테이지마다 혼자 컴파일되고 시험된다(`cargo test`·`clippy`·`fmt`, `pnpm typecheck`·`lint`·`test`). 화면은 스테이지 3(빌더)·4(지도 끌기)에서 열린다. 설계와 다른 것·결정은 설계 문서 §18에 스테이지마다 남긴다.

- [ ] **Task 1: VM 만들기 미리 확인 POST /vm-checks — 만들 볼륨·배치 규칙을 얹은 같은 검사, 문제마다 자리, 기능 vm-check, vm create --dry-run(api·cli)** — `crates/opl-types/src/vm_check.rs`(`VmCheckRequest`·`VmCheck`·`VmCheckProblem`, `deny_unknown_fields`·`ToSchema`), `crates/opl-api/src/vm_checks.rs`(핸들러: 이름 → 머신 → 폴더 `vm.create`(403) → 모으는 검사 → `prepare_with` → 채운 값, `area_of(code)`), `vms.rs` `prepare_with(Planned{volumes, rules})`(지금 `prepare`는 빈 `Planned` — 만들기·고치기 동작 그대로), `volumes.rs` `check_new_volume`(만들기와 함께 씀), `quota.rs` 만들 볼륨 크기 더하기, `audit.rs`가 `/vm-checks`를 읽기처럼 넘김, `opl_types::node::VM_CHECK`·`FEATURES`, `lib.rs` 경로, `web/src/api/schema.d.ts`(`gen:api`), `oplctl` `vm create --dry-run`(표·`--json`·끝 코드), 시험(통과·저장 안 됨·문제 모음·만들 볼륨·만들 규칙 영역·`area_of`·403·`node: null`·409·감사 없음·OpenAPI·CLI).
- [ ] **Task 2: 끌어 놓기 틀과 VM 빌더 모델 — 포인터·누르기·키보드 들기와 놓기, 알림 줄, 부품·칸·놓기 규칙·문제·요청·JSON·CLI(순수)(web·cli 시험)** — `web/src/dnd/{types,core,autoscroll,useDraggable,useDropSlot,DndProvider,Announcer}`·`dnd/i18n`(+ 사건열 시험), `web/src/vmBuilder/model/{state,parts,slots,accepts,drop,problems,requests,cli}.ts`(+ 시험: 부품 셈·`accepts` 표·놓기마다·이름 따라가기·순서·빼기·되돌리기·열쇠 → 칸·요청 셋·비밀번호 가림·따옴표·`VmFormValues` 열쇠 지킴), `model/__fixtures__/cli.json`, `web/src/placement/hard.ts`(+ 서버와 같은 표), `web/src/vms/migrate.ts`(`migrationTargets`·`localDisks` 옮김), 템플릿 배포 본문 함수 꺼냄, `crates/oplctl` 교차 시험(고정 파일 CLI 줄 → 가짜 API `POST /vms` 본문 = 웹 본문).
- [ ] **Task 3: 끌어 놓기 VM 빌더 화면 — 만들기 화면의 보기 "빌더", 부품 상자·VM 틀·살피기·살아 있는 확인·JSON/CLI 미리 보기·만들기, 휴대폰(web)** — `App.tsx` 늦은 경로 `vms/-new/builder`, `VmFormPage` 보기 전환·`vmCreate.mode` 기억·초안(`vmBuilder/draft.ts`), `Breadcrumbs`, 팔레트 "VM 빌더 열기", `web/src/vmBuilder/{BuilderPage,PartsPanel,Chassis,Inspector,SidePanel,useServerCheck}.tsx`·`vmBuilder/i18n`, vitest(마우스·키보드·누르기·메뉴·순서·되돌리기·막힌 칸 까닭·서버 확인 600 ms·끊음·흐림·409 안내·만들기 차례와 남은 것 알림·템플릿 바탕·폼 ↔ 빌더와 비밀번호 기록 없음·휴대폰 서랍·동작 줄이기·axe·대비), VoiceOver 손 확인, 번들 기록.
- [ ] **Task 4: 클러스터 지도에서 VM 타일을 노드로 끌어 옮기기 — 놓을 수 있는 노드 밝히기와 까닭, 키보드 m, 옮기기 대화상자(web)** — `web/src/clusterMap/drop.ts`(+ 표 시험), `MapScene` 포인터 갈래(타일 + 켜짐 + `vm.migrate`면 끌기, 터치 길게 누르기)·밝히기 `<style>`·가장자리 보기 옮기기·`m` 모드, `VmMigrateDialog`가 `vms/migrate.ts`를 씀, `lib/shortcuts.ts` 무리 `map`에 `m`, `clusterMap/i18n`, vitest(끌기 → 대화상자 노드 고름·막힌 노드·빈 곳·노드 끌기는 보기 움직이기·터치·`m` 키 표·권한 없는 VM·다시 그리기 0·axe), 번들 기록.
- [ ] **Task 5: VM 빌더·지도 끌어 옮기기 브라우저 E2E — 마우스·키보드·휴대폰 폭으로 만들기, 지도 m, 번들 크기 기록(web·opld)** — `web/e2e/builder.spec.ts`(넓은 화면 마우스 끌기 → 서버 확인 통과·JSON·CLI → 만들기 → 지움, 키보드만, 390 × 844 누르기 들기/놓기·가로 스크롤 없음), `vm.spec.ts` 지도 줄기에 `m`(한 노드 — "놓을 수 있는 노드가 없습니다")·`Esc`, 필요하면 `crates/opld/tests/web.rs`, `eager.sh` 앞뒤·조각 gzip을 §18에.
- [ ] **Task 6: 웹 가이드의 VM 빌더와 지도 끌어 옮기기, 미리 확인 API·CLI, 설계 문서 스테이지 보고, 계획 71 실행 기록(docs)** — `docs/guide/web-ui.md`(마법사 절의 빌더, 새 절 "VM 빌더 화면", 클러스터 지도 절의 끌어 옮기기·`m`, 단축키 표, 한계 고침), `docs/guide/vms.md`(`POST /vm-checks`·`vm create --dry-run`), `docs/guide/development.md`(시험 명령·`builder.spec.ts`·교차 고정 파일 갱신), `docs/index.md`, README 한 줄, 이 문서.

## 완료 기준 (계획 71)

- 서버 변경은 `POST /vm-checks`·기능 `vm-check`·`vm create --dry-run`뿐이고, 지금 리소스 타입의 직렬화가 그대로다(바이트 호환). 만들기·고치기 동작이 바뀌지 않는다(`prepare`를 쓰는 지금 시험 그대로 통과).
  - **결과:** (채움)
- 미리 확인이 만들기와 같은 검사를 쓰지 않고 돈다: 통과면 고를 노드·머신·CPU 모델·SDN 주소를 주고, 문제는 한 응답에 모아 칸(`at`)과 함께 준다. 만들 볼륨·배치 규칙을 얹어 본다. 폴더 `vm.create`가 없으면 403, 섞인 클러스터는 409 `cluster_upgrading`, 감사 로그에 없다.
  - **결과:** (채움)
- VM 탭 만들기 화면에 폼 \| 빌더가 있고 `/vms/-new/builder`는 늦게 읽는 조각, 마지막 보기를 기억하며 팔레트로 간다. 사이드바·탭·nav 표는 그대로다. 폼 ↔ 빌더가 값을 지키고 평문 비밀번호는 `history.state`·`sessionStorage`에 남지 않는다.
  - **결과:** (채움)
- 부품을 끌어 놓아(마우스·터치 길게 누르기), 누르기로, 키보드로, "⋯ 더하기" 메뉴로 — 네 길 모두 같은 VM을 만들 수 있다. 놓을 수 없는 칸은 까닭을 보이고 읽는다. 순서 바꾸기·빼기·되돌리기가 된다.
  - **결과:** (채움)
- 살아 있는 확인: 브라우저 문제는 즉시, 서버 문제는 마지막 바뀜 0.6초 뒤 그 칸에 보이고, 지난 값의 결과는 흐린다. 서버 확인이 없거나 실패하면 안내하고 만들기를 막지 않는다. 문제가 있으면 만들기가 막히고 까닭이 그 칸으로 데려간다.
  - **결과:** (채움)
- JSON·CLI 미리 보기가 만들기와 같은 요청이고 비밀번호를 가린다. 웹의 CLI 줄을 실제 CLI가 풀어 같은 `POST /vms` 본문을 만든다(교차 시험).
  - **결과:** (채움)
- 만들기가 폼과 같은 차례(볼륨 Task → 규칙 → VM, 템플릿은 `clone`)이고 실패하면 남은 것을 알리며 다시 보내도 또 만들지 않는다.
  - **결과:** (채움)
- 지도에서 켜진 VM 타일을 다른 노드로 끌면 놓을 수 있는 노드만 밝히고(블록 다시 그리기 0), 놓으면 그 노드를 고른 옮기기 대화상자가 열린다. 막힌 노드·빈 곳·원래 노드에서는 열리지 않고 까닭을 읽는다. 타일이 아닌 곳을 끌면 보기 움직이기 그대로다. `m` → 화살표 → `Enter`가 같은 일을 한다.
  - **결과:** (채움)
- 키보드만으로 모든 일이 되고, axe 위반이 없으며(빈 틀·찬 틀·들고 있는 동안·서랍·지도 끌기), 놓을 수 있음·경고·막힘 표시가 두 테마에서 3:1 이상, 동작 줄이기면 움직임이 없다.
  - **결과:** (채움)
- 390px 폭에서 빌더가 서랍·누르기 들기/놓기로 만들 수 있고 가로 스크롤이 없다.
  - **결과:** (채움)
- 첫 화면 묶음 증가 1 KB 밑, 빌더 조각 gzip 25 KB 밑, 끌기 틀 공유 조각 4 KB 밑, 지도 조각 증가 2.5 KB 밑(`eager.sh`).
  - **결과:** (채움)
- `cargo test`·`clippy`·`fmt`, `pnpm typecheck`·`lint`·`test`·`build`, 로컬 브라우저 E2E가 통과하고 svc01 일반 시험과 GitHub `check`·`web`을 포함한 모든 작업이 통과한다.
  - **결과:** (채움)

## 알고 받아들인 제한

- **미리 확인은 그 순간의 답:** (채움 — 잡아 두지 않음, 확인과 만들기 사이 바뀌면 만들기가 알림, SDN 주소·고를 노드는 예상)
- **`prepare` 문제는 첫 하나:** (채움 — 독립 검사는 모으지만 준비 단계는 첫 오류, `area_of`에 없는 코드는 `spec`)
- **템플릿 바탕:** (채움 — 서버 미리 확인 없음, 디스크·NIC 수를 바꾸지 못함)
- **빌더에 없는 칸:** (채움 — 스냅샷 예약·복제·NUMA 나누기·QoS 세부·부팅 순서 표는 폼에서, 고치기는 폼)
- **지도 끌기 판정은 예상:** (채움 — hard 규칙·메모리는 웹 셈, 서버 409가 최종, 꺼진 VM·여러 VM·영역 판 끌기 없음)
- **화면 읽기:** (채움 — VoiceOver 손 확인 결과, `aria-grabbed` 없이 알림 줄)

## 실행 후 기록

- 리뷰: (채움)
- svc01 일반 시험: (채움)
- GitHub: (채움)
- 웹 번들(eager, gzip -9): (채움)
- 배포: (배포 후 채움)

## 다음 계획으로 넘길 사항

- VM 고치기 빌더(핫플러그 규칙을 칸 규칙으로), 템플릿 배포 미리 확인, 여러 VM 한 번에, 이미지 라이브러리에서 받으며 만들기.
- 폴더 끌어 놓기를 이 틀로(터치·키보드), 지도에서 여러 VM·영역 판으로 끌기, 꺼진 VM 끌기 = 노드 바꾸기.
- 빌더 결과를 선언형 파일로(로드맵의 선언형 적용·Terraform).
