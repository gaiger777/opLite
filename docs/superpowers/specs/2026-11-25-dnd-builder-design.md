# 끌어 놓기 VM 빌더 설계 — 부품 상자·VM 틀·살아 있는 확인·JSON/CLI 미리 보기, 서버 미리 확인 API, 클러스터 지도에서 끌어 옮기기 (계획 71)

작성일: 2026-11-25
상위 문서: `2026-11-18-ui-ux-redesign-design.md`(계획 61 화면 규칙 — 사이드바에는 영역만, 하위 화면은 탭·보기·대화상자), `2026-11-24-cluster-view-design.md`(계획 70 지도 — §10·§15·§16이 끌어 옮기기를 이 계획으로 미룸), `2026-11-18-folders-permissions-quotas-2fa-design.md`(계획 62 권한·할당량, 폴더 끌어 놓기), `2026-11-11-availability-scale-design.md`(배치 규칙·스케줄러), 계획 57~59·63~65의 VM 폼(설치 매체·cloud-init·템플릿·CPU 모델·NUMA·SDN 주소)
기준 트리: 계획 70 끝(`5454f8c9`). 이 문서는 구현 전 설계다. 스테이지 보고는 구현하면서 §18에 덧붙인다. 로드맵의 "끌어 놓기 VM 만들기"(하위 프로젝트 14 D — "초보자도 마우스로 움직여서 만들 수 있는 화면", 부품 상자·밀대·작게/보통/크게·살아 있는 맞춤 확인·폼은 그대로·키보드로도)를 이 문서가 맡는다.

## 1. 목적

지금 VM 만들기(`/vms/-new`)는 정확하지만 **긴 폼**이다: 열한 절(일반·디스크·네트워크·CD/DVD·부팅 순서·보안·게스트 에이전트·cloud-init·스냅샷·복제·검토)을 내려가며 풀·네트워크·ISO 이름을 고르고, 할당량·풀 용량·배치 규칙·CPU 모델 호환은 **만들기를 눌러야** 서버가 알려 준다. 처음 쓰는 사람은 "무엇을 어디에 붙이는지"를 그림으로 보지 못하고, 숙련자는 같은 것을 CLI·API로 옮길 방법을 화면에서 얻지 못한다. 계획 71은 이것을 **끌어 놓기 빌더** 한 화면으로 보인다:

- **부품 상자**(왼쪽): 크기 프리셋(작게·보통·크게·아주 크게)·OS 프리셋·CPU 모델, 풀(새 디스크)·남는 볼륨·ISO·템플릿, 네트워크·보안 그룹, cloud-init 조각(사용자·SSH 키, DHCP·고정 주소, DNS, 패키지), 노드·장애 영역·HA·폴더. 부품마다 남은 용량·쓸 수 있는지가 붙어 있다.
- **VM 틀**(가운데): 서버 랙 장비처럼 칸이 정해진 틀 — 계산(vCPU·메모리 밀대), 디스크 칸(위가 먼저 부팅), CD 칸, NIC 포트(네트워크·보안 그룹 칩·주소), cloud-init 판, 배치 칸(자동·노드·영역), 보호 칸(HA), 폴더. 부품을 맞는 칸에 끌어 놓으면 붙고, 맞지 않는 칸은 놓을 수 없음과 까닭을 보인다.
- **살아 있는 확인**(오른쪽): 즉시 하는 브라우저 검사(폼과 같은 `validate()`·놓기 규칙) + 0.6초 쉬면 **서버가 만들기와 같은 검사를 그대로 돌리는 미리 확인**(할당량·풀 용량·네트워크·보안 그룹 권한·CPU 모델·배치 규칙·스케줄러가 고를 노드·SDN 주소·HA 용량). 문제는 그 칸에 표시된다.
- **미리 보기**: 보낼 요청 그대로의 **JSON**(볼륨·배치 규칙·VM)과 **CLI**(`orbivisorctl volume create …`, `vm create …`) — 복사 단추. 그리고 **만들기**는 폼과 같은 요청(볼륨 → VM)이다.

그리고 계획 70이 미룬 **클러스터 지도의 끌어 옮기기**: 켜진 VM 타일을 다른 노드로 끌면 놓을 수 있는 노드만 밝히고(오프라인·유지보수·아키텍처·CPU·NUMA·배치 규칙·메모리), 놓으면 대상을 고른 **지금의 옮기기 대화상자**가 열린다. 키보드는 `m` → 화살표 → `Enter`.

**모든 끌기에는 끌지 않는 길이 있다**(WCAG 2.2 SC 2.5.7): 누르기(탭)로 들고 누르기로 놓기, 키보드로 들고·옮기고·놓기, 부품마다 "더하기…" 메뉴. 휴대폰에서도 같은 화면이 된다.

원칙: **만들기 API는 그대로 쓴다**(`POST /volumes`·`POST /placement-rules`·`POST /vms`·템플릿은 `POST /vms/{key}/clone`). 서버에 더하는 것은 **아무것도 바꾸지 않는 미리 확인 하나**(`POST /vm-checks`, §9)뿐이다 — 웹이 서버 규칙을 흉내 내면 갈라지기 때문이다(§15). 새 의존성은 없다(끌기 틀을 직접 짠다 — §4·§15).

2026-11 공개 문서로 아는 것(제품 판마다 다를 수 있다):

| | Proxmox VE 9 | vSphere 8 | Nutanix Prism | Orbivisor 지금(계획 70) | Orbivisor 계획 71 |
|---|---|---|---|---|---|
| VM 만들기 | 탭 마법사 | 단계 마법사 | 한 장 폼 | 절 폼 + 프리셋 | 폼 + **끌어 놓기 빌더**(같은 값) |
| 만들기 전 검사 | 입력 칸 검사 | 호환성 검사(호스트·데이터스토어) | 일부 | 입력 칸 검사(서버 규칙은 만들 때) | **서버 미리 확인**(할당량·용량·권한·CPU·배치·노드 고르기) |
| 요청 보기 | 없음(API 문서) | "코드 캡처"(PowerCLI 등) | REST 보기 | 없음 | **JSON·CLI 미리 보기** |
| 호스트로 끌어 옮기기 | 없음 | 인벤토리 끌어 놓기(호스트로) | 없음 | 없음(서랍의 "옮기기…") | 지도에서 끌어 옮기기 + 키보드 `m` |
| 끌기의 키보드 대체 | — | 메뉴 | — | 폴더: "폴더로 옮기기…" | 들기·옮기기·놓기 키 + 메뉴 + 누르기 |

## 2. 범위

| 포함 | 제외 |
|---|---|
| VM 탭의 만들기 화면에 보기 전환 **폼 \| 빌더**(`/vms/-new` \| `/vms/-new/builder`), 마지막 보기 기억, 두 보기가 같은 값을 넘겨받음 | 새 사이드바 항목·탭·영역, VM **고치기** 빌더(고치기는 폼) |
| 부품 상자: 크기·OS 프리셋, CPU 모델, 풀(새 디스크)·남는 볼륨·ISO·템플릿, 네트워크·보안 그룹, cloud-init 조각 다섯, 노드·장애 영역·자동, HA·HA 그룹, 폴더 | 이미지 라이브러리에서 받으며 만들기(받은 볼륨은 부품으로 보임), 스냅샷 예약·복제 대상·QoS·NUMA 나누기 부품(살피기의 "폼에서 고치기") |
| VM 틀: 계산(밀대)·디스크·CD·NIC·cloud-init·배치·보호·폴더 칸, 칸 안 순서 바꾸기(부팅 순서), 빼기, 살피기(고른 칸의 값) | 자유 배치 캔버스(칸 없는 그림판), 여러 VM 한 번에 |
| 템플릿 바탕: 템플릿을 틀에 놓으면 배포(`clone`)로 바뀜 — 계산·네트워크 바꾸기·디스크 늘리기·cloud-init·노드·풀·연결 복제 | 템플릿 배포의 서버 미리 확인(§16) |
| 살아 있는 확인: 브라우저 검사(폼의 `validate()`·놓기 규칙·용량·할당량 어림) + **서버 미리 확인 `POST /vm-checks`**(만들 볼륨·배치 규칙을 얹어 만들기와 같은 검사, 문제마다 자리) | 서버가 미리 확인한 값을 잠가 두기(예약) — 확인과 만들기 사이 바뀌면 만들기가 알림 |
| JSON(볼륨·규칙·VM 요청 차례대로)·CLI 미리 보기와 복사, 웹 CLI 글이 실제 CLI와 같은 본문을 만드는지 보는 교차 시험 | Terraform·선언형 파일 내보내기(로드맵 뒤) |
| 끌기 틀 `web/src/dnd/`: 포인터(마우스·펜·터치 길게 누르기)·누르기 들기/놓기·키보드, 알림 줄, 가장자리 자동 스크롤, 동작 줄이기 | DnD 라이브러리(§4·§15), 폴더 끌어 놓기 바꾸기(HTML5 DnD 그대로) |
| 지도 끌어 옮기기: 켜진 VM 타일 → 노드, 놓을 자리 판정(예상)·까닭, `m` 키, 지금 `VmMigrateDialog`(대상 미리 고름) | 꺼진 VM의 노드 바꾸기 끌기(고치기에서), 여러 VM 끌기, 영역 판으로 끌기 |
| CLI `orbivisorctl vm create … --dry-run`(같은 미리 확인) | 다른 리소스의 미리 확인 |
| vitest(순수·화면·axe·대비)·Rust 시험·Playwright·번들 크기 | 실제 VoiceOver·TalkBack 자동 시험(손 확인) |

## 3. 지금 있는 것 (계획 70 끝)

