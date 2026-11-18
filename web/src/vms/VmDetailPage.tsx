import { Suspense, useEffect, useId, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";

import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { ReplicationState, Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { VmBackups } from "@/backups/VmBackups";
import { ListPending } from "@/components/ListPending";
import { Pager, usePage } from "@/components/Pager";
import { ReasonButton, ReasonList } from "@/components/Reason";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { HOTPLUG_ERROR_CODES, serverErrorMessage } from "@/i18n/errors";
import { choosableIsos } from "@/isos/isos";
import { formatBytes, formatMib, formatTime, uuidTime } from "@/lib/format";
import { describeProgress, trackTask } from "@/tasks/tasks";
import { newestFirst, TASK_PAGE_SIZE, TaskTable } from "@/tasks/TasksPage";
import { diskVolumes, isTemplate, linkedClones } from "@/templates/templates";
import {
  ACTIONS,
  availability,
  useVmActions,
  type VmAction,
  type VmOperation,
} from "@/vms/actions";
import { bootList, deviceName } from "@/vms/boot";
import { cloudInitSummary } from "@/vms/cloudInit";
import { changeCdrom, trackHotplug } from "@/vms/form/submit";
import { haEnabled, haPolicy } from "@/vms/ha";
import {
  failedChanges,
  hotplugPorts,
  isHotAdded,
  needsRestart,
  pendingFieldLabel,
  pendingReasonKey,
  runningMemory,
  runningVcpus,
  type WithMax,
} from "@/vms/hotplug";
import { VmConsole } from "@/vms/lazyConsole";
import { VmSecurity } from "@/vms/VmFirmware";
import { VmGuestCard, VmGuestTab } from "@/vms/VmGuest";
import { VmMetrics } from "@/vms/VmMetrics";
import {
  formatAge,
  replicaRows,
  replicationWarning,
  unusableDisk,
  type ReplicaRow,
} from "@/vms/replication";
import { useTemplateReasons, VmConfirm, type VmDialog } from "@/vms/VmMenu";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";
import { VmSnapshots } from "@/vms/VmSnapshots";

const TABS = [
  "summary",
  "metrics",
  "hardware",
  "guest",
  "console",
  "snapshots",
  "backups",
  "tasks",
  "json",
] as const;

/** 이름으로 VM을 찾는다. 목록이 아직 없으면 undefined, 없는 VM이면 null */
export function useVmByName(name: string): Vm | null | undefined {
  const vms = useList("vm");
  if (!vms.data) {
    return undefined;
  }
  return vms.data.find((vm) => vm.name === name) ?? null;
}

export function VmDetailPage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const vms = useList("vm");
  const vm = useVmByName(name);
  const me = usePrincipal();
  const [params, setParams] = useSearchParams();
  // 콘솔 탭은 VM 동작 권한이 있을 때만 보인다.
  const tabs = TABS.filter((id) => id !== "console" || can(me.role, "operate", "vm"));
  const tab = tabs.find((id) => id === params.get("tab")) ?? "summary";

  if (vm === undefined) {
    return <ListPending error={vms.error} />;
  }
  if (vm === null) {
    return (
      <div className="flex flex-col items-start gap-2">
        <h1 className="text-2xl font-semibold">{t("vms.notFound", { name })}</h1>
        <Link to="/vms" className="text-sm underline">
          {t("vms.backToList")}
        </Link>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-4">
      <VmHeader vm={vm} />
      <Tabs
        value={tab}
        onValueChange={(value) =>
          // 다른 쿼리 값은 그대로 둔다.
          setParams(
            (p) => {
              p.set("tab", value);
              return p;
            },
            { replace: true },
          )
        }
      >
        <TabsList>
          {tabs.map((id) => (
            <TabsTrigger key={id} value={id}>
              {t(`vms.tab.${id}`)}
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value="summary">
          <Summary vm={vm} />
          <Replication vm={vm} />
          <VmSecurity vm={vm} />
          <VmGuestCard vm={vm} />
          <PendingChanges vm={vm} />
        </TabsContent>
        <TabsContent value="metrics">
          {/* 탭을 열 때만 읽는다. */}
          {tab === "metrics" && <VmMetrics vm={vm} />}
        </TabsContent>
        <TabsContent value="hardware">
          <Hardware vm={vm} />
        </TabsContent>
        <TabsContent value="guest">
          {/* 탭을 열 때만 묻는다. */}
          {tab === "guest" && <VmGuestTab vm={vm} />}
        </TabsContent>
        <TabsContent value="console">
          {/* 탭을 열 때만 연결한다. */}
          {tab === "console" && (
            <Suspense fallback={null}>
              <VmConsole vm={vm} />
            </Suspense>
          )}
        </TabsContent>
        <TabsContent value="snapshots">
          <VmSnapshots vm={vm} />
        </TabsContent>
        <TabsContent value="backups">
          <VmBackups vm={vm} />
        </TabsContent>
        <TabsContent value="tasks">
          <VmTasks key={vm.id} vm={vm} />
        </TabsContent>
        <TabsContent value="json">
          <pre className="overflow-x-auto rounded-md bg-muted p-4 text-xs">
            {JSON.stringify(vm, null, 2)}
          </pre>
        </TabsContent>
      </Tabs>
    </div>
  );
}

function VmHeader({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const navigate = useNavigate();
  const nodes = useList("node");
  const vmTasks = useTaskIndex().get(vm.id) ?? [];
  const hint = useId();
  const { run, remove } = useVmActions();
  const [confirm, setConfirm] = useState<VmDialog | null>(null);
  const template = isTemplate(vm);
  const { convert, revert } = useTemplateReasons(vm);
  const migration = vm.status.migration;
  // 옮기는 중이면 QEMU가 도는 원본(확정하며 spec.node가 먼저 대상이 될 수 있다). 아니면 고정한 노드가
  // 있으면 그 노드, 없으면 스케줄러가 놓은 노드
  const nodeId = (migration ? vm.status.node : null) ?? vm.spec.node ?? vm.status.node;
  const node = nodes.data?.find((n) => n.id === nodeId);
  // 옮기는 중이면 옮겨 갈 노드(지금 노드면 보이지 않는다)
  const target =
    migration && migration.node !== nodeId
      ? nodes.data?.find((n) => n.id === migration.node)
      : undefined;
  // 옮기는 중이면 그 마이그레이션 Task의 진행률(디스크 복사, 메모리)
  const migrationTask = migration && vmTasks.find((task) => task.id === migration.task);
  const progress = migrationTask ? describeProgress(migrationTask, t) : null;
  const operate = can(me.role, "operate", "vm");
  const write = can(me.role, "write", "vm");
  // Task 목록을 아직 모르면 Task 때문에는 막지 않는다(서버에 맡긴다).
  const reasonOf = (action: VmOperation) => {
    const able = availability(vm, action, vmTasks);
    return able.ok ? null : able.reason;
  };
  const consoleReason = reasonOf("console");
  // 보이는 버튼의 막힌 이유만 모은다.
  const reasons = [
    ...(operate ? [...ACTIONS.map(reasonOf), consoleReason, reasonOf("migrate")] : []),
    write ? reasonOf("delete") : null,
    write ? (template ? revert : convert) : null,
  ];
  const button = (action: VmAction) => (
    <ReasonButton
      key={action}
      variant="outline"
      size="sm"
      reason={reasonOf(action)}
      reasons={hint}
      onClick={() => (action === "force-stop" ? setConfirm("force-stop") : void run(vm, action))}
    >
      {t(`vms.action.${action}`)}
    </ReasonButton>
  );
  return (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <div className="flex items-center gap-3">
        <h1 className="text-2xl font-semibold">{vm.name}</h1>
        <VmPhaseBadge vm={vm} />
        {haEnabled(vm.spec) && <Badge variant="outline">{t("vms.ha")}</Badge>}
        {template && <Badge variant="secondary">{t("vms.template")}</Badge>}
        {/* 실행 중에 적용하지 못한 하드웨어 변경이 있다. */}
        {needsRestart(vm) && <Badge variant="secondary">{t("vms.restartNeeded")}</Badge>}
        {node && (
          <span className="text-sm text-muted-foreground">
            {target ? t("vms.migratingTo", { from: node.name, to: target.name }) : node.name}
          </span>
        )}
        {progress && <span className="text-sm text-muted-foreground">{progress}</span>}
      </div>
      <div className="flex flex-wrap gap-2">
        {operate && ACTIONS.map(button)}
        {/* 막혀도 같은 링크로 두어 상태가 바뀔 때 포커스를 잃지 않는다. 막히면 눌러도 열리지 않는다. */}
        {operate && (
          <ReasonButton variant="outline" size="sm" reason={consoleReason} reasons={hint} asChild>
            <Link to={`/vms/${encodeURIComponent(vm.name)}/console`} target="_blank">
              {t("vms.openConsole")}
            </Link>
          </ReasonButton>
        )}
        {operate && (
          <ReasonButton
            variant="outline"
            size="sm"
            reason={reasonOf("migrate")}
            reasons={hint}
            onClick={() => setConfirm("migrate")}
          >
            {t("vms.action.migrate")}
          </ReasonButton>
        )}
        {write && (
          <Button variant="outline" size="sm" asChild>
            <Link to={`/vms/${encodeURIComponent(vm.name)}/edit`}>{t("vms.action.edit")}</Link>
          </Button>
        )}
        {write && can(me.role, "write", "volume") && (
          <Button
            variant="outline"
            size="sm"
            onClick={() => setConfirm(template ? "deploy" : "clone")}
          >
            {template ? t("templates.deploy") : t("vms.action.clone")}
          </Button>
        )}
        {write && (
          <ReasonButton
            variant="outline"
            size="sm"
            reason={template ? revert : convert}
            reasons={hint}
            onClick={() => setConfirm(template ? "untemplate" : "template")}
          >
            {template ? t("templates.revert") : t("templates.convert")}
          </ReasonButton>
        )}
        {write && (
          <ReasonButton
            variant="destructive"
            size="sm"
            reason={reasonOf("delete")}
            reasons={hint}
            onClick={() => setConfirm("delete")}
          >
            {t("vms.action.delete")}
          </ReasonButton>
        )}
      </div>
      <ReasonList base={hint} reasons={reasons} className="basis-full" />
      <VmConfirm
        vm={vm}
        which={confirm}
        onClose={() => setConfirm(null)}
        onRun={run}
        onRemove={remove}
        afterRemove={() => void navigate("/vms")}
      />
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="text-sm">{children}</dd>
    </div>
  );
}

/** 실제 부팅 순서(`disk0, cdrom0`). 켜진 VM이면 도는 QEMU의 CD-ROM 목록 대신 스펙으로 셈한다(순서는 재시작 때 적용). */
function bootLine(vm: Vm): string {
  const order = (o: number | null | undefined) => o ?? null;
  const { booting } = bootList(
    (vm.spec.disks ?? []).map((d) => order(d.boot_order)),
    (vm.spec.cdroms ?? []).map((c) => order(c.boot_order)),
  );
  return booting.map(deviceName).join(", ");
}

/** cloud-init 요약: 사용자·키 수·비밀번호·정적 주소와 켤 때 넣은 instance-id. 바뀐 설정은 다음 재시작 때 적용된다. */
function CloudInitLine({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const ci = vm.spec.cloud_init;
  const hw = vm.status.hardware;
  if (!ci) {
    return <>{t("vms.haOff")}</>;
  }
  const sum = cloudInitSummary(ci);
  const parts = [
    sum.user ? t("vms.ciUser", { user: sum.user }) : t("vms.ciDefaultUser"),
    t("vms.ciKeys", { count: sum.keys }),
    sum.password ? t("vms.ciPassword") : t("vms.ciNoPassword"),
    ...sum.addresses,
  ];
  const pending = (hw?.pending ?? []).some((c) => c.field === "cloud_init");
  return (
    <>
      {parts.join(", ")}
      {hw?.cloud_init && (
        <span className="block font-mono text-xs text-muted-foreground">
          {t("vms.ciInstance", { id: hw.cloud_init })}
        </span>
      )}
      {pending && <span className="block text-xs text-warning-fg">{t("vms.ciPending")}</span>}
    </>
  );
}

/**
 * 템플릿이면 연결 복제 수, 연결 복제 디스크가 있으면 디스크마다 기반 볼륨(`disk0: tmpl-root의 연결 복제`). 둘 다 아니면
 * 그리지 않는다.
 */
function TemplateField({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const volumes = useList("volume").data ?? [];
  const disks = diskVolumes(vm, volumes);
  const linked = disks.flatMap((v, i) => {
    const base = v?.spec.base;
    if (base == null) {
      return [];
    }
    const name = volumes.find((b) => b.id === base)?.name ?? base;
    return [t("vms.linkedDisk", { n: i, base: name })];
  });
  if (isTemplate(vm)) {
    return (
      <Field label={t("vms.template")}>
        {t("vms.templateClones", { count: linkedClones(disks, volumes).length })}
      </Field>
    );
  }
  if (linked.length === 0) {
    return null;
  }
  return (
    <Field label={t("vms.linkedClone")}>
      {linked.map((line) => (
        <span key={line} className="block">
          {line}
        </span>
      ))}
    </Field>
  );
}

function Summary({ vm }: { vm: Vm }) {
  const { t, i18n } = useTranslation();
  const { cpu } = vm.spec;
  const ha = haPolicy(vm.spec);
  const violations = vm.status.rule_violations ?? [];
  const haError = vm.status.ha?.error;
  // 도는 VM은 도는 QEMU의 값(핫플러그로 바뀐 것 포함), 아니면 스펙의 값. 상한은 지금 값보다 클 때만 보인다.
  const hw = vm.status.hardware;
  const vcpus = hw
    ? runningVcpus(hw)
    : { now: (cpu.sockets ?? 1) * cpu.cores, max: cpu.max_vcpus ?? null };
  const memory = hw
    ? runningMemory(hw)
    : { now: vm.spec.memory_mib, max: vm.spec.max_memory_mib ?? null };
  const ports = hw && hotplugPorts(hw);
  const withMax = ({ now, max }: WithMax, format: (n: number) => string) =>
    max !== null && max > now
      ? t("vms.withMax", { value: format(now), max: format(max) })
      : format(now);
  return (
    <dl className="grid grid-cols-1 gap-4 pt-2 sm:grid-cols-2 lg:grid-cols-3">
      <Field label={t("vms.cpu")}>
        {t("vms.cpuValue", {
          sockets: cpu.sockets ?? 1,
          cores: cpu.cores,
          model: cpu.model ?? "host",
        })}
      </Field>
      <Field label={t("vms.vcpus")}>{withMax(vcpus, String)}</Field>
      <Field label={t("vms.memory")}>{withMax(memory, formatMib)}</Field>
      {hw && (
        <Field label={t("vms.hotplug")}>
          {ports ? t("vms.hotplugPorts", ports) : t("vms.hotplugOff")}
        </Field>
      )}
      <Field label={t("vms.bootOrder")}>{bootLine(vm) || t("vms.bootNone")}</Field>
      <Field label={t("vms.cloudInit")}>
        <CloudInitLine vm={vm} />
      </Field>
      <TemplateField vm={vm} />
      <Field label={t("vms.firmware")}>{(vm.spec.firmware ?? "uefi").toUpperCase()}</Field>
      <Field label={t("vms.machine")}>{vm.spec.machine}</Field>
      <Field label={t("vms.restart")}>{t(`vms.restartPolicy.${vm.spec.restart ?? "never"}`)}</Field>
      <Field label={t("vms.power")}>{t(`vms.powerState.${vm.spec.power ?? "stopped"}`)}</Field>
      <Field label={t("vms.pid")}>{vm.status.pid ?? "—"}</Field>
      <Field label={t("vms.created")}>{formatTime(uuidTime(vm.id), i18n.language)}</Field>
      <Field label={t("vms.lastError")}>{vm.status.last_error ?? "—"}</Field>
      <Field label={t("vms.haPolicy")}>
        {ha ? t(`vms.haState.${ha.state}`) : t("vms.haOff")}
        {ha && (
          <span className="block text-xs text-muted-foreground">
            {t("vms.haCounts", { restart: ha.max_restart, relocate: ha.max_relocate })}
          </span>
        )}
      </Field>
      {ha && <Field label={t("vms.haGroup")}>{ha.group ?? "—"}</Field>}
      {ha && <Field label={t("vms.haPriority")}>{ha.priority}</Field>}
      {ha && (
        <Field label={t("vms.guestWatch")}>
          {ha.guest_watch
            ? t("vms.guestWatchSecs", { secs: ha.guest_watch.timeout_secs })
            : t("vms.haOff")}
        </Field>
      )}
      {vm.spec.placement?.pinned && <Field label={t("vms.pinned")}>{t("vms.pinnedOn")}</Field>}
      {/* 리더가 계산한 배치 규칙 위반(켜진 VM만) */}
      <Field label={t("vms.ruleViolations")}>
        {violations.length > 0 ? (
          <span className="text-warning-fg">{violations.join(", ")}</span>
        ) : (
          "—"
        )}
      </Field>
      {haError && (
        <Field label={t("vms.haError")}>
          <span className="text-destructive">{haError}</span>
        </Field>
      )}
    </dl>
  );
}

/** 복제 상태 배지의 모양 */
const REPLICA_BADGE: Record<ReplicationState, React.ComponentProps<typeof Badge>["variant"]> = {
  ready: "default",
  syncing: "secondary",
  pending: "outline",
  stale: "destructive",
};

/** 지금 시각(유닉스 초). 30초마다 다시 읽어 복제본의 나이와 다음 회차를 고친다. */
function useNowSecs(): number {
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    const timer = setInterval(() => setNow(Math.floor(Date.now() / 1000)), 30_000);
    return () => clearInterval(timer);
  }, []);
  return now;
}

/** 지금 복제(`POST /vms/{key}/replicate`) 거절 가운데 번역 대신 서버의 설명을 보이는 code */
const REPLICATE_ERROR_CODES = ["vm_busy"];

/**
 * 로컬 디스크 복제 카드: 대상마다 상태·마지막 시점과 나이·보낸 양·다음 회차·오류와 "지금 복제". 준비된 복제본이라도 VM의
 * 로컬 디스크가 바뀌어 HA가 쓸 수 없으면 그 디스크를 알린다. 복제가 없는 HA VM의 디스크가 노드 로컬 풀에 있으면 다른
 * 노드에서 켤 수 없다고 알린다. HA가 복제본으로 켠 마지막 복구(`last_recovery` — 복제본의 시각과 잃은 쓰기)는 VM이
 * 켜진 뒤에도 남고, 그 기록을 모르는 옛 노드가 쓴 복구 중 `last_error`는 그대로 보인다.
 */
function Replication({ vm }: { vm: Vm }) {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const hint = useId();
  const nodes = useList("node");
  const volumes = useList("volume");
  const pools = useList("storage-pool");
  const now = useNowSecs();
  const rows = replicaRows(vm, nodes.data ?? [], now);
  const warning =
    volumes.isSuccess && pools.isSuccess ? replicationWarning(vm, volumes.data, pools.data) : null;
  if (rows.length === 0 && !warning) {
    return null;
  }
  const operate = can(me.role, "operate", "vm");
  // 꺼진 VM은 복제하지 않는다(서버의 409 `vm_not_running`).
  const reason = vm.status.phase === "running" ? null : "vms.replication.notRunning";
  const recovery = vm.status.last_recovery;
  const recovered =
    !recovery && vm.status.last_error?.startsWith("recovering: ") ? vm.status.last_error : null;
  const nodeName = (id: string) => nodes.data?.find((n) => n.id === id)?.name ?? id;
  const unusable = (row: ReplicaRow) =>
    volumes.isSuccess && pools.isSuccess
      ? unusableDisk(vm, row.node, volumes.data, pools.data)
      : null;

  async function replicate(row: ReplicaRow) {
    try {
      const task = await call(
        api.POST("/vms/{key}/replicate", {
          params: { path: { key: vm.id } },
          body: { node: row.node },
        }),
      );
      trackTask(task, t("tasks.op.vm-replicate", { target: vm.name, node: row.name }));
    } catch (err) {
      toast.error(
        t("vms.actionFailed", {
          name: vm.name,
          error: serverErrorMessage(t, err, REPLICATE_ERROR_CODES),
        }),
      );
    }
  }

  return (
    <Card className="mt-6">
      <CardHeader>
        <CardTitle>
          <h2 id="vm-replication">{t("vms.replication.title")}</h2>
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {warning && (
          <p className="text-sm text-warning-fg">{t("vms.replication.noReplica", warning)}</p>
        )}
        {recovery && (
          <p className="text-sm text-muted-foreground">
            {t("vms.replication.lastRecovery", {
              time: formatTime(new Date(recovery.at * 1000), i18n.language),
              from: nodeName(recovery.from),
              node: nodeName(recovery.node),
              synced: formatTime(new Date(recovery.synced_at * 1000), i18n.language),
            })}
          </p>
        )}
        {recovered && <p className="text-sm text-muted-foreground">{recovered}</p>}
        {rows.length > 0 && (
          <ul aria-labelledby="vm-replication" className="flex flex-col gap-3">
            {rows.map((row) => (
              <li key={row.node} className="flex flex-col gap-1 rounded-md border p-3 text-sm">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-medium">{row.name}</span>
                  <Badge variant={REPLICA_BADGE[row.state]}>
                    {t(`vms.replication.state.${row.state}`)}
                  </Badge>
                  {row.minutes !== null && (
                    <span className="text-xs text-muted-foreground">
                      {t("vms.replication.every", { minutes: row.minutes })}
                      {row.pool && ` · ${t("vms.replication.pool", { pool: row.pool })}`}
                    </span>
                  )}
                  {operate && row.minutes !== null && (
                    <ReasonButton
                      variant="outline"
                      size="sm"
                      className="ml-auto"
                      aria-label={t("vms.replication.nowFor", { node: row.name })}
                      reason={reason}
                      reasons={hint}
                      onClick={() => void replicate(row)}
                    >
                      {t("vms.replication.now")}
                    </ReasonButton>
                  )}
                </div>
                <span>
                  {row.syncedAt !== null && row.age !== null
                    ? t("vms.replication.synced", {
                        time: formatTime(new Date(row.syncedAt * 1000), i18n.language),
                        age: formatAge(row.age),
                      })
                    : t("vms.replication.neverSynced")}
                </span>
                {row.last && (
                  <span className="text-muted-foreground">
                    {t(
                      row.last.full
                        ? "vms.replication.lastFull"
                        : "vms.replication.lastIncremental",
                      {
                        sent: formatBytes(row.last.bytes),
                        size: formatBytes(row.size),
                        secs: row.last.secs,
                      },
                    )}
                  </span>
                )}
                {row.nextRun !== null && (
                  <span className="text-muted-foreground">
                    {row.nextRun <= now
                      ? t("vms.replication.nextSoon")
                      : t("vms.replication.next", {
                          time: formatTime(new Date(row.nextRun * 1000), i18n.language),
                        })}
                  </span>
                )}
                {unusable(row) && (
                  <span className="text-warning-fg">
                    {t("vms.replication.unusable", { volume: unusable(row) })}
                  </span>
                )}
                {row.error && (
                  <span className="whitespace-pre-wrap break-words text-destructive">
                    {t("vms.replication.error", { error: row.error })}
                  </span>
                )}
              </li>
            ))}
          </ul>
        )}
        {operate && <ReasonList base={hint} reasons={[reason]} />}
      </CardContent>
    </Card>
  );
}

function Hardware({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-6 pt-2">
      <section className="flex flex-col gap-2">
        <h2 id="vm-disks" className="font-medium">
          {t("vms.disks")}
        </h2>
        <Table aria-labelledby="vm-disks">
          <TableHeader>
            <TableRow>
              <TableHead>{t("vms.volume")}</TableHead>
              <TableHead>{t("vms.bus")}</TableHead>
              <TableHead>{t("vms.bootOrder")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {(vm.spec.disks ?? []).length === 0 && (
              <TableRow>
                <TableCell colSpan={3} className="text-center text-muted-foreground">
                  {t("vms.noDisks")}
                </TableCell>
              </TableRow>
            )}
            {(vm.spec.disks ?? []).map((disk, i) => (
              <TableRow key={disk.volume}>
                <TableCell>
                  {disk.volume}
                  <HotAdded vm={vm} kind="disk" index={i} />
                </TableCell>
                <TableCell>{disk.bus}</TableCell>
                <TableCell>{disk.boot_order ?? "—"}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </section>
      <Cdroms vm={vm} />
      <section className="flex flex-col gap-2">
        <h2 id="vm-nics" className="font-medium">
          {t("vms.nics")}
        </h2>
        <Table aria-labelledby="vm-nics">
          <TableHeader>
            <TableRow>
              <TableHead>{t("vms.network")}</TableHead>
              <TableHead>{t("vms.model")}</TableHead>
              <TableHead>{t("vms.mac")}</TableHead>
              <TableHead>{t("vmForm.securityGroups")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {(vm.spec.nics ?? []).length === 0 && (
              <TableRow>
                <TableCell colSpan={4} className="text-center text-muted-foreground">
                  {t("vms.noNics")}
                </TableCell>
              </TableRow>
            )}
            {(vm.spec.nics ?? []).map((nic, i) => (
              <TableRow key={i}>
                <TableCell>
                  {nic.network}
                  <HotAdded vm={vm} kind="nic" index={i} />
                </TableCell>
                <TableCell>{nic.model}</TableCell>
                <TableCell className="font-mono text-xs">{nic.mac ?? "—"}</TableCell>
                <TableCell>
                  {nic.security_groups?.length ? nic.security_groups.join(", ") : "—"}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </section>
    </div>
  );
}

/** 매체 바꾸기(`POST /vms/{key}/cdrom`) 거절 가운데 서버의 설명을 보이는 code */
const CDROM_ERROR_CODES = [...HOTPLUG_ERROR_CODES, "volume_not_ready"];

/**
 * CD/DVD 드라이브: 드라이브마다 매체·부팅 순서와 넣기(ISO 고르기)·빼기. 실행 중에도 되고(서버의 핫플러그 Task), 도는
 * QEMU의 매체가 스펙과 다르면(바꾸는 중이거나 실패) 그것을 보인다. 드라이브 더하기·빼기는 고치기 화면에서(멈춘 VM).
 */
function Cdroms({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const volumes = useList("volume");
  const pools = useList("storage-pool");
  const [chosen, setChosen] = useState<Record<number, string>>({});
  const [busy, setBusy] = useState<number | null>(null);
  const cdroms = vm.spec.cdroms ?? [];
  if (cdroms.length === 0) {
    return null;
  }
  const write = can(me.role, "write", "vm");
  const running = vm.status.hardware?.cdroms;
  const isos = choosableIsos(
    volumes.data ?? [],
    pools.data,
    haEnabled(vm.spec) || (vm.spec.replication ?? []).length > 0,
  );
  const { booting } = bootList(
    (vm.spec.disks ?? []).map((d) => d.boot_order ?? null),
    cdroms.map((c) => c.boot_order ?? null),
  );
  const position = (i: number) => {
    const at = booting.findIndex((d) => d.kind === "cdrom" && d.index === i);
    return at === -1 ? "—" : String(at + 1);
  };

  async function change(drive: number, iso: string | null) {
    setBusy(drive);
    const target = iso
      ? t("vms.cdrom.insertLabel", { name: vm.name, drive, iso })
      : t("vms.cdrom.ejectLabel", { name: vm.name, drive });
    try {
      const task = await changeCdrom(vm, drive, iso);
      if (task) {
        void trackHotplug(task, target);
      } else {
        toast.success(t("tasks.done", { label: target }));
      }
    } catch (err) {
      toast.error(
        t("vms.actionFailed", {
          name: vm.name,
          error: serverErrorMessage(t, err, CDROM_ERROR_CODES),
        }),
      );
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className="flex flex-col gap-2">
      <h2 id="vm-cdroms" className="font-medium">
        {t("vms.cdroms")}
      </h2>
      <Table aria-labelledby="vm-cdroms">
        <TableHeader>
          <TableRow>
            <TableHead>{t("vms.cdrom.drive")}</TableHead>
            <TableHead>{t("vms.cdrom.media")}</TableHead>
            <TableHead>{t("vms.bootOrder")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {cdroms.map((cdrom, i) => {
            const was = running?.[i];
            const changing = running !== undefined && (was?.iso ?? null) !== (cdrom.iso ?? null);
            const pick = chosen[i] ?? "";
            return (
              <TableRow key={i}>
                <TableCell>{`cdrom${i}`}</TableCell>
                <TableCell>
                  {cdrom.iso ?? (
                    <span className="text-muted-foreground">{t("vmForm.cdromEmpty")}</span>
                  )}
                  {changing && (
                    <Badge variant="secondary" className="ml-2">
                      {t("vms.cdrom.changing", { from: was?.iso ?? t("vmForm.cdromEmpty") })}
                    </Badge>
                  )}
                </TableCell>
                <TableCell>{position(i)}</TableCell>
                {write && (
                  <TableCell>
                    <div className="flex flex-wrap items-center justify-end gap-2">
                      <NativeSelect
                        aria-label={t("vms.cdrom.choose", { drive: i })}
                        value={pick}
                        disabled={busy !== null}
                        onChange={(e) => setChosen((c) => ({ ...c, [i]: e.target.value }))}
                      >
                        <NativeSelectOption value="">—</NativeSelectOption>
                        {isos
                          .filter((v) => v.name !== cdrom.iso)
                          .map((v) => (
                            <NativeSelectOption key={v.id} value={v.name}>
                              {v.name}
                            </NativeSelectOption>
                          ))}
                      </NativeSelect>
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={busy !== null || pick === ""}
                        onClick={() => void change(i, pick)}
                      >
                        {t("vms.cdrom.insert")}
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        aria-label={t("vms.cdrom.ejectNamed", { drive: i })}
                        disabled={busy !== null || cdrom.iso === undefined}
                        onClick={() => void change(i, null)}
                      >
                        {t("vms.cdrom.eject")}
                      </Button>
                    </div>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </section>
  );
}

/** 실행 중에 붙인 장치의 표시 */
function HotAdded({ vm, kind, index }: { vm: Vm; kind: "disk" | "nic"; index: number }) {
  const { t } = useTranslation();
  if (!isHotAdded(vm, kind, index)) {
    return null;
  }
  return (
    <Badge variant="outline" className="ml-2">
      {t("vms.hotAdded")}
    </Badge>
  );
}

/**
 * 다음 재시작 때 적용할 하드웨어 변경(핫플러그 Task가 실행 중에 적용하지 못한 것). 실패한 것이 있으면 다시 적용해
 * 볼 수 있다(`actions/hotplug` — 게스트가 드라이버를 올린 뒤 등).
 */
function PendingChanges({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const pending = vm.status.hardware?.pending ?? [];
  if (pending.length === 0) {
    return null;
  }
  const retry =
    failedChanges(vm).length > 0 && can(me.role, "operate", "vm") && vm.status.phase === "running";

  async function applyAgain() {
    try {
      const task = await call(
        api.POST("/vms/{key}/actions/{action}", {
          params: { path: { key: vm.id, action: "hotplug" } },
        }),
      );
      trackTask(task, t("tasks.op.vm-hotplug", { target: vm.name }));
    } catch (err) {
      toast.error(
        t("vms.actionFailed", {
          name: vm.name,
          error: serverErrorMessage(t, err, HOTPLUG_ERROR_CODES),
        }),
      );
    }
  }

  return (
    <section className="mt-6 flex flex-col gap-2">
      <h2 id="vm-pending" className="font-medium">
        {t("vms.pendingTitle")}
      </h2>
      <p className="text-sm text-muted-foreground">{t("vms.pendingNote")}</p>
      <ul aria-labelledby="vm-pending" className="flex flex-col gap-1 text-sm">
        {pending.map((change) => (
          <li key={change.field}>
            <span className="font-medium">{pendingFieldLabel(change.field, t)}</span>
            {": "}
            {t("vms.pendingValue", { running: change.running, wanted: change.wanted })}
            {" — "}
            {t(pendingReasonKey(change.reason))}
            {change.detail && (
              <span className="block text-xs text-muted-foreground">{change.detail}</span>
            )}
          </li>
        ))}
      </ul>
      {retry && (
        <div>
          <Button variant="outline" size="sm" onClick={() => void applyAgain()}>
            {t("vms.applyAgain")}
          </Button>
        </div>
      )}
    </section>
  );
}

function VmTasks({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const tasks = useList("task");
  const mine = useMemo(
    () => newestFirst((tasks.data ?? []).filter((task) => task.spec.target === vm.id)),
    [tasks.data, vm.id],
  );
  const pager = usePage(mine, TASK_PAGE_SIZE);
  return (
    <section className="flex flex-col gap-4">
      <h2 id="vm-tasks" className="sr-only">
        {t("vms.tab.tasks")}
      </h2>
      <TaskTable tasks={pager.pageItems} labelledBy="vm-tasks" />
      <Pager {...pager} pageSize={TASK_PAGE_SIZE} total={mine.length} />
    </section>
  );
}
