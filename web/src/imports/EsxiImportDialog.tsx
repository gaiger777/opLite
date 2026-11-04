import { useQuery } from "@tanstack/react-query";
import { TriangleAlert } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { api, ApiError, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
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
import {
  esxiConnection,
  esxiImportName,
  importNameError,
  validateConnection,
  type ConnectionForm,
} from "@/imports/imports";
import { formatMib } from "@/lib/format";

type EsxiVm = components["schemas"]["EsxiVm"];

/** 연결 → (처음 보는 인증서면) 지문 확인 → VM 고르기 → 이름 */
type Step = "connect" | "trust" | "choose" | "name";

/** 잠긴 VM 라디오가 설명으로 가리키는 요소 */
const LOCKED_ID = "esxi-vm-locked";

/** 목록의 VM 한 행. 키는 `데이터스토어/.vmx 경로`다. */
interface Row {
  key: string;
  datastore: string;
  vm: EsxiVm;
}

/** 믿기로 한 인증서. 같은 호스트·포트로 다시 연결하면 지문을 함께 보낸다. */
interface Trusted {
  host: string;
  port: string;
  fingerprint: string;
}

function hasCode(error: unknown, code: string): boolean {
  return error instanceof ApiError && error.code === code;
}

/** 서버가 `untrusted_certificate`의 details로 알려 준 지문 */
function offeredFingerprint(error: unknown): string | undefined {
  if (error instanceof ApiError && error.code === "untrusted_certificate") {
    const fingerprint = (error.details as { fingerprint?: unknown } | undefined)?.fingerprint;
    return typeof fingerprint === "string" ? fingerprint : undefined;
  }
  return undefined;
}

/**
 * ESXi·vCenter에 연결해 VM을 고르고 가져오기를 만든다. 비밀번호는 이 컴포넌트의 상태에만 있고 요청
 * 본문으로만 보낸다(서버는 자격 증명을 접속한 노드의 메모리에만 둔다).
 */
export function EsxiImportDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const imports = useList("vm-import");
  const nodes = useList("node");
  // 연결과 가져오기는 접속한 노드에서 돈다(OVA 올리기와 같은 질의).
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const here = nodes.data?.find((n) => n.id === cluster.data?.node)?.name ?? "—";
  const [step, setStep] = useState<Step>("connect");
  const [form, setForm] = useState<ConnectionForm>({
    host: "",
    port: "443",
    user: "",
    password: "",
    datacenter: "ha-datacenter",
  });
  // 확인을 기다리는 지문, 믿기로 한 인증서, 목록을 읽은(만들기에 쓸) 지문
  const [offered, setOffered] = useState("");
  const [trusted, setTrusted] = useState<Trusted | null>(null);
  const [fingerprint, setFingerprint] = useState("");
  const [rows, setRows] = useState<Row[]>([]);
  const [chosen, setChosen] = useState("");
  const [name, setName] = useState("");
  // 이름을 제안한 VM. 고른 VM이 바뀔 때만 다시 제안해 사용자가 고친 이름을 지킨다.
  const [namedFor, setNamedFor] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const set = (patch: Partial<ConnectionForm>) => setForm((f) => ({ ...f, ...patch }));
  const row = rows.find((r) => r.key === chosen);

  /** 연결 단계로 돌아간다. 읽은 목록과 고른 VM은 버린다. */
  function backToConnect() {
    setRows([]);
    setChosen("");
    setFingerprint("");
    setErrors({});
    setStep("connect");
  }

  async function browse(trust?: string) {
    try {
      const found = await call(
        api.POST("/vm-imports/esxi/browse", { body: esxiConnection(form, trust) }),
      );
      setRows(
        found.datastores.flatMap((ds) =>
          ds.vms.map((vm) => ({ key: `${ds.name}/${vm.vmx}`, datastore: ds.name, vm })),
        ),
      );
      setFingerprint(trust ?? "");
      setStep("choose");
    } catch (error) {
      const offer = offeredFingerprint(error);
      if (offer && !trust) {
        setOffered(offer);
        setStep("trust");
        return;
      }
      if (hasCode(error, "fingerprint_mismatch")) {
        // 인증서가 바뀌었다. 다시 연결해 새 지문을 확인한다.
        setTrusted(null);
      }
      // 인증·연결 오류도 연결 단계에서 고쳐 다시 연결한다(오류는 폼 위에 보인다).
      backToConnect();
      throw error;
    }
  }

  async function submit() {
    switch (step) {
      case "connect": {
        const found = validateConnection(form);
        setErrors(found);
        if (Object.keys(found).length === 0) {
          // 같은 호스트·포트의 인증서를 이미 믿었으면 지문을 함께 보낸다.
          const same = trusted?.host === form.host.trim() && trusted.port === form.port;
          await browse(same ? trusted.fingerprint : undefined);
        }
        return false;
      }
      case "trust":
        setTrusted({ host: form.host.trim(), port: form.port, fingerprint: offered });
        await browse(offered);
        return false;
      case "choose":
        if (!row) {
          setErrors({ vm: "imports.errors.vm" });
          return false;
        }
        setErrors({});
        if (namedFor !== row.key) {
          setName(esxiImportName(row.vm));
          setNamedFor(row.key);
        }
        setStep("name");
        return false;
      case "name": {
        const nameError = importNameError(name, imports.data?.map((i) => i.name) ?? []);
        setErrors(nameError ? { name: nameError } : {});
        if (nameError || !row) {
          return false;
        }
        let imp;
        try {
          imp = await call(
            api.POST("/vm-imports/esxi", {
              body: {
                name,
                connection: esxiConnection(form, fingerprint),
                datastore: row.datastore,
                vmx: row.vm.vmx,
              },
            }),
          );
        } catch (error) {
          if (hasCode(error, "name_taken")) {
            setErrors({ name: "imports.errors.exists" });
            return false;
          }
          if (hasCode(error, "fingerprint_mismatch")) {
            // 목록을 읽은 뒤 인증서가 바뀌었다. 다시 연결해 새 지문을 확인한다.
            setTrusted(null);
            backToConnect();
          }
          throw error;
        }
        void navigate(`/imports/${encodeURIComponent(imp.name)}`);
      }
    }
  }

  const submitLabel = {
    connect: t("imports.connect"),
    trust: t("imports.trust"),
    choose: t("imports.next"),
    name: t("common.create"),
  }[step];

  return (
    <FormDialog
      title={t("imports.esxiTitle")}
      description={t("imports.esxiDescription", { node: here })}
      submitLabel={submitLabel}
      onClose={onClose}
      onSubmit={submit}
    >
      {cluster.isError && (
        <p role="alert" className="text-sm text-destructive">
          {t("uploads.noNode", { error: errorMessage(t, cluster.error) })}
        </p>
      )}
      {step === "connect" && (
        <>
          <Field id="esxi-host" label={t("imports.host")} error={errors.host}>
            {(a11y) => (
              <Input
                id="esxi-host"
                value={form.host}
                onChange={(e) => set({ host: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Field id="esxi-port" label={t("imports.port")} error={errors.port}>
            {(a11y) => (
              <Input
                id="esxi-port"
                inputMode="numeric"
                value={form.port}
                onChange={(e) => set({ port: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Field id="esxi-user" label={t("imports.user")} error={errors.user}>
            {(a11y) => (
              <Input
                id="esxi-user"
                autoComplete="off"
                value={form.user}
                onChange={(e) => set({ user: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Field id="esxi-password" label={t("imports.password")}>
            <Input
              id="esxi-password"
              type="password"
              autoComplete="new-password"
              value={form.password}
              onChange={(e) => set({ password: e.target.value })}
            />
          </Field>
          <Field
            id="esxi-datacenter"
            label={t("imports.datacenter")}
            error={errors.datacenter}
            hint={t("imports.datacenterHint")}
          >
            {(a11y) => (
              <Input
                id="esxi-datacenter"
                value={form.datacenter}
                onChange={(e) => set({ datacenter: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
        </>
      )}
      {step === "trust" && (
        <div className="flex flex-col gap-2 text-sm">
          <p>{t("imports.untrusted", { host: form.host.trim() })}</p>
          <p className="text-muted-foreground">{t("imports.fingerprint")}</p>
          <code className="rounded bg-muted p-2 font-mono text-xs break-all">{offered}</code>
          <Button type="button" variant="outline" className="self-start" onClick={backToConnect}>
            {t("imports.back")}
          </Button>
        </div>
      )}
      {step === "choose" && (
        <div className="flex flex-col gap-1.5">
          {rows.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t("imports.noVms")}</p>
          ) : (
            <Table aria-label={t("imports.vms")}>
              <TableHeader>
                <TableRow>
                  <TableHead />
                  <TableHead>{t("imports.datastore")}</TableHead>
                  <TableHead>{t("common.name")}</TableHead>
                  <TableHead>{t("imports.guestOs")}</TableHead>
                  <TableHead>{t("vms.cpu")}</TableHead>
                  <TableHead>{t("vms.memory")}</TableHead>
                  <TableHead>{t("vms.disks")}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map(({ key, datastore, vm }, i) => {
                  // 잠금 경고와 읽지 못한 이유는 라디오의 설명이다.
                  const errorId = `esxi-vm-${i}-error`;
                  const described = [vm.locked && LOCKED_ID, vm.error && errorId]
                    .filter(Boolean)
                    .join(" ");
                  return (
                    <TableRow key={key}>
                      <TableCell>
                        {/* 읽지 못한 VM은 고를 수 없다 */}
                        <input
                          type="radio"
                          name="esxi-vm"
                          aria-label={`${vm.name || vm.vmx} (${datastore})`}
                          aria-describedby={described || undefined}
                          disabled={!!vm.error}
                          checked={chosen === key}
                          onChange={() => setChosen(key)}
                        />
                      </TableCell>
                      <TableCell>{datastore}</TableCell>
                      <TableCell className="whitespace-normal">
                        <span className="inline-flex items-center gap-1">
                          {vm.name || vm.vmx}
                          {vm.locked && (
                            <TriangleAlert
                              role="img"
                              aria-label={t("imports.locked")}
                              className="size-4 text-amber-600"
                            />
                          )}
                        </span>
                        {vm.error && (
                          <div
                            id={errorId}
                            className="max-w-xs text-xs break-words text-destructive"
                          >
                            {vm.error}
                          </div>
                        )}
                      </TableCell>
                      <TableCell>{vm.error ? "" : vm.guest_os}</TableCell>
                      <TableCell>{vm.error ? "" : vm.vcpus}</TableCell>
                      <TableCell>{vm.error ? "" : formatMib(vm.memory_mib)}</TableCell>
                      <TableCell>{vm.error ? "" : vm.disks}</TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          )}
          {/* 잠긴 VM 라디오의 설명(아이콘의 이름은 설명으로 읽히지 않을 수 있다) */}
          <span id={LOCKED_ID} hidden>
            {t("imports.locked")}
          </span>
          {errors.vm && (
            <p role="alert" className="text-xs text-destructive">
              {t(errors.vm)}
            </p>
          )}
          <Button type="button" variant="outline" className="self-start" onClick={backToConnect}>
            {t("imports.back")}
          </Button>
        </div>
      )}
      {step === "name" && row && (
        <>
          <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 text-sm">
            <dt className="text-muted-foreground">{t("imports.chosenVm")}</dt>
            <dd className="break-all">{`${row.datastore} / ${row.vm.vmx}`}</dd>
          </dl>
          <Field id="esxi-name" label={t("common.name")} error={errors.name}>
            {(a11y) => (
              <Input
                id="esxi-name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Button
            type="button"
            variant="outline"
            className="self-start"
            onClick={() => {
              setErrors({});
              setStep("choose");
            }}
          >
            {t("imports.back")}
          </Button>
        </>
      )}
    </FormDialog>
  );
}
