import { useTranslation } from "react-i18next";

import { InfoButton } from "@/components/InfoButton";
import { Label } from "@/components/ui/label";

/** 입력에 붙일 접근성 속성. 오류나 도움말이 있을 때만 값이 있다. */
export interface FieldA11y {
  "aria-invalid"?: true;
  "aria-describedby"?: string;
}

/**
 * 이름표와 입력, 그 아래의 한 줄 설명(`help` — 오류가 나도 남는다)과 오류(번역 키, 끼울 값은 `errorValues`). `info`
 * (도움말 주제 `helpInfo.<info>`)를 주면 이름표 옆 ⓘ가 오른쪽 도움말 패널을 연다(계획 77-4). 설명이나 오류가 있는
 * 입력은 `children`에 함수를 주어 `FieldA11y`를 받는다.
 */
export function Field({
  id,
  label,
  error,
  errorValues,
  help,
  info,
  children,
}: {
  id: string;
  label: string;
  error?: string;
  errorValues?: Record<string, string>;
  /** 늘 보이는 한 줄: 무엇 + 기본값·권장 + 바꾸면 무엇이 달라지나 */
  help?: string;
  /** 도움말 패널 주제(`helpInfo.<info>` — 예: `pools.kind`) */
  info?: string;
  children: React.ReactNode | ((a11y: FieldA11y) => React.ReactNode);
}) {
  const { t } = useTranslation();
  const errorId = `${id}-error`;
  const helpId = `${id}-hint`;
  const described = [help && helpId, error && errorId].filter(Boolean).join(" ");
  const a11y: FieldA11y = {
    ...(error && { "aria-invalid": true }),
    ...(described && { "aria-describedby": described }),
  };
  return (
    <div className="flex flex-col gap-1.5">
      {info ? (
        <div className="flex items-center gap-1">
          <Label htmlFor={id}>{label}</Label>
          <InfoButton topic={info} label={label} line={help} className="-my-1" />
        </div>
      ) : (
        <Label htmlFor={id}>{label}</Label>
      )}
      {typeof children === "function" ? children(a11y) : children}
      {help && (
        <p id={helpId} className="text-xs text-muted-foreground">
          {help}
        </p>
      )}
      {error && (
        <p id={errorId} className="text-xs text-destructive">
          {t(error, errorValues)}
        </p>
      )}
    </div>
  );
}
