import { useSearchParams } from "react-router";

import { RANGES, type Range } from "@/charts/metrics";

/** 화면의 범위(`?range=`, 기본 1시간). 다른 쿼리 값(`?tab=`)은 그대로 둔다. */
export function useTimeRange(): [Range, (range: Range) => void] {
  const [params, setParams] = useSearchParams();
  const raw = params.get("range");
  const range = RANGES.find((r) => r === raw) ?? "hour";
  const setRange = (next: Range) =>
    setParams(
      (p) => {
        if (next === "hour") {
          p.delete("range");
        } else {
          p.set("range", next);
        }
        return p;
      },
      { replace: true },
    );
  return [range, setRange];
}
