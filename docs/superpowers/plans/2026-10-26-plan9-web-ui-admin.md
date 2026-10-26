# opLite 계획 9: 웹 UI 관리 화면 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 볼륨(이미지 업로드 포함)·스토리지 풀·네트워크·노드·사용자·API 토큰·클러스터 화면과 내 비밀번호 변경을 더해, CLI의 관리 기능을 모두 웹 UI로 할 수 있게 한다(하위 프로젝트 2의 뒤 절반).

**Architecture:** 계획 8의 구조를 그대로 쓴다. 감시하는 리소스(볼륨·풀·네트워크·노드)는 `useList`의 watch 캐시에서 읽고, 감시할 수 없는 사용자·토큰은 화면을 열 때와 바꾼 뒤에 다시 읽는다. 만들기와 고치기는 공통 `FormDialog`(보내는 동안 잠그고 서버 오류를 폼 위에 보인다)로, 되돌릴 수 없는 동작은 기존 `ConfirmDialog`로 한다. 이미지 업로드는 `XMLHttpRequest`로 보내 진행률과 취소를 알림에 보이고, 다 올리면 가져오기 Task를 기존 작업 알림에 넘긴다. 브라우저 E2E는 풀 만들기와 업로드부터 VM·볼륨·풀 삭제까지 브라우저로 한다.

**Tech Stack:** 계획 8과 같다(새 의존성 없음). TypeScript 5.9.3, React 19.3, Vite 8.3, shadcn/ui(radix-nova), TanStack Query 5, React Router 7, openapi-fetch 0.17, i18next 26, sonner 2, Vitest 5, Testing Library, MSW 2, Playwright 1.64; Rust 2024(E2E 하네스만)

**설계 문서:** `docs/superpowers/specs/2026-10-23-web-ui-design.md` (§1 표, §5, §11의 계획 9)
**선행 계획:** 계획 1~8 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| 볼륨: 목록(크기·사용량·상태·노드·사용하는 VM), 빈 볼륨 만들기, 삭제(이름 입력) | 노드 추가 자체(새 노드에서 `oplctl cluster join`을 실행해야 한다) |
| 이미지 업로드: 접속한 노드의 풀만, 진행률·취소, 페이지 이탈 확인, 로그아웃하면 멈춤 | 이어 올리기(끊기면 처음부터) |
| 스토리지 풀: 목록(경로·노드·남은 공간·볼륨 수), 만들기(노드 선택), 삭제 | 풀의 `shared` 설정(CLI에도 없다) |
| 네트워크: 목록(브리지가 있는 노드, VLAN 필터링 경고, 쓰는 VM 수), 만들기, 삭제 | 노드의 브리지 만들기(관리자가 노드에서 한다) |
| 노드: 목록(상태·주소·하이퍼바이저·CPU·메모리·브리지·VM 수), 유지보수 모드, 죽은 노드 제거 | 노드 아키텍처 API(머신 기본값) — 다음 계획 |
| 사용자(admin): 만들기, 역할 변경, 비밀번호 재설정, 삭제. 역할별 메뉴 | 작업 목록 페이지 나누기, `index.html` ETag — 다음 계획 |
| API 토큰: 내 토큰(관리자는 모두), 만들기(비밀값은 한 번만, 복사), 삭제 | VM 폼 컴포넌트 나누기 — 다음 계획 |
| 클러스터: 멤버·리더, CA 지문·내려받기, 조인 토큰 발급(명령 복사) | |
| 계정 메뉴의 내 비밀번호 변경 | |
| `can()`에 계정 종류 규칙, 공통 부품(폼 대화상자·필드·표 안내·복사 버튼), `ListPending` 하나로 | |
| 브라우저 E2E: 풀 만들기 → 업로드 → VM → 콘솔 → VM·볼륨·풀 삭제 | |

## 이전 계획에서 이어받는 사항

계획 8의 "다음 계획으로 넘길 사항" 가운데 다음을 한다.
- 계획 9 범위 전체(볼륨 업로드·풀·네트워크·노드·사용자·토큰·클러스터, 내 비밀번호 변경, E2E 업로드 단계).
- `can()`에 계정 종류 규칙(operator·viewer는 계정을 읽지 못한다): Task 1.
- 같은 로딩·오류 표시 하나로: `ListPending`과 `Field`를 `src/components/`로 옮기고, 표의 빈 행은 `TableMessage` 하나로 한다(Task 1).

노드 아키텍처 API, 작업 목록 페이지 나누기, `index.html` ETag, VM 폼 컴포넌트 나누기는 다음 계획으로 남긴다.

## 실행 환경

계획 8과 같다: macOS(Apple Silicon), Rust, `brew install qemu`, Node.js 22.12 이상, pnpm 10.33.2, `pnpm -C web exec playwright install chromium`(한 번). `cargo test --workspace --all-features`는 웹 E2E도 켜므로 빌드한 `web/dist`, pnpm, Chromium이 있어야 한다. 이 계획은 API를 바꾸지 않으므로 `web/src/api/schema.d.ts`는 그대로다.

## 설계 결정

- **감시하는 것과 다시 읽는 것.** 볼륨·풀·네트워크·노드는 계획 8의 `useList`(watch 캐시)에서 읽으므로 만들거나 지우면 목록이 저절로 바뀐다. 사용자(`["users"]`)와 토큰(`["tokens"]`)은 watch에 없어 바꾼 뒤 `invalidateQueries`로 다시 읽는다. 목록 키(`["list", ...]`)는 계획 8대로 무효화하지 않는다.
- **대화상자 폼.** `FormDialog`는 연 쪽이 열 때만 그리고(`{open && <XDialog />}`) 닫으면 지운다. 그래서 입력이 매번 새로 시작한다. `onSubmit`이 입력 오류로 `false`를 돌려주면 열어 두고, 던지면 서버 오류를 폼 위에 번역해 보인다. 보내는 동안은 입력과 닫기를 막는다.
- **입력 검사.** 이름 규칙은 VM 폼의 `validName`을 함께 쓴다. 풀 경로는 절대 경로, 네트워크는 서버의 `valid_ifname`·VLAN 1–4094·예약 이름 `user`, 볼륨은 예약 이름 `import`와 같은 이름, 비밀번호는 8글자(바이트가 아니라 글자) 이상과 확인 입력을 보내기 전에 본다.
- **삭제 확인.** 볼륨 삭제와 노드 제거는 이름을 입력해야 한다(설계 §5). 풀·네트워크·토큰·사용자 삭제는 확인만 한다. 서버가 거절할 상태(풀에 볼륨이 남음, VM이 쓰는 네트워크·볼륨, 만드는 중인 볼륨, 온라인 노드, 자기 계정)는 버튼을 미리 막고, 그래도 거절되면 번역한 오류를 보인다.
- **이미지 업로드.** fetch는 올리는 진행률을 주지 않아 `XMLHttpRequest`로 `/volumes/import`에 파일을 그대로 보낸다(`application/octet-stream`, 브라우저가 `Content-Length`를 붙인다). 서버는 받은 노드에서 쓸 수 있는 풀로만 가져오므로 `GET /cluster`의 `node`로 풀을 거른다. 업로드는 모듈 상태(`startUpload`)라 화면을 옮겨도 계속되고, 진행률과 취소 버튼은 같은 알림에 보인다. 올리는 동안 `beforeunload`로 페이지 이탈을 확인한다. 401은 다른 요청처럼 보낼 때의 인증 세대와 함께 알린다(`reportUnauthorized`). 로그아웃하거나 세션이 끝나면 `resetUploads`가 조용히 멈춘다. 다 올리면 받은 Task를 `trackTask`로 넘긴다. 파일 이름에서 볼륨 이름과 원본 형식(확장자)을 제안한다.
- **역할.** `can()`이 서버처럼 계정 종류(`user`, `api-token`, `join-token`)를 admin에게만 허락한다. 사용자 메뉴는 admin에게만 보인다. 토큰은 누구나 자기 것을 만들고 지우므로(서버 규칙) 메뉴를 모두에게 보이고, 주인 열과 사용자 목록 읽기는 admin만 한다. 클러스터 화면은 모든 역할이 보고(API가 읽기를 허락하고 CA 지문은 인증서 확인에 필요하다) 조인 토큰 발급만 admin이다. 설계 §1 표의 "클러스터 (admin)"를 이렇게 고친다.
- **노드.** 유지보수 모드는 읽은 revision으로 `If-Match`를 붙여 spec 전체를 보낸다. 제거는 오프라인인 다른 노드만 고를 수 있다(살아 있는 노드는 그 노드에서 `oplctl cluster leave`).
- **내 비밀번호.** 서버는 자기 비밀번호를 바꿀 때 현재 비밀번호를 확인하고(틀리면 403 `invalid_credentials`) 그 사용자의 세션을 모두 끝낸다. 그래서 틀린 현재 비밀번호는 그 필드의 오류로 보이고, 바꾸면 로그아웃한 뒤 알린다(로그아웃이 알림을 모두 닫으므로 그 뒤에). 관리자도 자기 비밀번호는 사용자 화면이 아니라 여기서 바꾼다.
- **비밀값은 한 번만.** API 토큰의 비밀값과 조인 URL은 서버가 다시 주지 않으므로 만든 직후 대화상자에 한 번 보이고 복사 버튼을 둔다. 브라우저 저장소에 두지 않는다.
- **E2E.** Rust 테스트는 빈 디렉터리(풀 경로)와 16 MiB raw 이미지를 만들어 넘기고(`OPL_POOL_PATH`, `OPL_IMAGE`), 브라우저가 풀 만들기 → 업로드 → VM 만들기·시작 → 콘솔 → 강제 정지·삭제 → 볼륨·풀 삭제 → 로그아웃을 한다. 끝나면 Rust가 VM·볼륨·풀이 모두 없는지 본다.
- **기존 테스트의 경쟁 상태.** 계획 8의 대시보드 테스트가 부하가 크면 MSW 핸들러가 불리기 전에 응답을 끝내려 해 가끔 실패했다. 요청이 도착한 뒤에 응답하게 고친다(Task 1).

## 파일 구조

```
crates/opld/tests/web.rs                     # 수정: 풀 경로와 이미지를 넘기고, 모두 지웠는지 확인
README.md, docs/superpowers/specs/2026-10-23-web-ui-design.md   # 수정
web/
├── e2e/vm.spec.ts                           # 수정: 풀·업로드부터 정리까지
├── src/App.tsx, src/layout/AppLayout.tsx    # 수정: 화면 경로, 역할별 메뉴, 내 비밀번호 변경
├── src/api/client.ts                        # 수정: 업로드의 401 알림
├── src/auth/{permissions,AuthProvider}.ts(x)  # 수정: 계정 종류 규칙, 로그아웃하면 업로드 멈춤
├── src/components/  FormDialog, Field, ListPending, TableMessage, CopyButton   # 새로(Field·ListPending은 VM 폼에서 옮김)
├── src/storage/     PoolsPage, VolumesPage, volumes(이름 검사), UploadDialog, upload(XHR·알림)
├── src/networks/NetworksPage.tsx
├── src/nodes/NodesPage.tsx
├── src/accounts/    UsersPage, TokensPage, ChangePasswordDialog, PasswordFields, password
├── src/cluster/ClusterPage.tsx
├── src/i18n/{ko,en}.json                    # 수정: 화면 문구와 오류 code
├── src/lib/format.ts                        # 수정: formatBytes
└── src/test/        fixtures(풀·볼륨), fakeXhr(업로드 흉내)
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 계정 권한 규칙과 공통 부품

`can()`이 서버의 `RoleAuthorizer`처럼 계정 종류(`user`, `api-token`, `join-token`)를 admin에게만 허락한다. 뒤 화면들이 함께 쓰는 부품을 `src/components/`에 둔다: `FormDialog`(대화상자 폼: 입력 오류면 열어 두고, 서버 오류는 폼 위에, 보내는 동안 잠금), `Field`·`ListPending`(VM 폼에서 옮긴다. VM 상세도 `ListPending`을 쓴다), `TableMessage`(표의 불러오는 중·오류·빈 목록 한 줄, VM 목록도 쓴다), `CopyButton`. `formatBytes`를 더하고, VM 폼의 이름 규칙을 `validName`으로 내보낸다. 계획 8의 대시보드 테스트가 부하가 크면 요청이 핸들러에 닿기 전에 응답하려다 실패하던 경쟁 상태도 고친다.

**Files:**
- Create: `web/src/components/CopyButton.tsx`
- Create: `web/src/components/Field.tsx`
- Create: `web/src/components/FormDialog.test.tsx`
- Create: `web/src/components/FormDialog.tsx`
- Create: `web/src/components/ListPending.tsx`
- Create: `web/src/components/TableMessage.tsx`
- Modify: `web/src/auth/permissions.test.ts`
- Modify: `web/src/auth/permissions.ts`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/lib/format.test.ts`
- Modify: `web/src/lib/format.ts`
- Modify: `web/src/pages/DashboardPage.test.tsx`
- Modify: `web/src/vms/VmDetailPage.tsx`
- Modify: `web/src/vms/VmFormPage.tsx`
- Modify: `web/src/vms/VmListPage.tsx`
- Modify: `web/src/vms/form.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/auth/permissions.test.ts`에서

```ts
});
```

을 다음으로 바꾼다:

```ts
});

it("keeps accounts to admins", () => {
  expect(can("admin", "read", "user")).toBe(true);
  expect(can("admin", "write", "join-token")).toBe(true);
  for (const role of ["operator", "viewer"] as const) {
    expect(can(role, "read", "user")).toBe(false);
    expect(can(role, "read", "api-token")).toBe(false);
    expect(can(role, "write", "join-token")).toBe(false);
  }
});
```

`web/src/auth/permissions.ts`에서

```ts

/**
 * 서버의 `RoleAuthorizer`와 같은 규칙으로 버튼과 메뉴를 숨긴다. 실제 권한 확인은 서버가 한다.
 * operator는 VM과 볼륨만 바꾸고, viewer는 읽기만 한다.
 */
export function can(role: Role, action: Action, kind: WatchKind): boolean {
  switch (role) {
```

을 다음으로 바꾼다:

```ts

/** 권한을 따지는 리소스 종류. 이름은 서버의 `Kind`와 같다. */
export type Kind = WatchKind | "user" | "api-token" | "join-token";

/** 계정 종류. admin이 아니면 읽지도 못한다(자기 토큰과 비밀번호는 예외로 서버가 따로 허락한다). */
const ACCOUNT: ReadonlySet<Kind> = new Set(["user", "api-token", "join-token"]);

/**
 * 서버의 `RoleAuthorizer`와 같은 규칙으로 버튼과 메뉴를 숨긴다. 실제 권한 확인은 서버가 한다.
 * operator는 VM과 볼륨만 바꾸고, viewer는 읽기만 한다. 둘 다 계정은 읽지 못한다.
 */
export function can(role: Role, action: Action, kind: Kind): boolean {
  const account = ACCOUNT.has(kind);
  switch (role) {
```

`web/src/auth/permissions.ts`에서

```ts
    case "operator":
      return action === "read" || kind === "vm" || kind === "volume";
    case "viewer":
      return action === "read";
  }
```

을 다음으로 바꾼다:

```ts
    case "operator":
      return action === "read" ? !account : kind === "vm" || kind === "volume";
    case "viewer":
      return action === "read" && !account;
  }
```

`web/src/components/CopyButton.tsx`를 만든다:

```tsx
import { Copy } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";

/** 글을 클립보드에 복사한다. `label`은 무엇을 복사하는지(버튼의 이름) */
export function CopyButton({ text, label }: { text: string; label: string }) {
  const { t } = useTranslation();
  async function copy() {
    try {
      await navigator.clipboard.writeText(text);
      toast.success(t("common.copied"));
    } catch {
      toast.error(t("common.copyFailed"));
    }
  }
  return (
    <Button
      type="button"
      variant="outline"
      size="icon-sm"
      aria-label={label}
      onClick={() => void copy()}
    >
      <Copy />
    </Button>
  );
}
```

`web/src/components/Field.tsx`를 만든다:

```tsx
import { useTranslation } from "react-i18next";

import { Label } from "@/components/ui/label";

/** 입력에 붙일 접근성 속성. 오류가 있을 때만 값이 있다. */
export interface FieldA11y {
  "aria-invalid"?: true;
  "aria-describedby"?: string;
}

/**
 * 이름표와 입력, 그 아래의 오류(번역 키)와 도움말. 오류가 있는 입력은 `children`에 함수를 주어
 * `FieldA11y`를 받는다.
 */
export function Field({
  id,
  label,
  error,
  hint,
  children,
}: {
  id: string;
  label: string;
  error?: string;
  hint?: string;
  children: React.ReactNode | ((a11y: FieldA11y) => React.ReactNode);
}) {
  const { t } = useTranslation();
  const errorId = `${id}-error`;
  const a11y: FieldA11y = error ? { "aria-invalid": true, "aria-describedby": errorId } : {};
  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      {typeof children === "function" ? children(a11y) : children}
      {hint && !error && <p className="text-xs text-muted-foreground">{hint}</p>}
      {error && (
        <p id={errorId} className="text-xs text-destructive">
          {t(error)}
        </p>
      )}
    </div>
  );
}
```

`web/src/components/FormDialog.test.tsx`를 만든다:

```tsx
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ApiError } from "@/api/client";
import { FormDialog } from "@/components/FormDialog";

function setup(onSubmit: () => Promise<boolean | void>) {
  const onClose = vi.fn();
  render(
    <FormDialog title="Create" submitLabel="Save" onClose={onClose} onSubmit={onSubmit}>
      <input aria-label="Name" />
    </FormDialog>,
  );
  return onClose;
}

it("closes after a successful submit", async () => {
  const onClose = setup(async () => {});
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(onClose).toHaveBeenCalledOnce();
});

it("stays open on input errors and shows server errors", async () => {
  let fail = false;
  const onClose = setup(async () => {
    if (fail) {
      throw new ApiError(409, "name_taken", "exists");
    }
    return false;
  });
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(onClose).not.toHaveBeenCalled();

  fail = true;
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("That name is already in use.");
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByLabelText("Name")).toBeEnabled();
});
```

`web/src/components/FormDialog.tsx`를 만든다:

```tsx
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { errorMessage } from "@/i18n/errors";

/**
 * 대화상자 하나의 폼. 연 쪽은 열 때만 그리고(`{open && <FormDialog ... />}`) 닫으면 지우므로 입력이 매번
 * 새로 시작한다. 보내는 동안은 입력과 닫기를 막고, 실패하면 서버의 오류를 폼 위에 보여 준다.
 */
export function FormDialog({
  title,
  description,
  submitLabel,
  destructive = false,
  onClose,
  onSubmit,
  children,
}: {
  title: string;
  description?: string;
  submitLabel: string;
  destructive?: boolean;
  onClose(): void;
  /** 보낸다. 입력 오류로 보내지 않았으면 false를 돌려준다(열어 둔다). 실패하면 던진다. */
  onSubmit(): Promise<boolean | void>;
  children: React.ReactNode;
}) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      if ((await onSubmit()) !== false) {
        onClose();
      }
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent {...(description ? {} : { "aria-describedby": undefined })}>
        <form noValidate onSubmit={(e) => void submit(e)} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            {description && <DialogDescription>{description}</DialogDescription>}
          </DialogHeader>
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          <fieldset disabled={busy} className="flex min-w-0 flex-col gap-4">
            {children}
          </fieldset>
          <DialogFooter>
            <Button type="button" variant="outline" disabled={busy} onClick={onClose}>
              {t("confirm.cancel")}
            </Button>
            <Button type="submit" variant={destructive ? "destructive" : "default"} disabled={busy}>
              {submitLabel}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
```

`web/src/components/ListPending.tsx`를 만든다:

```tsx
import { useTranslation } from "react-i18next";

import { errorMessage } from "@/i18n/errors";

/** 목록을 읽는 중이거나 읽지 못했을 때 */
export function ListPending({ error }: { error: unknown }) {
  const { t } = useTranslation();
  return error ? (
    <p role="alert" className="text-destructive">
      {errorMessage(t, error)}
    </p>
  ) : (
    <p role="status" className="text-muted-foreground">
      {t("common.loading")}
    </p>
  );
}
```

`web/src/components/TableMessage.tsx`를 만든다:

```tsx
import { useTranslation } from "react-i18next";

import { TableCell, TableRow } from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";

/** 보일 행이 없는 표의 한 줄: 불러오는 중, 읽지 못한 이유, 빈 목록의 문장 */
export function TableMessage({
  colSpan,
  query,
  empty,
}: {
  colSpan: number;
  query: { isPending: boolean; isError: boolean; error: unknown };
  empty: string;
}) {
  const { t } = useTranslation();
  return (
    <TableRow>
      <TableCell colSpan={colSpan} className="text-center text-muted-foreground">
        {query.isPending
          ? t("common.loading")
          : query.isError
            ? errorMessage(t, query.error)
            : empty}
      </TableCell>
    </TableRow>
  );
}
```

`web/src/i18n/en.json`에서

```json
  "common": {
    "loading": "Loading…"
  },
```

을 다음으로 바꾼다:

```json
  "common": {
    "loading": "Loading…",
    "copied": "Copied.",
    "copyFailed": "Could not copy. Select the text and copy it yourself."
  },
```

`web/src/i18n/ko.json`에서

```json
  "common": {
    "loading": "불러오는 중…"
  },
```

을 다음으로 바꾼다:

```json
  "common": {
    "loading": "불러오는 중…",
    "copied": "복사했습니다.",
    "copyFailed": "복사하지 못했습니다. 글을 선택해 직접 복사하세요."
  },
```

`web/src/lib/format.test.ts`에서

```ts
import { formatCapacity, formatMib, formatTime, uuidTime } from "@/lib/format";
```

을 다음으로 바꾼다:

```ts
import { formatBytes, formatCapacity, formatMib, formatTime, uuidTime } from "@/lib/format";
```

`web/src/lib/format.test.ts`에서

```ts
  expect(formatTime(time, "ko")).toContain("2025");
});
```

을 다음으로 바꾼다:

```ts
  expect(formatTime(time, "ko")).toContain("2025");
});

it("formats byte counts", () => {
  expect(formatBytes(0)).toBe("0 B");
  expect(formatBytes(512 * 1024 ** 2)).toBe("512 MiB");
  expect(formatBytes(1.5 * 1024 ** 3)).toBe("1.5 GiB");
  expect(formatBytes(64 * 1024 ** 4)).toBe("64 TiB");
});
```

`web/src/lib/format.ts`에서

```ts

/** 노드 용량처럼 GiB로 나누어떨어지지 않는 크기: 1.5 GiB, 15.5 GiB */
```

을 다음으로 바꾼다:

```ts

/** 바이트 수를 1024 단위로 소수 한 자리까지: 512 MiB, 1.5 GiB */
export function formatBytes(bytes: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${+value.toFixed(1)} ${units[unit]}`;
}

