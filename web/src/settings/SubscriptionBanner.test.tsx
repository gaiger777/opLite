import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { readPref, writePref } from "@/lib/prefs";
import {
  HIDDEN_UNTIL_PREF,
  SubscriptionBanner,
  subscriptionNotice,
} from "@/settings/SubscriptionBanner";
import { renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const DAY = 24 * 60 * 60;
const now = () => Math.floor(Date.now() / 1000);

const ACTIVE = {
  state: "active" as const,
  id: "sub-1",
  customer: "Example Corp",
  level: "standard" as const,
  nodes: 3,
  used_nodes: 2,
  expires_at: now() + 365 * DAY,
};

beforeEach(() => window.localStorage.clear());

function subscription(info: object) {
  server.use(http.get(apiUrl("/subscription"), () => HttpResponse.json(info)));
}

it("picks the notice from the state and the days left", () => {
  const at = Date.UTC(2026, 0, 1);
  const expires = at / 1000 + 30 * DAY;
  expect(subscriptionNotice({ ...ACTIVE, expires_at: expires + 1 }, at)).toBeNull();
  // 30일 남은 날부터 곧 만료다.
  expect(subscriptionNotice({ ...ACTIVE, expires_at: expires }, at)).toBe("expiring");
  expect(subscriptionNotice({ state: "none", used_nodes: 1 }, at)).toBe("none");
  expect(subscriptionNotice({ state: "invalid", used_nodes: 1 }, at)).toBe("invalid");
  expect(subscriptionNotice({ ...ACTIVE, state: "expired" }, at)).toBe("expired");
  expect(subscriptionNotice({ ...ACTIVE, state: "over_limit" }, at)).toBe("over_limit");
});

it.each([
  [{ state: "none", used_nodes: 2 }, "status", "This cluster has no subscription."],
  [
    { state: "invalid", used_nodes: 2 },
    "status",
    "The subscription key does not match this build.",
  ],
  [
    { ...ACTIVE, state: "expired", expires_at: now() - DAY },
    "alert",
    "The subscription expired on",
  ],
  [
    { ...ACTIVE, state: "over_limit", used_nodes: 5 },
    "alert",
    "The cluster has 5 nodes, but the subscription covers 3.",
  ],
  [{ ...ACTIVE, expires_at: now() + 10 * DAY }, "status", "The subscription expires on"],
])("shows a notice %#", async (info, role, text) => {
  subscription(info);
  renderPage(<SubscriptionBanner />);
  const banner = await screen.findByRole(role);
  expect(banner).toHaveTextContent(text);
  expect(screen.getByRole("link", { name: "Subscription settings" })).toHaveAttribute(
    "href",
    "/settings",
  );
});

it("shows nothing for an active subscription far from expiry", async () => {
  let reads = 0;
  server.use(
    http.get(apiUrl("/subscription"), () => {
      reads += 1;
      return HttpResponse.json(ACTIVE);
    }),
  );
  renderPage(<SubscriptionBanner />);
  await vi.waitFor(() => expect(reads).toBe(1));
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("hides the notice for seven days in this browser", async () => {
  let reads = 0;
  server.use(
    http.get(apiUrl("/subscription"), () => {
      reads += 1;
      return HttpResponse.json({ ...ACTIVE, state: "expired", expires_at: now() - DAY });
    }),
  );
  const first = renderPage(<SubscriptionBanner />);
  await screen.findByRole("alert");
  const before = Date.now();
  await userEvent.click(screen.getByRole("button", { name: "Hide for 7 days" }));
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  // 닫은 안내와 시각을 함께 둔다.
  const [notice, at] = readPref(HIDDEN_UNTIL_PREF)!.split(":");
  expect(notice).toBe("expired");
  const until = Number(at);
  expect(until).toBeGreaterThanOrEqual(before + 7 * DAY * 1000);
  expect(until).toBeLessThanOrEqual(Date.now() + 7 * DAY * 1000);
  first.unmount();

  // 다시 열어도 숨긴다.
  const second = renderPage(<SubscriptionBanner />);
  await vi.waitFor(() => expect(reads).toBe(2));
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  second.unmount();

  // 7일이 지나면 다시 보인다.
  writePref(HIDDEN_UNTIL_PREF, `expired:${Date.now() - 1}`);
  renderPage(<SubscriptionBanner />);
  expect(await screen.findByRole("alert")).toBeInTheDocument();
});

it("hides only the notice that was dismissed", async () => {
  let info: object = { state: "none", used_nodes: 2 };
  server.use(http.get(apiUrl("/subscription"), () => HttpResponse.json(info)));
  const first = renderPage(<SubscriptionBanner />);
  await screen.findByRole("status");
  await userEvent.click(screen.getByRole("button", { name: "Hide for 7 days" }));
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  first.unmount();

  // 구독을 넣었다가 만료됐다: 다른 안내이므로 보인다.
  info = { ...ACTIVE, state: "expired", expires_at: now() - DAY };
  renderPage(<SubscriptionBanner />);
  expect(await screen.findByRole("alert")).toHaveTextContent("The subscription expired on");
});

it("ignores a hidden-until value from an older version", async () => {
  writePref(HIDDEN_UNTIL_PREF, String(Date.now() + DAY * 1000));
  subscription({ state: "none", used_nodes: 2 });
  renderPage(<SubscriptionBanner />);
  expect(await screen.findByRole("status")).toBeInTheDocument();
});

it("shows the notice when the browser storage cannot be used", async () => {
  const getItem = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
    throw new Error("denied");
  });
  const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
    throw new Error("denied");
  });
  try {
    subscription({ state: "none", used_nodes: 2 });
    renderPage(<SubscriptionBanner />);
    expect(await screen.findByRole("status")).toBeInTheDocument();
    // 닫기는 이번 화면에서는 된다.
    await userEvent.click(screen.getByRole("button", { name: "Hide for 7 days" }));
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  } finally {
    getItem.mockRestore();
    setItem.mockRestore();
  }
});
