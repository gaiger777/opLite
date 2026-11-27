import {
  boxBounds,
  boxFace,
  boxesPath,
  boxFaces,
  byDepth,
  clearShift,
  COS30,
  depthOf,
  facesPath,
  gaugeFaces,
  iso,
  linkPath,
  pointsAttr,
  prismFaces,
  screenToWorld,
  shade,
  textWidth,
} from "@/components/iso/iso";
import { rng } from "@/test/clusterMap";
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

it("merges polygons into one path and boxes' faces into one path", () => {
  expect(
    facesPath([
      [
        { x: 0, y: 0 },
        { x: 1, y: 0 },
        { x: 1, y: 1 },
      ],
      // 꼭짓점이 셋보다 적은 것은 뺀다
      [{ x: 5, y: 5 }],
      [
        { x: 2, y: 2 },
        { x: 3.333, y: 2 },
        { x: 3, y: 3 },
      ],
    ]),
  ).toBe("M0,0L1,0L1,1Z M2,2L3.33,2L3,3Z");
  expect(facesPath([])).toBe("");
  const a = { x: 0, y: 0, z: 0, w: 10, d: 10, h: 4 };
  const b = { x: 20, y: 0, z: 0, w: 10, d: 10, h: 4 };
  for (const face of ["top", "left", "right"] as const) {
    expect(boxFace(a, face)).toEqual(boxFaces(a)[face]);
    expect(boxesPath([a, b], face)).toBe(facesPath([boxFaces(a)[face], boxFaces(b)[face]]));
  }
});

it("maps screen points back to the world at a known height", () => {
  expect(screenToWorld(iso(10, 4, 0))).toEqual({ x: expect.closeTo(10), y: expect.closeTo(4) });
  // 성질: 무작위 점 천 개가 왕복한다
  const r = rng(7);
  for (let i = 0; i < 1000; i++) {
    const x = (r() - 0.5) * 4000;
    const y = (r() - 0.5) * 4000;
    const z = r() * 200;
    const back = screenToWorld(iso(x, y, z), z);
    expect(back.x).toBeCloseTo(x, 6);
    expect(back.y).toBeCloseTo(y, 6);
  }
});

it("splits a side face into a gauge fill and the rest", () => {
  const box = { x: 0, y: 0, z: 10, w: 120, d: 120, h: 40 };
  const g = gaugeFaces(box, "left", 0.25);
  // 채움은 아래 10, 나머지는 위 30
  expect(g.fill).toEqual(boxFace({ ...box, h: 10 }, "left"));
  expect(g.rest).toEqual(boxFace({ ...box, z: 20, h: 30 }, "left"));
  // 범위 밖·숫자 아님은 0~1로
  expect(gaugeFaces(box, "right", 2).fill).toEqual(boxFace(box, "right"));
  expect(gaugeFaces(box, "right", Number.NaN).rest).toEqual(boxFace(box, "right"));
  expect(gaugeFaces(box, "right", -1).rest).toEqual(boxFace(box, "right"));
});

it("estimates wide (Hangul) glyphs wider than Latin ones", () => {
  expect(textWidth("ab", 10)).toBe(16);
  expect(textWidth("가나", 10)).toBe(24);
});

it("moves a floor label down a line at a time until it clears the node labels", () => {
  const label = { x: 0, y: 0, w: 100, h: 14 };
  expect(clearShift(label, [])).toBe(0);
  // 노드 이름이 같은 줄에 있으면 비켜날 때까지 아래로(한 줄 12px — 14px 높이라 두 줄)
  expect(clearShift(label, [{ x: 50, y: 0, w: 40, h: 14 }])).toBe(24);
  // 꼬리표 줄까지 겹치면 더 아래로
  expect(
    clearShift(label, [
      { x: 50, y: 0, w: 40, h: 14 },
      { x: 40, y: 14, w: 60, h: 15 },
    ]),
  ).toBe(36);
  // 옆으로 비켜 있으면 그대로
  expect(clearShift(label, [{ x: 120, y: 0, w: 40, h: 14 }])).toBe(0);
  // 끝내 자리가 없으면 처음 자리
  expect(clearShift(label, [{ x: 0, y: -100, w: 100, h: 1000 }])).toBe(0);
});
