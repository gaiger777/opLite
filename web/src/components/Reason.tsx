import { cn } from "cn";
import { useId, type ComponentProps } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";

/**
 * 막힐 수 있는 메뉴 항목. `reason`(번역 키)이 있으면 `disabled` 대신 `aria-disabled`로 막아, 키보드로
 * 닿고 화면 읽기 프로그램이 이름·비활성·이유를 읽게 한다. 골라도 동작하지 않고 메뉴도 닫히지 않는다.
 * 이름이 문자열이면 글자 입력으로 항목을 찾을 때(typeahead) 이유 글씨는 보지 않게 이름만 쓴다.
 */
export function ReasonMenuItem({
  reason,
  onSelect,
  className,
  children,
  ...props
}: Omit<ComponentProps<typeof DropdownMenuItem>, "disabled"> & { reason?: string | null }) {
  const { t } = useTranslation();
  const id = useId();
  return (
    <DropdownMenuItem
      textValue={typeof children === "string" ? children : undefined}
      {...props}
      aria-disabled={reason ? true : undefined}
      aria-describedby={reason ? id : undefined}
      className={cn(reason && "opacity-50", className)}
      onSelect={(e) => {
        if (reason) {
          e.preventDefault();
          return;
        }
        onSelect?.(e);
      }}
    >
      {children}
      {/* 이름에 섞이지 않게 숨기고 aria-describedby로만 읽힌다. */}
      {reason && (
        <span id={id} aria-hidden className="ml-auto pl-2 text-xs text-muted-foreground">
          {t(reason)}
        </span>
      )}
    </DropdownMenuItem>
  );
}

/** 이유 글씨의 id. 같은 이유로 막힌 버튼들은 같은 글씨를 가리킨다. */
function reasonId(base: string, reason: string) {
  return `${base}-${reason}`;
}

/**
 * 막힐 수 있는 버튼. `reason`(번역 키)이 있으면 `disabled` 대신 `aria-disabled`로 막아 탭 순서에 남기고,
 * 눌러도 동작하지 않는다(폼 안에서도 보내지 않는다). 이유 글씨는 같은 `reasons`(`useId()`)를 받은
 * `ReasonList`가 보이고 `aria-describedby`로 잇는다.
 */
export function ReasonButton({
  reason,
  reasons,
  onClick,
  className,
  ...props
}: Omit<ComponentProps<typeof Button>, "disabled"> & { reason?: string | null; reasons: string }) {
  return (
    <Button
      {...props}
      aria-disabled={reason ? true : undefined}
      aria-describedby={reason ? reasonId(reasons, reason) : undefined}
      className={cn(reason && "opacity-50", className)}
      onClick={(e) => {
        if (reason) {
          e.preventDefault();
          return;
        }
        onClick?.(e);
      }}
    />
  );
}

/** 막힌 `ReasonButton`들의 이유. 같은 이유는 한 번만 보이고, 막힌 것이 없으면 그리지 않는다. */
export function ReasonList({
  base,
  reasons,
  className,
}: {
  base: string;
  reasons: readonly (string | null | undefined)[];
  className?: string;
}) {
  const { t } = useTranslation();
  const shown = [...new Set(reasons.filter((r): r is string => !!r))];
  if (shown.length === 0) {
    return null;
  }
  return (
    <div className={cn("text-xs text-muted-foreground", className)}>
      {shown.map((reason) => (
        <p key={reason} id={reasonId(base, reason)}>
          {t(reason)}
        </p>
      ))}
    </div>
  );
}
