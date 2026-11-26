// 홈 자세히 판·표의 그림 색(계획 75): 모두 디자인 토큰 이름이라 두 테마가 같은 코드다. 색만으로 뜻을 나르지 않는다 — 끊김은 ×,
// 저하는 빗금, 스레드는 채움 모양(예약 빗금·전용 채움·공유 테두리)이 함께 간다. 대비 시험(`colors.test.ts`)이 두 테마의
// `--card` 위에서 3:1 이상인지 잰다.
import type { ChainHealth } from "@/physical/chain";
import type { ThreadKind } from "@/physical/threads";

export const HEALTH_TOKEN: Record<ChainHealth, string> = {
  up: "--success",
  degraded: "--warning",
  down: "--danger",
  absent: "--muted-foreground",
  unknown: "--muted-foreground",
};

export const THREAD_TOKEN: Record<ThreadKind, string> = {
  reserved: "--muted-foreground",
  dedicated: "--brand",
  shared: "--chart-1",
};

/** 막대: 사용(진한 칸), 할당(테두리), 넘침 꼬리 */
export const BAR_TOKEN = {
  used: "--brand",
  allocated: "--foreground",
  overflow: "--danger",
} as const;

export const tokenColor = (token: string) => `var(${token})`;
