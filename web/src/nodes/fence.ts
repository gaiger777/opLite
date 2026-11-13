import type { components } from "@/api/schema";

type Schemas = components["schemas"];
export type FenceKind = Schemas["FenceKind"];

/** 서버의 `MAX_FENCE_METHODS`·`DEFAULT_IPMI_PORT` */
export const MAX_FENCE_METHODS = 4;
const DEFAULT_IPMI_PORT = 623;

/** 대화상자의 방법 한 줄. 종류마다 쓰는 칸만 보낸다. */
export interface FenceRow {
  kind: FenceKind;
  /** ipmi `address` 또는 redfish `url` */
  address: string;
  port: string;
  user: string;
  /** 비면 저장된 비밀번호를 둔다(같은 종류·주소·사용자일 때). */
  password: string;
  /** 서버에 비밀번호가 저장되어 있는지(연 때) */
  hasPassword: boolean;
  /** 저장된 비밀번호를 지운다(빈 글을 보낸다). */
  clearPassword: boolean;
  lanplus: boolean;
  systemId: string;
  caPem: string;
  insecure: boolean;
  off: string;
  status: string;
}

export function emptyRow(kind: FenceKind): FenceRow {
  return {
    kind,
    address: "",
    port: String(DEFAULT_IPMI_PORT),
    user: "",
    password: "",
    hasPassword: false,
    clearPassword: false,
    lanplus: true,
    systemId: "",
    caPem: "",
    insecure: false,
    off: "",
    status: "",
  };
}

/** 응답의 방법을 대화상자의 줄로 */
export function fromView(m: Schemas["FenceMethodView"]): FenceRow {
  const row = emptyRow(m.kind);
  switch (m.kind) {
    case "ipmi":
      return {
        ...row,
        address: m.address,
        port: String(m.port),
        user: m.user,
        hasPassword: m.has_password,
        lanplus: m.lanplus,
      };
    case "redfish":
      return {
        ...row,
        address: m.url,
        user: m.user,
        hasPassword: m.has_password,
        systemId: m.system_id ?? "",
        caPem: m.ca_pem ?? "",
        insecure: m.insecure,
      };
    case "command":
      return { ...row, off: m.off, status: m.status };
  }
}

/** 줄을 PUT 본문의 방법으로. 빈 비밀번호는 빼서 저장된 값을 두고, 지우기면 빈 글을 보낸다. */
export function toMethod(r: FenceRow): Schemas["FenceMethod"] {
  const password = r.clearPassword
    ? { password: "" }
    : r.password !== ""
      ? { password: r.password }
      : {};
  switch (r.kind) {
    case "ipmi":
      return {
        kind: "ipmi",
        address: r.address.trim(),
        port: Number(r.port),
        user: r.user.trim(),
        lanplus: r.lanplus,
        ...password,
      };
    case "redfish":
      return {
        kind: "redfish",
        url: r.address.trim(),
        user: r.user.trim(),
        insecure: r.insecure,
        ...(r.systemId.trim() !== "" && { system_id: r.systemId.trim() }),
        ...(r.caPem.trim() !== "" && { ca_pem: r.caPem }),
        ...password,
      };
    case "command":
      return { kind: "command", off: r.off, status: r.status };
  }
}

/** 칸의 입력 오류(`<줄>.<칸>` → 번역 키). 나머지는 서버가 본다. */
export function fenceErrors(rows: FenceRow[]): Record<string, string> {
  const errors: Record<string, string> = {};
  rows.forEach((r, i) => {
    const need = (field: keyof FenceRow, value: string) => {
      if (value.trim() === "") {
        errors[`${i}.${field}`] = "nodes.fence.errors.required";
      }
    };
    if (r.kind === "command") {
      need("off", r.off);
      need("status", r.status);
      return;
    }
    need("address", r.address);
    need("user", r.user);
    if (r.kind === "ipmi") {
      const port = Number(r.port);
      if (!/^\d+$/.test(r.port) || port < 1 || port > 65535) {
        errors[`${i}.port`] = "nodes.fence.errors.port";
      }
    } else if (r.address.trim() !== "" && !/^https?:\/\/\S/.test(r.address.trim())) {
      errors[`${i}.address`] = "nodes.fence.errors.url";
    }
  });
  return errors;
}
