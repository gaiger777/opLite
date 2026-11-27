import "@testing-library/jest-dom/vitest";

import { cleanup } from "@testing-library/react";

import mapEn from "@/clusterMap/i18n/en.json";
import mapKo from "@/clusterMap/i18n/ko.json";
import dndEn from "@/dnd/i18n/en.json";
import dndKo from "@/dnd/i18n/ko.json";
import i18n from "@/i18n";
import mainEn from "@/i18n/en.json";
import mainKo from "@/i18n/ko.json";
import { FakeXhr } from "@/test/fakeXhr";
import { server } from "@/test/server";
import builderEn from "@/vmBuilder/i18n/en.json";
import builderKo from "@/vmBuilder/i18n/ko.json";
import exportsEn from "@/exports/i18n/en.json";
import exportsKo from "@/exports/i18n/ko.json";
import vmwareEn from "@/vmware/i18n/en.json";
import vmwareKo from "@/vmware/i18n/ko.json";
import historyEn from "@/history/i18n/en.json";
import historyKo from "@/history/i18n/ko.json";
import declarativeEn from "@/declarative/i18n/en.json";
import declarativeKo from "@/declarative/i18n/ko.json";
import certsEn from "@/certs/i18n/en.json";
import certsKo from "@/certs/i18n/ko.json";
import physicalEn from "@/physical/i18n/en.json";
import physicalKo from "@/physical/i18n/ko.json";
import drEn from "@/dr/i18n/en.json";
import drKo from "@/dr/i18n/ko.json";
import helpEn from "@/help/i18n/en.json";
import helpKo from "@/help/i18n/ko.json";
import failuresEn from "@/tasks/failures/i18n/en.json";
import failuresKo from "@/tasks/failures/i18n/ko.json";

// 늦게 받는 문구(첫 화면 묶음도 언어마다 늦게 받는다 — `i18n/index.ts`, 클러스터 지도 — `clusterMap/strings.ts`, 끌기 틀 — `dnd/strings.ts`, VM 빌더 —
// `vmBuilder/strings.ts`, VMware 이전 — `vmware/strings.ts`, VM 내보내기 — `exports/strings.ts`, 설정 기록·선언 묶음·인증서 —
// `declarative/strings.ts`, 운영자 대시보드 — `physical/strings.ts`)도 시험에서는 처음부터 둔다.
for (const [en, ko] of [
  [mainEn, mainKo],
  [mapEn, mapKo],
  [dndEn, dndKo],
  [builderEn, builderKo],
  [vmwareEn, vmwareKo],
  [exportsEn, exportsKo],
  [historyEn, historyKo],
  [declarativeEn, declarativeKo],
  [certsEn, certsKo],
  [physicalEn, physicalKo],
  [drEn, drKo],
  [helpEn, helpKo],
  [failuresEn, failuresKo],
] as const) {
  i18n.addResourceBundle("en", "translation", en, false, false);
  i18n.addResourceBundle("ko", "translation", ko, false, false);
}

// uPlot은 캔버스를 쓰므로 jsdom에서 그리지 않는다. 차트는 받은 선 이름만 보이는 가짜다(설계 §13).
vi.mock("@/charts/TimeChart", () => ({
  default: ({ lines }: { lines: { label: string }[] }) =>
    lines.map((line) => line.label).join(", "),
}));

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
beforeEach(async () => {
  await i18n.changeLanguage("en");
  // 앞 테스트의 요청이 남아 있으면 "XHR을 보내지 않았다"를 확인할 수 없다.
  FakeXhr.last = undefined;
});
afterEach(() => {
  cleanup();
  server.resetHandlers();
});
afterAll(() => server.close());
