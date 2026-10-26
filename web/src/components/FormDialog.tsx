import { useState } from "react";
import { useTranslation } from "react-i18next";

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
  onClose,
  onSubmit,
  children,
}: {
  title: string;
  description?: string;
  submitLabel: string;
  destructive?: boolean;
  onClose(): void;
  /** 보낸다. 입력 오류로 보내지 않았으면 false를 돌려준다(열어 둔다). 실패하면 던진다. */
  onSubmit(): Promise<boolean | void>;
  children: React.ReactNode;
}) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      if ((await onSubmit()) !== false) {
        onClose();
      }
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent {...(description ? {} : { "aria-describedby": undefined })}>
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
              {t("confirm.cancel")}
            </Button>
            <Button type="submit" variant={destructive ? "destructive" : "default"} disabled={busy}>
              {submitLabel}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
