import { Camera } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Snapshot, Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatBytes, formatTime } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";
import { isBusy } from "@/vms/actions";
import { descriptionError, snapshotNameError, snapshotTree } from "@/vms/snapshots";

/** VM 상세의 스냅샷 탭: 나무 목록과 만들기·되돌리기·삭제 */
export function VmSnapshots({ vm }: { vm: Vm }) {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const snapshots = useList("snapshot");
  const volumes = useList("volume");
  const [creating, setCreating] = useState(false);
  // 닫히는 동안에도 대상과 동작을 남겨 두어 대화상자의 글이 바뀌지 않게 한다.
  const [confirm, setConfirm] = useState<{
    action: "revert" | "delete";
    snapshot: Snapshot;
  } | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  // 트리거 없이 연 대화상자는 닫힐 때 포커스를 돌려줄 곳을 모르므로 연 버튼을 기억한다.
  const opener = useRef<HTMLElement | null>(null);
  const operate = can(me.role, "operate", "vm");
  const mine = (snapshots.data ?? []).filter((s) => s.spec.vm === vm.id);
  const rows = snapshotTree(mine);
  const names = new Map(mine.map((s) => [s.id, s.spec.name]));
  // 서버는 시작·정지 중인 VM의 스냅샷 작업을 받지 않는다(vm_busy).
  const busy = isBusy(vm);
  // qcow2가 아닌 디스크가 있으면 만들 수 없다(snapshot_unsupported). 볼륨 목록을 다 읽어야 안다.
  const unsupported = volumes.isSuccess
    ? (vm.spec.disks ?? [])
        .map((d) => volumes.data.find((v) => v.name === d.volume))
        .find((v) => v !== undefined && v.spec.format !== "qcow2")
    : undefined;
  const columns = operate ? 5 : 4;
  const schedule = vm.spec.snapshot_schedule;

  function ask(e: React.MouseEvent<HTMLElement>, action: "revert" | "delete", snapshot: Snapshot) {
    opener.current = e.currentTarget;
    setConfirm({ action, snapshot });
    setConfirmOpen(true);
  }

  return (
    <section className="flex flex-col gap-4 pt-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="vm-snapshots" className="font-medium">
          {t("vms.tab.snapshots")}
        </h2>
        {operate &&
          (unsupported ? (
            <p className="text-sm text-muted-foreground">
              {t("snapshots.unsupported", {
                volume: unsupported.name,
                format: unsupported.spec.format,
              })}
            </p>
          ) : (
            <div className="flex flex-wrap items-center gap-2 text-sm">
              {/* 볼륨 목록을 읽어야 디스크 형식을 알아 만들 수 있다. */}
              {!volumes.isSuccess && <ListPending error={volumes.error} />}
              <Button
                size="sm"
                disabled={busy || !snapshots.isSuccess || !volumes.isSuccess}
                onClick={(e) => {
                  opener.current = e.currentTarget;
                  setCreating(true);
                }}
              >
                <Camera />
                {t("snapshots.create")}
              </Button>
            </div>
          ))}
      </div>
      {operate && busy && <p className="text-sm text-muted-foreground">{t("snapshots.busy")}</p>}
      <div className="flex flex-col gap-1 text-sm">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <p>
            {schedule
              ? t(schedule.memory ? "snapshots.scheduleMemory" : "snapshots.schedule", {
                  every: t(`snapshots.every.${schedule.every}`),
                  keep: schedule.keep,
                })
              : t("snapshots.noSchedule")}
          </p>
          {/* 예약은 VM 설정이라 VM 편집에서 바꾼다. */}
          {can(me.role, "write", "vm") && (
            <Link
              to={`/vms/${encodeURIComponent(vm.name)}/edit`}
              className="underline-offset-4 hover:underline"
            >
              {t("snapshots.scheduleEdit")}
            </Link>
          )}
        </div>
        {/* 예약을 꺼도 마지막 실패는 남는다. 서버가 쓴 이유를 줄바꿈·긴 낱말까지 그대로 보인다. */}
        {vm.status.schedule_error && (
          <p className="text-destructive">
            <span className="font-medium">{t("snapshots.scheduleFailed")}</span>{" "}
            <span className="whitespace-pre-wrap break-words">
              {vm.status.schedule_error.message}
            </span>
          </p>
        )}
      </div>
      <Table aria-labelledby="vm-snapshots">
        <TableHeader>
          <TableRow>
            <TableHead>{t("snapshots.name")}</TableHead>
            <TableHead>{t("snapshots.created")}</TableHead>
            <TableHead>{t("snapshots.memory")}</TableHead>
            <TableHead>{t("snapshots.description")}</TableHead>
            {operate && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.length === 0 && (
            <TableMessage colSpan={columns} query={snapshots} empty={t("snapshots.empty")} />
          )}
          {rows.map(({ snapshot, depth }) => {
            const ready = snapshot.status.phase === "ready";
            const { name } = snapshot.spec;
            const parent = depth > 0 ? names.get(snapshot.spec.parent ?? "") : undefined;
            return (
              <TableRow key={snapshot.id}>
                {/* 깊이만큼 들여 써 나무를 보인다. */}
                <TableCell
                  className="font-medium"
                  style={{ paddingLeft: `${0.5 + depth * 1.5}rem` }}
                >
                  {name}
                  {/* 들여쓰기는 보이기만 하므로 스크린 리더에는 부모를 말로 알린다. */}
                  {parent !== undefined && (
                    <span className="sr-only"> {t("snapshots.after", { parent })}</span>
                  )}
                  {/* 공백이 없으면 접근 이름이 "이름현재"로 붙는다. */}
                  {snapshot.spec.scheduled && (
                    <>
                      {" "}
                      <Badge variant="outline" className="ml-1">
                        {t("snapshots.scheduled")}
                      </Badge>
                    </>
                  )}
                  {vm.status.snapshot === snapshot.id && (
                    <>
                      {" "}
                      <Badge className="ml-1">{t("snapshots.current")}</Badge>
                    </>
                  )}
                </TableCell>
                <TableCell>
                  {!ready ? (
                    <Badge variant="secondary">{t("snapshots.pending")}</Badge>
                  ) : snapshot.status.created_at !== null ? (
                    formatTime(new Date(snapshot.status.created_at * 1000), i18n.language)
                  ) : (
                    "—"
                  )}
                </TableCell>
                <TableCell>
                  {!snapshot.spec.memory
                    ? t("snapshots.memoryNo")
                    : snapshot.status.vmstate_bytes !== null
                      ? t("snapshots.memorySize", {
                          size: formatBytes(snapshot.status.vmstate_bytes),
                        })
                      : t("snapshots.memoryYes")}
                </TableCell>
                <TableCell className="whitespace-normal break-words">
                  {snapshot.spec.description || "—"}
                </TableCell>
                {operate && (
                  <TableCell className="text-right whitespace-nowrap">
                    <Button
                      variant="outline"
                      size="sm"
                      aria-label={t("snapshots.revertNamed", { name })}
                      disabled={busy || !ready}
                      onClick={(e) => ask(e, "revert", snapshot)}
                    >
                      {t("snapshots.revert")}
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="ml-2"
                      aria-label={t("snapshots.deleteNamed", { name })}
                      disabled={busy || !ready}
                      onClick={(e) => ask(e, "delete", snapshot)}
                    >
                      {t("snapshots.delete")}
                    </Button>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && (
        <CreateSnapshotDialog
          vm={vm}
          taken={mine.map((s) => s.spec.name)}
          returnFocus={opener}
          onClose={() => setCreating(false)}
        />
      )}
      <ConfirmDialog
        open={confirmOpen && confirm?.action === "revert"}
        onOpenChange={setConfirmOpen}
        returnFocus={opener}
        title={t("snapshots.revertTitle", { vm: vm.name, name: confirm?.snapshot.spec.name })}
        description={t(
          confirm?.snapshot.spec.memory
            ? "snapshots.revertDescriptionMemory"
            : "snapshots.revertDescription",
          { vm: vm.name },
        )}
        confirmLabel={t("snapshots.revert")}
        onConfirm={async () => {
          const { snapshot } = confirm!;
          const task = await call(
            api.POST("/vms/{key}/snapshots/{name}/revert", {
              params: { path: { key: vm.id, name: snapshot.id } },
            }),
          );
          trackTask(task, t("snapshots.op.revert", { vm: vm.name, name: snapshot.spec.name }));
        }}
      />
      <ConfirmDialog
        open={confirmOpen && confirm?.action === "delete"}
        onOpenChange={setConfirmOpen}
        returnFocus={opener}
        title={t("snapshots.deleteTitle", { name: confirm?.snapshot.spec.name })}
        description={t("snapshots.deleteDescription")}
        confirmLabel={t("snapshots.delete")}
        onConfirm={async () => {
          const { snapshot } = confirm!;
          const task = await call(
            api.DELETE("/vms/{key}/snapshots/{name}", {
              params: { path: { key: vm.id, name: snapshot.id } },
            }),
          );
          trackTask(task, t("snapshots.op.delete", { vm: vm.name, name: snapshot.spec.name }));
        }}
      />
    </section>
  );
}

function CreateSnapshotDialog({
  vm,
  taken,
  returnFocus,
  onClose,
}: {
  vm: Vm;
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [memory, setMemory] = useState(true);
  const [errors, setErrors] = useState<Record<string, string>>({});
  // 서버는 실행 중이고 디스크가 있는 VM만 메모리를 담는다. 그 밖에는 묻지 않고 디스크만 담는다.
  const canMemory = vm.status.phase === "running" && (vm.spec.disks ?? []).length > 0;

  async function submit() {
    const found: Record<string, string> = {};
    const nameError = snapshotNameError(name, taken);
    if (nameError) {
      found.name = nameError;
    }
    const tooLong = descriptionError(description);
    if (tooLong) {
      found.description = tooLong;
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const task = await call(
      api.POST("/vms/{key}/snapshots", {
        params: { path: { key: vm.id } },
        body: { name, description, memory: canMemory && memory },
      }),
    );
    trackTask(task, t("snapshots.op.create", { vm: vm.name, name }));
  }

  return (
    <FormDialog
      title={t("snapshots.createTitle", { name: vm.name })}
      description={t("snapshots.createDescription")}
      submitLabel={t("snapshots.create")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="snapshot-name" label={t("snapshots.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="snapshot-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field
        id="snapshot-description"
        label={t("snapshots.description")}
        error={errors.description}
      >
        {(a11y) => (
          <Input
            id="snapshot-description"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      {canMemory && (
        <div className="flex flex-col gap-1.5">
          <div className="flex items-center gap-2">
            <Checkbox
              id="snapshot-memory"
              checked={memory}
              onCheckedChange={(v) => setMemory(v === true)}
            />
            <Label htmlFor="snapshot-memory">{t("snapshots.includeMemory")}</Label>
          </div>
          <p className="text-xs text-muted-foreground">{t("snapshots.includeMemoryHint")}</p>
        </div>
      )}
    </FormDialog>
  );
}