- **VM 폼**(`web/src/vms/VmFormPage.tsx`, 늦게 읽는 경로 `vms/-new`·`vms/:name/edit`): 값은 `VmFormValues`(`web/src/vms/form.ts` — `name`·`sockets`·`cores`·`model`·`cpuFlags`·`memory`·`numa`·`machine`·`folder`·`disks[DiskRow{kind: existing|new, volume, pool, sizeGib, bus, bootOrder}]`·`cdroms[{iso, bootOrder}]`·`nics[NicRow{network, model, mac, securityGroups, ips, qos}]`·`cloudInit`(`CloudInitValues` — 사용자·비밀번호·SSH 키·NIC별 주소·DNS·user/vendor-data)·`node`·`ha`…·`pinned`·`start`). `emptyForm`·`emptyDisk(vmName, i)`·`validate(values, creating, volumes, sdn)`(오류 열쇠가 자리 — `disks.0.pool`, `nics.1.securityGroups`, `ci.…`)·`toSpec(values, power, sdn)`·`haUnavailable`·`firmwareUnavailable`·`tcMissing`. 단계 `form/steps.ts`(`FORM_STEPS`, `stepOf(열쇠)`, 프리셋 `applyPreset` — 빈 VM·Linux 서버·Windows 11·템플릿). 보내기 `form/submit.ts`: `createVolume`(`POST /volumes` → Task 기다림) → `createVm`(`POST /vms`), 실패하면 "만든 볼륨이 남음"(`vmForm.volumesCreated`).
- **목록·고르기 도우미**: `choosableIsos`(HA·복제 VM은 공유 풀 ISO만), `poolFormats`·`poolCapacity`·`usableOn`·`choosablePoolsOn`(`storage/pools.ts`), `sdnNetworkNamed`(`sdn/ips.ts`), `useCpuModels(arch)`(`GET /cpu/models` — 모델마다 `nodes[CpuNodeFit]`), `defaultFolder`, `folderTree`, 폴더 사용량 `GET /folders/{id}/usage`(`FolderUsageView{usage, limits[], binding}` — `UsageBars`), 템플릿 배포 `DeployDialog`(`POST /vms/{key}/clone` — 덮어쓰기 칸 `vcpus`·`memory_mib`·`nics[]`·`disks[]`(늘리기)·`ips`·`cloud_init`·`node`·`pool`·`linked`·`folder`·`cpu_model`·`start`).
- **서버 만들기**(`crates/opl-api/src/vms.rs` `create_vm`): `check_name` → `with_machine`(머신 채움) → 폴더 기능 → `authz.require_new(VmCreate, folder)` → `check_attachments`(네트워크·볼륨·ISO·복제 풀 권한) → CPU 플래그·NUMA 권한 → `quota::precheck_vm` → `prepare`(MAC 채움, 기능 플래그, `spec.validate`, CPU 모델 해석·고정, HA 그룹, 네트워크 있음, SDN 주소 `ipam::assign`, QoS·보안 그룹, 볼륨 있음·준비됨·다른 VM이 안 씀, ISO, 노드(위트니스·유지보수·HA·아키텍처·방화벽·QoS·CD)·hard 배치 규칙, 켜진 채 노드 없으면 `schedule_with`로 고름, HA 용량) → `cluster.create`. 검사만 따로 부르는 길은 없다. 미리 확인 꼴의 앞선 예: `POST /volumes/import/check`·`POST /vm-imports/ova/check`(204), `POST /network-migrations?dry_run=true`·`POST /sdn/lockdown?dry_run=true`.
- **CLI**: `orbivisorctl vm create NAME --sockets --cores --memory --max-vcpus --max-memory --machine --firmware --preset --disk VOL… --cdrom ISO… --cpu-model --cpu-flags (NUMA 칸) --boot --nic NETWORK[,sg=…][,ip=…][,qos-…] --restart --folder --node --start --ha (HA 정책) --pinned --replicate --ci-*`(`crates/oplctl/src/cli.rs` `VmCreate`), `volume create NAME --pool --size --format`, `placement-rule create NAME --kind --vm --node --node-tag`. 시험용 가짜 API `crates/oplctl/src/fake_api.rs`.
- **클러스터 지도(계획 70)**: `web/src/clusterMap/` — `hitTest(scene, lod, p)`(화면 점 → 노드·타일), `screenToWorld`, `neighbor`(노드·타일 두 단계 키보드), `MapScene`(SVG 한 장, 노드 블록 `memo`, 밝히기는 `<style>` 한 줄 `glowStyle`, 끌기 = 보기 움직이기 `useIsoViewport`, 4px 넘게 움직이면 누르기 아님), 서랍의 "옮기기…" → `VmActionHost.open({vm, which: "migrate", node?, returnFocus})` → `VmMigrateDialog`(`initialNode`, `migrationTargets(vm, nodes)` — 위트니스·오프라인·유지보수·아키텍처, `GET /vms/{key}/cpu`·`/numa` 노드별 맞음, 배치 규칙은 서버 409 `placement_rule` + admin "규칙 넘기"). 단축키 무리 `map`(`]`·`[`).
- **끌어 놓기 앞선 예**: 폴더 보기(계획 62)가 HTML5 DnD(`draggable` 행 → 트리, `DRAG_TYPE`)이고 키보드·휴대폰 대안은 일괄 막대의 "폴더로 옮기기…" 대화상자다. 터치에서는 끌 수 없다.
- **시험 틀**: vitest + msw + `test/axe.ts` + `test/contrast.ts`, opld `web-it`(`crates/opld/tests/web.rs` → `web/e2e/vm.spec.ts`), 첫 화면 묶음 `eager.sh`(계획 70 끝 478,370 B), 늦게 받는 화면 문구 규약(`clusterMap/i18n` + `addResourceBundle`).

## 4. 확인한 사실 (브라우저·끌기·접근성)

- **HTML5 DnD는 이 일에 맞지 않는다**: 터치 브라우저마다 다르게 되거나 안 되고(iOS Safari는 길게 누르기 끌기가 페이지 밖 끌기로 이어짐), 끄는 모양을 마음대로 그리지 못하며(`setDragImage` 정지 그림), 키보드 대체가 없고, **DOM 원소만** 끌 수 있다 — 지도의 VM 타일은 합친 경로 조각이라 원소가 없다(계획 70 §4). `dragover` 중에는 `dataTransfer`를 읽지 못해(폴더 코드가 모듈 변수로 우회) 놓을 자리 판정이 불편하다.
- **포인터 사건(Pointer Events)은 마우스·펜·터치를 한 길로** 준다: `pointerdown` → 문턱(마우스·펜 4px) 넘게 움직이면 끌기, `setPointerCapture`로 원소 밖에서도 받음, `pointercancel`(스크롤·몸짓이 가져감)은 취소. 터치는 스크롤과 다투므로 **길게 누르기**(350 ms 안에 8px 넘게 움직이면 스크롤로 넘김) 뒤에만 끌기이고, 끌기 동안만 `touch-action: none`(부품 목록은 평소 `pan-y`). 길게 누르기의 문맥 메뉴·글 고르기는 `contextmenu` 막기와 `-webkit-touch-callout: none`·`user-select: none`(부품만).
- **놓을 자리 찾기**: DOM 칸은 끌기 시작 때 `getBoundingClientRect`를 한 번 모으고 스크롤·크기 바뀜 때만 다시 모은다(움직일 때마다 재면 레이아웃을 강제). SVG 지도는 칸이 원소가 아니므로 자리 찾기 함수를 꽂는다(계획 70 `hitTest`). 그림자(끌리는 모양)는 `position: fixed` + `transform: translate3d`를 `requestAnimationFrame`마다 한 번 — 레이아웃 없음.
- **가장자리 자동 스크롤**: 스크롤 상자·창 가장자리 48px 안이면 거리에 비례해(프레임마다 최대 16px) 스크롤하고, 스크롤하면 칸 자리를 다시 모은다. 지도는 같은 자리에서 보기(`viewBox`)를 옮긴다.
- **화면 읽기**: ARIA 1.1의 `aria-grabbed`·`aria-dropeffect`는 폐기되어 읽어 주지 않는 프로그램이 많다. 널리 쓰는 꼴은 (1) 끌 수 있는 원소를 `button` + `aria-roledescription`("끌 수 있는 부품") + `aria-describedby`(키 안내), (2) 들기·자리 위·놓기·취소를 `aria-live` 줄로 읽기, (3) 들고 있는 동안 놓을 수 있는 칸만 탭 차례에 넣기다. 이 방식은 VoiceOver·NVDA에서 브라우저 쪽 "끌기 모드" 없이 된다(손 확인은 스테이지 3).
- **WCAG 2.2**: 2.5.7 끌기 움직임 — 한 번 누르기로 하는 대체가 있어야 한다(누르기 들기/놓기·메뉴), 2.5.8 누를 자리 24px 이상(부품 40px), 2.4.7 초점 보임, 2.4.11 초점이 가리지 않음(그림자는 초점 칸을 덮지 않게 포인터 오른쪽 아래로 12px), 1.4.11 놓을 수 있음·없음 테두리 3:1, 2.3.3 동작 줄이기.
- **동작 줄이기**: 놓을 칸 맥박·제자리로 날아가는 되돌림·칸 펼침 움직임을 끈다. 그림자가 포인터를 따라가는 것은 사용자가 움직이는 것이라 그대로 둔다(계획 61 §6 움직임 규칙).
- **라이브러리 크기(공개 수치 어림, gzip)**: `@dnd-kit/core` 약 13 KB(+ `sortable` 약 3 KB) — 키보드 센서·알림이 있으나 충돌 판정이 DOM 사각형이라 SVG 합친 경로에 맞지 않음, `react-dnd` + HTML5 백엔드 약 10 KB(터치·키보드 없음), `@atlaskit/pragmatic-drag-and-drop` 핵심 약 5 KB(HTML5 DnD 위 — 터치·키보드 없음, 메뉴 대체를 권함), React Aria 끌기 훅 25 KB 넘음(접근성은 가장 좋음). 직접 짠 틀은 3 KB 안팎으로 본다 — 쓰는 기능(문턱·길게 누르기·칸 모으기·자동 스크롤·키보드 들기·알림)이 적다.
- **CSS 묶음**: Tailwind는 모든 파일을 훑어 첫 화면 CSS에 넣으므로(계획 70 §4) 빌더·지도 끌기 전용 모양은 지금 쓰는 유틸 클래스와 조각 안 `<style>`(키프레임·무늬)로만 한다.

## 5. 전체 구조

```
가상 머신 › VM 탭   /vms/-new  [폼 | 빌더]  /vms/-new/builder     ← 만들기 화면 안 보기 전환(nav.ts 그대로)
                                     │ (늦게 읽는 조각 web/src/vmBuilder/ + 공유 조각 web/src/dnd/)
  ┌──────────────────────────────────┴───────────────────────────────────────────────┐
  │ 자료: useList(node·storage-pool·volume·network·security-group·ha-group·folder·vm) │
  │       useCpuModels(arch) · GET /folders/{id}/usage · GET /placement(노드 여유)      │
  │       usePermissions()                                                             │
  │ 순수(vmBuilder/model): BuilderState = VmFormValues + {base, zone, autoNames}       │
  │   parts(ctx) → Part[] · slots(state) → Slot[] · accepts(part, slot, ctx) → ok|까닭 │
  │   drop(state, part, slot) → state · problems(state, ctx) → Problem[at]             │
  │   requests(state) → [POST /volumes…, POST /placement-rules?, POST /vms|clone]      │
  │   cliText(state) → 줄들 · checkBody(state) → POST /vm-checks 본문                 │
  ├────────────────────────────────────────────────────────────────────────────────────┤
  │ BuilderPage: 부품 상자(PartsPanel) · VM 틀(Chassis) · 살피기(Inspector)            │
  │   · 확인|JSON|CLI 판(SidePanel) · 만들기 단추 · 알림 줄 · 휴대폰 아래 서랍           │
  │ dnd/: DndProvider · useDraggable · useDropSlot · 포인터/누르기/키보드 · Announcer    │
  └────────────────────────────────────────────────────────────────────────────────────┘
서버: POST /vm-checks (opl-api vms.rs — create_vm과 같은 검사, 쓰지 않음, 기능 vm-check)

인프라 › 노드 › 지도 /cluster/map (계획 70)  +  끌어 옮기기
  MapScene 포인터: 타일(켜짐·vm.migrate) 위 누르고 4px(터치는 길게 누르기) → 끌기, 아니면 보기 움직이기
  clusterMap/drop.ts(순수): dropCheck(vm, node, ctx) → ok|warn|blocked + 까닭
  놓기 → VmActionHost.open({vm, which: "migrate", node, returnFocus})  — 지금 대화상자·API
```

