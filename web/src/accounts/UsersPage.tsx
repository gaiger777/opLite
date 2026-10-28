import { useQuery, useQueryClient } from "@tanstack/react-query";
import { MoreHorizontal, Plus } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { passwordErrors } from "@/accounts/password";
import { PasswordFields } from "@/accounts/PasswordFields";
import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field, type FieldA11y } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ReasonMenuItem } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
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
import { validName } from "@/vms/form";

type User = components["schemas"]["UserView"];
type Role = components["schemas"]["Role"];

const ROLES: readonly Role[] = ["admin", "operator", "viewer"];

/**
 * 사용자 목록. 감시할 수 없으므로 화면을 열 때와 바꾼 뒤에 다시 읽는다. 관리자가 아니면 읽을 수 없으므로
 * `enabled`를 내린다.
 */
export function useUsers(enabled = true) {
  return useQuery({
    queryKey: ["users"],
    queryFn: () => call(api.GET("/users")),
    select: (list) => list.items,
    enabled,
  });
}

type Dialog = { kind: "create" } | { kind: "role" | "password" | "delete"; user: User };

export function UsersPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const admin = can(me.role, "write", "user");
  const users = useUsers(admin);
  const queryClient = useQueryClient();
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const close = () => setDialog(null);
  // 메뉴에서 연 대화상자를 닫으면 메뉴 항목이 사라지므로, 메뉴를 연 버튼으로 포커스를 돌린다.
  const trigger = useRef<HTMLButtonElement | null>(null);
  const rememberTrigger = (e: React.SyntheticEvent<HTMLButtonElement>) => {
    trigger.current = e.currentTarget;
  };
  const changed = () => queryClient.invalidateQueries({ queryKey: ["users"] });

  // 메뉴에 없지만 주소로 직접 들어올 수 있다. 관리자가 아니면 읽지 않고 안내한다.
  if (!admin) {
    return (
      <div className="flex flex-col gap-4">
        <h1 className="text-2xl font-semibold">{t("nav.users")}</h1>
        <p className="text-sm text-muted-foreground">{t("users.adminOnly")}</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="users-title" className="text-2xl font-semibold">
          {t("nav.users")}
        </h1>
        <Button onClick={() => setDialog({ kind: "create" })}>
          <Plus />
          {t("common.create")}
        </Button>
      </div>
      <Table aria-labelledby="users-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("users.role")}</TableHead>
            <TableHead>
              <span className="sr-only">{t("vms.actions")}</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {(users.data ?? []).length === 0 && (
            <TableMessage colSpan={3} query={users} empty={t("users.empty")} />
          )}
          {(users.data ?? []).map((user) => {
            // 자기 비밀번호는 현재 비밀번호가 필요하므로 계정 메뉴에서 바꾼다. 자기 자신은 지울 수 없다.
            // 자기 역할도 바꾸지 않는다(낮추면 이 화면의 권한이 옛 역할로 남는다).
            const self = user.id === me.user;
            return (
              <TableRow key={user.id}>
                <TableCell className="font-medium">
                  {user.name}
                  {self && (
                    <span className="ml-2 text-xs text-muted-foreground">{t("users.you")}</span>
                  )}
                </TableCell>
                <TableCell>{t(`roles.${user.role}`)}</TableCell>
                <TableCell className="text-right">
                  <DropdownMenu modal={false}>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("vms.actionsFor", { name: user.name })}
                        onPointerDown={rememberTrigger}
                        onKeyDown={rememberTrigger}
                      >
                        <MoreHorizontal />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <ReasonMenuItem
                        reason={self ? "users.reason.selfRole" : null}
                        onSelect={() => setDialog({ kind: "role", user })}
                      >
                        {t("users.changeRole")}
                      </ReasonMenuItem>
                      <ReasonMenuItem
                        reason={self ? "users.reason.selfPassword" : null}
                        onSelect={() => setDialog({ kind: "password", user })}
                      >
                        {t("users.resetPassword")}
                      </ReasonMenuItem>
                      <DropdownMenuSeparator />
                      <ReasonMenuItem
                        variant="destructive"
                        reason={self ? "users.reason.selfDelete" : null}
                        onSelect={() => setDialog({ kind: "delete", user })}
                      >
                        {t("common.delete")}
                      </ReasonMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      <p className="text-sm text-muted-foreground">{t("users.selfNote")}</p>
      {dialog?.kind === "create" && <CreateUserDialog onClose={close} onDone={changed} />}
      {dialog?.kind === "role" && (
        <RoleDialog user={dialog.user} returnFocus={trigger} onClose={close} onDone={changed} />
      )}
      {dialog?.kind === "password" && (
        <ResetPasswordDialog user={dialog.user} returnFocus={trigger} onClose={close} />
      )}
      <ConfirmDialog
        open={dialog?.kind === "delete"}
        onOpenChange={(open) => !open && close()}
        title={t("users.deleteTitle", { name: dialog?.kind === "delete" ? dialog.user.name : "" })}
        description={t("users.deleteDescription")}
        confirmLabel={t("common.delete")}
        returnFocus={trigger}
        onConfirm={async () => {
          if (dialog?.kind !== "delete") {
            return;
          }
          const { user } = dialog;
          await call(api.DELETE("/users/{key}", { params: { path: { key: user.id } } }));
          toast.success(t("users.deleted", { name: user.name }));
          await changed();
        }}
      />
    </div>
  );
}

function RoleSelect({
  value,
  onChange,
  a11y,
}: {
  value: Role;
  onChange(role: Role): void;
  a11y: FieldA11y;
}) {
  const { t } = useTranslation();
  return (
    <NativeSelect
      id="user-role"
      className="w-full"
      value={value}
      onChange={(e) => onChange(e.target.value as Role)}
      {...a11y}
    >
      {ROLES.map((role) => (
        <NativeSelectOption key={role} value={role}>
          {t(`roles.${role}`)}
        </NativeSelectOption>
      ))}
    </NativeSelect>
  );
}

function CreateUserDialog({ onClose, onDone }: { onClose(): void; onDone(): Promise<void> }) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [role, setRole] = useState<Role>("viewer");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found = passwordErrors(password, confirm);
    if (!validName(name)) {
      found.name = "forms.name";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    await call(api.POST("/users", { body: { name, role, password } }));
    toast.success(t("users.created", { name }));
    await onDone();
  }

  return (
    <FormDialog
      title={t("users.createTitle")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="user-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="user-name"
            autoComplete="off"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field id="user-role" label={t("users.role")} hint={t("users.roleHint")}>
        {(a11y) => <RoleSelect value={role} onChange={setRole} a11y={a11y} />}
      </Field>
      <PasswordFields
        id="user"
        password={password}
        confirm={confirm}
        errors={errors}
        onPassword={setPassword}
        onConfirm={setConfirm}
      />
    </FormDialog>
  );
}

function RoleDialog({
  user,
  returnFocus,
  onClose,
  onDone,
}: {
  user: User;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onDone(): Promise<void>;
}) {
  const { t } = useTranslation();
  const [role, setRole] = useState<Role>(user.role);

  async function submit() {
    await call(api.PUT("/users/{key}", { params: { path: { key: user.id } }, body: { role } }));
    toast.success(t("users.roleChanged", { name: user.name, role: t(`roles.${role}`) }));
    await onDone();
  }

  return (
    <FormDialog
      title={t("users.roleTitle", { name: user.name })}
      submitLabel={t("common.save")}
      submitDisabled={role === user.role}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="user-role" label={t("users.role")} hint={t("users.roleHint")}>
        {(a11y) => <RoleSelect value={role} onChange={setRole} a11y={a11y} />}
      </Field>
    </FormDialog>
  );
}

function ResetPasswordDialog({
  user,
  returnFocus,
  onClose,
}: {
  user: User;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found = passwordErrors(password, confirm);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    await call(api.PUT("/users/{key}", { params: { path: { key: user.id } }, body: { password } }));
    toast.success(t("users.passwordReset", { name: user.name }));
  }

  return (
    <FormDialog
      title={t("users.passwordTitle", { name: user.name })}
      description={t("users.passwordDescription")}
      submitLabel={t("common.save")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <PasswordFields
        id="reset"
        password={password}
        confirm={confirm}
        errors={errors}
        onPassword={setPassword}
        onConfirm={setConfirm}
      />
    </FormDialog>
  );
}
