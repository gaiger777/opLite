import { useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus, Trash2 } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { PageHeader } from "@/components/PageHeader";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { type RemoteView, useRemotes } from "@/remotes/remotes";
import { validName } from "@/vms/form";

/**
 * 설정 › 원격 클러스터(admin): 주소·CA 지문·토큰을 더하고 고치고 지운다. 상태 카드와 원격 VM은 보호 › 원격
 * 복제(`RemotesPage`)에 있다.
 */
export function RemotesManagePage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const remotes = useRemotes();
  const [dialog, setDialog] = useState<{ remote: RemoteView | null } | null>(null);
  const [deleting, setDeleting] = useState<RemoteView | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["remotes"] });
  const list = remotes.data ?? [];

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="remotes-title"
        title={t("nav.remotes")}
        description={t("remotes.manageHint")}
        actions={
          <Button
            onClick={(e) => {
              opener.current = e.currentTarget;
              setDialog({ remote: null });
            }}
          >
            <Plus />
            {t("remotes.add")}
          </Button>
        }
      />
      <Table aria-labelledby="remotes-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("remotes.url")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>
              <span className="sr-only">{t("vms.actions")}</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 && (
            <TableMessage colSpan={4} query={remotes} empty={t("remotes.empty")} />
          )}
          {list.map((remote) => (
            <TableRow key={remote.id}>
              <TableCell className="font-medium">
                <Link
                  to={`/remotes/${encodeURIComponent(remote.name)}`}
                  className="underline-offset-4 hover:underline"
                >
                  {remote.name}
                </Link>
              </TableCell>
              <TableCell className="font-mono text-xs">{remote.url}</TableCell>
              <TableCell>
                {remote.status.reachable ? (
                  <Badge>{t("remotes.reachable")}</Badge>
                ) : (
                  <Badge variant="destructive">{t("remotes.unreachable")}</Badge>
                )}
              </TableCell>
              <TableCell className="text-right">
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={t("remotes.editNamed", { name: remote.name })}
                  onClick={(e) => {
                    opener.current = e.currentTarget;
                    setDialog({ remote });
                  }}
                >
                  <Pencil />
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={t("common.deleteNamed", { name: remote.name })}
                  onClick={(e) => {
                    opener.current = e.currentTarget;
                    setDeleting(remote);
                    setDeleteOpen(true);
                  }}
                >
                  <Trash2 />
                </Button>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
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
        help={t("remotes.urlHint")}
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
        help={t(remote ? "remotes.tokenSaved" : "remotes.tokenHint")}
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
