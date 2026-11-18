# 웹 화면 구성·UX 개편 설계 (계획 61)

작성일: 2026-11-18
상위 문서: `2026-11-11-monitoring-ui-design.md`(계획 44 화면 스케치), `2026-11-18-dashboard-metrics-notifications-design.md`(계획 60 — 차트·대시보드·경보, §12 웹), `2026-11-18-vtpm-secure-boot-guest-agent-design.md`(계획 59 — 문서 틀)
기준 트리: main `0729922`(계획 59 끝). 계획 60은 다른 가지에서 구현 중이고 웹 단계(S7)는 아직 시작하지 않았다. 이 문서는 구현 전 설계다. 스테이지 보고는 구현하면서 덧붙인다.

## 1. 목적

사용자 요청은 "최신 트렌디한 화면 구성, 메뉴는 최소한, 사용자 편의성과 편리한 운영"이다. 덧붙인 말은 "좌측 메뉴 구성을 최대한 간단하게, 들어가서 상세 메뉴를 선택"이다. 지금 웹(`web/src/layout/AppLayout.tsx`)은 왼쪽에 **26개 항목을 평평하게** 늘어놓는다. 무엇이 무엇과 묶이는지 보이지 않는다(볼륨과 ISO 사이에 템플릿이 있고, 설정 성격의 사용자·토큰이 작업 옆에 있다). 화면 사이를 오가는 길도 메뉴 하나뿐이다. 검색, 명령 팔레트, 작업 센터, 즐겨찾기, 단축키가 없다. 일괄 동작은 VM 목록의 시작·정지 둘뿐이다(`VmListPage.tsx`의 `bulk`). 색 토큰은 shadcn neutral 그대로여서 상태색과 차트색(`--chart-1..5`)이 모두 회색이다. 계획 60이 더할 차트·경보 화면이 26개 메뉴 위에 또 쌓이기 전에 뼈대를 바꾼다.

| | Proxmox VE 9 | vSphere Client 8 | Nutanix Prism Central | Orbivisor 계획 61 |
|---|---|---|---|---|
| 이동 | 리소스 트리 + 노드·데이터센터마다 긴 세로 메뉴 | 인벤토리 트리 + 탭 | 앱 전환 메뉴 + 엔터티 목록 | 왼쪽 영역 6개만, 상세는 영역 안 탭(§4) |
| 검색·명령 | 트리 검색 | 전역 검색 | 검색 + 동작 | ⌘K 팔레트: 리소스·IP·동작·모든 하위 화면(§5.2) |
| 작업 | 아래 작업 패널 | Recent Tasks 패널 | 알림 센터 | 머리줄 종 서랍: 진행률·경보·다시 하기(§5.4) |
| 일괄 동작 | 일괄 시작·정지(노드 메뉴) | 여러 개 고르기 | 목록 일괄 동작 | 저장 보기·열·밀도·일괄 동작 공통 표(§5.3) |
| 휴대폰 | 별도 모바일 UI | 없음 | 없음 | 같은 앱의 당직 보기(홈·경보·VM 전원, §5.9) |

## 2. 범위

| 넣는 것 | 넣지 않는 것(§12) |
|---|---|
| 영역 6개 IA, 아이콘 레일 사이드바, 영역 안 탭, 마지막 탭 기억, 옛 주소 넘기기 | 사용자가 메뉴를 고치는 기능, 서버에 두는 사용자 설정 |
| ⌘K 명령 팔레트(이름·IP·동작·이동), 머리줄 검색, "+ 만들기" | 서버 쪽 전문 검색 API, 자연어 명령 |
| 공통 데이터 표(저장 보기·열·밀도·선택·일괄 동작·상태 칩), 미리 보기 서랍 | 표 셀 안 편집, CSV 내보내기 |
| 상세 틀(머리·요약 카드·탭), 작업·알림 센터, 다시 하기, 깊은 링크 | 서버의 Task 다시 하기 API(파라미터가 Task에 없음) |
| 실시간 표시·낙관적 갱신·스켈레톤, 빈 상태 첫 걸음, VM 만들기 프리셋·클러스터 합류 마법사 | 끌어 놓기 대시보드, 3D 보기 |
| 단축키와 도움말, 이동 경로, 최근 항목·즐겨찾기(브라우저 저장) | 여러 브라우저 사이 즐겨찾기 동기화 |
| 디자인 토큰(색·모서리·간격·그림자·글자), 밝음·어두움·시스템, 밀도, 움직임 줄이기 | 무거운 UI 프레임워크 바꾸기, 화면 회귀 스냅숏(선택, §12) |
| 키보드·포커스·ARIA, ko/en 전부, axe 검사, 토큰 대비 시험 | RTL 언어 |
| 계획 60 화면 흡수(홈·노드 상세·지표 탭·경보·알림 대상·Prometheus) | 계획 60의 서버·API |

새 런타임 의존성은 없다. Sheet·Tooltip·Popover·ToggleGroup은 이미 있는 `radix-ui` 1.8.1 묶음에서 shadcn 모양 파일로 더한다. 개발 의존성 `axe-core` 하나(정확한 버전 `4.10.3`)만 더한다. `cmdk`는 넣지 않는다(§11).

## 3. 지금 있는 것 (계획 59 끝)

- **뼈대**: `App.tsx`의 `createBrowserRouter`에 평평한 경로 34개가 있다. `AppLayout`은 사이드바(`NAV` 26개, `kind`·`admin`으로 `can()` 거르기)와 머리줄(클러스터 이름·`LiveBadge`·언어·테마·사용자 메뉴)로 되어 있다. 768px 아래에서는 사이드바가 가로 스크롤 줄이 된다. opld `web.rs`의 fallback이 모든 화면 주소에 `index.html`을 주므로 새 경로·넘기기는 웹 안에서만 하면 된다.
- **데이터**: `useList(kind)`(`api/live.ts`)는 `/watch`로 13종을 실시간으로 받는다. `TaskIndexProvider`는 대상 id → 끝나지 않은 Task 색인이다. `trackTask`·`TaskToasts`는 202 Task를 sonner 알림으로 따라간다. Task에는 `progress {stage, done_bytes, total_bytes}`가 있고, 동작 파라미터(옮길 노드 등)는 없다.
- **목록·상세**: react-table로 된 화면별 표가 있다. VM 목록에는 정렬 저장(`prefs` `vms.sort`)·검색·선택·일괄 시작/정지가 있다. VM 상세는 `?tab=`(summary·hardware·guest·console·snapshots·backups·tasks·json)이다. 노드는 상세 화면 없이 대화상자들이다. `ConfirmDialog`에는 `typeToConfirm`이 있다. `VmMenu`는 행 동작 메뉴다.
- **모양**: Tailwind 4 + `shadcn/tailwind.css`, Geist, `.dark` 클래스(`lib/theme.tsx`, system 기본), `lib/prefs.ts`(localStorage `opl.*`, 실패 무시). `components/ui`는 14개다. 단축키 코드는 없다.
- **e2e**: `web/e2e/vm.spec.ts`(GitHub `linux-kvm`의 "Web UI and browser")가 사이드바 링크 "Storage pools"·"Volumes"·"Virtual machines"와 제목 "Dashboard"를 누르고 본다.

## 4. 정보 구조와 이동

### 4.1 굳은 규칙

1. **왼쪽 사이드바에는 최상위 영역만 둔다.** 홈·가상 머신·인프라·보호·활동 다섯 개를 위에, 설정을 맨 아래에 고정한다. 펼치는 트리, 둘째 단계 항목, 그룹 제목은 두지 않는다. 사이드바는 아이콘 레일(56px)로 접힌다.
2. **상세 메뉴는 영역에 들어간 뒤 본문 위쪽의 영역 탭(`AreaTabs`)으로 고른다.** 탭 안에서 다시 둘로 나뉘는 곳(노드·클러스터 같은 곳)은 탭 아래의 보기 전환(segmented, `ViewSwitch`)으로 고른다. 이것도 경로 링크다.
3. **영역은 마지막으로 고른 탭을 기억한다.** 사이드바에서 영역을 누르면 그 탭으로 간다(§4.4).
4. **⌘K 팔레트는 모든 하위 화면에 곧바로 간다.** 메뉴를 거치지 않아도 된다(§5.2).
5. **한 리소스의 집은 한 곳이다.** 다른 영역에서는 링크만 건다.

### 4.2 영역과 탭

| 영역(사이드바) | 영역 탭 → 경로(보기 전환) | 영역 주소 |
|---|---|---|
| 홈 | 없음(대시보드 한 화면) | `/` |
| 가상 머신 | VM `/vms` · 템플릿 `/templates` · 이미지·ISO(`/images` \| `/isos`) · 가져오기 `/imports` | `/vms` |
| 인프라 | 노드(`/nodes` \| 클러스터 `/cluster`) · 스토리지 풀 `/storage-pools` · 볼륨 `/volumes` · Ceph `/ceph` · 네트워크 `/networks` · 보안 그룹 `/security-groups` | `/infra` |
| 보호 | 백업 `/backups` · 백업 작업 `/backup-jobs` · 원격 복제 `/remotes` · HA 그룹 `/ha-groups` · HA 검사 `/ha-checks` · 배치(규칙 `/placement-rules` \| 최적 배치 `/placement`) | `/protect` |
| 활동 | 작업 `/tasks` · 알림 `/alerts`(계획 60) · 감사 `/audit`(admin) | `/activity` |
| 설정(아래 고정) | 일반 `/settings` · 사용자 `/settings/users` · 인증 영역 `/settings/realms` · 토큰 `/settings/tokens` · 원격 클러스터 `/settings/remotes` · 알림 대상 `/settings/notifications`(계획 60) · 구독 `/settings/subscription` | `/settings` |

- **상세 경로는 그 탭에 속한다.** `/vms/:name`·`/vms/-new`·`/vms/:name/edit|console`은 VM 탭, `/imports/:name`은 가져오기, `/nodes/:name`(계획 60)은 노드, `/security-groups/:name`은 보안 그룹, `/remotes/:name`(원격 클러스터의 VM)은 원격 복제 탭이다. 콘솔(`/vms/:name/console`)은 영역 탭 없이 전체 폭으로 둔다.
- **원격**은 둘로 나눈다. 보호 > 원격 복제(`/remotes`)는 원격 클러스터의 상태 카드, 원격 대상이 있는 백업 작업 목록(`RemoteTargetFields`가 쓰는 작업), 원격 VM 보기다. 설정 > 원격 클러스터(`/settings/remotes`)는 주소·토큰 추가·고치기·지우기다(admin). 지금 `RemotesPage`의 카드 목록은 앞쪽으로, 쓰기 대화상자는 뒤쪽으로 옮긴다.
- **설정 > 일반**은 업데이트·종료·HA·텔레메트리·Prometheus(계획 60) 칸이다. 구독은 탭으로 뗀다(`SubscriptionBanner`는 홈에 남는다).
- **권한**: 탭마다 지금 `NAV`의 `kind`·`admin`을 그대로 옮긴다. 보이는 탭이 하나도 없는 영역은 숨긴다. 토큰 탭은 모두에게 보인다(자기 토큰).
- **한 곳의 정의**: `web/src/layout/nav.ts`의 `AREAS`(id, `nav.*` 키, 아이콘, `tabs: {to, key, kind?, admin?, views?, match?}`)가 사이드바·영역 탭·이동 경로·팔레트 이동 항목·넘기기 시험의 원천이다. `areaOf(pathname)`은 `match`(접두사 목록)로 영역과 탭을 찾는다.

