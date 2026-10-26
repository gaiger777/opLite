import { useTranslation } from "react-i18next";

import { Label } from "@/components/ui/label";

/** 입력에 붙일 접근성 속성. 오류나 도움말이 있을 때만 값이 있다. */
export interface FieldA11y {
  "aria-invalid"?: true;
  "aria-describedby"?: string;
}

/**
 * 이름표와 입력, 그 아래의 오류(번역 키)와 도움말. 오류나 도움말이 있는 입력은 `children`에 함수를
 * 주어 `FieldA11y`를 받는다.
 */
export function Field({
  id,
  label,
  error,
  hint,
  children,
}: {
  id: string;
  label: string;
  error?: string;
  hint?: string;
  children: React.ReactNode | ((a11y: FieldA11y) => React.ReactNode);
}) {
  const { t } = useTranslation();
  const errorId = `${id}-error`;
  const hintId = `${id}-hint`;
  const a11y: FieldA11y = error
    ? { "aria-invalid": true, "aria-describedby": errorId }
    : hint
      ? { "aria-describedby": hintId }
      : {};
  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      {typeof children === "function" ? children(a11y) : children}
      {hint && !error && (
        <p id={hintId} className="text-xs text-muted-foreground">
          {hint}
        </p>
      )}
      {error && (
        <p id={errorId} className="text-xs text-destructive">
          {t(error)}
        </p>
      )}
    </div>
  );
}