- **값은 하나다**: 빌더는 `VmFormValues`를 고치는 다른 편집기다. 그래서 검사(`validate`)·요청(`toSpec`·`createVolume`·`createVm`)·프리셋(`applyPreset`)을 폼과 함께 쓰고, 폼 ↔ 빌더를 오가도 값이 그대로다(§8.6). 빌더만의 것은 `base`(빈 VM | 템플릿 id), `zone`(만들 배치 규칙 — §7.4), `autoNames`(VM 이름을 따라가는 새 볼륨 이름)뿐이다.
- **서버 변경은 미리 확인 하나**(§9): 저장하는 것이 없고 지금 리소스 모양(`VmSpec` 등)은 그대로다. 기능 플래그 `vm-check`, 섞인 클러스터는 409 `cluster_upgrading`, OpenAPI·`schema.d.ts`에 새 타입만 더한다.
- **파일**: `web/src/dnd/`(`types.ts`, `core.ts` 순수 상태 기계, `DndProvider.tsx`, `useDraggable.ts`, `useDropSlot.ts`, `autoscroll.ts`, `Announcer.tsx`, `i18n/{en,ko}.json`), `web/src/vmBuilder/`(`model/{state,parts,slots,accepts,drop,problems,requests,cli}.ts`, `BuilderPage.tsx`, `PartsPanel.tsx`, `Chassis.tsx`, `Inspector.tsx`, `SidePanel.tsx`, `useServerCheck.ts`, `draft.ts`, `i18n/{en,ko}.json`), `web/src/clusterMap/drop.ts`(+ `MapScene` 끌기), `web/src/placement/hard.ts`(hard 규칙 예상 — 빌더·지도가 함께), `web/src/vms/migrate.ts`(`migrationTargets`·`localDisks`를 대화상자 파일에서 옮김). 서버: `crates/opl-api/src/vm_checks.rs`, `crates/opl-types/src/vm_check.rs`, CLI `--dry-run`.

## 6. 끌기 틀 (`web/src/dnd/`)

### 6.1 세 가지 들기, 한 상태 기계

| 길 | 들기 | 옮기기 | 놓기 | 취소 |
|---|---|---|---|---|
| 포인터 끌기(마우스·펜) | 누르고 4px 넘게 움직임 | 포인터 | 놓을 칸 위에서 뗌 | 칸 밖에서 뗌, `Esc`, `pointercancel` |
| 포인터 끌기(터치) | 350 ms 길게 누른 뒤 움직임 | 손가락 | 칸 위에서 뗌 | 칸 밖, 다른 손가락, `pointercancel` |
| 누르기(마우스·터치) | 부품을 한 번 누름(들림 표시) | — | 밝혀진 칸을 누름 | 다시 누름, 빈 곳 누름, `Esc` |
| 키보드 | 초점 부품에서 `Space`·`Enter` | `Tab`·`Shift+Tab`·화살표가 **놓을 수 있는 칸 사이만** | `Space`·`Enter` | `Esc`(초점은 든 부품으로) |

- 상태 기계(`core.ts`, 순수): `idle → pressing(문턱·타이머) → dragging(포인터) | picked(누르기·키보드) → over(칸) → dropped | cancelled`. 같은 사건열 같은 결과라 시험이 사건만 넣어 본다.
- **놓을 칸 판정은 틀이 하지 않는다**: 쓰는 쪽이 `accepts(item, slot) → {ok: true} | {ok: false, reason}`을 준다. 틀은 들면 모든 칸에 한 번 묻고(`data-drop="ok|no"`), 놓을 수 없는 칸 위에서는 까닭을 그림자 아래 글과 알림 줄에 보인다.
- **칸 찾기**: DOM 칸(`useDropSlot` — 들 때 사각형을 모음, 칸 안 칸이면 가장 안쪽)과 꽂는 찾기 함수(`registerSurface(el, find(point) → slotId | null)` — 지도 SVG).
- **그림자**: 든 부품의 작은 복제(이름·아이콘)를 `position: fixed`에 그림. 키보드·누르기 들기는 그림자 없이 부품에 "들림" 표시와 칸 밝히기만.
- **자동 스크롤**: §4. 키보드로 칸에 초점이 가면 `scrollIntoView({block: "nearest"})`.
- **알림 줄**(`Announcer`, `aria-live="polite"` 하나, 쪽마다 하나): "web-disk0 디스크를 들었습니다. 놓을 수 있는 칸 5개. Tab으로 고르고 Space로 놓고 Esc로 취소합니다." / "디스크 칸 위. 놓을 수 있습니다." / "NIC 1 위. 놓을 수 없습니다: 이 네트워크를 쓸 권한이 없습니다." / "디스크 칸에 놓았습니다. 디스크 2개." / "취소했습니다." 문구는 쓰는 쪽이 부품·칸 이름을 주고 틀이 꼴을 짠다.
- **끝난 뒤 초점**: 놓으면 새로 생긴 블록(빌더)이나 대화상자(지도)로, 취소하면 든 부품으로.
- **크기**: 순수 상태 기계 + 훅 셋 + 알림 줄 — gzip 4 KB 밑(§12). 빌더와 지도가 함께 쓰므로 Vite가 공유 조각으로 나눈다.

### 6.2 지금 있는 것과의 관계

- 지도의 보기 움직이기(`useIsoViewport`)는 그대로 두고, 지도 끌기는 **타일 위에서 시작한 누르기만** 가져간다(§10.1). 두 손가락 확대는 늘 보기 쪽이다.
- 폴더 끌어 놓기(HTML5 DnD)는 바꾸지 않는다(§16 — 이 틀로 옮기면 터치가 됨).
- 단축키: 들고 있는 동안 전역 한 글자 단축키를 받지 않는다(`useShortcuts`가 `dnd.active`면 넘김 — 팔레트 키 `⌘K`도).

## 7. 빌더 모델 (순수, `vmBuilder/model/`)

### 7.1 상태

`BuilderState = { values: VmFormValues; base: { kind: "blank" } | { kind: "template"; id: string; linked: boolean | null; pool: string }; zone: { key: "zone" | string; value: string; hard: boolean } | null; autoNames: boolean[] }`. 처음 값은 폼과 같은 `emptyForm(defaultMachine(nodes), defaultFolder(…))` + 디스크 0·NIC 0(폼은 디스크 하나·NIC 하나로 시작하지만 빌더는 빈 틀 — 첫 놓기가 "무엇을 붙이는지"를 보이게). `autoNames[i]`가 참인 새 디스크의 볼륨 이름은 VM 이름을 따라 `emptyDisk(name, i).volume`으로 바뀐다(살피기에서 고치면 거짓).

### 7.2 부품(`parts(ctx) → Part[]`, 묶음 차례대로)

| 묶음 | 부품 | 붙는 칸 | 부품에 붙은 정보 |
|---|---|---|---|
| 크기 | 작게 1 vCPU·1 GiB / 보통 2·4 / 크게 4·8 / 아주 크게 8·16 | 계산 | 고른 노드(있으면)의 남은 메모리로 "들어감·빠듯함·안 들어감" |
| OS | Linux 서버 / Windows 11(`applyPreset` 그대로) | 틀 전체 | Windows는 TPM 노드가 없으면 까닭(`firmwareUnavailable`) |
| CPU 모델 | `host`·`max`·`x86-64-v2/v3/v4`(aarch64는 그 목록)·쓸 수 있는 이름 모델 | 계산 | 켤 수 있는 노드 수(`CpuModelEntry.usable`·`nodes`) |
| 디스크 | 풀마다 "새 디스크"(기본 20 GiB) | 디스크 칸, 새 디스크 블록(풀 바꾸기) | 남은 용량(`poolCapacity`, 로컬 풀은 노드마다), 공유·로컬 |
| 디스크 | 남는 볼륨(준비됨·어디에도 안 붙음 — 폼의 `freeVolumes`) | 디스크 칸 | 크기·풀 |
| 설치 매체 | ISO(`choosableIsos`) | CD 칸, CD 블록(매체 바꾸기) | 공유 풀인지(HA면 로컬 ISO는 까닭) |
| 템플릿 | 템플릿 VM | 틀 전체(바탕 바꾸기) | vCPU·메모리·디스크 수·아키텍처 |
| 네트워크 | 네트워크(단순·SDN) | NIC 칸(새 NIC), NIC 블록(네트워크 바꾸기) | SDN·VLAN·서브넷, 권한 |
| 네트워크 | 보안 그룹 | NIC 블록 | 규칙 수 |
| cloud-init | 사용자·SSH 키 / 비밀번호 / DNS / 패키지(qemu-guest-agent) | cloud-init 판 | — |
| cloud-init | DHCP / 고정 주소 | NIC 블록(그 NIC의 `cloudInit.networks[i]`) | SDN NIC는 서버 주소라 고정 주소 막음(폼 규칙) |
| 배치 | 자동 / 노드 / 장애 영역(`zone` 레이블 값마다) | 배치 칸 | 노드: 온라인·유지보수·아키텍처·남은 메모리, 영역: 노드 수 |
| 보호 | HA / HA 그룹 | 보호 칸 | HA 못 쓰는 노드면 까닭(`haUnavailable`) |
| 폴더 | 폴더(`vm.create`가 있는 것만) | 폴더 칸 | 할당량 남음 |

부품은 지금 목록(watch)에서 매번 셈한다 — 다른 사람이 풀을 지우면 부품이 사라지고 이미 놓은 블록은 문제(§7.5)가 된다. 부품 상자 위 거르기 칸(이름)과 묶음 접기(기억 `vmBuilder.groups`).

### 7.3 칸(`slots(state) → Slot[]`)과 놓기 규칙(`accepts`)

칸 id는 폼의 오류 열쇠와 같은 꼴이다: `compute`, `disks`(새로 붙이기)·`disks.N`, `cdroms`·`cdroms.N`, `nics`·`nics.N`, `cloudInit`, `placement`, `protection`, `folder`, `vm`(틀 전체), `trash`(빼기 — 틀 블록을 끌 때만). 그래서 서버·브라우저 문제(`at`)가 같은 칸에 붙는다.

`accepts(part, slot, ctx)`가 막는 것(까닭 문구 열쇠와 함께, 위에서부터 첫 것):

- 종류가 맞지 않음(풀 → NIC 등) — 이 칸에는 밝히지 않음(까닭 없이 `no`).
- 개수: NIC 16(`MAX_NICS`), CD 4(`MAX_CDROMS`), q35 SATA 포트 6(`Q35_SATA_PORTS` — Windows 프리셋의 SATA 디스크).
- 권한: 네트워크 `network.attach`·보안 그룹·풀 `storage.allocate`·볼륨·ISO 읽기·HA(`vm.ha`)·영역 규칙(`placement-rule` 쓰기 = admin)·폴더 `vm.create` — `usePermissions().has(…, 객체)`.
- 자리: 고른 노드에서 못 쓰는 로컬 풀(`usableOn`), 고른 노드와 다른 아키텍처의 CPU 모델·템플릿, 그 노드에서 켤 수 없는 CPU 모델(`nodes[].can_run`), 오프라인·유지보수·위트니스 노드, HA인데 로컬 ISO, SDN NIC에 고정 주소 조각, 템플릿 바탕에서 디스크·NIC·CD 더하기(배포는 개수를 바꾸지 못함 — "만든 뒤 고치기에서").
- 용량(막지 않고 **경고**로 놓게 둠 — 서버가 정함): 새 디스크 크기 > 풀 남은 용량, 메모리 > 노드 남은 메모리(`/placement`의 `mem_total_mib − mem_allocated_mib`), 할당량 어림 넘음.

