import { createInstance } from "i18next";

import { loadCertStrings, loadDeclarativeStrings } from "@/declarative/strings";

// 첫 화면 묶음의 i18n에는 설정 기록·선언 묶음·인증서 문구가 없다 — 그 보기(또는 ⋯ 메뉴의 서랍)를 열 때 두 언어를 받아 더한다.
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

it("adds the history, apply set and certificate strings of both languages when their views load", async () => {
  expect(fresh.i18n.exists("history.pageHint")).toBe(false);
  expect(fresh.i18n.exists("declarative.sets.title")).toBe(false);
  expect(fresh.i18n.exists("certs.pageHint")).toBe(false);
  // 보기 이름·팔레트 별칭·⋯ 메뉴 두 줄은 첫 화면 묶음에 있다
  expect(fresh.i18n.t("nav.history")).toBe("변경 기록");
  expect(fresh.i18n.t("nav.certificatesKeywords")).toContain("ACME");
  expect(fresh.i18n.t("configMenu.history")).toBe("설정 기록…");
  expect(fresh.i18n.t("configMenu.yaml")).toBe("YAML로 내보내기");
  await loadDeclarativeStrings();
  expect(fresh.i18n.t("history.point.title")).toBe("한 시점으로 되돌리기");
  expect(fresh.i18n.getFixedT("en")("declarative.state.modified")).toBe("Different");
  expect(fresh.i18n.exists("certs.pageHint")).toBe(false);
  await loadCertStrings();
  expect(fresh.i18n.t("certs.account.title")).toBe("ACME 계정 더하기");
  // 원래 문구는 그대로
  expect(fresh.i18n.t("nav.audit")).toBe("감사 로그");
  await loadDeclarativeStrings();
});
