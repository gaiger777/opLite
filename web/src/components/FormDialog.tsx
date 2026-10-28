import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";

import { focusBack } from "@/components/focusBack";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { errorMessage } from "@/i18n/errors";

/**
 * 대화상자 하나의 폼. 연 쪽은 열 때만 그리고(`{open && <FormDialog ... />}`) 닫으면 지우므로 입력이 매번
 * 새로 시작한다. 보내는 동안은 입력과 닫기를 막고, 실패하면 서버의 오류를 폼 위에 보여 준다.
 */
export function FormDialog({
  title,
  description,
  submitLabel,
  destructive = false,
  submitDisabled = false,
  returnFocus,
  onClose,
  onSubmit,
  children,
}: {
  title: string;
  description?: string;
  submitLabel: string;
  destructive?: boolean;
  /** 보낼 것이 없으면(바뀐 것이 없으면) 보내기 버튼을 막는다. */
  submitDisabled?: boolean;
  /** 닫으면 포커스를 돌려줄 곳. 메뉴에서 열면 메뉴 항목이 사라지므로 메뉴 버튼을 준다. */
  returnFocus?: RefObject<HTMLElement | null>;
  onClose(): void;
  /** 보낸다. 입력 오류로 보내지 않았으면 false를 돌려준다(열어 둔다). 실패하면 던진다. */
  onSubmit(): Promise<boolean | void>;
  children: React.ReactNode;
}) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // busy는 다음 그리기 뒤에야 버튼을 막으므로 같은 틱의 두 번째 제출은 이것으로 막는다.
  const inFlight = useRef(false);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (inFlight.current) {
      return;
    }
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      if ((await onSubmit()) !== false) {
        onClose();
      }
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent
        {...(description ? {} : { "aria-describedby": undefined })}
        onCloseAutoFocus={(e) => focusBack(e, returnFocus)}
      >
        <form noValidate onSubmit={(e) => void submit(e)} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            {description && <DialogDescription>{description}</DialogDescription>}
          </DialogHeader>
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          <fieldset disabled={busy} className="flex min-w-0 flex-col gap-4">
            {children}
          </fieldset>
          <DialogFooter>
            <Button type="button" variant="outline" disabled={busy} onClick={onClose}>
              {t("common.cancel")}
            </Button>
            <Button
              type="submit"
              variant={destructive ? "destructive" : "default"}
              disabled={busy || submitDisabled}
            >
              {submitLabel}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