### 7.4 놓기(`drop(state, part, slot) → state`)

- 크기 → `cores`·`memory`(소켓 1), OS → `applyPreset(next, applied, values)`, CPU 모델 → `model`(플래그는 그대로).
- 풀 → `disks`: `{kind: "new", pool, sizeGib: "20", volume: emptyDisk(name, i).volume, bus: windows ? "sata" : "virtio-blk"}`, 풀 → `disks.N`(새 디스크): 풀만 바꿈. 볼륨 → `disks`: `{kind: "existing", volume}`.
- ISO → `cdroms`: 새 드라이브, → `cdroms.N`: 매체 바꿈.
- 네트워크 → `nics`: `{network, model: windows ? "e1000e" : "virtio", securityGroups: []}`, → `nics.N`: 네트워크만 바꿈(그 NIC의 주소·cloud-init 주소는 지움 — 폼의 `changeNics` 규칙). 보안 그룹 → `nics.N`: 칩 더함(있으면 그대로).
- cloud-init 조각 → `cloudInit.enabled = true` + 그 칸(사용자·SSH 키는 살피기에서 입력을 받음 — 놓으면 살피기가 그 칸에 초점). 패키지 조각은 `userData`가 비어 있을 때만 `#cloud-config\npackage_update: true\npackages: [qemu-guest-agent]`과 `guestAgent: true`, 비어 있지 않으면 경고와 함께 살피기로.
- DHCP·고정 주소 → `cloudInit.networks[N]`.
- 자동 → `node = ""`, `zone = null`. 노드 → `node`, `zone = null`(살피기에 "고정"(`pinned`)). 영역 → `node = ""`, `zone = {key: "zone", value, hard: true}` — 만들 때 VM보다 먼저 `POST /placement-rules {name: "<vm>-zone", spec: {kind: "node-affinity", vms: [<vm>], node_tag: "zone=<값>"}}`(규칙은 아직 없는 VM 이름을 받는다 — 스케줄러가 켤 때 따름). 살피기에서 "되도록"(soft)으로 바꿀 수 있다.
- HA → `ha = true`(`haState: started`), HA 그룹 → `haGroup`. 폴더 → `folder`.
- 템플릿 → `base = {kind: "template", id}` + 틀의 디스크·NIC·CD를 템플릿 것으로 보임(잠금), 계산·cloud-init 값은 템플릿 것을 기본으로. 빈 VM 바탕으로 돌아가면 놓기 전 값으로(되돌리기 한 번 — §8.3).
- 틀 블록 → 같은 칸 안 다른 자리: 순서 바꾸기(디스크 순서가 부팅 순서 — 명시 부팅 순서는 비워 기본 규칙 "디스크 차례, CD는 뒤", CD 블록 살피기의 "CD로 먼저 부팅"만 `bootOrder` 1), → `trash`: 빼기(NIC면 cloud-init 주소도 — `changeNics`).

### 7.5 문제(`problems(state, ctx) → Problem[]`)

`Problem = { at: SlotId | "name" | "spec" | "quota"; key: string; params?; level: "error" | "warn"; source: "browser" | "server" }`. 브라우저 문제는 `validate(values, true, volumeNames, sdn)`의 오류 열쇠를 칸으로(`disks.0.size` → `disks.0`, `ci.*` → `cloudInit`, `tpm`·`secureBoot` → `vm`, 나머지 → `compute`·`name`) + 놓은 뒤 바뀐 세상(지운 풀·네트워크, 못 쓰게 된 노드 — `accepts`를 놓인 블록에 다시 물음) + 용량 경고. 서버 문제는 §9의 `problems[]`를 그대로(`source: "server"`). 칸마다 가장 나쁜 수준이 블록 테두리(오류 빨강·경고 노랑 — 색만이 아니라 아이콘과 글).

### 7.6 요청·JSON·CLI

- `requests(state)`: 빈 VM 바탕이면 `[POST /volumes {name, spec: {pool, size_bytes, format: poolFormats(pool)[0]}} …새 디스크마다, POST /placement-rules …영역이면, POST /vms {name, spec: toSpec(values, start ? "running" : "stopped", sdn)}]`, 템플릿 바탕이면 `[POST /placement-rules?, POST /vms/{템플릿 id}/clone {name, vcpus?, memory_mib?, nics[]?, disks[]?, cloud_init?, node?, pool?, linked?, folder?, cpu_model?, start}]`(`DeployDialog`의 본문 짜기를 함수로 꺼내 함께 씀). 만들기(§8.5)와 JSON 미리 보기가 같은 함수를 쓴다.
- **JSON**: 위 요청을 차례대로 `METHOD 경로` + 본문(2칸 들여쓰기). cloud-init 비밀번호는 `"********"`로 가린다(평문을 화면·복사에 남기지 않음 — 붙여 쓸 때 바꾸라는 주석 줄).
- **CLI**(`cliText`): `orbivisorctl volume create <이름> --pool <풀> --size <n>G`, `orbivisorctl placement-rule create <vm>-zone --kind node-affinity --vm <vm> --node-tag zone=<값>`, `orbivisorctl vm create <이름> --cores … --memory … [--cpu-model …] --disk <볼륨>… --cdrom <ISO>… --nic <네트워크>[,sg=a+b][,ip=…] [--node …] [--folder <경로>] [--ha …] [--pinned] --ci-user … --ci-ssh-key '<키>' --ci-ip nic0=dhcp … [--start]`. 비밀번호는 `--ci-password-prompt`, user-data는 `--ci-user-data user-data.yaml`(그 파일 내용을 위에 `cat > … <<'EOF'`로). 템플릿 바탕은 `orbivisorctl template deploy …`(있는 플래그로). CLI 플래그가 없는 값(QoS 세부·NUMA 나누기 등)은 `# CLI 플래그 없음: …` 주석 줄로 알리고 JSON을 권한다. 셸 따옴표는 한 함수(`shq` — 작은따옴표, 안의 `'`는 `'\''`).
- **교차 시험**: 웹 시험이 대표 상태 열두 개의 `cliText`·`requests`를 `web/src/vmBuilder/model/__fixtures__/cli.json`으로 쓰고(파일이 다르면 실패 — 갱신은 `UPDATE_FIXTURES=1`), Rust 시험(`crates/oplctl`)이 그 파일의 CLI 줄을 `Cli::try_parse_from`으로 풀어 가짜 API에 보내 받은 `POST /vms` 본문이 웹의 본문과 같은지 본다. 플래그가 바뀌면 한쪽 시험이 깨진다.

## 8. 빌더 화면

### 8.1 자리

- **VM 탭의 만들기 화면 안 보기 전환** `폼 | 빌더`(제목 옆 둘 단추 — `role="radiogroup"` 아닌 두 링크, 지금 쪽 `aria-current="page"`). 주소 `/vms/-new`(폼)·`/vms/-new/builder`(빌더). 이동 경로 `가상 머신 › VM › 새로 만들기`(빌더도 같음 — `Breadcrumbs`의 `-new` 규칙을 `-new/builder`까지). 사이드바·탭·nav 표는 그대로(계획 61 규칙 — 상세 경로는 그 탭에 속함).
- `App.tsx`에 `{ path: "vms/-new/builder", lazy: page(() => import("@/vmBuilder/BuilderPage"), "BuilderPage") }`. 마지막 보기 기억 `vmCreate.mode`(`writeUserPref`, 사용자별): `/vms/-new`를 열 때 `builder`면 `?folder=` 등 쿼리를 지켜 빌더로 넘김. "+ 만들기" 메뉴·`c`·팔레트의 "VM 만들기"는 그대로 `/vms/-new`(기억을 따름). 팔레트에 "VM 빌더 열기"(별칭 en "builder, drag, compose"·ko "빌더, 끌어 놓기, 조립").
- 권한은 만들기 화면과 같다(`vm.create`가 어디에든 — `createItems`).

### 8.2 넓은 화면(1024px 이상) — 세 칸

```
┌ 부품 상자 (16rem) ───┐┌ VM 틀 ─────────────────────────────────────┐┌ 확인 | JSON | CLI (20rem) ┐
│ [거르기 ______]       ││ 이름 [web-01      ]  폴더 [팀 A ▾]  바탕: 빈 VM ││ ● 서버 확인: 통과(0.4초 전)   │
│ ▸ 크기  작게 보통 …    ││ ┌ 계산 ───────────┐ ┌ 배치 ────────────┐    ││   노드 n3에 놓임 · x86-64-v3 │
│ ▸ OS    Linux Windows ││ │ vCPU ●──── 2     │ │ 자동 / n3 / zone=a│    ││ ▲ 풀 local-n1 빠듯함(1.2 GiB)│
│ ▸ CPU 모델            ││ │ 메모리 ●── 4 GiB │ └──────────────────┘    ││ 할당량 팀 A  vCPU ▓▓▓░ 6/8   │
│ ▸ 디스크  ceph (1.8T) ││ │ CPU 모델 host    │ ┌ 보호 ─┐ ┌ cloud-init ┐ ││        메모리 ▓▓░░ 12/32G │
│          local-n1 …   ││ └─────────────────┘ │ HA 켬 │ │ 사용자 ubuntu│ ││                              │
│ ▸ 설치 매체 ISO …      ││ ┌ 디스크 (위가 부팅) ─────────────────────┐ │ └──────┘ │ 키 1개      │ ││ [만들기] [켠 채로 □]         │
│ ▸ 템플릿              ││ │ ① web-01-disk0  20 GiB  ceph  ⋮         │ │          └────────────┘ ││                              │
│ ▸ 네트워크 lan sdn-a  ││ │ + 여기에 풀·볼륨을 놓기                  │ │                         ││                              │
│   보안 그룹 web ssh   ││ └────────────────────────────────────────┘ │                         ││                              │
│ ▸ cloud-init 조각     ││ ┌ NIC ──────────────────────────────────┐ │                         ││                              │
│ ▸ 배치·보호·폴더      ││ │ nic0  lan  [web][ssh]  DHCP  ⋮          │ │                         ││                              │
└──────────────────────┘│ └────────────────────────────────────────┘ │ ┌ CD ──────┐            ││                              │
                         └───────────────────────── 살피기(아래 펼침) ─┘                         └──────────────────────────────┘
```

