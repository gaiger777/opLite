import { useId, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";

import { focusBack } from "@/components/focusBack";
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
  /** 확인 버튼을 막는다. 까닭은 `children`에 글로 보여 준다. */
  confirmDisabled?: boolean;
  /** 확인 전에 고를 것이나 알릴 것(요청 중에는 막는다) */
  children?: React.ReactNode;
  /** 닫으면 포커스를 돌려줄 곳(메뉴에서 열었으면 메뉴 버튼) */
  returnFocus?: RefObject<HTMLElement | null>;
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
  // 닫히는 애니메이션 동안 부모가 대상을 비워도 마지막 제목과 이름을 그대로 보인다.
  // jsdom엔 닫힘 애니메이션이 없어 테스트하지 않는다.
  const [shown, setShown] = useState({ title: props.title, name: props.typeToConfirm });
  if (props.open && (shown.title !== props.title || shown.name !== props.typeToConfirm)) {
    setShown({ title: props.title, name: props.typeToConfirm });
  }
  const ready = shown.name === undefined || typed === shown.name;

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
      <AlertDialogContent onCloseAutoFocus={(e) => focusBack(e, props.returnFocus)}>
        <AlertDialogHeader>
          <AlertDialogTitle>{shown.title}</AlertDialogTitle>
          <AlertDialogDescription>{props.description}</AlertDialogDescription>
        </AlertDialogHeader>
        {shown.name !== undefined && (
          <div className="flex flex-col gap-2">
            <Label htmlFor={inputId}>{t("confirm.typeName", { name: shown.name })}</Label>
            <Input
              id={inputId}
              autoComplete="off"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
            />
          </div>
        )}
        {props.children && (
          <fieldset disabled={busy} className="flex min-w-0 flex-col gap-2">
            {props.children}
          </fieldset>
        )}
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}
        <AlertDialogFooter>
          <AlertDialogCancel disabled={busy}>{t("common.cancel")}</AlertDialogCancel>
          <Button
            variant="destructive"
            disabled={!ready || busy || props.confirmDisabled}
            onClick={() => void confirm()}
          >
            {props.confirmLabel}
          </Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