### 4.3 주소와 넘기기

리소스 주소는 거의 그대로 둔다. 북마크, 문서(`docs/guide`), e2e가 깨지지 않게 하기 위해서다. 바뀌는 것은 설정으로 옮기는 셋뿐이다. `/users` → `/settings/users`, `/realms` → `/settings/realms`, `/tokens` → `/settings/tokens`로 넘긴다. 새 영역 주소 `/infra`·`/protect`·`/activity`는 기억한 탭으로 넘긴다. `LEGACY_REDIRECTS`(`nav.ts`)의 `Redirect` 요소는 `replace`이고 쿼리와 해시를 지킨다(`/tokens?x=1#a` → `/settings/tokens?x=1#a`). react-router의 `<Navigate to>`는 쿼리를 버리므로 쓰지 않는다. 계획 60이 만든 `/alerts?tab=targets`가 있으면 `/settings/notifications`로 넘긴다(§8.2를 따르면 생기지 않는다).

### 4.4 기억과 이동 경로

- **마지막 탭**: `prefs` 키 `nav.last.{area}`(경로와 쿼리)다. 영역 탭을 바꿀 때 쓴다. 읽을 때는 `AREAS`의 탭이고 지금 권한으로 보이는 것만 받는다. 아니면 보이는 첫 탭이다. 상세 경로(`/vms/web`)는 기억하지 않고 그 탭의 목록을 기억한다.
- **이동 경로**: 머리줄 왼쪽에 `영역 › 탭 › 리소스`를 둔다(`nav.ts`로 만들고 리소스 이름은 경로 파라미터). 768px 아래에서는 마지막 둘만 보인다.

```
┌──────┬──────────────────────────────────────────────────────────────────────┐
│ ◎    │ 인프라 › 스토리지 풀        [🔍 검색 또는 명령  ⌘K] [+ 만들기] 🔔3 ●live 👤 │
│──────│──────────────────────────────────────────────────────────────────────│
│ ⌂ 홈 │ 인프라                                                                │
│ ▣ VM │ ┌────┬──────────┬────┬────┬────────┬────────┐                       │
│ ▤ 인프│ │노드│스토리지 풀│볼륨│Ceph│네트워크│보안 그룹│   ← 영역 탭(AreaTabs)  │
│ ⛨ 보호│ └────┴══════════┴────┴────┴────────┴────────┘                       │
│ ↯ 활동│ 스토리지 풀                                   [+ 풀 만들기]          │
│      │ [🔍 이름·노드…] [상태 ▾] [보기: 기본 ▾] [열 ▾] [≡ 밀도]               │
│      │ ┌────────────────────────────────────────────────────────────────┐ │
│      │ │ ☐ 이름      종류  노드     사용률            상태              │ │
│      │ │ ☐ local     dir   a,b,c    ████░░ 62%        ● 준비            │ │
│      │ │ ☐ ceph-rbd  rbd   전체     ██░░░░ 31%        ● 준비            │ │
│      │ └────────────────────────────────────────────────────────────────┘ │
│──────│                                                                      │
│ ⚙ 설정│                                                                      │
│ «    │ ← 레일 접기([)                                                        │
└──────┴──────────────────────────────────────────────────────────────────────┘
접힌 레일: 아이콘만, 마우스·포커스에 Tooltip으로 이름. 하위 항목 플라이아웃은 두지 않는다(규칙 1).
```

## 5. 운영자를 위한 전역 UX

### 5.1 머리줄

왼쪽부터 이동 경로, 검색 상자, **+ 만들기**, 클러스터 전환, 종, `LiveBadge`, 사용자 메뉴 순이다. 검색 상자는 팔레트를 여는 단추 모양이고 `⌘K`를 보인다. + 만들기는 VM·VM 가져오기·풀·네트워크·백업 작업·ISO 올리기 메뉴이고 권한(`can(write)`)으로 거른다. 각 항목은 지금 만들기 화면·대화상자를 그 화면의 `?create=1`로 연다. 클러스터 전환은 이 클러스터와 `/remotes` 목록(읽기 권한이 있을 때)을 보이고, 원격을 고르면 `/remotes/:name`으로 간다. 사용자 메뉴에는 언어·테마·밀도·단축키 도움말·비밀번호·로그아웃이 들어간다(머리줄 단추 둘이 줄어든다).

### 5.2 명령 팔레트(⌘K / Ctrl+K)

```
┌──────────────────────────────────────────────────────┐
│ 🔍 migrate web to node2                          esc │
├──────────────────────────────────────────────────────┤
│ 동작                                                  │
│ ▸ web을 node2로 옮기기…              (대화상자 열기)  │
│ 가상 머신                                             │
│   web        ● 실행 중  node1  10.0.0.21              │
│   web-db     ○ 꺼짐     node2                         │
│ 이동                                                  │
│   인프라 › 노드                               g i     │
├──────────────────────────────────────────────────────┤
│ ↑↓ 고르기  ↵ 실행  ⌘↵ 새 탭  ⇥ 동작 보기             │
└──────────────────────────────────────────────────────┘
```

- **원천**은 `useList` 캐시의 VM·노드·풀·볼륨·네트워크·보안 그룹·백업 작업·HA 그룹, `nav.ts`의 모든 탭·보기, 정적 명령(만들기·테마·밀도·로그아웃), 최근 항목·즐겨찾기다. 새 API 호출은 없다.
- **IP 검색**: 노드 주소, VM cloud-init 고정 IP와 NIC MAC, 네트워크 CIDR(포함 검사), 이미 열어 본 VM의 게스트 정보 캐시(`GET /vms/{key}/guest`, 계획 59)를 본다. 계획 59가 정한 대로 모든 VM의 게스트 IP를 미리 모으지 않는다. 그래서 DHCP IP는 게스트 탭을 연 VM만 찾힌다(README 한계에 적는다).
- **동작 문법**: `{동사} {대상} [to {노드}]`이고 동사는 start·stop·reboot·force-stop·migrate·snapshot·console·open과 한국어 별칭(켜기·끄기·옮기기…)이다. 대상은 이름 접두사로 찾는다. 동작은 `VmActionHost`(§5.3)로 그 VM의 **같은 확인 대화상자**를 미리 채워 연다. 팔레트가 바로 실행하는 것은 확인이 없는 동작(start·console·open)뿐이다. 권한과 `availability()`가 막으면 이유와 함께 흐리게 보인다.
- **순위**: 정확 > 접두사 > 단어 접두사 > 부분 문자열이다. 즐겨찾기·최근이 같은 점수면 앞이다. 목록은 종류마다 8개까지 보인다. 순수 함수 `palette/rank.ts`로 시험한다.
- **접근성**: 입력은 `role="combobox"`, `aria-expanded`, `aria-activedescendant`이고 목록은 `role="listbox"`·그룹 `role="group"`이다. radix Dialog가 포커스를 가두고 닫으면 돌려준다.

### 5.3 목록 → 상세

- **공통 표 `components/data-table/`**(`DataTable`, `useTableState`, `Toolbar`, `BulkBar`, `ColumnMenu`)는 react-table 위에 얹는다.
  - 상태는 URL 쿼리에 둔다(`?q=&phase=running&node=a&sort=-memory`). 그래서 링크로 나눌 수 있다.
  - 저장 보기는 이름 붙인 쿼리이고 `prefs` `views.{table}`에 둔다. 기본 보기 몇 개는 코드에 둔다(VM: 실행 중·실패·이 노드). 열 고르기·순서는 `cols.{table}`에 둔다(지금 `vms.sort`는 옮겨 읽는다).
  - 밀도는 전역(§6)이다. 선택은 체크박스와 Shift 범위 선택이고, 필터가 바뀌면 보이지 않는 선택을 푼다.
  - 상태 칩 `StatusChip`(tone + 점 + 글)은 `VmPhaseBadge`·풀·노드·Task·경보가 함께 쓴다. 색만으로 뜻을 전하지 않는다(글·아이콘).
  - 로딩은 스켈레톤 행이고, 빈 상태와 오류는 `TableMessage`를 넓힌다.
- **VM 일괄 동작**: 시작·정지·재시작·스냅샷·옮기기·지우기다. `BulkBar`는 고른 수와, 동작마다 "가능 n / 불가 m"을 보인다(`availability()`).
  - 확인 대화상자는 영향 목록(이름·노드·상태, 불가 이유)을 보인다. 지우기와 강제 정지는 고른 수를 입력해야 한다.
  - 스냅샷 이름은 `bulk-YYYYMMDD-HHMM`이다. 옮기기는 대상 노드 하나를 고르고 VM마다 맞는지 미리 본다.
  - 요청은 동시에 4개씩 보내고 Task마다 `trackTask`를 건다. 결과는 알림 하나("8개 중 7개 성공, 1개 실패 – 보기")로 묶는다.
- **미리 보기 서랍(peek)**: VM·노드 목록에서 행의 눈 아이콘이나 `Space`로 오른쪽 Sheet를 연다. 요약 카드, 주 동작, "열기"가 있다. `Enter`는 상세로 간다. 서랍이 열린 채 `j/k`로 행을 옮기면 내용이 바뀐다.
- **상세 틀 `ResourceHeader`**는 이동 경로 아래에 이름, 상태 칩, 즐겨찾기 ★, 주 동작 2~3개, 케밥(나머지)을 둔다. 그 아래는 요약 카드 줄, 그 아래는 `?tab=` 탭이다(지금 VM 상세 규약). VM·노드(계획 60)·보안 그룹·가져오기·원격에 쓴다.
- **`VmActionHost`**는 `VmMenu`의 대화상자 상태를 맥락(context)으로 올린다. 행 메뉴, 상세 머리, 팔레트, 서랍, 일괄 막대가 같은 대화상자를 연다.

```
 가상 머신 › VM › web
 web  ● 실행 중   ★        [▶ 콘솔] [⏻ 정지 ▾] [⇄ 옮기기] [⋯]
 ┌ node1 ─────┐┌ vCPU 4 ─────┐┌ 메모리 8 GiB ┐┌ IP 10.0.0.21 ┐
 └────────────┘└─────────────┘└──────────────┘└──────────────┘
 요약 | 하드웨어 | 게스트 | 지표(계획 60) | 콘솔 | 스냅샷 | 백업 | 작업 | JSON
```

### 5.4 작업·알림 센터

머리줄 종의 숫자는 "도는 Task + 울리는 경보(critical·warning)"다. 누르거나 `Shift+N`으로 오른쪽 Sheet를 연다.

```
┌ 알림 센터 ──────────────────────── ✕ ┐
│ [진행 중 3] [최근] [경보 2]           │
│ ⇄ web 옮기기   node1→node2            │
│   ███████░░░ 3.1/4 GiB  메모리 복사   │
│ ⬇ ubuntu.iso 내려받기  ████░░ 41%     │
│ ✕ db 시작 실패 · 2분 전  [다시 하기] [VM 열기] │
│ ▲ critical  node3 CPU > 90% 5분       │
│ 모든 작업 보기 →   모든 경보 보기 →   │
└──────────────────────────────────────┘
```