- **부품**은 40px 높이 단추(아이콘·이름·작은 회색 정보·"⋯ 더하기" 메뉴). 쓸 수 없는 부품도 보이되 흐리고 까닭이 정보 줄에 있다(숨기면 "왜 없지?"가 됨). 권한이 없어 읽지 못하는 종류(노드 목록을 못 읽는 폴더 사용자 등)는 그 묶음을 빼고 "배치는 서버가 정합니다" 한 줄.
- **VM 틀**의 칸은 비었으면 점선 상자와 "여기에 … 놓기" 안내, 들고 있으면 놓을 수 있는 칸만 밝음(`data-drop="ok"` 테두리 `--brand` 2px), 못 놓는 칸은 그대로(흐리지 않음 — 읽을 수 있게). 블록은 단추(누르면 살피기), `⋮` 메뉴(위로·아래로·빼기·살피기), 끌어 순서 바꾸기·빼기.
- **살피기**는 고른 블록의 칸을 틀 아래(넓은 화면은 블록 바로 아래 펼침)에 보인다: 계산 = vCPU·메모리 **밀대**(`<input type="range">` + 숫자 칸 — 밀대만으로 정확한 값을 넣기 어려움, 메모리는 256 MiB 단위·로그 눈금), 고른 노드의 남은 양 눈금 / 새 디스크 = 이름·크기 밀대(풀 남은 용량까지)·버스 / NIC = 모델·MAC·주소(SDN)·QoS(폼의 칸 조각 `NicsSection` 한 행을 다시 씀) / cloud-init = 폼의 `CloudInitSection`을 그대로 / 배치 = 고정(`pinned`)·영역 hard·soft / 보호 = HA 정책 칸. 빌더에 없는 칸(스냅샷 예약·복제·NUMA 나누기·부팅 순서 표·펌웨어 세부)은 "폼에서 고치기" 링크(§8.6).
- **오른쪽 판** 탭 셋: **확인**(서버 확인 상태 줄 — 확인 중·통과·문제 n·업그레이드 중·확인 못 함, 그리고 문제 목록 — 누르면 그 칸으로 초점, 서버가 고를 노드·해석한 CPU 모델·받을 SDN 주소, 할당량 막대(폴더 사용량 + 이 VM), 노드·풀 여유), **JSON**, **CLI**(둘 다 `<pre>` + 복사 단추 `CopyButton`). 아래 고정 **만들기** 단추와 "켠 채로 만들기" 체크.

### 8.3 다루기

- 들기·놓기는 §6.1 세 길. 부품의 "⋯ 더하기"는 지금 놓을 수 있는 칸 목록 메뉴("디스크로 더하기", "nic1에 붙이기", "배치: 이 노드")라 끌기·들기 없이 같은 일을 한다. 블록 `⋮`의 "위로·아래로"가 순서 바꾸기 대체.
- **되돌리기**: 놓기·빼기·순서 바꾸기마다 상태를 쌓아(20개) `⌘Z`·`Ctrl+Z`(입력 칸 밖)와 "되돌리기" 알림 단추(계획 61 `undo.ts` 꼴). 템플릿 바탕 바꾸기도 되돌린다.
- **처음 화면**: 빈 틀 위에 "부품을 끌어 놓거나 눌러서 고르세요" 안내와 "보통 크기 Linux 서버로 시작"(크기 보통 + Linux + 첫 공유 풀 새 디스크 + 첫 네트워크 NIC) 단추 하나 — 초보자가 바로 만들 수 있게.

### 8.4 살아 있는 확인

1. **즉시(브라우저)**: 값이 바뀔 때마다 `problems()`(§7.5). 이름 칸은 칸 아래 오류(폼의 `Field`).
2. **서버 미리 확인**(`useServerCheck`): 브라우저 오류가 없을 때만, 마지막 바뀜 600 ms 뒤 `POST /vm-checks`(§9) — 하나만 날고(앞 요청은 `AbortController`로 끊음), 결과는 보낸 값의 열쇠(직렬화 해시)와 함께 두어 값이 그 사이 바뀌었으면 흐리게 "바뀐 뒤 다시 확인 중". 409 `cluster_upgrading`·404(옛 노드가 받음)면 "서버 미리 확인은 모든 노드를 올린 뒤에 됩니다 — 만들 때 서버가 확인합니다", 그 밖 실패는 "확인하지 못했습니다(다시)" — 둘 다 만들기를 막지 않는다. 비밀번호 평문은 확인 요청에도 넣지 않는다(빈 칸으로 — 비밀번호 검사는 브라우저 `cloudInitErrors`가 이미 함).
3. **만들기 때(서버)**: 진짜 요청의 오류를 확인 판 맨 위와 그 칸에(폼과 같은 `errorMessage`).

만들기 단추는 브라우저 오류가 있거나 **지금 값에 대한** 서버 문제(오류)가 있으면 막히고 까닭(첫 문제, 누르면 그 칸)이 단추 옆에 있다. 서버 확인이 없거나 늦으면 막지 않는다.

### 8.5 만들기

폼의 `submit`과 같은 차례를 `requests(state)`로: 새 볼륨마다 `createVolume`(Task 기다림, 진행 알림) → 영역 규칙 `POST /placement-rules` → `createVm`(또는 `clone` Task 추적). 화면을 떠나면 기다림을 끊는다(폼과 같은 `AbortController`). 실패하면 이미 만든 것(볼륨·규칙)을 이름과 링크로 알리고(`vmForm.volumesCreated` 꼴 + 규칙), 볼륨은 `existing`으로 바꿔 다시 보내도 또 만들지 않는다(폼 규칙). 성공하면 `/vms/<이름>`(템플릿은 Task 알림 + VM 목록).

### 8.6 폼 ↔ 빌더

- 보기 전환·"폼에서 고치기"는 지금 값을 **모듈 변수 초안**(`vmBuilder/draft.ts` — `{values, base, zone, at}`, 탭 안 메모리)에 두고 옮긴다. 주소 `state`·`sessionStorage`에 두지 않는 까닭: cloud-init 평문 비밀번호가 기록에 남는다. 새로 고치면 초안은 없다(폼 지금 규칙과 같음).
- 폼 → 빌더에서 빌더가 그리지 못하는 값(복제 대상·스냅샷 예약·NUMA 나누기·QoS)은 그대로 들고 다니고 틀 위 "폼에서 고친 칸 n개" 칩으로 알린다(JSON·CLI·만들기에 그대로 들어감).
- 템플릿 바탕은 폼의 "템플릿에서" 프리셋(배포 대화상자)과 같은 요청이고, 폼으로 가면 그 프리셋 + 배포 대화상자에 같은 값을 채워 연다.

### 8.7 휴대폰과 좁은 화면

- **1024px 밑**: 틀 한 칸 + 오른쪽 판은 틀 아래 접은 절(확인·JSON·CLI), 부품 상자는 틀 위 "부품 더하기" 단추 → **아래 서랍**(Radix Dialog, 높이 70vh). 서랍에서 부품을 누르면 서랍이 닫히고 "들림" 상태(누르기 들기 — §6.1)로 틀의 놓을 칸이 밝고 각 칸에 "여기에 놓기" 단추(44px)가 나온다. 길게 눌러 끌기도 된다(서랍 밖으로 끌면 서랍이 닫힘).
- 16px 여백, 가로 스크롤 없음, 밀대는 손가락 크기(트랙 44px 높이 히트 영역), 만들기 단추는 아래 고정 띠(안전 영역 `env(safe-area-inset-bottom)`). 폼과 달리 "넓은 화면에서 쓰기 좋습니다" 안내를 두지 않는다 — 빌더가 휴대폰 길이다.

## 9. 서버 미리 확인 `POST /vm-checks`

### 9.1 요청·응답(새 타입만 — `crates/opl-types/src/vm_check.rs`)

```
POST /api/v1/vm-checks
VmCheckRequest {                     // deny_unknown_fields
  name: String,
  labels: BTreeMap<String,String>,   // default
  spec: serde_json::Value,           // POST /vms와 같음(machine을 빼면 채움)
  volumes: Vec<CreateRequest<VolumeSpec>>,          // default [], 만들 볼륨(디스크가 이름으로 가리킴)
  rules: Vec<CreateRequest<PlacementRuleSpec>>,     // default [], 만들 배치 규칙
}
200 VmCheck {
  ok: bool,                          // problems에 error가 없음
  problems: Vec<VmCheckProblem { at: String, code: String, message: String }>,
  node: Option<String>,              // 켜진 채 노드 없이 만들면 스케줄러가 고를 노드(노드 읽기 권한이 있을 때만)
  machine: MachineType,              // 채운 머신
  cpu_model: CpuModel,               // 별칭을 고정한 모델
  ips: Vec<Vec<IpAddr>>,             // NIC마다 받을 SDN 주소(예상 — 잡아 두지 않음)
}
4XX ErrorBody                        // 403(그 폴더에 vm.create 없음), 409 cluster_upgrading, 400(본문 못 풂)
```

- 경로는 `/vm-checks`다. `/vms/check`는 `/vms/{key}`와 겹친다(이름이 `check`인 VM의 `GET`이 405가 됨 — 볼륨이 `import`·`download` 이름을 막는 까닭). `POST /vms?dry_run=true`는 **옛 노드가 질의를 모르고 진짜 VM을 만든다**(롤링 업그레이드 중 VIP·프록시가 옛 노드로 보낼 수 있음) — 버림.
- 응답 `ok`가 거짓이어도 **200**이다: 문제 목록이 이 요청의 답이다. 4XX는 확인 자체를 할 수 없을 때만.

### 9.2 검사 차례(만들기와 같은 함수, 쓰지 않음)

1. `check_name`(→ `at: "name"`), 같은 이름 VM 있음(`name_taken` → `name`), `with_machine`(실패 → `compute`), 폴더 기능.
2. `authz.require_new(VmCreate, folder)` — **실패하면 403으로 끝**(만들 수 없는 곳의 규칙·노드를 알려 주지 않음).
3. 문제를 **모으는** 검사(하나가 실패해도 다음을 봄): `check_attachments`를 항목마다(네트워크 → `nics.N`, 볼륨 → `disks.N`, ISO → `cdroms.N`, 복제 풀 → `replication`), CPU 플래그 권한(→ `compute`), NUMA 권한(→ `compute`), 만들 볼륨마다 `POST /volumes`의 검사를 함수로 꺼낸 `check_new_volume`(이름·풀 있음·`storage.allocate`·형식·풀 노드 — → 그 볼륨을 가리키는 `disks.N`) + 풀 남은 용량(`PoolState.available_bytes` < 크기 → `warn`), 만들 규칙마다 배치 규칙 만들기 검사(`placement-rule` 쓰기 권한·모양 → `placement`), 할당량 `quota::precheck_vm`에 만들 볼륨 크기를 더한 것(→ `quota`).
4. `prepare`를 **만들 것을 얹은 목록**으로: `prepare_with(state, &mut vm, &scorer, Planned { volumes, rules })` — `Inventory`에 만들 볼륨을 준비된 볼륨으로, 만들 규칙을 규칙으로 더한 뒤 지금과 똑같이 돈다(`prepare`는 `Planned::default()`로 부르는 얇은 함수가 됨 — 만들기·고치기 동작은 그대로). `prepare`는 첫 오류에서 멈추므로 문제는 하나이고, `at`은 오류 `code`로 정한다: `mac_in_use`·`ip_in_use`·`network_*` → `nics`, `volume_*` → `disks`, `placement_rule`·`no_node`·`node_maintenance`·`no_capacity`·위트니스·아키텍처 → `placement`, CPU 모델·플래그(`cpu_*`) → `compute`, HA(`ha_*`·HA 용량) → `protection`, `cluster_upgrading` → `spec`, 그 밖 400 → `spec`. 표는 `vm_checks.rs`의 `area_of(code)` 한 곳이고 시험이 코드마다 본다. 앞 단계 문제가 있으면 4단계 문제와 겹칠 수 있다(같은 볼륨이 둘 다 말함) — 같은 `at`·`code`는 하나로.
5. 통과하면 `prepare`가 채운 값에서 `node`(이름, `hide_node_reasons`와 같은 규칙으로 노드 읽기가 없으면 `null`)·`machine`·`cpu_model`·`ips`.

