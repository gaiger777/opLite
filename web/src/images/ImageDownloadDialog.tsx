import { useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { type CatalogEntry, defaultVolumeName } from "@/images/images";
import { formatBytes } from "@/lib/format";
import { choosablePools } from "@/storage/pools";
import { usePoolNodes } from "@/storage/usePoolNodes";
import { volumeNameError } from "@/storage/volumes";
import { trackTask } from "@/tasks/tasks";
import { cloudInitErrors, emptyCloudInit, toCloudInit, validateKeys } from "@/vms/cloudInit";
import { gibToBytes, MAX_VCPUS, MIN_MEMORY_MIB, validName } from "@/vms/form";

type CatalogTemplate = components["schemas"]["CatalogTemplate"];

const MIB = 1024 ** 2;

const TEXTAREA =
  "min-h-16 w-full rounded-lg border border-input bg-transparent px-2.5 py-1.5 font-mono text-xs outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 aria-invalid:border-destructive";

function intIn(value: string, min: number, max: number): boolean {
  const n = Number(value);
  return value.trim() !== "" && Number.isInteger(n) && n >= min && n <= max;
}

/**
 * 카탈로그 이미지를 풀에 받는다. "템플릿 만들기"를 켜면 받은 뒤 디스크를 늘리고 꺼진 템플릿 VM(NIC 하나, cloud-init 사용자·
 * 비밀번호·SSH 키)을 만든다 — 그 단계가 실패해도 받은 볼륨은 남는다. 받는 노드가 이미지 URL과 체크섬 파일에 닿아야 한다.
 */
export function ImageDownloadDialog({ entry, onClose }: { entry: CatalogEntry; onClose(): void }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const pools = useList("storage-pool");
  const nodes = usePoolNodes();
  const volumes = useList("volume");
  const vms = useList("vm");
  const networks = useList("network");
  // 노드를 읽지 못하면 거르지 않는다(서버가 막는다).
  const usable = choosablePools(pools.data ?? [], nodes);
  // 템플릿 VM은 폴더 없이 루트에 만든다: 루트 vm.create가 있어야 한다(설계 §18).
  const canTemplate = perms.has("vm.create");
  const suggested = defaultVolumeName(entry);
  const [pool, setPool] = useState("");
  const [name, setName] = useState("");
  const [template, setTemplate] = useState(false);
  const [tmplName, setTmplName] = useState(`${suggested}-tmpl`);
  const [network, setNetwork] = useState("");
  const [cpus, setCpus] = useState("2");
  const [memory, setMemory] = useState("2048");
  const [sizeGib, setSizeGib] = useState("");
  const [user, setUser] = useState("");
  const [password, setPassword] = useState("");
  const [sshKeys, setSshKeys] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    if (!pool) {
      found.pool = "vmForm.errors.pool";
    }
    const volumeName = name.trim() === "" ? suggested : name.trim();
    const nameError = volumeNameError(volumeName, volumes.data?.map((v) => v.name) ?? []);
    if (nameError) {
      found.name = nameError;
    }
    let draft: CatalogTemplate | undefined;
    if (template) {
      if (!validName(tmplName)) {
        found.tmplName = "forms.name";
      } else if ((vms.data ?? []).some((v) => v.name === tmplName)) {
        found.tmplName = "clone.errors.exists";
      }
      if (!network) {
        found.network = "images.errors.network";
      }
      if (!intIn(cpus, 1, MAX_VCPUS)) {
        found.cpus = "templates.errors.vcpus";
      }
      if (!intIn(memory, MIN_MEMORY_MIB, 16 * 1024 * 1024)) {
        found.memory = "templates.errors.memory";
      }
      let size: number | undefined;
      if (sizeGib.trim() !== "") {
        const bytes = gibToBytes(sizeGib.trim());
        size = bytes === undefined ? undefined : Math.ceil(bytes / MIB) * MIB;
        if (size === undefined) {
          found.size = "vmForm.errors.size";
        } else if (entry.min_size_bytes && size < entry.min_size_bytes) {
          found.size = "images.errors.size";
        }
      }
      const ci = { ...emptyCloudInit(), enabled: true, user, password, sshKeys };
      Object.assign(found, cloudInitErrors(ci, 1));
      const cloudInit = toCloudInit(ci, 1);
      draft = {
        name: tmplName,
        network,
        vcpus: Number(cpus),
        memory_mib: Number(memory),
        ...(size !== undefined && { size_bytes: size }),
        ...(cloudInit && Object.keys(cloudInit).length > 0 && { cloud_init: cloudInit }),
      };
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const task = await call(
      api.POST("/image-catalog/{id}/download", {
        params: { path: { id: entry.id } },
        body: {
          pool,
          arch: entry.arch,
          ...(name.trim() !== "" && { name: name.trim() }),
          ...(draft && { template: draft }),
        },
      }),
    );
    trackTask(
      task,
      draft
        ? t("images.downloadTemplateOp", { name: volumeName, template: draft.name })
        : t("tasks.op.volume-download", { target: volumeName }),
    );
  }

  return (
    <FormDialog
      title={t("images.downloadTitle", { name: `${entry.title} (${entry.arch})` })}
      description={t("images.downloadDescription")}
      submitLabel={t("downloads.submit")}
      onClose={onClose}
      onSubmit={submit}
    >
      <div className="-mx-1 flex max-h-[65svh] flex-col gap-4 overflow-y-auto px-1">
        {entry.min_size_bytes ? (
          <p className="text-sm">
            {t("images.imageSize", { size: formatBytes(entry.min_size_bytes) })}
          </p>
        ) : null}
        {!pools.isSuccess && <ListPending error={pools.error} />}
        <Field id="image-pool" label={t("volumes.pool")} error={errors.pool}>
          {(a11y) => (
            <NativeSelect
              id="image-pool"
              className="w-full"
              value={pool}
              onChange={(e) => setPool(e.target.value)}
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
        <Field id="image-name" label={t("images.volumeName")} error={errors.name}>
          {(a11y) => (
            <Input
              id="image-name"
              placeholder={suggested}
              value={name}
              onChange={(e) => setName(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
        {canTemplate && (
          <div className="flex items-center gap-2">
            <Checkbox
              id="image-template"
              checked={template}
              onCheckedChange={(v) => setTemplate(v === true)}
            />
            <Label htmlFor="image-template">{t("images.makeTemplate")}</Label>
          </div>
        )}
        {template && (
          <>
            <Field id="image-tmpl-name" label={t("images.templateName")} error={errors.tmplName}>
              {(a11y) => (
                <Input
                  id="image-tmpl-name"
                  value={tmplName}
                  onChange={(e) => setTmplName(e.target.value)}
                  {...a11y}
                />
              )}
            </Field>
            <Field id="image-network" label={t("vms.network")} error={errors.network}>
              {(a11y) => (
                <NativeSelect
                  id="image-network"
                  className="w-full"
                  value={network}
                  onChange={(e) => setNetwork(e.target.value)}
                  {...a11y}
                >
                  <NativeSelectOption value="">—</NativeSelectOption>
                  {(networks.data ?? []).map((n) => (
                    <NativeSelectOption key={n.id} value={n.name}>
                      {n.name}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              )}
            </Field>
            <div className="grid grid-cols-3 gap-4">
              <Field id="image-cpus" label={t("vms.vcpus")} error={errors.cpus}>
                {(a11y) => (
                  <Input
                    id="image-cpus"
                    inputMode="numeric"
                    value={cpus}
                    onChange={(e) => setCpus(e.target.value)}
                    {...a11y}
                  />
                )}
              </Field>
              <Field id="image-memory" label={t("templates.memoryMib")} error={errors.memory}>
                {(a11y) => (
                  <Input
                    id="image-memory"
                    inputMode="numeric"
                    value={memory}
                    onChange={(e) => setMemory(e.target.value)}
                    {...a11y}
                  />
                )}
              </Field>
              <Field id="image-size" label={t("images.diskSize")} error={errors.size}>
                {(a11y) => (
                  <Input
                    id="image-size"
                    inputMode="decimal"
                    value={sizeGib}
                    onChange={(e) => setSizeGib(e.target.value)}
                    {...a11y}
                  />
                )}
              </Field>
            </div>
            <Field
              id="image-user"
              label={t("vmForm.ciUser")}
              error={errors["ci.user"]}
              hint={t("vmForm.ciUserHint")}
            >
              {(a11y) => (
                <Input
                  id="image-user"
                  placeholder={entry.default_user ?? ""}
                  value={user}
                  onChange={(e) => setUser(e.target.value)}
                  {...a11y}
                />
              )}
            </Field>
            <Field
              id="image-password"
              label={t("vmForm.ciPassword")}
              error={errors["ci.password"]}
              hint={t("vmForm.ciPasswordHint")}
            >
              {(a11y) => (
                <Input
                  id="image-password"
                  type="password"
                  autoComplete="new-password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  {...a11y}
                />
              )}
            </Field>
            <Field
              id="image-keys"
              label={t("vmForm.ciKeys")}
              error={errors["ci.sshKeys"]}
              errorValues={validateKeys(sshKeys)?.values}
              hint={t("vmForm.ciKeysHint")}
            >
              {(a11y) => (
                <textarea
                  id="image-keys"
                  className={TEXTAREA}
                  value={sshKeys}
                  onChange={(e) => setSshKeys(e.target.value)}
                  {...a11y}
                />
              )}
            </Field>
          </>
        )}
      </div>
    </FormDialog>
  );
}
