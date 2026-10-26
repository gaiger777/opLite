# opLite 웹 UI 설계 (하위 프로젝트 2)

작성일: 2026-10-23
상위 문서: `2026-10-20-opl-node-core-design.md` (§1.1의 하위 프로젝트 2)

## 1. 목표와 범위

브라우저에서 CLI(`oplctl`)와 같은 수준으로 클러스터를 운영한다. 파일럿 고객이 CLI 없이 하루 운영을 할 수 있어야 한다.

| 메뉴 | 목록 | 상세와 동작 |
|---|---|---|
| 대시보드 | 노드·VM 요약, 노드별 할당량(VM spec 합), 최근 작업, 실패한 VM | — |
| 가상 머신 | 검색·정렬·여러 개 선택, 일괄 시작·정지 | 요약·하드웨어·콘솔·작업·JSON 탭, 만들기·편집·삭제·시작·정지·강제 정지·재시작 |
| 볼륨 | 풀·크기·사용 중인 VM | 만들기(빈 볼륨), 이미지 업로드(진행률), 삭제 |
| 스토리지 풀 | 백엔드·노드 | 만들기·삭제 |
| 네트워크 | 브리지·VLAN, 그 브리지가 있는 노드 | 만들기·삭제 |
| 노드 | 온라인·하이퍼바이저·CPU·메모리·브리지 | 유지보수 모드 켜고 끄기, 노드 제거 |
| 작업 | 종류·대상·상태·오류, 실패만 보기 | — |
| 사용자 (admin) | 이름·역할 | 만들기·역할 변경·비밀번호 재설정·삭제 |
| API 토큰 | 내 토큰 | 만들기(비밀값은 한 번만 표시)·삭제 |
| 클러스터 | 멤버·리더·CA 지문 | 조인 토큰 발급(admin, 복사용 명령 표시), CA 내려받기 |

오른쪽 위 메뉴: 언어(한국어·English), 테마(시스템·밝게·어둡게), 내 비밀번호 변경, 로그아웃.

범위 밖: 사용자 인증서로 HTTPS 교체, 노드 추가(새 노드에서 `opld join`을 실행해야 한다), 성능 그래프(메트릭 수집이 없다), 모바일 전용 화면(좁은 화면에서 깨지지 않는 정도만).

## 2. 기술 선택

| 항목 | 선택 | 이유 |
|---|---|---|
| 언어·빌드 | TypeScript, React, Vite, pnpm | 관리 콘솔(표·폼·실시간 갱신)에 생태계가 가장 크고 사람을 구하기 쉽다 |
| UI 컴포넌트 | shadcn/ui(Radix + Tailwind), TanStack Table | 컴포넌트를 저장소에 복사해 직접 고칠 수 있다 |
| 서버 상태 | TanStack Query | watch 이벤트를 캐시에 직접 반영한다 |
| API 타입 | `openapi-typescript` + `openapi-fetch` | Rust의 utoipa 문서에서 타입을 만든다 |
| 라우팅 | React Router | 화면마다 주소가 있다(`/vms/web-01`) |
| 번역 | i18next(ko, en), 기본은 브라우저 언어 | API 오류 `code`를 번역 키로 쓴다 |
| 콘솔 | noVNC | API가 WebSocket으로 RFB를 그대로 넘긴다 |
| 테스트 | Vitest, Testing Library, MSW, Playwright | §9 |

검토했지만 채택하지 않은 대안:
- **Svelte, Leptos(Rust→WASM), 서버 렌더링 + htmx:** 각각 생태계·인력, UI 컴포넌트·noVNC 연동, 화면이 많을 때의 상호작용 처리가 React보다 불리하다.
- **Mantine, Ant Design, MUI:** 완성된 컴포넌트는 많지만 생김새를 우리 제품에 맞게 바꾸기 어렵다(Ant의 고유한 느낌, MUI 고급 표의 유료 기능).
- **리소스 트리 화면(Proxmox·vSphere 방식):** 볼륨·풀·네트워크·사용자처럼 노드 아래에 딱 떨어지지 않는 리소스가 많다. 종류별 메뉴 + 표로 하고, 노드별 분포는 대시보드에서 보여 준다.

## 3. 구조와 빌드

```
web/                     Vite + React + TS (pnpm)
  src/api/               생성한 타입(schema.d.ts), fetch 클라이언트, watch
  src/components/ui/     shadcn/ui 컴포넌트
  src/pages/             리소스 종류별 화면
  src/i18n/{ko,en}.json  화면 문구와 API 오류 code 번역
  e2e/                   Playwright (실제 opld + QEMU)
crates/opld/src/web.rs   정적 파일 서빙 (feature "web-ui")
```