- **잠그지 않는다**(`vm_lock` 없음, 읽기만). `score::scorer`는 켜진 채 노드 없이 만들 때만 리더에 부하를 묻는다(만들기와 같음).
- **감사 로그에 적지 않는다**: 감사 미들웨어(`audit::layer`)는 GET이 아닌 요청을 모두 적는데, 빌더는 입력마다(0.6초에 한 번) 확인한다. `template == "/vm-checks"`를 읽기처럼 넘긴다(아무것도 바꾸지 않음). 
- **기능 플래그 `vm-check`**(`opl_types::node::VM_CHECK`, `FEATURES`): 저장하는 것은 없지만, 섞인 클러스터에서 웹이 옛 노드에 확인을 보내 404를 받거나 노드마다 다른 규칙으로 답하는 것을 막는다 — 핸들러 처음에 `require_feature(VM_CHECK)` → 409 `cluster_upgrading`, 웹은 모든 노드(위트니스 제외)의 `status.features`에 있을 때만 부른다.
- **serde·OpenAPI**: 지금 타입은 그대로(바이트 호환 — 새 칸 없음). 새 타입 넷(`VmCheckRequest`·`VmCheck`·`VmCheckProblem`, `Planned`는 내부)에 `ToSchema`, `utoipa::path(post, path = "/vm-checks", tag = "vms")`, `web/src/api/schema.d.ts`는 `pnpm -C web gen:api`(utoipa 문서 → 타입)로 다시 만든다 — 지금 규약.
- **CLI**: `orbivisorctl vm create … --dry-run`이 같은 본문을 `/vm-checks`로 보내 `AT  CODE  MESSAGE` 표(`--json`이면 응답 그대로)와 통과면 "ok: node n3, cpu x86-64-v3, machine q35"를 찍고 문제(error)가 있으면 끝 코드 1. CLI의 `vm create`는 있는 볼륨만 붙이므로 `volumes`·`rules`는 비어 있다.

## 10. 클러스터 지도에서 끌어 옮기기

### 10.1 몸짓

- **마우스·펜**: 누른 곳이 **켜진 VM 타일**(`hitTest` → 타일, `phase === "running"`, `availability(vm, "migrate")`가 됨, `vm.migrate` 권한)이고 4px 넘게 움직이면 VM 끌기, 아니면 지금처럼 보기 움직이기. 타일이 없는 단계(요약)에서는 끌기가 없다(타일이 안 보임 — 서랍·`m`).
- **터치**: 타일 위 350 ms 길게 누르기 → VM 끌기(그 전에 움직이면 보기 움직이기 — 지금 그대로). 두 손가락은 늘 확대.
- 끄는 동안: 그림자(VM 이름 알약)가 포인터를 따라가고, 노드마다 놓을 수 있음(`ok` 초록 테두리·`warn` 노랑·`blocked` 빗금 흐림)을 **밝히기 `<style>` 한 줄**로(계획 70 `glowStyle` 꼴 — 블록을 다시 그리지 않음, 시험), 원래 노드는 "여기서" 꼬리표. 포인터 아래 노드(`hitTest`의 노드)의 까닭이 작은 카드(`MapCard` 자리)와 알림 줄에. 그림 가장자리 48px 안이면 보기를 그쪽으로 옮김.
- **놓기**: `ok`·`warn` 노드면 `VmActionHost.open({vm, which: "migrate", node: 노드 id, returnFocus: 노드 <g>})` — **바로 옮기지 않고 지금 대화상자**(로컬 디스크 복사·풀 고르기·CPU·NUMA·규칙·admin 규칙 넘기·확인)가 결정한다. `blocked`·빈 곳·원래 노드면 아무것도 하지 않고 알림 줄이 까닭을 읽는다("놓을 수 없습니다: n4는 유지보수 중입니다").
- 끌기 뒤의 누르기는 열지 않는다(계획 70의 4px 규칙 그대로).

### 10.2 놓을 자리 판정(`clusterMap/drop.ts`, 순수 — "예상")

`dropCheck(vm, node, ctx) → { level: "ok" | "warn" | "blocked"; reason?: string }`, 위에서부터 첫 것:

| 까닭 | 수준 | 자료 |
|---|---|---|
| 같은 노드 | blocked | `vm.status.node` |
| 위트니스·오프라인·유지보수·얼림(종료 중)·아키텍처 다름 | blocked | `migrationTargets`(대화상자에서 `vms/migrate.ts`로 옮김) + `isFrozen` |
| 그 노드에서 VM CPU를 켤 수 없음 | blocked | 들 때 `GET /vms/{key}/cpu`(`useVmCpu` 질의 — 대화상자와 캐시 공유) `nodes[].can_run` |
| 전용 CPU·hugepages·vNUMA가 안 맞음 | blocked | NUMA를 쓰는 VM만 `GET /vms/{key}/numa` `fits` |
| hard 배치 규칙을 어김 | blocked(admin은 warn — 대화상자에서 넘길 수 있음) | `placement/hard.ts`(규칙 목록 + VM 자리로 서버 `hard_violations`를 따라 셈: affinity·anti-affinity·node-affinity·node-anti-affinity, `vms`·`vm_tag`·`nodes`·`node_tag`) |
| 남은 메모리가 VM 메모리보다 적음 | warn | `/placement` `mem_total_mib − mem_allocated_mib`(없으면 판정 안 함) |
| 로컬 디스크를 복사함 | warn(정보) | `localDisks` — "디스크 n GiB 복사" |

- "예상"인 까닭: 서버가 최종이다(대화상자 → `POST /vms/{key}/migrate`가 409로 알림). 판정은 들 때 한 번 하고 끄는 동안 자료가 바뀌면(1초 모음) 다시 한다. CPU·NUMA 질의를 아직 받지 못했으면 그 줄은 판정하지 않는다(대화상자가 봄).
- `placement/hard.ts`는 빌더의 노드 부품 판정(§7.3)도 함께 쓴다. 서버 규칙(`opl-api` `placement.rs`의 `hard_violations`)과 같은 경우를 시험 표로 맞춘다(같은 입력 → 같은 어김 이름).

### 10.3 키보드

| 키 | 어디서 | 동작 |
|---|---|---|
| `m` | VM 타일 단계(`]`로 들어간 뒤) | 옮기기 모드: 알림 "VM web 옮기기. 화살표로 노드를 고르고 Enter로 놓고 Esc로 취소합니다. 놓을 수 있는 노드 3개." — 초점은 VM이 있는 노드 |
| `←` `→` `↑` `↓` `Home` `End` | 옮기기 모드 | 노드 단계 이웃(계획 70 `neighbor`), 노드마다 "n5, 놓을 수 있습니다, 메모리 여유 12 GiB" / "n4, 놓을 수 없습니다: 유지보수 중" |
| `Enter` `Space` | 옮기기 모드 | `ok`·`warn` 노드면 옮기기 대화상자(대상 고름), 아니면 까닭을 다시 읽음 |
| `Esc` | 옮기기 모드 | 취소 — 초점은 그 VM 타일 |

`m`은 꺼진 VM·권한 없음·옮길 수 없는 상태면 까닭만 읽는다("꺼진 VM은 편집에서 노드를 바꿉니다"). 단축키 표(`SHORTCUTS`) 무리 `map`에 `m`("VM 옮기기 — 끌기 대신")을 더한다(그림에 초점이 있을 때만 — `[`·`]`와 같음). VM 서랍의 "옮기기…" 단추는 그대로다(세 번째 길).

## 11. 키보드·화면 읽기·터치·동작 줄이기 (두 화면 함께)

- **탭 차례**: 빌더는 이름 → 폴더 → 부품 상자(묶음 머리 단추 → 부품, 묶음 안은 화살표 `↑`·`↓` — 탭 차례 하나, 부품이 수십 개라서) → 틀 칸(블록마다 탭) → 오른쪽 판 탭 → 만들기. 들고 있는 동안은 놓을 수 있는 칸만 탭 차례(§6.1).
- **이름**: 부품 `button` `aria-roledescription="끌 수 있는 부품"`, `aria-label` "새 디스크, 풀 ceph, 남은 1.8 TiB"(쓸 수 없으면 ", 쓸 수 없음: 권한 없음"), `aria-describedby` 키 안내(숨긴 글 하나). 칸 `role="group"` `aria-label` "디스크, 2개, 문제 1" + 들고 있으면 `aria-dropeffect` 대신 `aria-description` "놓을 수 있음/없음: …". 블록 `button` "디스크 1, web-01-disk0, 20 GiB, ceph, 부팅 첫째".
- **알림**: 들기·칸 위·놓기·취소·되돌리기·서버 확인 결과 바뀜("서버 확인: 문제 2개")을 한 `aria-live="polite"` 줄로. 오류만 바뀌었을 때 확인 판으로 초점을 옮기지 않는다(입력을 빼앗지 않음).
- **터치**: §4·§8.7. 길게 누르기 350 ms(지도·빌더 같은 값 — `dnd/core.ts` 상수), 진동은 쓰지 않음(브라우저마다 다르고 동작 줄이기와 다툼).
- **동작 줄이기**: `prefers-reduced-motion: reduce`이면 칸 맥박·되돌림 날기·서랍 미끄러짐 없음(계획 61 토큰 규칙), 지도 끌기의 자동 보기 옮기기는 한 번에 뛰기(부드럽게 미끄러지지 않음).
- **대비**: 놓을 수 있음 테두리(`--brand`)·경고(`--warning`)·막힘 빗금을 두 테마의 `--card`·`--background` 위 3:1(`colors.test` 꼴), 글은 지금 토큰(4.5:1).

## 12. 화면 자리·주소·번들

