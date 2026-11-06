import type { components } from "@/api/schema";
import type { Role } from "@/api/types";
import { validName } from "@/vms/form";

type Schemas = components["schemas"];

export type RealmView = Schemas["RealmView"];
export type RealmRequest = Schemas["RealmRequest"];
export type RealmTestResult = Schemas["RealmTestResult"];

/** 서버의 기본값과 같다(`opl_types::realm`). */
export const DEFAULT_USER_FILTER = "(&(objectClass=person)(uid={username}))";
export const DEFAULT_GROUP_ATTR = "memberOf";
export const DEFAULT_TIMEOUT_SECS = 10;

export interface GroupRow {
  group: string;
  role: Role;
}

/** LDAP 영역 만들기·고치기 폼. 입력 그대로의 글이다. */
export interface RealmForm {
  name: string;
  display: string;
  url: string;
  baseDn: string;
  bindDn: string;
  /** 새 비밀번호. 고칠 때 비우면 저장된 값을 그대로 둔다. */
  bindPassword: string;
  /** 고칠 때 저장된 비밀번호를 지운다. */
  clearBindPassword: boolean;
  userFilter: string;
  groupAttr: string;
  caPem: string;
  timeout: string;
  groups: GroupRow[];
  /** 빈 글이면 기본 역할이 없다(맞는 그룹이 없으면 로그인을 거절한다). */
  defaultRole: Role | "";
}

export function emptyForm(): RealmForm {
  return {
    name: "",
    display: "",
    url: "",
    baseDn: "",
    bindDn: "",
    bindPassword: "",
    clearBindPassword: false,
    userFilter: DEFAULT_USER_FILTER,
    groupAttr: DEFAULT_GROUP_ATTR,
    caPem: "",
    timeout: String(DEFAULT_TIMEOUT_SECS),
    groups: [],
    defaultRole: "",
  };
}

export function formOf(realm: RealmView): RealmForm {
  // OIDC 영역의 폼은 아직 없다. LDAP 칸은 기본값으로 둔다.
  const ldap = realm.backend.kind === "ldap" ? realm.backend : null;
  return {
    name: realm.name,
    display: realm.display ?? "",
    url: ldap?.url ?? "",
    baseDn: ldap?.base_dn ?? "",
    bindDn: ldap?.bind_dn ?? "",
    bindPassword: "",
    clearBindPassword: false,
    userFilter: ldap?.user_filter ?? DEFAULT_USER_FILTER,
    groupAttr: ldap?.group_attr ?? DEFAULT_GROUP_ATTR,
    caPem: ldap?.ca_pem ?? "",
    timeout: String(ldap?.timeout_secs ?? DEFAULT_TIMEOUT_SECS),
    groups: (realm.roles.groups ?? []).map((g) => ({ ...g })),
    defaultRole: realm.roles.default ?? "",
  };
}

/**
 * 서버의 `RealmSpec::validate`와 PUT의 비밀번호 규칙과 같은 검사. 값은 번역 키다. 그룹 행의 오류는 `group-<번호>`.
 * `realm`은 고칠 때의 지금 영역이다.
 */
export function checkForm(
  form: RealmForm,
  taken: string[] | null,
  realm: RealmView | null = null,
): Record<string, string> {
  const errors: Record<string, string> = {};
  // 만들 때만 이름을 본다(`taken`이 있으면 만들기).
  if (taken) {
    if (!validName(form.name)) {
      errors.name = "forms.name";
    } else if (form.name === "local") {
      errors.name = "realms.errors.local";
    } else if (taken.includes(form.name)) {
      errors.name = "realms.errors.taken";
    }
  }
  if (!/^ldaps?:\/\/\[?[^/?:\]]+/.test(form.url.trim())) {
    errors.url = "realms.errors.url";
  }
  if (!form.baseDn.trim()) {
    errors.baseDn = "realms.errors.required";
  }
  if (form.userFilter.split("{username}").length !== 2) {
    errors.userFilter = "realms.errors.userFilter";
  }
  if (!form.groupAttr.trim()) {
    errors.groupAttr = "realms.errors.required";
  }
  const bindDn = form.bindDn.trim();
  const saved = (realm?.backend.kind === "ldap" && realm.backend.has_bind_password) || false;
  if (form.bindPassword && !form.clearBindPassword && !bindDn) {
    errors.bindPassword = "realms.errors.bindDn";
  }
  if (form.clearBindPassword && bindDn) {
    // 바인드 DN에는 비밀번호가 있어야 한다.
    errors.bindDn = "realms.errors.clearNeedsNoBindDn";
  } else if (bindDn && !form.bindPassword && !saved) {
    errors.bindPassword = "realms.errors.bindPasswordNeeded";
  } else if (!bindDn && saved && !form.clearBindPassword && !form.bindPassword) {
    // 바인드 DN을 비우면 익명 검색이므로 저장된 비밀번호를 지워야 한다(다시 넣을 것이 아니다).
    errors.bindPassword = "realms.errors.clearSaved";
  } else if (
    realm?.backend.kind === "ldap" &&
    saved &&
    !form.clearBindPassword &&
    !form.bindPassword
  ) {
    // 저장된 비밀번호를 다른 서버·계정으로 보내지 않는다(서버도 400).
    const ldap = realm.backend;
    if (
      form.url.trim() !== ldap.url ||
      bindDn !== (ldap.bind_dn ?? "") ||
      form.caPem.trim() !== (ldap.ca_pem ?? "")
    ) {
      errors.bindPassword = "realms.errors.bindPasswordAgain";
    }
  }
  const timeout = Number(form.timeout);
  if (!/^\d+$/.test(form.timeout.trim()) || timeout < 1 || timeout > 30) {
    errors.timeout = "realms.errors.timeout";
  }
  form.groups.forEach((g, i) => {
    if (!g.group.trim()) {
      errors[`group-${i}`] = "realms.errors.group";
    }
  });
  return errors;
}

/**
 * 폼을 요청 본문으로. 고칠 때(`editing`) 비밀번호를 비워 두면 `bind_password`를 빼서 서버가 저장된 값을 두고,
 * 지우기를 골랐으면 빈 글을 보낸다.
 */
export function requestOf(form: RealmForm, editing: boolean): RealmRequest {
  const bindDn = form.bindDn.trim();
  const display = form.display.trim();
  const password = form.clearBindPassword && editing ? "" : form.bindPassword || undefined;
  return {
    ...(!editing && { name: form.name }),
    ...(display && { display }),
    backend: {
      kind: "ldap",
      url: form.url.trim(),
      base_dn: form.baseDn.trim(),
      ...(bindDn && { bind_dn: bindDn }),
      ...(password !== undefined && { bind_password: password }),
      user_filter: form.userFilter.trim(),
      group_attr: form.groupAttr.trim(),
      ...(form.caPem.trim() && { ca_pem: form.caPem.trim() }),
      timeout_secs: Number(form.timeout),
    },
    roles: {
      groups: form.groups.map((g) => ({ group: g.group.trim(), role: g.role })),
      ...(form.defaultRole && { default: form.defaultRole }),
    },
  };
}
