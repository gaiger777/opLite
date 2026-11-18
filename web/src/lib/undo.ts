import type { TFunction } from "i18next";
import { toast } from "sonner";

/** 되돌리기 알림이 남는 시간(설계 §5.8) */
export const UNDO_MS = 10_000;

/**
 * 브라우저나 `If-Match`로 되돌릴 수 있는 고치기 뒤의 알림. "되돌리기"를 누르면 `undo`를 부른다.
 * 서버 Task가 지운 것에는 쓰지 않는다.
 */
export function undoToast(t: TFunction, message: string, undo: () => void | Promise<void>) {
  toast.success(message, {
    duration: UNDO_MS,
    action: { label: t("common.undo"), onClick: () => void undo() },
  });
}
