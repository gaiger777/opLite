import {
  boxBounds,
  boxFaces,
  byDepth,
  COS30,
  depthOf,
  iso,
  linkPath,
  pointsAttr,
  prismFaces,
  shade,
} from "@/components/iso/iso";
import { fitBox, zoomAt } from "@/components/iso/viewport";

it("projects world points isometrically", () => {
  expect(iso(0, 0, 0)).toEqual({ x: 0, y: 0 });
  // x는 오른쪽 아래, y는 왼쪽 아래, z는 위
  expect(iso(10, 0, 0)).toEqual({ x: 10 * COS30, y: 5 });
  expect(iso(0, 10, 0)).toEqual({ x: -10 * COS30, y: 5 });
  expect(iso(0, 0, 7)).toEqual({ x: 0, y: -7 });
});

it("gives the three visible faces of a box", () => {
  const f = boxFaces({ x: 0, y: 0, z: 0, w: 10, d: 10, h: 4 });
  expect(f.top).toHaveLength(4);
  // 윗면은 높이 h에 있다
  expect(f.top[0]).toEqual({ x: 0, y: -4 });
  // 왼쪽 면은 +y 쪽, 오른쪽 면은 +x 쪽 — 둘 다 아래 끝이 바닥에 닿는다
  expect(f.left[3]).toEqual(iso(0, 10, 0));
  expect(f.right[3]).toEqual(iso(10, 0, 0));
  expect(pointsAttr(f.top)).toBe("0,-4 8.66,1 0,6 -8.66,1");
  expect(boxBounds({ x: 0, y: 0, z: 0, w: 10, d: 10, h: 4 })).toEqual({
    minX: -10 * COS30,
    minY: -4,
    maxX: 10 * COS30,
    maxY: 10,
  });
});

it("sorts far things first, lower layers first", () => {
  const near = { box: { x: 50, y: 50, z: 0, w: 1, d: 1, h: 1 }, id: "near" };
  const far = { box: { x: 0, y: 0, z: 0, w: 1, d: 1, h: 1 }, id: "far" };
  const floor = { box: { x: 90, y: 90, z: 0, w: 1, d: 1, h: 1 }, id: "floor", layer: -1 };
  expect(byDepth([near, far, floor]).map((i) => i.id)).toEqual(["floor", "far", "near"]);
  // 같은 바닥이면 높은 것이 뒤에 그려진다(위에 보임)
  expect(depthOf({ x: 0, y: 0, z: 5, w: 1, d: 1, h: 1 })).toBeGreaterThan(
    depthOf({ x: 0, y: 0, z: 0, w: 1, d: 1, h: 1 }),
  );
});

it("shades faces from one token", () => {
  expect(shade("var(--brand)", "left")).toBe("var(--brand)");
  expect(shade("var(--brand)", "top")).toContain("white");
  expect(shade("var(--brand)", "right")).toContain("black");
});

it("draws only the sides of a prism that face the viewer", () => {
  const hex = prismFaces(0, 0, 0, 10, 5);
  expect(hex.top).toHaveLength(6);
  expect(hex.sides).toHaveLength(3);
});

it("draws straight and curved links", () => {
  expect(linkPath({ x: 0, y: 0 }, { x: 10, y: 0 })).toBe("M0,0 L10,0");
  expect(linkPath({ x: 0, y: 0 }, { x: 10, y: 0 }, 6)).toBe("M0,0 Q5,-6 10,0");
});

it("fits and zooms the view box around a point within limits", () => {
  const fit = fitBox({ minX: 0, minY: 0, maxX: 100, maxY: 50 }, 0);
  expect(fit).toEqual({ x: 0, y: 0, w: 100, h: 50 });
  const zoomed = zoomAt(fit, 2, 50, 25, fit);
  expect(zoomed).toEqual({ x: 25, y: 12.5, w: 50, h: 25 });
  // 한도(6배)를 넘지 않는다
  let v = fit;
  for (let i = 0; i < 20; i++) {
    v = zoomAt(v, 2, 50, 25, fit);
  }
  expect(fit.w / v.w).toBeCloseTo(6);
});
