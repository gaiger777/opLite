import type { Role, WatchKind } from "@/api/types";

/** 읽기, 쓰기(만들기·고치기·지우기), 동작(시작·정지와 콘솔) */
export type Action = "read" | "write" | "operate";

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
    case "admin":
      return true;
    case "operator":
      return action === "read" ? !account : kind === "vm" || kind === "volume";
    case "viewer":
      return action === "read" && !account;
  }
}
