import { can } from "@/auth/permissions";

it("mirrors the server roles", () => {
  expect(can("admin", "write", "network")).toBe(true);
  expect(can("operator", "operate", "vm")).toBe(true);
  expect(can("operator", "write", "volume")).toBe(true);
  expect(can("operator", "write", "network")).toBe(false);
  expect(can("viewer", "read", "vm")).toBe(true);
  expect(can("viewer", "operate", "vm")).toBe(false);
  // 스냅샷 목록은 누구나 읽는다. 만들기·되돌리기·삭제는 서버처럼 VM 동작 권한으로 따진다.
  expect(can("viewer", "read", "snapshot")).toBe(true);
  // 보안 그룹은 누구나 읽고 admin만 바꾼다.
  expect(can("viewer", "read", "security-group")).toBe(true);
  expect(can("operator", "write", "security-group")).toBe(false);
  expect(can("admin", "write", "security-group")).toBe(true);
  // 백업은 누구나 읽고 operator도 지우고 되살린다(서버의 백업 쓰기 권한).
  expect(can("viewer", "read", "backup")).toBe(true);
  expect(can("viewer", "write", "backup")).toBe(false);
  expect(can("operator", "write", "backup")).toBe(true);
  // 백업 작업도 백업과 같다.
  expect(can("viewer", "read", "backup-job")).toBe(true);
  expect(can("viewer", "write", "backup-job")).toBe(false);
  expect(can("operator", "write", "backup-job")).toBe(true);
  // VM 가져오기도 같다.
  expect(can("viewer", "read", "vm-import")).toBe(true);
  expect(can("viewer", "write", "vm-import")).toBe(false);
  expect(can("operator", "write", "vm-import")).toBe(true);
  // 클러스터 설정은 누구나 읽고 admin만 바꾼다.
  expect(can("viewer", "read", "settings")).toBe(true);
  expect(can("operator", "write", "settings")).toBe(false);
  expect(can("admin", "write", "settings")).toBe(true);
  // 원격 클러스터도 누구나 읽고 admin만 바꾼다(중계로 원격을 바꾸는 요청도).
  expect(can("viewer", "read", "remote")).toBe(true);
  expect(can("operator", "write", "remote")).toBe(false);
  expect(can("admin", "write", "remote")).toBe(true);
});

it("keeps accounts to admins", () => {
  expect(can("admin", "read", "user")).toBe(true);
  expect(can("admin", "write", "join-token")).toBe(true);
  expect(can("admin", "write", "realm")).toBe(true);
  for (const role of ["operator", "viewer"] as const) {
    expect(can(role, "read", "user")).toBe(false);
    expect(can(role, "read", "api-token")).toBe(false);
    expect(can(role, "write", "join-token")).toBe(false);
    // 인증 영역도 계정 종류다(서비스 계정 비밀번호를 다룬다).
    expect(can(role, "read", "realm")).toBe(false);
    expect(can(role, "write", "realm")).toBe(false);
  }
});