- 주소: `/vms/-new/builder`(쿼리 `?folder=` 지킴), 지도 끌기는 주소가 없다(대화상자는 지금처럼).
- i18n: 빌더 문구는 `web/src/vmBuilder/i18n/{en,ko}.json`(키 `vmBuilder.*`), 끌기 틀 문구 `web/src/dnd/i18n/{en,ko}.json`(키 `dnd.*`) — 계획 70의 늦게 받는 문구 규약(`strings.ts` + `addResourceBundle` 얕게). 첫 화면 묶음에 들어가는 것은 보기 전환 두 낱말(`vmForm.viewForm`·`vmForm.viewBuilder` — 폼 조각이 늦게 읽지만 nav 이동 경로와 팔레트 별칭 때문에)과 팔레트 별칭·단축키 `m` 글뿐이다. 지도 끌기 문구는 `clusterMap/i18n`에.
- 기억(`writeUserPref`): `vmCreate.mode`(form·builder), `vmBuilder.groups`(접은 묶음), `vmBuilder.panel`(확인·JSON·CLI).
- **번들 목표**: 첫 화면 묶음 증가 **1 KB 밑**(계획 70 끝 478,370 B), 빌더 조각 gzip **25 KB 밑**(폼의 칸 조각 `CloudInitSection`·`NicsSection` 행은 폼 조각과 공유 — Vite가 나눔), 끌기 틀 공유 조각 **4 KB 밑**, 지도 조각 증가 **2.5 KB 밑**(18,597 B → 21,100 B 밑, `drop.ts`·`hard.ts`·끌기). 새 의존성 없음. 스테이지 3·4가 `eager.sh`와 조각 크기를 §18에 적는다.

## 13. 권한·버전 섞임·다른 기능과의 관계

| 것 | 권한 | 없을 때 |
|---|---|---|
| 빌더 열기 | `vm.create`가 어디에든(만들기 메뉴와 같음) | 보기 전환·팔레트 항목 없음 |
| 부품: 풀·볼륨·ISO·네트워크·보안 그룹·HA 그룹·템플릿 | 그 목록 읽기(`useList`가 주는 것) | 묶음 빠짐 + 한 줄 |
| 부품을 붙이기 | 서버와 같은 동작 권한(`network.attach`·`storage.allocate`·`vm.ha` 등, 폴더·객체 grant까지 `perms.has`) | 흐림 + 까닭, 서버 확인이 최종 |
| 노드·영역 부품 | 노드 읽기(영역은 + `placement-rule` 쓰기) | 배치 묶음 없음 — "서버가 고릅니다" |
| 서버 미리 확인 | 그 폴더의 `vm.create`(아니면 403) | 확인 판 "이 폴더에 만들 권한이 없습니다" |
| 지도 끌기 | VM 읽기 + `vm.migrate`(그 VM) | 그 타일은 끌리지 않음(보기 움직이기), `m`은 까닭 |

- **버전 섞임**: `vm-check`가 없는 노드가 있으면 서버 확인만 꺼지고(§8.4) 나머지는 그대로다 — 만들기·옮기기 요청은 지금 API라 옛 노드도 받는다. 지도 끌기는 서버 변경이 없다.
- **폴더 권한만 있는 사용자**: 노드 목록을 읽지 못하면 머신을 서버에 맡기는 폼 규칙(`machine: ""`)과 같고 배치 묶음이 없다. 할당량 막대는 그 폴더 사용량을 읽을 수 있을 때만.
- **폼과의 관계**: 폼이 기준이다 — 빌더에 없는 칸은 폼이 맡고, 둘은 같은 값·검사·요청을 쓴다. 폼에 새 칸이 생기면 빌더는 "폼에서 고친 칸"으로 들고 다닌다(빌더 시험이 `VmFormValues` 열쇠마다 빌더가 그리거나 들고 다니는지 본다 — 새 칸을 잊지 않게).
- **지도와의 관계**: 끌기는 대화상자를 여는 또 하나의 길이고 옮기기 규칙·API는 그대로다(계획 70 §16의 약속).

## 14. 시험과 CI

- **Rust(opl-api, 스테이지 1)**: `vm_checks` 시험 — 통과(고를 노드·채운 머신·고정한 CPU 모델·SDN 주소 예상, 아무것도 저장 안 됨: 리소스 수·revision 그대로), 문제를 모음(권한 없는 네트워크 + 없는 ISO + 할당량 넘음이 한 응답에 셋), 만들 볼륨(없는 풀·권한 없는 풀·풀 용량 경고·같은 이름 볼륨 있음)과 그 볼륨을 쓰는 디스크가 `prepare`를 통과, 만들 규칙(영역 hard 규칙 + 켜진 채 → 그 영역 노드를 고름, 영역에 노드 없음 → `placement`), `area_of`(코드마다), 폴더 `vm.create` 없음 → 403, 노드 읽기 없음 → `node: null`, 기능 없는 노드가 있으면 409 `cluster_upgrading`, 감사 로그에 없음, `prepare`를 쓰는 지금 시험(만들기·고치기) 그대로. OpenAPI 시험(경로·타입), `oplctl` `--dry-run`(가짜 API — 표·`--json`·끝 코드).
- **순수(vitest)**: `dnd/core`(사건열 → 상태: 문턱·길게 누르기·스크롤로 넘김·취소·키보드 들기와 칸 차례·알림 문구), `autoscroll`(가장자리 거리 → 속도), `vmBuilder/model`(부품 셈 — 권한·용량·아키텍처, `accepts` 표 전체, `drop` 놓기마다, 이름 따라가기, 순서 바꾸기·빼기·되돌리기, `problems`의 열쇠 → 칸, `requests`(빈 VM·템플릿·영역), JSON 비밀번호 가림, `cliText` 따옴표·주석 줄, 교차 시험 고정 파일), `VmFormValues` 열쇠 지킴 시험(§13), `clusterMap/drop`(표 §10.2 줄마다), `placement/hard`(서버 시험과 같은 표).
- **화면(vitest + msw)**: 빌더 — 마우스 끌기(포인터 사건 흉내)로 풀 → 디스크, 네트워크 → NIC, 보안 그룹 → NIC, 노드 → 배치, 키보드만으로 같은 VM(들기·Tab·놓기·Esc 취소·초점 돌아옴), 누르기 들기/놓기, "⋯ 더하기" 메뉴, 순서 바꾸기·빼기·되돌리기, 막힌 칸 까닭(알림 줄 글), 서버 확인(600 ms 모음·앞 요청 끊음·바뀐 뒤 흐림·문제 → 칸 표시·409 → 업그레이드 안내·만들기 막힘과 풀림), 만들기 요청 차례(볼륨 Task → 규칙 → VM)와 실패 때 남은 것 알림, 템플릿 바탕(배포 본문·막힌 칸), 폼 ↔ 빌더 값 지킴(비밀번호가 `history.state`·`sessionStorage`에 없음), 휴대폰 폭(서랍·"여기에 놓기"·가로 스크롤 없음), 동작 줄이기(`matchMedia` 흉내 → 키프레임 `<style>` 없음), **axe**(빈 틀·찬 틀·들고 있는 동안·서랍), 대비 두 테마. 지도 — 타일 끌기 → 대화상자가 그 노드를 고른 채 열림, 빈 곳·막힌 노드·원래 노드에 놓으면 안 열림과 까닭, 노드 끌기는 보기 움직이기 그대로, 터치 길게 누르기, `m` 모드 키 표, 끄는 동안 블록 다시 그리기 0(계획 70 성능 시험 꼴), 권한 없는 VM은 끌리지 않음. `routes.test`(늦은 경로)·`nav.test`(이동 경로)·단축키 시험(`m`은 그림 안에서만)·`i18n.test`(두 파일 키).
- **브라우저 E2E(Playwright, opld `web-it`)**: 새 파일 `web/e2e/builder.spec.ts` — (1) 넓은 화면 마우스: VM › 만들기 → 보기 "빌더" → "보통" 끌기 → 계산, 풀 끌기 → 디스크, 네트워크 끌기 → NIC, 확인 판 "서버 확인: 통과"와 고를 노드, JSON에 `POST /volumes`·`POST /vms`, CLI 줄 → 만들기 → `/vms/<이름>` → 지움. (2) 키보드만: 같은 VM을 `Space`·`Tab`·`Space`로, 알림 줄 글. (3) 390 × 844: "부품 더하기" 서랍 → 누르기 → "여기에 놓기" → 만들기, 가로 스크롤 없음. (4) 지도: `vm.spec.ts` 지도 줄기에 타일 `m` → "놓을 수 있는 노드가 없습니다"(한 노드) 알림과 `Esc`. 두 노드 끌어 옮기기는 vitest와 배포 확인(svc01·svc02).
- **번들**: `pnpm build` 뒤 `eager.sh` 앞뒤, 빌더·끌기 틀·지도 조각 gzip을 §18에.
- **할 수 없는 것과 대신**: VoiceOver(맥)·TalkBack 손 확인(스테이지 3), 진짜 두 노드 끌어 옮기기(배포 확인 — svc01 ↔ svc02, 공유 풀 없으면 로컬 디스크 복사 경고가 대화상자에 보이는지).
- **svc01**: 일반 시험 묶음 전체(서버가 바뀜 — opl-api·oplctl 시험 포함, 웹 묶음 넣은 빌드). **GitHub**: `check`·`web` 통과, `linux-kvm`·`ldap`·`package`는 바뀐 것이 없어 탐침하지 않고 도는 그대로 통과만 본다.

## 15. 견준 길

- **끌기: 직접 짠 포인터 틀(채택) / `@dnd-kit` / HTML5 DnD / React Aria.** dnd-kit은 키보드 센서와 알림이 있어 가장 가깝지만 13~16 KB이고 충돌 판정이 DOM 사각형이라 지도(합친 경로 — 원소 없음)에 쓰려면 결국 찾기 함수를 따로 꽂아야 한다. HTML5 DnD는 터치·키보드·SVG 타일이 안 된다(§4). React Aria는 접근성이 가장 좋으나 25 KB 넘고 지도에 맞지 않는다. 쓰는 기능이 좁아(문턱·길게 누르기·칸 모으기·자동 스크롤·키보드 들기·알림) 직접 짜면 4 KB 밑이고 두 화면이 같은 규칙을 쓴다. 대가: 우리가 시험해야 할 사건 경우가 늘어난다(상태 기계를 순수로 두어 사건열 시험).
- **확인: 서버 미리 확인 API(채택) / 브라우저에서 서버 규칙 흉내 / 만들 때만.** 할당량(폴더 사슬·백업 바이트)·스케줄러 점수·CPU 카탈로그·IPAM·HA 용량·hard 규칙을 웹에 다시 짜면 서버와 갈라진다(이미 `defaultMachine`이 "서버와 같다"를 손으로 지킨다). 같은 함수를 쓰는 미리 확인은 갈라질 수 없고 CLI `--dry-run`도 얻는다. 대가: 서버 경로 하나·기능 플래그 하나·`prepare`에 만들 것 얹기. 브라우저 검사는 즉시 반응(놓기 판정)을 위해 남기되 "예상"이고 서버가 최종이다.
- **확인 경로: `POST /vm-checks`(채택) / `POST /vms?dry_run=true` / `POST /vms/check`.** 질의 꼴은 옛 노드가 진짜로 만든다, `/vms/check`는 VM 이름 `check`와 겹친다(§9.1).
- **확인을 문제 목록으로(채택) / 첫 오류 4XX.** 빌더는 여러 칸을 한 번에 보여야 한다. 독립 검사는 모으고 `prepare`만 첫 오류다 — `prepare`를 모든 오류를 모으게 고치면 만들기 동작이 바뀔 위험이 크다.
- **빌더 = 폼 값의 다른 편집기(채택) / 따로 된 빌더 모델과 변환.** 따로 두면 폼에 새 칸이 생길 때마다 빌더가 늦는다. 같은 값이면 검사·요청·프리셋을 그대로 쓰고 폼 ↔ 빌더가 공짜다. 대가: 빌더가 그리지 않는 칸을 "들고 다니기"로 알려야 한다.
- **칸이 정해진 틀(채택) / 자유 그림판(노드 잇기).** 자유 그림판은 보기에 화려하지만 VM 스펙은 나무 모양(디스크 목록·NIC 목록)이라 선 잇기가 뜻을 더하지 않고, 키보드·화면 읽기·휴대폰이 어렵다. 칸 틀은 놓을 곳이 분명해 초보자가 덜 틀린다.
- **자리: 만들기 화면 안 보기 전환(채택) / VM 탭의 새 보기 / 대화상자.** 계획 61 규칙(새 사이드바·탭 없음). 만들기는 이미 VM 탭의 상세 경로(`/vms/-new`)이고 폼과 빌더는 같은 일의 두 보기다. 대화상자는 세 칸 화면에 좁다.
- **영역에 놓기 = 배치 규칙 만들기(채택) / 빌더가 영역 안 노드를 하나 고름 / 영역 칸 없음.** 노드를 하나 고르면 영역의 뜻(그 노드가 죽으면 영역 안 다른 노드로)이 사라진다. 규칙은 HA·최적 배치·지도가 이미 쓰는 `zone` 레이블 그대로다. 대가: admin만(규칙 쓰기 권한), 만들기 실패 때 남는 규칙 알림.
- **지도 끌기: 타일에서 시작한 누르기만 끌기(채택) / 끌기 모드 단추 / 끌기 없음.** 모드 단추는 한 번 더 눌러야 하고 잊기 쉽다. 타일이 아닌 곳은 보기 움직이기로 남아 계획 70 손버릇이 그대로이고, 놓으면 바로 옮기지 않고 확인 대화상자라 실수가 싸다.

