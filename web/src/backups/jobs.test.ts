import {
  checkForm,
  emptyForm,
  formatUtc,
  formOf,
  keepParts,
  parseLabels,
  patchOf,
  specOf,
} from "@/backups/jobs";
import { backupJob } from "@/test/fixtures";

it("shows times in UTC", () => {
  expect(formatUtc(1_793_761_200)).toBe("2026-11-04 03:00 UTC");
  expect(formatUtc(0)).toBe("1970-01-01 00:00 UTC");
});

it("reads labels as comma-separated KEY=VALUE pairs", () => {
  expect(parseLabels("tier=prod, env = a=b")).toEqual({ tier: "prod", env: "a=b" });
  expect(parseLabels("")).toEqual({});
  expect(parseLabels("tier")).toBeNull();
  expect(parseLabels("=prod")).toBeNull();
});

it("lists only the retention units in use", () => {
  expect(keepParts({ last: 2, daily: 0, weekly: 4, monthly: 0 })).toEqual([
    ["last", 2],
    ["weekly", 4],
  ]);
  expect(keepParts({ last: 0, daily: 0, weekly: 0, monthly: 0 })).toEqual([]);
  expect(keepParts(undefined)).toEqual([]);
});

it("builds the spec from the form", () => {
  const form = {
    ...emptyForm("bk"),
    name: "nightly",
    target: "labels" as const,
    labels: "tier=prod",
    at: "03:30",
    days: ["sun" as const, "mon" as const],
    keep: { last: "2", daily: "", weekly: "4", monthly: "" },
  };
  expect(checkForm(form, [])).toEqual({});
  // 요일은 월요일부터 차례로 보낸다.
  expect(specOf(form)).toEqual({
    vms: { labels: { tier: "prod" } },
    pool: "bk",
    schedule: { at: "03:30", days: ["mon", "sun"] },
    keep: { last: 2, daily: 0, weekly: 4, monthly: 0 },
    enabled: true,
  });
  expect(specOf({ ...form, target: "names", names: "web, db" }).vms).toEqual({
    names: ["web", "db"],
  });
  expect(specOf({ ...form, target: "all" }).vms).toEqual({ all: true });
});

it("checks the form like the server", () => {
  const form = { ...emptyForm("bk"), name: "nightly", at: "03:00" };
  expect(checkForm(form, [])).toEqual({});
  expect(checkForm({ ...form, name: "Bad name" }, []).name).toBe("forms.name");
  expect(checkForm(form, ["nightly"]).name).toBe("backupJobs.errors.exists");
  // 바꾸기에는 이름이 없다.
  expect(checkForm({ ...form, name: "" }, null)).toEqual({});
  expect(checkForm({ ...form, pool: "" }, []).pool).toBe("backups.errors.pool");
  for (const at of ["3:00", "24:00", "03:60", "0300", ""]) {
    expect(checkForm({ ...form, at }, []).at).toBe("backupJobs.errors.at");
  }
  expect(checkForm({ ...form, at: "23:59" }, [])).toEqual({});
  expect(checkForm({ ...form, target: "names", names: " , " }, []).names).toBe(
    "backupJobs.errors.names",
  );
  expect(checkForm({ ...form, target: "labels", labels: "" }, []).labels).toBe(
    "backupJobs.errors.labels",
  );
  expect(checkForm({ ...form, target: "labels", labels: "tier" }, []).labels).toBe(
    "backupJobs.errors.labels",
  );
  for (const n of ["-1", "1.5", "x", "4294967296"]) {
    expect(checkForm({ ...form, keep: { ...form.keep, daily: n } }, []).keep).toBe(
      "backupJobs.errors.keep",
    );
  }
});

it("reads a job back into the form", () => {
  const job = backupJob("nightly", 1, {
    spec: {
      vms: { labels: { tier: "prod", env: "a" } },
      schedule: { at: "04:00", days: ["sat"] },
      keep: { last: 0, daily: 7, weekly: 0, monthly: 0 },
      enabled: false,
    },
  });
  const form = formOf(job);
  expect(form).toMatchObject({
    name: "nightly",
    pool: "bk",
    target: "labels",
    labels: "tier=prod, env=a",
    at: "04:00",
    days: ["sat"],
    keep: { last: "0", daily: "7", weekly: "0", monthly: "0" },
    enabled: false,
  });
  expect(specOf(form)).toEqual(job.spec);
  expect(formOf(backupJob("n", 2, { spec: { vms: { names: ["web", "db"] } } })).names).toBe(
    "web, db",
  );
});

