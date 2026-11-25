import { useQueryClient } from "@tanstack/react-query";
import { KeyRound, Plus, RefreshCw, ShieldCheck, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Task } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import {
  type AcmeAccount,
  type AcmeDns,
  type Certificate,
  daysLeft,
  expiryTone,
  issued,
  refreshCerts,
  useAccounts,
  useCertificates,
  useDnsProviders,
} from "@/certs/api";
import { AccountDialog, CertificateDialog, DnsDialog } from "@/certs/dialogs";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { EmptyState } from "@/components/EmptyState";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatDate } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";

type Deleting =
  | { kind: "account"; item: AcmeAccount }
  | { kind: "dns"; item: AcmeDns }
  | { kind: "certificate"; item: Certificate };

function Section({
  id,
  title,
  action,
  children,
}: {
  id: string;
  title: string;
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <section aria-labelledby={id} className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id={id} className="text-base font-semibold">
          {title}
        </h2>
        {action}
      </div>
      {children}
    </section>
  );
}

/** 인증서 하나의 받은 것들: 노드(또는 공유)마다 만료·다음 갱신·끼우기 오류 칩 */
function IssuedCells({ cert }: { cert: Certificate }) {
  const { t, i18n } = useTranslation();
  const nodes = useList("node");
  const all = issued(cert);
  const nodeName = (slot: string) =>
    slot === "shared"
      ? t("certs.scope.shared")
      : (nodes.data?.find((n) => n.id === slot)?.name ?? slot.slice(-8));
  if (all.length === 0) {
    return <span className="text-muted-foreground">{t("certs.notIssued")}</span>;
  }
  return (
    <ul className="flex flex-col gap-1">
      {all.map(({ slot, cert: c }) => {
        const days = daysLeft(c.not_after);
        return (
          <li key={slot} className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs">
            <span className="font-medium">{nodeName(slot)}</span>
            <StatusChip tone={expiryTone(days)}>
              {days < 0 ? t("certs.expired") : t("certs.expiresIn", { count: days })}
            </StatusChip>
            <span className="text-muted-foreground">
              {t("certs.renewAt", { date: formatDate(new Date(c.renew_at * 1000), i18n.language) })}
            </span>
            {c.error && <span className="text-danger-fg">{c.error}</span>}
          </li>
        );
      })}
    </ul>
  );
}

/**
 * 설정 › 보안 › 인증서(계획 73 §11·§13): ACME 계정·DNS 공급자·인증서 목록, 노드마다 만료·다음 갱신·오류 칩, "지금 갱신", 만들기
 * 대화상자 셋. 읽기는 루트 `cluster.read`(가린 보기), 쓰기는 `cluster.admin`. 늦게 읽는 조각이다.
 */
