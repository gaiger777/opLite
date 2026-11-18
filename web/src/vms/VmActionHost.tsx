import { type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { useLocation } from "react-router";

import { useList } from "@/api/live";
import { useVmActions } from "@/vms/actions";
import { VmActionHostContext, type VmDialogRequest } from "@/vms/actionHost";
import { VmConfirm } from "@/vms/VmMenu";

/**
 * VM 확인·동작 대화상자를 화면 하나 위에 둔다(설계 §5.3). 다른 화면으로 가면 닫는다.
 */
export function VmActionHost({ children }: { children: ReactNode }) {
  const [request, setRequest] = useState<VmDialogRequest | null>(null);
  // 닫히는 동안에도 대상 VM과 글을 남겨 둔다.
  const [open, setOpen] = useState(false);
  const { pathname } = useLocation();
  const value = useMemo(
    () => ({
      open(next: VmDialogRequest) {
        setRequest(next);
        setOpen(true);
      },
    }),
    [],
  );
  const shown = useRef(pathname);
  useEffect(() => {
    if (shown.current !== pathname) {
      shown.current = pathname;
      setOpen(false);
    }
  }, [pathname]);
  return (
    <VmActionHostContext.Provider value={value}>
      {children}
      {request && <HostDialog request={request} open={open} onClose={() => setOpen(false)} />}
    </VmActionHostContext.Provider>
  );
}

/** 연 VM을 실시간 목록의 지금 값으로 그린다(목록에서 빠지면 연 때의 값). 연 뒤에만 목록을 본다. */
function HostDialog({
  request,
  open,
  onClose,
}: {
  request: VmDialogRequest;
  open: boolean;
  onClose(): void;
}) {
  const vms = useList("vm");
  const { run, remove } = useVmActions();
  const vm = vms.data?.find((v) => v.id === request.vm.id) ?? request.vm;
  return (
    <VmConfirm
      vm={vm}
      which={open ? request.which : null}
      node={request.node}
      onClose={onClose}
      onRun={run}
      onRemove={remove}
      afterRemove={request.afterRemove}
      returnFocus={request.returnFocus}
    />
  );
}
