import type { Role, WatchKind } from "@/api/types";

/** 읽기, 쓰기(만들기·고치기·지우기), 동작(시작·정지와 콘솔) */
export type Action = "read" | "write" | "operate";

/**
 * 서버의 `RoleAuthorizer`와 같은 규칙으로 버튼과 메뉴를 숨긴다. 실제 권한 확인은 서버가 한다.
 * operator는 VM과 볼륨만 바꾸고, viewer는 읽기만 한다.
 */
export function can(role: Role, action: Action, kind: WatchKind): boolean {
  switch (role) {
    case "admin":
      return true;
    case "operator":
      return action === "read" || kind === "vm" || kind === "volume";
    case "viewer":
      return action === "read";
  }
}