- 데이터는 `useList("task")`(이미 실시간)와 계획 60의 `useAlerts()`(30초)다. 새 요청이 없다.
- **다시 하기**는 Task에 파라미터가 없으므로 파라미터가 필요 없는 op(`vm-start`·`vm-stop`·`vm-force-stop`·`vm-reboot`, 백업 작업 지금 실행)만 같은 API로 다시 보낸다. 나머지는 "열기"로 대상 화면과 알맞은 대화상자를 연다(`taskTarget(task)` → 경로 + `?tab=`).
- **깊은 링크**는 Task 대상과 경보 subject `{kind, id}`를 `nav.ts`의 상세 경로로 바꾼다.
- `TaskToasts`는 그대로 두되, 서랍이 열려 있으면 성공 알림을 띄우지 않는다(중복). 실패 알림에는 "보기"를 단다.

### 5.5 실시간·낙관적 갱신·스켈레톤

- `/watch`가 이미 행을 바꾸므로, 바뀐 행의 상태 칩에 짧은 강조(`data-changed`, 600ms, 움직임 줄이기면 끔)를 준다.
- 낙관적 갱신은 서버 Task가 없는 쓰기에만 한다. 대상은 즐겨찾기, 저장 보기, 라벨·설명 고치기(`If-Match` revision, 실패하면 되돌리고 오류), 경보 규칙 켜기다. 전원·옮기기는 상태를 꾸며 내지 않는다. 202를 받으면 `TaskIndex`의 "요청됨" 칩을 보인다(지금과 같음).
- 첫 로딩은 `Skeleton`(표 행·카드·차트 모양)이고, 다시 읽는 중에는 내용을 그대로 둔다. `LiveBadge`가 `offline`이면 본문 위에 "실시간 연결이 끊겼습니다 – 다시 연결 중" 띠를 둔다.

### 5.6 빈 상태와 마법사

- **첫 걸음**: 풀이 없으면 홈과 VM 목록에 "1 스토리지 풀 만들기 → 2 ISO 올리기·이미지 받기 → 3 VM 만들기" 카드를 둔다. 끝난 단계는 ✓이고, 각 단계는 그 화면의 `?create=1`로 간다. 단계는 `useList` 데이터로 판정한다(저장하지 않음). viewer에게는 보이지 않는다.
- **목록 빈 상태 `EmptyState`**: 아이콘, 한 줄 설명, 주 단추, 문서 링크(`docs/guide`의 해당 절)다.
- **VM 만들기 프리셋**: 지금 폼(`VmFormPage`의 섹션들) 위에 첫 단계 "프리셋 고르기"(Linux 서버·Windows 11(계획 59 미리 채우기)·템플릿에서·빈 VM)를 둔다. 프리셋은 폼 값을 채울 뿐이고 같은 `submit.ts`를 쓴다. 섹션은 접히는 단계 목록(왼쪽 단계 표시, 오류 있는 단계 표시)이 되고 마지막에 요약 검토가 있다.
- **클러스터 합류 마법사**(`/cluster`): 토큰 만들기 → `joinCommand` 복사 → 새 노드가 Node 목록에 나타나기 기다리기(watch) → 완료다. **Ceph**은 지금 `CephWizard`를 빈 상태의 주 단추로 연다.

### 5.7 단축키·최근·즐겨찾기

| 키 | 동작 | 키 | 동작 |
|---|---|---|---|
| `⌘K`·`Ctrl+K` | 팔레트 | `?` | 단축키 도움말 |
| `/` | 목록 검색 상자 | `[` | 레일 접기·펴기 |
| `g h`·`g v`·`g i`·`g p`·`g a`·`g s` | 영역으로 | `c` | + 만들기 메뉴 |
| `j`·`k` | 행 옮기기 | `x` | 행 고르기 |
| `Space` | 미리 보기 | `Enter` | 상세 열기 |
| `Shift+N` | 알림 센터 | `f` | 상세에서 즐겨찾기 |

- 입력 칸·`contenteditable`·열린 대화상자(팔레트 제외), 콘솔 화면에서는 받지 않는다. 전원·지우기에는 한 글자 키를 두지 않는다.
- `lib/shortcuts.ts`는 의존성 없는 순서 키 해석기(`g` 다음 1초)다. 도움말 표도 같은 표로 만든다.
- **최근 항목**(20개)과 **즐겨찾기**(VM·노드·풀)는 `prefs` `{user}.recent`·`{user}.pins`다. 사용자마다 따로 두고, 지운 리소스는 읽을 때 뺀다. 즐겨찾기는 홈 카드와 팔레트 맨 위에 보인다.

### 5.8 파괴 동작의 안전

- 되돌릴 수 없는 동작(VM·풀·네트워크·백업 지우기, 강제 정지, 노드 펜스, TPM 초기화)은 `ConfirmDialog`에 **영향 목록**(`impact` prop)을 단다. 예를 들어 지울 디스크·스냅샷·백업 수, 이 네트워크를 쓰는 VM, 노드에서 도는 VM이다. 데이터는 이미 받은 목록에서 셈한다. 하나는 이름, 여럿은 수를 입력해야 한다.
- 위험 단추는 오른쪽 끝이고 `destructive` 톤이다. 기본 포커스는 취소 단추다.
- **되돌리기 알림**(10초): 즐겨찾기 빼기, 저장 보기 지우기, 라벨·설명 고치기(이전 값을 `If-Match`로 다시 씀)에 둔다. VM 정지 뒤 알림에는 "다시 켜기"를 둔다. 서버 Task가 지운 것은 되돌린다고 하지 않는다.

### 5.9 반응형과 휴대폰

- **1280px 이상**: 펼친 사이드바(224px, 접기 기억 `nav.rail`).
- **768~1279px(태블릿)**: 레일이 기본이다. 표는 덜 중요한 열을 숨긴다(열 정의의 `priority`).
- **768px 미만**: 사이드바를 숨기고 머리줄 ☰가 영역 6개 Sheet를 연다. 영역 탭은 가로로 스크롤한다.
- **당직 보기**: VM·노드·경보 목록은 카드 목록(이름, 상태 칩, 노드, 전원 단추)이 된다. 홈은 경보 → 상태 요약 → 즐겨찾기 순으로 쌓는다. 콘솔·폼은 데스크톱 전용 안내를 둔다. 터치 대상은 최소 40px이다.

```
┌───────────────────────────┐
│ ☰ 홈           🔍  🔔2  👤 │
├───────────────────────────┤
│ ▲ critical node3 CPU>90%  │
│ ▲ warning  pool local 87% │
├───────────────────────────┤
│ 노드 3/3  VM 41▶ 2✕  풀 ok │
├───────────────────────────┤
│ ★ web   ● 실행 중   [⏻]   │
│ ★ db    ✕ 실패     [▶]   │
└───────────────────────────┘
```

## 6. 시각 디자인

- **방향**: Linear·Vercel처럼 중립 배경, 얇은 테두리, 절제된 색 하나(브랜드 강조), 상태색은 의미에만 쓴다. Grafana 11·Prism Central처럼 숫자를 크게 보이고 tabular-nums를 쓴다. 기본 단추는 지금처럼 중립(검정·흰색)이다. 강조색은 포커스 링, 활성 내비 표시, 선택 행, 링크, `--chart-1`에 쓴다.
- **토큰**(`index.css`, `@theme inline`에 이어 붙임):

| 묶음 | 토큰 | 밝음 / 어두움 |
|---|---|---|
| 강조 | `--brand`, `--brand-foreground`, `--ring` | `oklch(0.52 0.19 264)` / `oklch(0.68 0.16 264)` |
| 상태 | `--success`, `--warning`, `--danger`(=`--destructive`), `--info`, 각 `-soft`(칩 배경)·`-fg`(칩 글) | 초록 145°·주황 70°·빨강 27°·파랑 240°. `-fg`는 `-soft`와 배경 위에서 4.5:1 이상이다 |
| 차트 | `--chart-1..5` | 264°·170°·70°·330°·27°, 밝음 L 0.55~0.65, 어두움 L 0.70~0.78(지금 회색을 바꿈) |
| 표면 | `--surface-1..3`(카드·떠 있는 층), `--shadow-sm/md/lg` | 어두움은 그림자 대신 더 밝은 표면 + 테두리 |
| 모서리 | `--radius` 0.5rem(지금 0.625) | 칩·입력 `radius-md`, 카드 `radius-lg`, Sheet·팔레트 `radius-xl` |
| 간격·밀도 | `--row-h` 40/32px, `--control-h` 36/32px, `--gap` 16/12px | `html[data-density=compact]`로 바꿈 |
| 글자 | 12·13·14·16·20·24px, 제목 600, 본문 400 | UI 기본 14px, 페이지 제목 20px(지금 24px), 숫자 `tabular-nums`, ID·IP `font-mono`(`ui-monospace`) |
| 움직임 | `--ease-out`, 120·200ms | `@media (prefers-reduced-motion: reduce)`에서 0ms, 깜박이는 강조 끔 |

- **테마**: 지금 `lib/theme.tsx`(system·light·dark)를 그대로 쓴다. 밀도도 같은 틀로 `lib/density.tsx`(`prefs` `density`, 기본 comfortable, 768px 미만은 comfortable 고정)를 둔다.
- **아이콘**: lucide를 그대로 쓴다. 크기는 16px(표·메뉴)·20px(레일)이고 stroke 1.75이다. 영역 아이콘은 하나로 고정한다(House·Server·Network·ShieldCheck·Activity·Settings). 같은 동작에는 어디서나 같은 아이콘을 쓴다(`components/icons.ts` 표).
- **작은 움직임**: Sheet·팔레트는 미끄러지며 흐려지고(`tw-animate-css`), 행 hover는 배경만 바뀐다. 진행 막대는 이어지는 값으로 그린다.

## 7. 접근성과 i18n

- 사이드바는 `<nav aria-label>`이고 현재 영역은 `aria-current="page"`다. 영역 탭은 링크 목록 `<nav aria-label="{영역} 메뉴">`이다(경로가 바뀌므로 `role=tablist`가 아니다). 상세 `?tab=`은 지금처럼 radix Tabs다.
- 모든 대화상자·Sheet·팔레트는 radix 포커스 가두기를 쓰고, 닫으면 연 단추로 돌아간다(지금 `focusBack`·`returnFocus` 규약). 경로가 바뀌면 `<main>`의 `h1`로 포커스를 옮기고 `aria-live` 알림으로 화면 이름을 읽어 준다. "본문으로 건너뛰기" 링크를 둔다.
- 표의 체크박스에는 `aria-label`("web 고르기")을 둔다. 일괄 막대는 `role="region"`이고 고른 수를 `aria-live`로 알린다. 정렬 머리에는 `aria-sort`를 둔다. 차트는 계획 60의 요약·"표로 보기"를 쓴다.
- 새 글은 모두 `ko.json`·`en.json`에 둔다(`nav.area.*`, `palette.*`, `shortcuts.*`, `center.*`, `empty.*`, `wizard.*`, `table.*`). 지금 `i18n.test.ts`(두 파일 키 같음)가 빠진 키를 잡는다. 팔레트 동사는 두 언어 별칭을 모두 받는다.

## 8. 계획 60과 맞추기

### 8.1 흡수