## 16. 나중 할 일

- VM **고치기** 빌더(실행 중 핫플러그 규칙 — 끝에 붙이기만·가운데 떼기 막힘을 칸 규칙으로).
- 템플릿 배포의 서버 미리 확인(`/vm-checks`에 `clone` 꼴), 여러 VM 한 번에(이름 꼴 `web-{1..3}`).
- 이미지 라이브러리에서 받으며 만들기(카탈로그 이미지 → 풀에 받기 Task → 디스크), 클라우드 이미지 디스크를 복제해 붙이기.
- 폴더 끌어 놓기를 이 틀로(터치·키보드 들기), 지도에서 여러 VM 끌기·영역 판으로 끌기(비우기·규칙), 꺼진 VM 끌기 = 노드 바꾸기.
- 빌더 결과를 선언형 파일(로드맵의 선언형 적용·Terraform)로 내보내기, 저장해 두는 "내 부품"(자주 쓰는 NIC+보안 그룹 묶음).

## 17. 스테이지 나누기

각 스테이지는 커밋 하나이고, 혼자 컴파일되고 시험된다(`cargo test`·`clippy`·`fmt`, `pnpm typecheck`·`lint`·`test`, 스테이지 5는 `web-it`). 화면은 스테이지 3(빌더)·4(지도 끌기)에서 열린다(스테이지 2는 쓰는 곳 없는 순수 모듈과 그 시험만).

1. **feat(api,cli): VM 만들기 미리 확인 POST /vm-checks — 만들 볼륨·배치 규칙을 얹은 같은 검사, 문제마다 자리, 기능 vm-check, vm create --dry-run** — `crates/opl-types/src/vm_check.rs`(새 타입), `crates/opl-api/src/vm_checks.rs`(핸들러·`area_of`), `vms.rs`의 `prepare_with(Planned)`(지금 `prepare`는 빈 `Planned`로), `volumes.rs`의 `check_new_volume`(만들기와 함께 씀), `quota.rs`의 만들 볼륨 더하기, `audit.rs` 넘김, `node.rs` `VM_CHECK`, `lib.rs` 경로와 OpenAPI 등록, `web/src/api/schema.d.ts`(`gen:api`), `oplctl` `--dry-run`, 시험(§14 Rust).
2. **feat(web): 끌어 놓기 틀과 VM 빌더 모델 — 포인터·누르기·키보드 들기와 놓기, 알림 줄, 부품·칸·놓기 규칙·문제·요청·JSON·CLI(순수)** — `web/src/dnd/`(`core`·`autoscroll`·훅·`Announcer`·문구), `web/src/vmBuilder/model/`(+ 시험, 교차 고정 파일), `web/src/placement/hard.ts`, `web/src/vms/migrate.ts`(옮김), `templates`의 배포 본문 함수 꺼냄, `crates/oplctl` 교차 시험(고정 파일의 CLI 줄 → 가짜 API 본문).
3. **feat(web): 끌어 놓기 VM 빌더 화면 — 만들기 화면의 보기 "빌더", 부품 상자·VM 틀·살피기·살아 있는 확인·JSON/CLI 미리 보기·만들기, 휴대폰** — `App.tsx` 늦은 경로, `VmFormPage` 보기 전환·초안, `Breadcrumbs`, 팔레트 항목, `web/src/vmBuilder/{BuilderPage,PartsPanel,Chassis,Inspector,SidePanel,useServerCheck,draft}.tsx`·문구, vitest(화면·키보드·누르기·서버 확인·만들기·폼 ↔ 빌더·휴대폰·동작 줄이기·axe·대비), VoiceOver 손 확인.
4. **feat(web): 클러스터 지도에서 VM 타일을 노드로 끌어 옮기기 — 놓을 수 있는 노드 밝히기와 까닭, 키보드 m, 옮기기 대화상자** — `clusterMap/drop.ts`(+ 시험), `MapScene` 포인터 갈래·밝히기 `<style>`·`m` 모드, `VmMigrateDialog`가 `vms/migrate.ts`를 씀, 단축키 `m`, 문구, vitest(끌기·키보드·터치·다시 그리기 0·axe), 번들 기록.
5. **test(web,opld): VM 빌더·지도 끌어 옮기기 브라우저 E2E — 마우스·키보드·휴대폰 폭으로 만들기, 지도 m, 번들 크기 기록** — `web/e2e/builder.spec.ts`, `vm.spec.ts` 지도 줄기에 `m`, 필요하면 `crates/opld/tests/web.rs`, `eager.sh` 앞뒤와 조각 크기를 §18에.
6. **docs: 웹 가이드의 VM 빌더와 지도 끌어 옮기기, 미리 확인 API·CLI, 설계 문서 스테이지 보고, 계획 71 실행 기록** — `docs/guide/web-ui.md`(마법사 절의 빌더, 새 절 "VM 빌더 화면", 클러스터 지도 절의 끌어 옮기기·`m`, 단축키 표, 한계 고침), `docs/guide/vms.md`(`POST /vm-checks`·`vm create --dry-run`), `docs/guide/development.md`(`pnpm -C web test src/dnd src/vmBuilder`, `builder.spec.ts`, 교차 고정 파일 갱신), `docs/index.md`, README 한 줄, `docs/superpowers/plans/2026-11-25-plan71-dnd-builder.md`.

## 18. 스테이지 보고 (설계와 다른 것·결정)

### 스테이지 1

- **요청 타입 자리**: `VmCheckRequest`는 `opl-types`가 아니라 `crates/opl-api/src/vm_checks.rs`에 둔다 — `volumes`·`rules`가 쓰는 `CreateRequest<S>`가 opl-api 타입이다(OpenAPI 이름 `CreateRequest_VolumeSpec`·`CreateRequest_PlacementRuleSpec`, `spec`은 `VmSpec` 스키마). 응답 `VmCheck`·`VmCheckProblem`·`VmCheckLevel`은 `opl-types/src/vm_check.rs`.
- **문제에 `level`(`error`·`warn`)을 더함**: §9.1 표에는 없지만 §9.2의 풀 용량 경고와 `ok`(오류 없음)의 정의에 필요하다. 경고 코드는 `pool_space`(만들 볼륨의 노드가 알린 풀 상태가 `total_bytes > 0`이고 `available_bytes < size_bytes`).
- **`node`·`machine`·`cpu_model`은 null일 수 있음**: `prepare`를 지나지 못했으면(또는 머신을 채우지 못했으면) null, `ips`는 빈 목록. `prepare`를 지나면 앞 단계 문제(권한·할당량 등)가 있어도 채운다. `node`는 켜진 채 노드 없이 만들 때 스케줄러가 고른 노드나 준 노드의 이름이고, 루트 `cluster.read`가 없으면 null.
- **자리 정하기**: `area_of(code)` 표에 더해 `slot_of` — 문구가 따옴표 이름(`{:?}`)으로 가리키는 NIC 네트워크·디스크 볼륨·CD ISO가 있으면 `nics.N`·`disks.N`·`cdroms.N`이다. `prepare`의 많은 400이 `invalid_request` 한 코드라서(예: 없는 ISO `volume "x.iso" not found` → `cdroms.0`, `network "x" not found` → `nics.N`). 자리가 정해진 코드(`no_node` 등)는 그대로.
- **감사**: 미들웨어가 `/vm-checks`를 GET처럼 그냥 넘기면 요청 자리가 없어 `audit::principal()`이 None이 되고, IPAM이 주체 없음을 관리자처럼 본다(공급자 네트워크 주소 검사를 건너뜀 — 만들기와 갈라짐). 그래서 자리(주체)는 그대로 두고 **기록만 하지 않는다**(`QUIET_CHECK`).
- **`check_attachments`**를 항목마다 자리를 붙여 모두 모으는 `attachment_problems` 위로 다시 썼다(만들기는 그 첫 거절). 여러 볼륨이 함께 거절될 때 알리는 것이 이름 순 첫 것에서 디스크 순 첫 것으로 바뀐다(상태·코드는 같음).
- **머신을 채울 수 없음**(아키텍처 섞임 400): spec을 `VmSpec`으로 풀 수 없으므로 spec의 `folder`만 읽어 권한(403)을 본 뒤 `compute` 문제 하나로 답한다. 그 밖에 spec을 풀 수 없으면 400.
- **만들 규칙**: 권한(`placement-rule` 쓰기) → 이름 → 같은 이름 규칙(`name_taken`) → `placement_rules::check`(형식·노드·모순) 차례로 `placement`. 맞는 노드가 없는 영역 규칙은 규칙 만들기가 422 `rule_conflict`로 거절하므로 `rule_conflict`와 `prepare`의 `placement_rule`이 함께 `placement`에 나온다.
- **만들 볼륨**: 같은 이름 볼륨(또는 만들 볼륨끼리 같은 이름) → `name_taken`. 검사를 지나지 못한 볼륨도 풀이 있으면 그 노드에 둔 준비된 볼륨으로 얹는다(스케줄러가 "풀에 닿는 노드 없음"을 덧붙이지 않게). 할당량에는 만들 볼륨 크기를 디스크로 더한다(`quota::precheck_new_vm`).
- **CLI** `vm create --dry-run`: 경고는 `MESSAGE` 앞에 `warning: `, 오류가 있으면 표 뒤 `error: the check found N problem(s); the VM would not be created`(끝 코드 1), `--start`와 함께 쓸 수 없다. `-o json`은 답 그대로.
