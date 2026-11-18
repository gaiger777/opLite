import { cn } from "cn";
import type { LucideIcon } from "lucide-react";
import { useEffect, useState } from "react";

/** 상태의 뜻. 색은 `index.css`의 상태 토큰(`--{tone}`·`-soft`·`-fg`)으로만 쓴다. */
export type StatusTone = "success" | "warning" | "danger" | "info" | "neutral";

const CHIP: Record<StatusTone, string> = {
  success: "bg-success-soft text-success-fg",
  warning: "bg-warning-soft text-warning-fg",
  danger: "bg-danger-soft text-danger-fg",
  info: "bg-info-soft text-info-fg",
  neutral: "bg-muted text-muted-foreground",
};

const DOT: Record<StatusTone, string> = {
  success: "bg-success",
  warning: "bg-warning",
  danger: "bg-danger",
  info: "bg-info",
  neutral: "bg-muted-foreground",
};

/** 바뀐 칩을 강조하는 시간(설계 §5.5) */
export const CHANGED_MS = 600;

/** 값이 바뀐 뒤(처음 그릴 때는 빼고) `CHANGED_MS` 동안 참이다. */
export function useChanged(value: string): boolean {
  const [last, setLast] = useState(value);
  const [changed, setChanged] = useState(false);
  if (last !== value) {
    setLast(value);
    setChanged(true);
  }
  useEffect(() => {
    if (!changed) {
      return;
    }
    const timer = setTimeout(() => setChanged(false), CHANGED_MS);
    return () => clearTimeout(timer);
  }, [changed, last]);
  return changed;
}

/**
 * 상태 칩: 색 + 점(또는 아이콘) + 글. 색만으로 뜻을 전하지 않도록 글은 늘 있다(설계 §5.3).
 * `busy`는 바뀌는 중(시작·옮기기 …)인 상태이고 점이 깜박인다(움직임 줄이기면 멈춤).
 * watch로 상태(색·글)가 바뀌면 잠깐 `data-changed`로 강조한다(움직임 줄이기면 꺼짐).
 */
export function StatusChip({
  tone,
  icon: Icon,
  busy = false,
  className,
  children,
  ...props
}: React.ComponentProps<"span"> & {
  tone: StatusTone;
  /** 점 대신 보일 아이콘 */
  icon?: LucideIcon;
  busy?: boolean;
}) {
  const changed = useChanged(`${tone}|${typeof children === "string" ? children : ""}`);
  return (
    <span
      data-slot="status-chip"
      data-tone={tone}
      data-changed={changed ? "" : undefined}
      className={cn(
        "inline-flex h-5 w-fit shrink-0 items-center gap-1.5 rounded-md px-2 text-xs font-medium whitespace-nowrap",
        CHIP[tone],
        className,
      )}
      {...props}
    >
      {Icon ? (
        <Icon aria-hidden className="size-3" />
      ) : (
        <span
          aria-hidden
          className={cn("size-1.5 shrink-0 rounded-full", DOT[tone], busy && "animate-pulse")}
        />
      )}
      {children}
    </span>
  );
}
