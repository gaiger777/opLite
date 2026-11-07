import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus, RefreshCw, Trash2 } from "lucide-react";
import { useId, useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { errorMessage } from "@/i18n/errors";
import type { RemoteView } from "@/remotes/remotes";
import { validName } from "@/vms/form";

/** 목록을 다시 읽는 간격. 원격은 watch로 받지 않는다(토큰을 담고 있다). */
const REFRESH_MS = 10_000;

/**
 * 원격 클러스터: 카드 목록과 admin의 추가·고치기·지우기·새로고침. 카드를 누르면 그 클러스터의 VM 목록으로
 * 간다. 상태는 리더가 모은 것을 10초마다 다시 읽는다.
 */
export function RemotesPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const admin = can(me.role, "write", "remote");
  const queryClient = useQueryClient();
  const remotes = useQuery({
    queryKey: ["remotes"],
    queryFn: () => call(api.GET("/remotes")),
    select: (list) => [...list.items].sort((a, b) => a.name.localeCompare(b.name)),
    refetchInterval: REFRESH_MS,
  });
  const [dialog, setDialog] = useState<{ remote: RemoteView | null } | null>(null);
  const [deleting, setDeleting] = useState<RemoteView | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["remotes"] });

  async function refresh(remote: RemoteView) {
    try {
      await call(
        api.POST("/remotes/{key}/actions/refresh", { params: { path: { key: remote.name } } }),
      );
      await changed();
    } catch (err) {
      toast.error(t("remotes.refreshFailed", { name: remote.name, error: errorMessage(t, err) }));
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 className="text-2xl font-semibold">{t("nav.remotes")}</h1>
        {admin && (
          <Button
            onClick={(e) => {
              opener.current = e.currentTarget;
              setDialog({ remote: null });
            }}
          >
            <Plus />
            {t("remotes.add")}
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">{t("remotes.pageHint")}</p>
      {!remotes.data ? (
        <ListPending error={remotes.error} />
      ) : remotes.data.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("remotes.empty")}</p>
      ) : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {remotes.data.map((remote) => (
            <RemoteCard
              key={remote.id}
              remote={remote}
              admin={admin}
              onRefresh={() => void refresh(remote)}
              onEdit={(target) => {
                opener.current = target;
                setDialog({ remote });
              }}
              onDelete={(target) => {
                opener.current = target;
                setDeleting(remote);
                setDeleteOpen(true);
              }}
            />
          ))}
        </div>
      )}
      {dialog && (
        <RemoteDialog
          remote={dialog.remote}
          returnFocus={opener}
          onClose={() => setDialog(null)}
          onDone={changed}
        />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("remotes.deleteTitle", { name: deleting?.name })}
        description={t("remotes.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const remote = deleting!;
          await call(api.DELETE("/remotes/{key}", { params: { path: { key: remote.name } } }));
          toast.success(t("remotes.deleted", { name: remote.name }));
          await changed();
        }}
      />
    </div>
  );
}

