import "@testing-library/jest-dom/vitest";
import "@/i18n";

import { cleanup } from "@testing-library/react";

import i18n from "@/i18n";
import { server } from "@/test/server";

beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
beforeEach(() => i18n.changeLanguage("en"));
afterEach(() => {
  cleanup();
  server.resetHandlers();
});
afterAll(() => server.close());