- **홈**: 지금 `DashboardPage`의 통계 카드와 계획 60의 위젯(클러스터 차트·노드 줄·상위 VM·풀 사용률·활성 경보)을 한 격자로 다시 놓는다. 맨 위에 건강 띠(노드 온라인, 실패 VM, 울리는 경보, 혼합 버전·구독 경고)를 두고, 이어서 경보 · 용량(풀·클러스터 CPU/메모리) · 차트 · 상위 VM · 최근 작업 · 즐겨찾기 순이다. 계획 60 웹이 먼저 들어오면 S7이 위젯을 옮겨 놓기만 한다. 나중에 들어오면 계획 60이 §8.2대로 위젯만 만든다.
- **노드 상세** `/nodes/:name` → 인프라 > 노드(S5의 `ResourceHeader`로 머리를 바꿈). **VM 지표 탭** → VM 상세 `?tab=metrics`.
- **경보 화면** `/alerts` → 활동 > 알림(`?tab=active|rules|history`). 종 서랍과 홈 위젯이 같은 `useAlerts()`를 쓴다.
- **알림 대상** → 설정 > 알림 대상 `/settings/notifications`(admin). **Prometheus** → 설정 > 일반의 칸.

### 8.2 계획 60 웹 단계 지침

계획 60 웹 단계(S7) 구현자에게 그대로 넘긴다. 목표는 계획 61이 고쳐 쓸 것을 줄이는 것이다.

1. **사이드바에 항목을 더하지 않는다.** 계획 60 화면은 모두 영역 안 탭이나 상세 탭이다.
   - 계획 61 S2가 아직 들어오지 않았으면 `AppLayout`의 `NAV`를 건드리지 않는다. `/alerts`에는 홈 경보 위젯의 "모두 보기"와 작업 화면 위 링크로 간다.
   - S2가 들어왔으면 `layout/nav.ts`의 활동 영역 `tabs`에 `{to: "/alerts", key: "nav.alerts"}`를, 설정 영역에 `{to: "/settings/notifications", key: "nav.notificationTargets", admin: true}`를 더한다.
2. **경로**:
   - 경보는 `/alerts?tab=active|rules|history`다. 알림 대상은 경보 화면의 탭이 아니라 처음부터 `/settings/notifications`에 둔다(계획 60 문서 §12의 "알림 대상 탭"을 이 경로로 바꾼다).
   - 노드 상세는 `/nodes/:name`, VM 지표는 VM 상세 `TABS`의 `metrics`(summary 다음)다.
   - Prometheus는 `settings/PrometheusSection.tsx`(`TelemetrySection` 모양)로 `/settings`에 넣는다.
3. **컴포넌트 경계**:
   - `web/src/charts/`: `TimeChart`(데이터·시리즈·높이만 받음), `RangePicker`(radix ToggleGroup, 값 `hour|day|week|month|year`), `useTimeRange()`(`?range=` 쿼리), `chartColors.ts`(토큰 읽기).
   - `web/src/alerts/`: `alerts.ts`의 `useAlerts()`(쿼리 키 `["alerts"]`, 30초. 다른 곳에서 따로 폴링하지 않음), `severity.ts`의 `severityTone(sev): "danger"|"warning"|"info"`, 보여 주기만 하는 `AlertList({alerts, compact})`(서랍·홈 위젯이 다시 씀), `NotificationTargetsPage.tsx`.
   - `web/src/dashboard/`: 위젯마다 파일 하나(`ClusterChartsWidget`·`NodeStripWidget`·`TopVmsWidget`·`PoolUsageWidget`·`ActiveAlertsWidget`)를 둔다. 위젯은 자기 `Card` 하나를 그리고 바깥 격자·여백을 정하지 않는다. 배치는 `DashboardPage`만 한다.
   - `nodes/NodeDetailPage.tsx`는 머리(`NodeHeader`: 이름·상태·동작)와 본문을 다른 컴포넌트로 둔다. 동작은 지금 `NodesPage`의 대화상자를 다시 쓴다.
4. **토큰**: 색은 CSS 변수로만 쓴다(hex·Tailwind 색 이름 금지).
   - 차트는 `--chart-1..5`를 쓴다. 상태와 임계는 `--success`·`--warning`·`--danger`·`--info`다(풀 85% 경고는 `--warning`).
   - 계획 61 S1보다 먼저 들어오면 §6 표의 값으로 이 변수들을 `index.css`의 `:root`·`.dark`에 정의한다. S1은 같은 이름이면 그대로 둔다.
   - `chartColors.ts`는 `getComputedStyle(document.documentElement)`로 읽고, `useTheme()`가 바뀌면 다시 읽어 uPlot 시리즈 색을 바꾼다.
5. **모양**:
   - 페이지 위는 `<h1>`과 오른쪽 동작 한 줄이다. 그 위에 자기 탭 줄이나 이동 경로를 만들지 않는다(S2의 영역 탭·이동 경로가 맡음).
   - 로딩은 차트 높이만 한 `animate-pulse bg-muted` 상자다(S1 뒤면 `Skeleton`). 차트 폭은 화면 폭이 아니라 컨테이너 폭(`ResizeObserver`)을 따른다. 그래야 레일 접기·서랍에서 다시 그려진다.
6. **접근성·i18n**: 범위 고르기는 키보드로 움직인다. 차트의 `aria-label` 요약과 "표로 보기"를 둔다. 키는 `metrics.*`·`alerts.*`·`nav.alerts`·`nav.notificationTargets`이고 ko/en 모두 둔다.
7. **알림 중복 금지**: 경보를 sonner 알림으로 띄우지 않는다. 종 서랍(S6)이 맡는다.
8. **시험**: 역할·이름(`getByRole`)으로 찾는다. 클래스·DOM 깊이에 기대지 않는다(S1·S2의 모양 바꾸기에 깨지지 않게). `TimeChart`는 시험에서 가짜다.

## 9. 다른 기능과의 관계

- **권한**: 메뉴·탭·팔레트 항목·일괄 동작은 모두 `can()`으로 거른다. 서버가 실제로 막는다(지금과 같음). operator는 VM 일괄 동작을 쓰고 노드 펜스는 보지 못한다. viewer는 팔레트에서 이동과 열기만 한다.
- **실시간·Task**: `LiveProvider`·`TaskIndexProvider`·`trackTask`를 그대로 쓴다. 서랍과 팔레트는 같은 캐시만 읽는다. 일괄 동작은 Task 수만큼 `trackTask`를 걸되, 알림은 묶어서 하나다.
- **계획 57~59 화면**: ISO 내려받기·cloud-init·템플릿 배포·vTPM·게스트 탭은 자리만 옮긴다. Windows 11 미리 채우기는 VM 만들기 프리셋 하나가 된다.
- **원격 클러스터**: `RemotesPage`를 보기(보호)와 관리(설정)로 나눈다. 원격 VM 보기 `/remotes/:name`은 그대로다.
- **문서**: `docs/guide`의 "왼쪽 메뉴의 …" 세 곳과 화면 이름을 S8에서 새 영역·탭 이름으로 고친다. 가이드에 "웹 화면 둘러보기·단축키" 절을 더한다.
- **opld**: 바뀌는 것이 없다(`web.rs` fallback). 묶음 크기는 S1 전과 S7 뒤의 `vite build` gzip 크기를 실행 기록에 남긴다. 목표는 늘어난 양 25 KB 이하이다. 설정·Ceph·감사 화면은 경로 `lazy`로 뗀다.

## 10. 시험과 CI

- **vitest + Testing Library(어디서나, `pnpm test`)**:
  - `nav.test.ts`: `AREAS` 탭 경로가 모두 라우트에 있다. `areaOf`가 상세 경로를 맞는 탭에 붙인다. 권한별(admin·operator·viewer)로 보이는 영역·탭이 맞다. 사이드바 항목이 6개 이하이고 둘째 단계가 없다.
  - **넘기기 `routes.test.tsx`**: `App.tsx`의 경로 배열을 `appRoutes`로 내보낸다. 표로 시험한다.
    1. `LEGACY_REDIRECTS`의 모든 옛 주소를 `?x=1#h`를 붙여 `createMemoryRouter`로 열면, `router.state.location`이 새 경로이고 쿼리·해시가 같다.
    2. 지금 `NAV` 26개 경로를 굳힌 목록 `OLD_NAV_PATHS`의 모든 주소가 NotFound가 아닌 화면 제목을 보인다.
    3. `/infra`·`/protect`·`/activity`는 기억한 탭으로, 기억이 없거나 권한 밖이면 첫 탭으로 간다.
  - 영역 탭 기억: 탭을 옮긴 뒤 사이드바를 누르면 그 탭으로 간다. `prefs`가 던져도 동작한다.
  - 팔레트: `rank.ts` 순위 표, 동작 문법 해석 표(한·영 별칭, `to`), IP·CIDR 찾기를 시험한다. 렌더 시험은 ⌘K·Ctrl+K로 열기, ↑↓·Enter, "migrate web to node2"가 옮기기 대화상자를 node2로 채워 여는지, viewer에게 동작이 없는지, Esc 뒤 포커스가 돌아오는지 본다.
  - 일괄 동작: 고른 VM 셋 중 불가 하나를 보인다. 지우기는 수를 입력해야 열린다. msw로 4개 동시 상한과 묶은 결과 알림을 본다.
  - 단축키: `g v` 이동, 입력 칸 안에서 무시, `?` 도움말, 순서 시간 초과.
  - 서랍: 진행 중·실패 Task, 다시 하기가 같은 전원 API를 부르는지, 파라미터가 필요한 op에는 다시 하기가 없고 "열기"가 맞는 경로인지 본다.
  - 토큰 대비 `tokens.test.ts`: `index.css`의 `:root`·`.dark` oklch 값을 풀어 sRGB 대비를 셈한다(순수 함수). 글/배경 4.5:1, 상태 `-fg`/`-soft` 4.5:1, 아이콘·테두리·차트 선/배경 3:1을 본다.
- **접근성**: `test/axe.ts`의 `expectNoAxeViolations(container)`가 `axe-core`를 직접 부른다(jsdom에서 안 되는 `color-contrast` 규칙은 끄고 위 토큰 시험이 대신한다). `vitest-axe`는 오래 고치지 않았으므로 쓰지 않는다. 셸·팔레트·서랍·VM 목록·VM 상세·설정·홈에서 돈다.
- **e2e**(`web/e2e/vm.spec.ts`, GitHub `linux-kvm`): S2에서 "Storage pools"·"Volumes"를 "Infrastructure"를 누른 뒤의 영역 탭 링크로, 제목 "Dashboard"를 "Home"으로 고친다. S4에서 VM 일괄 정지 한 단계를, S3에서 ⌘K로 VM 열기 한 단계를 더한다. svc01은 웹 묶음을 돌지 않으므로 S2·S4·S7 뒤 GitHub 탐침으로 확인한다.
- **화면 회귀**는 선택이고 이 계획에서는 넣지 않는다(§12).

## 11. 견준 길

