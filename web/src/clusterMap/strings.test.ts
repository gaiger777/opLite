import { createInstance } from "i18next";

import { loadMapStrings } from "@/clusterMap/strings";

// 첫 화면 묶음의 i18n에는 지도 문구가 없다 — 지도 경로를 열 때 두 언어를 받아 더한다.
const fresh = vi.hoisted(() => ({ i18n: null as unknown as import("i18next").i18n }));
vi.mock("@/i18n", async () => {
  const en = (await import("@/i18n/en.json")).default;
  const ko = (await import("@/i18n/ko.json")).default;
  fresh.i18n = createInstance();
  await fresh.i18n.init({
    resources: { en: { translation: en }, ko: { translation: ko } },
    lng: "ko",
    fallbackLng: "en",
  });
  return { default: fresh.i18n };
});

it("adds the map strings of both languages when the map route loads", async () => {
  expect(fresh.i18n.exists("clusterMap.pictureLabel")).toBe(false);
  await loadMapStrings();
  expect(fresh.i18n.t("clusterMap.pictureLabel")).toBe("클러스터 지도");
  expect(fresh.i18n.getFixedT("en")("clusterMap.pictureLabel")).toBe("Cluster map");
  // 원래 문구는 그대로
  expect(fresh.i18n.t("nav.clusterMap")).toBe("지도");
  await loadMapStrings();
});
