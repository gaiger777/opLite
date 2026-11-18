import type { NotificationTargetRequest, NotificationTargetView } from "@/alerts/alerts";
import type { components } from "@/api/schema";

type Schemas = components["schemas"];
export type SmtpTls = Schemas["SmtpTls"];
export type WebhookFormat = Schemas["WebhookFormat"];

export const SMTP_TLS: readonly SmtpTls[] = ["starttls", "tls", "none"];
export const WEBHOOK_FORMATS: readonly WebhookFormat[] = [
  "json",
  "slack",
  "teams",
  "ntfy",
  "gotify",
];

/** 토큰을 쓰는 형식(ntfy·Gotify). 다른 형식에 토큰을 주면 서버가 거절한다. */
export function usesToken(format: WebhookFormat): boolean {
  return format === "ntfy" || format === "gotify";
}

/** 헤더 한 줄. `saved`는 서버에 저장된 이름이다(값이 비면 저장된 값을 그대로 둔다). */
export interface HeaderRow {
  name: string;
  value: string;
  saved: boolean;
}

/**
 * 알림 대상 폼. 비밀(SMTP 비밀번호, webhook 토큰·헤더 값, URL의 경로)은 서버가 돌려주지 않는다. 고칠 때 비워 두면
 * 저장된 값을 지키고, "지우기"를 고르면 지운다.
 */
export interface TargetForm {
  name: string;
  enabled: boolean;
  kind: "smtp" | "webhook";
  host: string;
  port: string;
  tls: SmtpTls;
  user: string;
  password: string;
  clearPassword: boolean;
  from: string;
  /** 받는 주소(쉼표·공백으로 나눔) */
  to: string;
  /** 고칠 때는 서버가 줄인 URL(`scheme://host/…`)로 시작한다. 그대로 보내면 저장된 URL을 지킨다. */
  url: string;
  format: WebhookFormat;
  headers: HeaderRow[];
  token: string;
  clearToken: boolean;
  caPem: string;
}

export function emptyTargetForm(): TargetForm {
  return {
    name: "",
    enabled: true,
    kind: "webhook",
    host: "",
    port: "587",
    tls: "starttls",
    user: "",
    password: "",
    clearPassword: false,
    from: "",
    to: "",
    url: "",
    format: "json",
    headers: [],
    token: "",
    clearToken: false,
    caPem: "",
  };
}

export function targetFormOf(view: NotificationTargetView): TargetForm {
  const base = { ...emptyTargetForm(), name: view.name, enabled: view.enabled };
  const t = view.target;
  if (t.kind === "smtp") {
    return {
      ...base,
      kind: "smtp",
      host: t.host,
      port: String(t.port),
      tls: t.tls,
      user: t.user ?? "",
      from: t.from,
      to: t.to.join(", "),
    };
  }
  return {
    ...base,
    kind: "webhook",
    url: t.url,
    format: t.format,
    headers: t.header_names.map((name) => ({ name, value: "", saved: true })),
    caPem: t.ca_pem ?? "",
  };
}

function split(text: string): string[] {
  return text
    .split(/[\s,;]+/)
    .map((s) => s.trim())
    .filter((s) => s !== "");
}

/** URL의 출처(scheme·host·port). 읽지 못하면 글 그대로 */
function origin(url: string): string {
  try {
    return new URL(url.trim()).origin;
  } catch {
    return url.trim();
  }
}

/**
 * 서버가 지킬 수 없는 비밀이 있는가. SMTP는 호스트·포트·사용자가, webhook은 URL 출처나 CA가 바뀌면 저장된 비밀을
 * 다시 넣어야 한다(바뀐 곳으로 옛 비밀을 보내지 않게 서버가 400으로 막는다).
 */
function secretsMoved(form: TargetForm, view: NotificationTargetView): boolean {
  const t = view.target;
  if (t.kind === "smtp") {
    return (
      form.host.trim() !== t.host ||
      form.port.trim() !== String(t.port) ||
      form.user.trim() !== (t.user ?? "")
    );
  }
  return origin(form.url) !== origin(t.url) || form.caPem.trim() !== (t.ca_pem ?? "");
}

