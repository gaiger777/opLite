import { readChartColors, withAlpha } from "@/charts/chartColors";

it("adds transparency to token colors", () => {
  expect(withAlpha("oklch(0.55 0.19 264)", 0.2)).toBe("oklch(0.55 0.19 264 / 0.2)");
  expect(withAlpha("rgb(1 2 3)", 0.5)).toBe("rgb(1 2 3 / 0.5)");
  // 이미 투명도가 있거나 모르는 형식은 그대로
  expect(withAlpha("oklch(1 0 0 / 10%)", 0.2)).toBe("oklch(1 0 0 / 10%)");
  expect(withAlpha("#fff", 0.2)).toBe("#fff");
});

it("reads the chart and status tokens from the page", () => {
  const root = document.createElement("div");
  root.style.setProperty("--chart-1", "oklch(0.5 0.1 264)");
  root.style.setProperty("--chart-5", " oklch(0.5 0.1 27) ");
  root.style.setProperty("--warning", "oklch(0.7 0.16 70)");
  document.body.append(root);
  const colors = readChartColors(root);
  expect(colors.series[0]).toBe("oklch(0.5 0.1 264)");
  expect(colors.series[4]).toBe("oklch(0.5 0.1 27)");
  expect(colors.series).toHaveLength(5);
  expect(colors.warning).toBe("oklch(0.7 0.16 70)");
  root.remove();
});
