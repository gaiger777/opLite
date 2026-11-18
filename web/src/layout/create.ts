// "+ 만들기" 메뉴와 팔레트의 만들기 명령(설계 §5.1). 만들기 화면이 따로 있는 VM 말고는 그 목록 화면의
// `?create=1`이 대화상자를 연다(`useCreateParam`). 계획 62: 폴더에만 권한이 있어도 그 폴더에 만들 수 있으므로
// 어디서든(루트·폴더) 그 권한이 있으면 보인다. 만들기 화면이 권한 있는 폴더를 골라 둔다.
import type { Permissions, Privilege } from "@/auth/permissions";

export interface CreateItem {
  id: "vm" | "import" | "pool" | "network" | "backup-job" | "iso";
  to: string;
  /** 만드는 데 필요한 권한 */
  privilege: Privilege;
  /** 루트에만 만든다(폴더가 없는 종류) */
  root?: boolean;
}

export const CREATE_ITEMS: readonly CreateItem[] = [
  { id: "vm", to: "/vms/-new", privilege: "vm.create" },
  { id: "import", to: "/imports?create=1", privilege: "vm.create" },
  { id: "pool", to: "/storage-pools?create=1", privilege: "storage.config" },
  { id: "network", to: "/networks?create=1", privilege: "network.config" },
  { id: "backup-job", to: "/backup-jobs?create=1", privilege: "backup.job", root: true },
  { id: "iso", to: "/isos?create=1", privilege: "storage.allocate" },
];

/** 이 사용자가 만들 수 있는 것 */
export function createItems(perms: Pick<Permissions, "has" | "anywhere">): CreateItem[] {
  return CREATE_ITEMS.filter((item) =>
    item.root ? perms.has(item.privilege) : perms.anywhere(item.privilege),
  );
}