**opld의 서빙** (8006, API와 같은 출처):
- `/api/v1/*`는 지금 그대로다. `/api/`로 시작하는 경로는 절대 `index.html`로 대체하지 않는다(JSON 404).
- 그 밖의 경로는 파일이 있으면 그 파일, 없으면 `index.html`이다(화면 주소 새로고침).
- 캐시: `/assets/*`(파일 이름에 해시)는 `max-age=31536000, immutable`, `index.html`은 `no-cache`.
- 보안 헤더(§6)를 비 API 응답에 붙인다.
- `web-ui` 기능이 없으면 비 API 경로는 "웹 UI가 포함되지 않은 빌드입니다"라는 짧은 HTML이다.
- `web-ui`는 `rust-embed`로 `web/dist`를 넣는다. 릴리스 빌드는 `web/dist`가 없으면 컴파일 오류다. 디버그
  빌드(테스트, clippy)는 없어도 되고 안내 페이지를 주며, 파일을 실행 중에 디스크에서 읽는다.

**빌드 흐름:**
- 개발: `pnpm -C web dev`(5173)가 `/api`(WebSocket 포함)를 `https://localhost:8006`으로 프록시한다. 인증서는 검증하지 않는다. Host를 바꾸지 않아야(`changeOrigin` 없음) 쿠키 요청의 Origin 검사를 통과한다. Chrome은 `http://localhost`에서도 `Secure` 쿠키를 저장한다.
- 릴리스: `pnpm -C web build` 다음 `cargo build -p opld --release --features web-ui`.
- Rust CI(check 2개, deny): 바꾸지 않는다(디버그 빌드는 `web/dist` 없이 된다). linux-kvm은 E2E를 위해 실제 UI를 빌드하고 Chromium을 설치한다.
- 새 CI 작업 `web`: `pnpm install --frozen-lockfile`, 타입 검사, ESLint, Vitest, OpenAPI 타입 동기화 검사, `pnpm audit --prod`, 빌드.

검토했지만 채택하지 않은 대안: `build.rs`가 매번 `pnpm build`를 실행(모든 cargo 명령에 Node가 필요하고 느리다), `dist`를 git에 커밋(압축된 JS diff, 소스와 어긋남).

## 4. 데이터 흐름

**API 타입:** opl-api에 OpenAPI 문서를 표준 출력으로 내는 바이너리(`opl-api-openapi`)를 둔다. `pnpm -C web gen:api`가 이것으로 `web/src/api/schema.d.ts`를 만들고, 그 파일은 커밋한다. CI는 다시 만든 결과가 커밋된 파일과 다르면 실패한다.

**요청:** `openapi-fetch`로 경로·본문·응답을 타입 검사한다. 쿠키는 같은 출처라 자동으로 실린다. 서버 상태는 TanStack Query 캐시에 둔다(키: `["list", kind]`). 상세 화면도 목록에서 찾는다.

**실시간 갱신:**
1. 로그인 직후 그 역할이 읽을 수 있는 6종 목록(vm, volume, storage-pool, node, task, network)을 함께 읽는다.
2. 응답 `revision` 중 가장 작은 값을 `since`로 `/api/v1/watch` WebSocket을 하나 연다. 앱 전체가 이 연결 하나를 쓴다.
3. `put`과 `deleted` 이벤트를 캐시에 반영한다. 리소스의 `revision`이 캐시에 있는 것보다 클 때만 적용한다(목록마다 읽은 시점이 달라도 순서가 꼬이지 않는다).
4. `error`(expired·lagged·closed), `reset`, 연결 끊김이면 1부터 다시 한다. 재연결 간격은 1초에서 30초까지 늘리고, 그동안 상단에 "실시간 연결 끊김"을 표시한다.

사용자와 토큰은 감시할 수 없으므로 화면을 열 때와 변경 뒤에 다시 읽는다.

**작업(Task):** 202로 Task를 받으면 "진행 중" 알림을 띄우고, watch로 그 Task가 `succeeded`·`failed`가 되면 결과 알림으로 바꾼다. 실패면 Task의 `error`를 보여 준다.

**로그인 상태:** 시작할 때 `/auth/whoami`. 401이면 로그인 화면으로 가고, 로그인 뒤 원래 주소로 돌아온다. 어느 요청이든 401이면(세션 12시간 만료, opld 재시작, 비밀번호 변경) 캐시를 비우고 로그인 화면으로 간다. 역할(admin·operator·viewer)에 따라 메뉴와 버튼을 숨기지만 권한 검사는 서버가 한다.

