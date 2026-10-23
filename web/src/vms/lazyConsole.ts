import { lazy } from "react";

// noVNC는 콘솔을 열 때만 내려받는다.
export const VmConsole = lazy(() => import("@/vms/VmConsole"));