export function CertificatesPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const readable = perms.reads("certificate");
  const write = perms.can("write", "certificate");
  const accounts = useAccounts(readable);
  const dns = useDnsProviders(readable);
  const certs = useCertificates(readable);
  const [creating, setCreating] = useState<"account" | "dns" | "certificate" | null>(null);
  const [deleting, setDeleting] = useState<Deleting | null>(null);

  async function renew(cert: Certificate) {
    try {
      const started = await call(
        api.POST("/certificates/{key}/actions/renew", { params: { path: { key: cert.id } } }),
      );
      for (const task of started.tasks) {
        trackTask(task as unknown as Task, t("tasks.op.acme-order", { target: cert.name }));
      }
      await refreshCerts(queryClient);
    } catch (err) {
      toast.error(errorMessage(t, err));
    }
  }

  async function check(account: AcmeAccount) {
    try {
      await call(
        api.POST("/acme-accounts/{key}/actions/check", { params: { path: { key: account.id } } }),
      );
      toast.success(t("certs.account.checked", { name: account.name }));
      await refreshCerts(queryClient);
    } catch (err) {
      toast.error(errorMessage(t, err));
    }
  }

  if (!readable) {
    return (
      <div className="flex flex-col gap-4">
        <PageHeader title={t("nav.certificates")} />
        <p className="text-sm text-muted-foreground">{t("nav.noAccess")}</p>
      </div>
    );
  }

  const accountList = accounts.data ?? [];
  const dnsList = dns.data ?? [];
  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        id="certs-title"
        title={t("nav.certificates")}
        description={t("certs.pageHint")}
      />
      <Section
        id="certs-list"
        title={t("certs.certificates")}
        action={
          write && (
            <Button
              size="sm"
              disabled={accountList.length === 0}
              onClick={() => setCreating("certificate")}
            >
              <Plus />
              {t("certs.cert.open")}
            </Button>
          )
        }
      >
        {certs.data === undefined ? (
          <ListPending error={certs.error} />
        ) : certs.data.length === 0 ? (
          <EmptyState
            icon={ShieldCheck}
            title={t("certs.emptyCertificates")}
            description={t(
              accountList.length === 0 ? "certs.needAccount" : "certs.emptyCertificatesHint",
            )}
          />
        ) : (
          <div className="overflow-x-auto">
            <Table aria-labelledby="certs-list">
              <TableHeader>
                <TableRow>
                  <TableHead>{t("common.name")}</TableHead>
                  <TableHead>{t("certs.cert.names")}</TableHead>
                  <TableHead>{t("certs.cert.challenge")}</TableHead>
                  <TableHead>{t("certs.issued")}</TableHead>
                  <TableHead>
                    <span className="sr-only">{t("vms.actions")}</span>
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {certs.data.map((cert) => (
                  <TableRow key={cert.id}>
                    <TableCell className="align-top font-medium">
                      {cert.name}
                      <div className="text-xs font-normal text-muted-foreground">
                        {t(`certs.scope.${cert.spec.scope?.kind ?? "per_node"}`)} ·{" "}
                        {cert.spec.account}
                      </div>
                    </TableCell>
                    <TableCell className="max-w-56 align-top text-xs break-all whitespace-normal">
                      {cert.spec.names.join(", ")}
                    </TableCell>
                    <TableCell className="align-top text-xs">
                      {cert.spec.challenge.kind === "http01"
                        ? "HTTP-01"
                        : `DNS-01 (${cert.spec.challenge.dns})`}
                    </TableCell>
                    <TableCell className="align-top whitespace-normal">
                      <IssuedCells cert={cert} />
                      {cert.status.error && (
                        <div className="mt-1 flex flex-wrap items-center gap-2 text-xs">
                          <StatusChip tone="danger">
                            {t("certs.failures", { count: cert.status.failures || 1 })}
                          </StatusChip>
                          <span className="break-words text-danger-fg">{cert.status.error}</span>
                        </div>
                      )}
                    </TableCell>
                    <TableCell className="text-right align-top whitespace-nowrap">
                      {write && (
                        <>
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            aria-label={t("certs.renewNamed", { name: cert.name })}
                            onClick={() => void renew(cert)}
                          >
                            <RefreshCw />
                          </Button>
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            aria-label={t("common.deleteNamed", { name: cert.name })}
                            onClick={() => setDeleting({ kind: "certificate", item: cert })}
                          >
                            <Trash2 />
                          </Button>
                        </>
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </Section>
      <Section
        id="certs-accounts"
        title={t("certs.accounts")}
        action={
          write && (
            <Button size="sm" variant="outline" onClick={() => setCreating("account")}>
              <Plus />
              {t("certs.account.open")}
            </Button>
          )
        }
      >
        {accounts.data === undefined ? (
          <ListPending error={accounts.error} />
        ) : accountList.length === 0 ? (
          <EmptyState icon={KeyRound} title={t("certs.emptyAccounts")} />
        ) : (
          <div className="overflow-x-auto">
            <Table aria-labelledby="certs-accounts">
              <TableHeader>
                <TableRow>
                  <TableHead>{t("common.name")}</TableHead>
                  <TableHead>{t("certs.account.directory")}</TableHead>
                  <TableHead>{t("certs.account.email")}</TableHead>
                  <TableHead>{t("common.state")}</TableHead>
                  <TableHead>
                    <span className="sr-only">{t("vms.actions")}</span>
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {accountList.map((a) => (
                  <TableRow key={a.id}>
                    <TableCell className="font-medium">{a.name}</TableCell>
                    <TableCell className="max-w-56 text-xs break-all whitespace-normal">
                      {a.spec.directory}
                    </TableCell>
                    <TableCell>{a.spec.email ?? "—"}</TableCell>
                    <TableCell className="whitespace-normal">
                      {a.status.error ? (
                        <>
                          <StatusChip tone="danger">{t("certs.account.error")}</StatusChip>
                          <div className="text-xs break-words text-danger-fg">{a.status.error}</div>
                        </>
                      ) : a.status.url ? (
                        <StatusChip tone="success">{t("certs.account.registered")}</StatusChip>
                      ) : (
                        <StatusChip tone="neutral">{t("certs.account.pending")}</StatusChip>
                      )}
                    </TableCell>
                    <TableCell className="text-right whitespace-nowrap">
                      {write && (
                        <>
                          <Button variant="ghost" size="sm" onClick={() => void check(a)}>
                            {t("certs.account.check")}
                          </Button>
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            aria-label={t("common.deleteNamed", { name: a.name })}
                            onClick={() => setDeleting({ kind: "account", item: a })}
                          >
                            <Trash2 />
                          </Button>
                        </>
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </Section>
      <Section
        id="certs-dns"
        title={t("certs.dnsProviders")}
        action={
          write && (
            <Button size="sm" variant="outline" onClick={() => setCreating("dns")}>
              <Plus />
              {t("certs.dns.open")}
            </Button>
          )
        }
      >
        {dns.data === undefined ? (
          <ListPending error={dns.error} />
        ) : dnsList.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("certs.emptyDns")}</p>
        ) : (
          <div className="overflow-x-auto">
            <Table aria-labelledby="certs-dns">
              <TableHeader>
                <TableRow>
                  <TableHead>{t("common.name")}</TableHead>
                  <TableHead>{t("certs.dns.kind")}</TableHead>
                  <TableHead>{t("certs.dns.zones")}</TableHead>
                  <TableHead>
                    <span className="sr-only">{t("vms.actions")}</span>
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {dnsList.map((d) => (
                  <TableRow key={d.id}>
                    <TableCell className="font-medium">{d.name}</TableCell>
                    <TableCell>{t(`certs.dnsKinds.${d.spec.provider.kind}`)}</TableCell>
                    <TableCell className="text-xs">
                      {(d.spec.zones ?? []).join(", ") || "—"}
                    </TableCell>
                    <TableCell className="text-right">
                      {write && (
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={t("common.deleteNamed", { name: d.name })}
                          onClick={() => setDeleting({ kind: "dns", item: d })}
                        >
                          <Trash2 />
                        </Button>
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </Section>
      {creating === "account" && <AccountDialog onClose={() => setCreating(null)} />}
      {creating === "dns" && <DnsDialog onClose={() => setCreating(null)} />}
      {creating === "certificate" && (
        <CertificateDialog accounts={accountList} dns={dnsList} onClose={() => setCreating(null)} />
      )}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={t("certs.deleteTitle", { name: deleting?.item.name })}
        description={t(`certs.deleteHint.${deleting?.kind ?? "certificate"}`)}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const d = deleting!;
          const path = { params: { path: { key: d.item.id } } };
          if (d.kind === "account") {
            await call(api.DELETE("/acme-accounts/{key}", path));
          } else if (d.kind === "dns") {
            await call(api.DELETE("/acme-dns/{key}", path));
          } else {
            await call(api.DELETE("/certificates/{key}", path));
          }
          await refreshCerts(queryClient);
        }}
      />
    </div>
  );
}
