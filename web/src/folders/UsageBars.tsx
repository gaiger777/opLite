import { useQuery } from "@tanstack/react-query";
import { cn } from "cn";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import type { FolderKind, FolderUsageView } from "@/api/types";
import { formatBytes, formatMib } from "@/lib/format";

export const DIMENSIONS = [
  "vcpus",
  "memory_mib",
  "disk_bytes",
  "vms",
  "dedicated_vcpus",
  "networks",
  "routers",
  "floating_ips",
  "load_balancers",
] as const;
export type Dimension = (typeof DIMENSIONS)[number];

/** 폴더 종류마다 받는 할당량 차원(서버 `FolderKind::quota_dimensions` — 네트워크 폴더는 계획 66·67) */
export function dimensionsOf(kind: FolderKind): readonly Dimension[] {
  switch (kind) {
    case "vm":
      return ["vcpus", "memory_mib", "disk_bytes", "vms", "dedicated_vcpus"];
    case "network":
      return ["networks", "routers", "floating_ips", "load_balancers"];
    case "storage":
      return [];
  }
}

export function formatDimension(dim: Dimension, value: number): string {
  switch (dim) {
    case "memory_mib":
      return formatMib(value);
    case "disk_bytes":
      return formatBytes(value);
    default:
      return String(value);
  }
}

export interface Bar {
  dim: Dimension;
  used: number;
  /** 묶는 상한(없으면 상한 없음) */
  limit?: number;
  /** 묶는 폴더가 이 폴더가 아니면 그 경로(부모가 묶음) */
  boundBy?: string;
}

/**
 * 차원마다 막대 하나: 묶는 폴더(`binding`)의 사용량/상한. 이 폴더의 상한이 부모보다 커도 부모가 묶으면 부모의 값과
 * 경로를 보인다(설계 §9 "부모가 묶음"). 상한이 없는 차원은 이 폴더의 사용량만이다.
 */
export function usageBars(view: FolderUsageView, kind: FolderKind = "vm"): Bar[] {
  // 전용 vCPU(계획 64)는 쓰거나 상한이 있을 때만 막대가 있다(지금 화면 그대로).
  const dims = dimensionsOf(kind).filter(
    (dim) =>
      dim !== "dedicated_vcpus" ||
      (view.usage.dedicated_vcpus ?? 0) > 0 ||
      view.limits.some((l) => l.quota.dedicated_vcpus != null),
  );
  return dims.map((dim) => {
    const binder = view.binding[dim];
    const limit = binder ? view.limits.find((l) => l.folder === binder) : undefined;
    const max = limit?.quota[dim];
    if (!limit || max === undefined || max === null) {
      return { dim, used: view.usage[dim] ?? 0 };
    }
    return {
      dim,
      used: limit.usage[dim] ?? 0,
      limit: max,
      boundBy: limit.folder === view.folder ? undefined : limit.path,
    };
  });
}

export function useFolderUsage(id: string, enabled = true) {
  return useQuery({
    queryKey: ["folder-usage", id],
    queryFn: () => call(api.GET("/folders/{id}/usage", { params: { path: { id } } })),
    enabled,
    // 서버가 10초 캐시한다.
    refetchInterval: 10_000,
  });
}

/** 사용량/상한 막대(VM 폴더 넷, 네트워크 폴더 둘 — 요약 카드·할당량 탭) */
export function UsageBars({ view, kind = "vm" }: { view: FolderUsageView; kind?: FolderKind }) {
  const { t } = useTranslation();
  return (
    <ul className="grid gap-(--gap) sm:grid-cols-2 lg:grid-cols-4">
      {usageBars(view, kind).map((bar) => {
        const pct = bar.limit ? Math.min(100, (bar.used / bar.limit) * 100) : 0;
        const over = bar.limit !== undefined && bar.used > bar.limit;
        const label = t(`folders.dim.${bar.dim}`);
        return (
          <li
            key={bar.dim}
            className="flex flex-col gap-1 rounded-lg border bg-surface-2 px-4 py-3"
          >
            <span className="text-xs text-muted-foreground">{label}</span>
            <span className="text-base font-medium tabular-nums">
              {bar.limit !== undefined
                ? t("folders.usedOf", {
                    used: formatDimension(bar.dim, bar.used),
                    limit: formatDimension(bar.dim, bar.limit),
                  })
                : formatDimension(bar.dim, bar.used)}
            </span>
            {bar.limit !== undefined ? (
              <div
                role="meter"
                aria-label={label}
                aria-valuemin={0}
                aria-valuemax={bar.limit}
                aria-valuenow={bar.used}
                className="h-2 overflow-hidden rounded-full bg-muted"
              >
                <div
                  className={cn("h-full rounded-full", over ? "bg-danger" : "bg-brand")}
                  style={{ width: `${pct}%` }}
                />
              </div>
            ) : (
              <span className="text-xs text-muted-foreground">{t("folders.noLimit")}</span>
            )}
            {over && <span className="text-xs text-destructive">{t("folders.over")}</span>}
            {bar.boundBy && (
              <span className="text-xs text-muted-foreground">
                {t("folders.boundBy", { path: bar.boundBy })}
              </span>
            )}
          </li>
        );
      })}
    </ul>
  );
}
