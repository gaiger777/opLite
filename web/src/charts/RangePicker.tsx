import { ToggleGroup } from "radix-ui";
import { useTranslation } from "react-i18next";

import { RANGES, type Range } from "@/charts/metrics";

/** 1시간·1일·1주·1개월·1년. 화살표 키로 옮겨 다닌다(radix ToggleGroup). */
export function RangePicker({ value, onChange }: { value: Range; onChange(range: Range): void }) {
  const { t } = useTranslation();
  return (
    <ToggleGroup.Root
      type="single"
      value={value}
      // 누른 것을 다시 누르면 빈 값이 온다. 범위는 늘 하나다.
      onValueChange={(v) => v && onChange(v as Range)}
      aria-label={t("metrics.range")}
      className="inline-flex w-fit rounded-md border p-0.5"
    >
      {RANGES.map((range) => (
        <ToggleGroup.Item
          key={range}
          value={range}
          className="rounded-sm px-2 py-1 text-xs text-muted-foreground outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring data-[state=on]:bg-muted data-[state=on]:font-medium data-[state=on]:text-foreground"
        >
          {t(`metrics.ranges.${range}`)}
        </ToggleGroup.Item>
      ))}
    </ToggleGroup.Root>
  );
}
