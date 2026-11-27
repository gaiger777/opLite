import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import { useOptionalPrincipal } from "@/auth/AuthProvider";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { isoPools } from "@/isos/isos";
import { formatBytes } from "@/lib/format";
import {
  chunkedSupported,
  type Fingerprint,
  fingerprintOf,
  loadServerUploads,
  resumableFor,
  sameTarget,
  startChunked,
  type UploadItem,
  useUploads,
} from "@/storage/chunked";
import { choosablePoolsOn, poolFormats } from "@/storage/pools";
import { nameFromFile, sourceFormatOf, startUpload } from "@/storage/upload";
import { UploadProgress } from "@/storage/UploadProgress";
import { setCenterOpen } from "@/tasks/center";
import { trackTask } from "@/tasks/tasks";
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
  // 조각 업로드(계획 77-8): 접속한 노드가 알면 쓰고, 보낸 뒤 이 창이 진행을 보인다(닫아도 계속 — 작업·알림 센터에도 있음).
  const chunked = chunkedSupported(nodes ?? undefined, serving?.id);
  const uploads = useUploads();
  const [started, setStarted] = useState<string | null>(null);
  const item = started ? uploads.find((x) => x.key === started) : undefined;
  useEffect(() => {
    if (chunked) {
      void loadServerUploads();
    }
  }, [chunked]);
  // 고른 파일의 지문(이름·고친 시각·앞 1 MiB의 SHA-256 — 이어 올릴 업로드를 찾는다)
  const [fingerprint, setFingerprint] = useState<{ file: File; value: Fingerprint } | null>(null);
  const fp = fingerprint && fingerprint.file === file ? fingerprint.value : null;
  // 고른 파일을 전에 올리다 멈췄으면 그 업로드를 잇는다. 이름·풀·형식을 바꾸면 그 업로드를 버리고 새로 올린다.
  const resumable = chunked && file && fp ? resumableFor(file, fp) : undefined;
  const params = iso
    ? ({ name, pool, format: "raw", sourceFormat: "raw", content: "iso" } as const)
    : { name, pool, format, sourceFormat };
  const resuming = resumable !== undefined && sameTarget(resumable, params);
  const onTask = (task: Parameters<typeof trackTask>[0], up: UploadItem) =>
    trackTask(task, t("tasks.op.volume-import", { target: up.name }));
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
    if (!chunked || !chosen) {
      return;
    }
    // 전에 올리다 멈춘 파일이면 그 이름·풀·형식으로 채운다(지문을 센 뒤 — 이 사이에 다른 파일을 골랐으면 버린다).
    void fingerprintOf(chosen).then((value) => {
      setFingerprint({ file: chosen, value });
      const before = resumableFor(chosen, value);
      if (before) {
        setName(before.name);
        setPool(before.pool);
        setSourceFormat(before.sourceFormat);
        if (before.format) {
          setFormat(before.format);
        }
      }
    });
  }

  async function submit() {
    const found: Record<string, string> = {};
    if (!file) {
      found.file = "uploads.errors.file";
    }
    if (file && resuming) {
      setStarted(startChunked(file, params, onTask, fp!));
      return false;
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
    if (chunked) {
      // 이 창이 진행을 보인다(닫아도 계속).
      setStarted(startChunked(file!, params, onTask, fp ?? (await fingerprintOf(file!))));
      return false;
    }
    // 옛 한 번 올리기(섞인 판): 올리는 동안 대화상자를 닫아 둔다. 진행률은 알림에 보인다.
    startUpload(file!, params, t);
  }

  if (started) {
    return (
      <Dialog open onOpenChange={(open) => !open && onClose()}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{iso ? t("uploads.isoTitle") : t("uploads.title")}</DialogTitle>
            <DialogDescription>{t("uploads.keepsGoing")}</DialogDescription>
          </DialogHeader>
          {item ? <UploadProgress item={item} onTask={onTask} /> : null}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => {
                onClose();
                setCenterOpen(true);
              }}
            >
              {t("uploads.showInCenter")}
            </Button>
            <Button type="button" onClick={onClose}>
              {t("uploads.close")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    );
  }

  return (
    <FormDialog
      title={iso ? t("uploads.isoTitle") : t("uploads.title")}
      description={t(iso ? "uploads.isoDescription" : "uploads.description", { node: here })}
      submitLabel={resuming ? t("uploads.resume") : t("uploads.submit")}
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
        help={
          resumable
            ? t(resuming ? "uploads.resumeNote" : "uploads.restartNote", {
                sent: formatBytes(resumable.sent),
                total: formatBytes(resumable.size),
              })
            : file
              ? formatBytes(file.size)
              : t("help.uploads.file")
        }
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
      <Field
        id="upload-name"
        label={t("common.name")}
        error={errors.name}
        help={t("help.uploads.name")}
      >
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
            : t("help.uploads.pool")
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
          <Field
            id="upload-source"
            label={t("uploads.sourceFormat")}
            help={t("help.uploads.sourceFormat")}
          >
            {(a11y) => (
              <NativeSelect
                id="upload-source"
                className="w-full"
                value={sourceFormat}
                onChange={(e) => setSourceFormat(e.target.value as typeof sourceFormat)}
                {...a11y}
              >
                <NativeSelectOption value="vmdk">vmdk</NativeSelectOption>
                <NativeSelectOption value="qcow2">qcow2</NativeSelectOption>
                <NativeSelectOption value="raw">raw</NativeSelectOption>
              </NativeSelect>
            )}
          </Field>
          <Field
            id="upload-format"
            label={t("volumes.format")}
            help={t("help.volumes.format")}
            info="volumes.format"
          >
            {(a11y) => (
              <NativeSelect
                id="upload-format"
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