- **사이드바 트리·펼침 그룹(Proxmox·vSphere) vs 영역 6개 + 영역 안 탭(채택).** 사용자가 바란 "좌측은 최대한 간단히"를 그대로 지킨다. 깊은 곳은 팔레트와 즐겨찾기로 간다. 대가로 탭 하나 더 누르지만, 마지막 탭 기억이 대부분 이를 지운다.
- **영역 접두 주소(`/infra/storage/pools`) vs 리소스 주소 유지 + 메타데이터로 묶기(채택).** 북마크·문서·e2e가 그대로이고 넘기기는 셋뿐이다. 이동 경로·활성 표시는 `nav.ts`가 만든다. 설정만 `/settings/*`로 모은다(설정은 사이드바에서 떨어진 자리이고 화면 셋이 계정 성격이다).
- **`cmdk` vs 직접 만든 팔레트(채택).** 순위·IP·동작 문법을 어차피 직접 짜야 하므로 `cmdk`의 거르기는 끄게 된다(`shouldFilter=false`). 남는 것은 목록 키보드 이동뿐이다. `cmdk`는 낱개 `@radix-ui/*` 패키지를 따로 끌어와 `radix-ui` 묶음과 겹칠 수 있다. 직접 만든 combobox는 200줄 남짓이고 시험으로 덮는다.
- **팔레트에서 바로 실행 vs 같은 확인 대화상자(채택).** 손가락 실수가 정지·옮기기가 되지 않는다. 확인이 없는 동작만 바로 한다.
- **서버 다시 하기 API vs 클라이언트 다시 보내기(채택, 파라미터 없는 op만).** Task에 요청 파라미터가 없어서 서버 API를 만들려면 타입 변경이 필요하다. 나머지는 열기로 간다.
- **즐겨찾기·저장 보기를 서버에 vs 브라우저(채택).** 새 종류·API·권한 없이 바로 쓴다. 여러 브라우저 사이 동기화는 §12다.
- **휴대폰 따로 앱 vs 같은 앱의 반응형(채택).** 당직에 필요한 셋(홈·경보·전원)만 다듬는다.
- **`vitest-axe` vs `axe-core` 직접(채택).** 의존성 하나, 감싸는 코드 10줄이다.

## 12. 나중 할 일

서버에 두는 사용자 설정(즐겨찾기·저장 보기·마지막 탭 동기화), 서버 전문 검색(모든 VM의 게스트 IP 색인), Task 파라미터 기록과 서버 다시 하기, 사용자 정의 대시보드(끌어 놓기), 표 CSV 내보내기·셀 편집, Playwright 화면 회귀 스냅숏, 보호 영역 개요(RPO·마지막 백업 지도), 여러 클러스터를 한 표에 모은 보기(원격 VM 일괄 동작), RTL, 고대비 테마.

## 13. 스테이지 나누기

각 스테이지는 커밋 하나다. 혼자 `pnpm typecheck`·`pnpm lint`·`pnpm test`를 지나고, 끝난 뒤에도 앱 전체를 쓸 수 있다. 계획 60 웹 단계와 겹치면 늦게 들어오는 쪽이 §8.2에 맞춘다.

1. **feat(web): 디자인 토큰·밀도·움직임 줄이기와 바탕 컴포넌트**
   - `index.css` 토큰(§6), `lib/density.tsx`, 사용자 메뉴의 밀도 항목.
   - `ui/sheet`·`tooltip`·`popover`·`toggle-group`·`skeleton`·`kbd`(radix 묶음), `StatusChip`·`EmptyState`·`components/icons.ts`, `VmPhaseBadge`를 `StatusChip`으로.
   - `tokens.test.ts`, `axe-core` 4.10.3와 `test/axe.ts`. 화면 구조는 그대로다.
2. **feat(web): 영역 6개 IA — 아이콘 레일 사이드바, 영역 탭·보기 전환, 마지막 탭 기억, 이동 경로, 설정 묶기와 옛 주소 넘기기**
   - `layout/nav.ts`(`AREAS`·`LEGACY_REDIRECTS`), `Sidebar`·`AreaTabs`·`ViewSwitch`·`Breadcrumbs`·`PageHeader`, `appRoutes` 내보내기.
   - `/settings/*`(사용자·영역·토큰·원격 관리·구독)와 `RemotesPage` 나누기, 언어·테마를 사용자 메뉴로, 768px 미만 ☰ Sheet, 경로 바뀔 때 포커스.
   - `nav.test.ts`·`routes.test.tsx`, e2e 링크·제목 고치기.
3. **feat(web): ⌘K 명령 팔레트·머리줄 검색·+ 만들기·단축키와 도움말·최근 항목·즐겨찾기**
   - `palette/`(`rank.ts`·`parse.ts`·`CommandPalette.tsx`), `VmActionHost`(`VmMenu` 대화상자를 맥락으로), `?create=1` 열기 규약, 클러스터 전환.
   - `lib/shortcuts.ts`와 도움말, `recent`·`pins`. 시험과 e2e ⌘K 단계.
4. **feat(web): 공통 데이터 표와 VM 일괄 동작·미리 보기 서랍**
   - `components/data-table/`(URL 상태·저장 보기·열·선택·스켈레톤·`priority` 열), VM 목록 옮기기와 일괄 재시작·스냅샷·옮기기·지우기, VM·노드 미리 보기 서랍.
   - 노드·볼륨·풀·백업·작업·감사 목록을 같은 표로. 휴대폰 카드 목록. 시험과 e2e 일괄 정지.
5. **feat(web): 상세 틀과 파괴 동작 안전**
   - `ResourceHeader`·요약 카드를 VM·보안 그룹·가져오기·원격 상세에(계획 60 노드 상세가 있으면 거기도).
   - `ConfirmDialog`의 `impact`와 수 입력, 되돌리기 알림(즐겨찾기·저장 보기·라벨·설명), 낙관적 갱신, 바뀐 행 강조, 오프라인 띠.
6. **feat(web): 작업·알림 센터와 활동 영역**
   - 종·`NotificationCenter` Sheet(진행 중·최근·경보), `taskTarget`·다시 하기, `TaskToasts` 중복 줄이기.
   - 활동 영역 탭(작업·알림·감사). `useAlerts()`가 없으면(계획 60 전) 경보 탭을 숨긴다.
7. **feat(web): 홈 대시보드·첫 걸음·마법사·당직 보기**
   - 홈 격자(건강 띠·경보·용량·차트 자리·상위 VM·최근 작업·즐겨찾기)에 계획 60 위젯을 끼운다. 계획 60 전이면 지금 카드만 둔다.
   - 첫 걸음 카드, VM 만들기 프리셋·단계 폼, 클러스터 합류 마법사, Ceph 빈 상태. 휴대폰 홈 순서, `lazy` 경로, 묶음 크기 기록.
8. **docs: 가이드 "웹 화면 둘러보기·단축키"와 메뉴 이름 고치기, README 한계(팔레트 IP 검색), 설계 문서 스테이지 보고, 계획 61 실행 기록**.

## 14. 스테이지 보고 (설계와 다른 것·결정)

