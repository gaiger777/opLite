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
  /**
   * 주면 이것을 그대로 입력해야 진행한다(되돌릴 수 없는 삭제). 하나면 이름, 여럿이면 수다(설계 §5.8).
   */
  typeToConfirm?: string | number;
  /** 영향 목록: 지울 디스크·스냅샷 수, 쓰는 VM … 이미 받은 목록에서 센 것(설계 §5.8) */
  impact?: string[];
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
  const ready =
    shown.name === undefined ||
    (typeof shown.name === "number" ? typed.trim() : typed) === String(shown.name);

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
        {props.impact && props.impact.length > 0 && <ImpactList items={props.impact} />}
        {shown.name !== undefined && (
          <TypeToConfirm id={inputId} expected={shown.name} value={typed} onChange={setTyped} />
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

/** 확인 대화상자의 영향 목록 */
export function ImpactList({ items }: { items: string[] }) {
  const { t } = useTranslation();
  return (
    <ul
      aria-label={t("confirm.impact")}
      className="flex list-disc flex-col gap-1 rounded-md border bg-surface-1 py-2 pr-3 pl-7 text-sm"
    >
      {items.map((item) => (
        <li key={item}>{item}</li>
      ))}
    </ul>
  );
}

/** 이름(하나)이나 수(여럿)를 입력하게 하는 칸. 일괄 대화상자도 쓴다. */
export function TypeToConfirm({
  id,
  expected,
  value,
  onChange,
}: {
  id: string;
  expected: string | number;
  value: string;
  onChange(value: string): void;
}) {
  const { t } = useTranslation();
  const count = typeof expected === "number";
  return (
    <div className="flex flex-col gap-2">
      <Label htmlFor={id}>
        {count
          ? t("confirm.typeCount", { count: expected })
          : t("confirm.typeName", { name: expected })}
      </Label>
      <Input
        id={id}
        autoComplete="off"
        inputMode={count ? "numeric" : undefined}
        value={value}
        onChange={(e) => onChange(e.target.value)}
      />
    </div>
  );
}
