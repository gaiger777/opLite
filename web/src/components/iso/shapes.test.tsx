import { render } from "@testing-library/react";

import { gaugeFaces, pointsAttr } from "@/components/iso/iso";
import { IsoGauge } from "@/components/iso/shapes";

it("draws a gauge as the rest and the fill of one face", () => {
  const box = { x: 0, y: 0, z: 0, w: 120, d: 120, h: 44 };
  const { container } = render(
    <svg>
      <IsoGauge box={box} face="right" fraction={0.5} color="var(--card)" fill="var(--warning)" />
    </svg>,
  );
  const polys = container.querySelectorAll("polygon");
  expect(polys).toHaveLength(2);
  const g = gaugeFaces(box, "right", 0.5);
  expect(polys[0]!.getAttribute("points")).toBe(pointsAttr(g.rest));
  expect(polys[1]!.getAttribute("points")).toBe(pointsAttr(g.fill));
  expect(polys[1]!.getAttribute("style")).toContain("var(--warning)");
});
