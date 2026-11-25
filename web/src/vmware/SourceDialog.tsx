import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { validName } from "@/vms/form";
import {
  type CreateSourceRequest,
  localized,
  offeredCertificate,
  refreshVmware,
  type SourcePatch,
  type VmwareSource,
} from "@/vmware/api";

/** 연결 칸 → (처음 보는 인증서면) 지문 확인 */
type Step = "form" | "trust";

interface SourceForm {
  name: string;
  host: string;
  port: string;
  user: string;
  password: string;
  datacenter: string;
  vddk: boolean;
  libdir: string;
}

function formOf(source?: VmwareSource): SourceForm {
  const vddk =
    source && typeof source.spec.transport === "object" ? source.spec.transport.vddk : null;
  return {
    name: source?.name ?? "",
    host: source?.spec.host ?? "",
    port: String(source?.spec.port ?? 443),
    user: source?.spec.user ?? "",
    password: "",
    datacenter: source?.spec.datacenter_hint ?? "",
    vddk: !!vddk,
    libdir: vddk?.libdir ?? "",
  };
}

/** 입력 오류(칸 → 번역 키). 고치기에서 비밀번호를 비우면 그대로 둔다. */
export function checkSourceForm(
  form: SourceForm,
  editing: boolean,
  taken: readonly string[],
): Record<string, string> {
  const errors: Record<string, string> = {};
  if (!editing) {
    if (!validName(form.name)) {
      errors.name = "vmware.source.errors.name";
    } else if (taken.includes(form.name)) {
      errors.name = "vmware.source.errors.nameTaken";
    }
    if (!form.password) {
      errors.password = "vmware.source.errors.password";
    }
  }
  if (!form.host.trim()) {
    errors.host = "vmware.source.errors.host";
  }
  const port = Number(form.port);
  if (!/^\d+$/.test(form.port) || port < 1 || port > 65535) {
    errors.port = "vmware.source.errors.port";
  }
  if (!form.user.trim()) {
    errors.user = "vmware.source.errors.user";
  }
  return errors;
}

/**
 * vCenter·ESXi 연결을 더하거나 고친다. 처음 보는 인증서면 서버가 알려 준 지문(SHA-256, VDDK에 쓰는 SHA-1)을 보이고 믿기로
 * 하면 다시 보낸다(`EsxiImportDialog`와 같은 흐름). 비밀번호는 이 컴포넌트의 상태에만 있고 요청 본문으로만 보낸다(서버는
 * Raft에 두고 보기·watch·감사에서 뺀다).
 */