/** 노드 용량처럼 GiB로 나누어떨어지지 않는 크기: 1.5 GiB, 15.5 GiB */
```

`web/src/pages/DashboardPage.test.tsx`에서

```tsx
it("shows loading instead of zeros, then errors", async () => {
  let fail = () => {};
  server.use(
```

을 다음으로 바꾼다:

```tsx
it("shows loading instead of zeros, then errors", async () => {
  let fail: (() => void) | undefined;
  server.use(
```

`web/src/pages/DashboardPage.test.tsx`에서

```tsx

  fail();
  expect(await within(nodes).findByText("You do not have permission to do this.")).toBeVisible();
```

을 다음으로 바꾼다:

```tsx

  // 요청이 핸들러에 닿은 뒤에 실패시킨다(부하가 크면 화면이 먼저 그려진다).
  await vi.waitFor(() => expect(fail).toBeDefined());
  fail!();
  expect(await within(nodes).findByText("You do not have permission to do this.")).toBeVisible();
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
import { can } from "@/auth/permissions";
import { Button } from "@/components/ui/button";
```

을 다음으로 바꾼다:

```tsx
import { can } from "@/auth/permissions";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { errorMessage } from "@/i18n/errors";
import { formatMib, formatTime, uuidTime } from "@/lib/format";
```

을 다음으로 바꾼다:

```tsx
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { formatMib, formatTime, uuidTime } from "@/lib/format";
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
  if (vm === undefined) {
    return vms.isError ? (
      <p role="alert" className="text-destructive">
        {errorMessage(t, vms.error)}
      </p>
    ) : (
      <p role="status" className="text-muted-foreground">
        {t("common.loading")}
      </p>
    );
  }
```

을 다음으로 바꾼다:

```tsx
  if (vm === undefined) {
    return <ListPending error={vms.error} />;
  }
```

`web/src/vms/VmFormPage.tsx`에서

```tsx
import type { Vm } from "@/api/types";
import { Button } from "@/components/ui/button";
```

을 다음으로 바꾼다:

```tsx
import type { Vm } from "@/api/types";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
```

`web/src/vms/VmFormPage.tsx`에서

```tsx

/** 목록을 읽는 중이거나 읽지 못했을 때 */
function ListPending({ error }: { error: unknown }) {
  const { t } = useTranslation();
  return error ? (
    <p role="alert" className="text-destructive">
      {errorMessage(t, error)}
    </p>
  ) : (
    <p role="status" className="text-muted-foreground">
      {t("common.loading")}
    </p>
  );
}

export function VmCreatePage() {
```

을 다음으로 바꾼다:

```tsx

export function VmCreatePage() {
```

`web/src/vms/VmFormPage.tsx`에서

```tsx
  return <VmForm key={vm.id} vm={vm} initial={fromVm(vm)} />;
}

/** 입력에 붙일 접근성 속성. 오류가 있을 때만 값이 있다. */
interface FieldA11y {
  "aria-invalid"?: true;
  "aria-describedby"?: string;
}

/** 이름표와 입력, 그 아래의 오류. 오류가 있는 입력은 `children`에 함수를 주어 `FieldA11y`를 받는다. */
function Field({
  id,
  label,
  error,
  children,
}: {
  id: string;
  label: string;
  error?: string;
  children: React.ReactNode | ((a11y: FieldA11y) => React.ReactNode);
}) {
  const { t } = useTranslation();
  const errorId = `${id}-error`;
  const a11y: FieldA11y = error ? { "aria-invalid": true, "aria-describedby": errorId } : {};
  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      {typeof children === "function" ? children(a11y) : children}
      {error && (
        <p id={errorId} className="text-xs text-destructive">
          {t(error)}
        </p>
      )}
    </div>
  );
}
```

을 다음으로 바꾼다:

```tsx
  return <VmForm key={vm.id} vm={vm} initial={fromVm(vm)} />;
}
```

`web/src/vms/VmListPage.tsx`에서

```tsx
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
```

을 다음으로 바꾼다:

```tsx
import { Checkbox } from "@/components/ui/checkbox";
import { TableMessage } from "@/components/TableMessage";
import { Input } from "@/components/ui/input";
```

`web/src/vms/VmListPage.tsx`에서

```tsx
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatMib } from "@/lib/format";
```

을 다음으로 바꾼다:

```tsx
} from "@/components/ui/table";
import { formatMib } from "@/lib/format";
```

`web/src/vms/VmListPage.tsx`에서

```tsx
          {table.getRowModel().rows.length === 0 && (
            <TableRow>
              <TableCell colSpan={columns.length} className="text-center text-muted-foreground">
                {vms.isPending
                  ? t("common.loading")
                  : vms.isError
                    ? errorMessage(t, vms.error)
                    : t("vms.empty")}
              </TableCell>
            </TableRow>
          )}
```

을 다음으로 바꾼다:

```tsx
          {table.getRowModel().rows.length === 0 && (
            <TableMessage colSpan={columns.length} query={vms} empty={t("vms.empty")} />
          )}
```

`web/src/vms/form.ts`에서

```ts

/** `min`부터 `max`까지의 정수인지 */
```

을 다음으로 바꾼다:

```ts

/** 서버의 리소스 이름 규칙에 맞는지. id(UUID)처럼 보이는 이름은 id와 헷갈리므로 받지 않는다. */
export function validName(name: string): boolean {
  return NAME.test(name) && !UUID.test(name);
}

/** `min`부터 `max`까지의 정수인지 */
```

`web/src/vms/form.ts`에서

```ts
  const errors: Record<string, string> = {};
  const nameOk = (name: string) => NAME.test(name) && !UUID.test(name);
  if (creating && !nameOk(v.name)) {
    errors.name = "vmForm.errors.name";
```

을 다음으로 바꾼다:

```ts
  const errors: Record<string, string> = {};
  if (creating && !validName(v.name)) {
    errors.name = "vmForm.errors.name";
```

`web/src/vms/form.ts`에서

```ts
      errors[`disks.${i}.volume`] = "vmForm.errors.volumeChoose";
    } else if (!nameOk(d.volume)) {
      errors[`disks.${i}.volume`] = "vmForm.errors.volume";
```

을 다음으로 바꾼다:

```ts
      errors[`disks.${i}.volume`] = "vmForm.errors.volumeChoose";
    } else if (!validName(d.volume)) {
      errors[`disks.${i}.volume`] = "vmForm.errors.volume";
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 97개 (`keeps accounts to admins`, `stays open on input errors and shows server errors`, `formats byte counts` 포함)

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 계정 권한 규칙과 공통 부품 (폼 대화상자, 필드, 표 안내, 복사 버튼)"
```

---

### Task 2: 스토리지 풀 화면

`/storage-pools`: 이름·디렉터리·노드(비우면 모든 노드)·남은 공간·볼륨 수. admin은 만들기(이름, 절대 경로, 쓸 노드 선택)와 삭제(확인)를 한다. 볼륨이 남은 풀의 삭제 버튼은 막는다. 왼쪽 메뉴와 번역을 더하고, 테스트용 풀·볼륨 fixture를 둔다.

**Files:**
- Create: `web/src/storage/PoolsPage.test.tsx`
- Create: `web/src/storage/PoolsPage.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.tsx`
- Modify: `web/src/test/fixtures.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.tsx`에서

```tsx
import { NotFoundPage } from "@/pages/NotFoundPage";
import { TasksPage } from "@/tasks/TasksPage";
```

을 다음으로 바꾼다:

```tsx
import { NotFoundPage } from "@/pages/NotFoundPage";
import { PoolsPage } from "@/storage/PoolsPage";
import { TasksPage } from "@/tasks/TasksPage";
```

`web/src/App.tsx`에서

```tsx
            { path: "vms/:name/console", element: <ConsolePage /> },
            { path: "tasks", element: <TasksPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "vms/:name/console", element: <ConsolePage /> },
            { path: "storage-pools", element: <PoolsPage /> },
            { path: "tasks", element: <TasksPage /> },
```

`web/src/i18n/en.json`에서

```json
    "vms": "Virtual machines",
    "tasks": "Tasks"
```

을 다음으로 바꾼다:

```json
    "vms": "Virtual machines",
    "pools": "Storage pools",
    "tasks": "Tasks"
```

`web/src/i18n/en.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "forms": {
    "name": "Use 1–63 lowercase letters, digits and -, not starting or ending with -."
  },
  "pools": {
    "name": "Name",
    "path": "Directory",
    "nodes": "Nodes",
    "free": "Free / total",
    "volumes": "Volumes",
    "empty": "No storage pools.",
    "allNodes": "All nodes",
    "create": "Create",
    "createTitle": "Create a storage pool",
    "createDescription": "A directory on the nodes that holds volume files. Create the directory on each node first.",
    "pathHint": "An absolute path on the nodes.",
    "nodesHint": "Leave all unchecked to use it on every node.",
    "created": "Created storage pool {{name}}.",
    "delete": "Delete",
    "deleteNamed": "Delete {{name}}",
    "deleteTitle": "Delete storage pool {{name}}?",
    "deleteDescription": "Only the pool is removed. Files in its directory stay on the nodes.",
    "deleted": "Deleted storage pool {{name}}.",
    "inUse": "Delete its volumes first.",
    "errors": {
      "path": "Enter an absolute path (starting with /)."
    }
  },
  "vmForm": {
```

`web/src/i18n/ko.json`에서

```json
    "vms": "가상 머신",
    "tasks": "작업"
```

을 다음으로 바꾼다:

```json
    "vms": "가상 머신",
    "pools": "스토리지 풀",
    "tasks": "작업"
```

`web/src/i18n/ko.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "forms": {
    "name": "소문자·숫자·-로 1–63자, -로 시작하거나 끝날 수 없습니다."
  },
  "pools": {
    "name": "이름",
    "path": "디렉터리",
    "nodes": "노드",
    "free": "남은 공간 / 전체",
    "volumes": "볼륨",
    "empty": "스토리지 풀이 없습니다.",
    "allNodes": "모든 노드",
    "create": "만들기",
    "createTitle": "스토리지 풀 만들기",
    "createDescription": "볼륨 파일을 두는 노드의 디렉터리입니다. 먼저 각 노드에 디렉터리를 만드세요.",
    "pathHint": "노드의 절대 경로입니다.",
    "nodesHint": "모두 비워 두면 모든 노드에서 씁니다.",
    "created": "스토리지 풀 {{name}}을(를) 만들었습니다.",
    "delete": "삭제",
    "deleteNamed": "{{name}} 삭제",
    "deleteTitle": "스토리지 풀 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "풀만 지웁니다. 디렉터리의 파일은 노드에 남습니다.",
    "deleted": "스토리지 풀 {{name}}을(를) 삭제했습니다.",
    "inUse": "먼저 볼륨을 지우세요.",
    "errors": {
      "path": "절대 경로(/로 시작)를 입력하세요."
    }
  },
  "vmForm": {
```

`web/src/layout/AppLayout.tsx`에서

```tsx
import {
  Languages,
```

을 다음으로 바꾼다:

```tsx
import {
  Database,
  Languages,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/vms", key: "nav.vms", icon: Server, end: false },
  { to: "/tasks", key: "nav.tasks", icon: ListChecks, end: false },
```

을 다음으로 바꾼다:

```tsx
  { to: "/vms", key: "nav.vms", icon: Server, end: false },
  { to: "/storage-pools", key: "nav.pools", icon: Database, end: false },
  { to: "/tasks", key: "nav.tasks", icon: ListChecks, end: false },
```

`web/src/storage/PoolsPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { PoolsPage } from "@/storage/PoolsPage";
import { NODE, pool, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

it("lists pools with their nodes, space and volumes", async () => {
  mockLists({
    "storage-pool": [pool("local"), pool("busy", { nodes: ["n1"] })],
    node: [NODE],
    volume: [volume("disk", "busy")],
  });
  renderPage(<PoolsPage />);
  const table = await screen.findByRole("table", { name: "Storage pools" });
  const local = (await within(table).findByText("local")).closest("tr")!;
  expect(local).toHaveTextContent("/srv/local");
  expect(local).toHaveTextContent("All nodes");
  expect(local).toHaveTextContent("64 GiB / 128 GiB");
  const busy = within(table).getByText("busy").closest("tr")!;
  expect(busy).toHaveTextContent("node-a");
  expect(within(busy).getByRole("button", { name: "Delete busy" })).toBeDisabled();
});

it("creates a pool on the chosen nodes", async () => {
  mockLists({ node: [NODE, { ...NODE, id: "n2", name: "node-b" }] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("fast"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "fast");
  await userEvent.type(within(dialog).getByLabelText("Directory"), "srv/fast");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("Enter an absolute path (starting with /).")).toBeVisible();
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("Directory"));
  await userEvent.type(within(dialog).getByLabelText("Directory"), "/srv/fast");
  await userEvent.click(await within(dialog).findByRole("checkbox", { name: "node-b" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created storage pool fast.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "fast",
    spec: { backend: { kind: "dir", path: "/srv/fast" }, nodes: ["n2"] },
  });
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("deletes an unused pool after confirming", async () => {
  const local = pool("local");
  mockLists({ "storage-pool": [local] });
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/storage-pools/${local.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete local" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Deleted storage pool local.")).toBeInTheDocument();
  expect(deleted).toBe(true);
});

it("hides changes from operators", async () => {
  mockLists({ "storage-pool": [pool("local")] });
  renderPage(<PoolsPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByText("local")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Delete local" })).not.toBeInTheDocument();
});
```

`web/src/storage/PoolsPage.tsx`를 만든다:

```tsx
import { Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { StoragePool } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatBytes } from "@/lib/format";
import { validName } from "@/vms/form";

export function PoolsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const nodes = useList("node");
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<StoragePool | null>(null);
  const write = can(me.role, "write", "storage-pool");

  const nodeNames = (pool: StoragePool) => {
    const ids = pool.spec.nodes ?? [];
    // 노드를 정하지 않은 풀은 모든 노드에서 쓴다.
    return ids.length === 0
      ? t("pools.allNodes")
      : ids.map((id) => nodes.data?.find((n) => n.id === id)?.name ?? id.slice(-8)).join(", ");
  };
  const volumeCount = (pool: StoragePool) =>
    volumes.data?.filter((v) => v.spec.pool === pool.name).length ?? 0;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="pools-title" className="text-2xl font-semibold">
          {t("nav.pools")}
        </h1>
        {write && (
          <Button onClick={() => setCreating(true)}>
            <Plus />
            {t("pools.create")}
          </Button>
        )}
      </div>
      <Table aria-labelledby="pools-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("pools.name")}</TableHead>
            <TableHead>{t("pools.path")}</TableHead>
            <TableHead>{t("pools.nodes")}</TableHead>
            <TableHead>{t("pools.free")}</TableHead>
            <TableHead>{t("pools.volumes")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(pools.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 6 : 5} query={pools} empty={t("pools.empty")} />
          )}
          {(pools.data ?? []).map((pool) => {
            const { available_bytes: free, total_bytes: total } = pool.status;
            const used = volumeCount(pool);
            return (
              <TableRow key={pool.id}>
                <TableCell className="font-medium">{pool.name}</TableCell>
                <TableCell className="font-mono text-xs">{pool.spec.backend.path}</TableCell>
                <TableCell>{nodeNames(pool)}</TableCell>
                <TableCell>
                  {free !== null && total !== null
                    ? `${formatBytes(free)} / ${formatBytes(total)}`
                    : "—"}
                </TableCell>
                <TableCell>{used}</TableCell>
                {write && (
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("pools.deleteNamed", { name: pool.name })}
                      // 볼륨이 남은 풀은 서버가 지우지 않는다.
                      disabled={used > 0}
                      title={used > 0 ? t("pools.inUse") : undefined}
                      onClick={() => setDeleting(pool)}
                    >
                      <Trash2 />
                    </Button>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && <CreatePoolDialog onClose={() => setCreating(false)} />}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("pools.deleteTitle", { name: deleting?.name })}
        description={t("pools.deleteDescription")}
        confirmLabel={t("pools.delete")}
        onConfirm={async () => {
          const pool = deleting!;
          await call(api.DELETE("/storage-pools/{key}", { params: { path: { key: pool.id } } }));
          toast.success(t("pools.deleted", { name: pool.name }));
        }}
      />
    </div>
  );
}

function CreatePoolDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const nodes = useList("node");
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [chosen, setChosen] = useState<string[]>([]);
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    if (!validName(name)) {
      found.name = "forms.name";
    }
    if (!path.startsWith("/")) {
      found.path = "pools.errors.path";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    await call(
      api.POST("/storage-pools", {
        body: { name, spec: { backend: { kind: "dir", path }, nodes: chosen } },
      }),
    );
    toast.success(t("pools.created", { name }));
  }

  return (
    <FormDialog
      title={t("pools.createTitle")}
      description={t("pools.createDescription")}
      submitLabel={t("pools.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="pool-name" label={t("pools.name")} error={errors.name}>
        {(a11y) => (
          <Input id="pool-name" value={name} onChange={(e) => setName(e.target.value)} {...a11y} />
        )}
      </Field>
      <Field id="pool-path" label={t("pools.path")} error={errors.path} hint={t("pools.pathHint")}>
        {(a11y) => (
          <Input
            id="pool-path"
            placeholder="/var/lib/opl/pool"
            value={path}
            onChange={(e) => setPath(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-2 text-sm font-medium">{t("pools.nodes")}</legend>
        {(nodes.data ?? []).map((node) => (
          <div key={node.id} className="flex items-center gap-2">
            <Checkbox
              id={`pool-node-${node.id}`}
              checked={chosen.includes(node.id)}
              onCheckedChange={(v) =>
                setChosen((c) => (v === true ? [...c, node.id] : c.filter((id) => id !== node.id)))
              }
            />
            <Label htmlFor={`pool-node-${node.id}`}>{node.name}</Label>
          </div>
        ))}
        <p className="text-xs text-muted-foreground">{t("pools.nodesHint")}</p>
      </fieldset>
    </FormDialog>
  );
}
```

`web/src/test/fixtures.ts`에서

```ts
import type { Node, Task, Vm } from "@/api/types";
```

을 다음으로 바꾼다:

```ts
import type { Node, StoragePool, Task, Vm, Volume } from "@/api/types";
```

`web/src/test/fixtures.ts`에서

```ts
  } as Task;
}
```

을 다음으로 바꾼다:

```ts
  } as Task;
}

export function pool(name: string, extra: Partial<StoragePool["spec"]> = {}): StoragePool {
  return {
    id: `019a0f3c-0000-7000-8001-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    labels: {},
    spec: { backend: { kind: "dir", path: `/srv/${name}` }, nodes: [], ...extra },
    status: { available_bytes: 64 * 1024 ** 3, total_bytes: 128 * 1024 ** 3 },
  };
}

export function volume(name: string, pool: string, extra: Partial<Volume["status"]> = {}): Volume {
  return {
    id: `019a0f3c-0000-7000-8002-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    labels: {},
    spec: { pool, size_bytes: 16 * 1024 ** 3, format: "qcow2" },
    status: { phase: "ready", node: "n1", allocated_bytes: 1024 ** 3, attached_to: null, ...extra },
  };
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 101개 (`PoolsPage.test.tsx`의 4개 포함)

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 스토리지 풀 화면 (목록·만들기·삭제)"
```

---

### Task 3: 네트워크 화면

`/networks`: 이름·브리지·VLAN, 그 브리지가 있는 노드(없으면 경고, VLAN을 쓰는데 VLAN 필터링이 꺼진 노드도 경고), 쓰는 VM 수. admin은 만들기(노드가 알린 브리지를 제안, 서버의 인터페이스 이름 규칙·VLAN 1–4094·예약 이름 `user` 검사)와 삭제를 한다. `network_in_use`·`pool_in_use` 오류 번역을 더한다.

**Files:**
- Create: `web/src/networks/NetworksPage.test.tsx`
- Create: `web/src/networks/NetworksPage.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.tsx`에서

```tsx
import { LoginPage } from "@/pages/LoginPage";
import { NotFoundPage } from "@/pages/NotFoundPage";
```

을 다음으로 바꾼다:

```tsx
import { LoginPage } from "@/pages/LoginPage";
import { NetworksPage } from "@/networks/NetworksPage";
import { NotFoundPage } from "@/pages/NotFoundPage";
```

`web/src/App.tsx`에서

```tsx
            { path: "storage-pools", element: <PoolsPage /> },
            { path: "tasks", element: <TasksPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "storage-pools", element: <PoolsPage /> },
            { path: "networks", element: <NetworksPage /> },
            { path: "tasks", element: <TasksPage /> },
```

`web/src/i18n/en.json`에서

```json
    "not_leader": "The cluster leader is changing. Try again shortly.",
    "internal": "The server failed: {{message}}"
```

을 다음으로 바꾼다:

```json
    "not_leader": "The cluster leader is changing. Try again shortly.",
    "network_in_use": "A VM uses this network. Remove its interface first.",
    "pool_in_use": "The pool still has volumes. Delete them first.",
    "internal": "The server failed: {{message}}"
```

`web/src/i18n/en.json`에서

```json
    "pools": "Storage pools",
    "tasks": "Tasks"
```

을 다음으로 바꾼다:

```json
    "pools": "Storage pools",
    "networks": "Networks",
    "tasks": "Tasks"
```

`web/src/i18n/en.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "networks": {
    "name": "Name",
    "bridge": "Bridge",
    "vlan": "VLAN",
    "vlanOptional": "VLAN tag (optional)",
    "vlanHint": "1–4094. The bridge must be VLAN-aware (vlan_filtering 1).",
    "nodes": "Nodes with the bridge",
    "vms": "VMs",
    "empty": "No networks.",
    "userNote": "The user network (QEMU NAT) is always available and is not listed.",
    "noNode": "No node has this bridge",
    "noVlanFiltering": "VLAN filtering is off on {{nodes}}",
    "create": "Create",
    "createTitle": "Create a network",
    "createDescription": "Points VM interfaces at a Linux bridge that already exists on the nodes. VMs on this network run only on nodes with the bridge.",
    "created": "Created network {{name}}.",
    "delete": "Delete",
    "deleteNamed": "Delete {{name}}",
    "deleteTitle": "Delete network {{name}}?",
    "deleteDescription": "Only the network is removed. The bridges on the nodes are not changed.",
    "deleted": "Deleted network {{name}}.",
    "inUse": "VMs use this network.",
    "errors": {
      "reserved": "The name user is reserved for NAT.",
      "bridge": "Use 1–15 characters without /, : or spaces.",
      "vlan": "Use a number from 1 to 4094."
    }
  },
  "vmForm": {
```

`web/src/i18n/ko.json`에서

```json
    "not_leader": "클러스터 리더가 바뀌는 중입니다. 잠시 뒤 다시 시도하세요.",
    "internal": "서버 오류: {{message}}"
```

을 다음으로 바꾼다:

```json
    "not_leader": "클러스터 리더가 바뀌는 중입니다. 잠시 뒤 다시 시도하세요.",
    "network_in_use": "이 네트워크를 쓰는 VM이 있습니다. 먼저 그 인터페이스를 빼세요.",
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
    "internal": "서버 오류: {{message}}"
```

`web/src/i18n/ko.json`에서

```json
    "pools": "스토리지 풀",
    "tasks": "작업"
```

을 다음으로 바꾼다:

```json
    "pools": "스토리지 풀",
    "networks": "네트워크",
    "tasks": "작업"
```

`web/src/i18n/ko.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "networks": {
    "name": "이름",
    "bridge": "브리지",
    "vlan": "VLAN",
    "vlanOptional": "VLAN 태그(선택)",
    "vlanHint": "1–4094. 브리지가 VLAN을 알아야 합니다(vlan_filtering 1).",
    "nodes": "브리지가 있는 노드",
    "vms": "VM",
    "empty": "네트워크가 없습니다.",
    "userNote": "user 네트워크(QEMU NAT)는 늘 쓸 수 있으며 목록에 나오지 않습니다.",
    "noNode": "이 브리지가 있는 노드가 없습니다",
    "noVlanFiltering": "{{nodes}}에서 VLAN 필터링이 꺼져 있습니다",
    "create": "만들기",
    "createTitle": "네트워크 만들기",
    "createDescription": "노드에 이미 있는 Linux 브리지에 VM 인터페이스를 연결합니다. 이 네트워크의 VM은 그 브리지가 있는 노드에서만 실행됩니다.",
    "created": "네트워크 {{name}}을(를) 만들었습니다.",
    "delete": "삭제",
    "deleteNamed": "{{name}} 삭제",
    "deleteTitle": "네트워크 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "네트워크만 지웁니다. 노드의 브리지는 바꾸지 않습니다.",
    "deleted": "네트워크 {{name}}을(를) 삭제했습니다.",
    "inUse": "이 네트워크를 쓰는 VM이 있습니다.",
    "errors": {
      "reserved": "user는 NAT에 예약된 이름입니다.",
      "bridge": "/, :, 공백 없이 1–15자로 쓰세요.",
      "vlan": "1부터 4094까지의 숫자를 쓰세요."
    }
  },
  "vmForm": {
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  Moon,
  Server,
```

을 다음으로 바꾼다:

```tsx
  Moon,
  Network,
  Server,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/storage-pools", key: "nav.pools", icon: Database, end: false },
  { to: "/tasks", key: "nav.tasks", icon: ListChecks, end: false },
```

을 다음으로 바꾼다:

```tsx
  { to: "/storage-pools", key: "nav.pools", icon: Database, end: false },
  { to: "/networks", key: "nav.networks", icon: Network, end: false },
  { to: "/tasks", key: "nav.tasks", icon: ListChecks, end: false },
```

`web/src/networks/NetworksPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Network } from "@/api/types";
import { NetworksPage, validBridge } from "@/networks/NetworksPage";
import { NODE, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

function network(name: string, bridge: string, vlan: number | null = null): Network {
  return {
    id: `019a0f3c-0000-7000-8003-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    labels: {},
    spec: { bridge, vlan },
    status: {},
  };
}

const bridged = {
  ...NODE,
  status: { ...NODE.status, bridges: [{ name: "vmbr0", vlan_filtering: false }] },
};

it("follows the server's bridge name rule", () => {
  expect(validBridge("vmbr0")).toBe(true);
  expect(validBridge("br.100")).toBe(true);
  expect(validBridge("")).toBe(false);
  expect(validBridge("a".repeat(16))).toBe(false);
  expect(validBridge("br 0")).toBe(false);
  expect(validBridge("br:0")).toBe(false);
  expect(validBridge("..")).toBe(false);
});

it("shows which nodes have each bridge and which VMs use it", async () => {
  const web = vm("web", "running", {
    spec: { ...vm("web", "running").spec, nics: [{ network: "lan", model: "virtio" }] },
  });
  mockLists({
    network: [network("lan", "vmbr0"), network("dmz", "vmbr0", 20), network("lab", "br9")],
    node: [bridged],
    vm: [web],
  });
  renderPage(<NetworksPage />);
  const table = await screen.findByRole("table", { name: "Networks" });
  const lan = (await within(table).findByText("lan")).closest("tr")!;
  expect(lan).toHaveTextContent("node-a");
  expect(within(lan).getByRole("button", { name: "Delete lan" })).toBeDisabled();
  const dmz = within(table).getByText("dmz").closest("tr")!;
  expect(dmz).toHaveTextContent("VLAN filtering is off on node-a");
  const lab = within(table).getByText("lab").closest("tr")!;
  expect(lab).toHaveTextContent("No node has this bridge");
  expect(within(lab).getByRole("button", { name: "Delete lab" })).toBeEnabled();
});

it("creates a network after checking its fields", async () => {
  mockLists({ node: [bridged] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/networks"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(network("lan", "vmbr0", 10), { status: 201 });
    }),
  );
  renderPage(<NetworksPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a network" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "user");
  await userEvent.type(within(dialog).getByLabelText("VLAN tag (optional)"), "5000");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("The name user is reserved for NAT.")).toBeVisible();
  expect(within(dialog).getByText("Use 1–15 characters without /, : or spaces.")).toBeVisible();
  expect(within(dialog).getByText("Use a number from 1 to 4094.")).toBeVisible();

  await userEvent.clear(within(dialog).getByLabelText("Name"));
  await userEvent.type(within(dialog).getByLabelText("Name"), "lan");
  await userEvent.type(within(dialog).getByLabelText("Bridge"), "vmbr0");
  await userEvent.clear(within(dialog).getByLabelText("VLAN tag (optional)"));
  await userEvent.type(within(dialog).getByLabelText("VLAN tag (optional)"), "10");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created network lan.")).toBeInTheDocument();
  expect(body).toEqual({ name: "lan", spec: { bridge: "vmbr0", vlan: 10 } });
});

it("deletes an unused network and shows why a delete failed", async () => {
  const lab = network("lab", "br9");
  mockLists({ network: [lab] });
  server.use(
    http.delete(apiUrl(`/networks/${lab.id}`), () =>
      HttpResponse.json(
        { code: "network_in_use", message: 'network "lab" is used by VM "x"' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<NetworksPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete lab" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "A VM uses this network. Remove its interface first.",
  );
});

it("hides changes from operators", async () => {
  mockLists({ network: [network("lan", "vmbr0")] });
  renderPage(<NetworksPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByText("lan")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
});
```

`web/src/networks/NetworksPage.tsx`를 만든다:

```tsx
import { Plus, Trash2 } from "lucide-react";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Network, Node } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { validName } from "@/vms/form";

/** QEMU user 모드 NAT. 서버가 이 이름을 예약해 둔다. */
const USER_NETWORK = "user";

/** 서버의 `valid_ifname`: 1–15바이트의 보이는 ASCII, `/`·`:` 없음, `.`·`..`가 아님 */
export function validBridge(name: string): boolean {
  return /^[\x21-\x7e]{1,15}$/.test(name) && !/[/:]/.test(name) && name !== "." && name !== "..";
}

/** 그 브리지가 있는 노드와, VLAN을 쓰는데 VLAN 필터링이 꺼진 노드 */
export function bridgeNodes(network: Network, nodes: Node[]) {
  const having = nodes.filter((n) => n.status.bridges.some((b) => b.name === network.spec.bridge));
  const noVlan =
    network.spec.vlan == null
      ? []
      : having.filter(
          (n) => !n.status.bridges.find((b) => b.name === network.spec.bridge)?.vlan_filtering,
        );
  return { having, noVlan };
}

export function NetworksPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const networks = useList("network");
  const nodes = useList("node");
  const vms = useList("vm");
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<Network | null>(null);
  const write = can(me.role, "write", "network");
  const users = (network: Network) =>
    vms.data?.filter((vm) => vm.spec.nics?.some((nic) => nic.network === network.name)).length ?? 0;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="networks-title" className="text-2xl font-semibold">
          {t("nav.networks")}
        </h1>
        {write && (
          <Button onClick={() => setCreating(true)}>
            <Plus />
            {t("networks.create")}
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">{t("networks.userNote")}</p>
      <Table aria-labelledby="networks-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("networks.name")}</TableHead>
            <TableHead>{t("networks.bridge")}</TableHead>
            <TableHead>{t("networks.vlan")}</TableHead>
            <TableHead>{t("networks.nodes")}</TableHead>
            <TableHead>{t("networks.vms")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(networks.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 6 : 5} query={networks} empty={t("networks.empty")} />
          )}
          {(networks.data ?? []).map((network) => {
            const { having, noVlan } = bridgeNodes(network, nodes.data ?? []);
            const used = users(network);
            return (
              <TableRow key={network.id}>
                <TableCell className="font-medium">{network.name}</TableCell>
                <TableCell className="font-mono text-xs">{network.spec.bridge}</TableCell>
                <TableCell>{network.spec.vlan ?? "—"}</TableCell>
                <TableCell>
                  {having.length === 0 ? (
                    <span className="text-destructive">{t("networks.noNode")}</span>
                  ) : (
                    having.map((n) => n.name).join(", ")
                  )}
                  {noVlan.length > 0 && (
                    <div className="text-xs text-destructive">
                      {t("networks.noVlanFiltering", {
                        nodes: noVlan.map((n) => n.name).join(", "),
                      })}
                    </div>
                  )}
                </TableCell>
                <TableCell>{used}</TableCell>
                {write && (
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("networks.deleteNamed", { name: network.name })}
                      // VM이 쓰는 네트워크는 서버가 지우지 않는다.
                      disabled={used > 0}
                      title={used > 0 ? t("networks.inUse") : undefined}
                      onClick={() => setDeleting(network)}
                    >
                      <Trash2 />
                    </Button>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && <CreateNetworkDialog onClose={() => setCreating(false)} />}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("networks.deleteTitle", { name: deleting?.name })}
        description={t("networks.deleteDescription")}
        confirmLabel={t("networks.delete")}
        onConfirm={async () => {
          const network = deleting!;
          await call(api.DELETE("/networks/{key}", { params: { path: { key: network.id } } }));
          toast.success(t("networks.deleted", { name: network.name }));
        }}
      />
    </div>
  );
}

function CreateNetworkDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const nodes = useList("node");
  const bridgesId = useId();
  const [name, setName] = useState("");
  const [bridge, setBridge] = useState("");
  const [vlan, setVlan] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  // 노드가 알린 브리지를 제안한다. 아직 브리지를 만들지 않은 노드도 있으므로 직접 쓸 수도 있다.
  const known = [
    ...new Set((nodes.data ?? []).flatMap((n) => n.status.bridges.map((b) => b.name))),
  ].sort();

  async function submit() {
    const found: Record<string, string> = {};
    if (name === USER_NETWORK) {
      found.name = "networks.errors.reserved";
    } else if (!validName(name)) {
      found.name = "forms.name";
    }
    if (!validBridge(bridge)) {
      found.bridge = "networks.errors.bridge";
    }
    const tag = Number(vlan);
    if (vlan !== "" && !(Number.isInteger(tag) && tag >= 1 && tag <= 4094)) {
      found.vlan = "networks.errors.vlan";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    await call(
      api.POST("/networks", {
        body: { name, spec: { bridge, vlan: vlan === "" ? null : tag } },
      }),
    );
    toast.success(t("networks.created", { name }));
  }

  return (
    <FormDialog
      title={t("networks.createTitle")}
      description={t("networks.createDescription")}
      submitLabel={t("networks.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="network-name" label={t("networks.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="network-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field id="network-bridge" label={t("networks.bridge")} error={errors.bridge}>
        {(a11y) => (
          <Input
            id="network-bridge"
            list={bridgesId}
            placeholder="vmbr0"
            value={bridge}
            onChange={(e) => setBridge(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <datalist id={bridgesId}>
        {known.map((b) => (
          <option key={b} value={b} />
        ))}
      </datalist>
      <Field
        id="network-vlan"
        label={t("networks.vlanOptional")}
        error={errors.vlan}
        hint={t("networks.vlanHint")}
      >
        {(a11y) => (
          <Input
            id="network-vlan"
            inputMode="numeric"
            value={vlan}
            onChange={(e) => setVlan(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 106개 (`NetworksPage.test.tsx`의 5개 포함)

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 네트워크 화면 (브리지가 있는 노드, 만들기·삭제)"
```

---

### Task 4: 노드 화면

`/nodes`: 상태(온라인, 유지보수)·API 주소·하이퍼바이저·CPU·메모리·브리지·VM 수, 지금 접속한 노드 표시. admin의 행 메뉴는 유지보수 시작·끝내기(읽은 revision으로 `If-Match`)와 클러스터에서 제거(오프라인인 다른 노드만, 이름 입력)다. 노드를 더하는 방법(조인 토큰과 `oplctl cluster join`)을 안내한다.

**Files:**
- Create: `web/src/nodes/NodesPage.test.tsx`
- Create: `web/src/nodes/NodesPage.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.tsx`에서

```tsx
import { NetworksPage } from "@/networks/NetworksPage";
import { NotFoundPage } from "@/pages/NotFoundPage";
```

을 다음으로 바꾼다:

```tsx
import { NetworksPage } from "@/networks/NetworksPage";
import { NodesPage } from "@/nodes/NodesPage";
import { NotFoundPage } from "@/pages/NotFoundPage";
```

`web/src/App.tsx`에서

```tsx
            { path: "networks", element: <NetworksPage /> },
            { path: "tasks", element: <TasksPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "networks", element: <NetworksPage /> },
            { path: "nodes", element: <NodesPage /> },
            { path: "tasks", element: <TasksPage /> },
```

`web/src/i18n/en.json`에서

```json
    "network_in_use": "A VM uses this network. Remove its interface first.",
    "pool_in_use": "The pool still has volumes. Delete them first.",
```

을 다음으로 바꾼다:

```json
    "network_in_use": "A VM uses this network. Remove its interface first.",
    "node_online": "The node is online. Run oplctl cluster leave on it instead.",
    "pool_in_use": "The pool still has volumes. Delete them first.",
```

`web/src/i18n/en.json`에서

```json
    "networks": "Networks",
    "tasks": "Tasks"
```

을 다음으로 바꾼다:

```json
    "networks": "Networks",
    "nodes": "Nodes",
    "tasks": "Tasks"
```

`web/src/i18n/en.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "nodes": {
    "name": "Name",
    "state": "State",
    "address": "API address",
    "hypervisor": "Hypervisor",
    "cpus": "CPUs",
    "memory": "Memory",
    "bridges": "Bridges",
    "vms": "VMs",
    "thisNode": "(this node)",
    "joinNote": "To add a node, issue a join token on the Cluster page and run oplctl cluster join on the new node.",
    "maintenanceNote": "No new VMs are placed on a node in maintenance. VMs already running there keep running.",
    "startMaintenance": "Enter maintenance",
    "endMaintenance": "End maintenance",
    "maintenanceOn": "{{name}} is in maintenance.",
    "maintenanceOff": "{{name}} is out of maintenance.",
    "remove": "Remove from cluster",
    "removeTitle": "Remove {{name}} from the cluster?",
    "removeDescription": "Only for a node that is dead for good. It cannot rejoin with its current data, and VMs assigned to it stay assigned.",
    "removed": "Removed {{name}} from the cluster."
  },
  "vmForm": {
```

`web/src/i18n/ko.json`에서

```json
    "network_in_use": "이 네트워크를 쓰는 VM이 있습니다. 먼저 그 인터페이스를 빼세요.",
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
```

을 다음으로 바꾼다:

```json
    "network_in_use": "이 네트워크를 쓰는 VM이 있습니다. 먼저 그 인터페이스를 빼세요.",
    "node_online": "온라인인 노드입니다. 그 노드에서 oplctl cluster leave를 실행하세요.",
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
```

`web/src/i18n/ko.json`에서

```json
    "networks": "네트워크",
    "tasks": "작업"
```

을 다음으로 바꾼다:

```json
    "networks": "네트워크",
    "nodes": "노드",
    "tasks": "작업"
```

`web/src/i18n/ko.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "nodes": {
    "name": "이름",
    "state": "상태",
    "address": "API 주소",
    "hypervisor": "하이퍼바이저",
    "cpus": "CPU",
    "memory": "메모리",
    "bridges": "브리지",
    "vms": "VM",
    "thisNode": "(이 노드)",
    "joinNote": "노드를 더하려면 클러스터 화면에서 조인 토큰을 발급한 뒤 새 노드에서 oplctl cluster join을 실행하세요.",
    "maintenanceNote": "유지보수 중인 노드에는 새 VM을 놓지 않습니다. 이미 실행 중인 VM은 그대로 둡니다.",
    "startMaintenance": "유지보수 시작",
    "endMaintenance": "유지보수 끝내기",
    "maintenanceOn": "{{name}}을(를) 유지보수 모드로 바꿨습니다.",
    "maintenanceOff": "{{name}}의 유지보수를 끝냈습니다.",
    "remove": "클러스터에서 제거",
    "removeTitle": "{{name}}을(를) 클러스터에서 제거할까요?",
    "removeDescription": "아주 죽은 노드에만 쓰세요. 지금 데이터로는 다시 합류할 수 없고, 이 노드에 배정된 VM은 그대로 남습니다.",
    "removed": "{{name}}을(를) 클러스터에서 제거했습니다."
  },
  "vmForm": {
```

`web/src/layout/AppLayout.tsx`에서

```tsx
import {
  Database,
```

을 다음으로 바꾼다:

```tsx
import {
  Cpu,
  Database,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/networks", key: "nav.networks", icon: Network, end: false },
  { to: "/tasks", key: "nav.tasks", icon: ListChecks, end: false },
```

을 다음으로 바꾼다:

```tsx
  { to: "/networks", key: "nav.networks", icon: Network, end: false },
  { to: "/nodes", key: "nav.nodes", icon: Cpu, end: false },
  { to: "/tasks", key: "nav.tasks", icon: ListChecks, end: false },
```

`web/src/nodes/NodesPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { NodesPage } from "@/nodes/NodesPage";
import { NODE, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const DOWN = {
  ...NODE,
  id: "n2",
  name: "node-b",
  revision: 7,
  status: { ...NODE.status, online: false, bridges: [{ name: "vmbr0", vlan_filtering: true }] },
};

function cluster() {
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "x",
        leader: "n1",
        node: "n1",
        members: [],
      }),
    ),
  );
}

it("lists nodes with their state, capacity and VMs", async () => {
  cluster();
  mockLists({ node: [NODE, DOWN], vm: [vm("web", "running"), vm("db", "stopped")] });
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const a = (await within(table).findByText("node-a")).closest("tr")!;
  expect(a).toHaveTextContent("Online");
  expect(a).toHaveTextContent("16 GiB");
  expect(await within(a).findByText("(this node)")).toBeInTheDocument();
  // 두 VM 모두 node-a에 고정돼 있다.
  expect(within(a).getAllByRole("cell").at(-2)).toHaveTextContent("2");
  const b = within(table).getByText("node-b").closest("tr")!;
  expect(b).toHaveTextContent("Offline");
  expect(b).toHaveTextContent("vmbr0");
});

it("turns maintenance on with the revision it read", async () => {
  cluster();
  mockLists({ node: [DOWN] });
  let sent: { ifMatch: string | null; body: unknown } | undefined;
  server.use(
    http.put(apiUrl("/nodes/n2"), async ({ request }) => {
      sent = { ifMatch: request.headers.get("If-Match"), body: await request.json() };
      return HttpResponse.json(DOWN);
    }),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-b" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Enter maintenance" }));
  expect(await screen.findByText("node-b is in maintenance.")).toBeInTheDocument();
  expect(sent).toEqual({
    ifMatch: "7",
    body: { labels: {}, spec: { address: "10.0.0.1", maintenance: true } },
  });
});

it("removes only offline nodes, after typing the name", async () => {
  cluster();
  mockLists({ node: [NODE, DOWN] });
  let removed = false;
  server.use(
    http.delete(apiUrl("/nodes/n2"), () => {
      removed = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  expect(await screen.findByRole("menuitem", { name: "Remove from cluster" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  await userEvent.keyboard("{Escape}");

  await userEvent.click(screen.getByRole("button", { name: "Actions for node-b" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Remove from cluster" }));
  const dialog = await screen.findByRole("alertdialog");
  const confirm = within(dialog).getByRole("button", { name: "Remove from cluster" });
  expect(confirm).toBeDisabled();
  await userEvent.type(within(dialog).getByLabelText("Type node-b to confirm"), "node-b");
  await userEvent.click(confirm);
  expect(await screen.findByText("Removed node-b from the cluster.")).toBeInTheDocument();
  expect(removed).toBe(true);
});

it("hides node changes from operators", async () => {
  cluster();
  mockLists({ node: [NODE] });
  renderPage(<NodesPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByText("node-a")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Actions for node-a" })).not.toBeInTheDocument();
});
```

`web/src/nodes/NodesPage.tsx`를 만든다:

```tsx
import { useQuery } from "@tanstack/react-query";
import { MoreHorizontal } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Node, Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatCapacity } from "@/lib/format";

/** 그 노드에 고정했거나 그 노드에서 마지막으로 돈 VM의 수 */
function vmCount(node: Node, vms: Vm[]): number {
  return vms.filter((vm) => (vm.spec.node ?? vm.status.node) === node.id).length;
}

export function NodesPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const nodes = useList("node");
  const vms = useList("vm");
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const [removing, setRemoving] = useState<Node | null>(null);
  const write = can(me.role, "write", "node");

  async function setMaintenance(node: Node, maintenance: boolean) {
    try {
      await call(
        api.PUT("/nodes/{key}", {
          params: { path: { key: node.id }, header: { "If-Match": node.revision } },
          body: { labels: node.labels, spec: { ...node.spec, maintenance } },
        }),
      );
      toast.success(
        t(maintenance ? "nodes.maintenanceOn" : "nodes.maintenanceOff", { name: node.name }),
      );
    } catch (err) {
      toast.error(t("vms.actionFailed", { name: node.name, error: errorMessage(t, err) }));
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <h1 id="nodes-title" className="text-2xl font-semibold">
        {t("nav.nodes")}
      </h1>
      <p className="text-sm text-muted-foreground">{t("nodes.joinNote")}</p>
      <Table aria-labelledby="nodes-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("nodes.name")}</TableHead>
            <TableHead>{t("nodes.state")}</TableHead>
            <TableHead>{t("nodes.address")}</TableHead>
            <TableHead>{t("nodes.hypervisor")}</TableHead>
            <TableHead>{t("nodes.cpus")}</TableHead>
            <TableHead>{t("nodes.memory")}</TableHead>
            <TableHead>{t("nodes.bridges")}</TableHead>
            <TableHead>{t("nodes.vms")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(nodes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 9 : 8} query={nodes} empty={t("dashboard.noNodes")} />
          )}
          {(nodes.data ?? []).map((node) => (
            <TableRow key={node.id}>
              <TableCell className="font-medium">
                {node.name}
                {node.id === cluster.data?.node && (
                  <span className="ml-2 text-xs text-muted-foreground">{t("nodes.thisNode")}</span>
                )}
              </TableCell>
              <TableCell>
                <div className="flex flex-wrap gap-1">
                  <Badge variant={node.status.online ? "default" : "destructive"}>
                    {t(node.status.online ? "dashboard.nodeOnline" : "dashboard.nodeOffline")}
                  </Badge>
                  {node.spec.maintenance && (
                    <Badge variant="outline">{t("dashboard.maintenance")}</Badge>
                  )}
                </div>
              </TableCell>
              <TableCell className="font-mono text-xs">{node.spec.address}</TableCell>
              <TableCell>{node.status.hypervisor}</TableCell>
              <TableCell>{node.status.cpus}</TableCell>
              <TableCell>{formatCapacity(node.status.memory_mib)}</TableCell>
              <TableCell className="font-mono text-xs">
                {node.status.bridges.map((b) => b.name).join(", ") || "—"}
              </TableCell>
              <TableCell>{vmCount(node, vms.data ?? [])}</TableCell>
              {write && (
                <TableCell className="text-right">
                  <DropdownMenu modal={false}>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("vms.actionsFor", { name: node.name })}
                      >
                        <MoreHorizontal />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem
                        onSelect={() => void setMaintenance(node, !node.spec.maintenance)}
                      >
                        {t(
                          node.spec.maintenance ? "nodes.endMaintenance" : "nodes.startMaintenance",
                        )}
                      </DropdownMenuItem>
                      <DropdownMenuSeparator />
                      <DropdownMenuItem
                        variant="destructive"
                        // 살아 있는 노드는 그 노드에서 `oplctl cluster leave`로 뺀다(서버도 막는다).
                        disabled={node.status.online || node.id === cluster.data?.node}
                        onSelect={() => setRemoving(node)}
                      >
                        {t("nodes.remove")}
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </TableCell>
              )}
            </TableRow>
          ))}
        </TableBody>
      </Table>
      <p className="text-sm text-muted-foreground">{t("nodes.maintenanceNote")}</p>
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={t("nodes.removeTitle", { name: removing?.name })}
        description={t("nodes.removeDescription")}
        confirmLabel={t("nodes.remove")}
        typeToConfirm={removing?.name}
        onConfirm={async () => {
          const node = removing!;
          await call(api.DELETE("/nodes/{key}", { params: { path: { key: node.id } } }));
          toast.success(t("nodes.removed", { name: node.name }));
        }}
      />
    </div>
  );
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 110개 (`turns maintenance on with the revision it read`, `removes only offline nodes, after typing the name` 포함)

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 노드 화면 (유지보수 모드, 죽은 노드 제거)"
```

---

### Task 5: 볼륨 화면

`/volumes`: 이름·풀·크기(가져오는 중이면 —)·사용량·형식·상태·노드·사용하는 VM(그 VM 화면으로). operator 이상은 빈 볼륨 만들기(이름·풀·GiB·형식)와 삭제(이름 입력)를 한다. 둘 다 202로 받은 Task를 `trackTask`로 지켜본다. VM이 쓰거나 아직 만드는 볼륨은 지우지 못하게 막는다. 새 볼륨 이름 검사(`volumeNameError`: 이름 규칙, 예약 이름 `import`, 같은 이름)는 업로드도 쓰므로 `src/storage/volumes.ts`에 둔다.

**Files:**
- Create: `web/src/storage/VolumesPage.test.tsx`
- Create: `web/src/storage/VolumesPage.tsx`
- Create: `web/src/storage/volumes.ts`
- Modify: `web/src/App.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.tsx`에서

```tsx
import { PoolsPage } from "@/storage/PoolsPage";
import { TasksPage } from "@/tasks/TasksPage";
```

을 다음으로 바꾼다:

```tsx
import { PoolsPage } from "@/storage/PoolsPage";
import { VolumesPage } from "@/storage/VolumesPage";
import { TasksPage } from "@/tasks/TasksPage";
```

`web/src/App.tsx`에서

```tsx
            { path: "vms/:name/console", element: <ConsolePage /> },
            { path: "storage-pools", element: <PoolsPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "vms/:name/console", element: <ConsolePage /> },
            { path: "volumes", element: <VolumesPage /> },
            { path: "storage-pools", element: <PoolsPage /> },
```

`web/src/i18n/en.json`에서

```json
    "vms": "Virtual machines",
    "pools": "Storage pools",
```

을 다음으로 바꾼다:

```json
    "vms": "Virtual machines",
    "volumes": "Volumes",
    "pools": "Storage pools",
```

`web/src/i18n/en.json`에서

```json
  },
  "pools": {
```

을 다음으로 바꾼다:

```json
  },
  "volumes": {
    "name": "Name",
    "pool": "Storage pool",
    "size": "Size",
    "allocated": "Allocated",
    "format": "Format",
    "state": "State",
    "node": "Node",
    "vm": "Used by",
    "empty": "No volumes.",
    "phase": {
      "pending": "Creating",
      "ready": "Ready"
    },
    "create": "Create",
    "createTitle": "Create an empty volume",
    "createDescription": "A new disk image in a storage pool. A qcow2 volume takes space only as it is written.",
    "delete": "Delete",
    "deleteNamed": "Delete {{name}}",
    "deleteTitle": "Delete volume {{name}}?",
    "deleteDescription": "The disk image and all data on it are deleted.",
    "inUse": "Attached to {{vm}}.",
    "errors": {
      "reserved": "The name import is reserved.",
      "exists": "A volume with this name already exists."
    }
  },
  "pools": {
```

`web/src/i18n/ko.json`에서

```json
    "vms": "가상 머신",
    "pools": "스토리지 풀",
```

을 다음으로 바꾼다:

```json
    "vms": "가상 머신",
    "volumes": "볼륨",
    "pools": "스토리지 풀",
```

`web/src/i18n/ko.json`에서

```json
  },
  "pools": {
```

을 다음으로 바꾼다:

```json
  },
  "volumes": {
    "name": "이름",
    "pool": "스토리지 풀",
    "size": "크기",
    "allocated": "사용량",
    "format": "형식",
    "state": "상태",
    "node": "노드",
    "vm": "사용하는 VM",
    "empty": "볼륨이 없습니다.",
    "phase": {
      "pending": "만드는 중",
      "ready": "준비됨"
    },
    "create": "만들기",
    "createTitle": "빈 볼륨 만들기",
    "createDescription": "스토리지 풀에 새 디스크 이미지를 만듭니다. qcow2 볼륨은 쓴 만큼만 공간을 차지합니다.",
    "delete": "삭제",
    "deleteNamed": "{{name}} 삭제",
    "deleteTitle": "볼륨 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "디스크 이미지와 그 안의 데이터가 모두 지워집니다.",
    "inUse": "{{vm}}에 붙어 있습니다.",
    "errors": {
      "reserved": "import는 예약된 이름입니다.",
      "exists": "같은 이름의 볼륨이 이미 있습니다."
    }
  },
  "pools": {
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  Database,
  Languages,
```

을 다음으로 바꾼다:

```tsx
  Database,
  HardDrive,
  Languages,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/vms", key: "nav.vms", icon: Server, end: false },
  { to: "/storage-pools", key: "nav.pools", icon: Database, end: false },
```

을 다음으로 바꾼다:

```tsx
  { to: "/vms", key: "nav.vms", icon: Server, end: false },
  { to: "/volumes", key: "nav.volumes", icon: HardDrive, end: false },
  { to: "/storage-pools", key: "nav.pools", icon: Database, end: false },
```

`web/src/storage/VolumesPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { volumeNameError } from "@/storage/volumes";
import { VolumesPage } from "@/storage/VolumesPage";
import { resetTasks } from "@/tasks/tasks";
import { NODE, pool, task, vm, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(resetTasks);

it("checks new volume names like the server", () => {
  expect(volumeNameError("data", [])).toBeUndefined();
  expect(volumeNameError("Data", [])).toBe("forms.name");
  expect(volumeNameError("import", [])).toBe("volumes.errors.reserved");
  expect(volumeNameError("data", ["data"])).toBe("volumes.errors.exists");
});

it("lists volumes with their size, state and VM", async () => {
  const web = vm("web", "stopped", {
    spec: { ...vm("web", "stopped").spec, disks: [{ volume: "root", bus: "virtio-blk" }] },
  });
  const importing = {
    ...volume("img", "local", { phase: "pending", allocated_bytes: null }),
    spec: { pool: "local", size_bytes: 0, format: "qcow2" as const },
  };
  mockLists({ volume: [volume("root", "local"), importing], vm: [web], node: [NODE] });
  renderPage(<VolumesPage />);
  const table = await screen.findByRole("table", { name: "Volumes" });
  const root = (await within(table).findByText("root")).closest("tr")!;
  expect(root).toHaveTextContent("16 GiB");
  expect(root).toHaveTextContent("1 GiB");
  expect(root).toHaveTextContent("Ready");
  expect(within(root).getByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
  expect(within(root).getByRole("button", { name: "Delete root" })).toBeDisabled();
  const img = within(table).getByText("img").closest("tr")!;
  expect(img).toHaveTextContent("Creating");
  expect(within(img).getByRole("button", { name: "Delete img" })).toBeDisabled();
});

it("creates an empty volume and follows its task", async () => {
  mockLists({ "storage-pool": [pool("local")], volume: [volume("root", "local")] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/volumes"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "volume-create", "v9"), { status: 202 });
    }),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create an empty volume" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "root");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("A volume with this name already exists.")).toBeVisible();
  expect(within(dialog).getByText("Choose a storage pool.")).toBeVisible();

  await userEvent.clear(within(dialog).getByLabelText("Name"));
  await userEvent.type(within(dialog).getByLabelText("Name"), "data");
  await userEvent.selectOptions(within(dialog).getByLabelText("Storage pool"), "local");
  await userEvent.clear(within(dialog).getByLabelText("Size (GiB)"));
  await userEvent.type(within(dialog).getByLabelText("Size (GiB)"), "0.5");
  await userEvent.selectOptions(within(dialog).getByLabelText("Format"), "raw");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Create volume data")).toBeInTheDocument();
  expect(body).toEqual({
    name: "data",
    spec: { pool: "local", size_bytes: 512 * 1024 ** 2, format: "raw" },
  });
});

it("deletes a volume after typing its name", async () => {
  const data = volume("data", "local");
  mockLists({ volume: [data] });
  server.use(
    http.delete(apiUrl(`/volumes/${data.id}`), () =>
      HttpResponse.json(task("t2", "volume-delete", data.id), { status: 202 }),
    ),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete data" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type data to confirm"), "data");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Delete volume data")).toBeInTheDocument();
});

it("lets viewers only read", async () => {
  mockLists({ volume: [volume("data", "local")] });
  renderPage(<VolumesPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByText("data")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Delete data" })).not.toBeInTheDocument();
});
```

`web/src/storage/VolumesPage.tsx`를 만든다:

```tsx
import { Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import type { Volume } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatBytes } from "@/lib/format";
import { volumeNameError } from "@/storage/volumes";
import { trackTask } from "@/tasks/tasks";

const GIB = 1024 ** 3;

export function VolumesPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const volumes = useList("volume");
  const vms = useList("vm");
  const nodes = useList("node");
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<Volume | null>(null);
  const write = can(me.role, "write", "volume");
  // 서버는 VM spec의 디스크로 사용 중인지 판단한다(`attached_to`는 마지막으로 실행한 VM이다).
  const userOf = (volume: Volume) =>
    vms.data?.find((vm) => vm.spec.disks?.some((d) => d.volume === volume.name));

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="volumes-title" className="text-2xl font-semibold">
          {t("nav.volumes")}
        </h1>
        {write && (
          <div className="flex gap-2">
            <Button onClick={() => setCreating(true)}>
              <Plus />
              {t("volumes.create")}
            </Button>
          </div>
        )}
      </div>
      <Table aria-labelledby="volumes-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("volumes.name")}</TableHead>
            <TableHead>{t("volumes.pool")}</TableHead>
            <TableHead>{t("volumes.size")}</TableHead>
            <TableHead>{t("volumes.allocated")}</TableHead>
            <TableHead>{t("volumes.format")}</TableHead>
            <TableHead>{t("volumes.state")}</TableHead>
            <TableHead>{t("volumes.node")}</TableHead>
            <TableHead>{t("volumes.vm")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(volumes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 9 : 8} query={volumes} empty={t("volumes.empty")} />
          )}
          {(volumes.data ?? []).map((volume) => {
            const vm = userOf(volume);
            const ready = volume.status.phase === "ready";
            const node = nodes.data?.find((n) => n.id === volume.status.node);
            return (
              <TableRow key={volume.id}>
                <TableCell className="font-medium">{volume.name}</TableCell>
                <TableCell>{volume.spec.pool}</TableCell>
                {/* 가져오는 볼륨은 끝나기 전까지 크기를 모른다. */}
                <TableCell>
                  {volume.spec.size_bytes > 0 ? formatBytes(volume.spec.size_bytes) : "—"}
                </TableCell>
                <TableCell>
                  {volume.status.allocated_bytes !== null
                    ? formatBytes(volume.status.allocated_bytes)
                    : "—"}
                </TableCell>
                <TableCell>{volume.spec.format}</TableCell>
                <TableCell>
                  <Badge variant={ready ? "default" : "secondary"}>
                    {t(`volumes.phase.${volume.status.phase}`)}
                  </Badge>
                </TableCell>
                <TableCell>{node?.name ?? "—"}</TableCell>
                <TableCell>
                  {vm ? (
                    <Link
                      to={`/vms/${encodeURIComponent(vm.name)}`}
                      className="underline-offset-4 hover:underline"
                    >
                      {vm.name}
                    </Link>
                  ) : (
                    "—"
                  )}
                </TableCell>
                {write && (
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("volumes.deleteNamed", { name: volume.name })}
                      // 서버는 VM이 쓰거나 아직 만드는 볼륨을 지우지 않는다.
                      disabled={vm !== undefined || !ready}
                      title={vm ? t("volumes.inUse", { vm: vm.name }) : undefined}
                      onClick={() => setDeleting(volume)}
                    >
                      <Trash2 />
                    </Button>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && <CreateVolumeDialog onClose={() => setCreating(false)} />}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("volumes.deleteTitle", { name: deleting?.name })}
        description={t("volumes.deleteDescription")}
        confirmLabel={t("volumes.delete")}
        typeToConfirm={deleting?.name}
        onConfirm={async () => {
          const volume = deleting!;
          const task = await call(
            api.DELETE("/volumes/{key}", { params: { path: { key: volume.id } } }),
          );
          trackTask(task, t("tasks.op.volume-delete", { target: volume.name }));
        }}
      />
    </div>
  );
}

function CreateVolumeDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const [name, setName] = useState("");
  const [pool, setPool] = useState("");
  const [sizeGib, setSizeGib] = useState("16");
  const [format, setFormat] = useState<components["schemas"]["ImageFormat"]>("qcow2");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    const nameError = volumeNameError(name, volumes.data?.map((v) => v.name) ?? []);
    if (nameError) {
      found.name = nameError;
    }
    if (!pool) {
      found.pool = "vmForm.errors.pool";
    }
    if (!(Number(sizeGib) > 0)) {
      found.size = "vmForm.errors.size";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const task = await call(
      api.POST("/volumes", {
        body: { name, spec: { pool, size_bytes: Math.round(Number(sizeGib) * GIB), format } },
      }),
    );
    trackTask(task, t("tasks.op.volume-create", { target: name }));
  }

  return (
    <FormDialog
      title={t("volumes.createTitle")}
      description={t("volumes.createDescription")}
      submitLabel={t("volumes.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="volume-name" label={t("volumes.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="volume-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field id="volume-pool" label={t("volumes.pool")} error={errors.pool}>
        {(a11y) => (
          <NativeSelect
            id="volume-pool"
            className="w-full"
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">—</NativeSelectOption>
            {(pools.data ?? []).map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
                {p.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <div className="grid grid-cols-2 gap-4">
        <Field id="volume-size" label={t("vmForm.sizeGib")} error={errors.size}>
          {(a11y) => (
            <Input
              id="volume-size"
              inputMode="decimal"
              value={sizeGib}
              onChange={(e) => setSizeGib(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
        <Field id="volume-format" label={t("volumes.format")}>
          <NativeSelect
            id="volume-format"
            className="w-full"
            value={format}
            onChange={(e) => setFormat(e.target.value as typeof format)}
          >
            <NativeSelectOption value="qcow2">qcow2</NativeSelectOption>
            <NativeSelectOption value="raw">raw</NativeSelectOption>
          </NativeSelect>
        </Field>
      </div>
    </FormDialog>
  );
}
```

`web/src/storage/volumes.ts`를 만든다:

```ts
import { validName } from "@/vms/form";

/** `/volumes/import` 경로와 겹쳐 서버가 볼륨 이름으로 받지 않는다. */
const RESERVED = "import";

/** 새 볼륨 이름의 입력 오류(번역 키). `taken`은 이미 있는 볼륨의 이름이다. */
export function volumeNameError(name: string, taken: readonly string[]): string | undefined {
  if (!validName(name)) {
    return "forms.name";
  }
  if (name === RESERVED) {
    return "volumes.errors.reserved";
  }
  if (taken.includes(name)) {
    return "volumes.errors.exists";
  }
  return undefined;
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 115개 (`VolumesPage.test.tsx`의 5개 포함)

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 볼륨 화면 (목록·빈 볼륨 만들기·삭제)"
```

---

### Task 6: 디스크 이미지 업로드

`uploadImage`가 `XMLHttpRequest`로 `/volumes/import`에 파일을 보내고 진행률을 알린다(202면 Task, 오류 응답은 `ApiError`, 연결 실패는 `NetworkError`, 취소는 `signal`의 이유). 401은 `client.ts`에 더한 `reportUnauthorized`로 다른 요청처럼 알린다. `startUpload`는 진행률과 취소 버튼을 알림 하나에 보이고, 올리는 동안 `beforeunload`로 이탈을 확인하며, 끝나면 Task를 `trackTask`로 넘긴다. `resetUploads`는 로그아웃·세션 만료 때 `AuthProvider`가 불러 조용히 멈춘다. 볼륨 화면의 "이미지 올리기" 대화상자는 지금 접속한 노드(`GET /cluster`의 `node`)에서 쓸 수 있는 풀만 보이고, 파일 이름에서 볼륨 이름과 원본 형식을 제안한다. jsdom은 업로드 진행률을 내지 않으므로 테스트는 `FakeXhr`를 쓴다.

**Files:**
- Create: `web/src/storage/UploadDialog.test.tsx`
- Create: `web/src/storage/UploadDialog.tsx`
- Create: `web/src/storage/upload.test.tsx`
- Create: `web/src/storage/upload.ts`
- Create: `web/src/test/fakeXhr.ts`
- Modify: `web/src/api/client.ts`
- Modify: `web/src/auth/AuthProvider.test.tsx`
- Modify: `web/src/auth/AuthProvider.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/storage/VolumesPage.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/api/client.ts`에서

```ts
  authEpoch = epoch;
}
```

을 다음으로 바꾼다:

```ts
  authEpoch = epoch;
}

/** 지금의 인증 세대. fetch를 거치지 않는 요청(업로드)이 보낼 때 기록한다. */
export function currentAuthEpoch(): number {
  return authEpoch();
}

/** fetch를 거치지 않는 요청이 401을 받았다. `sentAt`은 보낼 때의 `currentAuthEpoch()`다. */
export function reportUnauthorized(sentAt: number) {
  onUnauthorized(sentAt);
}
```

`web/src/auth/AuthProvider.test.tsx`에서

```tsx
import { ThemeProvider } from "@/lib/theme";
import { settleTasks, trackTask } from "@/tasks/tasks";
import { ADMIN } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx
import { ThemeProvider } from "@/lib/theme";
import { startUpload } from "@/storage/upload";
import { settleTasks, trackTask } from "@/tasks/tasks";
import { FakeXhr } from "@/test/fakeXhr";
import { ADMIN } from "@/test/render";
```

`web/src/auth/AuthProvider.test.tsx`에서

```tsx
  expect(screen.getByText("explicit")).toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByText("explicit")).toBeInTheDocument();
});

it("stops uploads when signing out", async () => {
  vi.stubGlobal("XMLHttpRequest", FakeXhr);
  server.use(
    http.get(apiUrl("/auth/whoami"), () => HttpResponse.json(ADMIN)),
    http.post(apiUrl("/auth/logout"), () => new HttpResponse(null, { status: 204 })),
  );
  renderAuth();
  expect(await screen.findByText("admin")).toBeInTheDocument();
  const params = { name: "img", pool: "local", format: "qcow2", sourceFormat: "raw" } as const;
  act(() => startUpload(new File(["x"], "img.raw"), params, i18n.t));
  expect(await screen.findByText(/^Uploading img/)).toBeInTheDocument();

  await userEvent.click(screen.getByRole("button", { name: "logout" }));
  expect(await screen.findByText("signed out")).toBeInTheDocument();
  expect(FakeXhr.last!.aborted).toBe(true);
  await vi.waitFor(() => expect(screen.queryByText(/img/)).not.toBeInTheDocument());
  vi.unstubAllGlobals();
});
```

`web/src/auth/AuthProvider.tsx`에서

```tsx
import type { Principal } from "@/api/types";
import { resetTasks } from "@/tasks/tasks";
```

을 다음으로 바꾼다:

```tsx
import type { Principal } from "@/api/types";
import { resetUploads } from "@/storage/upload";
import { resetTasks } from "@/tasks/tasks";
```

`web/src/auth/AuthProvider.tsx`에서

```tsx
      resetTasks();
      toast.dismiss();
```

을 다음으로 바꾼다:

```tsx
      resetTasks();
      resetUploads();
      toast.dismiss();
```

`web/src/i18n/en.json`에서

```json
    "node_online": "The node is online. Run oplctl cluster leave on it instead.",
    "pool_in_use": "The pool still has volumes. Delete them first.",
```

을 다음으로 바꾼다:

```json
    "node_online": "The node is online. Run oplctl cluster leave on it instead.",
    "pool_not_on_node": "That storage pool is not available on the node you are connected to.",
    "insufficient_storage": "The node does not have enough free space for this upload.",
    "pool_in_use": "The pool still has volumes. Delete them first.",
```

`web/src/i18n/en.json`에서

```json
  },
  "pools": {
```

을 다음으로 바꾼다:

```json
  },
  "uploads": {
    "open": "Upload image",
    "title": "Upload a disk image",
    "description": "The file goes to the node you are connected to ({{node}}), so only storage pools on that node can be chosen. The image is converted into a new volume.",
    "file": "Image file",
    "sourceFormat": "File format",
    "submit": "Upload",
    "noPool": "No storage pool is available on this node.",
    "progress": "Uploading {{name}}: {{percent}}% ({{loaded}} / {{total}})",
    "cancelled": "Cancelled the upload of {{name}}.",
    "failed": "Upload of {{name}} failed. {{error}}",
    "errors": {
      "file": "Choose a file."
    }
  },
  "pools": {
```

`web/src/i18n/ko.json`에서

```json
    "node_online": "온라인인 노드입니다. 그 노드에서 oplctl cluster leave를 실행하세요.",
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
```

을 다음으로 바꾼다:

```json
    "node_online": "온라인인 노드입니다. 그 노드에서 oplctl cluster leave를 실행하세요.",
    "pool_not_on_node": "지금 접속한 노드에서 쓸 수 없는 스토리지 풀입니다.",
    "insufficient_storage": "노드에 이 파일을 받을 빈 공간이 부족합니다.",
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
```

`web/src/i18n/ko.json`에서

```json
  },
  "pools": {
```

을 다음으로 바꾼다:

```json
  },
  "uploads": {
    "open": "이미지 올리기",
    "title": "디스크 이미지 올리기",
    "description": "파일은 지금 접속한 노드({{node}})로 올라가므로 그 노드에서 쓸 수 있는 스토리지 풀만 고를 수 있습니다. 이미지는 새 볼륨으로 변환됩니다.",
    "file": "이미지 파일",
    "sourceFormat": "파일 형식",
    "submit": "올리기",
    "noPool": "이 노드에서 쓸 수 있는 스토리지 풀이 없습니다.",
    "progress": "{{name}} 올리는 중: {{percent}}% ({{loaded}} / {{total}})",
    "cancelled": "{{name}} 업로드를 취소했습니다.",
    "failed": "{{name}} 업로드에 실패했습니다. {{error}}",
    "errors": {
      "file": "파일을 고르세요."
    }
  },
  "pools": {
```

`web/src/storage/UploadDialog.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { poolsOnNode } from "@/storage/UploadDialog";
import { resetUploads } from "@/storage/upload";
import { VolumesPage } from "@/storage/VolumesPage";
import { FakeXhr } from "@/test/fakeXhr";
import { NODE, pool } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

beforeEach(() => vi.stubGlobal("XMLHttpRequest", FakeXhr));
afterEach(() => {
  vi.unstubAllGlobals();
  resetUploads();
});

it("offers only the pools of the node it is connected to", () => {
  const pools = [pool("all"), pool("here", { nodes: ["n1"] }), pool("far", { nodes: ["n2"] })];
  expect(poolsOnNode(pools, "n1").map((p) => p.name)).toEqual(["all", "here"]);
  expect(poolsOnNode(pools, undefined)).toEqual([]);
});

it("uploads a chosen file into a new volume", async () => {
  mockLists({ "storage-pool": [pool("local"), pool("far", { nodes: ["n2"] })], node: [NODE] });
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "x",
        leader: "n1",
        node: "n1",
        members: [],
      }),
    ),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Upload image" }));
  const dialog = await screen.findByRole("dialog", { name: "Upload a disk image" });
  expect(await within(dialog).findByText(/connected to \(node-a\)/)).toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  expect(within(dialog).getByText("Choose a file.")).toBeVisible();

  await userEvent.upload(
    within(dialog).getByLabelText("Image file"),
    new File(["disk"], "Ubuntu 24.04.qcow2"),
  );
  expect(within(dialog).getByLabelText("Name")).toHaveValue("ubuntu-24-04");
  expect(within(dialog).getByLabelText("File format")).toHaveValue("qcow2");
  const pools = within(within(dialog).getByLabelText("Storage pool")).getAllByRole("option");
  expect(pools.map((o) => o.textContent)).toEqual(["—", "local"]);
  await userEvent.selectOptions(within(dialog).getByLabelText("Storage pool"), "local");
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));

  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(FakeXhr.last!.url).toContain(
    "/volumes/import?name=ubuntu-24-04&pool=local&format=qcow2&source_format=qcow2",
  );
  expect(await screen.findByText(/^Uploading ubuntu-24-04: 0%/)).toBeInTheDocument();
});
```

`web/src/storage/UploadDialog.tsx`를 만든다:

```tsx
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import type { StoragePool } from "@/api/types";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { formatBytes } from "@/lib/format";
import { nameFromFile, sourceFormatOf, startUpload } from "@/storage/upload";
import { volumeNameError } from "@/storage/volumes";

type Schemas = components["schemas"];

/** 서버는 받은 노드에서 쓸 수 있는 풀로만 가져온다(노드를 정하지 않은 풀은 모든 노드). */
export function poolsOnNode(pools: StoragePool[], node: string | undefined): StoragePool[] {
  return node === undefined
    ? []
    : pools.filter((p) => (p.spec.nodes ?? []).length === 0 || p.spec.nodes!.includes(node));
}

export function UploadDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const nodes = useList("node");
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const usable = poolsOnNode(pools.data ?? [], cluster.data?.node);
  const here = nodes.data?.find((n) => n.id === cluster.data?.node)?.name ?? "—";
  const [file, setFile] = useState<File | null>(null);
  const [name, setName] = useState("");
  const [pool, setPool] = useState("");
  const [sourceFormat, setSourceFormat] = useState<Schemas["SourceFormat"]>("vmdk");
  const [format, setFormat] = useState<Schemas["ImageFormat"]>("qcow2");
  const [errors, setErrors] = useState<Record<string, string>>({});

  function choose(chosen: File | null) {
    setFile(chosen);
    if (chosen) {
      setName(nameFromFile(chosen.name));
      setSourceFormat(sourceFormatOf(chosen.name));
    }
  }

  async function submit() {
    const found: Record<string, string> = {};
    if (!file) {
      found.file = "uploads.errors.file";
    }
    const nameError = volumeNameError(name, volumes.data?.map((v) => v.name) ?? []);
    if (nameError) {
      found.name = nameError;
    }
    if (!pool) {
      found.pool = "vmForm.errors.pool";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    // 올리는 동안 대화상자를 닫아 둔다. 진행률은 알림에 보인다.
    startUpload(file!, { name, pool, format, sourceFormat }, t);
  }

  return (
    <FormDialog
      title={t("uploads.title")}
      description={t("uploads.description", { node: here })}
      submitLabel={t("uploads.submit")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field
        id="upload-file"
        label={t("uploads.file")}
        error={errors.file}
        hint={file ? formatBytes(file.size) : undefined}
      >
        {(a11y) => (
          <Input
            id="upload-file"
            type="file"
            onChange={(e) => choose(e.target.files?.[0] ?? null)}
            {...a11y}
          />
        )}
      </Field>
      <Field id="upload-name" label={t("volumes.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="upload-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field
        id="upload-pool"
        label={t("volumes.pool")}
        error={errors.pool}
        hint={usable.length === 0 && cluster.data ? t("uploads.noPool") : undefined}
      >
        {(a11y) => (
          <NativeSelect
            id="upload-pool"
            className="w-full"
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">—</NativeSelectOption>
            {usable.map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
                {p.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <div className="grid grid-cols-2 gap-4">
        <Field id="upload-source" label={t("uploads.sourceFormat")}>
          <NativeSelect
            id="upload-source"
            className="w-full"
            value={sourceFormat}
            onChange={(e) => setSourceFormat(e.target.value as typeof sourceFormat)}
          >
            <NativeSelectOption value="vmdk">vmdk</NativeSelectOption>
            <NativeSelectOption value="qcow2">qcow2</NativeSelectOption>
            <NativeSelectOption value="raw">raw</NativeSelectOption>
          </NativeSelect>
        </Field>
        <Field id="upload-format" label={t("volumes.format")}>
          <NativeSelect
            id="upload-format"
            className="w-full"
            value={format}
            onChange={(e) => setFormat(e.target.value as typeof format)}
          >
            <NativeSelectOption value="qcow2">qcow2</NativeSelectOption>
            <NativeSelectOption value="raw">raw</NativeSelectOption>
          </NativeSelect>
        </Field>
      </div>
    </FormDialog>
  );
}
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
import { Plus, Trash2 } from "lucide-react";
import { useState } from "react";
```

을 다음으로 바꾼다:

```tsx
import { Plus, Trash2, Upload } from "lucide-react";
import { useState } from "react";
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
import { formatBytes } from "@/lib/format";
import { volumeNameError } from "@/storage/volumes";
```

을 다음으로 바꾼다:

```tsx
import { formatBytes } from "@/lib/format";
import { UploadDialog } from "@/storage/UploadDialog";
import { volumeNameError } from "@/storage/volumes";
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<Volume | null>(null);
```

을 다음으로 바꾼다:

```tsx
  const [creating, setCreating] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [deleting, setDeleting] = useState<Volume | null>(null);
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
          <div className="flex gap-2">
            <Button onClick={() => setCreating(true)}>
```

을 다음으로 바꾼다:

```tsx
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => setUploading(true)}>
              <Upload />
              {t("uploads.open")}
            </Button>
            <Button onClick={() => setCreating(true)}>
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
      {creating && <CreateVolumeDialog onClose={() => setCreating(false)} />}
      <ConfirmDialog
```

을 다음으로 바꾼다:

```tsx
      {creating && <CreateVolumeDialog onClose={() => setCreating(false)} />}
      {uploading && <UploadDialog onClose={() => setUploading(false)} />}
      <ConfirmDialog
```

`web/src/storage/upload.test.tsx`를 만든다:

```tsx
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ApiError, NetworkError, setUnauthorizedHandler } from "@/api/client";
import { Toaster } from "@/components/ui/sonner";
import i18n from "@/i18n";
import { ThemeProvider } from "@/lib/theme";
import {
  nameFromFile,
  resetUploads,
  sourceFormatOf,
  startUpload,
  uploadImage,
} from "@/storage/upload";
import { resetTasks } from "@/tasks/tasks";
import { FakeXhr } from "@/test/fakeXhr";
import { task } from "@/test/fixtures";

function toasts() {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
}

const PARAMS = { name: "img", pool: "local", format: "qcow2", sourceFormat: "vmdk" } as const;

beforeEach(() => vi.stubGlobal("XMLHttpRequest", FakeXhr));
afterEach(() => {
  vi.unstubAllGlobals();
  resetUploads();
  resetTasks();
});

it("suggests a volume name and format from the file name", () => {
  expect(nameFromFile("Ubuntu 24.04.vmdk")).toBe("ubuntu-24-04");
  expect(nameFromFile("--disk--.qcow2")).toBe("disk");
  expect(nameFromFile(`${"a".repeat(62)}-b.img`)).toBe("a".repeat(62));
  expect(sourceFormatOf("disk.VMDK")).toBe("vmdk");
  expect(sourceFormatOf("disk.qcow2")).toBe("qcow2");
  expect(sourceFormatOf("disk.img")).toBe("raw");
});

it("posts the file to the import API and reports progress", async () => {
  const progress = vi.fn();
  const file = new Blob(["x".repeat(1024)]);
  const done = uploadImage(file, PARAMS, { onProgress: progress });
  const xhr = FakeXhr.last!;
  expect(xhr.method).toBe("POST");
  expect(xhr.url).toBe(
    `${window.location.origin}/api/v1/volumes/import?name=img&pool=local&format=qcow2&source_format=vmdk`,
  );
  expect(xhr.headers["Content-Type"]).toBe("application/octet-stream");
  expect(xhr.body).toBe(file);
  xhr.progress(512, 1024);
  expect(progress).toHaveBeenCalledWith(512, 1024);
  xhr.respond(202, task("t1", "volume-import", "v1"));
  await expect(done).resolves.toMatchObject({ id: "t1" });
});

it("turns failures into API, network and abort errors", async () => {
  const conflict = uploadImage(new Blob(["x"]), PARAMS, {});
  FakeXhr.last!.respond(409, { code: "pool_not_on_node", message: "elsewhere" });
  await expect(conflict).rejects.toEqual(new ApiError(409, "pool_not_on_node", "elsewhere"));

  const offline = uploadImage(new Blob(["x"]), PARAMS, {});
  FakeXhr.last!.fail();
  await expect(offline).rejects.toBeInstanceOf(NetworkError);

  const controller = new AbortController();
  const cancelled = uploadImage(new Blob(["x"]), PARAMS, { signal: controller.signal });
  controller.abort(new Error("stop"));
  await expect(cancelled).rejects.toThrow("stop");
  expect(FakeXhr.last!.aborted).toBe(true);
});

it("reports a 401 like other requests", async () => {
  const unauthorized = vi.fn();
  setUnauthorizedHandler(unauthorized, () => 3);
  const done = uploadImage(new Blob(["x"]), PARAMS, {});
  FakeXhr.last!.respond(401, { code: "unauthorized", message: "no session" });
  await expect(done).rejects.toBeInstanceOf(ApiError);
  expect(unauthorized).toHaveBeenCalledWith(3);
  setUnauthorizedHandler(
    () => {},
    () => 0,
  );
});

it("shows progress in a toast, guards the page and hands the task over", async () => {
  toasts();
  act(() => startUpload(new File(["x".repeat(2048)], "img.vmdk"), PARAMS, i18n.t));
  expect(await screen.findByText("Uploading img: 0% (0 B / 2 KiB)")).toBeInTheDocument();
  const leave = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(leave);
  expect(leave.defaultPrevented).toBe(true);

  act(() => FakeXhr.last!.progress(1024, 2048));
  expect(await screen.findByText("Uploading img: 50% (1 KiB / 2 KiB)")).toBeInTheDocument();
  act(() => FakeXhr.last!.respond(202, task("t9", "volume-import", "v1")));
  expect(await screen.findByText("Import volume img")).toBeInTheDocument();
  const after = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(after);
  expect(after.defaultPrevented).toBe(false);
});

it("cancels from the toast", async () => {
  toasts();
  act(() => startUpload(new File(["x"], "img.vmdk"), PARAMS, i18n.t));
  await userEvent.click(await screen.findByRole("button", { name: "Cancel" }));
  expect(await screen.findByText("Cancelled the upload of img.")).toBeInTheDocument();
  expect(FakeXhr.last!.aborted).toBe(true);
  expect(screen.queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
});
```

`web/src/storage/upload.ts`를 만든다:

```ts
import type { TFunction } from "i18next";
import { toast } from "sonner";

import { currentAuthEpoch, NetworkError, reportUnauthorized, toApiError } from "@/api/client";
import type { components } from "@/api/schema";
import type { Task } from "@/api/types";
import { errorMessage } from "@/i18n/errors";
import { formatBytes } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";

type Schemas = components["schemas"];

export interface UploadParams {
  name: string;
  pool: string;
  format: Schemas["ImageFormat"];
  sourceFormat: Schemas["SourceFormat"];
}

/** 파일 이름으로 볼륨 이름을 제안한다: `Ubuntu 24.04.vmdk` → `ubuntu-24-04` */
export function nameFromFile(file: string): string {
  return file
    .replace(/\.[^.]*$/, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+/, "")
    .slice(0, 63)
    .replace(/-+$/, "");
}

/** 확장자로 원본 형식을 짐작한다. 모르면 raw다(`.img`, `.raw`, `.iso`). */
export function sourceFormatOf(file: string): Schemas["SourceFormat"] {
  const ext = file.toLowerCase().split(".").pop();
  return ext === "vmdk" ? "vmdk" : ext === "qcow2" || ext === "qcow" ? "qcow2" : "raw";
}

/**
 * 이미지를 `/volumes/import`로 올린다. fetch는 올리는 진행률을 알려 주지 않아 `XMLHttpRequest`를 쓴다.
 * 받은 Task(202)로 끝나고, 오류 응답은 `ApiError`, 연결 실패는 `NetworkError`, 취소는 `signal`의 이유로
 * 거부한다. 중간에 끊기면 서버가 만들던 볼륨과 받은 파일을 지운다.
 */
export function uploadImage(
  file: Blob,
  params: UploadParams,
  {
    onProgress,
    signal,
  }: { onProgress?(loaded: number, total: number): void; signal?: AbortSignal },
): Promise<Task> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(signal.reason);
      return;
    }
    const query = new URLSearchParams({
      name: params.name,
      pool: params.pool,
      format: params.format,
      source_format: params.sourceFormat,
    });
    const xhr = new XMLHttpRequest();
    const sentAt = currentAuthEpoch();
    const onAbort = () => xhr.abort();
    signal?.addEventListener("abort", onAbort);
    const done = () => signal?.removeEventListener("abort", onAbort);
    xhr.open("POST", `${window.location.origin}/api/v1/volumes/import?${query}`);
    xhr.setRequestHeader("Content-Type", "application/octet-stream");
    xhr.upload.onprogress = (e) => onProgress?.(e.loaded, e.total || file.size);
    xhr.onload = () => {
      done();
      let body: unknown = null;
      try {
        body = JSON.parse(xhr.responseText);
      } catch {
        // 프록시의 HTML 오류 페이지 따위
      }
      if (xhr.status === 202) {
        resolve(body as Task);
        return;
      }
      if (xhr.status === 401) {
        reportUnauthorized(sentAt);
      }
      reject(toApiError(xhr.status, body));
    };
    xhr.onerror = () => {
      done();
      reject(new NetworkError(new Error("upload failed")));
    };
    xhr.onabort = () => {
      done();
      reject(signal?.reason ?? new DOMException("upload aborted", "AbortError"));
    };
    xhr.send(file);
  });
}

/** 진행 중인 업로드: 알림 id → 취소. `silent`면 취소를 알리지 않는다. */
const active = new Map<string, { controller: AbortController; silent: boolean }>();

/** 올리는 중에 페이지를 떠나려 하면 브라우저가 확인한다. */
function guard(e: BeforeUnloadEvent) {
  e.preventDefault();
}

/** 업로드를 모두 취소한다. 로그아웃하거나 세션이 끝나면 부른다(다음 사용자에게 이어지지 않게). */
export function resetUploads() {
  for (const upload of active.values()) {
    upload.silent = true;
    upload.controller.abort();
  }
}

/**
 * 업로드를 시작하고 진행률과 취소 버튼을 알림으로 보여 준다. 화면을 옮겨도 계속된다. 다 올리면 볼륨을
 * 채우는 Task를 `trackTask`로 넘긴다.
 */
export function startUpload(file: File, params: UploadParams, t: TFunction) {
  const id = `upload-${crypto.randomUUID()}`;
  const controller = new AbortController();
  const cancel = { label: t("confirm.cancel"), onClick: () => controller.abort() };
  if (active.size === 0) {
    window.addEventListener("beforeunload", guard);
  }
  const upload = { controller, silent: false };
  active.set(id, upload);
  let shown = -1;
  const show = (loaded: number, total: number) => {
    const percent = total > 0 ? Math.floor((loaded / total) * 100) : 0;
    if (percent === shown) {
      return;
    }
    shown = percent;
    toast.loading(
      t("uploads.progress", {
        name: params.name,
        percent,
        loaded: formatBytes(loaded),
        total: formatBytes(total),
      }),
      { id, action: cancel, duration: Infinity },
    );
  };
  show(0, file.size);
  void uploadImage(file, params, { onProgress: show, signal: controller.signal })
    .then(
      (task) => {
        toast.dismiss(id);
        trackTask(task, t("tasks.op.volume-import", { target: params.name }));
      },
      (err: unknown) => {
        // 같은 id로 바꾸는 알림은 이전 옵션을 물려받으므로 취소 버튼을 지운다.
        if (upload.silent) {
          toast.dismiss(id);
        } else if (controller.signal.aborted) {
          toast.info(t("uploads.cancelled", { name: params.name }), {
            id,
            action: undefined,
            duration: 4000,
          });
        } else {
          toast.error(t("uploads.failed", { name: params.name, error: errorMessage(t, err) }), {
            id,
            action: undefined,
            duration: Infinity,
            closeButton: true,
          });
        }
      },
    )
    .finally(() => {
      active.delete(id);
      if (active.size === 0) {
        window.removeEventListener("beforeunload", guard);
      }
    });
}
```

`web/src/test/fakeXhr.ts`를 만든다:

```ts
/**
 * 테스트용 `XMLHttpRequest`. jsdom은 업로드 진행률을 내지 않는다. `vi.stubGlobal("XMLHttpRequest",
 * FakeXhr)`로 바꿔 끼우고, 마지막 요청(`FakeXhr.last`)에 진행률과 응답을 흉내 낸다.
 */
export class FakeXhr {
  static last: FakeXhr | undefined;
  method = "";
  url = "";
  headers: Record<string, string> = {};
  body: unknown;
  status = 0;
  responseText = "";
  aborted = false;
  upload: { onprogress: ((e: { loaded: number; total: number }) => void) | null } = {
    onprogress: null,
  };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;

  constructor() {
    FakeXhr.last = this;
  }

  open(method: string, url: string) {
    this.method = method;
    this.url = url;
  }

  setRequestHeader(name: string, value: string) {
    this.headers[name] = value;
  }

  send(body: unknown) {
    this.body = body;
  }

  abort() {
    this.aborted = true;
    this.onabort?.();
  }

  progress(loaded: number, total: number) {
    this.upload.onprogress?.({ loaded, total });
  }

  respond(status: number, body: unknown) {
    this.status = status;
    this.responseText = typeof body === "string" ? body : JSON.stringify(body);
    this.onload?.();
  }

  fail() {
    this.onerror?.();
  }
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 124개 (`upload.test.tsx`의 6개, `uploads a chosen file into a new volume`, `stops uploads when signing out` 포함)

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 디스크 이미지 업로드 (진행률·취소, 접속한 노드의 풀, 이탈 확인)"
```

---

### Task 7: 사용자 화면 (admin)

`/users`: 이름·역할. 만들기(이름·역할·비밀번호와 확인), 역할 변경, 다른 사용자의 비밀번호 재설정, 삭제(확인). 자기 행의 재설정과 삭제는 막는다(자기 비밀번호는 Task 9의 계정 메뉴에서). 목록은 watch가 없어 바꾼 뒤 다시 읽는다. 왼쪽 메뉴 항목에 `kind`를 두어 읽을 수 있는 역할에만 보이고, 사용자 메뉴는 admin에게만 보인다. `last_admin`·`cannot_delete_self` 오류를 번역한다.

**Files:**
- Create: `web/src/accounts/PasswordFields.tsx`
- Create: `web/src/accounts/UsersPage.test.tsx`
- Create: `web/src/accounts/UsersPage.tsx`
- Create: `web/src/accounts/password.ts`
- Create: `web/src/layout/AppLayout.test.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.tsx`에서

```tsx

import { LiveProvider } from "@/api/LiveProvider";
```

을 다음으로 바꾼다:

```tsx

import { UsersPage } from "@/accounts/UsersPage";
import { LiveProvider } from "@/api/LiveProvider";
```

`web/src/App.tsx`에서

```tsx
            { path: "tasks", element: <TasksPage /> },
            { path: "*", element: <NotFoundPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "tasks", element: <TasksPage /> },
            { path: "users", element: <UsersPage /> },
            { path: "*", element: <NotFoundPage /> },
```

`web/src/accounts/PasswordFields.tsx`를 만든다:

```tsx
import { useTranslation } from "react-i18next";

import { Field } from "@/components/Field";
import { Input } from "@/components/ui/input";

/** 새 비밀번호와 그 확인. 오류는 `passwordErrors`의 결과다. */
export function PasswordFields({
  id,
  password,
  confirm,
  errors,
  onPassword,
  onConfirm,
}: {
  id: string;
  password: string;
  confirm: string;
  errors: Record<string, string>;
  onPassword(value: string): void;
  onConfirm(value: string): void;
}) {
  const { t } = useTranslation();
  return (
    <>
      <Field
        id={`${id}-password`}
        label={t("accounts.newPassword")}
        error={errors.password}
        hint={t("accounts.passwordHint")}
      >
        {(a11y) => (
          <Input
            id={`${id}-password`}
            type="password"
            autoComplete="new-password"
            value={password}
            onChange={(e) => onPassword(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field id={`${id}-confirm`} label={t("accounts.confirmPassword")} error={errors.confirm}>
        {(a11y) => (
          <Input
            id={`${id}-confirm`}
            type="password"
            autoComplete="new-password"
            value={confirm}
            onChange={(e) => onConfirm(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </>
  );
}
```

`web/src/accounts/UsersPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { passwordErrors } from "@/accounts/password";
import { UsersPage } from "@/accounts/UsersPage";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const ME = { id: ADMIN.user, name: "admin", role: "admin", revision: 1 };
const KIM = {
  id: "00000000-0000-0000-0000-0000000000k1",
  name: "kim",
  role: "viewer",
  revision: 2,
};

function users(list: unknown[]) {
  let items = list;
  server.use(http.get(apiUrl("/users"), () => HttpResponse.json({ revision: 1, items })));
  return (next: unknown[]) => {
    items = next;
  };
}

it("checks new passwords like the server", () => {
  expect(passwordErrors("short", "short")).toEqual({ password: "accounts.errors.password" });
  expect(passwordErrors("long enough", "long enougH")).toEqual({
    confirm: "accounts.errors.mismatch",
  });
  // 서버는 바이트가 아니라 글자를 센다.
  expect(passwordErrors("비밀번호여덟글자", "비밀번호여덟글자")).toEqual({});
});

it("lists users and keeps admins from deleting themselves", async () => {
  users([ME, KIM]);
  renderPage(<UsersPage />);
  const table = await screen.findByRole("table", { name: "Users" });
  const me = (await within(table).findByText("admin")).closest("tr")!;
  expect(me).toHaveTextContent("(you)");
  await userEvent.click(within(me).getByRole("button", { name: "Actions for admin" }));
  expect(await screen.findByRole("menuitem", { name: "Delete" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(screen.getByRole("menuitem", { name: "Reset password" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(within(table).getByText("kim").closest("tr")).toHaveTextContent("Viewer");
});

it("creates a user and reads the list again", async () => {
  const change = users([ME]);
  let body: unknown;
  server.use(
    http.post(apiUrl("/users"), async ({ request }) => {
      body = await request.json();
      change([ME, KIM]);
      return HttpResponse.json(KIM, { status: 201 });
    }),
  );
  renderPage(<UsersPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a user" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "kim");
  await userEvent.selectOptions(within(dialog).getByLabelText("Role"), "operator");
  await userEvent.type(within(dialog).getByLabelText("New password"), "secret-123");
  await userEvent.type(within(dialog).getByLabelText("Confirm the new password"), "secret-12");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("The passwords do not match.")).toBeVisible();

  await userEvent.type(within(dialog).getByLabelText("Confirm the new password"), "3");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created user kim.")).toBeInTheDocument();
  expect(body).toEqual({ name: "kim", role: "operator", password: "secret-123" });
  expect(await screen.findByText("kim")).toBeInTheDocument();
});

it("changes a role and shows why the server refused", async () => {
  users([ME, KIM]);
  const sent: unknown[] = [];
  server.use(
    http.put(apiUrl(`/users/${ME.id}`), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(
        { code: "last_admin", message: "the last admin cannot give up the admin role" },
        { status: 409 },
      );
    }),
  );
  renderPage(<UsersPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for admin" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Change role" }));
  const dialog = await screen.findByRole("dialog", { name: "Change the role of admin" });
  await userEvent.selectOptions(within(dialog).getByLabelText("Role"), "viewer");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The last administrator cannot give up the administrator role.",
  );
  expect(sent).toEqual([{ role: "viewer" }]);
});

it("resets another user's password", async () => {
  users([ME, KIM]);
  let body: unknown;
  server.use(
    http.put(apiUrl(`/users/${KIM.id}`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(KIM);
    }),
  );
  renderPage(<UsersPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for kim" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Reset password" }));
  const dialog = await screen.findByRole("dialog", { name: "Reset the password of kim" });
  await userEvent.type(within(dialog).getByLabelText("New password"), "new-secret");
  await userEvent.type(within(dialog).getByLabelText("Confirm the new password"), "new-secret");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Reset the password of kim.")).toBeInTheDocument();
  expect(body).toEqual({ password: "new-secret" });
});

it("deletes a user after confirming", async () => {
  const change = users([ME, KIM]);
  server.use(
    http.delete(apiUrl(`/users/${KIM.id}`), () => {
      change([ME]);
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<UsersPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for kim" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
  await userEvent.click(
    within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Delete" }),
  );
  expect(await screen.findByText("Deleted user kim.")).toBeInTheDocument();
  await vi.waitFor(() =>
    expect(screen.queryByRole("cell", { name: "kim" })).not.toBeInTheDocument(),
  );
});
```

`web/src/accounts/UsersPage.tsx`를 만든다:

```tsx
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { MoreHorizontal, Plus } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { passwordErrors } from "@/accounts/password";
import { PasswordFields } from "@/accounts/PasswordFields";
import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import { usePrincipal } from "@/auth/AuthProvider";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { validName } from "@/vms/form";

type User = components["schemas"]["UserView"];
type Role = components["schemas"]["Role"];

const ROLES: readonly Role[] = ["admin", "operator", "viewer"];

/** 사용자 목록. 감시할 수 없으므로 화면을 열 때와 바꾼 뒤에 다시 읽는다. */
export function useUsers() {
  return useQuery({
    queryKey: ["users"],
    queryFn: () => call(api.GET("/users")),
    select: (list) => list.items,
  });
}

type Dialog = { kind: "create" } | { kind: "role" | "password" | "delete"; user: User };

export function UsersPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const users = useUsers();
  const queryClient = useQueryClient();
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const close = () => setDialog(null);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["users"] });

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="users-title" className="text-2xl font-semibold">
          {t("nav.users")}
        </h1>
        <Button onClick={() => setDialog({ kind: "create" })}>
          <Plus />
          {t("users.create")}
        </Button>
      </div>
      <Table aria-labelledby="users-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("users.name")}</TableHead>
            <TableHead>{t("users.role")}</TableHead>
            <TableHead>
              <span className="sr-only">{t("vms.actions")}</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {(users.data ?? []).length === 0 && (
            <TableMessage colSpan={3} query={users} empty={t("users.empty")} />
          )}
          {(users.data ?? []).map((user) => {
            // 자기 비밀번호는 현재 비밀번호가 필요하므로 계정 메뉴에서 바꾼다. 자기 자신은 지울 수 없다.
            const self = user.id === me.user;
            return (
              <TableRow key={user.id}>
                <TableCell className="font-medium">
                  {user.name}
                  {self && (
                    <span className="ml-2 text-xs text-muted-foreground">{t("users.you")}</span>
                  )}
                </TableCell>
                <TableCell>{t(`roles.${user.role}`)}</TableCell>
                <TableCell className="text-right">
                  <DropdownMenu modal={false}>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("vms.actionsFor", { name: user.name })}
                      >
                        <MoreHorizontal />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem onSelect={() => setDialog({ kind: "role", user })}>
                        {t("users.changeRole")}
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        disabled={self}
                        onSelect={() => setDialog({ kind: "password", user })}
                      >
                        {t("users.resetPassword")}
                      </DropdownMenuItem>
                      <DropdownMenuSeparator />
                      <DropdownMenuItem
                        variant="destructive"
                        disabled={self}
                        onSelect={() => setDialog({ kind: "delete", user })}
                      >
                        {t("users.delete")}
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {dialog?.kind === "create" && <CreateUserDialog onClose={close} onDone={changed} />}
      {dialog?.kind === "role" && (
        <RoleDialog user={dialog.user} onClose={close} onDone={changed} />
      )}
      {dialog?.kind === "password" && <ResetPasswordDialog user={dialog.user} onClose={close} />}
      <ConfirmDialog
        open={dialog?.kind === "delete"}
        onOpenChange={(open) => !open && close()}
        title={t("users.deleteTitle", { name: dialog?.kind === "delete" ? dialog.user.name : "" })}
        description={t("users.deleteDescription")}
        confirmLabel={t("users.delete")}
        onConfirm={async () => {
          if (dialog?.kind !== "delete") {
            return;
          }
          const { user } = dialog;
          await call(api.DELETE("/users/{key}", { params: { path: { key: user.id } } }));
          toast.success(t("users.deleted", { name: user.name }));
          await changed();
        }}
      />
    </div>
  );
}

function RoleSelect({ value, onChange }: { value: Role; onChange(role: Role): void }) {
  const { t } = useTranslation();
  return (
    <NativeSelect
      id="user-role"
      className="w-full"
      value={value}
      onChange={(e) => onChange(e.target.value as Role)}
    >
      {ROLES.map((role) => (
        <NativeSelectOption key={role} value={role}>
          {t(`roles.${role}`)}
        </NativeSelectOption>
      ))}
    </NativeSelect>
  );
}

function CreateUserDialog({ onClose, onDone }: { onClose(): void; onDone(): Promise<void> }) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [role, setRole] = useState<Role>("viewer");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found = passwordErrors(password, confirm);
    if (!validName(name)) {
      found.name = "forms.name";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    await call(api.POST("/users", { body: { name, role, password } }));
    toast.success(t("users.created", { name }));
    await onDone();
  }

  return (
    <FormDialog
      title={t("users.createTitle")}
      submitLabel={t("users.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="user-name" label={t("users.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="user-name"
            autoComplete="off"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field id="user-role" label={t("users.role")} hint={t("users.roleHint")}>
        <RoleSelect value={role} onChange={setRole} />
      </Field>
      <PasswordFields
        id="user"
        password={password}
        confirm={confirm}
        errors={errors}
        onPassword={setPassword}
        onConfirm={setConfirm}
      />
    </FormDialog>
  );
}

function RoleDialog({
  user,
  onClose,
  onDone,
}: {
  user: User;
  onClose(): void;
  onDone(): Promise<void>;
}) {
  const { t } = useTranslation();
  const [role, setRole] = useState<Role>(user.role);

  async function submit() {
    await call(api.PUT("/users/{key}", { params: { path: { key: user.id } }, body: { role } }));
    toast.success(t("users.roleChanged", { name: user.name, role: t(`roles.${role}`) }));
    await onDone();
  }

  return (
    <FormDialog
      title={t("users.roleTitle", { name: user.name })}
      submitLabel={t("users.save")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="user-role" label={t("users.role")} hint={t("users.roleHint")}>
        <RoleSelect value={role} onChange={setRole} />
      </Field>
    </FormDialog>
  );
}

function ResetPasswordDialog({ user, onClose }: { user: User; onClose(): void }) {
  const { t } = useTranslation();
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found = passwordErrors(password, confirm);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    await call(api.PUT("/users/{key}", { params: { path: { key: user.id } }, body: { password } }));
    toast.success(t("users.passwordReset", { name: user.name }));
  }

  return (
    <FormDialog
      title={t("users.passwordTitle", { name: user.name })}
      description={t("users.passwordDescription")}
      submitLabel={t("users.save")}
      onClose={onClose}
      onSubmit={submit}
    >
      <PasswordFields
        id="reset"
        password={password}
        confirm={confirm}
        errors={errors}
        onPassword={setPassword}
        onConfirm={setConfirm}
      />
    </FormDialog>
  );
}
```

`web/src/accounts/password.ts`를 만든다:

```ts
/** 서버의 `MIN_PASSWORD_LEN`(글자 수) */
export const MIN_PASSWORD_LEN = 8;

/** 새 비밀번호와 확인 입력의 오류(번역 키). 서버는 글자 수만 본다. */
export function passwordErrors(password: string, confirm: string): Record<string, string> {
  const errors: Record<string, string> = {};
  if ([...password].length < MIN_PASSWORD_LEN) {
    errors.password = "accounts.errors.password";
  } else if (password !== confirm) {
    errors.confirm = "accounts.errors.mismatch";
  }
  return errors;
}
```

`web/src/i18n/en.json`에서

```json
    "insufficient_storage": "The node does not have enough free space for this upload.",
    "pool_in_use": "The pool still has volumes. Delete them first.",
```

을 다음으로 바꾼다:

```json
    "insufficient_storage": "The node does not have enough free space for this upload.",
    "last_admin": "The last administrator cannot give up the administrator role.",
    "cannot_delete_self": "You cannot delete your own account.",
    "pool_in_use": "The pool still has volumes. Delete them first.",
```

`web/src/i18n/en.json`에서

```json
    "nodes": "Nodes",
    "tasks": "Tasks"
  },
```

을 다음으로 바꾼다:

```json
    "nodes": "Nodes",
    "tasks": "Tasks",
    "users": "Users"
  },
```

`web/src/i18n/en.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "accounts": {
    "newPassword": "New password",
    "confirmPassword": "Confirm the new password",
    "passwordHint": "At least 8 characters.",
    "errors": {
      "password": "Use at least 8 characters.",
      "mismatch": "The passwords do not match."
    }
  },
  "users": {
    "name": "Name",
    "role": "Role",
    "you": "(you)",
    "empty": "No users.",
    "roleHint": "Operators change VMs and volumes. Viewers only read. Neither can see accounts.",
    "create": "Create",
    "createTitle": "Create a user",
    "created": "Created user {{name}}.",
    "changeRole": "Change role",
    "roleTitle": "Change the role of {{name}}",
    "roleChanged": "{{name}} is now {{role}}.",
    "resetPassword": "Reset password",
    "passwordTitle": "Reset the password of {{name}}",
    "passwordDescription": "The user's sessions end and they sign in with the new password.",
    "passwordReset": "Reset the password of {{name}}.",
    "save": "Save",
    "delete": "Delete",
    "deleteTitle": "Delete user {{name}}?",
    "deleteDescription": "The user and their API tokens are deleted.",
    "deleted": "Deleted user {{name}}."
  },
  "vmForm": {
```

`web/src/i18n/ko.json`에서

```json
    "insufficient_storage": "노드에 이 파일을 받을 빈 공간이 부족합니다.",
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
```

을 다음으로 바꾼다:

```json
    "insufficient_storage": "노드에 이 파일을 받을 빈 공간이 부족합니다.",
    "last_admin": "마지막 관리자는 관리자 역할을 내려놓을 수 없습니다.",
    "cannot_delete_self": "자기 계정은 지울 수 없습니다.",
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
```

`web/src/i18n/ko.json`에서

```json
    "nodes": "노드",
    "tasks": "작업"
  },
```

을 다음으로 바꾼다:

```json
    "nodes": "노드",
    "tasks": "작업",
    "users": "사용자"
  },
```

`web/src/i18n/ko.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "accounts": {
    "newPassword": "새 비밀번호",
    "confirmPassword": "새 비밀번호 확인",
    "passwordHint": "8자 이상입니다.",
    "errors": {
      "password": "8자 이상으로 쓰세요.",
      "mismatch": "두 비밀번호가 다릅니다."
    }
  },
  "users": {
    "name": "이름",
    "role": "역할",
    "you": "(나)",
    "empty": "사용자가 없습니다.",
    "roleHint": "운영자는 VM과 볼륨을 바꾸고, 조회자는 읽기만 합니다. 둘 다 계정은 볼 수 없습니다.",
    "create": "만들기",
    "createTitle": "사용자 만들기",
    "created": "사용자 {{name}}을(를) 만들었습니다.",
    "changeRole": "역할 바꾸기",
    "roleTitle": "{{name}}의 역할 바꾸기",
    "roleChanged": "{{name}}의 역할을 {{role}}(으)로 바꿨습니다.",
    "resetPassword": "비밀번호 재설정",
    "passwordTitle": "{{name}}의 비밀번호 재설정",
    "passwordDescription": "그 사용자의 세션이 끝나고, 새 비밀번호로 다시 로그인합니다.",
    "passwordReset": "{{name}}의 비밀번호를 재설정했습니다.",
    "save": "저장",
    "delete": "삭제",
    "deleteTitle": "사용자 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "사용자와 그 사용자의 API 토큰이 지워집니다.",
    "deleted": "사용자 {{name}}을(를) 삭제했습니다."
  },
  "vmForm": {
```

`web/src/layout/AppLayout.test.tsx`를 만든다:

```tsx
import { screen } from "@testing-library/react";
import { http, HttpResponse } from "msw";

import { AppLayout } from "@/layout/AppLayout";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

beforeEach(() =>
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: "n1", members: [] }),
    ),
  ),
);

it("shows account pages only to admins", async () => {
  renderPage(<AppLayout />);
  expect(await screen.findByRole("link", { name: "Volumes" })).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Users" })).toHaveAttribute("href", "/users");
});

it("hides account pages from other roles", async () => {
  renderPage(<AppLayout />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByRole("link", { name: "Volumes" })).toBeInTheDocument();
  expect(screen.queryByRole("link", { name: "Users" })).not.toBeInTheDocument();
});
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  User,
} from "lucide-react";
```

을 다음으로 바꾼다:

```tsx
  User,
  Users,
  type LucideIcon,
} from "lucide-react";
```

`web/src/layout/AppLayout.tsx`에서

```tsx
import { useAuth, usePrincipal } from "@/auth/AuthProvider";
import { Button } from "@/components/ui/button";
```

을 다음으로 바꾼다:

```tsx
import { useAuth, usePrincipal } from "@/auth/AuthProvider";
import { can, type Kind } from "@/auth/permissions";
import { Button } from "@/components/ui/button";
```

`web/src/layout/AppLayout.tsx`에서

```tsx

const NAV = [
  { to: "/", key: "nav.dashboard", icon: LayoutDashboard, end: true },
```

을 다음으로 바꾼다:

```tsx

/** 왼쪽 메뉴. `kind`가 있으면 그 종류를 읽을 수 있는 역할에만 보인다. */
const NAV: { to: string; key: string; icon: LucideIcon; end: boolean; kind?: Kind }[] = [
  { to: "/", key: "nav.dashboard", icon: LayoutDashboard, end: true },
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/tasks", key: "nav.tasks", icon: ListChecks, end: false },
] as const;

export function AppLayout() {
  const { t } = useTranslation();
  const cluster = useQuery({
```

을 다음으로 바꾼다:

```tsx
  { to: "/tasks", key: "nav.tasks", icon: ListChecks, end: false },
  { to: "/users", key: "nav.users", icon: Users, end: false, kind: "user" },
];

export function AppLayout() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const cluster = useQuery({
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  });
  return (
```

을 다음으로 바꾼다:

```tsx
  });
  const nav = NAV.filter((item) => !item.kind || can(me.role, "read", item.kind));
  return (
```

`web/src/layout/AppLayout.tsx`에서

```tsx
        <nav className="flex gap-1 overflow-x-auto px-2 pb-2 md:flex-col">
          {NAV.map(({ to, key, icon: Icon, end }) => (
            <NavLink
```

을 다음으로 바꾼다:

```tsx
        <nav className="flex gap-1 overflow-x-auto px-2 pb-2 md:flex-col">
          {nav.map(({ to, key, icon: Icon, end }) => (
            <NavLink
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 132개 (`UsersPage.test.tsx`의 6개, `hides account pages from other roles` 포함)

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 사용자 화면 (admin: 만들기·역할 변경·비밀번호 재설정·삭제), 역할별 메뉴"
```

---

### Task 8: API 토큰 화면

`/tokens`(모든 역할): 이름·만든 시각, admin에게는 주인도(사용자 목록은 admin만 읽는다). 만들면 비밀값을 한 번만 대화상자에 보이고 복사 버튼을 둔다. 삭제는 확인만 한다.

**Files:**
- Create: `web/src/accounts/TokensPage.test.tsx`
- Create: `web/src/accounts/TokensPage.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/accounts/UsersPage.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.tsx`에서

```tsx

import { UsersPage } from "@/accounts/UsersPage";
```

을 다음으로 바꾼다:

```tsx

import { TokensPage } from "@/accounts/TokensPage";
import { UsersPage } from "@/accounts/UsersPage";
```

`web/src/App.tsx`에서

```tsx
            { path: "users", element: <UsersPage /> },
            { path: "*", element: <NotFoundPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "users", element: <UsersPage /> },
            { path: "tokens", element: <TokensPage /> },
            { path: "*", element: <NotFoundPage /> },
```

`web/src/accounts/TokensPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { TokensPage } from "@/accounts/TokensPage";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const KIM = "00000000-0000-0000-0000-00000000000b";
const MINE = {
  id: "019a0f3c-8a10-7000-8000-0000000000c1",
  name: "cli",
  user: ADMIN.user,
  revision: 1,
};
const THEIRS = { id: "019a0f3c-8a10-7000-8000-0000000000c2", name: "ci", user: KIM, revision: 2 };

function tokens(list: unknown[]) {
  let items = list;
  server.use(http.get(apiUrl("/tokens"), () => HttpResponse.json({ revision: 1, items })));
  return (next: unknown[]) => {
    items = next;
  };
}

it("shows owners to admins", async () => {
  tokens([MINE, THEIRS]);
  server.use(
    http.get(apiUrl("/users"), () =>
      HttpResponse.json({
        revision: 1,
        items: [{ id: KIM, name: "kim", role: "operator", revision: 1 }],
      }),
    ),
  );
  renderPage(<TokensPage />);
  const table = await screen.findByRole("table", { name: "API tokens" });
  expect((await within(table).findByText("cli")).closest("tr")).toHaveTextContent("admin");
  expect(await within(table).findByText("kim")).toBeInTheDocument();
  expect(
    within(table)
      .getAllByRole("columnheader")
      .map((h) => h.textContent),
  ).toContain("Owner");
});

it("does not read users for other roles", async () => {
  tokens([MINE]);
  // /users 응답을 정하지 않았으므로 읽으면 테스트가 실패한다.
  renderPage(<TokensPage />, { as: { ...ADMIN, role: "viewer" } });
  const table = await screen.findByRole("table", { name: "API tokens" });
  expect(await within(table).findByText("cli")).toBeInTheDocument();
  expect(within(table).queryByRole("columnheader", { name: "Owner" })).not.toBeInTheDocument();
});

it("shows a new token's secret once, with a copy button", async () => {
  const user = userEvent.setup();
  const change = tokens([]);
  const created = { ...MINE, id: "019a0f3c-8a10-7000-8000-0000000000c3", name: "backup" };
  server.use(
    http.post(apiUrl("/tokens"), async ({ request }) => {
      expect(await request.json()).toEqual({ name: "backup" });
      change([created]);
      return HttpResponse.json({ token: created, secret: "opl_abc_secret" }, { status: 201 });
    }),
  );
  renderPage(<TokensPage />, { as: { ...ADMIN, role: "viewer" } });
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const form = await screen.findByRole("dialog", { name: "Create an API token" });
  await user.type(within(form).getByLabelText("Name"), "backup");
  await user.click(within(form).getByRole("button", { name: "Create" }));

  const dialog = await screen.findByRole("dialog", { name: "Token backup was created" });
  expect(within(dialog).getByLabelText("Token")).toHaveValue("opl_abc_secret");
  await user.click(within(dialog).getByRole("button", { name: "Copy the token" }));
  expect(await navigator.clipboard.readText()).toBe("opl_abc_secret");
  expect(await screen.findByText("Copied.")).toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Done" }));
  expect(screen.queryByText("opl_abc_secret")).not.toBeInTheDocument();
  expect(await screen.findByRole("cell", { name: "backup" })).toBeInTheDocument();
});

it("deletes a token after confirming", async () => {
  const change = tokens([MINE]);
  server.use(
    http.delete(apiUrl(`/tokens/${MINE.id}`), () => {
      change([]);
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<TokensPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete cli" }));
  await userEvent.click(
    within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Delete" }),
  );
  expect(await screen.findByText("Deleted API token cli.")).toBeInTheDocument();
  expect(await screen.findByText("No API tokens.")).toBeInTheDocument();
});
```

`web/src/accounts/TokensPage.tsx`를 만든다:

```tsx
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useUsers } from "@/accounts/UsersPage";
import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { CopyButton } from "@/components/CopyButton";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatTime, uuidTime } from "@/lib/format";
import { validName } from "@/vms/form";

type Token = components["schemas"]["TokenView"];
type CreatedToken = components["schemas"]["CreatedToken"];

/**
 * API 토큰. 누구나 자기 토큰을 만들고 지운다(토큰의 권한은 주인의 역할을 따른다). 관리자는 모든 토큰을
 * 보고 지울 수 있다. 감시할 수 없으므로 화면을 열 때와 바꾼 뒤에 다시 읽는다.
 */
export function TokensPage() {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const queryClient = useQueryClient();
  const tokens = useQuery({
    queryKey: ["tokens"],
    queryFn: () => call(api.GET("/tokens")),
    select: (list) => list.items,
  });
  const admin = can(me.role, "read", "user");
  const users = useUsers(admin);
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<CreatedToken | null>(null);
  const [deleting, setDeleting] = useState<Token | null>(null);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["tokens"] });
  const owner = (token: Token) =>
    token.user === me.user
      ? me.name
      : (users.data?.find((u) => u.id === token.user)?.name ?? token.user.slice(-8));

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="tokens-title" className="text-2xl font-semibold">
          {t("nav.tokens")}
        </h1>
        <Button onClick={() => setCreating(true)}>
          <Plus />
          {t("tokens.create")}
        </Button>
      </div>
      <p className="text-sm text-muted-foreground">{t("tokens.note")}</p>
      <Table aria-labelledby="tokens-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("tokens.name")}</TableHead>
            {admin && <TableHead>{t("tokens.owner")}</TableHead>}
            <TableHead>{t("tokens.created")}</TableHead>
            <TableHead>
              <span className="sr-only">{t("vms.actions")}</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {(tokens.data ?? []).length === 0 && (
            <TableMessage colSpan={admin ? 4 : 3} query={tokens} empty={t("tokens.empty")} />
          )}
          {(tokens.data ?? []).map((token) => {
            const time = uuidTime(token.id);
            return (
              <TableRow key={token.id}>
                <TableCell className="font-medium">{token.name}</TableCell>
                {admin && <TableCell>{owner(token)}</TableCell>}
                <TableCell className="whitespace-nowrap">
                  <time dateTime={time.toISOString()}>{formatTime(time, i18n.language)}</time>
                </TableCell>
                <TableCell className="text-right">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("tokens.deleteNamed", { name: token.name })}
                    onClick={() => setDeleting(token)}
                  >
                    <Trash2 />
                  </Button>
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && (
        <CreateTokenDialog
          onClose={() => setCreating(false)}
          onCreated={(token) => {
            setCreated(token);
            void changed();
          }}
        />
      )}
      {created && <SecretDialog created={created} onClose={() => setCreated(null)} />}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("tokens.deleteTitle", { name: deleting?.name })}
        description={t("tokens.deleteDescription")}
        confirmLabel={t("tokens.delete")}
        onConfirm={async () => {
          const token = deleting!;
          await call(api.DELETE("/tokens/{key}", { params: { path: { key: token.id } } }));
          toast.success(t("tokens.deleted", { name: token.name }));
          await changed();
        }}
      />
    </div>
  );
}

function CreateTokenDialog({
  onClose,
  onCreated,
}: {
  onClose(): void;
  onCreated(token: CreatedToken): void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [error, setError] = useState<string | undefined>();

  async function submit() {
    const invalid = validName(name) ? undefined : "forms.name";
    setError(invalid);
    if (invalid) {
      return false;
    }
    onCreated(await call(api.POST("/tokens", { body: { name } })));
  }

  return (
    <FormDialog
      title={t("tokens.createTitle")}
      description={t("tokens.createDescription")}
      submitLabel={t("tokens.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="token-name" label={t("tokens.name")} error={error}>
        {(a11y) => (
          <Input
            id="token-name"
            autoComplete="off"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}

/** 만든 토큰의 비밀값. 서버는 다시 보여 주지 않는다. */
function SecretDialog({ created, onClose }: { created: CreatedToken; onClose(): void }) {
  const { t } = useTranslation();
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("tokens.secretTitle", { name: created.token.name })}</DialogTitle>
          <DialogDescription>{t("tokens.secretDescription")}</DialogDescription>
        </DialogHeader>
        <div className="flex items-center gap-2">
          <Input
            readOnly
            aria-label={t("tokens.secret")}
            className="font-mono text-xs"
            value={created.secret}
            onFocus={(e) => e.target.select()}
          />
          <CopyButton text={created.secret} label={t("tokens.copySecret")} />
        </div>
        <p className="text-xs text-muted-foreground">
          {t("tokens.usage")} <code>Authorization: Bearer …</code>
        </p>
        <DialogFooter>
          <Button onClick={onClose}>{t("tokens.done")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
```

`web/src/accounts/UsersPage.tsx`에서

```tsx

/** 사용자 목록. 감시할 수 없으므로 화면을 열 때와 바꾼 뒤에 다시 읽는다. */
export function useUsers() {
  return useQuery({
```

을 다음으로 바꾼다:

```tsx

/**
 * 사용자 목록. 감시할 수 없으므로 화면을 열 때와 바꾼 뒤에 다시 읽는다. 관리자가 아니면 읽을 수 없으므로
 * `enabled`를 내린다.
 */
export function useUsers(enabled = true) {
  return useQuery({
```

`web/src/accounts/UsersPage.tsx`에서

```tsx
    select: (list) => list.items,
  });
```

을 다음으로 바꾼다:

```tsx
    select: (list) => list.items,
    enabled,
  });
```

`web/src/i18n/en.json`에서

```json
    "tasks": "Tasks",
    "users": "Users"
  },
```

을 다음으로 바꾼다:

```json
    "tasks": "Tasks",
    "users": "Users",
    "tokens": "API tokens"
  },
```

`web/src/i18n/en.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "tokens": {
    "name": "Name",
    "owner": "Owner",
    "created": "Created",
    "empty": "No API tokens.",
    "note": "Scripts and tools use API tokens. A token has the role of its owner. oplctl login makes one for the CLI.",
    "create": "Create",
    "createTitle": "Create an API token",
    "createDescription": "The token acts as you, with your role.",
    "secretTitle": "Token {{name}} was created",
    "secretDescription": "Copy the token now. It cannot be shown again.",
    "secret": "Token",
    "copySecret": "Copy the token",
    "usage": "Send it in the header",
    "done": "Done",
    "delete": "Delete",
    "deleteNamed": "Delete {{name}}",
    "deleteTitle": "Delete API token {{name}}?",
    "deleteDescription": "Anything using the token stops working right away.",
    "deleted": "Deleted API token {{name}}."
  },
  "vmForm": {
```

`web/src/i18n/ko.json`에서

```json
    "tasks": "작업",
    "users": "사용자"
  },
```

을 다음으로 바꾼다:

```json
    "tasks": "작업",
    "users": "사용자",
    "tokens": "API 토큰"
  },
```

`web/src/i18n/ko.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "tokens": {
    "name": "이름",
    "owner": "주인",
    "created": "만든 시각",
    "empty": "API 토큰이 없습니다.",
    "note": "스크립트와 도구는 API 토큰을 씁니다. 토큰의 권한은 주인의 역할을 따릅니다. oplctl login은 CLI용 토큰을 만듭니다.",
    "create": "만들기",
    "createTitle": "API 토큰 만들기",
    "createDescription": "토큰은 내 역할로 나 대신 요청합니다.",
    "secretTitle": "토큰 {{name}}을(를) 만들었습니다",
    "secretDescription": "지금 토큰을 복사하세요. 다시 볼 수 없습니다.",
    "secret": "토큰",
    "copySecret": "토큰 복사",
    "usage": "요청 헤더에 넣어 보냅니다",
    "done": "완료",
    "delete": "삭제",
    "deleteNamed": "{{name}} 삭제",
    "deleteTitle": "API 토큰 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "이 토큰을 쓰는 곳은 곧바로 동작하지 않습니다.",
    "deleted": "API 토큰 {{name}}을(를) 삭제했습니다."
  },
  "vmForm": {
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  HardDrive,
  Languages,
```

을 다음으로 바꾼다:

```tsx
  HardDrive,
  KeyRound,
  Languages,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/users", key: "nav.users", icon: Users, end: false, kind: "user" },
];
```

을 다음으로 바꾼다:

```tsx
  { to: "/users", key: "nav.users", icon: Users, end: false, kind: "user" },
  { to: "/tokens", key: "nav.tokens", icon: KeyRound, end: false },
];
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 136개 (`shows a new token's secret once, with a copy button`, `does not read users for other roles` 포함)

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): API 토큰 화면 (비밀값은 한 번만, 복사 버튼)"
```

---

### Task 9: 내 비밀번호 변경

계정 메뉴에 "내 비밀번호 변경"을 더한다. 현재 비밀번호와 새 비밀번호(확인)를 보내고, 서버가 403 `invalid_credentials`로 답하면 현재 비밀번호 필드의 오류로 보인다. 바꾸면 서버가 내 세션을 모두 끝내므로 로그아웃한 뒤 알린다. 메뉴에서 대화상자를 열므로 계정 메뉴는 모달로 두지 않는다.

**Files:**
- Create: `web/src/accounts/ChangePasswordDialog.test.tsx`
- Create: `web/src/accounts/ChangePasswordDialog.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/accounts/ChangePasswordDialog.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { AppLayout } from "@/layout/AppLayout";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

beforeEach(() =>
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: "n1", members: [] }),
    ),
  ),
);

async function openDialog() {
  await userEvent.click(await screen.findByRole("button", { name: "Account (admin)" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Change my password" }));
  return screen.findByRole("dialog", { name: "Change my password" });
}

it("says when the current password is wrong", async () => {
  server.use(
    http.put(apiUrl(`/users/${ADMIN.user}`), () =>
      HttpResponse.json(
        { code: "invalid_credentials", message: "the current password is wrong" },
        { status: 403 },
      ),
    ),
  );
  renderPage(<AppLayout />);
  const dialog = await openDialog();
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(within(dialog).getByText("Enter your current password.")).toBeVisible();

  await userEvent.type(within(dialog).getByLabelText("Current password"), "old-secret");
  await userEvent.type(within(dialog).getByLabelText("New password"), "new-secret");
  await userEvent.type(within(dialog).getByLabelText("Confirm the new password"), "new-secret");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await within(dialog).findByText("The current password is wrong.")).toBeVisible();
  expect(within(dialog).getByLabelText("Current password")).toHaveAttribute("aria-invalid", "true");
});

it("changes the password and signs out", async () => {
  let body: unknown;
  let signedOut = false;
  server.use(
    http.put(apiUrl(`/users/${ADMIN.user}`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ id: ADMIN.user, name: "admin", role: "admin", revision: 2 });
    }),
    http.post(apiUrl("/auth/logout"), () => {
      signedOut = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { router } = renderPage(<AppLayout />);
  const dialog = await openDialog();
  await userEvent.type(within(dialog).getByLabelText("Current password"), "old-secret");
  await userEvent.type(within(dialog).getByLabelText("New password"), "new-secret");
  await userEvent.type(within(dialog).getByLabelText("Confirm the new password"), "new-secret");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(
    await screen.findByText("Your password was changed. Sign in with the new password."),
  ).toBeInTheDocument();
  expect(body).toEqual({ current_password: "old-secret", password: "new-secret" });
  expect(signedOut).toBe(true);
  expect(router.state.location.pathname).toBe("/login");
});
```

`web/src/accounts/ChangePasswordDialog.tsx`를 만든다:

```tsx
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { passwordErrors } from "@/accounts/password";
import { PasswordFields } from "@/accounts/PasswordFields";
import { ApiError, api, call } from "@/api/client";
import { useAuth, usePrincipal } from "@/auth/AuthProvider";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Input } from "@/components/ui/input";

/**
 * 내 비밀번호를 바꾼다. 서버는 현재 비밀번호를 확인하고(훔친 세션으로 계정을 빼앗지 못하게), 바꾸면 내
 * 세션을 모두 끝낸다. 그래서 바꾼 뒤에는 로그아웃하고 새 비밀번호로 다시 로그인하게 한다.
 */
export function ChangePasswordDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const { logout } = useAuth();
  const [current, setCurrent] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found = passwordErrors(password, confirm);
    if (current === "") {
      found.current = "accounts.errors.current";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    try {
      await call(
        api.PUT("/users/{key}", {
          params: { path: { key: me.user } },
          body: { current_password: current, password },
        }),
      );
    } catch (err) {
      if (err instanceof ApiError && err.code === "invalid_credentials") {
        setErrors({ current: "accounts.errors.wrongCurrent" });
        return false;
      }
      throw err;
    }
    await logout();
    // 로그아웃이 알림을 모두 닫으므로 그 뒤에 알린다.
    toast.success(t("accounts.passwordChanged"));
  }

  return (
    <FormDialog
      title={t("accounts.changePassword")}
      description={t("accounts.changePasswordDescription")}
      submitLabel={t("users.save")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="current-password" label={t("accounts.currentPassword")} error={errors.current}>
        {(a11y) => (
          <Input
            id="current-password"
            type="password"
            autoComplete="current-password"
            value={current}
            onChange={(e) => setCurrent(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <PasswordFields
        id="mine"
        password={password}
        confirm={confirm}
        errors={errors}
        onPassword={setPassword}
        onConfirm={setConfirm}
      />
    </FormDialog>
  );
}
```

`web/src/i18n/en.json`에서

```json
  "accounts": {
    "newPassword": "New password",
```

을 다음으로 바꾼다:

```json
  "accounts": {
    "currentPassword": "Current password",
    "changePassword": "Change my password",
    "changePasswordDescription": "All your sessions end. Sign in again with the new password.",
    "passwordChanged": "Your password was changed. Sign in with the new password.",
    "newPassword": "New password",
```

`web/src/i18n/en.json`에서

```json
      "password": "Use at least 8 characters.",
      "mismatch": "The passwords do not match."
    }
```

을 다음으로 바꾼다:

```json
      "password": "Use at least 8 characters.",
      "mismatch": "The passwords do not match.",
      "current": "Enter your current password.",
      "wrongCurrent": "The current password is wrong."
    }
```

`web/src/i18n/ko.json`에서

```json
  "accounts": {
    "newPassword": "새 비밀번호",
```

을 다음으로 바꾼다:

```json
  "accounts": {
    "currentPassword": "현재 비밀번호",
    "changePassword": "내 비밀번호 변경",
    "changePasswordDescription": "내 세션이 모두 끝납니다. 새 비밀번호로 다시 로그인하세요.",
    "passwordChanged": "비밀번호를 바꿨습니다. 새 비밀번호로 로그인하세요.",
    "newPassword": "새 비밀번호",
```

`web/src/i18n/ko.json`에서

```json
      "password": "8자 이상으로 쓰세요.",
      "mismatch": "두 비밀번호가 다릅니다."
    }
```

을 다음으로 바꾼다:

```json
      "password": "8자 이상으로 쓰세요.",
      "mismatch": "두 비밀번호가 다릅니다.",
      "current": "현재 비밀번호를 입력하세요.",
      "wrongCurrent": "현재 비밀번호가 틀렸습니다."
    }
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  ListChecks,
  LogOut,
```

을 다음으로 바꾼다:

```tsx
  ListChecks,
  Lock,
  LogOut,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { NavLink, Outlet } from "react-router";

import { api, call } from "@/api/client";
```

을 다음으로 바꾼다:

```tsx
} from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { NavLink, Outlet } from "react-router";

import { ChangePasswordDialog } from "@/accounts/ChangePasswordDialog";
import { api, call } from "@/api/client";
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  const me = usePrincipal();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" aria-label={`${t("header.account")} (${me.name})`}>
          <User />
          <span className="hidden sm:inline">{me.name}</span>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>
          {me.name} · {t(`roles.${me.role}`)}
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={() => void logout()}>
          <LogOut />
          {t("header.logout")}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
```

을 다음으로 바꾼다:

```tsx
  const me = usePrincipal();
  const [changing, setChanging] = useState(false);
  return (
    <>
      {/* 메뉴에서 대화상자를 열므로 모달로 두지 않는다(닫힌 메뉴가 화면 클릭을 막지 않게). */}
      <DropdownMenu modal={false}>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" aria-label={`${t("header.account")} (${me.name})`}>
            <User />
            <span className="hidden sm:inline">{me.name}</span>
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuLabel>
            {me.name} · {t(`roles.${me.role}`)}
          </DropdownMenuLabel>
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => setChanging(true)}>
            <Lock />
            {t("accounts.changePassword")}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => void logout()}>
            <LogOut />
            {t("header.logout")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      {changing && <ChangePasswordDialog onClose={() => setChanging(false)} />}
    </>
  );
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 138개 (`says when the current password is wrong`, `changes the password and signs out` 포함)

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 내 비밀번호 변경 (현재 비밀번호 확인, 바꾼 뒤 다시 로그인)"
```

---

### Task 10: 클러스터 화면

`/cluster`(모든 역할): 이름, CA 지문(복사)과 내려받기(`/api/v1/cluster/ca`), 멤버(이름·상태·리더/투표 멤버/학습자·주소). admin은 조인 토큰을 발급하고(1시간·1일·7일), 새 노드에서 실행할 `oplctl cluster join '<URL>'`을 한 번만 보이고 복사하게 한다.

**Files:**
- Create: `web/src/cluster/ClusterPage.test.tsx`
- Create: `web/src/cluster/ClusterPage.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.tsx`에서

```tsx
import { RequireAuth } from "@/auth/RequireAuth";
import { Toaster } from "@/components/ui/sonner";
```

을 다음으로 바꾼다:

```tsx
import { RequireAuth } from "@/auth/RequireAuth";
import { ClusterPage } from "@/cluster/ClusterPage";
import { Toaster } from "@/components/ui/sonner";
```

`web/src/App.tsx`에서

```tsx
            { path: "tokens", element: <TokensPage /> },
            { path: "*", element: <NotFoundPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "tokens", element: <TokensPage /> },
            { path: "cluster", element: <ClusterPage /> },
            { path: "*", element: <NotFoundPage /> },
```

`web/src/cluster/ClusterPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { ClusterPage, joinCommand } from "@/cluster/ClusterPage";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const member = (id: string, name: string | null, extra = {}) => ({
  id,
  name,
  address: `${name}:8006`,
  cluster_address: `${name}:8007`,
  voter: true,
  online: true,
  ...extra,
});

beforeEach(() =>
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "sha256:00ff",
        leader: "n1",
        node: "n2",
        members: [
          member("n1", "node-a"),
          member("n2", "node-b"),
          member("n3", null, { voter: false, online: false, address: null }),
        ],
      }),
    ),
  ),
);

it("quotes the join URL in the command", () => {
  expect(joinCommand("opl-join://h:8006?token=a&ca=sha256:00")).toBe(
    "oplctl cluster join 'opl-join://h:8006?token=a&ca=sha256:00'",
  );
});

it("shows the CA and the members", async () => {
  renderPage(<ClusterPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByRole("heading", { name: "Cluster lab" })).toBeInTheDocument();
  expect(screen.getByText("sha256:00ff")).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Download the CA" })).toHaveAttribute(
    "href",
    "/api/v1/cluster/ca",
  );
  const table = screen.getByRole("table", { name: "Members" });
  expect(within(table).getByText("node-a").closest("tr")).toHaveTextContent("Leader");
  expect(within(table).getByText("node-b").closest("tr")).toHaveTextContent("(this node)");
  const joining = within(table).getByText("n3").closest("tr")!;
  expect(joining).toHaveTextContent("Learner");
  expect(joining).toHaveTextContent("Offline");
  expect(screen.queryByRole("button", { name: "Issue a join token" })).not.toBeInTheDocument();
});

it("issues a join token and shows the command once", async () => {
  const user = userEvent.setup();
  let body: unknown;
  server.use(
    http.post(apiUrl("/cluster/join-tokens"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(
        { url: "opl-join://node-b:8006?token=oplj_x&ca=sha256:00ff", expires_at: 1_900_000_000 },
        { status: 201 },
      );
    }),
  );
  renderPage(<ClusterPage />);
  await user.click(await screen.findByRole("button", { name: "Issue a join token" }));
  const form = await screen.findByRole("dialog", { name: "Issue a join token" });
  await user.selectOptions(within(form).getByLabelText("Valid for"), "1 day");
  await user.click(within(form).getByRole("button", { name: "Issue a join token" }));

  const dialog = await screen.findByRole("dialog", { name: "Join a new node" });
  expect(body).toEqual({ ttl_secs: 86400 });
  const command = "oplctl cluster join 'opl-join://node-b:8006?token=oplj_x&ca=sha256:00ff'";
  expect(within(dialog).getByLabelText("Command")).toHaveValue(command);
  expect(within(dialog).getByText(/2030/)).toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Copy the command" }));
  expect(await navigator.clipboard.readText()).toBe(command);
});
```

`web/src/cluster/ClusterPage.tsx`를 만든다:

```tsx
import { useQuery } from "@tanstack/react-query";
import { Download, Plus } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { CopyButton } from "@/components/CopyButton";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatTime } from "@/lib/format";

type JoinToken = components["schemas"]["CreatedJoinToken"];

/** 조인 토큰의 유효 시간(초). 서버의 기본은 1시간, 최대는 7일이다. */
const TTLS = [3600, 86400, 604800] as const;

/** 새 노드에서 실행할 명령. URL에 `?`와 `&`가 있어 따옴표로 감싼다. */
export function joinCommand(url: string): string {
  return `oplctl cluster join '${url}'`;
}

export function ClusterPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const [issuing, setIssuing] = useState(false);
  const [issued, setIssued] = useState<JoinToken | null>(null);

  if (!cluster.data) {
    return <ListPending error={cluster.error} />;
  }
  const { name, ca_fingerprint: fingerprint, leader, node: here, members } = cluster.data;
  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 className="text-2xl font-semibold">{t("cluster.title", { name })}</h1>
        {can(me.role, "write", "join-token") && (
          <Button onClick={() => setIssuing(true)}>
            <Plus />
            {t("cluster.issue")}
          </Button>
        )}
      </div>
      <section className="flex flex-col gap-2">
        <h2 className="font-medium">{t("cluster.ca")}</h2>
        <p className="text-sm text-muted-foreground">{t("cluster.caNote")}</p>
        <div className="flex flex-wrap items-center gap-2">
          <code className="rounded-md bg-muted px-2 py-1 text-xs break-all">{fingerprint}</code>
          <CopyButton text={fingerprint} label={t("cluster.copyFingerprint")} />
          <Button variant="outline" size="sm" asChild>
            <a href="/api/v1/cluster/ca" download="opl-ca.pem">
              <Download />
              {t("cluster.downloadCa")}
            </a>
          </Button>
        </div>
      </section>
      <section className="flex flex-col gap-2">
        <h2 id="cluster-members" className="font-medium">
          {t("cluster.members")}
        </h2>
        <Table aria-labelledby="cluster-members">
          <TableHeader>
            <TableRow>
              <TableHead>{t("nodes.name")}</TableHead>
              <TableHead>{t("nodes.state")}</TableHead>
              <TableHead>{t("cluster.role")}</TableHead>
              <TableHead>{t("nodes.address")}</TableHead>
              <TableHead>{t("cluster.clusterAddress")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {members.map((m) => (
              <TableRow key={m.id}>
                <TableCell className="font-medium">
                  {m.name ?? m.id.slice(-8)}
                  {m.id === here && (
                    <span className="ml-2 text-xs text-muted-foreground">
                      {t("nodes.thisNode")}
                    </span>
                  )}
                </TableCell>
                <TableCell>
                  <Badge variant={m.online ? "default" : "destructive"}>
                    {t(m.online ? "dashboard.nodeOnline" : "dashboard.nodeOffline")}
                  </Badge>
                </TableCell>
                <TableCell>
                  {m.id === leader
                    ? t("cluster.leader")
                    : t(m.voter ? "cluster.voter" : "cluster.learner")}
                </TableCell>
                <TableCell className="font-mono text-xs">{m.address ?? "—"}</TableCell>
                <TableCell className="font-mono text-xs">{m.cluster_address}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
        <p className="text-sm text-muted-foreground">{t("cluster.membersNote")}</p>
      </section>
      {issuing && (
        <IssueDialog onClose={() => setIssuing(false)} onIssued={(token) => setIssued(token)} />
      )}
      {issued && <JoinTokenDialog token={issued} onClose={() => setIssued(null)} />}
    </div>
  );
}

function IssueDialog({ onClose, onIssued }: { onClose(): void; onIssued(token: JoinToken): void }) {
  const { t } = useTranslation();
  const [ttl, setTtl] = useState<number>(TTLS[0]);

  async function submit() {
    onIssued(await call(api.POST("/cluster/join-tokens", { body: { ttl_secs: ttl } })));
  }

  return (
    <FormDialog
      title={t("cluster.issueTitle")}
      description={t("cluster.issueDescription")}
      submitLabel={t("cluster.issue")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="join-ttl" label={t("cluster.ttl")}>
        <NativeSelect
          id="join-ttl"
          className="w-full"
          value={ttl}
          onChange={(e) => setTtl(Number(e.target.value))}
        >
          {TTLS.map((secs) => (
            <NativeSelectOption key={secs} value={secs}>
              {t(`cluster.ttlOption.${secs}`)}
            </NativeSelectOption>
          ))}
        </NativeSelect>
      </Field>
    </FormDialog>
  );
}

/** 발급한 조인 URL. 서버는 다시 보여 주지 않는다. */
function JoinTokenDialog({ token, onClose }: { token: JoinToken; onClose(): void }) {
  const { t, i18n } = useTranslation();
  const command = joinCommand(token.url);
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("cluster.issuedTitle")}</DialogTitle>
          <DialogDescription>
            {t("cluster.issuedDescription", {
              time: formatTime(new Date(token.expires_at * 1000), i18n.language),
            })}
          </DialogDescription>
        </DialogHeader>
        <div className="flex items-center gap-2">
          <Input
            readOnly
            aria-label={t("cluster.command")}
            className="font-mono text-xs"
            value={command}
            onFocus={(e) => e.target.select()}
          />
          <CopyButton text={command} label={t("cluster.copyCommand")} />
        </div>
        <p className="text-xs text-muted-foreground">{t("cluster.commandNote")}</p>
        <DialogFooter>
          <Button onClick={onClose}>{t("tokens.done")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
```

`web/src/i18n/en.json`에서

```json
    "users": "Users",
    "tokens": "API tokens"
  },
```

을 다음으로 바꾼다:

```json
    "users": "Users",
    "tokens": "API tokens",
    "cluster": "Cluster"
  },
```

`web/src/i18n/en.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "cluster": {
    "title": "Cluster {{name}}",
    "ca": "Certificate authority",
    "caNote": "Every node's HTTPS certificate is signed by the cluster CA. Import the CA into your browser to stop the certificate warning, and check that its fingerprint matches.",
    "copyFingerprint": "Copy the CA fingerprint",
    "downloadCa": "Download the CA",
    "members": "Members",
    "membersNote": "Voters keep the cluster state. A learner is a node that is still joining.",
    "role": "Role",
    "leader": "Leader",
    "voter": "Voter",
    "learner": "Learner",
    "clusterAddress": "Cluster address",
    "issue": "Issue a join token",
    "issueTitle": "Issue a join token",
    "issueDescription": "A new node joins with the token once. Start opld on the new node first.",
    "ttl": "Valid for",
    "ttlOption": {
      "3600": "1 hour",
      "86400": "1 day",
      "604800": "7 days"
    },
    "issuedTitle": "Join a new node",
    "issuedDescription": "Run this on the new node before {{time}}. It cannot be shown again.",
    "command": "Command",
    "copyCommand": "Copy the command",
    "commandNote": "Add --socket if opld on the new node does not use /run/opl/opld.sock."
  },
  "vmForm": {
```

`web/src/i18n/ko.json`에서

```json
    "users": "사용자",
    "tokens": "API 토큰"
  },
```

을 다음으로 바꾼다:

```json
    "users": "사용자",
    "tokens": "API 토큰",
    "cluster": "클러스터"
  },
```

`web/src/i18n/ko.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "cluster": {
    "title": "클러스터 {{name}}",
    "ca": "인증 기관(CA)",
    "caNote": "모든 노드의 HTTPS 인증서는 클러스터 CA가 서명합니다. 인증서 경고를 없애려면 CA를 브라우저에 가져오고, 지문이 같은지 확인하세요.",
    "copyFingerprint": "CA 지문 복사",
    "downloadCa": "CA 내려받기",
    "members": "멤버",
    "membersNote": "투표 멤버가 클러스터 상태를 지킵니다. 학습자는 아직 합류 중인 노드입니다.",
    "role": "역할",
    "leader": "리더",
    "voter": "투표 멤버",
    "learner": "학습자",
    "clusterAddress": "클러스터 주소",
    "issue": "조인 토큰 발급",
    "issueTitle": "조인 토큰 발급",
    "issueDescription": "새 노드가 이 토큰으로 한 번 합류합니다. 먼저 새 노드에서 opld를 시작하세요.",
    "ttl": "유효 기간",
    "ttlOption": {
      "3600": "1시간",
      "86400": "1일",
      "604800": "7일"
    },
    "issuedTitle": "새 노드 합류",
    "issuedDescription": "{{time}} 전에 새 노드에서 실행하세요. 다시 볼 수 없습니다.",
    "command": "명령",
    "copyCommand": "명령 복사",
    "commandNote": "새 노드의 opld가 /run/opl/opld.sock을 쓰지 않으면 --socket을 붙이세요."
  },
  "vmForm": {
```

`web/src/layout/AppLayout.tsx`에서

```tsx
import {
  Cpu,
```

을 다음으로 바꾼다:

```tsx
import {
  Boxes,
  Cpu,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/tokens", key: "nav.tokens", icon: KeyRound, end: false },
];
```

을 다음으로 바꾼다:

```tsx
  { to: "/tokens", key: "nav.tokens", icon: KeyRound, end: false },
  { to: "/cluster", key: "nav.cluster", icon: Boxes, end: false },
];
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 141개 (`ClusterPage.test.tsx`의 3개 포함)

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 클러스터 화면 (멤버, CA 지문과 내려받기, 조인 토큰 발급)"
```

---

### Task 11: 브라우저 E2E: 업로드부터 정리까지

Rust 테스트는 클러스터와 로그인만 준비하고, 빈 풀 디렉터리와 16 MiB raw 이미지를 만들어 `OPL_POOL_PATH`·`OPL_IMAGE`로 넘긴다. Playwright가 풀 만들기 → 이미지 올리기(준비될 때까지) → VM 만들기·시작 → 콘솔 화면 → 강제 정지·삭제 → 볼륨 삭제 → 풀 삭제 → 로그아웃을 하고, Rust가 VM·볼륨·풀이 모두 없는지 본다. 실패하면 남은 VM과 볼륨을 CLI로 지운다.

**Files:**
- Modify: `crates/opld/tests/web.rs`
- Modify: `web/e2e/vm.spec.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opld/tests/web.rs`에서

```rust
//! 웹 UI E2E: 실제 opld(QEMU)에 브라우저(Playwright)로 로그인해 VM을 만들고 콘솔을 본 뒤 지운다.
//! 클러스터와 볼륨은 여기서 oplctl로 준비한다.
//! `pnpm -C web build && cargo test -p opld --features web-it --test web`
```

을 다음으로 바꾼다:

```rust
//! 웹 UI E2E: 실제 opld(QEMU)에 브라우저(Playwright)로 로그인해 풀을 만들고 이미지를 올린 뒤, 그 볼륨으로
//! VM을 만들어 콘솔을 보고, VM·볼륨·풀을 모두 지운다. 클러스터와 로그인 정보는 여기서 준비한다.
//! `pnpm -C web build && cargo test -p opld --features web-it --test web`
```

`crates/opld/tests/web.rs`에서

```rust

#[tokio::test]
async fn browser_creates_vm_opens_console_and_deletes_it() {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web");
```

을 다음으로 바꾼다:

```rust

/// 브라우저가 올릴 디스크 이미지(raw, 0으로 채운 sparse 파일)의 크기
const IMAGE_BYTES: u64 = 16 << 20;

#[tokio::test]
async fn browser_uploads_image_runs_vm_and_cleans_up() {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web");
```

`crates/opld/tests/web.rs`에서

```rust
    std::fs::create_dir(&pool).unwrap();
    d.ctl(&["pool", "create", "local", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    d.ctl(&[
        "volume", "create", "root", "--pool", "local", "--size", "64M",
    ])
    .await
    .unwrap();

    let out = tokio::task::spawn_blocking({
        let (url, password) = (d.server(), d.admin_password.clone().unwrap());
        move || {
```

을 다음으로 바꾼다:

```rust
    std::fs::create_dir(&pool).unwrap();
    let image = d.path("image.raw");
    std::fs::File::create(&image)
        .unwrap()
        .set_len(IMAGE_BYTES)
        .unwrap();

    let out = tokio::task::spawn_blocking({
        let (url, password) = (d.server(), d.admin_password.clone().unwrap());
        let (pool, image) = (pool.clone(), image.clone());
        move || {
```

`crates/opld/tests/web.rs`에서

```rust
                .env("OPL_PASSWORD", password)
                .env("OPL_VOLUME", "root")
                .output()
```

을 다음으로 바꾼다:

```rust
                .env("OPL_PASSWORD", password)
                .env("OPL_POOL_PATH", pool)
                .env("OPL_IMAGE", image)
                .output()
```

`crates/opld/tests/web.rs`에서

```rust
    if !out.status.success() {
        // 브라우저가 VM을 지우지 못했으면 QEMU가 남지 않게 끈다(실패는 무시한다).
        let _ = d.ctl(&["vm", "stop", "web", "--force"]).await;
        let _ = d.ctl(&["vm", "delete", "web"]).await;
    }
```

을 다음으로 바꾼다:

```rust
    if !out.status.success() {
        // 브라우저가 지우지 못한 것을 지운다(QEMU가 남지 않게). 실패는 무시한다.
        let _ = d.ctl(&["vm", "stop", "web", "--force"]).await;
        let _ = d.ctl(&["vm", "delete", "web"]).await;
        let _ = d.ctl(&["volume", "delete", "root"]).await;
    }
```

`crates/opld/tests/web.rs`에서

```rust
    );
    // 브라우저가 지운 VM은 없고, 볼륨은 남는다.
    let vms = d.ctl_json(&["vm", "list"]).await;
    assert_eq!(vms.as_array().map(Vec::len), Some(0), "{vms}");
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.stop();
```

을 다음으로 바꾼다:

```rust
    );
    // 브라우저가 만든 것은 모두 지웠다.
    for kind in ["vm", "volume", "pool"] {
        let list = d.ctl_json(&[kind, "list"]).await;
        assert_eq!(list.as_array().map(Vec::len), Some(0), "{kind}: {list}");
    }
    d.stop();
```

`web/e2e/vm.spec.ts`에서

```ts
const password = process.env.OPL_PASSWORD ?? "";
if (!password) {
  throw new Error("Set OPL_PASSWORD to the admin password (the Rust test sets it).");
}
/** Rust 테스트가 미리 만든 빈 볼륨 */
const volume = process.env.OPL_VOLUME ?? "root";
```

을 다음으로 바꾼다:

```ts
const password = process.env.OPL_PASSWORD ?? "";
/** 노드에 미리 만든 빈 디렉터리(풀)와 올릴 디스크 이미지. Rust 테스트가 준비한다. */
const poolPath = process.env.OPL_POOL_PATH ?? "";
const image = process.env.OPL_IMAGE ?? "";
if (!password || !poolPath || !image) {
  throw new Error("Set OPL_PASSWORD, OPL_POOL_PATH and OPL_IMAGE (the Rust test sets them).");
}
```

`web/e2e/vm.spec.ts`에서

```ts

test("create a VM, open its console, stop and delete it", async ({ page }) => {
  await signIn(page);
  await expect(page.getByRole("status").filter({ hasText: "Live" })).toBeVisible();
```

을 다음으로 바꾼다:

```ts

test("upload an image, run a VM from it, then delete everything", async ({ page }) => {
  await signIn(page);
  await expect(page.getByRole("status").filter({ hasText: "Live" })).toBeVisible();

  // 풀을 만들고 이미지를 올린다. 볼륨은 가져오기 Task가 끝나면 준비된다(watch).
  await page.getByRole("link", { name: "Storage pools" }).click();
  await page.getByRole("button", { name: "Create" }).click();
  let dialog = page.getByRole("dialog", { name: "Create a storage pool" });
  await dialog.getByLabel("Name").fill("local");
  await dialog.getByLabel("Directory").fill(poolPath);
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("cell", { name: "local", exact: true })).toBeVisible();

  await page.getByRole("link", { name: "Volumes" }).click();
  await page.getByRole("button", { name: "Upload image" }).click();
  dialog = page.getByRole("dialog", { name: "Upload a disk image" });
  await dialog.getByLabel("Image file").setInputFiles(image);
  await expect(dialog.getByLabel("File format")).toHaveValue("raw");
  await dialog.getByLabel("Name").fill("root");
  await dialog.getByLabel("Storage pool").selectOption("local");
  await dialog.getByRole("button", { name: "Upload" }).click();
  const root = page.getByRole("row").filter({ has: page.getByRole("cell", { name: "root" }) });
  await expect(root).toContainText("Ready", { timeout: 60_000 });
  await expect(root).toContainText("16 MiB");
```

`web/e2e/vm.spec.ts`에서

```ts
  await page.getByRole("button", { name: "Attach a volume" }).click();
  await page.getByRole("group", { name: "Disk 1" }).getByLabel("Volume").selectOption(volume);
  await page.getByRole("button", { name: "Create", exact: true }).click();
```

을 다음으로 바꾼다:

```ts
  await page.getByRole("button", { name: "Attach a volume" }).click();
  await page.getByRole("group", { name: "Disk 1" }).getByLabel("Volume").selectOption("root");
  await page.getByRole("button", { name: "Create", exact: true }).click();
```

`web/e2e/vm.spec.ts`에서

```ts
  await page.getByRole("button", { name: "Delete" }).click();
  const dialog = page.getByRole("alertdialog");
  await dialog.getByLabel("Type web to confirm").fill("web");
  await dialog.getByRole("button", { name: "Delete" }).click();
  await expect(page).toHaveURL(/\/vms$/);
  await expect(page.getByRole("link", { name: "web", exact: true })).toHaveCount(0);
```

을 다음으로 바꾼다:

```ts
  await page.getByRole("button", { name: "Delete" }).click();
  let confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type web to confirm").fill("web");
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page).toHaveURL(/\/vms$/);
  await expect(page.getByRole("link", { name: "web", exact: true })).toHaveCount(0);

  // 볼륨과 풀을 지운다. 볼륨은 삭제 Task가 끝나야 목록에서 사라진다.
  await page.getByRole("link", { name: "Volumes" }).click();
  await page.getByRole("button", { name: "Delete root" }).click();
  confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type root to confirm").fill("root");
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page.getByRole("cell", { name: "root" })).toHaveCount(0, { timeout: 60_000 });

  await page.getByRole("link", { name: "Storage pools" }).click();
  await page.getByRole("button", { name: "Delete local" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete" }).click();
  await expect(page.getByText("No storage pools.")).toBeVisible();
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 141개

Run: `cargo fmt --all --check && cargo clippy -p opld --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `pnpm -C web build && cargo test -p opld --features web-it --test web`
Expected: macOS에서 `browser_uploads_image_runs_vm_and_cleans_up ... ok`(약 9초), 남는 QEMU 없음

- [ ] **Step 3: 커밋**

```bash
git add web crates
git commit -m "test(web): E2E가 브라우저에서 풀 만들기·이미지 업로드부터 VM·볼륨·풀 삭제까지"
```

---

### Task 12: 문서

README의 웹 UI 설명(다룰 수 있는 기능, 클러스터 화면의 CA, 업로드는 접속한 노드로), E2E 명령 설명, 문서 목록에 계획 9를 더한다. 설계 문서에 구현하며 정한 것(클러스터 화면은 모든 역할이 보고 조인 토큰만 admin, 업로드는 화면을 옮겨도 계속되고 로그아웃하면 멈춤, 노드·사용자·내 비밀번호 화면의 규칙, 이어 올리기 없음)을 반영한다. CI는 바꾸지 않는다(`linux-kvm`이 이미 `opld/web-it`을 돌린다).

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-23-web-ui-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (M7, 웹 UI 진행 중)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI)
```

`README.md`에서

```markdown
-fingerprint -sha256`의 지문을 `cluster init` 출력과 비교) 운영체제나 브라우저의 신뢰할 인증서로
가져오면 경고가 사라진다.

지금 웹 UI는 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔), 작업을 다룬다. 볼륨·스토리지 풀·
네트워크·노드·사용자·클러스터 화면은 계획 9에서 더한다.
```

을 다음으로 바꾼다:

```markdown
-fingerprint -sha256`의 지문을 `cluster init` 출력과 비교) 운영체제나 브라우저의 신뢰할 인증서로
가져오면 경고가 사라진다. 로그인한 뒤에는 클러스터 화면에서 CA를 내려받고 지문을 볼 수 있다.

웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔),
볼륨(빈 볼륨, 이미지 업로드), 스토리지 풀, 네트워크, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
`oplctl cluster join`을 실행한다(클러스터 화면이 명령을 만들어 준다). 이미지는 지금 접속한 노드로 올라가므로
그 노드에서 쓸 수 있는 풀에만 가져올 수 있다.
```

`README.md`에서

````markdown
pnpm -C web exec playwright install chromium
pnpm -C web build && cargo test -p opld --features web-it --test web   # E2E: 브라우저 → VM → 콘솔
```
````

을 다음으로 바꾼다:

````markdown
pnpm -C web exec playwright install chromium
pnpm -C web build && cargo test -p opld --features web-it --test web   # E2E: 브라우저 → 업로드 → VM → 콘솔 → 정리
```
````

`README.md`에서

```markdown
- [계획 8: 웹 UI 기반과 가상 머신 화면](docs/superpowers/plans/2026-10-23-plan8-web-ui.md)
```

을 다음으로 바꾼다:

```markdown
- [계획 8: 웹 UI 기반과 가상 머신 화면](docs/superpowers/plans/2026-10-23-plan8-web-ui.md)
- [계획 9: 웹 UI 관리 화면](docs/superpowers/plans/2026-10-26-plan9-web-ui-admin.md)
```

`docs/superpowers/specs/2026-10-23-web-ui-design.md`에서

```markdown
| API 토큰 | 내 토큰 | 만들기(비밀값은 한 번만 표시)·삭제 |
| 클러스터 (admin) | 멤버·리더·CA 지문 | 조인 토큰 발급(복사용 명령 표시), CA 내려받기 |
```

을 다음으로 바꾼다:

```markdown
| API 토큰 | 내 토큰 | 만들기(비밀값은 한 번만 표시)·삭제 |
| 클러스터 | 멤버·리더·CA 지문 | 조인 토큰 발급(admin, 복사용 명령 표시), CA 내려받기 |
```

`docs/superpowers/specs/2026-10-23-web-ui-design.md`에서

```markdown
- **콘솔:** noVNC. 상세 페이지 탭과 새 탭(전체 화면). Ctrl+Alt+Del 보내기, 클립보드 입력(브라우저 클립보드의 글자를 키 입력으로 보낸다. QEMU VNC의 클립보드 공유는 게스트 에이전트가 필요하다).
- **볼륨 업로드:** API는 받은 노드에서 쓸 수 있는 풀로만 가져온다. 그래서 지금 접속한 노드의 풀만 고르게 한다(§8의 `GET /cluster`의 `node`). `XMLHttpRequest`로 보내 진행률과 취소를 지원하고, 업로드 중 페이지를 떠나면 확인한다.
- **되돌릴 수 없는 동작:** VM·볼륨 삭제, 노드 제거, 강제 정지는 확인 대화상자. 삭제와 노드 제거는 이름을 입력해야 한다.
- **API 토큰:** 만든 직후 한 번만 보여 주고 복사 버튼을 둔다.
```

을 다음으로 바꾼다:

```markdown
- **콘솔:** noVNC. 상세 페이지 탭과 새 탭(전체 화면). Ctrl+Alt+Del 보내기, 클립보드 입력(브라우저 클립보드의 글자를 키 입력으로 보낸다. QEMU VNC의 클립보드 공유는 게스트 에이전트가 필요하다).
- **볼륨 업로드:** API는 받은 노드에서 쓸 수 있는 풀로만 가져온다. 그래서 지금 접속한 노드의 풀만 고르게 한다(§8의 `GET /cluster`의 `node`). `XMLHttpRequest`로 보내 진행률과 취소를 지원하고, 업로드 중 페이지를 떠나면 확인한다. 업로드는 화면을 옮겨도 계속되며 진행률과 취소 버튼은 알림에 있다. 다 올리면 가져오기 Task를 다른 작업처럼 알림으로 지켜본다. 로그아웃하거나 세션이 끝나면 업로드를 멈춘다.
- **되돌릴 수 없는 동작:** VM·볼륨 삭제, 노드 제거, 강제 정지는 확인 대화상자. 삭제와 노드 제거는 이름을 입력해야 한다.
- **API 토큰:** 만든 직후 한 번만 보여 주고 복사 버튼을 둔다. 누구나 자기 토큰을 만들고 지운다(관리자는 모든 토큰).
- **클러스터:** 멤버·리더·CA는 모든 역할이 본다(API가 읽기를 허락하고, CA 지문은 인증서 확인에 필요하다). 조인 토큰 발급만 admin이다. 발급하면 새 노드에서 실행할 `oplctl cluster join '<URL>'` 명령을 한 번만 보여 준다.
- **노드:** 유지보수 모드는 `If-Match`로 바꾼다. 제거는 오프라인인 다른 노드만 고를 수 있다(살아 있는 노드는 그 노드에서 `oplctl cluster leave`).
- **사용자와 내 비밀번호:** admin이 아니면 사용자 메뉴를 숨긴다(operator·viewer는 계정을 읽지 못한다). 관리자는 다른 사용자의 비밀번호를 재설정하고, 자기 비밀번호는 계정 메뉴의 "내 비밀번호 변경"에서 현재 비밀번호와 함께 바꾼다. 서버가 그 사용자의 세션을 모두 끝내므로 바꾼 뒤 로그아웃한다.
```

`docs/superpowers/specs/2026-10-23-web-ui-design.md`에서

```markdown
- API에 페이지 나누기가 없다. 작업 목록은 모두 받아 화면에서 최근 순으로 자른다.
- 볼륨 업로드는 지금 접속한 노드에서 쓸 수 있는 풀로만 된다.
- noVNC 1.7은 기본으로 하위 프로토콜을 요청하지 않아 지금의 콘솔 WebSocket에 그대로 붙는다(계획 8 E2E로 확인).
```

을 다음으로 바꾼다:

```markdown
- API에 페이지 나누기가 없다. 작업 목록은 모두 받아 화면에서 최근 순으로 자른다.
- 볼륨 업로드는 지금 접속한 노드에서 쓸 수 있는 풀로만 된다. 한 요청이라 끊기면 처음부터 다시 올린다(이어 올리기 없음).
- noVNC 1.7은 기본으로 하위 프로토콜을 요청하지 않아 지금의 콘솔 WebSocket에 그대로 붙는다(계획 8 E2E로 확인).
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo deny check`
Expected: 경고 없음

Run: `cargo test --workspace --all-features`
Expected: macOS에서 PASS(실제 QEMU E2E와 브라우저 E2E 포함)

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test && pnpm -C web audit --prod && pnpm -C web build`
Expected: PASS, `No known vulnerabilities found`

Run: `pnpm -C web gen:api && git diff --exit-code web/src/api/schema.d.ts`
Expected: 차이 없음

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: 웹 UI 관리 화면 (README, 설계 문서에 구현하며 정한 것)"
```

---

## 완료 기준 (계획 9)

- 브라우저로 로그인해 스토리지 풀을 만들고, 이미지를 올려 볼륨으로 가져오고, 그 볼륨으로 VM을 만들어 시작해 콘솔에 화면이 그려지고, VM·볼륨·풀을 지우고 로그아웃하는 E2E가 macOS(hvf)에서 통과한다 (`pnpm -C web build && cargo test -p opld --features web-it --test web`). CI `linux-kvm`에서도 통과한다.
- CLI(`oplctl`)의 관리 기능(클러스터 상태·조인 토큰·노드 제거, 노드, 풀, 네트워크, 볼륨 만들기·가져오기·삭제, VM, 작업, 사용자, 토큰)을 모두 웹 UI로 할 수 있다. 노드 합류와 탈퇴(`cluster join`·`leave`)는 그 노드의 제어 소켓으로 하므로 제외한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.

## 알고 받아들인 제한

- **업로드는 접속한 노드로만:** 서버가 받은 노드에서 쓸 수 있는 풀로만 가져온다. 다른 노드의 풀에 넣으려면 그 노드의 주소로 접속한다.
- **이어 올리기 없음:** 업로드가 끊기면(네트워크, 새로 고침, 로그아웃) 서버가 만들던 볼륨과 받은 파일을 지우고, 처음부터 다시 올린다.
- **화면 안 이동은 확인하지 않는다:** 업로드는 화면을 옮겨도 계속되므로 확인하지 않고, 페이지를 떠날 때(새로 고침·닫기·다른 사이트)만 브라우저가 확인한다.
- **사용자와 토큰은 실시간이 아니다:** watch가 없어 다른 사람이 바꾼 것은 화면을 다시 열어야 보인다.
- **노드 제거:** 오프라인인 노드만 된다. 제거한 노드는 지금 데이터로 다시 합류할 수 없고, 그 노드에 배정된 VM은 그대로 남는다(서버 동작).
- **조인 명령:** 새 노드의 제어 소켓이 기본값(`/run/opl/opld.sock`)이 아니면 `--socket`을 붙여야 한다(대화상자에 안내).

## 실행 후 기록 (2026-10-26)

12개 Task는 계획대로 적용했고, Task마다 검증된 단계와 트리가 같았다. Task 리뷰, 수정 묶음의 재리뷰, 최종 리뷰를 거쳐 아래가 바뀌었다. 저장소의 코드가 기준이다.

수정 A(`92a785f`, 공통 부품):
- **도움말을 입력에 잇는다**(`aria-describedby`). 계획대로면 `Field`의 도움말을 스크린 리더가 읽지 않았다.
- **같은 틱의 두 번째 제출을 막는다**(`FormDialog`의 `useRef`). 계획대로면 Enter와 클릭이 겹치면 사용자·토큰 만들기가 두 번 갈 수 있었다.
- **메뉴에서 연 대화상자를 닫으면 메뉴 버튼으로 포커스를 돌린다**(`returnFocus`, `components/focusBack.ts`). 계획대로면 메뉴 항목이 사라져 포커스가 `body`로 갔다(VM·사용자·노드 메뉴, 계정 메뉴).
- **크기 입력은 바꾼 바이트로 검사한다**(`gibToBytes`). 계획대로면 `1e-10`, `Infinity`, `0x10`이 통과해 서버 오류가 그대로 보였다. VM 폼의 새 디스크 이름도 예약 이름 `import`를 거부한다.

수정 B(`c8fa46d`, 풀·네트워크·노드·볼륨):
- **다른 목록에 기대는 값은 그 목록을 다 읽은 뒤에만 판단한다.** 계획대로면 읽는 중이거나 실패했을 때 볼륨·VM 수가 0으로 보여 삭제가 켜지고, 노드를 읽기 전에 "브리지가 있는 노드 없음" 경고가 떴다. 그동안은 "—"이고 삭제는 꺼진다.
- **삭제할 수 없는 이유를 글로 보인다.** 계획대로면 비활성 버튼의 `title`에만 있어 마우스로도 키보드로도 볼 수 없었다. 노드 화면 아래에 제거 안내(오프라인 노드만, 살아 있는 노드는 `oplctl cluster leave`).
- **유지보수 응답을 바로 캐시에 넣는다**(`applyEvent`). 계획대로면 watch 이벤트 전에 다시 바꾸면 옛 revision으로 412가 났다.
- 확인 대화상자가 닫히는 동안 제목과 입력할 이름을 유지한다. VLAN은 숫자만. 풀 만들기는 사라진 노드를 보내지 않고, 고른 노드가 모두 사라지면 오류를 보인다(수정 F·G).

수정 C(`9030a38`, 업로드):
- **다 보낸 뒤에는 "처리 중"으로 바꾸고 취소를 뺀다.** 계획대로면 서버가 파일을 쓰고 Task를 만드는 동안 취소할 수 있어, "취소했다"고 보인 볼륨이 생길 수 있었다.
- **연결이 끊기면 "서버가 거절했거나 연결이 끊겼다"고 보인다.** 큰 파일을 서버가 본문 전에 거절하면 브라우저가 응답 대신 끊김을 알리기 때문이다(아래 제한).
- 202 본문에 Task id가 없거나 Task 넘기기가 실패하면 오류 알림으로 끝난다. 진행률은 5% 단위, 옛 브라우저의 이탈 확인(`returnValue`), `.qcow`는 qcow2로 보지 않는다, 접속한 노드를 모르면 안내.

수정 D(`9d14499`, 계정·클러스터):
- **자기 역할은 바꿀 수 없다.** 계획대로면 admin이 자기를 낮춰도 화면의 권한이 admin으로 남아 모든 동작이 "권한 없음"으로 실패했다. 자기 행 아래에 까닭을 안내한다(수정 G).
- **비밀값(API 토큰, 조인 명령) 대화상자는 "완료"로만 닫는다.** 계획대로면 Esc나 바깥 클릭으로 닫혀 다시 볼 수 없었다(조인 명령은 수정 G).
- **클러스터 화면은 열려 있는 동안 10초마다 다시 읽는다.** 계획대로면 멤버·리더·온라인이 watch 대상이 아니라 새 노드가 합류해도 보이지 않았다. 다시 읽지 못하면 한 줄로 알린다.
- admin이 아닌 사용자가 `/users`로 들어오면 안내만 보인다. 틀린 현재 비밀번호면 그 칸에 포커스. 조인 명령의 `'` 이스케이프. 실패할 수 없던 테스트(토큰 화면의 `/users` 미요청, 비밀값 사라짐)를 다시 썼다.

수정 E(`371a9bb`, E2E): 볼륨을 지운 뒤 풀 디렉터리가 비었는지(파일까지 지웠는지) 확인한다. 실패 정리의 볼륨 삭제는 30초만 기다린다(`oplctl`이 Task를 15분까지 기다린다). 가져온 형식 qcow2 확인, `root` 칸은 정확히, 제한 시간 300초.

수정 F(`6cd42c5`)와 G(`19457b3`, `b0eae44`): 재리뷰와 최종 리뷰의 후속. 코드를 되돌려도 통과하던 테스트(5% 진행률, Task 넘기기 실패, 비밀값 대화상자 바깥 클릭)를 실패하게 고쳤다. 볼륨 만들기·업로드는 풀 목록의 로딩·오류를 보인다. 노드 화면의 VM 수는 VM 목록을 읽은 뒤에만, 안내는 쓰기 권한이 있을 때만. 설계 문서 §5·§10에 클러스터 갱신과 아래 제한을 적었다.

알고 받아들인 동작(위 제한 목록에 더해):
- 큰 파일을 서버가 본문 전에 거절하면(권한·이름·풀·공간) "서버가 거절했거나 연결이 끊겼다"로만 보인다. 올리기 전에 확인하는 API는 나중이다.
- 다 보낸 뒤 서버가 응답하지 않으면 "처리 중" 알림과 이탈 확인이 새로 고침이나 로그아웃 전까지 남는다.
- Task 넘기기가 실패해도 서버의 가져오기는 돌고 있을 수 있다(알림은 실패로 보인다).
- 웹 UI에서 자기 역할은 바꿀 수 없고(다른 관리자가 바꾼다), 그래서 `last_admin` 오류에 이르는 화면 경로가 없다(번역은 CLI·API용으로 둔다).
- 비밀값 대화상자를 닫은 뒤 포커스는 페이지로 간다. 새 관리 화면은 VM 화면의 번역 키 일부(`vms.actions` 등)를 빌려 쓴다.
- 서버는 현재 비밀번호 확인에 시도 제한이 없다(세션을 가진 사람이 추측할 수 있다).

테스트: macOS `cargo test --workspace --all-features` 256개(실제 QEMU E2E와 브라우저 E2E 포함, 브라우저 E2E 약 9초), 웹 Vitest 177개, `fmt`·`clippy -D warnings`(모든 기능)·`cargo deny`·`pnpm audit --prod`, API 타입 동기화.
CI(확인용 브랜치): `check` 2개, `deny`, `web`, `linux-kvm`이 모두 통과했다(`linux-kvm`은 실제 KVM에서 root로 브라우저 E2E를 돌린다, 약 4분).

## 다음 계획으로 넘길 사항

- 업로드 사전 확인 API(권한·이름·풀·공간을 본문 전에 확인해 거절 이유를 보이기), 처리 중 응답 제한 시간.
- 현재 비밀번호 확인과 로그인의 시도 제한.
- 노드 아키텍처를 API로 알리기(머신 기본값), 작업 목록 페이지 나누기, `index.html` ETag, VM 폼 컴포넌트 나누기(계획 8에서 넘김).
- 빌린 번역 키를 `common.*`로, 비밀값 대화상자 뒤 포커스.
