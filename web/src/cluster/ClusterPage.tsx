import { useQuery } from "@tanstack/react-query";
import { Download, Plus } from "lucide-react";
import { Check } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import { usePermissions } from "@/auth/usePermissions";
import { CopyButton } from "@/components/CopyButton";
import { Diagram } from "@/help/Diagram";
import { ClusterCpuSection } from "@/cpu/ClusterCpuSection";
import { Field } from "@/components/Field";
import { focusBack } from "@/components/focusBack";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
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
  return `orbivisorctl cluster join '${url.replaceAll("'", "'\\''")}'`;
}

export function ClusterPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  // 멤버·리더·온라인은 감시할 수 없으므로 이 화면에서만 주기적으로 다시 읽는다.
  const cluster = useQuery({
    queryKey: ["cluster"],
    queryFn: () => call(api.GET("/cluster")),
    refetchInterval: 10_000,
  });
  const [issuing, setIssuing] = useState(false);
  const [issued, setIssued] = useState<JoinToken | null>(null);
  // 발급·명령 대화상자를 닫으면 "참가 토큰 발급" 버튼으로 돌아간다.
  const opener = useRef<HTMLButtonElement>(null);

  if (!cluster.data) {
    return <ListPending error={cluster.error} />;
  }
  const { name, ca_fingerprint: fingerprint, leader, node: here, members } = cluster.data;
  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title={t("cluster.title", { name })}
        help="cluster"
        intro={t("help.cluster.intro")}
        actions={
          perms.can("write", "join-token") && (
            <Button ref={opener} onClick={() => setIssuing(true)}>
              <Plus />
              {t("cluster.issue")}
            </Button>
          )
        }
      />
      {/* 다시 읽지 못하면 마지막으로 읽은 내용을 그대로 보이고 오류를 알린다. */}
      {cluster.isError && (
        <p role="alert" className="text-sm text-destructive">
          {t("cluster.refreshFailed", { error: errorMessage(t, cluster.error) })}
        </p>
      )}
      <section className="flex flex-col gap-2">
        <h2 className="font-medium">{t("cluster.ca")}</h2>
        <p className="text-sm text-muted-foreground">{t("cluster.caNote")}</p>
        {/* ACME 공인 인증서는 관리 › 보안·인증서에 있다(두 화면이 서로 가리킨다). */}
        {perms.reads("certificate") && (
          <p className="text-sm text-muted-foreground">
            {t("cluster.acmeHint")}{" "}
            <Link to="/settings/certificates" className="underline underline-offset-4">
              {t("nav.group.admin")} › {t("nav.item.security")}
            </Link>
          </p>
        )}
        <details className="text-sm text-muted-foreground">
          <summary className="cursor-pointer">{t("cluster.caCheck")}</summary>
          <p className="mt-1">{t("cluster.caCheckNote")}</p>
        </details>
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
              <TableHead>{t("nodes.fencing")}</TableHead>
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
                  {m.id === leader ? t("cluster.leader") : t(`cluster.${m.role}`)}
                  {m.witness && (
                    <Badge variant="outline" className="ml-2">
                      {t("dashboard.witness")}
                    </Badge>
                  )}
                </TableCell>
                <TableCell>{m.fencing ? t(`nodes.fencingMode.${m.fencing}`) : "—"}</TableCell>
                <TableCell className="font-mono text-xs">{m.address ?? "—"}</TableCell>
                <TableCell className="font-mono text-xs">
                  {m.cluster_address}
                  {/* 둘째 링크부터(넘어갈 링크)는 우선 링크 아래 작게 */}
                  {m.cluster_addresses.slice(1).map((link) => (
                    <div key={link} className="text-muted-foreground">
                      {link}
                    </div>
                  ))}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
        <div className="flex flex-wrap items-center gap-4">
          <Diagram id="quorum" className="w-full max-w-xs" />
          <p className="min-w-0 flex-1 text-sm text-muted-foreground">{t("cluster.membersNote")}</p>
        </div>
      </section>
      <ClusterCpuSection />
      {issuing && (
        <IssueDialog
          returnFocus={opener}
          onClose={() => setIssuing(false)}
          onIssued={(token) => setIssued(token)}
        />
      )}
      {issued && (
        <JoinTokenDialog token={issued} returnFocus={opener} onClose={() => setIssued(null)} />
      )}
    </div>
  );
}

