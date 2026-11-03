import { Pencil, Play, Plus, Trash2 } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { BackupJob } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { backupPools } from "@/backups/backups";
import {
  checkForm,
  emptyForm,
  formatUtc,
  formOf,
  type JobForm,
  KEEP_UNITS,
  keepParts,
  patchOf,
  specOf,
  WEEKDAYS,
} from "@/backups/jobs";
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
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";

/** 백업 작업: 목록과 만들기·바꾸기·지우기·지금 실행. 시각은 모두 UTC다(작업 일정이 UTC다). */
export function BackupJobsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const jobs = useList("backup-job");
  const pools = useList("storage-pool");
  // 만들기는 `null`, 바꾸기는 그 작업
  const [editing, setEditing] = useState<BackupJob | null | undefined>(undefined);
  const [deleting, setDeleting] = useState<BackupJob | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const write = can(me.role, "write", "backup-job");
  const list = [...(jobs.data ?? [])].sort((a, b) => a.name.localeCompare(b.name));

  async function run(job: BackupJob) {
    try {
      await call(api.POST("/backup-jobs/{key}/run", { params: { path: { key: job.id } } }));
      toast.success(t("backupJobs.runRequested", { name: job.name }));
    } catch (err) {
      toast.error(errorMessage(t, err));
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="backup-jobs-title" className="text-2xl font-semibold">
          {t("nav.backupJobs")}
        </h1>
        {write && (
          <div className="flex flex-wrap items-center gap-2 text-sm">
            {/* 풀 목록을 읽어야 백업 풀을 고른다. */}
            {!pools.isSuccess && <ListPending error={pools.error} />}
            <Button
              disabled={!pools.isSuccess || !jobs.isSuccess}
              onClick={(e) => {
                opener.current = e.currentTarget;
                setEditing(null);
              }}
            >
              <Plus />
              {t("common.create")}
            </Button>
          </div>
        )}
      </div>
      <p className="text-sm text-muted-foreground">{t("backupJobs.pageHint")}</p>
      <Table aria-labelledby="backup-jobs-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("backupJobs.vms")}</TableHead>
            <TableHead>{t("backups.pool")}</TableHead>
            <TableHead>{t("backupJobs.schedule")}</TableHead>
            <TableHead>{t("backupJobs.keep")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("backupJobs.nextRun")}</TableHead>
            <TableHead>{t("backupJobs.lastRun")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 && (
            <TableMessage colSpan={write ? 9 : 8} query={jobs} empty={t("backupJobs.empty")} />
          )}
          {list.map((job) => {
            const { vms, schedule, enabled } = job.spec;
            const { run: running, last_run: last, next_run: next, errors } = job.status;
            const days = schedule.days ?? [];
            const keep = keepParts(job.spec.keep);
            return (
              <TableRow key={job.id}>
                <TableCell className="font-medium">{job.name}</TableCell>
                <TableCell>
                  {"all" in vms ? (
                    t("backupJobs.targets.all")
                  ) : "names" in vms ? (
                    vms.names.join(", ")
                  ) : (
                    <span className="font-mono text-xs">
                      {Object.entries(vms.labels)
                        .map(([k, v]) => `${k}=${v}`)
                        .join(", ")}
                    </span>
                  )}
                </TableCell>
                <TableCell>{job.spec.pool}</TableCell>
                <TableCell>
                  {days.length === 0
                    ? t("backupJobs.everyDay")
                    : days.map((d) => t(`backupJobs.day.${d}`)).join(", ")}{" "}
                  {schedule.at}
                </TableCell>
                <TableCell>
                  {keep.length === 0
                    ? t("backupJobs.keepAll")
                    : keep.map(([unit, n]) => t(`backupJobs.keepUnit.${unit}`, { n })).join(", ")}
                </TableCell>
                <TableCell>
                  {running ? (
                    <>
                      <Badge>{t("backupJobs.state.running")}</Badge>
                      <div className="text-xs text-muted-foreground">
                        {t("backupJobs.waiting", { count: running.queue?.length ?? 0 })}
                      </div>
                    </>
                  ) : (
                    <Badge variant={enabled ? "secondary" : "outline"}>
                      {t(enabled ? "backupJobs.state.enabled" : "backupJobs.state.disabled")}
                    </Badge>
                  )}
                </TableCell>
                <TableCell>{enabled && next !== null ? formatUtc(next) : "—"}</TableCell>
                <TableCell>
                  {last ? (
                    <>
                      {formatUtc(last.finished_at)}
                      <div className="text-xs text-muted-foreground">
                        {t("backupJobs.result", { ok: last.ok, failed: last.failed })}
                      </div>
                    </>
                  ) : (
                    "—"
                  )}
                  {/* 마지막(또는 진행 중인) 실행에서 백업하지 못한 VM */}
                  {errors.length > 0 && (
                    <ul className="max-w-xs text-xs whitespace-normal break-words text-destructive">
                      {errors.map((e) => (
                        <li key={e.vm}>
                          {e.vm}: {e.error}
                        </li>
                      ))}
                    </ul>
                  )}
                </TableCell>
                {write && (
                  <TableCell className="text-right whitespace-nowrap">
                    <Button
                      variant="outline"
                      size="sm"
                      aria-label={t("backupJobs.runNamed", { name: job.name })}
                      // 서버는 진행 중이거나 꺼 둔 작업을 거절한다.
                      disabled={running !== null || !enabled}
                      onClick={() => void run(job)}
                    >
                      <Play />
                      {t("backupJobs.runNow")}
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      className="ml-2"
                      aria-label={t("backupJobs.editNamed", { name: job.name })}
                      onClick={(e) => {
                        opener.current = e.currentTarget;
                        setEditing(job);
                      }}
                    >
                      <Pencil />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("backupJobs.deleteNamed", { name: job.name })}
                      onClick={(e) => {
                        opener.current = e.currentTarget;
                        setDeleting(job);
                        setDeleteOpen(true);
                      }}
                    >
                      <Trash2 />
                    </Button>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {editing !== undefined && (
        <JobDialog
          job={editing}
          taken={(jobs.data ?? []).map((j) => j.name)}
          returnFocus={opener}
          onClose={() => setEditing(undefined)}
        />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("backupJobs.deleteTitle", { name: deleting?.name })}
        description={t("backupJobs.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const job = deleting!;
          await call(api.DELETE("/backup-jobs/{key}", { params: { path: { key: job.id } } }));
          toast.success(t("backupJobs.deleted", { name: job.name }));
        }}
      />
    </div>
  );
}

/** 작업 만들기(`job`이 null)와 바꾸기. 열 때만 그린다. */
function JobDialog({
  job,
  taken,
  returnFocus,
  onClose,
}: {
  job: BackupJob | null;
  /** 있는 작업 이름 */
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const pools = backupPools(useList("storage-pool").data ?? []).map((p) => p.name);
  // 작업의 풀이 이제 백업 풀이 아니어도 고른 값으로 보인다(서버가 저장할 때 거절한다).
  if (job && !pools.includes(job.spec.pool)) {
    pools.unshift(job.spec.pool);
  }
  const [form, setForm] = useState<JobForm>(() => (job ? formOf(job) : emptyForm(pools[0] ?? "")));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const change = (part: Partial<JobForm>) => setForm((f) => ({ ...f, ...part }));
  const unchanged = job !== null && Object.keys(patchOf(job, specOf(form))).length === 0;

  async function submit() {
    const found = checkForm(form, job ? null : taken);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    if (job) {
      await call(
        api.PATCH("/backup-jobs/{key}", {
          params: { path: { key: job.id } },
          body: patchOf(job, specOf(form)),
        }),
      );
      toast.success(t("backupJobs.saved", { name: job.name }));
    } else {
      await call(api.POST("/backup-jobs", { body: { name: form.name, spec: specOf(form) } }));
      toast.success(t("backupJobs.created", { name: form.name }));
    }
  }

  return (
    <FormDialog
      title={job ? t("backupJobs.editTitle", { name: job.name }) : t("backupJobs.createTitle")}
      description={t(job ? "backupJobs.editDescription" : "backupJobs.createDescription")}
      submitLabel={job ? t("common.save") : t("common.create")}
      submitDisabled={pools.length === 0 || unchanged}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {!job && (
        <Field id="job-name" label={t("common.name")} error={errors.name}>
          {(a11y) => (
            <Input
              id="job-name"
              value={form.name}
              onChange={(e) => change({ name: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      {pools.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("backups.noPools")}</p>
      ) : (
        <Field id="job-pool" label={t("backups.pool")} error={errors.pool}>
          {(a11y) => (
            <NativeSelect
              id="job-pool"
              className="w-full"
              value={form.pool}
              onChange={(e) => change({ pool: e.target.value })}
              {...a11y}
            >
              {pools.map((p) => (
                <NativeSelectOption key={p} value={p}>
                  {p}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          )}
        </Field>
      )}
      <Field id="job-target" label={t("backupJobs.target")}>
        <NativeSelect
          id="job-target"
          className="w-full"
          value={form.target}
          onChange={(e) => change({ target: e.target.value as JobForm["target"] })}
        >
          {(["all", "names", "labels"] as const).map((target) => (
            <NativeSelectOption key={target} value={target}>
              {t(`backupJobs.targets.${target}`)}
            </NativeSelectOption>
          ))}
        </NativeSelect>
      </Field>
      {form.target === "names" && (
        <Field
          id="job-names"
          label={t("backupJobs.names")}
          error={errors.names}
          hint={t("backupJobs.namesHint")}
        >
          {(a11y) => (
            <Input
              id="job-names"
              placeholder="web, db"
              value={form.names}
              onChange={(e) => change({ names: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      {form.target === "labels" && (
        <Field
          id="job-labels"
          label={t("backupJobs.labels")}
          error={errors.labels}
          hint={t("backupJobs.labelsHint")}
        >
          {(a11y) => (
            <Input
              id="job-labels"
              placeholder="tier=prod"
              value={form.labels}
              onChange={(e) => change({ labels: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      <Field id="job-at" label={t("backupJobs.at")} error={errors.at} hint={t("backupJobs.atHint")}>
        {(a11y) => (
          <Input
            id="job-at"
            placeholder="03:00"
            value={form.at}
            onChange={(e) => change({ at: e.target.value })}
            {...a11y}
          />
        )}
      </Field>
      <fieldset className="flex flex-col gap-1.5">
        <legend className="mb-1.5 text-sm font-medium">{t("backupJobs.days")}</legend>
        <div className="flex flex-wrap gap-3">
          {WEEKDAYS.map((day) => (
            <div key={day} className="flex items-center gap-1.5">
              <Checkbox
                id={`job-day-${day}`}
                checked={form.days.includes(day)}
                onCheckedChange={(on) =>
                  change({
                    days: on === true ? [...form.days, day] : form.days.filter((d) => d !== day),
                  })
                }
              />
              <Label htmlFor={`job-day-${day}`}>{t(`backupJobs.day.${day}`)}</Label>
            </div>
          ))}
        </div>
        <p className="text-xs text-muted-foreground">{t("backupJobs.daysHint")}</p>
      </fieldset>
      <fieldset className="flex flex-col gap-1.5">
        <legend className="mb-1.5 text-sm font-medium">{t("backupJobs.retention")}</legend>
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          {KEEP_UNITS.map((unit) => (
            <div key={unit} className="flex flex-col gap-1.5">
              <Label htmlFor={`job-keep-${unit}`}>{t(`backupJobs.keepLabel.${unit}`)}</Label>
              <Input
                id={`job-keep-${unit}`}
                inputMode="numeric"
                placeholder="0"
                value={form.keep[unit]}
                aria-invalid={errors.keep ? true : undefined}
                onChange={(e) => change({ keep: { ...form.keep, [unit]: e.target.value } })}
              />
            </div>
          ))}
        </div>
        {errors.keep ? (
          <p className="text-xs text-destructive">{t(errors.keep)}</p>
        ) : (
          <p className="text-xs text-muted-foreground">{t("backupJobs.retentionHint")}</p>
        )}
      </fieldset>
      <div className="flex items-center gap-2">
        <Checkbox
          id="job-enabled"
          checked={form.enabled}
          onCheckedChange={(on) => change({ enabled: on === true })}
        />
        <Label htmlFor="job-enabled">{t("backupJobs.enabled")}</Label>
      </div>
    </FormDialog>
  );
}
