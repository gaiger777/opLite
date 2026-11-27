// DR(계획 76) 화면 시험의 견본: 서버 보기 모양 그대로의 계획·실행·보고.
import type { Node } from "@/api/types";
import type { DrReport, DrRun, RecoveryPlan, RecoveryPlanSpec } from "@/dr/api";
import { NODE } from "@/test/fixtures";

export const PLAN_ID = "019a0f3c-0000-7000-8076-000000000001";
export const RUN_ID = "019a0f3c-0000-7000-8076-0000000000a1";
/** 시험의 지금(2026-11-27 근처, 유닉스 초) */
export const NOW = 1_795_000_000;

/** `dr-plans`를 아는 노드 */
export const DR_NODE: Node = { ...NODE, status: { ...NODE.status, features: ["dr-plans"] } };

export function planSpec(extra: Partial<RecoveryPlanSpec> = {}): RecoveryPlanSpec {
  return {
    source: "prod",
    pool: "local",
    rpo_target_secs: 86_400,
    on_timeout: "continue",
    networks: { lan: "dr-lan" },
    groups: [
      { name: "db", wait: "guest", timeout_secs: 600, delay_secs: 30, vms: [{ vm: "db1" }] },
      {
        name: "web",
        wait: "running",
        timeout_secs: 300,
        delay_secs: 0,
        vms: [
          { vm: "web1", ip: [{ nic: 0, address: "10.20.0.5/24", gateway: "10.20.0.1" }] },
          { vm: "web2" },
        ],
      },
    ],
    ...extra,
  };
}

export function plan(
  name = "web-tier",
  extra: {
    spec?: Partial<RecoveryPlanSpec>;
    status?: Partial<RecoveryPlan["status"]>;
    revision?: number;
  } = {},
): RecoveryPlan {
  return {
    id: PLAN_ID,
    name,
    revision: extra.revision ?? 4,
    labels: {},
    spec: planSpec(extra.spec),
    status: {
      checked_at: NOW,
      protection: [
        { vm: "db1", ok: true, backup: "db1-copy", point_at: NOW - 3600, age_secs: 3600 },
        { vm: "web1", ok: true, backup: "web1-copy", point_at: NOW - 7200, age_secs: 7200 },
        { vm: "web2", ok: false, issue: "no-recovery-point" },
      ],
      last_test: { run: RUN_ID, at: NOW - 600, rto_secs: 754, result: "succeeded" },
      ...extra.status,
    },
  };
}

export function report(extra: Partial<DrReport> = {}): DrReport {
  return {
    at: NOW,
    plan: "web-tier",
    plan_id: PLAN_ID,
    source: "prod",
    source_registered: true,
    rpo_target_secs: 86_400,
    protected: 2,
    total: 3,
    protection: plan().status.protection!,
    tests: [
      {
        id: RUN_ID,
        name: "web-tier-test-1",
        kind: "test",
        state: "partial",
        started_at: NOW - 1400,
        finished_at: NOW - 600,
        rto_secs: 754,
        failed_vms: ["web2"],
      },
    ],
    ...extra,
  };
}

export function run(
  extra: { spec?: Partial<DrRun["spec"]>; status?: Partial<DrRun["status"]> } = {},
): DrRun {
  return {
    id: RUN_ID,
    name: "web-tier-test-1",
    revision: 3,
    labels: {},
    spec: {
      kind: "test",
      plan: PLAN_ID,
      plan_name: "web-tier",
      plan_spec: planSpec(),
      max_parallel: 4,
      ...extra.spec,
    },
    status: {
      state: "running",
      started_at: NOW - 90,
      group: 0,
      steps: [
        {
          group: "db",
          vm: "db1",
          phase: "waiting",
          at: NOW - 30,
          new_vm: "db1-drt",
          backup: "db1-copy",
          task: "019a0f3c-0000-7000-8005-000000000001",
        },
        { group: "web", vm: "web1", phase: "pending", at: NOW - 90 },
        { group: "web", vm: "web2", phase: "pending", at: NOW - 90 },
      ],
      ...extra.status,
    },
  };
}
