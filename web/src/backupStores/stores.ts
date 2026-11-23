// 백업 저장소(계획 68)의 질의와 순수 도움 함수. 목록은 watch 사건이 오면 다시 읽는다(`LiveProvider`의 `VIEW_KINDS` — 비밀을
// 뺀 보기라 사건으로 고치지 않는다). 백업 표·작업 폼·VM 백업 탭도 이 질의로 저장소를 고른다.
import { type QueryClient, useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { Backup, Weekday } from "@/api/types";
import { validName } from "@/vms/form";

type Schemas = components["schemas"];
export type BackupStore = Schemas["Resource_BackupStoreSpecView_BackupStoreStatus"];
export type StoreUsage = Schemas["StoreUsage"];
export type StoreCheck = Schemas["StoreCheck"];
export type VerifyPolicy = Schemas["VerifyPolicy"];
export type VerifyMode = Schemas["VerifyMode"];
export type CreateStoreRequest = Schemas["CreateStoreRequest"];
export type StorePatch = Schemas["StorePatch"];

export const STORES_KEY = ["backup-stores"] as const;

/** 저장소 목록(`GET /backup-stores`, 루트 `system.read`). 읽을 수 없으면 묻지 않는다. */
export function useBackupStores(enabled = true) {
  return useQuery({
    queryKey: STORES_KEY,
    queryFn: () => call(api.GET("/backup-stores")),
    select: (list) => list.items,
    enabled,
  });
}

/** 저장소를 바꾼 뒤 목록을 다시 읽는다(사건이 오기 전에도 화면이 바뀌게). */
export async function refreshStores(queryClient: QueryClient) {
  await queryClient.invalidateQueries({ queryKey: STORES_KEY });
}

/** 백엔드를 한 줄로: 풀 이름 또는 `https://s3.example.com/bk/orbi/` */
export function backendText(store: BackupStore): string {
  const backend = store.spec.backend;
  if ("pool" in backend) {
    return backend.pool;
  }
  const s3 = backend.s3;
  const prefix = s3.prefix ? `/${s3.prefix.replace(/^\/+|\/+$/g, "")}` : "";
  return `${s3.endpoint.replace(/\/+$/, "")}/${s3.bucket}${prefix}`;
}

export function isS3(store: BackupStore): boolean {
  return "s3" in store.spec.backend;
}

/** dedup 배율(논리 / 저장). 센 적이 없거나 저장한 것이 없으면 undefined */
export function dedupRatio(usage: StoreUsage | undefined): number | undefined {
  if (!usage || usage.stored_bytes <= 0 || usage.logical_bytes <= 0) {
    return undefined;
  }
  return usage.logical_bytes / usage.stored_bytes;
}

/** 배율을 `3.2×`처럼(10 이상은 정수) */
export function formatRatio(ratio: number): string {
  return `${ratio >= 10 ? Math.round(ratio) : +ratio.toFixed(1)}×`;
}

/** 지문 앞 몇 묶음(표에 들어갈 만큼): `ab:cd:ef:01…` */
export function shortFingerprint(fingerprint: string): string {
  const parts = fingerprint.split(":");
  return parts.length > 4 ? `${parts.slice(0, 4).join(":")}…` : fingerprint;
}

/** 백업이 가는 곳: 백업 풀 또는 저장소 */
export type BackupTarget = { kind: "pool"; name: string } | { kind: "store"; name: string };

export function backupTarget(backup: Backup): BackupTarget {
  return backup.spec.store
    ? { kind: "store", name: backup.spec.store }
    : { kind: "pool", name: backup.spec.pool ?? "" };
}

/** 검증 배지: 확인됨·실패·안 함. 풀 백업은 검증하지 않으므로(스테이지 6 — 저장소 백업만) 없음이다. */
export type VerifyBadge = "ok" | "failed" | "never" | "none";

export function verifyBadge(backup: Backup): VerifyBadge {
  const verify = backup.status.verify;
  if (verify) {
    return verify.state;
  }
  return backup.spec.store ? "never" : "none";
}

/** 만들기 마법사의 단계 */
export const WIZARD_STEPS = ["backend", "encryption", "schedule", "key"] as const;
export type WizardStep = (typeof WIZARD_STEPS)[number];

/** 암호화 고르기: 끔, 새 키, 복구 키 가져오기, 저장소 안 escrow 풀기 */
export type EncryptionChoice = "none" | "generate" | "recovery" | "escrow";

/** 마법사 폼. 숫자도 입력 그대로 글이다. */
export interface StoreForm {
  name: string;
  backend: "pool" | "s3";
  pool: string;
  endpoint: string;
  region: string;
  bucket: string;
  prefix: string;
  pathStyle: boolean;
  caPem: string;
  accessKeyId: string;
  secretKey: string;
  readOnly: boolean;
  /** 읽기 전용으로 붙이는 풀 안 저장소의 id(남이 만든 `{풀}/store/<id>`) */
  storeId: string;
  encryption: EncryptionChoice;
  recoveryKey: string;
  /** 가져오기: 저장소 안 escrow를 풀 암호 문구 */
  unlockPassphrase: string;
  /** 새 키·가져온 키를 저장소 안에도 감싸 둘 암호 문구(비면 escrow 없음) */
  escrowPassphrase: string;
  gcAt: string;
  gcDays: Weekday[];
  verify: boolean;
  verifyAt: string;
  verifyDays: Weekday[];
  reverifyDays: string;
  verifyMode: VerifyMode;
  verifyNew: boolean;
}

/** 새 폼. GC는 매일 05:00, 검증은 매주 일요일 06:00(설계 §6 — CLI·웹 만들기 기본) */
export function emptyStoreForm(pool = ""): StoreForm {
  return {
    name: "",
    backend: "pool",
    pool,
    endpoint: "",
    region: "us-east-1",
    bucket: "",
    prefix: "",
    pathStyle: false,
    caPem: "",
    accessKeyId: "",
    secretKey: "",
    readOnly: false,
    storeId: "",
    encryption: "none",
    recoveryKey: "",
    unlockPassphrase: "",
    escrowPassphrase: "",
    gcAt: "05:00",
    gcDays: [],
    verify: true,
    verifyAt: "06:00",
    verifyDays: ["sun"],
    reverifyDays: "30",
    verifyMode: "full",
    verifyNew: false,
  };
}

const HHMM = /^([01]\d|2[0-3]):[0-5]\d$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
/** 서버의 escrow 암호 문구 하한(스테이지 6) */
export const MIN_PASSPHRASE = 8;

/**
 * 복구 키 꼴(`OPK1-` + base32 55자). 서버처럼 대소문자·공백·`-`는 가리지 않는다(인쇄본에서 묶음 사이를 띄어 붙여도).
 * 체크섬은 서버가 본다.
 */
export function looksLikeRecoveryKey(text: string): boolean {
  const key = text.trim().toUpperCase();
  return key.startsWith("OPK1-") && /^[A-Z2-7]{55}$/.test(key.slice(5).replace(/[\s-]+/g, ""));
}

/** S3 끝점: 호스트와 포트만 있는 https URL(경로·질의·사용자 정보 없음 — 서버 `endpoint_authority`와 같다) */
function validEndpoint(text: string): boolean {
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return false;
  }
  return (
    url.protocol === "https:" &&
    url.hostname !== "" &&
    url.username === "" &&
    url.password === "" &&
    url.pathname === "/" &&
    url.search === "" &&
    url.hash === "" &&
    !/[?#]/.test(text)
  );
}

/** 버킷: 3-63자 소문자·숫자·`.`·`-`, 처음과 끝은 글자나 숫자, `..` 없음(서버 `valid_bucket`) */
function validBucket(bucket: string): boolean {
  return /^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$/.test(bucket) && !bucket.includes("..");
}

/** 접두사: 512자 이하, `/`로 시작하지 않고, 빈·`.`·`..` 조각이 없다(끝 `/` 하나는 된다 — 서버 `valid_prefix`). */
function validPrefix(prefix: string): boolean {
  if (prefix.length > 512 || prefix.startsWith("/") || /\p{Cc}/u.test(prefix)) {
    return false;
  }
  return (prefix.endsWith("/") ? prefix.slice(0, -1) : prefix)
    .split("/")
    .every((part) => part !== "" && part !== "." && part !== "..");
}

/** CA: PEM 인증서가 든 64 KiB 이하 글 */
function validCaPem(pem: string): boolean {
  return (
    pem.includes("-----BEGIN CERTIFICATE-----") && new TextEncoder().encode(pem).length <= 64 << 10
  );
}

/** 한 단계의 오류(칸 → 번역 키). `taken`은 있는 저장소 이름이다. */
export function checkStoreStep(
  form: StoreForm,
  step: WizardStep,
  taken: readonly string[],
): Record<string, string> {
  const errors: Record<string, string> = {};
  if (step === "backend") {
    if (!validName(form.name)) {
      errors.name = "forms.name";
    } else if (taken.includes(form.name)) {
      errors.name = "backupStores.errors.exists";
    }
    if (form.backend === "pool") {
      if (form.pool === "") {
        errors.pool = "backups.errors.pool";
      }
      if (form.readOnly && !UUID.test(form.storeId.trim())) {
        errors.storeId = "backupStores.errors.storeId";
      }
    } else {
      // 서버(`S3Target::validate`)와 같은 규칙 — 만들기에서 400이 마지막 단계에 뜨지 않게
      if (!validEndpoint(form.endpoint.trim())) {
        errors.endpoint = "backupStores.errors.endpoint";
      }
      if (!validBucket(form.bucket.trim())) {
        errors.bucket = "backupStores.errors.bucket";
      }
      if (!/^[a-z0-9-]{1,32}$/.test(form.region.trim())) {
        errors.region = "backupStores.errors.region";
      }
      if (form.prefix.trim() !== "" && !validPrefix(form.prefix.trim())) {
        errors.prefix = "backupStores.errors.prefix";
      }
      if (form.caPem.trim() !== "" && !validCaPem(form.caPem.trim())) {
        errors.caPem = "backupStores.errors.caPem";
      }
      if (!validAccessKeyId(form.accessKeyId.trim())) {
        errors.accessKeyId = "backupStores.errors.accessKeyId";
      }
      if (!validSecretKey(form.secretKey)) {
        errors.secretKey = "backupStores.errors.secretKey";
      }
    }
  }
  if (step === "encryption") {
    if (form.encryption === "recovery" && !looksLikeRecoveryKey(form.recoveryKey)) {
      errors.recoveryKey = "backupStores.errors.recoveryKey";
    }
    if (form.encryption === "escrow" && form.unlockPassphrase.length < MIN_PASSPHRASE) {
      errors.unlockPassphrase = "backupStores.errors.passphrase";
    }
    // 읽기 전용 저장소에는 escrow를 쓰지 않는다(서버 409 `store_read_only`) — 칸도 숨으므로 보지 않는다.
    if (
      !form.readOnly &&
      form.escrowPassphrase !== "" &&
      (form.encryption === "generate" || form.encryption === "recovery") &&
      form.escrowPassphrase.length < MIN_PASSPHRASE
    ) {
      errors.escrowPassphrase = "backupStores.errors.passphrase";
    }
  }
  if (step === "schedule") {
    if (!HHMM.test(form.gcAt)) {
      errors.gcAt = "backupJobs.errors.at";
    }
    if (form.verify) {
      if (!HHMM.test(form.verifyAt)) {
        errors.verifyAt = "backupJobs.errors.at";
      }
      const days = form.reverifyDays.trim();
      if (!/^\d+$/.test(days) || Number(days) < 1 || Number(days) > 3650) {
        errors.reverifyDays = "backupStores.errors.reverifyDays";
      }
    }
  }
  return errors;
}

/** 요일을 월요일부터 */
function weekdays(days: readonly Weekday[]): Weekday[] {
  const order: Weekday[] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
  return order.filter((d) => days.includes(d));
}

/** 폼의 검증 정책(끄면 없음) */
export function verifyPolicyOf(form: StoreForm): VerifyPolicy | undefined {
  if (!form.verify) {
    return undefined;
  }
  return {
    schedule: { at: form.verifyAt, days: weekdays(form.verifyDays) },
    reverify_days: Number(form.reverifyDays.trim()),
    mode: form.verifyMode,
    ...(form.verifyNew && { verify_new: true }),
  };
}

/** 검사한 폼의 만들기 본문. 쓰지 않는 칸은 보내지 않는다. */
export function createBodyOf(form: StoreForm): CreateStoreRequest {
  const backend =
    form.backend === "pool"
      ? { pool: form.pool }
      : {
          s3: {
            endpoint: form.endpoint.trim(),
            region: form.region.trim(),
            bucket: form.bucket.trim(),
            ...(form.prefix.trim() !== "" && { prefix: form.prefix.trim() }),
            ...(form.pathStyle && { path_style: true }),
            ...(form.caPem.trim() !== "" && { ca_pem: form.caPem.trim() }),
            access_key_id: form.accessKeyId.trim(),
            secret_access_key: form.secretKey,
          },
        };
  const verify = verifyPolicyOf(form);
  const readOnly = form.readOnly;
  const encryption =
    form.encryption === "generate"
      ? { generate: true }
      : form.encryption === "recovery"
        ? { recovery_key: form.recoveryKey.trim().toUpperCase() }
        : form.encryption === "escrow"
          ? { escrow_passphrase: form.unlockPassphrase }
          : undefined;
  const escrow =
    !readOnly &&
    (form.encryption === "generate" || form.encryption === "recovery") &&
    form.escrowPassphrase !== "";
  return {
    name: form.name,
    spec: {
      backend,
      ...(readOnly && { read_only: true }),
      ...(readOnly && form.backend === "pool" && { store_id: form.storeId.trim() }),
      gc: { at: form.gcAt, days: weekdays(form.gcDays) },
      ...(verify && { verify }),
    },
    ...(encryption && { encryption }),
    ...(escrow && { escrow_passphrase: form.escrowPassphrase }),
  };
}

/** 설정 바꾸기 폼(서버가 받는 칸만 — 백엔드·암호화는 바꾸지 못한다) */
export interface StoreSettingsForm {
  gcAt: string;
  gcDays: Weekday[];
  verify: boolean;
  verifyAt: string;
  verifyDays: Weekday[];
  reverifyDays: string;
  verifyMode: VerifyMode;
  verifyNew: boolean;
  maxStreams: string;
  /** S3만: 둘 다 주면 자격 증명을 바꾼다. */
  accessKeyId: string;
  secretKey: string;
}

export function settingsFormOf(store: BackupStore): StoreSettingsForm {
  const verify = store.spec.verify;
  return {
    gcAt: store.spec.gc.at,
    gcDays: store.spec.gc.days ?? [],
    verify: verify !== undefined,
    verifyAt: verify?.schedule.at ?? "06:00",
    verifyDays: verify?.schedule.days ?? ["sun"],
    reverifyDays: String(verify?.reverify_days ?? 30),
    verifyMode: verify?.mode ?? "full",
    verifyNew: verify?.verify_new ?? false,
    maxStreams: String(store.spec.max_streams),
    accessKeyId: "",
    secretKey: "",
  };
}

/** 서버 규칙(`printable`): 액세스 키 ID는 공백 없는 ASCII 1-128자(앞뒤 공백은 보낼 때 뺀다) */
export function validAccessKeyId(id: string): boolean {
  return /^[\x21-\x7e]{1,128}$/.test(id);
}

/** 비밀 키는 공백·줄바꿈 없는 ASCII 1-256자. 일부러 넣었을 수도 있는 끝 글자를 몰래 빼지 않고 오류로 보인다(리뷰 2 N4). */
export function validSecretKey(secret: string): boolean {
  return /^[\x21-\x7e]{1,256}$/.test(secret);
}

export function checkSettings(form: StoreSettingsForm, s3: boolean): Record<string, string> {
  const errors = checkStoreStep({ ...emptyStoreForm(), ...form }, "schedule", []);
  const streams = form.maxStreams.trim();
  if (!/^\d+$/.test(streams) || Number(streams) < 1 || Number(streams) > 64) {
    errors.maxStreams = "backupStores.errors.maxStreams";
  }
  if (s3 && (form.accessKeyId.trim() === "") !== (form.secretKey === "")) {
    errors.secretKey = "backupStores.errors.credentials";
  } else if (s3 && form.secretKey !== "") {
    if (!validAccessKeyId(form.accessKeyId.trim())) {
      errors.accessKeyId = "backupStores.errors.accessKeyId";
    }
    if (!validSecretKey(form.secretKey)) {
      errors.secretKey = "backupStores.errors.secretKey";
    }
  }
  return errors;
}

/** 바뀐 칸만(`PATCH` 본문). 검증을 끄면 `verify: null`이다. */
export function settingsPatchOf(store: BackupStore, form: StoreSettingsForm): StorePatch {
  const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
  const patch: StorePatch = {};
  const gc = { at: form.gcAt, days: weekdays(form.gcDays) };
  if (!same({ at: store.spec.gc.at, days: weekdays(store.spec.gc.days ?? []) }, gc)) {
    patch.gc = gc;
  }
  const verify = verifyPolicyOf({ ...emptyStoreForm(), ...form });
  const old = store.spec.verify;
  const normal = (v: VerifyPolicy | undefined) =>
    v && {
      at: v.schedule.at,
      days: weekdays(v.schedule.days ?? []),
      reverify: v.reverify_days,
      mode: v.mode ?? "full",
      new: v.verify_new ?? false,
    };
  if (!same(normal(old), normal(verify))) {
    patch.verify = verify ?? null;
  }
  const streams = Number(form.maxStreams.trim());
  if (streams !== store.spec.max_streams) {
    patch.max_streams = streams;
  }
  if (form.accessKeyId.trim() !== "" && form.secretKey !== "") {
    patch.s3_credentials = {
      access_key_id: form.accessKeyId.trim(),
      secret_access_key: form.secretKey,
    };
  }
  return patch;
}
