import type uPlot from "uplot";

// 설정(`test/setup.ts`)은 차트를 가짜로 바꾸므로 진짜 모듈에서 풍선 플러그인만 꺼낸다.
const { tooltipPlugin, timeAxisValues } =
  await vi.importActual<typeof import("@/charts/TimeChart")>("@/charts/TimeChart");

/** 플러그인이 쓰는 만큼만 갖춘 uPlot */
function fakePlot(cursor: { idx?: number | null; left?: number }) {
  const over = document.createElement("div");
  Object.defineProperty(over, "clientWidth", { value: 400 });
  document.body.append(over);
  return {
    over,
    cursor,
    // 시각(초), CPU AVG, CPU MAX, CPU MIN, 메모리 AVG
    data: [
      [1_700_000_000, 1_700_000_060],
      [12.5, 40],
      [20, 50],
      [5, 30],
      [null, 7],
    ],
    series: [
      {},
      { label: "CPU", stroke: "rgb(1, 2, 3)" },
      { label: "CPU max" },
      { label: "CPU min" },
      { label: "Memory", stroke: "rgb(4, 5, 6)" },
    ],
  } as unknown as uPlot;
}

function hover(u: uPlot, mains = [1, 4]) {
  const hooks = tooltipPlugin(mains, "percent").hooks as {
    init: (u: uPlot) => void;
    setCursor: (u: uPlot) => void;
  };
  hooks.init(u);
  hooks.setCursor(u);
  const tip = u.over.querySelector<HTMLElement>('[data-slot="chart-tooltip"]')!;
  return { tip, move: () => hooks.setCursor(u) };
}

afterEach(() => document.body.replaceChildren());

it("shows the time and every line's value at the hovered point", () => {
  const { tip } = hover(fakePlot({ idx: 1, left: 100 }));
  expect(tip.style.display).toBe("block");
  expect(tip).toHaveAttribute("aria-hidden", "true");
  expect(tip.className).toContain("pointer-events-none");
  const rows = [...tip.children].map((r) => r.textContent);
  expect(rows).toHaveLength(3);
  expect(rows[1]).toBe("CPU40 %");
  expect(rows[2]).toBe("Memory7 %");
  // MAX·MIN 띠 가장자리는 따로 줄을 만들지 않는다
  expect(tip.textContent).not.toContain("CPU max");
});

it("shows a dash for a missing value", () => {
  const { tip } = hover(fakePlot({ idx: 0, left: 100 }));
  expect(tip.children[2]!.textContent).toBe("Memory—");
});

it("hides when the cursor leaves the plot", () => {
  const u = fakePlot({ idx: 1, left: 100 });
  const { tip, move } = hover(u);
  expect(tip.style.display).toBe("block");
  u.cursor.idx = null;
  u.cursor.left = -10;
  move();
  expect(tip.style.display).toBe("none");
});

describe("time axis labels (plan 77-6)", () => {
  // 2026-11-28 13:40, 13:50 (현지 시각)
  const at = (h: number, m: number, d = 28) => new Date(2026, 10, d, h, m).getTime() / 1000;
  const u = {} as uPlot;

  it("uses the screen language's clock and adds the date on the first tick and day changes", () => {
    const ko = timeAxisValues("ko")(u, [at(13, 40), at(13, 50), at(0, 0, 29)], 0, 50, 600);
    expect(ko[0]).toMatch(/^오후 1:40\n11월 28일$/);
    expect(ko[1]).toBe("오후 1:50");
    expect(ko[2]).toMatch(/\n11월 29일$/);
    const en = timeAxisValues("en")(u, [at(13, 40), at(13, 50)], 0, 50, 600);
    expect(en[0]).toMatch(/^1:40\sPM\nNov 28$/);
    expect(en[1]).toMatch(/^1:50\sPM$/);
  });

  it("shows only dates for day-long steps", () => {
    expect(timeAxisValues("en")(u, [at(0, 0), at(0, 0, 29)], 0, 50, 86_400)).toEqual([
      "Nov 28",
      "Nov 29",
    ]);
  });
});
