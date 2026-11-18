import type { NotificationTargetView } from "@/alerts/alerts";
import {
  checkTargetForm,
  destinationText,
  emptyTargetForm,
  targetFormOf,
  targetRequestOf,
} from "@/alerts/targets";

const SMTP: NotificationTargetView = {
  id: "t1",
  name: "ops-mail",
  revision: 4,
  enabled: true,
  target: {
    kind: "smtp",
    host: "mail.example.com",
    port: 587,
    tls: "starttls",
    user: "alerts",
    has_password: true,
    from: "orbivisor@example.com",
    to: ["ops@example.com", "oncall@example.com"],
  },
};

const HOOK: NotificationTargetView = {
  id: "t2",
  name: "phone",
  revision: 2,
  enabled: true,
  target: {
    kind: "webhook",
    url: "https://ntfy.example.com/…",
    format: "ntfy",
    header_names: ["X-Priority"],
    has_token: true,
  },
};

it("keeps saved SMTP secrets by leaving the password out", () => {
  const form = targetFormOf(SMTP);
  expect(form.password).toBe("");
  expect(checkTargetForm(form, SMTP, [])).toEqual({});
  expect(targetRequestOf(form, SMTP)).toEqual({
    enabled: true,
    target: {
      kind: "smtp",
      host: "mail.example.com",
      port: 587,
      tls: "starttls",
      user: "alerts",
      from: "orbivisor@example.com",
      to: ["ops@example.com", "oncall@example.com"],
    },
  });
  // 지우기는 빈 글
  expect(targetRequestOf({ ...form, clearPassword: true }, SMTP).target).toMatchObject({
    password: "",
  });
  // 새 비밀번호
  expect(targetRequestOf({ ...form, password: "s3cret" }, SMTP).target).toMatchObject({
    password: "s3cret",
  });
  // 사용자를 지우면 비밀번호도 지운다(서버는 사용자 없는 비밀번호를 받지 않는다).
  const anonymous = targetRequestOf({ ...form, user: "" }, SMTP).target;
  expect(anonymous).toMatchObject({ password: "" });
  expect(anonymous).not.toHaveProperty("user");
});

it("asks for the SMTP password again when the server or user changes", () => {
  const form = { ...targetFormOf(SMTP), host: "smtp.other.example" };
  expect(checkTargetForm(form, SMTP, [])).toEqual({
    password: "alerts.targets.errors.passwordAgain",
  });
  expect(checkTargetForm({ ...form, password: "x" }, SMTP, [])).toEqual({});
  expect(checkTargetForm({ ...form, clearPassword: true }, SMTP, [])).toEqual({});
  // TLS 방식만 바꿔도(평문으로 낮추기) 다시 묻는다.
  expect(checkTargetForm({ ...targetFormOf(SMTP), tls: "none" }, SMTP, [])).toEqual({
    password: "alerts.targets.errors.passwordAgain",
  });
});

it("keeps the webhook URL, token and header values unless changed", () => {
  const form = targetFormOf(HOOK);
  expect(form.url).toBe("https://ntfy.example.com/…");
  expect(form.headers).toEqual([{ name: "X-Priority", value: "", saved: true }]);
  expect(checkTargetForm(form, HOOK, [])).toEqual({});
  expect(targetRequestOf(form, HOOK)).toEqual({
    enabled: true,
    target: {
      kind: "webhook",
      url: "https://ntfy.example.com/…",
      format: "ntfy",
      headers: { "X-Priority": "" },
    },
  });
  // 새 값과 지우기
  expect(
    targetRequestOf(
      { ...form, token: "tk", headers: [{ name: "X-Priority", value: "5", saved: true }] },
      HOOK,
    ).target,
  ).toMatchObject({ token: "tk", headers: { "X-Priority": "5" } });
  expect(targetRequestOf({ ...form, clearToken: true }, HOOK).target).toMatchObject({ token: "" });
  // 뺀 헤더는 보내지 않아 서버가 지운다.
  expect(targetRequestOf({ ...form, headers: [] }, HOOK).target).not.toHaveProperty("headers");
  // 토큰을 쓰지 않는 형식으로 바꾸면 저장된 토큰을 지운다.
  expect(targetRequestOf({ ...form, format: "json" }, HOOK).target).toMatchObject({ token: "" });
});

it("asks for webhook secrets again when the URL or CA changes", () => {
  const form = { ...targetFormOf(HOOK), url: "https://other.example.com/topic" };
  expect(checkTargetForm(form, HOOK, [])).toEqual({
    token: "alerts.targets.errors.tokenAgain",
    headers: "alerts.targets.errors.headersAgain",
  });
  // 같은 출처의 다른 경로도 비밀을 다시 묻는다(그 경로가 받은 요청을 보여 주는 곳일 수 있다).
  expect(checkTargetForm({ ...form, url: "https://ntfy.example.com/new" }, HOOK, [])).toEqual({
    token: "alerts.targets.errors.tokenAgain",
    headers: "alerts.targets.errors.headersAgain",
  });
  expect(
    checkTargetForm(
      { ...form, token: "t", headers: [{ name: "X-Priority", value: "1", saved: true }] },
      HOOK,
      [],
    ),
  ).toEqual({});
});

it("checks a new target", () => {
  const form = { ...emptyTargetForm(), name: "hook" };
  expect(checkTargetForm(form, null, [])).toEqual({ url: "alerts.targets.errors.url" });
  expect(
    checkTargetForm(
      {
        ...form,
        url: "https://hooks.slack.com/services/x",
        format: "slack",
        token: "t",
        headers: [{ name: "X-A", value: "", saved: false }],
      },
      null,
      ["hook"],
    ),
  ).toEqual({
    name: "alerts.targets.errors.taken",
    token: "alerts.targets.errors.tokenUnused",
    headers: "alerts.targets.errors.header",
  });
  const smtp = { ...form, kind: "smtp" as const, port: "0", password: "p" };
  expect(checkTargetForm(smtp, null, [])).toEqual({
    host: "alerts.targets.errors.host",
    port: "alerts.targets.errors.port",
    from: "alerts.targets.errors.from",
    to: "alerts.targets.errors.to",
    password: "alerts.targets.errors.passwordWithoutUser",
  });
  expect(
    targetRequestOf(
      { ...smtp, host: "h", port: "25", tls: "none", from: "a@b", to: "x@y; z@w", password: "" },
      null,
    ),
  ).toEqual({
    name: "hook",
    enabled: true,
    target: { kind: "smtp", host: "h", port: 25, tls: "none", from: "a@b", to: ["x@y", "z@w"] },
  });
});

it("shows where a target sends", () => {
  expect(destinationText(SMTP)).toBe("mail.example.com:587 → ops@example.com, oncall@example.com");
  expect(destinationText(HOOK)).toBe("https://ntfy.example.com/…");
});
