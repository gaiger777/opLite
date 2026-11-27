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
import { isoPools, nameFromUrl, sha256Error, urlError } from "@/isos/isos";
import { choosablePools, poolFormats } from "@/storage/pools";
import { usePoolNodes } from "@/storage/usePoolNodes";
import { volumeNameError } from "@/storage/volumes";
import { trackTask } from "@/tasks/tasks";

type Schemas = components["schemas"];

/**
 * URL에서 이미지를 받아 새 볼륨을 만든다(`POST /volumes/download`, Task `volume-download`). 풀을 쓸 수 있는 노드가 받으므로
 * 그 노드가 URL에 닿아야 한다. `content: "iso"`면 ISO 이미지(디렉터리·NFS 풀, raw), 아니면 디스크 이미지(원본 형식·형식을
 * 고른다 — 클라우드 이미지는 보통 qcow2).
 */
export function DownloadDialog({
  onClose,
  content,
}: {
  onClose(): void;
  content: Schemas["VolumeContent"];
}) {
  const { t } = useTranslation();
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const nodes = usePoolNodes();
  const iso = content === "iso";
  // 노드를 읽지 못하면 거르지 않는다(서버가 막는다).
  const somewhere = choosablePools(pools.data ?? [], nodes);
  const usable = iso ? isoPools(somewhere) : somewhere;
  const [url, setUrl] = useState("");
  const [name, setName] = useState("");
  // 이름을 직접 고치기 전에는 URL에서 이름을 제안한다.
  const [named, setNamed] = useState(false);
  const [pool, setPool] = useState("");
  const [sha256, setSha256] = useState("");
  const [sourceFormat, setSourceFormat] = useState<Schemas["SourceFormat"]>("qcow2");
  const [format, setFormat] = useState<Schemas["ImageFormat"]>("qcow2");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const formats = poolFormats(usable.find((p) => p.name === pool));
  const choosePool = (chosen: string) => {
    setPool(chosen);
    setFormat(poolFormats(usable.find((p) => p.name === chosen))[0]);
  };

  async function submit() {
    const found: Record<string, string> = {};
    const badUrl = urlError(url.trim());
    if (badUrl) {
      found.url = badUrl;
    }
    const nameError = volumeNameError(name, volumes.data?.map((v) => v.name) ?? []);
    if (nameError) {
      found.name = nameError;
    }
    if (!pool) {
      found.pool = "vmForm.errors.pool";
    }
    const badSum = sha256Error(sha256.trim());
    if (badSum) {
      found.sha256 = badSum;
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const task = await call(
      api.POST("/volumes/download", {
        body: {
          name,
          pool,
          url: url.trim(),
          ...(sha256.trim() !== "" && { sha256: sha256.trim().toLowerCase() }),
          ...(iso ? { content: "iso" as const } : { source_format: sourceFormat, format }),
        },
      }),
    );
    trackTask(task, t("tasks.op.volume-download", { target: name }));
  }

  return (
    <FormDialog
      title={iso ? t("downloads.isoTitle") : t("downloads.title")}
      description={t("downloads.description")}
      submitLabel={t("downloads.submit")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field
        id="download-url"
        label={t("downloads.url")}
        error={errors.url}
        help={t("help.downloads.url")}
      >
        {(a11y) => (
          <Input
            id="download-url"
            type="url"
            value={url}
            onChange={(e) => {
              setUrl(e.target.value);
              if (!named) {
                setName(nameFromUrl(e.target.value));
              }
            }}
            {...a11y}
          />
        )}
      </Field>
      <Field
        id="download-name"
        label={t("common.name")}
        error={errors.name}
        help={t("help.downloads.name")}
      >
        {(a11y) => (
          <Input
            id="download-name"
            value={name}
            onChange={(e) => {
              setName(e.target.value);
              setNamed(true);
            }}
            {...a11y}
          />
        )}
      </Field>
      {!pools.isSuccess && <ListPending error={pools.error} />}
      <Field
        id="download-pool"
        label={t("volumes.pool")}
        error={errors.pool}
        help={
          usable.length === 0 && pools.isSuccess && iso
            ? t("uploads.noIsoPool")
            : t("help.downloads.pool")
        }
      >
        {(a11y) => (
          <NativeSelect
            id="download-pool"
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
      <Field
        id="download-sha256"
        label={t("downloads.sha256")}
        error={errors.sha256}
        help={t("downloads.sha256Hint")}
      >
        {(a11y) => (
          <Input
            id="download-sha256"
            className="font-mono"
            value={sha256}
            onChange={(e) => setSha256(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      {!iso && (
        <div className="grid grid-cols-2 gap-4">
          <Field
            id="download-source"
            label={t("uploads.sourceFormat")}
            help={t("help.downloads.sourceFormat")}
          >
            {(a11y) => (
              <NativeSelect
                id="download-source"
                className="w-full"
                value={sourceFormat}
                onChange={(e) => setSourceFormat(e.target.value as typeof sourceFormat)}
                {...a11y}
              >
                <NativeSelectOption value="qcow2">qcow2</NativeSelectOption>
                <NativeSelectOption value="raw">raw</NativeSelectOption>
                <NativeSelectOption value="vmdk">vmdk</NativeSelectOption>
              </NativeSelect>
            )}
          </Field>
          <Field
            id="download-format"
            label={t("volumes.format")}
            help={t("help.volumes.format")}
            info="volumes.format"
          >
            {(a11y) => (
              <NativeSelect
                id="download-format"
                className="w-full"
                value={format}
                onChange={(e) => setFormat(e.target.value as typeof format)}
                {...a11y}
              >
                {formats.map((f) => (
                  <NativeSelectOption key={f} value={f}>
                    {f}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
        </div>
      )}
    </FormDialog>
  );
}
