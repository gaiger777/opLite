import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
import { useMemo, useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useUsers } from "@/accounts/UsersPage";
import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { CopyButton } from "@/components/CopyButton";
import { Field } from "@/components/Field";
import { focusBack } from "@/components/focusBack";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatTime, uuidTime } from "@/lib/format";
import { validName } from "@/vms/form";

type Token = components["schemas"]["TokenView"];
type CreatedToken = components["schemas"]["CreatedToken"];

/**
 * API 토큰. 누구나 자기 토큰을 만들고 지운다(토큰의 권한은 주인의 역할을 따른다). 관리자는 모든 토큰을
 * 보고 지울 수 있다. 감시할 수 없으므로 화면을 열 때와 바꾼 뒤에 다시 읽는다.
 */
export function TokensPage() {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const queryClient = useQueryClient();
  const tokens = useQuery({
    queryKey: ["tokens"],
    queryFn: () => call(api.GET("/tokens")),
    select: (list) => list.items,
  });
  // 남의 토큰을 다루는 것은 관리자뿐이다(서버의 may_manage와 같은 규칙).
  const admin = can(me.role, "write", "api-token");
  const users = useUsers(admin);
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<CreatedToken | null>(null);
  const [deleting, setDeleting] = useState<Token | null>(null);
  // 만들기·비밀값 대화상자를 닫으면 "만들기" 버튼으로 돌아간다.
  const opener = useRef<HTMLButtonElement>(null);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["tokens"] });
  const userName = useMemo(() => new Map(users.data?.map((u) => [u.id, u.name])), [users.data]);
  const owner = (token: Token) =>
    token.user === me.user ? me.name : (userName.get(token.user) ?? token.user.slice(-8));

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="tokens-title" className="text-2xl font-semibold">
          {t("nav.tokens")}
        </h1>
        <Button ref={opener} onClick={() => setCreating(true)}>
          <Plus />
          {t("common.create")}
        </Button>
      </div>
      <p className="text-sm text-muted-foreground">{t("tokens.note")}</p>
      <Table aria-labelledby="tokens-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            {admin && <TableHead>{t("tokens.owner")}</TableHead>}
            <TableHead>{t("tokens.created")}</TableHead>
            <TableHead>
              <span className="sr-only">{t("vms.actions")}</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {(tokens.data ?? []).length === 0 && (
            <TableMessage colSpan={admin ? 4 : 3} query={tokens} empty={t("tokens.empty")} />
          )}
          {(tokens.data ?? []).map((token) => {
            const time = uuidTime(token.id);
            return (
              <TableRow key={token.id}>
                <TableCell className="font-medium">{token.name}</TableCell>
                {admin && <TableCell>{owner(token)}</TableCell>}
                <TableCell className="whitespace-nowrap">
                  <time dateTime={time.toISOString()}>{formatTime(time, i18n.language)}</time>
                </TableCell>
                <TableCell className="text-right">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("common.deleteNamed", { name: token.name })}
                    onClick={() => setDeleting(token)}
                  >
                    <Trash2 />
                  </Button>
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && (
        <CreateTokenDialog
          returnFocus={opener}
          onClose={() => setCreating(false)}
          onCreated={(token) => {
            setCreated(token);
            void changed();
          }}
        />
      )}
      {created && (
        <SecretDialog created={created} returnFocus={opener} onClose={() => setCreated(null)} />
      )}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("tokens.deleteTitle", { name: deleting?.name })}
        description={t("tokens.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const token = deleting!;
          await call(api.DELETE("/tokens/{key}", { params: { path: { key: token.id } } }));
          toast.success(t("tokens.deleted", { name: token.name }));
          await changed();
        }}
      />
    </div>
  );
}

function CreateTokenDialog({
  returnFocus,
  onClose,
  onCreated,
}: {
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onCreated(token: CreatedToken): void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [error, setError] = useState<string | undefined>();

  async function submit() {
    const invalid = validName(name) ? undefined : "forms.name";
    setError(invalid);
    if (invalid) {
      return false;
    }
    onCreated(await call(api.POST("/tokens", { body: { name } })));
  }

  return (
    <FormDialog
      title={t("tokens.createTitle")}
      description={t("tokens.createDescription")}
      submitLabel={t("common.create")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="token-name" label={t("common.name")} error={error}>
        {(a11y) => (
          <Input
            id="token-name"
            autoComplete="off"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}

/** 만든 토큰의 비밀값. 서버는 다시 보여 주지 않으므로 "완료"로만 닫는다. */
function SecretDialog({
  created,
  returnFocus,
  onClose,
}: {
  created: CreatedToken;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent
        showCloseButton={false}
        onEscapeKeyDown={(e) => e.preventDefault()}
        onInteractOutside={(e) => e.preventDefault()}
        onCloseAutoFocus={(e) => focusBack(e, returnFocus)}
      >
        <DialogHeader>
          <DialogTitle>{t("tokens.secretTitle", { name: created.token.name })}</DialogTitle>
          <DialogDescription>{t("tokens.secretDescription")}</DialogDescription>
        </DialogHeader>
        <div className="flex items-center gap-2">
          <Input
            readOnly
            aria-label={t("tokens.secret")}
            className="font-mono text-xs"
            value={created.secret}
            onFocus={(e) => e.target.select()}
          />
          <CopyButton text={created.secret} label={t("tokens.copySecret")} />
        </div>
        <p className="text-xs text-muted-foreground">
          {t("tokens.usage")} <code>Authorization: Bearer …</code>
        </p>
        <DialogFooter>
          <Button onClick={onClose}>{t("common.done")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