**오류 표시:** API 오류 `code`를 번역 키로 쓴다(`errors.vm_not_running`). 번역이 없으면 서버의 `message`를 보여 준다. 서버의 400 메시지는 필드를 가리키지 않으므로 폼 위에 보여 주고, 흔한 입력 오류(이름 규칙, 최소 메모리, 부팅 순서, MAC 형식)는 보내기 전에 해당 필드 아래에 보여 준다.

## 5. 주요 화면 결정

- **VM 만들기:** 한 페이지 폼(마법사 아님). 이름, vCPU, 메모리, 펌웨어, 머신, 디스크, NIC, 노드(자동 또는 지정), 재시작 정책, "만든 뒤 시작". API가 노드의 아키텍처를 알려 주지 않으므로 머신 기본값은 모든 노드가 hvf(Apple Silicon)면 `virt`, 아니면 `q35`다. 디스크는 기존 볼륨을 고르거나 새 빈 볼륨(풀·크기)을 만든다. 새 볼륨이면 볼륨 Task가 끝난 뒤 VM을 만든다. "만든 뒤 시작"은 `power: running`이다.
- **VM 편집:** 같은 폼. `If-Match: revision`으로 보내고, 충돌하면 "다시 불러오기"를 안내한다. 실행 중이면 노드와 디스크 칸을 잠그고 "다음 시작부터 적용"을 안내한다.
- **콘솔:** noVNC. 상세 페이지 탭과 새 탭(전체 화면). Ctrl+Alt+Del 보내기, 클립보드 입력(브라우저 클립보드의 글자를 키 입력으로 보낸다. QEMU VNC의 클립보드 공유는 게스트 에이전트가 필요하다).
- **볼륨 업로드:** API는 받은 노드에서 쓸 수 있는 풀로만 가져온다. 그래서 지금 접속한 노드의 풀만 고르게 한다(§8의 `GET /cluster`의 `node`). `XMLHttpRequest`로 보내 진행률과 취소를 지원하고, 업로드 중 페이지를 떠나면 확인한다. 업로드는 화면을 옮겨도 계속되며 진행률과 취소 버튼은 알림에 있다. 다 올리면 가져오기 Task를 다른 작업처럼 알림으로 지켜본다. 로그아웃하거나 세션이 끝나면 업로드를 멈춘다.
- **되돌릴 수 없는 동작:** VM·볼륨 삭제, 노드 제거, 강제 정지는 확인 대화상자. 삭제와 노드 제거는 이름을 입력해야 한다.
- **API 토큰:** 만든 직후 한 번만 보여 주고 복사 버튼을 둔다. 누구나 자기 토큰을 만들고 지운다(관리자는 모든 토큰).
- **클러스터:** 멤버·리더·CA는 모든 역할이 본다(API가 읽기를 허락하고, CA 지문은 인증서 확인에 필요하다). 조인 토큰 발급만 admin이다. 발급하면 새 노드에서 실행할 `oplctl cluster join '<URL>'` 명령을 한 번만 보여 준다.
- **노드:** 유지보수 모드는 `If-Match`로 바꾼다. 제거는 오프라인인 다른 노드만 고를 수 있다(살아 있는 노드는 그 노드에서 `oplctl cluster leave`).
- **사용자와 내 비밀번호:** admin이 아니면 사용자 메뉴를 숨긴다(operator·viewer는 계정을 읽지 못한다). 관리자는 다른 사용자의 비밀번호를 재설정하고, 자기 비밀번호는 계정 메뉴의 "내 비밀번호 변경"에서 현재 비밀번호와 함께 바꾼다. 서버가 그 사용자의 세션을 모두 끝내므로 바꾼 뒤 로그아웃한다.

## 6. 보안

- **XSS:** React 이스케이프만 쓴다. `dangerouslySetInnerHTML`은 ESLint로 막는다.
- **CSP:** `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'`. 같이 붙이는 헤더: `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`.
- **CSRF:** 지금의 `SameSite=Strict` 쿠키와 Origin 검사를 그대로 쓴다.
- **브라우저 저장소:** 비밀값을 두지 않는다. `localStorage`에는 언어, 테마, 표 정렬만 둔다.
- **의존성:** `pnpm-lock.yaml` 커밋, 버전 정확히 고정, CI에서 `pnpm audit --prod`.

## 7. 오류 처리