/** 원격 하나의 카드: 이름(VM 목록으로 가는 링크), 연결 상태, 모은 요약 */
function RemoteCard({
  remote,
  admin,
  onRefresh,
  onEdit,
  onDelete,
}: {
  remote: RemoteView;
  admin: boolean;
  onRefresh(): void;
  onEdit(target: HTMLElement): void;
  onDelete(target: HTMLElement): void;
}) {
  const { t } = useTranslation();
  const id = useId();
  const { status } = remote;
  const rows: [string, string][] = [
    [t("remotes.cluster"), status.cluster_name ?? "—"],
    [t("remotes.version"), status.version ?? "—"],
    [t("remotes.nodes"), status.nodes ? t("remotes.nodesValue", status.nodes) : "—"],
    [t("remotes.vms"), status.vms ? t("remotes.vmsValue", status.vms) : "—"],
    [t("remotes.cpus"), status.cpus != null ? String(status.cpus) : "—"],
    [
      t("remotes.memory"),
      status.memory_gib != null ? t("remotes.memoryValue", { gib: status.memory_gib }) : "—",
    ],
  ];
  return (
    <Card role="group" aria-labelledby={id}>
      <CardHeader>
        <CardTitle id={id} className="flex flex-wrap items-center gap-2">
          <Link
            to={`/remotes/${encodeURIComponent(remote.name)}`}
            className="underline-offset-4 hover:underline"
          >
            {remote.name}
          </Link>
          {status.reachable ? (
            <Badge>{t("remotes.reachable")}</Badge>
          ) : (
            <Badge variant="destructive">{t("remotes.unreachable")}</Badge>
          )}
        </CardTitle>
        <p className="truncate font-mono text-xs text-muted-foreground">{remote.url}</p>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {!status.reachable && status.error && (
          <p className="text-sm break-words text-destructive">{status.error}</p>
        )}
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
          {rows.map(([label, value]) => (
            <div key={label} className="contents">
              <dt className="text-muted-foreground">{label}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
        {admin && (
          <div className="flex justify-end gap-1">
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={t("remotes.refreshNamed", { name: remote.name })}
              onClick={onRefresh}
            >
              <RefreshCw />
            </Button>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={t("remotes.editNamed", { name: remote.name })}
              onClick={(e) => onEdit(e.currentTarget)}
            >
              <Pencil />
            </Button>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={t("common.deleteNamed", { name: remote.name })}
              onClick={(e) => onDelete(e.currentTarget)}
            >
              <Trash2 />
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

/** 서버가 CA를 받아 돌려준 정규화한 URL과 지문 */
interface Probe {
  url: string;
  ca_fingerprint: string;
}

/**
 * 원격 추가(`remote`가 null)와 고치기. 브라우저는 다른 출처의 CA를 읽지 못하므로 서버(`/remotes/probe`)가
 * 받아 온 지문을 보이고 사람이 원격 클러스터에서 확인하게 한다. 고칠 때는 저장된 URL·지문으로 시작하고,
 * URL이 바뀌면 CA를 다시 받는다. 토큰은 비우면 저장된 것을 두되, URL이나 지문이 바뀌면 다시 받는다(서버 규칙).
 */
function RemoteDialog({
  remote,
  returnFocus,
  onClose,
  onDone,
}: {
  remote: RemoteView | null;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onDone(): Promise<void>;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [url, setUrl] = useState(remote?.url ?? "");
  const [probe, setProbe] = useState<Probe | null>(
    remote && { url: remote.url, ca_fingerprint: remote.ca_fingerprint },
  );
  const [probing, setProbing] = useState(false);
  const [confirmed, setConfirmed] = useState(false);
  const [token, setToken] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [probeError, setProbeError] = useState<string | null>(null);
  // 저장된 URL·지문 그대로면 이미 확인한 것이다.
  const same =
    remote !== null &&
    probe !== null &&
    probe.url === remote.url &&
    probe.ca_fingerprint === remote.ca_fingerprint;

  async function getCa() {
    setProbing(true);
    setProbeError(null);
    setErrors((e) => ({ ...e, url: "" }));
    try {
      const found = await call(api.POST("/remotes/probe", { body: { url } }));
      setProbe(found);
      setUrl(found.url);
      setConfirmed(false);
    } catch (err) {
      setProbeError(errorMessage(t, err));
    } finally {
      setProbing(false);
    }
  }

  async function submit() {
    const found: Record<string, string> = {};
    if (!remote && !validName(name)) {
      found.name = "forms.name";
    }
    if (!probe) {
      found.url = "remotes.errors.probeFirst";
    } else if (!same && !confirmed) {
      found.confirm = "remotes.errors.confirm";
    }
    if (token.trim() === "" && !same) {
      found.token = remote ? "remotes.errors.tokenAgain" : "remotes.errors.token";
    }
    setErrors(found);
    if (Object.keys(found).length > 0 || !probe) {
      return false;
    }
    const body = {
      url: probe.url,
      ca_fingerprint: probe.ca_fingerprint,
      ...(token.trim() !== "" && { token: token.trim() }),
    };
    // 상태를 모을 때마다 revision이 바뀌므로 If-Match 없이 보낸다(서버가 지금 것에 쓴다).
    if (remote) {
      await call(api.PUT("/remotes/{key}", { params: { path: { key: remote.name } }, body }));
      toast.success(t("remotes.saved", { name: remote.name }));
    } else {
      await call(api.POST("/remotes", { body: { name, ...body } }));
      toast.success(t("remotes.created", { name }));
    }
    await onDone();
  }

  return (
    <FormDialog
      title={remote ? t("remotes.editTitle", { name: remote.name }) : t("remotes.addTitle")}
      description={t("remotes.addDescription")}
      submitLabel={remote ? t("common.save") : t("remotes.add")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {!remote && (
        <Field id="remote-name" label={t("common.name")} error={errors.name}>
          {(a11y) => (
            <Input
              id="remote-name"
              autoComplete="off"
              value={name}
              onChange={(e) => setName(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
      )}
      <Field
        id="remote-url"
        label={t("remotes.url")}
        error={probeError ? "remotes.errors.probe" : errors.url}
        errorValues={probeError ? { error: probeError } : undefined}
        hint={t("remotes.urlHint")}
      >
        {(a11y) => (
          <div className="flex gap-2">
            <Input
              id="remote-url"
              autoComplete="off"
              spellCheck={false}
              placeholder="https://"
              value={url}
              onChange={(e) => {
                setUrl(e.target.value);
                // 받은 지문은 그 URL의 것이다.
                setProbe(null);
                setConfirmed(false);
                setProbeError(null);
              }}
              {...a11y}
            />
            <Button
              type="button"
              variant="outline"
              disabled={probing || url.trim() === ""}
              onClick={() => void getCa()}
            >
              {t("remotes.getCa")}
            </Button>
          </div>
        )}
      </Field>
      {probe && (
        <div className="flex flex-col gap-2">
          <dl className="flex flex-col gap-2">
            <dt className="text-sm leading-none font-medium">{t("remotes.fingerprint")}</dt>
            <dd>
              <code className="block rounded-md bg-muted px-2 py-1 text-xs break-all">
                {probe.ca_fingerprint}
              </code>
            </dd>
          </dl>
          {!same && (
            <div className="flex items-start gap-2">
              <Checkbox
                id="remote-confirm"
                checked={confirmed}
                aria-invalid={errors.confirm ? true : undefined}
                aria-describedby={errors.confirm ? "remote-confirm-error" : undefined}
                onCheckedChange={(on) => setConfirmed(on === true)}
              />
              <Label htmlFor="remote-confirm" className="leading-snug font-normal">
                {t("remotes.confirmFingerprint")}
              </Label>
            </div>
          )}
          {errors.confirm && (
            <p id="remote-confirm-error" className="text-xs text-destructive">
              {t(errors.confirm)}
            </p>
          )}
        </div>
      )}
      <Field
        id="remote-token"
        label={t("remotes.token")}
        error={errors.token}
        hint={t(remote ? "remotes.tokenSaved" : "remotes.tokenHint")}
      >
        {(a11y) => (
          <Input
            id="remote-token"
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={token}
            onChange={(e) => setToken(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}
