import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
import { useId, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useUsers } from "@/accounts/UsersPage";
import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Grant, GrantTarget, PrincipalRef, RoleRef } from "@/api/types";
import { type Target } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { useFolderPaths } from "@/folders/useFolderPaths";
import {
  grantTarget,
  objectParam,
  principalName,
  roleName,
  targetLabel,
  useAccessRoles,
  useGroups,
} from "@/permissions/grants";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** 사용자·그룹 id → 이름(읽을 수 있는 것만). 위임자는 사용자·그룹 목록을 읽지 못한다. */
export function usePrincipalNames(): ReadonlyMap<string, string> {
  const perms = usePermissions();
  const admin = perms.has("cluster.admin");
  const users = useUsers(admin);
  const groups = useGroups(admin);
  return useMemo(
    () =>
      new Map([
        ...(users.data ?? []).map((u) => [u.id, u.name] as const),
        ...(groups.data ?? []).map((g) => [g.id, g.name] as const),
      ]),
    [users.data, groups.data],
  );
}

/** 객체 id → 이름(VM·네트워크·풀) */
export function useObjectNames(): (kind: string, id: string) => string | undefined {
  const vms = useList("vm");
  const networks = useList("network");
  const pools = useList("storage-pool");
  return useMemo(() => {
    const names = new Map<string, string>();
    for (const [kind, list] of [
      ["vm", vms.data],
      ["network", networks.data],
      ["storage-pool", pools.data],
    ] as const) {
      for (const item of list ?? []) {
        names.set(`${kind}/${item.id}`, item.name);
      }
    }
    return (kind: string, id: string) => names.get(`${kind}/${id}`);
  }, [vms.data, networks.data, pools.data]);
}

/** grant 대상을 화면의 대상으로(지우기 권한을 볼 때). 객체 grant는 이 패널의 객체다. */
function asTarget(target: GrantTarget, here: Target): Target {
  switch (target.type) {
    case "root":
      return { type: "root" };
    case "folder":
      return { type: "folder", id: target.id };
    case "object":
      return here;
  }
}

/**
 * 객체·폴더의 "권한" 탭(설계 §13 웹): 내 유효 권한과 근거, `permission.modify`가 있으면 누가 접근하는지
 * (`/permissions/who`: 이 대상과 조상에 건 grant, 전역 역할 사용자)와 grant 더하기·빼기.
 */
