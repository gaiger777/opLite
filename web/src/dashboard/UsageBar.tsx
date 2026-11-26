// 할당·사용 막대 하나(계획 75 §4.1 "한 막대 모양"): 바탕 = 총, 진한 칸 = 사용, 테두리 칸 = 할당, 할당이 총을 넘으면 빨간 빗금
// 넘침 꼬리. 표·자세히 판·클러스터 요약이 같은 것을 쓴다. 수치는 글로도 보이고(`text`) 화면 읽기는 `aria-valuetext`로 듣는다.
import { BAR_TOKEN, tokenColor } from "@/physical/colors";
import type { Measure } from "@/physical/capacity";

const HATCH = (token: string) =>
  `repeating-linear-gradient(135deg, ${tokenColor(token)} 0 3px, transparent 3px 6px)`;

export function UsageBar({
  label,
  m,
  text,
  compact = false,
}: {
  label: string;
  m: Measure;
  /** 보이는 글이자 `aria-valuetext`(예: "12 / 16 vCPU · 사용 40 %") */
  text: string;
  /** 표 칸처럼 좁은 곳: 글을 막대 아래 작게 */
  compact?: boolean;
}) {
  const scale = Math.max(m.total, m.allocated ?? 0, m.used ?? 0) || 1;
  const width = (v: number) => `${Math.min(100, (v / scale) * 100)}%`;
  const main = m.allocated ?? m.used ?? 0;
  const percent = m.total > 0 ? Math.round((main / m.total) * 100) : 0;
  return (
    <div className="flex min-w-0 flex-col gap-1">
      {!compact && <span className="text-sm font-medium">{label}</span>}
      <div
        role="meter"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.min(100, percent)}
        aria-valuetext={text}
        className="relative h-3 w-full overflow-hidden rounded-sm"
      >
        {/* 총 */}
        <div
          className="absolute inset-y-0 left-0 rounded-sm bg-muted"
          style={{ width: width(m.total) }}
        />
        {/* 사용 */}
        {m.used !== undefined && (
          <div
            data-part="used"
            className="absolute inset-y-0 left-0 rounded-sm"
            style={{ width: width(m.used), background: tokenColor(BAR_TOKEN.used) }}
          />
        )}
        {/* 할당(총 안쪽) */}
        {m.allocated !== undefined && (
          <div
            data-part="allocated"
            className="absolute inset-y-0 left-0 rounded-sm border-2"
            style={{
              width: width(Math.min(m.allocated, m.total)),
              borderColor: tokenColor(BAR_TOKEN.allocated),
            }}
          />
        )}
        {/* 넘침 꼬리 */}
        {m.allocated !== undefined && m.allocated > m.total && (
          <div
            data-part="overflow"
            className="absolute inset-y-0 rounded-r-sm border-2"
            style={{
              left: width(m.total),
              width: width(m.allocated - m.total),
              borderColor: tokenColor(BAR_TOKEN.overflow),
              background: HATCH(BAR_TOKEN.overflow),
            }}
          />
        )}
      </div>
      <span className="truncate text-xs text-muted-foreground tabular-nums">{text}</span>
    </div>
  );
}
