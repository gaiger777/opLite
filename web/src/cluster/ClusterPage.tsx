import { useQuery } from "@tanstack/react-query";
import { Download, Plus } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { CopyButton } from "@/components/CopyButton";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Badge } from "@/components/ui/badge";
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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatTime } from "@/lib/format";

type JoinToken = components["schemas"]["CreatedJoinToken"];

/** 조인 토큰의 유효 시간(초). 서버의 기본은 1시간, 최대는 7일이다. */
const TTLS = [3600, 86400, 604800] as const;

/** 새 노드에서 실행할 명령. URL에 `?`와 `&`가 있어 작은따옴표로 감싸고, 안의 `'`는 `'\''`로 바꾼다. */
export function joinCommand(url: string): string {
  return `oplctl cluster join '${url.replaceAll("'", "'\\''")}'`;
}

export function ClusterPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  // 멤버·리더·온라인은 감시할 수 없으므로 이 화면에서만 주기적으로 다시 읽는다.
  const cluster = useQuery({
    queryKey: ["cluster"],
    queryFn: () => call(api.GET("/cluster")),
    refetchInterval: 10_000,
  });
  const [issuing, setIssuing] = useState(false);
  const [issued, setIssued] = useState<JoinToken | null>(null);

  if (!cluster.data) {
    return <ListPending error={cluster.error} />;
  }
  const { name, ca_fingerprint: fingerprint, leader, node: here, members } = cluster.data;
  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 className="text-2xl font-semibold">{t("cluster.title", { name })}</h1>
        {can(me.role, "write", "join-token") && (
          <Button onClick={() => setIssuing(true)}>
            <Plus />
            {t("cluster.issue")}
          </Button>
        )}
      </div>
      {/* 다시 읽지 못하면 마지막으로 읽은 내용을 그대로 보이고 오류를 알린다. */}
      {cluster.isError && (
        <p role="alert" className="text-sm text-destructive">
          {t("cluster.refreshFailed", { error: errorMessage(t, cluster.error) })}
        </p>
      )}
      <section className="flex flex-col gap-2">
        <h2 className="font-medium">{t("cluster.ca")}</h2>
        <p className="text-sm text-muted-foreground">{t("cluster.caNote")}</p>
        <div className="flex flex-wrap items-center gap-2">
          <code className="rounded-md bg-muted px-2 py-1 text-xs break-all">{fingerprint}</code>
          <CopyButton text={fingerprint} label={t("cluster.copyFingerprint")} />
          <Button variant="outline" size="sm" asChild>
            <a href="/api/v1/cluster/ca" download="opl-ca.pem">
              <Download />
              {t("cluster.downloadCa")}
            </a>
          </Button>
        </div>
      </section>
      <section className="flex flex-col gap-2">
        <h2 id="cluster-members" className="font-medium">
          {t("cluster.members")}
        </h2>
        <Table aria-labelledby="cluster-members">
          <TableHeader>
            <TableRow>
              <TableHead>{t("common.name")}</TableHead>
              <TableHead>{t("common.state")}</TableHead>
              <TableHead>{t("cluster.role")}</TableHead>
              <TableHead>{t("nodes.address")}</TableHead>
              <TableHead>{t("cluster.clusterAddress")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {members.map((m) => (
              <TableRow key={m.id}>
                <TableCell className="font-medium">
                  {m.name ?? m.id.slice(-8)}
                  {m.id === here && (
                    <span className="ml-2 text-xs text-muted-foreground">
                      {t("nodes.thisNode")}
                    </span>
                  )}
                </TableCell>
                <TableCell>
                  <Badge variant={m.online ? "default" : "destructive"}>
                    {t(m.online ? "dashboard.nodeOnline" : "dashboard.nodeOffline")}
                  </Badge>
                </TableCell>
                <TableCell>
                  {m.id === leader
                    ? t("cluster.leader")
                    : t(m.voter ? "cluster.voter" : "cluster.learner")}
                </TableCell>
                <TableCell className="font-mono text-xs">{m.address ?? "—"}</TableCell>
                <TableCell className="font-mono text-xs">{m.cluster_address}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
        <p className="text-sm text-muted-foreground">{t("cluster.membersNote")}</p>
      </section>
      {issuing && (
        <IssueDialog onClose={() => setIssuing(false)} onIssued={(token) => setIssued(token)} />
      )}
      {issued && <JoinTokenDialog token={issued} onClose={() => setIssued(null)} />}
    </div>
  );
}

function IssueDialog({ onClose, onIssued }: { onClose(): void; onIssued(token: JoinToken): void }) {
  const { t } = useTranslation();
  const [ttl, setTtl] = useState<number>(TTLS[0]);

  async function submit() {
    onIssued(await call(api.POST("/cluster/join-tokens", { body: { ttl_secs: ttl } })));
  }

  return (
    <FormDialog
      title={t("cluster.issueTitle")}
      description={t("cluster.issueDescription")}
      submitLabel={t("cluster.issue")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="join-ttl" label={t("cluster.ttl")}>
        <NativeSelect
          id="join-ttl"
          className="w-full"
          value={ttl}
          onChange={(e) => setTtl(Number(e.target.value))}
        >
          {TTLS.map((secs) => (
            <NativeSelectOption key={secs} value={secs}>
              {t(`cluster.ttlOption.${secs}`)}
            </NativeSelectOption>
          ))}
        </NativeSelect>
      </Field>
    </FormDialog>
  );
}

/** 발급한 조인 URL. 서버는 다시 보여 주지 않으므로 "완료"로만 닫는다. */
function JoinTokenDialog({ token, onClose }: { token: JoinToken; onClose(): void }) {
  const { t, i18n } = useTranslation();
  const command = joinCommand(token.url);
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent
        showCloseButton={false}
        onEscapeKeyDown={(e) => e.preventDefault()}
        onInteractOutside={(e) => e.preventDefault()}
      >
        <DialogHeader>
          <DialogTitle>{t("cluster.issuedTitle")}</DialogTitle>
          <DialogDescription>
            {t("cluster.issuedDescription", {
              time: formatTime(new Date(token.expires_at * 1000), i18n.language),
            })}
          </DialogDescription>
        </DialogHeader>
        <div className="flex items-center gap-2">
          <Input
            readOnly
            aria-label={t("cluster.command")}
            className="font-mono text-xs"
            value={command}
            onFocus={(e) => e.target.select()}
          />
          <CopyButton text={command} label={t("cluster.copyCommand")} />
        </div>
        <p className="text-xs text-muted-foreground">{t("cluster.commandNote")}</p>
        <DialogFooter>
          <Button onClick={onClose}>{t("common.done")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
