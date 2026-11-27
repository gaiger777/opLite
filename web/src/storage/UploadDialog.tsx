import { useState } from "react";
import { useTranslation } from "react-i18next";

import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import { useOptionalPrincipal } from "@/auth/AuthProvider";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { isoPools } from "@/isos/isos";
import { formatBytes } from "@/lib/format";
import { choosablePoolsOn, poolFormats } from "@/storage/pools";
import { nameFromFile, sourceFormatOf, startUpload } from "@/storage/upload";
import { usePoolNodes } from "@/storage/usePoolNodes";
import { volumeNameError } from "@/storage/volumes";

type Schemas = components["schemas"];

/** 이미지 올리기. `content: "iso"`면 ISO 이미지를 디렉터리·NFS 풀에 raw로 올린다(형식을 고르지 않는다). */
export function UploadDialog({
  onClose,
  content = "disk",
}: {
  onClose(): void;
  content?: Schemas["VolumeContent"];
}) {
  const { t } = useTranslation();
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const nodes = usePoolNodes();
  // 접속한 노드는 whoami가 알린다(노드를 읽지 못하면 없다).
  const serving = useOptionalPrincipal()?.node;
  // 서버는 받은 노드에서 쓸 수 있는(준비된) 풀로만 가져온다.
  const iso = content === "iso";
  const onNode = choosablePoolsOn(pools.data ?? [], nodes, serving?.id);
  const usable = iso ? isoPools(onNode) : onNode;
  const here = serving?.name ?? "—";
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
    startUpload(
      file!,
      iso
        ? { name, pool, format: "raw", sourceFormat: "raw", content: "iso" }
        : { name, pool, format, sourceFormat },
      t,
    );
  }

  return (
    <FormDialog
      title={iso ? t("uploads.isoTitle") : t("uploads.title")}
      description={t(iso ? "uploads.isoDescription" : "uploads.description", { node: here })}
      submitLabel={t("uploads.submit")}
      onClose={onClose}
      onSubmit={submit}
    >
      {!serving && (
        <p role="alert" className="text-sm text-destructive">
          {t("uploads.unknownNode")}
        </p>
      )}
      <Field
        id="upload-file"
        label={t("uploads.file")}
        error={errors.file}
        help={file ? formatBytes(file.size) : undefined}
      >
        {(a11y) => (
          <Input
            id="upload-file"
            type="file"
            {...(iso && { accept: ".iso" })}
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
        help={
          usable.length === 0 && serving && pools.isSuccess
            ? t(iso ? "uploads.noIsoPool" : "uploads.noPool")
            : undefined
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
      {!iso && (
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
      )}
    </FormDialog>
  );
}
