import type { TFunction } from "i18next";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams } from "react-router";

import { ApiError, api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Task, VmImport } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { codeMessage, errorMessage } from "@/i18n/errors";
import { runForm, sourceNetworks, toRun, validateRun, type RunForm } from "@/imports/imports";
import { ImportedVm, ImportPhaseBadge } from "@/imports/ImportsPage";
import { formatBytes, formatMib } from "@/lib/format";
import { poolsUsableOn } from "@/storage/pools";
import { trackTask } from "@/tasks/tasks";

/** 사용자 네트워크(NAT). 네트워크 리소스가 아니어도 매핑할 수 있다. */
const USER_NETWORK = "user";

/** 서버 오류를 가져오기에 맞는 문장으로(`node_offline`은 VM이 아니라 OVA가 있는 노드다). */
function importError(t: TFunction, err: unknown): string {
  return err instanceof ApiError
    ? codeMessage(t, err.code, err.message, "imports.codes")
    : errorMessage(t, err);
}

/** 가져오기 하나: 원본 요약·경고·오류, `ready`면 실행 폼, 끝났으면 만든 VM. */
export function ImportPage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const imports = useList("vm-import");
  if (!imports.data) {
    return <ListPending error={imports.error} />;
  }
  const imp = imports.data.find((i) => i.name === name);
  if (!imp) {
    return <h1 className="text-2xl font-semibold">{t("imports.notFound", { name })}</h1>;
  }
  return <ImportView key={imp.id} imp={imp} />;
}