- **S1 토큰 값**: §6 값 그대로 두면 대비 시험에 걸리는 것 둘을 고쳤다. 밝음 `--warning`을 `oklch(0.7 0.16 70)`에서 `oklch(0.63 0.15 70)`으로(흰 바탕·`--warning-soft` 위 3:1), 밝음 `--muted-foreground`를 0.556에서 0.54로(`--muted` 위 4.5:1) 낮췄다.
- **S1 그림자 이름**: Tailwind의 `--shadow-*` 테마 이름과 겹치지 않게 값은 `--elevation-sm/md/lg`(`:root`·`.dark`)에 두고 `@theme inline`의 `--shadow-sm/md/lg`가 그것을 가리킨다. 그래서 `shadow-sm` 같은 유틸리티가 테마를 따른다. 어두움은 sm·md 그림자가 없고 lg만 짙게 남는다.
- **S1 표면**: `--surface-1`(바탕 위 판)·`--surface-2`(카드)·`--surface-3`(떠 있는 층)을 더했다. 지금 `--card`·`--popover`는 그대로 두고 S2부터 새 화면이 표면 토큰을 쓴다.
- **S1 대비 시험 범위**: §10의 "테두리 3:1"은 포커스 링(`--ring` = `--brand`)과 강조색에만 둔다. 장식 테두리 `--border`·`--input`(shadcn 0.922)은 3:1로 올리면 화면이 무거워지므로 빼고, 입력 칸은 레이블과 포커스 링으로 알아본다.
- **S1 밀도 적용**: `--control-h`는 `Button` 기본·아이콘 크기, `Input`, `NativeSelect` 높이에, `--row-h`는 `TableRow`·`TableHead` 높이에 걸었다(셀 위아래 여백은 `p-2` → `px-2 py-1`). `--gap`은 정의만 하고 S2의 페이지 틀이 쓴다. 기본 단추 높이가 32px에서 36px(편안함)이 된다.
- **S1 글자**: 본문 기본을 14px(`body` `text-sm`)로 했다. 페이지 제목 20px는 S2의 `PageHeader`가 맡는다(지금 화면마다 `h1` 클래스가 있어 S1에서 고치면 S2에서 다시 고친다).
- **S1 색 이름 정리**: `amber-*`·`emerald-*` 직접 색(경고 글·띠, `LiveBadge` 점)을 `warning`·`success`·`danger` 토큰으로 바꿨다(§8.2 4 "색은 CSS 변수로만").
- **S1 움직임 줄이기**: `--duration-fast/normal`을 0ms로 하고, 모든 요소의 `animation-duration`·`transition-duration`을 0.01ms·한 번으로 줄인다(Sheet 미끄러짐, 스켈레톤 맥박, `StatusChip`의 `busy` 점, 이후 `data-changed` 강조까지 한 곳에서 끔).
- **S2 탭 이름**: 영역 이름과 탭 이름이 겹치지 않게 새 키를 두었다. VM 탭 `nav.vmList`("VM"/"VMs"), 이미지·ISO 탭 `nav.imagesIsos`(보기는 이미지 라이브러리·ISO 이미지), 배치 탭 `nav.placementTab`(보기는 배치 규칙·최적 배치), 원격 복제 탭 `nav.replication`, 설정 일반 `nav.general`, 구독 `nav.subscription`. 영역 이름은 `nav.area.*`이다. 홈 제목은 "홈"("Home")이고 쓰지 않게 된 `nav.dashboard`를 지웠다.
- **S2 본문 위 영역 이름**: §4.4 그림의 탭 위 "인프라" 글은 두지 않았다. 이동 경로 첫 칸이 영역이고, 제목(h1)이 둘로 보이지 않게 했다. 보이는 탭이 하나뿐인 영역(홈)은 탭 줄을 그리지 않는다. 보기 전환은 목록 화면에서만 보이고 상세(`/nodes/:name`)에서는 숨긴다.
- **S2 머리줄**: 언어·테마는 사용자 메뉴의 하위 메뉴(라디오)로 옮겼고 밀도는 S1처럼 메뉴 안 라디오 묶음이다. 클러스터 이름은 S3의 클러스터 전환이 들어올 때까지 머리줄 오른쪽(1024px 이상)에 둔다. 레일 접기 단추는 사이드바 아래에 있고, `[` 단축키는 S3의 `lib/shortcuts.ts`가 단다. 레일은 `prefs` `nav.rail`("1"/"0")을 기억하고, 고른 적이 없으면 1280px 이상에서 펼친다.
- **S2 PageHeader**: `components/PageHeader.tsx`(h1 20px·동작·설명)는 새로 만들거나 고친 화면(설정 일반·구독, 원격 복제, 원격 클러스터 관리)에 썼다. 나머지 화면은 `h1`의 글 크기만 `text-xl`로 바꿨고, S4·S5가 목록·상세를 고칠 때 `PageHeader`로 옮긴다.
- **S2 원격 나누기**: 보호 › 원격 복제(`RemotesPage`)는 상태 카드(이름은 원격 VM 보기 링크), admin의 새로고침(상태를 다시 모으는 동작이라 보기 쪽에 남김), "원격 클러스터 관리" 링크, 원격 대상이 있는 백업 작업 표(이름은 `/backup-jobs` 링크)다. 설정 › 원격 클러스터(`RemotesManagePage`, admin 탭)는 표와 추가·고치기·지우기 대화상자다. 목록 쿼리는 `remotes.ts`의 `useRemotes()`로 함께 쓴다.
- **S2 계획 60 자리**: 설정 일반의 "알림 대상 관리" 칸(`settings.notificationsIntro`·`manageNotifications` 키)과 작업 화면 위 "경보" 링크는 탭이 생겨 지웠다. 경보 규칙 탭의 알림 대상 링크와 홈 경보 위젯의 "모든 경보 보기"는 맥락 링크라 남겼다. `/alerts?tab=targets`는 계획 60이 만들지 않았으므로 넘기기를 두지 않았다. 구독 배너 링크는 `/settings/subscription`으로 바꿨다.
- **S2 넘기기**: `Redirect`는 `<Navigate to={{pathname, search, hash}} replace>`로 쿼리·해시를 지킨다(글로 준 `to`만 쿼리를 버린다). `/infra`·`/protect`·`/activity`는 `AreaRedirect`가 기억한 탭(권한 밖·다른 영역·상세 경로면 보이는 첫 탭)으로 간다. 경로 표는 `App.tsx`의 `appRoutes`다.
- **S2 포커스**: 경로(pathname)가 바뀔 때만 본문 `h1`(없으면 `<main>`)로 포커스를 옮기고 `aria-live` 영역에 이동 경로를 읽어 준다. VM 상세 `?tab=`처럼 쿼리만 바뀌면 옮기지 않는다.
- **S3 팔레트 IP**: Network 리소스에는 CIDR 칸이 없다(브리지·VLAN·VXLAN뿐). 그래서 CIDR 질의(`10.0.0.0/24`)는 "네트워크 CIDR 포함"이 아니라 그 범위 안의 VM 주소(cloud-init 고정 IP·이미 연 게스트 정보 IP)를 찾는다. IP 질의는 접두사부터 맞히고(부분 문자열 `0.0.21`은 받지 않는다), 노드는 API 주소의 호스트로 찾는다. 여러 낱말 질의(`create pool`)는 통째로 맞지 않으면 낱말마다 맞아야 하고 점수는 많아야 단어 접두사다.
- **S3 팔레트 확인**: 메뉴·상세의 정지·재시작은 지금처럼 바로 하지만, 팔레트는 §5.2대로 시작·콘솔·열기만 바로 한다. 그래서 `VmConfirm`에 정지·재시작 확인(`ConfirmDialog`)과 스냅샷 만들기(`VmSnapshotDialog` — 탭의 `CreateSnapshotDialog`를 이름·블록 방식만 셈해 감쌈)를 더했다. 옮기기는 `VmMigrateDialog`의 `initialNode`로 노드를 미리 고른다. 노드를 못 찾거나 옮길 수 있는 노드가 아니면 그 동작은 이유와 함께 흐리다.
- **S3 ⇥·⌘↵**: ⇥은 고른 VM의 이름을 입력에 넣어 그 VM의 동작 묶음을 보인다(VM 이름을 그대로 치면 동작이 앞에 온다). ⌘↵(Ctrl+↵)는 경로가 있는 항목을 새 탭으로 연다. 빈 질의는 즐겨찾기·최근(즐겨찾기 뺌)·이동·명령만 보이고 리소스는 치면 보인다.
- **S3 `VmActionHost`**: 맥락은 `vms/actionHost.ts`(대소문자만 다른 `VmActionHost.tsx`와 이름이 겹치면 macOS에서 모듈이 섞여서), 자리는 `App.tsx`의 `AppLayout` 바깥과 `renderPage`다. 경로가 바뀌면 닫는다(지우기 대화상자가 사라지면 기다리던 스냅샷 지우기를 멈추는 지금 규약 그대로). VM 목록은 대화상자를 연 뒤에만 구독한다. 행 메뉴와 VM 상세 머리가 이것을 쓴다.
- **S3 + 만들기**: VM은 만들기 화면이 따로 있으므로 `/vms/-new`로 바로 가고, 나머지(VM 가져오기는 OVA 올리기, 풀, 네트워크, 백업 작업, ISO 올리기)는 `?create=1`(`lib/createParam.ts`)이다. 쿼리는 열면서 지우고(다른 쿼리는 둔다), 권한이 없으면 열지 않고 지우기만 한다. 백업 작업은 풀·작업 목록을 읽을 때까지 기다린다. 768px~1023px에서 검색 상자는 아이콘 단추, 클러스터 전환은 1024px 이상에서만 보인다(S2의 클러스터 이름 자리).
- **S3 클러스터 전환**: 원격 목록은 메뉴를 열 때만 읽는다(`useRemotes(enabled)` — 머리줄이 10초마다 묻지 않게). "이 클러스터"는 홈으로 간다.
- **S3 단축키 범위**: 이번에 단 것은 ⌘K·`?`·`/`·`c`·`[`·`f`·`g h/v/i/p/a/s`다. `j`·`k`·`x`·`Space`·`Enter`는 S4 공통 표, `Shift+N`은 S6 서랍이 `SHORTCUTS`에 행을 더한다(도움말은 그 표로 그린다). `/`는 본문의 `[data-list-search]`(지금은 VM 목록)에, `f`는 본문의 `[data-pin-toggle]`(★)에 간다. ⌘K는 입력 칸 안에서도 받지만 다른 대화상자가 열려 있으면 받지 않고, 한 글자 키는 메뉴(`role=menu`)가 열려 있어도 받지 않는다. 시각은 `Date.now()`다.
- **S3 즐겨찾기·최근**: ★는 VM·노드 상세 머리에 있다(풀은 상세 화면이 없어 S4 표나 S5 `ResourceHeader`가 단다). 최근 항목은 VM·노드·보안 그룹 상세를 열 때 넣는다. 키는 `opl.{user id}.recent|pins`이고 항목은 `{kind, id, name}`이며, 읽을 때 지금 목록에 없는 id는 빼고 이름은 지금 이름으로 바꾼다. 홈의 즐겨찾기 카드는 S7 홈 격자에서 단다.
- **S3 e2e**: ⌘K 단계는 큰 VM 시험의 "VM 목록 → web 링크"를 팔레트(`ControlOrMeta+k` → "web" → "가상 머신" 묶음의 web)로 바꿨다. 머리줄 "Create new" 단추와 겹치지 않게 풀 만들기·VM 만들기 찾기를 `exact`로 고쳤다.
- **S4 공통 표 모양**: `components/data-table/`은 `DataTable`(열·거름·도구 줄·일괄 막대·카드·서랍을 한 컴포넌트에), `Toolbar.tsx`(`ViewMenu`·`ColumnMenu`), `state.ts`(`useTableState`·보기·열 설정), `PeekFacts.tsx`다. §5.3의 `BulkBar`는 따로 두지 않고 `DataTable`의 `bulk(rows, clear)` 자리가 `role="region"`("일괄 동작") 막대를 그린다. 열은 react-table `ColumnDef`를 직접 쓰지 않고 작은 `Column<T>`(`header`·`cell`·`value`(정렬)·`priority`·`actions`)를 받아 안에서 react-table 정렬·보이기에 넘긴다. 화면마다 손으로 짠 칸을 옮기기 쉽게 하려는 것이다.
- **S4 주소 상태**: `?q=`·`?sort=`(한 열, `-`는 내림)·거름 키(VM `phase`·`node`·`template`, 볼륨 `iso`)를 `replace`로 쓴다. 정렬이 주소에 없으면 `cols.{table}`의 마지막 정렬을 쓴다. 옛 `vms.sort`(`[{id, desc}]`)는 `cols.vms`가 없을 때 첫 항목을 옮겨 읽는다. 상세 화면 안의 표(VM 백업·작업 탭)는 주소를 쓰지 않는다. 열은 고르기(숨기기)만 하고 순서 바꾸기는 넣지 않았다(묶음 크기와 쓰임에 비해 끌기 UI가 무겁다).
- **S4 저장 보기**: 기본 보기는 "전체" + 코드의 보기(VM: 실행 중·실패·이 노드)다. "이 노드"는 머리줄 클러스터 전환이 이미 읽은 `["cluster"]` 캐시의 `node`가 있을 때만 보인다(목록이 따로 묻지 않는다). 저장은 지금 표 쿼리(`q`·`sort`·거름)에 이름을 붙여 `views.{table}`에 둔다.
- **S4 일괄 동작**: 막대 동작은 시작·정지·강제 정지·재시작·스냅샷·옮기기·지우기다(§5.8이 강제 정지의 수 입력을 말하므로 강제 정지도 넣었다. e2e "일괄 정지"는 OS 없는 게스트라 ACPI 정지가 끝나지 않아 강제 정지로 한다). 단추 이름은 "정지 (가능 n, 불가 m)", 가능 0이면 막힌다. 시작도 확인 대화상자(영향 목록)를 거친다. 지우기·강제 정지는 고른 수를 입력한다. 스냅샷은 `bulk-YYYYMMDD-HHMM`이고 디스크만 담는다(메모리 칸을 VM마다 따지지 않게). 옮기기 대상 노드는 온라인·유지보수 아님·위트니스 아님 노드에서 고르고, VM마다 `migrationTargets`로 맞는지 미리 보인다. 스냅샷이 있는 VM은 일괄 지우기에서 뺀다(스냅샷까지 지우는 길은 VM 하나의 대화상자에 있다).
- **S4 Task 추적·알림**: 요청은 `runLimited`로 동시에 4개다. Task를 받으면 `trackTask` 대신 `waitForTask`로 VM마다 끝나기를 기다려 대화상자의 결과 칸(요청함·성공·실패: 까닭)에 보인다. `trackTask`는 Task마다 알림을 띄우므로 §5.3의 "알림 하나로 묶기"와 맞지 않아서다. 알림은 하나(`toast.loading` → "정지: 8개 중 7개 성공, 1개 실패." + 건너뛴 수)이고 실패가 있으면 닫을 때까지 남고 "작업 보기"(`/tasks`)가 있다. 대화상자를 닫아도 기다림과 알림은 이어진다. 요청을 보내기 시작하면 고르기를 푼다.
- **S4 미리 보기·단축키**: 서랍은 `DataTable`의 `peek`이 그리는 `Sheet`(`data-peek`)다. 목록 키는 `DataTable`이 문서에서 받고(`keys`인 표 하나), 입력 칸·메뉴·`data-peek` 아닌 대화상자에서는 받지 않는다. `j`/`k`는 행에 포커스를 옮기고(서랍이 열려 있으면 포커스는 그대로 두고 내용만 바꾼다), `x`는 고르기, `Space`는 서랍 열기·닫기, `Enter`는 상세다. 단추·링크·체크박스 위의 `Space`·`Enter`·`x`는 그 요소에 맡긴다. `SHORTCUTS`의 `list` 묶음은 도움말에만 쓰고 머리줄 해석기는 뺀다. 노드 서랍은 상태·주소·버전·CPU·메모리·VM 수와 열기이고 VM 서랍은 노드·vCPU·메모리·고정 IP와 시작·정지(확인)·콘솔·열기다. 서랍과 풀 행의 ★는 `PinButton shortcut={false}`(`f`가 첫 행의 ★를 누르지 않게)다.
- **S4 다른 목록**: 노드·풀·볼륨·백업(`BackupTable`, 백업 화면만 주소 상태)·작업(`TaskTable`)·감사 표를 `DataTable`로 옮겼다. 칸 내용·순서는 그대로이고, 덜 중요한 열에 `priority` 2·3을 주었다(노드는 19열 가운데 이름·상태·VM 수만 1). 작업·감사는 서버·화면 페이지 나누기가 있어 검색·정렬을 달지 않았다. 풀의 ★는 이름 칸에 있다.
- **S4 휴대폰 카드**: 768px 미만이면 `matchMedia`로 표 대신 카드 목록(`<ul aria-labelledby>`)을 그린다. 카드는 첫 열(이름)·동작 열·우선순위 1 열만 보인다. 표와 카드를 둘 다 그려 CSS로 숨기면 시험의 역할 찾기가 둘씩 잡혀서다.
- **S4 묶음 크기와 lazy 경로**: 표·일괄·서랍만으로 첫 화면 JS가 S3보다 늘어 25 KB 한도를 넘으므로 S7의 lazy 경로 일부를 앞당겼다. Ceph·감사·경보·배치 둘·HA 검사·HA 그룹·클러스터·가져오기 둘·원격 셋·이미지·설정 일곱 화면이 react-router `lazy`다(`App.tsx`의 `page()`, 뿌리 경로에 빈 `HydrateFallback`). rolldown이 공유 조각을 여럿으로 나누므로 크기는 `index-*.js` 하나가 아니라 `index.html`이 내려받는(`script`·`modulepreload`) JS의 gzip 합으로 잰다.
- **S5 `ResourceHeader`**: `components/ResourceHeader.tsx`는 이름(h1)·상태·★·작은 글(`meta`)·주 동작·케밥(`menu`, "{이름}의 동작")과 그 아래 요약 카드 줄(`SummaryCards`, `<dl>`, 휴대폰 2칸·넓으면 4칸)이다. 노드(주소·CPU·메모리·VM 수), VM(노드·vCPU·메모리·고정 IP), 보안 그룹(규칙 수·쓰는 VM 수), 가져오기(노드·원본 VM·CPU·메모리 — 원본 카드에서는 이 셋을 뺐다), 원격(버전·노드·VM)에 썼다. 상세 위의 "목록으로" 링크(보안 그룹·가져오기·원격)는 이동 경로가 맡으므로 지웠고, 원격 화면의 h1은 원격 이름, VM 표 제목은 h2다.
- **S5 VM 머리**: §5.3 그림의 "주 동작 2~3개 + 케밥"으로 접지 않고 지금 단추 줄(전원 넷·콘솔·옮기기·고치기·복제·템플릿·지우기)을 `ResourceHeader`의 `actions`에 그대로 두었다. 막힌 까닭 목록(`ReasonList`)과 GitHub e2e(복제·시작·지우기·고치기 단추)가 이 단추들에 기대고, e2e는 이 Mac에서 돌릴 수 없어서다. 케밥으로 접는 일은 S7(VM 만들기 단계 폼과 함께 e2e를 고칠 때)로 미룬다.
- **S5 노드 동작 모으기**: `nodes/NodeActions.tsx`의 `useNodeActions()`가 유지보수·비우기·업데이트·인터페이스·디스크·레이블·펜스·얼림 풀기·지우기 메뉴 항목(`items(node, {primary})`)과 대화상자를 함께 가진다. 노드 목록 행 메뉴와 노드 상세 머리가 같은 것을 쓴다. 상세 머리는 유지보수·비우기를 단추로 두고 나머지를 케밥에 둔다. viewer는 업데이트·인터페이스만 본다. 상세에서 지우면 `/nodes`로 간다.
- **S5 확인 대화상자**: `ConfirmDialog`에 `impact`(글 목록, `<ul aria-label="영향">`)를 더하고 `typeToConfirm`이 수면 "확인하려면 N을(를) 입력하세요"(앞뒤 공백 무시, 숫자 자판)다. 영향은 이미 받은 목록에서 센다: VM 지우기(남는 디스크 볼륨·스냅샷·남는 백업), 네트워크 지우기(쓰는 VM), 풀 지우기(안의 볼륨·백업), 노드 지우기(배정된 VM). 노드 펜스·TPM 초기화·백업 지우기는 이미 자기 설명이 대상 하나라 넣지 않았다. `VmBulkDialog`는 시작한 뒤에도 열려 VM마다 결과를 보이므로(닫히는 `AlertDialog`인 `ConfirmDialog`와 맞지 않음) 합치지 않고, 수 입력 칸(`TypeToConfirm`)만 함께 쓴다. 취소가 기본 포커스(radix AlertDialog)이고 위험 단추는 오른쪽 끝이다(지금과 같음, 시험으로 굳힘).
- **S5 되돌리기·낙관적 갱신**: `lib/undo.ts`의 `undoToast`(10초, "되돌리기")를 즐겨찾기 빼기(`PinButton`, `setPin`으로 다시 넣음), 저장 보기 지우기(`ViewMenu`, 그 사이 같은 이름을 저장했으면 두기), 노드 레이블 저장(이전 레이블을 그때의 revision으로 `If-Match`, 그 사이 바뀌었으면 "바뀌었습니다" 오류), 보안 그룹 저장(설명과 규칙을 한 번에 저장하므로 둘 다 이전 값으로 `If-Match`)에 달았다. VM 정지 뒤 "다시 켜기"는 S6 알림 센터의 다시 하기와 함께 둔다. 낙관적 갱신(`api/optimistic.ts`)은 노드 유지보수 켜기·끄기와 노드 레이블이다(실패하면 되돌림). 캐시가 구조를 나눠 쓰므로(structural sharing) 되돌릴 때 객체가 아니라 revision으로 "아직 꾸민 값인지"를 본다. 경보 규칙 켜기는 목록에 바로 바꾸는 스위치가 없어(고치기 대화상자) 넣지 않았다.
- **S5 바뀐 행 강조·오프라인 띠**: `StatusChip`이 색·글이 바뀌면(처음 그릴 때는 빼고) 600ms `data-changed`를 달고 `index.css`의 `changed-flash`(강조색 테두리 그림자)가 돈다. 표를 건드리지 않아 VM·가져오기·노드 머리 등 모든 칩에 걸린다. 움직임 줄이기는 S1의 전역 규칙이 끈다. 오프라인 띠는 `AppLayout`의 본문 맨 위(`data-offline-banner`, 역할 없음 — 머리줄 `LiveBadge`의 role=status가 읽어 준다)다.
- **S5 PageHeader**: 남은 목록·개요 화면 27개(작업·감사·경보·알림 대상·사용자·영역·토큰·템플릿·이미지·ISO·가져오기·노드·풀·볼륨·네트워크·보안 그룹·HA 둘·배치 둘·클러스터·Ceph·백업 둘·VM 목록·홈·없는 화면)의 제목을 `PageHeader`로 옮겼다. 상세는 `ResourceHeader`다. VM 만들기·고치기(S7 단계 폼이 다시 짬)와 전체 화면 콘솔은 그대로다.
- **S6 센터 모양**: §5.4 그림의 위쪽 토글([진행 중] [최근] [경보])은 두지 않고 세 묶음(`<section>` 제목 "진행 중 (n)"·"최근"·"경보 (n개 울림)")을 한 서랍에 차례로 둔다. 서랍 하나에 보일 것이 많지 않고(최근은 끝난 Task 10개), 묶음마다 목록 이름이 있어 화면 읽기에도 낫다. 종 숫자는 끝나지 않은 Task(pending·running) + `state === "firing"`인 critical·warning 경보다(해제를 기다리는 경보도 센다, info는 세지 않는다). 숫자가 있으면 경보가 울릴 때 빨강, Task만이면 강조색이다. 열림 상태는 `tasks/center.ts`의 모듈 상태(`useSyncExternalStore`)다 — 종, `settleTasks`, 실패 알림의 "보기"가 함께 읽고 써서다.
- **S6 깊은 링크**: `tasks/target.ts`의 `taskTarget(task, lookup)`이 Task 대상 → 경로를 정한다. VM은 상세에 op에 맞는 `?tab=`(스냅샷 → snapshots, 백업·백업 복제 → backups, 펌웨어 초기화·핫플러그 → hardware), 노드는 `/nodes/:name`, 볼륨·백업은 목록 검색(`?q=`), 가져오기는 상세, Ceph Task는 `/ceph`다. `alertHref(subject)`는 노드·VM 상세, 풀 목록 검색, 백업 작업·Task 목록이다. 작업 화면·VM 작업 탭·홈의 `TaskTable`도 같은 함수를 쓰므로 백업 Task 링크가 `/vms/web?tab=backups`가 됐다. 경보 화면·홈 경보 위젯·센터의 `AlertList`는 `subjectHref`를 받아 대상이 링크다.
- **S6 다시 하기**: `tasks/retry.ts`의 `retryOf(task, ctx)`가 실패한 Task만 본다. 전원 넷은 VM이 남아 있고 operate 권한이면 `useVmActions().run`으로 다시 보내고, 지금 상태로 막히면(`availability()`) 단추를 흐리고 그 이유를 옆에 보인다. 백업은 `vm-backup` Task의 `op.backup` → 그 백업의 `spec.job`이 있을 때만 "작업 다시 실행"(`POST /backup-jobs/{id}/run`, 작업 쓰기 권한)이고, 작업이 도는 중이거나 꺼져 있으면 흐리다. 작업 전체를 다시 돌리는 것이라 단추 글을 "다시 하기"와 다르게 했다. 확인이 있던 동작(정지·강제 정지)도 §5.4대로 확인 없이 다시 보낸다. 따로 "열기" 단추는 두지 않고 Task 설명 자체가 대상 링크다.
- **S6 알림 줄이기**: 센터가 열려 있으면 `trackTask`가 도는 알림을 띄우지 않고 단추 없는 성공 알림은 닫기만 한다. 경고·실패·"결과를 알 수 없음"은 늘 띄운다. 실패 알림에는 센터를 여는 "보기"가 있다. 경보는 알림으로 띄우지 않는다(지금과 같음). VM 정지·강제 정지 성공 알림에는 10초 "다시 켜기"를 단다(`TaskToasts`가 캐시의 VM을 찾았을 때, 센터가 열려 있어도 띄운다).
- **S6 묶음 크기**: 센터 본문을 `lazy`로 떼어 봤으나 rolldown이 함께 쓰는 모듈(`actions`·`target`·`TasksPage`·`StatusChip`)을 조각으로 더 나눠 첫 화면 JS가 오히려 0.8 KB 컸다. 그래서 바로 넣었다(radix Dialog는 열릴 때만 본문을 그리므로 목록 구독도 열릴 때만이다).
- **S6 활동 영역**: 탭(작업·경보·감사 로그, 감사는 admin)은 S2 그대로다. 작업 상태(`TaskStateBadge`, running은 깜박이는 점), 경보 심각도(`SeverityBadge`), 알림 기록의 보냄·실패, 규칙의 켜짐·꺼짐을 모두 `StatusChip`으로 바꿔 세 탭과 서랍·홈이 같은 칩을 쓴다. 쓰지 않게 된 `severity.ts`의 `TONE_CHIP`은 지웠다. `Shift+N`은 `SHORTCUTS`의 `center`(키 이름 `N`)이고 도움말에는 `⇧N`(맥)·`Shift N`으로 보인다.
- **S7 홈 격자**: 첫 걸음 → 건강 띠(노드 온라인, VM 실행·정지·실패 — 실패 수는 `/vms?phase=failed` 링크, 울리는 경보 수 — critical·warning만, 리더) → 혼합 버전 경고 → 경보·용량·풀 사용률(세 칸) → 클러스터 차트 → 노드 부하·상위 VM → 최근 작업·즐겨찾기다. 지난 홈의 노드별 표와 실패 VM 목록은 지웠다(노드 부하 위젯·노드 화면·경보 `vm_failed`가 맡는다). 용량은 새 `CapacityWidget`(온라인이고 위트니스가 아닌 노드의 코어·메모리 가운데 도는 VM에 나눠 준 몫, 85 % 넘으면 경고색)이고 `usage()`가 이리 옮겼다. 최근 작업은 `TaskTable`을 `WidgetCard`에 넣었다(`WidgetCard`에 `titleId`).
- **S7 당직 보기**: 휴대폰 순서는 CSS `order`가 아니라 `useIsPhone()`(공통 표의 훅을 내보냄)으로 DOM 순서를 바꾼다. 화면 읽기·탭 순서도 경보 → 건강 띠 → 즐겨찾기 → 용량 → 풀 → 최근 작업이 되게 하려는 것이다. 휴대폰 홈에는 차트·노드 부하·상위 VM을 그리지 않는다(좁은 폭에 맞지 않고 무겁다). 전원은 `VmPowerButton`(꺼졌으면 시작은 바로, 아니면 정지 확인 — `VmActionHost`, 40px)으로 즐겨찾기 행과 VM 목록의 휴대폰 카드에 있다. VM 폼·콘솔에는 768px 미만에서 "넓은 화면에서 쓰기 좋습니다" 안내가 보인다.
- **S7 첫 걸음**: VM이 하나도 없을 때만(그리고 VM 쓰기 권한이 있을 때만) 홈과 VM 목록 위에 보인다. 풀·볼륨 목록으로 ✓을 정하고(볼륨은 ISO든 디스크 이미지든 하나면 둘째 단계 끝), VM이 생기면 셋째 단계를 ✓으로 보이지 않고 카드째 사라진다. 둘째 단계는 ISO 올리기(`/isos?create=1`)와 "이미지 받기"(`/images`) 링크 둘이다.
- **S7 VM 만들기**: 프리셋은 폼 위 라디오 묶음(빈 VM·Linux 서버·Windows 11·템플릿에서)이다. 이름과 이미 고른 디스크·NIC·CD-ROM은 남기고 나머지는 처음 값에서 다시 채운다(`vms/form/steps.ts`의 `applyPreset`). Linux 서버는 코어 2·2048 MiB·게스트 에이전트·cloud-init 켬, Windows 11은 계획 59의 `windows11()`이다(옛 "Windows 11용으로 채우기" 단추를 대신함). "템플릿에서"는 폼과 만들기 단추 대신 템플릿 목록을 보이고 같은 배포 대화상자(`VmActionHost` `deploy`)를 연다. 단계 폼은 섹션을 접지 않고 모두 펼친 채 둔다. 왼쪽(1024px 이상)의 단계 목록 `<nav aria-label="폼 단계">`이 섹션으로 옮기고 오류가 있는 단계에 표시와 수를 보이며, 보내기에 실패하면 오류가 있는 첫 단계로 옮긴다. 접으면 숨은 섹션의 오류·필드를 키보드·화면 읽기 사용자가 놓치고, 시험·e2e가 섹션 안 필드를 바로 찾기 때문이다. 맨 아래 "검토"가 보낼 값을 요약한다.
- **S7 클러스터 합류 마법사**: 새 화면을 만들지 않고 지금 두 대화상자(토큰 발급 → 명령)에 단계 표시(1 토큰 만들기 · 2 새 노드에서 명령 실행 · 3 노드 기다리기, `aria-current="step"`)를 달았다. 명령 칸·복사를 누르면 3단계가 되고, 명령 대화상자를 연 때의 노드 목록에 없던 Node가 watch로 나타나면 "{이름}이(가) 클러스터에 합류했습니다"를 `aria-live`로 알린다. 닫기는 지금처럼 "완료"뿐이다(기다리지 않고 닫아도 된다).
- **S7 Ceph 빈 상태**: Ceph이 없으면 마법사를 바로 보이지 않고 `EmptyState`(아이콘·설명, admin에게 "Ceph 설정 시작")를 보인다. 주 단추가 지금 마법사를 연다. 다른 역할은 단추 없이 지금 안내 글을 본다. 안내서 링크(`docsHref`)는 앱 안에 문서 주소 규약이 없어 달지 않았다.
- **S7 VM 머리 접기(S5에서 미룸)**: 단추는 시작·정지·콘솔·옮기기이고, 재시작·강제 정지·고치기·복제(템플릿이면 배포)·템플릿으로 바꾸기(되돌리기)·지우기는 케밥("{이름}의 동작")이다. 막힌 항목은 `ReasonMenuItem`으로 이유를 읽는다. 케밥에서 연 대화상자는 닫히면 케밥으로 포커스를 돌린다. e2e `vm.spec.ts`는 `vmMenu(page, vm, item)`로 복제·강제 정지·지우기·고치기를 고른다.
- **S7 lazy 경로와 묶음 크기**: 템플릿·ISO·볼륨·네트워크·보안 그룹 둘·백업·백업 작업·VM 만들기·고치기·VM 상세·콘솔·노드 목록·노드 상세·풀을 lazy로 더 뗐다. 첫 화면(`eager.sh`) JS gzip 410,963 → 374,433 B(−36.5 KB), CSS 13,952 → 14,120 B(+0.17 KB). S1~S6이 약 +19.8 KB였으므로 S1 전보다 약 16.6 KB 작다(25 KB 한도 안). 홈·VM 목록·작업은 첫 화면이라 바로 넣었다.
- **S8 문서**: README는 짧은 첫 화면이라 §5.2의 팔레트 IP 한계는 README가 아니라 새 가이드 `docs/guide/web-ui.md`("웹 화면 둘러보기")의 "한계"에 즐겨찾기·저장 보기가 브라우저에만 있다는 것과 함께 적었다. README에는 기능 표의 "웹 UI" 줄과 안내 표의 새 가이드만 더했다. 단축키 표는 `SHORTCUTS` 그대로다. 다른 가이드의 "사이드바 …"·"메뉴 …"·"설정 화면"·"대시보드" 같은 옛 이름은 `영역 › 탭` 꼴로 고쳤다.