/** 폼의 오류(칸 → 번역 키). `view`는 고치는 대상(만들 때 null), `taken`은 만들 때 있는 이름 */
export function checkTargetForm(
  form: TargetForm,
  view: NotificationTargetView | null,
  taken: string[],
): Record<string, string> {
  const errors: Record<string, string> = {};
  if (!view) {
    if (!/^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/.test(form.name)) {
      errors.name = "alerts.targets.errors.name";
    } else if (taken.includes(form.name)) {
      errors.name = "alerts.targets.errors.taken";
    }
  }
  const saved = view && view.target.kind === form.kind ? view : null;
  const moved = saved !== null && secretsMoved(form, saved);
  if (form.kind === "smtp") {
    if (form.host.trim() === "") {
      errors.host = "alerts.targets.errors.host";
    }
    const port = Number(form.port);
    if (!/^\d+$/.test(form.port.trim()) || port < 1 || port > 65535) {
      errors.port = "alerts.targets.errors.port";
    }
    if (form.from.trim() === "") {
      errors.from = "alerts.targets.errors.from";
    }
    const to = split(form.to);
    if (to.length === 0 || to.length > 20) {
      errors.to = "alerts.targets.errors.to";
    }
    if (form.password !== "" && form.user.trim() === "") {
      errors.password = "alerts.targets.errors.passwordWithoutUser";
    } else if (
      saved?.target.kind === "smtp" &&
      saved.target.has_password &&
      moved &&
      form.user.trim() !== "" &&
      form.password === "" &&
      !form.clearPassword
    ) {
      errors.password = "alerts.targets.errors.passwordAgain";
    }
  } else {
    const url = form.url.trim();
    const unchanged = saved?.target.kind === "webhook" && url === saved.target.url;
    if (!unchanged && !/^https?:\/\/[^/\s]+/.test(url)) {
      errors.url = "alerts.targets.errors.url";
    }
    if (form.token !== "" && !usesToken(form.format)) {
      errors.token = "alerts.targets.errors.tokenUnused";
    } else if (
      saved?.target.kind === "webhook" &&
      saved.target.has_token &&
      usesToken(form.format) &&
      moved &&
      form.token === "" &&
      !form.clearToken
    ) {
      errors.token = "alerts.targets.errors.tokenAgain";
    }
    if (form.headers.some((h) => h.name.trim() === "" || (!h.saved && h.value === ""))) {
      errors.headers = "alerts.targets.errors.header";
    } else if (moved && form.headers.some((h) => h.saved && h.value === "")) {
      errors.headers = "alerts.targets.errors.headersAgain";
    }
  }
  return errors;
}

/**
 * 보낼 본문. 고칠 때 비밀은 이렇게 보낸다(서버 규칙, 설계 §17 스테이지 5):
 * SMTP 비밀번호는 입력하지 않았으면 빼고(지킨다), "지우기"면 빈 글이다. 사용자를 지우면 비밀번호도 지운다.
 * webhook URL은 줄인 URL을 그대로 두면 지킨다. 토큰은 입력하지 않았으면 빼고, "지우기"거나 토큰을 쓰지 않는
 * 형식으로 바꾸었으면 빈 글이다. 저장된 헤더는 값을 비워 두면 빈 글로 보내 지키고, 뺀 헤더는 서버가 지운다.
 */
export function targetRequestOf(
  form: TargetForm,
  view: NotificationTargetView | null,
): NotificationTargetRequest {
  const saved = view && view.target.kind === form.kind ? view.target : null;
  const head = { enabled: form.enabled, ...(view ? {} : { name: form.name }) };
  if (form.kind === "smtp") {
    const user = form.user.trim();
    const hadPassword = saved?.kind === "smtp" && saved.has_password;
    let password: string | undefined;
    if (form.password !== "") {
      password = form.password;
    } else if (hadPassword && (form.clearPassword || user === "")) {
      password = "";
    }
    return {
      ...head,
      target: {
        kind: "smtp",
        host: form.host.trim(),
        port: Number(form.port),
        tls: form.tls,
        ...(user !== "" && { user }),
        ...(password !== undefined && { password }),
        from: form.from.trim(),
        to: split(form.to),
      },
    };
  }
  const hadToken = saved?.kind === "webhook" && saved.has_token;
  let token: string | undefined;
  if (!usesToken(form.format)) {
    token = hadToken ? "" : undefined;
  } else if (form.token !== "") {
    token = form.token;
  } else if (hadToken && form.clearToken) {
    token = "";
  }
  const headers = Object.fromEntries(form.headers.map((h) => [h.name.trim(), h.value]));
  const ca = form.caPem.trim();
  return {
    ...head,
    target: {
      kind: "webhook",
      url: form.url.trim(),
      format: form.format,
      ...(form.headers.length > 0 && { headers }),
      ...(token !== undefined && { token }),
      ...(ca !== "" && { ca_pem: ca }),
    },
  };
}

/** 표에 보일 곳: SMTP `host:port → 받는 주소`, webhook 줄인 URL */
export function destinationText(view: NotificationTargetView): string {
  const t = view.target;
  return t.kind === "smtp" ? `${t.host}:${t.port} → ${t.to.join(", ")}` : t.url;
}
