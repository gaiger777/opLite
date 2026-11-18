import { createContext, type RefObject, useContext } from "react";

import type { Vm } from "@/api/types";
import type { VmDialog } from "@/vms/VmMenu";

/** VM 대화상자 하나를 여는 요청 */
export interface VmDialogRequest {
  vm: Vm;
  which: VmDialog;
  /** 옮기기 대화상자에 미리 고를 노드 id */
  node?: string;
  /** 닫으면 포커스를 돌려줄 곳 */
  returnFocus?: RefObject<HTMLElement | null>;
  /** 지운 뒤(상세 화면은 목록으로 간다) */
  afterRemove?(): void;
}

export interface VmActionHostValue {
  open(request: VmDialogRequest): void;
}

export const VmActionHostContext = createContext<VmActionHostValue | null>(null);

/**
 * VM 대화상자를 여는 곳(설계 §5.3 `VmActionHost`). 행 메뉴·상세 머리·팔레트가 같은 확인 대화상자를 연다.
 */
export function useVmActionHost(): VmActionHostValue {
  const host = useContext(VmActionHostContext);
  if (!host) {
    throw new Error("useVmActionHost outside VmActionHost");
  }
  return host;
}