export function PermissionsPanel({ target, label }: { target: Target; label: string }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const object = objectParam(target);
  const manage = perms.has("permission.modify", target);
  const mine = useQuery({
    queryKey: ["permissions", "effective", object],
    queryFn: () => call(api.GET("/permissions/effective", { params: { query: { object } } })),
  });
  const who = useQuery({
    queryKey: ["permissions", "who", object],
    queryFn: () => call(api.GET("/permissions/who", { params: { query: { object } } })),
    enabled: manage,
  });
  const roles = useAccessRoles();
  const names = usePrincipalNames();
  const folderPath = useFolderPaths();
  const objectName = useObjectNames();
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<Grant | null>(null);
  const headingId = useId();
  const changed = () => queryClient.invalidateQueries({ queryKey: ["permissions"] });
  const grants = (who.data?.grants ?? []) as Grant[];

  return (
    <section
      className="flex flex-col gap-4 pt-2"
      aria-label={t("permissions.title", { name: label })}
    >
      <div className="flex flex-col gap-2">
        <h2 className="font-medium">{t("permissions.mine")}</h2>
        {mine.data ? (
          <ul className="flex flex-wrap gap-1" aria-label={t("permissions.mine")}>
            {mine.data.privileges.length === 0 && (
              <li className="text-sm text-muted-foreground">{t("permissions.none")}</li>
            )}
            {mine.data.privileges.map((p) => (
              <li key={p}>
                <Badge variant="outline" className="font-mono">
                  {p}
                </Badge>
              </li>
            ))}
          </ul>
        ) : (
          <p className="text-sm text-muted-foreground">
            {mine.isError ? t("permissions.loadFailed") : t("common.loading")}
          </p>
        )}
      </div>
      {manage && (
        <div className="flex flex-col gap-2">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h2 id={headingId} className="font-medium">
              {t("permissions.who")}
            </h2>
            <Button size="sm" onClick={() => setAdding(true)}>
              <Plus />
              {t("permissions.add")}
            </Button>
          </div>
          <Table aria-labelledby={headingId}>
            <TableHeader>
              <TableRow>
                <TableHead>{t("permissions.principal")}</TableHead>
                <TableHead>{t("permissions.role")}</TableHead>
                <TableHead>{t("permissions.from")}</TableHead>
                <TableHead>
                  <span className="sr-only">{t("vms.actions")}</span>
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {grants.length === 0 && (who.data?.users ?? []).length === 0 && (
                <TableMessage colSpan={4} query={who} empty={t("permissions.empty")} />
              )}
              {(who.data?.users ?? []).map((u) => (
                <TableRow key={`user-${u.id}`}>
                  <TableCell className="font-medium">{u.name}</TableCell>
                  <TableCell>{t(`roles.${u.role}`)}</TableCell>
                  <TableCell className="text-muted-foreground">
                    {t("permissions.globalRole")}
                  </TableCell>
                  <TableCell />
                </TableRow>
              ))}
              {grants.map((g) => {
                const p = g.spec.principal;
                const removable = perms.has("permission.modify", asTarget(g.spec.target, target));
                const who = principalName(p, names);
                return (
                  <TableRow key={g.id}>
                    <TableCell className="font-medium">
                      {who}
                      {p.type === "group" && (
                        <Badge variant="secondary" className="ml-2">
                          {t("permissions.group")}
                        </Badge>
                      )}
                    </TableCell>
                    <TableCell>
                      {roleName(g.spec.role, roles.data, t)}
                      {g.spec.propagate === false && (
                        <span className="ml-2 text-xs text-muted-foreground">
                          {t("permissions.noPropagate")}
                        </span>
                      )}
                    </TableCell>
                    <TableCell>{targetLabel(g.spec.target, folderPath, objectName, t)}</TableCell>
                    <TableCell className="text-right">
                      {removable && (
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={t("permissions.removeNamed", { name: who })}
                          onClick={() => setRemoving(g)}
                        >
                          <Trash2 />
                        </Button>
                      )}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
      )}
      {adding && (
        <AddGrantDialog
          target={target}
          label={label}
          known={grants.map((g) => g.spec.principal)}
          onClose={() => setAdding(false)}
          onDone={changed}
        />
      )}
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={t("permissions.removeTitle", {
          name: removing ? principalName(removing.spec.principal, names) : "",
        })}
        description={t("permissions.removeDescription")}
        confirmLabel={t("permissions.remove")}
        onConfirm={async () => {
          const grant = removing!;
          await call(api.DELETE("/grants/{id}", { params: { path: { id: grant.id } } }));
          toast.success(t("permissions.removed"));
          await changed();
        }}
      />
    </section>
  );
}

/**
 * grant 더하기. 주체는 사용자 목록을 읽을 수 있으면(admin) 사용자·그룹에서 고르고, 못 읽으면(폴더 위임자) 이미
 * 이 대상에 권한이 있는 주체에서 고르거나 id를 넣는다. 역할은 누구나 읽는 역할 목록에서 고른다. 서버가 위임
 * 상승을 막는다(가진 권한보다 넓은 역할은 403).
 */
export function AddGrantDialog({
  target: initial,
  label,
  known = [],
  choices: targets,
  onClose,
  onDone,
}: {
  target: Target;
  label: string;
  /** 대상을 고르게 한다(설정 › 권한의 더하기) */
  choices?: { target: Target; label: string }[];
  /** 이미 보이는 주체(위임자가 고를 수 있는 것) */
  known?: PrincipalRef[];
  onClose(): void;
  onDone(): Promise<unknown>;
}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const admin = perms.has("cluster.admin");
  const users = useUsers(admin);
  const groups = useGroups(admin);
  const roles = useAccessRoles();
  const names = usePrincipalNames();
  const [kind, setKind] = useState<PrincipalRef["type"]>("user");
  const [principal, setPrincipal] = useState("");
  const [role, setRole] = useState("builtin:operator");
  const [propagate, setPropagate] = useState(true);
  const [error, setError] = useState<string | undefined>();
  const [targetIndex, setTargetIndex] = useState(0);
  const target = targets?.[targetIndex]?.target ?? initial;
  const choices: { id: string; name: string }[] =
    kind === "user"
      ? admin
        ? (users.data ?? []).map((u) => ({ id: u.id, name: u.name }))
        : known
            .filter((p) => p.type === "user")
            .map((p) => ({ id: p.id, name: principalName(p, names) }))
      : admin
        ? (groups.data ?? []).map((g) => ({ id: g.id, name: g.name }))
        : known
            .filter((p) => p.type === "group")
            .map((p) => ({ id: p.id, name: principalName(p, names) }));
  // 목록을 읽지 못하면 id를 넣는다(이미 보이는 주체는 고를 수도 있다).
  const typed = !admin;
  const unique = [...new Map(choices.map((c) => [c.id, c])).values()];
  const listId = useId();

  async function submit() {
    const id = principal.trim();
    if (!UUID.test(id)) {
      setError("permissions.badPrincipal");
      return false;
    }
    const [type, value] = role.split(":") as ["builtin" | "custom", string];
    const roleRef: RoleRef =
      type === "builtin"
        ? { type: "builtin", role: value as "admin" | "operator" | "viewer" }
        : { type: "custom", id: value };
    await call(
      api.POST("/grants", {
        body: {
          principal: { type: kind, id },
          role: roleRef,
          target: grantTarget(target),
          propagate: target.type === "object" ? true : propagate,
        },
      }),
    );
    toast.success(t("permissions.added", { name: targets?.[targetIndex]?.label ?? label }));
    await onDone();
  }

  return (
    <FormDialog
      title={t("permissions.addTitle", { name: label })}
      description={t("permissions.addDescription")}
      submitLabel={t("permissions.add")}
      onClose={onClose}
      onSubmit={submit}
    >
      {targets && (
        <Field id="grant-target" label={t("permissions.target")}>
          <NativeSelect
            id="grant-target"
            className="w-full"
            value={targetIndex}
            onChange={(e) => setTargetIndex(Number(e.target.value))}
          >
            {targets.map((c, i) => (
              <NativeSelectOption key={i} value={i}>
                {c.label}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
      )}
      <Field id="grant-kind" label={t("permissions.principalKind")}>
        <NativeSelect
          id="grant-kind"
          className="w-full"
          value={kind}
          onChange={(e) => {
            setKind(e.target.value as PrincipalRef["type"]);
            setPrincipal("");
          }}
        >
          <NativeSelectOption value="user">{t("permissions.user")}</NativeSelectOption>
          <NativeSelectOption value="group">{t("permissions.group")}</NativeSelectOption>
        </NativeSelect>
      </Field>
      <Field
        id="grant-principal"
        label={t(kind === "user" ? "permissions.user" : "permissions.group")}
        error={error}
        hint={typed ? t("permissions.typedHint") : undefined}
      >
        {(a11y) =>
          typed ? (
            <>
              <Input
                id="grant-principal"
                list={listId}
                autoComplete="off"
                placeholder="00000000-0000-0000-0000-000000000000"
                value={principal}
                onChange={(e) => setPrincipal(e.target.value)}
                {...a11y}
              />
              <datalist id={listId}>
                {unique.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </datalist>
            </>
          ) : (
            <NativeSelect
              id="grant-principal"
              className="w-full"
              value={principal}
              onChange={(e) => setPrincipal(e.target.value)}
              {...a11y}
            >
              <NativeSelectOption value="">{t("permissions.choose")}</NativeSelectOption>
              {unique.map((c) => (
                <NativeSelectOption key={c.id} value={c.id}>
                  {c.name}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          )
        }
      </Field>
      <Field id="grant-role" label={t("permissions.role")}>
        <NativeSelect
          id="grant-role"
          className="w-full"
          value={role}
          onChange={(e) => setRole(e.target.value)}
        >
          {(roles.data ?? [])
            .filter((r) => r.builtin || r.id)
            .map((r) => {
              const value = r.builtin ? `builtin:${r.name}` : `custom:${r.id}`;
              return (
                <NativeSelectOption key={value} value={value}>
                  {r.builtin ? t(`roles.${r.name}`) : r.name}
                </NativeSelectOption>
              );
            })}
          {!roles.data && (
            <NativeSelectOption value="builtin:operator">{t("roles.operator")}</NativeSelectOption>
          )}
        </NativeSelect>
      </Field>
      {target.type !== "object" && (
        <div className="flex items-center gap-2">
          <Checkbox
            id="grant-propagate"
            checked={propagate}
            onCheckedChange={(v) => setPropagate(v === true)}
          />
          <Label htmlFor="grant-propagate">{t("permissions.propagate")}</Label>
        </div>
      )}
    </FormDialog>
  );
}
