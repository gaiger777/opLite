import "@testing-library/jest-dom/vitest";
import "@/i18n";

import { cleanup } from "@testing-library/react";

import i18n from "@/i18n";
import { server } from "@/test/server";

// jsdom에는 matchMedia가 없다(테마가 시스템 설정을 읽는다).
window.matchMedia ??= (query: string) =>
  ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  }) as MediaQueryList;

// jsdom에는 ResizeObserver가 없다(Radix 컴포넌트가 크기를 잰다).
globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
beforeEach(() => i18n.changeLanguage("en"));
afterEach(() => {
  cleanup();
  server.resetHandlers();
});
afterAll(() => server.close());
