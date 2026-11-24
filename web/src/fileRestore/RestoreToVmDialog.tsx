import { useQuery } from "@tanstack/react-query";
import { type RefObject, useState } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, call } from "@/api/client";
import { useList } from "@/api/live";
import { objectTarget } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  defaultTargetDir,
  entryPath,
  type EntryView,
  fileRestoreError,
  type GuestConflict,
  type Part,
  partOf,
  type Session,
  targetDirOk,
  type VolumeView,
} from "@/fileRestore/fileRestore";
import { trackTask } from "@/tasks/tasks";
import { agentChannel } from "@/vms/guest";

const CONFLICTS: GuestConflict[] = ["keep", "skip", "overwrite"];

/**
 * 고른 항목을 실행 중 VM에 게스트 에이전트로 되돌려 쓰는 대화상자(설계 §12·§14): VM(실행 중이고 에이전트 채널이 있고 내가
 * `vm.config`를 가진 것만 — 원래 VM이 그렇다면 그것이 기본이고, 아니면 직접 고르게 한다), 대상 디렉터리(기본은 원래 부모
 * 경로), 충돌 정책. VM을 고르면 에이전트가 허락하는 것(`restore-caps`)을 미리 묻고,
 * 허락하지 않은 선택은 막고 까닭을 보인다. 보내면 Task 진행을 알림으로 본다.
 */
export function RestoreToVmDialog({
  session,
  volume,
  parent,
  entries,
  originalVm,
  returnFocus,
  onClose,
}: {
  session: Session;
  volume: VolumeView;
  /** 고른 항목이 있는 디렉터리 */
  parent: readonly Part[];
  entries: readonly EntryView[];
  originalVm?: string;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const vms = useList("vm");
  const perms = usePermissions();
  // 서버와 같은 조건: 실행 중, 에이전트 채널(`guest_agent`나 HA 게스트 감시), 그 VM의 `vm.config`
  const running = (vms.data ?? []).filter(
    (v) =>
      v.status.phase === "running" &&
      agentChannel(v) &&
      perms.has("vm.config", objectTarget("vm", v)),
  );
  const [vmId, setVmId] = useState<string | undefined>(undefined);
  // 다른 VM을 저절로 고르지 않는다(엉뚱한 게스트에 쓰지 않게) — 원래 VM이 없으면 사용자가 고른다.
  const vm = running.find((v) => v.id === (vmId ?? originalVm));
  const caps = useQuery({
    queryKey: ["file-restore", "caps", vm?.id],
    queryFn: () =>
      call(api.GET("/vms/{key}/guest/restore-caps", { params: { path: { key: vm!.id } } })),
    enabled: !!vm,
    retry: false,
    staleTime: 0,
  });
  const os = caps.data?.os;
  // 사용자가 고치기 전에는 게스트 OS에 맞춘 기본값이다.
  const [typed, setTyped] = useState<string | null>(null);
  const dir = typed ?? defaultTargetDir(parent, os);
  const [conflict, setConflict] = useState<GuestConflict>("keep");
  const [dirError, setDirError] = useState<string | undefined>();
  const hasDir = entries.some((e) => e.kind === "dir");
  const writable = caps.data?.file_write === true;
  const exec = caps.data?.exec === true;
  const blocked =
    !vm || !caps.isSuccess || !writable || (hasDir && !exec) || (conflict === "overwrite" && !exec);

  async function submit() {
    if (!vm || !caps.data) {
      return false;
    }
    if (!targetDirOk(dir, caps.data.os)) {
      setDirError(caps.data.os === "windows" ? "fileRestore.dirWindows" : "fileRestore.dirLinux");
      return false;
    }
    setDirError(undefined);
    let task;
    try {
      task = await call(
        api.POST("/file-restore/sessions/{id}/restore-to-vm", {
          params: { path: { id: session.id } },
          body: {
            volume: volume.id,
            paths: entries.map((e) => entryPath([...parent, partOf(e)])),
            vm: vm.id,
            target_dir: dir,
            conflict,
          },
        }),
      );
    } catch (err) {
      throw err instanceof ApiError ? new Error(fileRestoreError(t, err)) : err;
    }
    trackTask(
      task,
      t("tasks.op.file-restore-to-guest", { target: vm.name, count: entries.length }),
    );
  }

  const capsError = caps.error ? fileRestoreError(t, caps.error) : null;
  return (
    <FormDialog
      title={t("fileRestore.toVmTitle", { count: entries.length })}
      description={t("fileRestore.toVmDescription")}
      submitLabel={t("fileRestore.toVmSubmit")}
      submitDisabled={blocked}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field
        id="fr-vm"
        label={t("fileRestore.vm")}
        hint={running.length === 0 ? t("fileRestore.noRunningVms") : undefined}
      >
        {(a11y) => (
          <NativeSelect
            id="fr-vm"
            className="w-full"
            value={vm?.id ?? ""}
            onChange={(e) => {
              setVmId(e.target.value);
              setTyped(null);
            }}
            {...a11y}
          >
            {!vm && <NativeSelectOption value="">{t("fileRestore.chooseVm")}</NativeSelectOption>}
            {running.map((v) => (
              <NativeSelectOption key={v.id} value={v.id}>
                {v.id === originalVm ? t("fileRestore.originalVm", { name: v.name }) : v.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      {vm && caps.isPending && (
        <p role="status" className="text-sm text-muted-foreground">
          {t("fileRestore.checkingAgent")}
        </p>
      )}
      {capsError && (
        <p role="alert" className="text-sm text-destructive">
          {capsError}
        </p>
      )}
      {caps.data && !writable && (
        <p role="alert" className="text-sm text-destructive">
          {t("fileRestore.noFileWrite", { reason: caps.data.reason ?? "" })}
        </p>
      )}
      {caps.data && writable && hasDir && !exec && (
        <p role="alert" className="text-sm text-destructive">
          {t("fileRestore.noMkdir", { reason: caps.data.reason ?? "" })}
        </p>
      )}
      <Field
        id="fr-target"
        label={t("fileRestore.targetDir")}
        error={dirError}
        hint={t("fileRestore.targetDirHint")}
      >
        {(a11y) => (
          <Input
            id="fr-target"
            className="font-mono"
            spellCheck={false}
            autoComplete="off"
            value={dir}
            onChange={(e) => setTyped(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-1.5 text-sm font-medium">{t("fileRestore.conflict")}</legend>
        {CONFLICTS.map((c) => {
          const off = c === "overwrite" && caps.isSuccess && !exec;
          return (
            <label
              key={c}
              className={`flex items-start gap-2 text-sm ${off ? "text-muted-foreground" : ""}`}
            >
              <input
                type="radio"
                name="fr-conflict"
                value={c}
                checked={conflict === c}
                disabled={off}
                aria-describedby={`fr-conflict-${c}-hint`}
                onChange={() => setConflict(c)}
                className="mt-1"
              />
              <span className="flex flex-col">
                <span className="font-medium">{t(`fileRestore.conflicts.${c}`)}</span>
                <span id={`fr-conflict-${c}-hint`} className="text-xs text-muted-foreground">
                  {off
                    ? t("fileRestore.overwriteNeedsExec", { reason: caps.data?.reason ?? "" })
                    : t(`fileRestore.conflictHints.${c}`)}
                </span>
              </span>
            </label>
          );
        })}
      </fieldset>
      <p className="text-xs text-muted-foreground">{t("fileRestore.toVmLimits")}</p>
    </FormDialog>
  );
}