### 리뷰 1에서 고친 것

- **늦은 경로의 오류 화면과 새 버전 안내**: 셸 안의 모든 화면을 경로 없는 자식 경로 하나로 감싸 `errorElement`(`layout/RouteError.tsx`)를 달았다(셸 자체가 깨지면 뿌리 경로의 같은 화면). 화면 조각을 받지 못한 오류(Chrome·Firefox·Safari·Vite CSS의 글)면 "새 버전이 있습니다"와 새로고침, 다른 오류면 셸 안에 짧은 설명·새로고침·홈 링크다(React Router의 기본 화면과 스택을 보이지 않는다). `main.tsx`가 `vite:preloadError`에 "새 버전이 있습니다" 알림 하나(새로고침 단추)를 띄운다. 스스로 새로고침하지는 않는다(쓰던 폼을 잃거나 서버가 정말 깨졌을 때 되풀이할 수 있어서). 늦은 화면을 받는 동안 셸 위에 얇은 막대(`NavProgress`, `useNavigation()`, 150ms 늦게 보임, role=progressbar "화면을 여는 중")가 보인다.
- **Shift 범위 기준을 행 id로**: `DataTable`이 기준 행을 색인이 아니라 id로 두고 Shift를 누를 때 지금 행 순서에서 찾는다. 다시 정렬·거름·watch로 순서가 바뀌어도 고른 행부터이고, 기준 행이 보이지 않으면 누른 행 하나만 바꾼다.
- **다시 하기의 확인과 두 번 누르기**: 센터의 다시 하기는 시작만 바로 보내고, 정지·재시작·강제 정지는 `VmActionHost`의 같은 확인 대화상자를 연다(S6의 "확인 없이 다시 보낸다"를 바꿈). 요청 중에는 단추를 막는다. 실패 뒤 같은 VM에 새 전원 Task(또는 같은 VM의 새 백업 Task)가 있으면 다시 하기를 보이지 않는다(`latestTasks`).
- **팔레트 시작은 정확한 이름만**: `start db`가 앞부분만 맞는 `db-prod`를 바로 켜지 않는다. 친 이름이 VM 이름과 정확히 같을 때(대소문자 무시)만 바로 하고, 아니면 새 "시작할까요?" 확인(`VmDialog`의 `start`)을 연다. 입력 칸은 `isComposing` 중의 `Enter`를 무시한다(한글 마지막 글자를 끝내는 Enter).
- **일괄 창의 살아 있는 VM**: VM 목록은 고른 VM의 id만 넘기고 `VmBulkDialog`가 `useList("vm")`에서 지금 값을 찾는다. 상태·불가 까닭이 열린 동안 바뀐다. 목록에서 빠진 VM은 연 때의 이름으로 "지워짐"이다. 입력할 수는 고른 수가 아니라 실제로 할 수(`able.length`)이고 "고른 VM n개 중 m개에 합니다"로 두 수를 함께 보인다(§5.8의 "고른 수"를 바꿈).
- **사용자별 저장**: 저장 보기·열 설정·마지막 탭을 `{user}.views.{table}`·`{user}.cols.{table}`·`{user}.nav.last.{area}`에 둔다(한 브라우저를 여럿이 쓸 때 앞 사용자의 `?q=`가 넘어가지 않게, §5.3·S2의 공용 키를 바꿈). 사용자 키가 없으면 옛 공용 키(옛 `vms.sort` 포함)를 처음 읽는 사용자에게 한 번 옮기고 공용 키는 지운다(`readUserPref`).
- **한글 입력의 단축키**: `letterKey`가 영문이 아닌 한 글자 `e.key`(`ㅎ`)를 `e.code`(`KeyG`)의 영문자로 바꾼다. 머리줄 단축키(`g` 줄, `?`는 그대로, `⌘K`, `Shift+N`)와 표의 `j`·`k`·`x`가 쓴다. 영문 자판(AZERTY 등)은 `e.key`를 그대로 쓴다.
- **작은 것**: "다시 켜기" 알림은 누를 때 VM의 지금 상태를 다시 보고(켜져 있거나 지워졌으면 까닭 알림), `actionFor`가 그릴 때마다 바뀌지 않게 `run`을 ref로 넘긴다. 일괄 막대·창의 지우기 까닭은 스냅샷이 있는 VM id 집합을 한 번 만들어 본다(VM × 스냅샷 훑기 없앰). 팔레트 `migrate web to node2`의 노드가 그 사이 후보에서 빠지면 옮기기 창이 다른 노드를 골랐다고 알린다.
- **보이는 탭이 없는 영역(계획 62 메모)**: 역할에 보이는 탭이 없는 영역 주소는 `AreaRedirect`가 자기 주소로 넘기며 끝없이 돌았다. 이제 넘기지 않고 영역 이름과 빈 상태("이 영역에는 지금 역할로 볼 수 있는 화면이 없습니다")를 보인다. 사이드바는 이미 그런 영역을 숨긴다(`visibleAreas`).
