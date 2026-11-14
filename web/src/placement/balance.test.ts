import i18n from "@/i18n";
import {
  barWidth,
  formFromSettings,
  moveReasonText,
  pausedText,
  pct,
  placementPatch,
  thresholdError,
} from "@/placement/balance";
import { PLACEMENT } from "@/test/fixtures";

const t = i18n.t.bind(i18n);

it("fills settings the server left out with the defaults", () => {
  expect(formFromSettings(undefined)).toEqual({
    mode: "recommend",
    policy: "balance",
    threshold: "15",
  });
  expect(formFromSettings({ mode: "auto", threshold: 20 })).toEqual({
    mode: "auto",
    policy: "balance",
    threshold: "20",
  });
});

it("accepts whole thresholds from 1 to 50", () => {
  for (const ok of ["1", "15", "50"]) {
    expect(thresholdError(ok)).toBeNull();
  }
  for (const bad of ["0", "51", "", "1.5", "-3", "abc"]) {
    expect(thresholdError(bad)).toBe("placement.errors.threshold");
  }
});

it("sends only the settings that changed", () => {
  const current = formFromSettings(undefined);
  expect(placementPatch(current, current)).toEqual({});
  // "015"는 15와 같다.
  expect(placementPatch({ ...current, threshold: "015" }, current)).toEqual({});
  expect(placementPatch({ ...current, mode: "auto", threshold: "20" }, current)).toEqual({
    mode: "auto",
    threshold: 20,
  });
  expect(placementPatch({ ...current, policy: "pack" }, current)).toEqual({ policy: "pack" });
});

it("shows tenths of a percent and clamps the bars", () => {
  expect(pct(210)).toBe("21.0%");
  expect(pct(5)).toBe("0.5%");
  expect(pct(undefined)).toBe("—");
  expect(barWidth(455)).toBe(45.5);
  // 사용률은 100%를 넘을 수 있다(CPU 몫의 합).
  expect(barWidth(1180)).toBe(100);
  expect(barWidth(-1)).toBe(0);
});

it("translates fixed pause reasons and keeps the others as the server wrote them", () => {
  expect(pausedText(t, "policy is pack")).toBe(
    "The policy is pack, so the balancer does not move VMs for balance.",
  );
  expect(pausedText(t, "mode is off")).toBe("The mode is off.");
  expect(pausedText(t, 'node "node-b" is in maintenance')).toBe('node "node-b" is in maintenance');
});

it("explains each recommendation", () => {
  const [balance, rule] = PLACEMENT.recommendations;
  expect(moveReasonText(t, balance!)).toBe("Imbalance −8.5%");
  expect(moveReasonText(t, rule!)).toBe("Breaks only-b");
});