export function SourceDialog({
  source,
  taken,
  onClose,
}: {
  /** 고칠 연결(없으면 더하기) */
  source?: VmwareSource;
  taken: readonly string[];
  onClose(): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const editing = source !== undefined;
  const [step, setStep] = useState<Step>("form");
  const [form, setForm] = useState<SourceForm>(() => formOf(source));
  const [offered, setOffered] = useState<{ fingerprint: string; thumbprint?: string } | null>(null);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const set = (patch: Partial<SourceForm>) => setForm((f) => ({ ...f, ...patch }));

  function createBody(fingerprint?: string): CreateSourceRequest {
    return {
      name: form.name,
      host: form.host.trim(),
      port: Number(form.port),
      user: form.user.trim(),
      password: form.password,
      ...(form.datacenter.trim() ? { datacenter_hint: form.datacenter.trim() } : {}),
      ...(form.vddk ? { vddk: form.libdir.trim() ? { libdir: form.libdir.trim() } : {} } : {}),
      ...(fingerprint ? { fingerprint } : {}),
    };
  }

  /** 바꾼 칸만(`SourcePatch`) */
  function patchBody(fingerprint?: string): SourcePatch {
    const was = formOf(source);
    const patch: SourcePatch = {};
    if (form.host.trim() !== was.host) {
      patch.host = form.host.trim();
    }
    if (form.port !== was.port) {
      patch.port = Number(form.port);
    }
    if (form.user.trim() !== was.user) {
      patch.user = form.user.trim();
    }
    if (form.password) {
      patch.password = form.password;
    }
    if (form.datacenter.trim() !== was.datacenter) {
      patch.datacenter_hint = form.datacenter.trim() || null;
    }
    if (form.vddk !== was.vddk || (form.vddk && form.libdir.trim() !== was.libdir)) {
      patch.vddk = form.vddk ? (form.libdir.trim() ? { libdir: form.libdir.trim() } : {}) : null;
    }
    if (fingerprint) {
      patch.fingerprint = fingerprint;
    }
    return patch;
  }

  async function send(fingerprint?: string) {
    try {
      if (editing) {
        await call(
          api.PATCH("/vmware-sources/{key}", {
            params: { path: { key: source.id } },
            body: patchBody(fingerprint),
          }),
        );
      } else {
        await call(api.POST("/vmware-sources", { body: createBody(fingerprint) }));
      }
    } catch (err) {
      const offer = offeredCertificate(err);
      if (offer && !fingerprint) {
        setOffered(offer);
        setStep("trust");
        return false;
      }
      setStep("form");
      throw localized(t, err);
    }
    await refreshVmware(queryClient);
    toast.success(
      editing
        ? t("vmware.source.saved", { name: source.name })
        : t("vmware.source.added", { name: form.name }),
    );
  }

  async function submit() {
    if (step === "trust") {
      return send(offered!.fingerprint);
    }
    const found = checkSourceForm(form, editing, taken);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    return send();
  }

  const field = (
    key: keyof SourceForm,
    label: string,
    props: Partial<React.ComponentProps<"input">> = {},
    hint?: string,
  ) => (
    <Field id={`vmware-source-${key}`} label={label} error={errors[key]} hint={hint}>
      {(a11y) => (
        <Input
          id={`vmware-source-${key}`}
          value={String(form[key])}
          onChange={(e) => set({ [key]: e.target.value })}
          {...props}
          {...a11y}
        />
      )}
    </Field>
  );

  return (
    <FormDialog
      title={
        editing ? t("vmware.source.editTitle", { name: source.name }) : t("vmware.source.addTitle")
      }
      description={t("vmware.source.description")}
      submitLabel={
        step === "trust"
          ? t("vmware.source.trust")
          : editing
            ? t("common.save")
            : t("vmware.source.connect")
      }
      onClose={onClose}
      onSubmit={submit}
    >
      {step === "form" && (
        <>
          {!editing && field("name", t("common.name"))}
          {field("host", t("vmware.source.host"), {}, t("vmware.source.hostHint"))}
          {field("port", t("vmware.source.port"), { inputMode: "numeric" })}
          {field("user", t("vmware.source.user"), { autoComplete: "off" })}
          {field(
            "password",
            t("vmware.source.password"),
            { type: "password", autoComplete: "new-password" },
            editing ? t("vmware.source.passwordKeep") : undefined,
          )}
          {field(
            "datacenter",
            t("vmware.source.datacenter"),
            {},
            t("vmware.source.datacenterHint"),
          )}
          <div className="flex items-start gap-2">
            <Checkbox
              id="vmware-source-vddk"
              checked={form.vddk}
              onCheckedChange={(checked) => set({ vddk: checked === true })}
              aria-describedby="vmware-source-vddk-hint"
            />
            <div className="flex flex-col gap-1">
              <Label htmlFor="vmware-source-vddk">{t("vmware.source.vddk")}</Label>
              <p id="vmware-source-vddk-hint" className="text-xs text-muted-foreground">
                {t("vmware.source.vddkHint")}
              </p>
            </div>
          </div>
          {form.vddk &&
            field("libdir", t("vmware.source.libdir"), {}, t("vmware.source.libdirHint"))}
        </>
      )}
      {step === "trust" && offered && (
        <div className="flex flex-col gap-2 text-sm">
          <p>{t("vmware.source.untrusted", { host: form.host.trim() })}</p>
          <p className="text-muted-foreground">{t("vmware.source.fingerprint")}</p>
          <code className="rounded bg-muted p-2 font-mono text-xs break-all">
            {offered.fingerprint}
          </code>
          {offered.thumbprint && (
            <>
              <p className="text-muted-foreground">{t("vmware.source.thumbprint")}</p>
              <code className="rounded bg-muted p-2 font-mono text-xs break-all">
                {offered.thumbprint}
              </code>
            </>
          )}
          <Button
            type="button"
            variant="outline"
            className="self-start"
            onClick={() => setStep("form")}
          >
            {t("vmware.back")}
          </Button>
        </div>
      )}
    </FormDialog>
  );
}
