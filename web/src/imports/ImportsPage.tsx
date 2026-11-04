import { useQuery } from "@tanstack/react-query";
import { Server, Upload } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import type { VmImport } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { EsxiImportDialog } from "@/imports/EsxiImportDialog";
import { CREDENTIALS_LOST, importNameError, sourceText } from "@/imports/imports";
import { startOvaUpload } from "@/imports/ova";
import { formatBytes } from "@/lib/format";
import { nameFromFile } from "@/storage/upload";

type ImportPhase = components["schemas"]["ImportPhase"];

const PHASE_VARIANT = {
  ready: "default",
  importing: "secondary",
  done: "outline",
  failed: "destructive",
} as const;

/** 가져오기 상태 */
export function ImportPhaseBadge({ phase }: { phase: ImportPhase }) {
  const { t } = useTranslation();
  return <Badge variant={PHASE_VARIANT[phase]}>{t(`imports.phase.${phase}`)}</Badge>;
}

/** 가져오기의 오류. ESXi 자격 증명이 사라진 오류는 무엇을 할지 알려 주는 문장으로 바꾼다. */
export function useImportError(imp: VmImport): string | null | undefined {
  const { t } = useTranslation();
  const error = imp.status.error;
  return error === CREDENTIALS_LOST ? t("imports.credentialsLost") : error;
}

/** 목록의 상태 아래 오류 */
function ImportError({ imp }: { imp: VmImport }) {
  const error = useImportError(imp);
  return error ? (
    <div className="max-w-xs text-xs whitespace-normal break-words text-destructive">{error}</div>
  ) : null;
}

/** 만든 VM으로 가는 링크. 만들지 않았으면 "—", VM을 지웠으면 그렇다고 보인다. */
export function ImportedVm({ imp }: { imp: VmImport }) {
  const { t } = useTranslation();
  const vms = useList("vm");
  const id = imp.status.vm;
  if (!id) {
    return <>—</>;
  }
  const vm = vms.data?.find((v) => v.id === id);
  if (!vm) {
    return <>{vms.isSuccess ? t("imports.vmGone") : "—"}</>;
  }
  return (
    <Link to={`/vms/${encodeURIComponent(vm.name)}`} className="underline-offset-4 hover:underline">
      {vm.name}
    </Link>
  );
}

/** VM 가져오기 목록과 OVA 올리기. 항목을 열면 원본을 보고 가져오기를 실행한다. */
export function ImportsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const imports = useList("vm-import");
  const nodes = useList("node");
  const [uploading, setUploading] = useState(false);
  const [esxi, setEsxi] = useState(false);
  const write = can(me.role, "write", "vm-import");
  const nodeName = (id: string) => nodes.data?.find((n) => n.id === id)?.name ?? "—";

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-col gap-1">
          <h1 id="imports-title" className="text-2xl font-semibold">
            {t("nav.imports")}
          </h1>
          <p className="text-sm text-muted-foreground">{t("imports.pageHint")}</p>
        </div>
        {write && (
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" onClick={() => setEsxi(true)}>
              <Server />
              {t("imports.esxi")}
            </Button>
            <Button onClick={() => setUploading(true)}>
              <Upload />
              {t("imports.upload")}
            </Button>
          </div>
        )}
      </div>
      <Table aria-labelledby="imports-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("common.node")}</TableHead>
            <TableHead>{t("imports.sourceVm")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("imports.vm")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {(imports.data ?? []).length === 0 && (
            <TableMessage colSpan={5} query={imports} empty={t("imports.empty")} />
          )}
          {(imports.data ?? []).map((imp) => (
            <TableRow key={imp.id}>
              <TableCell className="font-medium">
                <Link
                  to={`/imports/${encodeURIComponent(imp.name)}`}
                  className="underline-offset-4 hover:underline"
                >
                  {imp.name}
                </Link>
              </TableCell>
              <TableCell>{nodeName(imp.spec.node)}</TableCell>
              <TableCell>
                {imp.status.source_vm?.name ?? "—"}
                <div className="text-xs text-muted-foreground">{sourceText(imp)}</div>
              </TableCell>
              <TableCell>
                <ImportPhaseBadge phase={imp.status.phase} />
                <ImportError imp={imp} />
              </TableCell>
              <TableCell>
                <ImportedVm imp={imp} />
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {uploading && <OvaUploadDialog onClose={() => setUploading(false)} />}
      {esxi && <EsxiImportDialog onClose={() => setEsxi(false)} />}
    </div>
  );
}

/** OVA를 고르고 가져오기 이름을 정한다. 이름은 파일 이름에서 제안한다. */
function OvaUploadDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const imports = useList("vm-import");
  const nodes = useList("node");
  // 파일은 접속한 노드로 간다(이미지 올리기와 같은 질의).
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const here = nodes.data?.find((n) => n.id === cluster.data?.node)?.name ?? "—";
  const [file, setFile] = useState<File | null>(null);
  const [name, setName] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  function choose(chosen: File | null) {
    setFile(chosen);
    if (chosen) {
      setName(nameFromFile(chosen.name));
    }
  }

  async function submit() {
    const found: Record<string, string> = {};
    if (!file) {
      found.file = "uploads.errors.file";
    }
    const nameError = importNameError(name, imports.data?.map((i) => i.name) ?? []);
    if (nameError) {
      found.name = nameError;
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    // 올리는 동안 대화상자를 닫아 둔다. 진행률은 알림에 보인다.
    startOvaUpload(file!, name, t, (imp) => void navigate(`/imports/${encodeURIComponent(imp)}`));
  }

  return (
    <FormDialog
      title={t("imports.uploadTitle")}
      description={t("imports.uploadDescription", { node: here })}
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
        id="ova-file"
        label={t("imports.file")}
        error={errors.file}
        hint={file ? formatBytes(file.size) : undefined}
      >
        {(a11y) => (
          <Input
            id="ova-file"
            type="file"
            accept=".ova"
            onChange={(e) => choose(e.target.files?.[0] ?? null)}
            {...a11y}
          />
        )}
      </Field>
      <Field id="ova-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input id="ova-name" value={name} onChange={(e) => setName(e.target.value)} {...a11y} />
        )}
      </Field>
    </FormDialog>
  );
}
