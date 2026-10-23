import { useId, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { errorMessage } from "@/i18n/errors";

interface Props {
  open: boolean;
  onOpenChange(open: boolean): void;
  title: string;
  description: string;
  confirmLabel: string;
  /** 주면 이 이름을 그대로 입력해야 진행한다(되돌릴 수 없는 삭제). */
  typeToConfirm?: string;
  /** 실패하면 던진다. 대화상자가 오류를 보여 준다. */
  onConfirm(): Promise<void>;
}

/** 되돌릴 수 없는 동작을 확인한다. */
export function ConfirmDialog(props: Props) {
  const { t } = useTranslation();
  const [typed, setTyped] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inputId = useId();
  const ready = props.typeToConfirm === undefined || typed === props.typeToConfirm;

  function openChange(open: boolean) {
    // 요청 중에는 닫지 않는다(결과와 오류를 보여 줄 곳이 사라진다).
    if (!open && busy) {
      return;
    }
    if (!open) {
      setTyped("");
      setError(null);
    }
    props.onOpenChange(open);
  }

  async function confirm() {
    setBusy(true);
    setError(null);
    try {
      await props.onConfirm();
      setTyped("");
      props.onOpenChange(false);
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <AlertDialog open={props.open} onOpenChange={openChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{props.title}</AlertDialogTitle>
          <AlertDialogDescription>{props.description}</AlertDialogDescription>
        </AlertDialogHeader>
        {props.typeToConfirm !== undefined && (
          <div className="flex flex-col gap-2">
            <Label htmlFor={inputId}>{t("confirm.typeName", { name: props.typeToConfirm })}</Label>
            <Input
              id={inputId}
              autoComplete="off"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
            />
          </div>
        )}
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}
        <AlertDialogFooter>
          <AlertDialogCancel disabled={busy}>{t("confirm.cancel")}</AlertDialogCancel>
          <Button variant="destructive" disabled={!ready || busy} onClick={() => void confirm()}>
            {props.confirmLabel}
          </Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
