import { EDGE, edgeSpeed, MAX_STEP, scrollStep } from "@/dnd/autoscroll";

it("scrolls faster the closer the pointer is to an edge", () => {
  expect(edgeSpeed(500, 0, 1000)).toBe(0);
  expect(edgeSpeed(EDGE, 0, 1000)).toBe(0);
  expect(edgeSpeed(0, 0, 1000)).toBe(-MAX_STEP);
  expect(edgeSpeed(1000, 0, 1000)).toBe(MAX_STEP);
  expect(edgeSpeed(EDGE / 2, 0, 1000)).toBe(-MAX_STEP / 2);
  // 밖의 점은 스크롤하지 않는다.
  expect(edgeSpeed(-5, 0, 1000)).toBe(0);
  expect(edgeSpeed(1005, 0, 1000)).toBe(0);
});

it("never goes over the frame limit and grows towards the edge", () => {
  let previous = 0;
  for (let d = EDGE; d >= 0; d--) {
    const speed = edgeSpeed(1000 - d, 0, 1000);
    expect(speed).toBeGreaterThanOrEqual(previous);
    expect(speed).toBeLessThanOrEqual(MAX_STEP);
    previous = speed;
  }
});

it("gives a step for each axis of a box", () => {
  const box = { left: 100, top: 100, right: 500, bottom: 300 };
  expect(scrollStep(box, { x: 300, y: 200 })).toEqual({ dx: 0, dy: 0 });
  expect(scrollStep(box, { x: 100, y: 300 })).toEqual({ dx: -MAX_STEP, dy: MAX_STEP });
});
