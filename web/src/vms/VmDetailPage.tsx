import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";

import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { formatMib, formatTime, uuidTime } from "@/lib/format";
import { newestFirst, TaskTable } from "@/tasks/TasksPage";
import { canDelete, canRun, useVmActions, type VmAction } from "@/vms/actions";
import { VmConfirm } from "@/vms/VmMenu";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";

const TABS = ["summary", "hardware", "tasks", "json"] as const;

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
  const vm = useVmByName(name);
  const [params, setParams] = useSearchParams();
  const tab = TABS.find((t) => t === params.get("tab")) ?? "summary";

  if (vm === undefined) {
    return <p className="text-muted-foreground">{t("common.loading")}</p>;
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
      <Tabs value={tab} onValueChange={(value) => setParams({ tab: value }, { replace: true })}>
        <TabsList>
          {TABS.map((id) => (
            <TabsTrigger key={id} value={id}>
              {t(`vms.tab.${id}`)}
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value="summary">
          <Summary vm={vm} />
        </TabsContent>
        <TabsContent value="hardware">
          <Hardware vm={vm} />
        </TabsContent>
        <TabsContent value="tasks">
          <VmTasks vm={vm} />
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
  const { run, remove } = useVmActions();
  const [confirm, setConfirm] = useState<"force-stop" | "delete" | null>(null);
  const nodeId = vm.status.node ?? vm.spec.node;
  const node = nodes.data?.find((n) => n.id === nodeId);
  const button = (action: VmAction) => (
    <Button
      key={action}
      variant="outline"
      size="sm"
      disabled={!canRun(vm, action)}
      onClick={() => (action === "force-stop" ? setConfirm("force-stop") : void run(vm, action))}
    >
      {t(`vms.action.${action}`)}
    </Button>
  );
  return (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <div className="flex items-center gap-3">
        <h1 className="text-2xl font-semibold">{vm.name}</h1>
        <VmPhaseBadge vm={vm} />
        {node && <span className="text-sm text-muted-foreground">{node.name}</span>}
      </div>
      <div className="flex flex-wrap gap-2">
        {can(me.role, "operate", "vm") &&
          (["start", "stop", "force-stop", "reboot"] as const).map(button)}
        {can(me.role, "write", "vm") && (
          <Button
            variant="destructive"
            size="sm"
            disabled={!canDelete(vm)}
            onClick={() => setConfirm("delete")}
          >
            {t("vms.action.delete")}
          </Button>
        )}
      </div>
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

function Summary({ vm }: { vm: Vm }) {
  const { t, i18n } = useTranslation();
  const { cpu } = vm.spec;
  return (
    <dl className="grid grid-cols-1 gap-4 pt-2 sm:grid-cols-2 lg:grid-cols-3">
      <Field label={t("vms.cpu")}>
        {t("vms.cpuValue", {
          sockets: cpu.sockets ?? 1,
          cores: cpu.cores,
          model: cpu.model ?? "host",
        })}
      </Field>
      <Field label={t("vms.memory")}>{formatMib(vm.spec.memory_mib)}</Field>
      <Field label={t("vms.firmware")}>{(vm.spec.firmware ?? "uefi").toUpperCase()}</Field>
      <Field label={t("vms.machine")}>{vm.spec.machine}</Field>
      <Field label={t("vms.restart")}>{t(`vms.restartPolicy.${vm.spec.restart ?? "never"}`)}</Field>
      <Field label={t("vms.power")}>{t(`vms.powerState.${vm.spec.power ?? "stopped"}`)}</Field>
      <Field label={t("vms.pid")}>{vm.status.pid ?? "—"}</Field>
      <Field label={t("vms.created")}>{formatTime(uuidTime(vm.id), i18n.language)}</Field>
      <Field label={t("vms.lastError")}>{vm.status.last_error ?? "—"}</Field>
    </dl>
  );
}

function Hardware({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-6 pt-2">
      <section className="flex flex-col gap-2">
        <h2 className="font-medium">{t("vms.disks")}</h2>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t("vms.volume")}</TableHead>
              <TableHead>{t("vms.bus")}</TableHead>
              <TableHead>{t("vms.bootOrder")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {(vm.spec.disks ?? []).map((disk) => (
              <TableRow key={disk.volume}>
                <TableCell>{disk.volume}</TableCell>
                <TableCell>{disk.bus}</TableCell>
                <TableCell>{disk.boot_order ?? "—"}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </section>
      <section className="flex flex-col gap-2">
        <h2 className="font-medium">{t("vms.nics")}</h2>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t("vms.network")}</TableHead>
              <TableHead>{t("vms.model")}</TableHead>
              <TableHead>{t("vms.mac")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {(vm.spec.nics ?? []).map((nic, i) => (
              <TableRow key={i}>
                <TableCell>{nic.network}</TableCell>
                <TableCell>{nic.model}</TableCell>
                <TableCell className="font-mono text-xs">{nic.mac ?? "—"}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </section>
    </div>
  );
}

function VmTasks({ vm }: { vm: Vm }) {
  const tasks = useList("task");
  const mine = newestFirst((tasks.data ?? []).filter((task) => task.spec.target === vm.id));
  return <TaskTable tasks={mine.slice(0, 50)} />;
}