function IssueDialog({
  returnFocus,
  onClose,
  onIssued,
}: {
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onIssued(token: JoinToken): void;
}) {
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
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <JoinSteps current="issue" />
      <Field id="join-ttl" label={t("cluster.ttl")} help={t("help.cluster.ttl")}>
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

/** 합류 마법사의 단계(설계 §5.6): 토큰 만들기 → 새 노드에서 명령 실행 → 노드가 나타나기 기다리기 */
const JOIN_STEPS = ["issue", "run", "wait"] as const;
type JoinStep = (typeof JOIN_STEPS)[number] | "done";

function JoinSteps({ current }: { current: JoinStep }) {
  const { t } = useTranslation();
  const at = current === "done" ? JOIN_STEPS.length : JOIN_STEPS.indexOf(current);
  return (
    <ol aria-label={t("cluster.join.steps")} className="flex flex-wrap gap-x-4 gap-y-1 text-sm">
      {JOIN_STEPS.map((step, i) => (
        <li
          key={step}
          aria-current={i === at ? "step" : undefined}
          className={i === at ? "font-medium" : "flex items-center gap-1 text-muted-foreground"}
        >
          {i < at && <Check aria-hidden className="size-3.5 text-success" />}
          {`${i + 1}. ${t(`cluster.join.step.${step}`)}`}
        </li>
      ))}
    </ol>
  );
}

/**
 * 발급한 조인 URL과 합류 기다리기. 서버는 URL을 다시 보여 주지 않으므로 "완료"로만 닫는다. 열 때의 노드 목록에 없던
 * 노드가 watch로 나타나면 클러스터에 노드가 들어왔다고 알린다(어느 토큰인지는 모른다).
 */
function JoinTokenDialog({
  token,
  returnFocus,
  onClose,
}: {
  token: JoinToken;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t, i18n } = useTranslation();
  const command = joinCommand(token.url);
  const nodes = useList("node");
  // 열 때 이미 있던 노드. 목록을 아직 읽지 못했으면 읽은 때의 목록이다.
  const [known, setKnown] = useState<Set<string> | null>(null);
  const [copied, setCopied] = useState(false);
  if (known === null && nodes.data) {
    setKnown(new Set(nodes.data.map((n) => n.id)));
  }
  // 서버는 노드가 어느 토큰으로 합류했는지 알려 주지 않는다. 다른 토큰으로 들어온 노드일 수 있으므로 새로 나타난
  // 노드를 모두 이름으로 알리고 이 토큰의 노드라고 하지 않는다.
  const joined = known ? (nodes.data ?? []).filter((n) => !known.has(n.id)) : [];
  const step: JoinStep = joined.length > 0 ? "done" : copied ? "wait" : "run";
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent
        showCloseButton={false}
        onEscapeKeyDown={(e) => e.preventDefault()}
        onInteractOutside={(e) => e.preventDefault()}
        onCloseAutoFocus={(e) => focusBack(e, returnFocus)}
      >
        <DialogHeader>
          <DialogTitle>{t("cluster.issuedTitle")}</DialogTitle>
          <DialogDescription>
            {t("cluster.issuedDescription", {
              time: formatTime(new Date(token.expires_at * 1000), i18n.language),
            })}
          </DialogDescription>
        </DialogHeader>
        <JoinSteps current={step} />
        <div className="flex items-center gap-2" onClickCapture={() => setCopied(true)}>
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
        {/* 합류를 화면 읽기 프로그램에 알린다(머리줄 LiveBadge의 status와 겹치지 않게 role 없이). */}
        <p aria-live="polite" className="text-sm">
          {joined.length > 0 ? (
            <span className="flex items-center gap-1 font-medium text-success-fg">
              <Check aria-hidden className="size-4" />
              {t("cluster.join.joined", {
                names: joined.map((n) => n.name).join(", "),
                count: joined.length,
              })}
            </span>
          ) : (
            <span className="text-muted-foreground">{t("cluster.join.waiting")}</span>
          )}
        </p>
        <DialogFooter>
          <Button onClick={onClose}>{t("common.done")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
