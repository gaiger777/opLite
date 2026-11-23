import {
  backendText,
  backupTarget,
  checkSettings,
  checkStoreStep,
  createBodyOf,
  dedupRatio,
  emptyStoreForm,
  formatRatio,
  looksLikeRecoveryKey,
  settingsFormOf,
  settingsPatchOf,
  shortFingerprint,
  verifyBadge,
} from "@/backupStores/stores";
import { backup, backupStore, storeBackup, vm } from "@/test/fixtures";

const KEY = "OPK1-ABCD-EFGH-IJKL-MNOP-QRST-UVWX-YZ23-4567-ABCD-EFGH-IJKL-MNOP-QRST-UVW";

it("describes the backend in one line", () => {
  expect(backendText(backupStore("local", 1))).toBe("bk");
  const s3 = backupStore("s3", 2, {
    spec: {
      backend: {
        s3: {
          endpoint: "https://s3.example.com/",
          region: "us-east-1",
          bucket: "bk",
          prefix: "/orbi/",
          access_key_id: "AK",
        },
      },
    },
  });
  expect(backendText(s3)).toBe("https://s3.example.com/bk/orbi");
});

it("computes the dedup ratio only when something was stored", () => {
  expect(dedupRatio(undefined)).toBeUndefined();
  const usage = { snapshots: 3, chunks: 10, stored_bytes: 0, logical_bytes: 10, measured_at: 1 };
  expect(dedupRatio(usage)).toBeUndefined();
  expect(dedupRatio({ ...usage, stored_bytes: 4, logical_bytes: 13 })).toBe(3.25);
  expect(formatRatio(3.25)).toBe("3.3×");
  expect(formatRatio(12.6)).toBe("13×");
  expect(shortFingerprint("ab:cd:ef:01:23:45")).toBe("ab:cd:ef:01…");
});

it("tells pool and store backups and their verify badges apart", () => {
  const web = vm("web", "running");
  const inPool = backup("a", web, 1);
  const inStore = storeBackup("b", web, 2, "st");
  expect(backupTarget(inPool)).toEqual({ kind: "pool", name: "bk" });
  expect(backupTarget(inStore)).toEqual({ kind: "store", name: "st" });
  expect(verifyBadge(inPool)).toBe("none");
  expect(verifyBadge(inStore)).toBe("never");
  expect(
    verifyBadge(storeBackup("c", web, 3, "st", { status: { verify: { at: 1, state: "failed" } } })),
  ).toBe("failed");
});

it("checks each wizard step like the server", () => {
  const form = { ...emptyStoreForm("bk"), name: "local" };
  expect(checkStoreStep(form, "backend", [])).toEqual({});
  expect(checkStoreStep(form, "backend", ["local"])).toEqual({
    name: "backupStores.errors.exists",
  });
  expect(checkStoreStep({ ...form, readOnly: true, storeId: "x" }, "backend", [])).toEqual({
    storeId: "backupStores.errors.storeId",
  });
  // S3는 https 끝점·버킷·자격 증명이 든다(시험용 http는 웹에서 받지 않는다).
  expect(
    checkStoreStep({ ...form, backend: "s3", endpoint: "http://minio:9000" }, "backend", []),
  ).toEqual({
    endpoint: "backupStores.errors.endpoint",
    bucket: "backupStores.errors.bucket",
    accessKeyId: "backupStores.errors.accessKeyId",
    secretKey: "backupStores.errors.secretKey",
  });
  expect(checkStoreStep({ ...form, encryption: "recovery" }, "encryption", [])).toEqual({
    recoveryKey: "backupStores.errors.recoveryKey",
  });
  expect(looksLikeRecoveryKey(KEY.toLowerCase())).toBe(true);
  expect(
    checkStoreStep(
      { ...form, encryption: "generate", escrowPassphrase: "short" },
      "encryption",
      [],
    ),
  ).toEqual({ escrowPassphrase: "backupStores.errors.passphrase" });
  expect(checkStoreStep({ ...form, gcAt: "25:00", reverifyDays: "0" }, "schedule", [])).toEqual({
    gcAt: "backupJobs.errors.at",
    reverifyDays: "backupStores.errors.reverifyDays",
  });
  // 검증을 끄면 검증 칸은 보지 않는다.
  expect(checkStoreStep({ ...form, verify: false, verifyAt: "x" }, "schedule", [])).toEqual({});
});