function ImportView({ imp }: { imp: VmImport }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const navigate = useNavigate();
  const nodes = useList("node");
  const write = can(me.role, "write", "vm-import");
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  // 폼은 가져오기마다 하나다. 실행이 실패해 `ready`로 돌아와도 고친 값이 남는다.
  const [form, setForm] = useState<RunForm>(() => runForm(imp));
  // 실행을 보낸 때의 revision. 가져오기가 바뀔 때까지(성공하면 `importing`이 된다) 다시 보내거나 지우지 않는다.
  const [sentAt, setSentAt] = useState<number | null>(null);
  const busy = sentAt === imp.revision;
  const node = nodes.data?.find((n) => n.id === imp.spec.node)?.name ?? "—";
  const { phase, error, warnings, source_vm: src } = imp.status;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-end justify-between gap-2">
        <div className="flex flex-col gap-1">
          <Link
            to="/imports"
            className="text-sm text-muted-foreground underline-offset-4 hover:underline"
          >
            {t("nav.imports")}
          </Link>
          <h1 className="text-2xl font-semibold">{imp.name}</h1>
        </div>
        {write && (
          // 가져오는 중인지는 서버가 정한다(실행 Task가 끝나지 않았으면 import_busy). 실행을 보내는 동안만 막는다.
          <Button
            variant="outline"
            aria-label={t("imports.deleteNamed", { name: imp.name })}
            disabled={busy}
            onClick={(e) => {
              opener.current = e.currentTarget;
              setDeleteOpen(true);
            }}
          >
            {t("common.delete")}
          </Button>
        )}
      </div>
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <ImportPhaseBadge phase={phase} />
        {phase === "importing" && (
          <span className="text-muted-foreground">{t("imports.importing")}</span>
        )}
        {phase === "done" && (
          <span>
            {t("imports.imported")} <ImportedVm imp={imp} />
          </span>
        )}
      </div>
      {error && (
        <p role="status" className="text-sm text-destructive">
          {t(phase === "failed" ? "imports.failedError" : "imports.lastError", { error })}
        </p>
      )}
      <Card>
        <CardHeader>
          <CardTitle>{t("imports.source")}</CardTitle>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 text-sm">
            <dt className="text-muted-foreground">{t("imports.fileName")}</dt>
            <dd>
              {imp.spec.source.ova.file_name} ({formatBytes(imp.spec.source.ova.size_bytes)}),{" "}
              {node}
            </dd>
            {src && (
              <>
                <dt className="text-muted-foreground">{t("imports.sourceVm")}</dt>
                <dd>{src.name}</dd>
                <dt className="text-muted-foreground">{t("imports.guestOs")}</dt>
                <dd>{src.guest_os}</dd>
                <dt className="text-muted-foreground">{t("vms.cpu")}</dt>
                <dd>{t("imports.cpuValue", { vcpus: src.vcpus, cores: src.cores_per_socket })}</dd>
                <dt className="text-muted-foreground">{t("vms.memory")}</dt>
                <dd>{formatMib(src.memory_mib)}</dd>
                <dt className="text-muted-foreground">{t("vms.firmware")}</dt>
                <dd>{src.firmware}</dd>
              </>
            )}
          </dl>
          {src && (
            <>
              <Table aria-label={t("vms.disks")}>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("common.name")}</TableHead>
                    <TableHead>{t("imports.diskFile")}</TableHead>
                    <TableHead>{t("imports.capacity")}</TableHead>
                    <TableHead>{t("imports.controller")}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {src.disks.map((d) => (
                    <TableRow key={d.name}>
                      <TableCell>{d.name}</TableCell>
                      <TableCell>{d.file}</TableCell>
                      <TableCell>{formatBytes(d.capacity_bytes)}</TableCell>
                      <TableCell>{d.controller}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
              <Table aria-label={t("vms.nics")}>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("vms.network")}</TableHead>
                    <TableHead>{t("imports.adapter")}</TableHead>
                    <TableHead>{t("vms.mac")}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {src.nics.map((n, i) => (
                    <TableRow key={i}>
                      <TableCell>{n.network}</TableCell>
                      <TableCell>{n.adapter}</TableCell>
                      <TableCell>{n.mac ?? "—"}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </>
          )}
          {warnings.length > 0 && (
            <section aria-labelledby="import-warnings" className="flex flex-col gap-1">
              <h2 id="import-warnings" className="text-sm font-medium">
                {t("imports.warnings")}
              </h2>
              <ul className="list-disc pl-5 text-sm text-muted-foreground">
                {warnings.map((w) => (
                  <li key={w}>{w}</li>
                ))}
              </ul>
            </section>
          )}
        </CardContent>
      </Card>
      {phase === "ready" && write && src && (
        <RunSection
          imp={imp}
          node={node}
          form={form}
          setForm={setForm}
          busy={busy}
          setSent={(sent) => setSentAt(sent ? imp.revision : null)}
        />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("imports.deleteTitle", { name: imp.name })}
        description={t("imports.deleteDescription", { node })}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          try {
            // `force` 없이 지우면 응답은 늘 Task다(200 `{ message }`는 force만).
            const task = (await call(
              api.DELETE("/vm-imports/{key}", { params: { path: { key: imp.id } } }),
            )) as Task;
            trackTask(task, t("tasks.op.vm-import-delete", { target: imp.name }));
          } catch (err) {
            // 원본 노드가 없으면 웹에서는 지울 수 없다. 리소스만 지우는 방법을 알린다.
            if (err instanceof ApiError && err.code === "node_offline") {
              throw new Error(
                t("imports.forceHint", { error: importError(t, err), name: imp.name }),
                { cause: err },
              );
            }
            throw err;
          }
          void navigate("/imports");
        }}
      />
    </div>
  );
}

/**
 * 제안으로 채운 실행 폼. 보내면 가져오기가 `importing`이 되어 폼이 사라진다. 폼 값과 보내는 중인지는
 * `ImportView`가 들고 있다.
 */
function RunSection({
  imp,
  node,
  form,
  setForm,
  busy,
  setSent,
}: {
  imp: VmImport;
  node: string;
  form: RunForm;
  setForm: React.Dispatch<React.SetStateAction<RunForm>>;
  busy: boolean;
  setSent: (sent: boolean) => void;
}) {
  const { t } = useTranslation();
  const vms = useList("vm");
  const volumes = useList("volume");
  const pools = useList("storage-pool");
  const networks = useList("network");
  const nodes = useList("node");
  const [errors, setErrors] = useState<Record<string, string>>({});
  // `name_taken`의 서버 메시지(VM과 만들 볼륨 가운데 어느 이름인지)
  const [taken, setTaken] = useState("");
  const [failure, setFailure] = useState<string | null>(null);
  // Task는 OVA가 있는 노드에서 돈다. 그 노드에서 준비된 풀만 고른다.
  const usable = poolsUsableOn(pools.data ?? [], nodes.data ?? [], imp.spec.node);
  const networkNames = [USER_NETWORK, ...(networks.data ?? []).map((n) => n.name)];
  const set = (patch: Partial<RunForm>) => setForm((f) => ({ ...f, ...patch }));

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const found = validateRun(
      form,
      imp,
      {
        vms: (vms.data ?? []).map((v) => v.name),
        volumes: (volumes.data ?? []).map((v) => v.name),
      },
      networkNames,
    );
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    // 성공하면 가져오기가 `importing`으로 바뀔 때까지 막아 두어 두 번 보내지 않는다. 실패할 때만 푼다.
    setSent(true);
    setFailure(null);
    try {
      const task = await call(
        api.POST("/vm-imports/{key}/run", {
          params: { path: { key: imp.id } },
          body: toRun(form),
        }),
      );
      trackTask(task, t("tasks.op.vm-import", { target: form.name }));
    } catch (err) {
      setSent(false);
      // 일반 번역은 업로드의 "접속한 노드"를 말한다. 가져오기는 OVA가 있는 노드에서 돈다.
      if (err instanceof ApiError && err.code === "pool_not_on_node") {
        setErrors({ pool: "imports.errors.poolNotOnNode" });
      } else if (err instanceof ApiError && err.code === "pool_unavailable") {
        setErrors({ pool: "imports.errors.poolUnavailable" });
      } else if (err instanceof ApiError && err.code === "name_taken") {
        // VM이나 만들 볼륨의 이름이다. 서버가 어느 것인지 말한다.
        setTaken(err.message);
        setErrors({ name: "imports.errors.nameTaken" });
      } else {
        setFailure(importError(t, err));
      }
    }
  }

  const select = (
    id: string,
    label: string,
    value: string,
    options: readonly string[],
    onChange: (value: string) => void,
  ) => (
    <Field id={id} label={label}>
      <NativeSelect
        id={id}
        className="w-full"
        value={value}
        onChange={(e) => onChange(e.target.value)}
      >
        {options.map((o) => (
          <NativeSelectOption key={o} value={o}>
            {o}
          </NativeSelectOption>
        ))}
      </NativeSelect>
    </Field>
  );
  const number = (id: string, label: string, field: "sockets" | "cores" | "memory") => (
    <Field id={id} label={label} error={errors[field]}>
      {(a11y) => (
        <Input
          id={id}
          inputMode="numeric"
          value={form[field]}
          onChange={(e) => set({ [field]: e.target.value })}
          {...a11y}
        />
      )}
    </Field>
  );

  return (
    <form onSubmit={(e) => void submit(e)} className="flex flex-col gap-4" noValidate>
      <fieldset disabled={busy} className="flex min-w-0 flex-col gap-4">
        <Card>
          <CardHeader>
            <CardTitle>{t("imports.settings")}</CardTitle>
            <CardDescription>{t("imports.settingsHint")}</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4 sm:grid-cols-2">
            <Field
              id="run-name"
              label={t("imports.vmName")}
              error={errors.name}
              errorValues={{ message: taken }}
            >
              {(a11y) => (
                <Input
                  id="run-name"
                  value={form.name}
                  onChange={(e) => set({ name: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            <Field
              id="run-pool"
              label={t("imports.pool")}
              error={errors.pool}
              hint={t("imports.poolHint", { node })}
            >
              {(a11y) => (
                <NativeSelect
                  id="run-pool"
                  className="w-full"
                  value={form.pool}
                  onChange={(e) => set({ pool: e.target.value })}
                  {...a11y}
                >
                  <NativeSelectOption value="">—</NativeSelectOption>
                  {usable.map((p) => (
                    <NativeSelectOption key={p.id} value={p.name}>
                      {p.name}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              )}
            </Field>
            {!pools.isSuccess && <ListPending error={pools.error} />}
            {number("run-sockets", t("imports.sockets"), "sockets")}
            {number("run-cores", t("imports.cores"), "cores")}
            {number("run-memory", t("vmForm.memory"), "memory")}
            {select("run-firmware", t("vms.firmware"), form.firmware, ["uefi", "bios"], (v) =>
              set({ firmware: v as RunForm["firmware"] }),
            )}
            {select(
              "run-disk-bus",
              t("imports.diskBus"),
              form.diskBus,
              ["virtio-blk", "sata"],
              (v) => set({ diskBus: v as RunForm["diskBus"] }),
            )}
            {select(
              "run-nic-model",
              t("imports.nicModel"),
              form.nicModel,
              ["virtio", "e1000e"],
              (v) => set({ nicModel: v as RunForm["nicModel"] }),
            )}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>{t("imports.networks")}</CardTitle>
            <CardDescription>{t("imports.networksHint")}</CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-4">
            {!networks.isSuccess && <ListPending error={networks.error} />}
            <div className="grid gap-4 sm:grid-cols-2">
              {sourceNetworks(imp).map((source, i) => (
                <Field
                  key={source}
                  id={`run-network-${i}`}
                  label={source}
                  error={errors[`networks.${source}`]}
                >
                  {(a11y) => (
                    <NativeSelect
                      id={`run-network-${i}`}
                      className="w-full"
                      value={form.networks[source] ?? ""}
                      onChange={(e) =>
                        set({ networks: { ...form.networks, [source]: e.target.value } })
                      }
                      {...a11y}
                    >
                      <NativeSelectOption value="">{t("imports.chooseNetwork")}</NativeSelectOption>
                      <NativeSelectOption value={USER_NETWORK}>
                        {t("vmForm.userNetwork")}
                      </NativeSelectOption>
                      {(networks.data ?? []).map((n) => (
                        <NativeSelectOption key={n.id} value={n.name}>
                          {n.name}
                        </NativeSelectOption>
                      ))}
                    </NativeSelect>
                  )}
                </Field>
              ))}
            </div>
            <div className="flex flex-col gap-1">
              <div className="flex items-center gap-2">
                <Switch
                  id="run-keep-macs"
                  aria-describedby="run-keep-macs-hint"
                  checked={form.keepMacs}
                  onCheckedChange={(keepMacs) => set({ keepMacs })}
                />
                <Label htmlFor="run-keep-macs">{t("imports.keepMacs")}</Label>
              </div>
              <p id="run-keep-macs-hint" className="text-xs text-muted-foreground">
                {t("imports.keepMacsHint")}
              </p>
            </div>
          </CardContent>
        </Card>
        <div className="flex items-center gap-3">
          <Button type="submit">{t("imports.run")}</Button>
          {failure && (
            <p role="alert" className="text-sm text-destructive">
              {failure}
            </p>
          )}
        </div>
      </fieldset>
    </form>
  );
}
