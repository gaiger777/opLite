import { act, render, screen } from "@testing-library/react";

import { applyDensity, DensityProvider, readDensity, useDensity } from "@/lib/density";

const original = window.matchMedia;

/** 창 폭이 768px 이상인지 정한다. 바꾸면 듣는 쪽에 알린다. */
function screenWidth(wide: boolean) {
  const listeners = new Set<() => void>();
  const media = {
    get matches() {
      return wide;
    },
    addEventListener: (_: string, fn: () => void) => listeners.add(fn),
    removeEventListener: (_: string, fn: () => void) => listeners.delete(fn),
  };
  window.matchMedia = (() => media) as unknown as typeof window.matchMedia;
  return (next: boolean) => {
    wide = next;
    listeners.forEach((fn) => fn());
  };
}

afterEach(() => {
  window.matchMedia = original;
  localStorage.clear();
  delete document.documentElement.dataset.density;
});

function Probe() {
  const { density, setDensity } = useDensity();
  return (
    <button onClick={() => setDensity(density === "compact" ? "comfortable" : "compact")}>
      {density}
    </button>
  );
}

it("reads comfortable by default and compact when saved", () => {
  expect(readDensity()).toBe("comfortable");
  localStorage.setItem("opl.density", "compact");
  expect(readDensity()).toBe("compact");
  localStorage.setItem("opl.density", "junk");
  expect(readDensity()).toBe("comfortable");
});

it("ignores storage that throws", () => {
  const get = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
    throw new Error("denied");
  });
  expect(readDensity()).toBe("comfortable");
  get.mockRestore();
});

it("keeps phones comfortable", () => {
  screenWidth(false);
  applyDensity("compact");
  expect(document.documentElement.dataset.density).toBe("comfortable");
});

it("switches density, saves it and follows the window width", async () => {
  const resize = screenWidth(true);
  render(
    <DensityProvider>
      <Probe />
    </DensityProvider>,
  );
  expect(document.documentElement.dataset.density).toBe("comfortable");
  await act(async () => screen.getByRole("button", { name: "comfortable" }).click());
  expect(document.documentElement.dataset.density).toBe("compact");
  expect(localStorage.getItem("opl.density")).toBe("compact");
  act(() => resize(false));
  expect(document.documentElement.dataset.density).toBe("comfortable");
  act(() => resize(true));
  expect(document.documentElement.dataset.density).toBe("compact");
});
