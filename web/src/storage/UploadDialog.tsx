import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { errorMessage } from "@/i18n/errors";
import { formatBytes } from "@/lib/format";
import { poolFormats, poolsUsableOn } from "@/storage/pools";
import { nameFromFile, sourceFormatOf, startUpload } from "@/storage/upload";
import { volumeNameError } from "@/storage/volumes";

type Schemas = components["schemas"];

export function UploadDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const nodes = useList("node");
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  // 서버는 받은 노드에서 쓸 수 있는(준비된) 풀로만 가져온다.
  const usable = poolsUsableOn(pools.data ?? [], nodes.data ?? [], cluster.data?.node);
  const here = nodes.data?.find((n) => n.id === cluster.data?.node)?.name ?? "—";
  const [file, setFile] = useState<File | null>(null);
  const [name, setName] = useState("");
  const [pool, setPool] = useState("");
  const [sourceFormat, setSourceFormat] = useState<Schemas["SourceFormat"]>("vmdk");
  const [format, setFormat] = useState<Schemas["ImageFormat"]>("qcow2");
  const [errors, setErrors] = useState<Record<string, string>>({});
  // 형식은 풀 종류를 따른다(LVM-thin 풀은 raw만). 풀을 바꾸면 그 풀의 기본 형식으로 돌린다.
  const formats = poolFormats(usable.find((p) => p.name === pool));
  const choosePool = (chosen: string) => {
    setPool(chosen);
    setFormat(poolFormats(usable.find((p) => p.name === chosen))[0]);
  };

  function choose(chosen: File | null) {
    setFile(chosen);
    if (chosen) {
      setName(nameFromFile(chosen.name));
      setSourceFormat(sourceFormatOf(chosen.name));
    }
  }

  async function submit() {
    const found: Record<string, string> = {};
    if (!file) {
      found.file = "uploads.errors.file";
    }
    const nameError = volumeNameError(name, volumes.data?.map((v) => v.name) ?? []);
    if (nameError) {
      found.name = nameError;
    }
    if (!pool) {
      found.pool = "vmForm.errors.pool";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    // 올리는 동안 대화상자를 닫아 둔다. 진행률은 알림에 보인다.
    startUpload(file!, { name, pool, format, sourceFormat }, t);
  }

  return (
    <FormDialog
      title={t("uploads.title")}
      description={t("uploads.description", { node: here })}
      submitLabel={t("uploads.submit")}
      onClose={onClose}
      onSubmit={submit}
    >
      {cluster.isError && (
        <p role="alert" className="text-sm text-destructive">
          {t("uploads.noNode", { error: errorMessage(t, cluster.error) })}
        </p>
      )}
      <Field
        id="upload-file"
        label={t("uploads.file")}
        error={errors.file}
        hint={file ? formatBytes(file.size) : undefined}
      >
        {(a11y) => (
          <Input
            id="upload-file"
            type="file"
            onChange={(e) => choose(e.target.files?.[0] ?? null)}
            {...a11y}
          />
        )}
      </Field>
      <Field id="upload-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="upload-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      {!pools.isSuccess && <ListPending error={pools.error} />}
      <Field
        id="upload-pool"
        label={t("volumes.pool")}
        error={errors.pool}
        hint={
          usable.length === 0 && cluster.data && pools.isSuccess ? t("uploads.noPool") : undefined
        }
      >
        {(a11y) => (
          <NativeSelect
            id="upload-pool"
            className="w-full"
            value={pool}
            onChange={(e) => choosePool(e.target.value)}
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
      <div className="grid grid-cols-2 gap-4">
        <Field id="upload-source" label={t("uploads.sourceFormat")}>
          <NativeSelect
            id="upload-source"
            className="w-full"
            value={sourceFormat}
            onChange={(e) => setSourceFormat(e.target.value as typeof sourceFormat)}
          >
            <NativeSelectOption value="vmdk">vmdk</NativeSelectOption>
            <NativeSelectOption value="qcow2">qcow2</NativeSelectOption>
            <NativeSelectOption value="raw">raw</NativeSelectOption>
          </NativeSelect>
        </Field>
        <Field id="upload-format" label={t("volumes.format")}>
          <NativeSelect
            id="upload-format"
            className="w-full"
            value={format}
            onChange={(e) => setFormat(e.target.value as typeof format)}
          >
            {formats.map((f) => (
              <NativeSelectOption key={f} value={f}>
                {f}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
      </div>
    </FormDialog>
  );
}
