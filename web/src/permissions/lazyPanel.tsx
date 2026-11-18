import { lazy } from "react";

/** 권한 탭은 열 때 내려받는다(설계 §9 묶음 크기). */
export const PermissionsPanel = lazy(async () => ({
  default: (await import("@/permissions/PermissionsPanel")).PermissionsPanel,
}));
