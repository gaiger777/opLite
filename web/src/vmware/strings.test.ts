import { createInstance } from "i18next";

import { loadExportStrings } from "@/exports/strings";
import { loadVmwareStrings } from "@/vmware/strings";

// 첫 화면 묶음의 i18n에는 VMware·내보내기 문구가 없다 — 그 경로(또는 내보내기 대화상자)를 열 때 두 언어를 받아 더한다.
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

it("adds the VMware and export strings of both languages when their views load", async () => {
  expect(fresh.i18n.exists("vmware.sources.title")).toBe(false);
  expect(fresh.i18n.exists("vmExports.dialog.title")).toBe(false);
  // 보기 이름과 VM 메뉴 항목은 첫 화면 묶음에 있다
  expect(fresh.i18n.t("nav.vmwareMigrations")).toBe("VMware 이전");
  expect(fresh.i18n.t("vms.action.export")).toBe("OVA로 내보내기…");
  await loadVmwareStrings();
  await loadExportStrings();
  expect(fresh.i18n.t("vmware.wizard.title")).toBe("새 VMware 이전");
  expect(fresh.i18n.getFixedT("en")("vmware.wizard.title")).toBe("New VMware migration");
  expect(fresh.i18n.t("vmExports.dialog.submit")).toBe("내보내기");
  // 서버 오류 코드와 다듬은 문구(계획 72 리뷰 2)
  expect(fresh.i18n.t("vmware.errors.password_required")).toBe(
    "호스트·포트·사용자를 바꾸면 비밀번호를 다시 넣으세요.",
  );
  expect(fresh.i18n.getFixedT("en")("vmware.errors.password_required")).toBe(
    "Enter the password again when you change the host, port or user.",
  );
  expect(fresh.i18n.t("vmware.wizard.errors.tooMany")).toBe(
    "계획 하나에 VM은 200대까지입니다. 여러 계획으로 나누세요.",
  );
  // 원래 문구는 그대로
  expect(fresh.i18n.t("nav.imports")).toBe("가져오기");
  await loadVmwareStrings();
});