// 리뷰 1(M3·M4·M5): 서버(`S3Target::validate`·`from_recovery_key`)와 같은 규칙, 숨은 escrow 칸은 보지 않는다.
it("checks S3 fields and recovery keys like the server", () => {
  const s3 = {
    ...emptyStoreForm("bk"),
    name: "dr",
    backend: "s3" as const,
    endpoint: "https://s3.example.com:9000/",
    bucket: "my.bk-1",
    accessKeyId: "AK",
    secretKey: "SK",
  };
  expect(checkStoreStep(s3, "backend", [])).toEqual({});
  expect(
    checkStoreStep(
      {
        ...s3,
        endpoint: "https://s3.example.com/x",
        region: "US_EAST",
        bucket: "a..b",
        prefix: "/orbi",
        caPem: "not a cert",
      },
      "backend",
      [],
    ),
  ).toEqual({
    endpoint: "backupStores.errors.endpoint",
    region: "backupStores.errors.region",
    bucket: "backupStores.errors.bucket",
    prefix: "backupStores.errors.prefix",
    caPem: "backupStores.errors.caPem",
  });
  for (const endpoint of ["https://u:p@h", "https://h?x=1", "https://h#", "ftp://h"]) {
    expect(checkStoreStep({ ...s3, endpoint }, "backend", [])).toHaveProperty("endpoint");
  }
  for (const bucket of ["ab", "-ab", "ab-", "Ab1", "a".repeat(64)]) {
    expect(checkStoreStep({ ...s3, bucket }, "backend", [])).toHaveProperty("bucket");
  }
  for (const prefix of ["a//b", "a/./b", "../a", "a/.."]) {
    expect(checkStoreStep({ ...s3, prefix }, "backend", [])).toHaveProperty("prefix");
  }
  expect(checkStoreStep({ ...s3, prefix: "orbi/prod/" }, "backend", [])).toEqual({});
  const pem = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----";
  expect(checkStoreStep({ ...s3, caPem: pem }, "backend", [])).toEqual({});
  expect(
    checkStoreStep({ ...s3, caPem: pem + "A".repeat(64 << 10) }, "backend", []),
  ).toHaveProperty("caPem");
  expect(checkStoreStep({ ...s3, region: "a".repeat(33) }, "backend", [])).toHaveProperty("region");

  // 복구 키: 인쇄본에서 띄어 쓰거나 `-` 없이, 소문자로 붙여도 된다. 길이가 틀리면 안 된다.
  const groups = KEY.slice(5);
  expect(looksLikeRecoveryKey(`OPK1-${groups.replaceAll("-", " ")}`)).toBe(true);
  expect(looksLikeRecoveryKey(`opk1-${groups.replaceAll("-", "")}`)).toBe(true);
  expect(looksLikeRecoveryKey(`OPK1-${groups.replaceAll("-", "\n")}\n`)).toBe(true);
  expect(looksLikeRecoveryKey(KEY.slice(0, -1))).toBe(false);
  expect(looksLikeRecoveryKey(`${KEY}A`)).toBe(false);
  expect(looksLikeRecoveryKey(KEY.replace("OPK1-", "OPK2-"))).toBe(false);
  expect(looksLikeRecoveryKey(KEY.replace("ABCD", "AB1D"))).toBe(false);

  // 읽기 전용이면 escrow 칸이 숨으므로 남은 짧은 암호 문구로 막지 않는다.
  expect(
    checkStoreStep(
      {
        ...s3,
        readOnly: true,
        encryption: "recovery",
        recoveryKey: KEY,
        escrowPassphrase: "short",
      },
      "encryption",
      [],
    ),
  ).toEqual({});
});

it("builds the create body with only the fields in use", () => {
  const form = { ...emptyStoreForm("bk"), name: "local" };
  expect(createBodyOf(form)).toEqual({
    name: "local",
    spec: {
      backend: { pool: "bk" },
      gc: { at: "05:00", days: [] },
      verify: { schedule: { at: "06:00", days: ["sun"] }, reverify_days: 30, mode: "full" },
    },
  });
  expect(
    createBodyOf({
      ...form,
      backend: "s3",
      endpoint: " https://s3.example.com ",
      bucket: "bk",
      prefix: "orbi/",
      pathStyle: true,
      accessKeyId: "AK",
      secretKey: "SK",
      encryption: "generate",
      escrowPassphrase: "correct horse",
      verify: false,
      gcDays: ["sun", "mon"],
    }),
  ).toEqual({
    name: "local",
    spec: {
      backend: {
        s3: {
          endpoint: "https://s3.example.com",
          region: "us-east-1",
          bucket: "bk",
          prefix: "orbi/",
          path_style: true,
          access_key_id: "AK",
          secret_access_key: "SK",
        },
      },
      gc: { at: "05:00", days: ["mon", "sun"] },
    },
    encryption: { generate: true },
    escrow_passphrase: "correct horse",
  });
  // 읽기 전용 붙이기: 남의 저장소 id와 escrow 풀기, escrow 쓰기는 없다.
  expect(
    createBodyOf({
      ...form,
      readOnly: true,
      storeId: "019a0f3c-0000-7000-8068-000000000001",
      encryption: "escrow",
      unlockPassphrase: "correct horse",
      escrowPassphrase: "ignored!",
    }),
  ).toMatchObject({
    spec: { read_only: true, store_id: "019a0f3c-0000-7000-8068-000000000001" },
    encryption: { escrow_passphrase: "correct horse" },
  });
});

it("patches only the changed settings", () => {
  const store = backupStore("st", 1);
  const form = settingsFormOf(store);
  expect(checkSettings(form, false)).toEqual({});
  expect(settingsPatchOf(store, form)).toEqual({});
  expect(settingsPatchOf(store, { ...form, gcAt: "04:00", verify: false })).toEqual({
    gc: { at: "04:00", days: [] },
    verify: null,
  });
  expect(settingsPatchOf(store, { ...form, verifyNew: true, maxStreams: "16" })).toEqual({
    verify: {
      schedule: { at: "06:00", days: ["sun"] },
      reverify_days: 30,
      mode: "full",
      verify_new: true,
    },
    max_streams: 16,
  });
  expect(checkSettings({ ...form, accessKeyId: "AK", maxStreams: "65" }, true)).toEqual({
    maxStreams: "backupStores.errors.maxStreams",
    secretKey: "backupStores.errors.credentials",
  });
  expect(settingsPatchOf(store, { ...form, accessKeyId: "AK", secretKey: "SK" })).toEqual({
    s3_credentials: { access_key_id: "AK", secret_access_key: "SK" },
  });
});