| 상황 | UI 동작 |
|---|---|
| 401 | 캐시를 비우고 로그인 화면, 로그인 뒤 원래 주소 |
| 403 | "권한이 없습니다" 알림 |
| 400·409 | 폼 필드나 알림에 번역된 메시지. revision 충돌은 "다시 불러오기" |
| 5xx·네트워크 끊김 | 알림. 읽기는 2번까지 다시 시도, 쓰기는 다시 시도하지 않음 |
| watch 끊김 | 상단 표시, 재연결 뒤 목록 새로 읽기 |
| 업로드 중 이탈 | 브라우저 확인창. 끊기면 서버가 볼륨을 정리한다 |
| 콘솔 실패 | 연결 끊김 표시와 다시 연결 버튼(§10) |

## 8. API 변경

웹 UI를 위해 API를 두 가지만 바꾼다.
- `GET /api/v1/cluster` 응답에 `node`(응답한 노드의 id)를 더한다. 볼륨 업로드에서 쓸 수 있는 풀을 고르는 데 쓴다.
- `POST /api/v1/auth/logout`은 `opl_session` 쿠키를 지우는 `Set-Cookie`(`Max-Age=0`)를 돌려준다(M5부터 알려진 제한을 고친다).

## 9. 테스트

| 층 | 도구 | 확인 |
|---|---|---|
| Rust | `cargo test` | `web.rs`(정적 파일, SPA 대체, `/api/*` JSON 404, 캐시·보안 헤더, 기능 없을 때 안내), 로그아웃 쿠키, `GET /cluster`의 `node` |
| 타입 동기화 | CI | 다시 만든 `schema.d.ts`가 커밋된 것과 같다 |
| 프론트 단위 | Vitest | watch 적용(revision 순서, 삭제, reset), 오류 code 번역, 401 처리, ko·en 번역 키 집합이 같다 |
| 프론트 컴포넌트 | Vitest + Testing Library + MSW | 역할별 메뉴·버튼, VM 폼 검증, revision 충돌 안내, 삭제 확인 |
| 브라우저 E2E | Playwright(Chromium) + 실제 opld·QEMU | 아래 흐름 |

E2E 흐름: opld 하나를 `init` → 로그인 → 테스트 이미지를 볼륨으로 업로드(계획 9부터, 계획 8은 CLI로 미리 만든다) → VM 만들고 시작 → 목록에 "실행 중"(watch 갱신) → 콘솔 캔버스에 화면이 그려짐(전부 검은색이 아님) → 정지·삭제 → 목록에서 사라짐 → 로그아웃 뒤 보호된 주소는 로그인 화면.

맥 로컬(hvf)과 CI `linux-kvm`에서 돌린다. 브리지 네트워크는 root와 Linux가 필요해 E2E에 넣지 않는다(컴포넌트 테스트로 확인).

## 10. 알고 받아들인 제한

- 클러스터 CA를 브라우저에 가져오기 전까지 인증서 경고가 뜬다. README에 `/api/v1/cluster/ca`로 가져오는 방법을 적는다.
- 세션은 노드의 메모리에 있다. 접속한 노드의 opld가 다시 시작하면 다시 로그인한다.
- 콘솔 WebSocket 업그레이드 전의 API 오류(`vm_not_running`, `console_unavailable`)는 브라우저가 본문을 주지 않아 "연결이 끊겼다"로만 보인다.
- API에 페이지 나누기가 없다. 작업 목록은 모두 받아 화면에서 최근 순으로 자른다.
- 볼륨 업로드는 지금 접속한 노드에서 쓸 수 있는 풀로만 된다. 한 요청이라 끊기면 처음부터 다시 올린다(이어 올리기 없음).
- noVNC 1.7은 기본으로 하위 프로토콜을 요청하지 않아 지금의 콘솔 WebSocket에 그대로 붙는다(계획 8 E2E로 확인).

## 11. 계획 분할

| 계획 | 범위 | 완료 기준 |
|---|---|---|
| 8 | `web/` 뼈대, opld `web-ui` 서빙, API 변경 2개, OpenAPI 타입 생성, 로그인·레이아웃·i18n·다크 모드, watch, VM 목록·상세·만들기·편집·동작, 콘솔, 작업, 대시보드, CI `web`, E2E(볼륨은 CLI로 준비) | 브라우저로 로그인해 VM을 만들고 시작해 콘솔로 들어가는 E2E가 맥과 CI `linux-kvm`에서 통과 |
| 9 | 볼륨(업로드 포함)·풀·네트워크·노드·사용자·토큰·클러스터 화면, E2E 업로드 단계 | CLI의 모든 관리 기능을 웹 UI로 할 수 있고 E2E가 업로드부터 통과 |
