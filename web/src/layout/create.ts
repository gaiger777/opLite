// "+ 만들기" 메뉴와 팔레트의 만들기 명령(설계 §5.1). 만들기 화면이 따로 있는 VM 말고는 그 목록 화면의
// `?create=1`이 대화상자를 연다(`useCreateParam`).
import type { Role } from "@/api/types";
import { can, type Kind } from "@/auth/permissions";

export interface CreateItem {
  id: "vm" | "import" | "pool" | "network" | "backup-job" | "iso";
  to: string;
  /** 쓰기 권한을 따지는 종류 */
  kind: Kind;
}

export const CREATE_ITEMS: readonly CreateItem[] = [
  { id: "vm", to: "/vms/-new", kind: "vm" },
  { id: "import", to: "/imports?create=1", kind: "vm-import" },
  { id: "pool", to: "/storage-pools?create=1", kind: "storage-pool" },
  { id: "network", to: "/networks?create=1", kind: "network" },
  { id: "backup-job", to: "/backup-jobs?create=1", kind: "backup-job" },
  { id: "iso", to: "/isos?create=1", kind: "volume" },
];

/** 이 역할이 만들 수 있는 것 */
export function createItems(role: Role): CreateItem[] {
  return CREATE_ITEMS.filter((item) => can(role, "write", item.kind));
}