it("sends only what changed", () => {
  const job = backupJob("nightly", 1);
  const form = formOf(job);
  expect(patchOf(job, specOf(form))).toEqual({});
  expect(
    patchOf(job, specOf({ ...form, at: "04:00", keep: { ...form.keep, weekly: "2" } })),
  ).toEqual({
    schedule: { at: "04:00", days: [] },
    keep: { last: 3, daily: 0, weekly: 2, monthly: 0 },
  });
  expect(patchOf(job, specOf({ ...form, enabled: false, target: "names", names: "web" }))).toEqual({
    vms: { names: ["web"] },
    enabled: false,
  });
  // 요일은 집합으로 비교한다(순서만 다르면 바뀐 것이 아니다).
  const days = backupJob("days", 3, { spec: { schedule: { at: "03:00", days: ["wed", "mon"] } } });
  expect(patchOf(days, specOf(formOf(days)))).toEqual({});
  expect(patchOf(days, specOf({ ...formOf(days), days: ["mon"] }))).toEqual({
    schedule: { at: "03:00", days: ["mon"] },
  });
  // 서버가 보존 규칙을 적지 않았어도 모두 0과 같다.
  const bare = backupJob("bare", 2, { spec: { keep: undefined } });
  expect(
    patchOf(
      bare,
      specOf({ ...formOf(bare), keep: { last: "", daily: "", weekly: "", monthly: "" } }),
    ),
  ).toEqual({});
});

it("copies backups to a remote only when one is chosen", () => {
  const form = { ...emptyForm("bk"), name: "nightly", at: "03:00" };
  expect(specOf(form)).not.toHaveProperty("replicate");
  const copying = { ...form, replicateRemote: "dr", replicatePool: "dr-bk" };
  expect(checkForm(copying, [])).toEqual({});
  expect(specOf(copying).replicate).toEqual({ remote: "dr", pool: "dr-bk" });
  // 원격을 골랐으면 그 백업 풀도 고른다.
  expect(checkForm({ ...copying, replicatePool: "" }, []).replicatePool).toBe(
    "backups.errors.pool",
  );

  const job = backupJob("nightly", 1, { spec: { replicate: { remote: "dr", pool: "dr-bk" } } });
  expect(formOf(job)).toMatchObject({ replicateRemote: "dr", replicatePool: "dr-bk" });
  expect(specOf(formOf(job))).toEqual(job.spec);
  expect(patchOf(job, specOf(formOf(job)))).toEqual({});
  expect(patchOf(job, specOf({ ...formOf(job), replicatePool: "other" }))).toEqual({
    replicate: { remote: "dr", pool: "other" },
  });
  // 끄면 null을 보낸다(빼면 그대로 둔다).
  expect(patchOf(job, specOf({ ...formOf(job), replicateRemote: "" }))).toEqual({
    replicate: null,
  });
  expect(patchOf(backupJob("n", 2), specOf(copying))).toMatchObject({
    replicate: { remote: "dr", pool: "dr-bk" },
  });
});

it("asks for incremental backups only when full_every is above 0", () => {
  const form = { ...emptyForm("bk"), name: "nightly", at: "03:00" };
  // 비우거나 0이면 늘 전체라 보내지 않는다.
  expect(specOf(form)).not.toHaveProperty("full_every");
  expect(specOf({ ...form, fullEvery: "0" })).not.toHaveProperty("full_every");
  expect(specOf({ ...form, fullEvery: "6" }).full_every).toBe(6);
  for (const n of ["-1", "1.5", "x", "4294967296"]) {
    expect(checkForm({ ...form, fullEvery: n }, []).fullEvery).toBe("backupJobs.errors.fullEvery");
  }
  // 서버처럼 증분 백업의 복제는 아직 거절한다.
  const copying = { ...form, replicateRemote: "dr", replicatePool: "dr-bk" };
  expect(checkForm({ ...copying, fullEvery: "6" }, []).fullEvery).toBe(
    "backupJobs.errors.fullEveryReplicate",
  );
  expect(checkForm({ ...copying, fullEvery: "0" }, [])).toEqual({});

  const job = backupJob("nightly", 1, { spec: { full_every: 6 } });
  expect(formOf(job).fullEvery).toBe("6");
  expect(formOf(backupJob("n", 2)).fullEvery).toBe("0");
  expect(specOf(formOf(job))).toEqual(job.spec);
  expect(patchOf(job, specOf(formOf(job)))).toEqual({});
  // 0으로 바꾸면 0을 보낸다(빼면 그대로 둔다).
  expect(patchOf(job, specOf({ ...formOf(job), fullEvery: "" }))).toEqual({ full_every: 0 });
  expect(patchOf(backupJob("n", 2), specOf({ ...form, fullEvery: "3" }))).toMatchObject({
    full_every: 3,
  });
});
