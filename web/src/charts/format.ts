import { formatBytes } from "@/lib/format";

// 차트(`TimeChart`, 따로 내려받음)도 쓰므로 react-query·API 없이 둔다.

/** 값의 단위: CPU %, 메모리·용량 MiB, 디스크·네트워크 바이트/초, IOPS */
export type Unit = "percent" | "mib" | "bytesRate" | "opsRate";

/** 값을 단위와 함께. 모르는 값(`null`)은 "—" */
export function formatValue(value: number | null | undefined, unit: Unit): string {
  if (value === null || value === undefined || !Number.isFinite(value)) {
    return "—";
  }
  switch (unit) {
    case "percent":
      return `${+value.toFixed(1)} %`;
    case "mib":
      return formatBytes(Math.max(0, value) * 1024 * 1024);
    case "bytesRate":
      return `${formatBytes(Math.max(0, value))}/s`;
    case "opsRate":
      return `${+value.toFixed(1)}/s`;
  }
}
