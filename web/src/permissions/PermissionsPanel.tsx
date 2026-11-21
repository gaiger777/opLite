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
import { usePrincipal } from "@/auth/AuthProvider";
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
  grantable,
  grantError,
  grantTarget,
  objectParam,
  principalName,
  revokesSelf,
  roleName,
  rolePrivileges,
  targetLabel,
  targetName,
  useAccessRoles,
  useGroups,
} from "@/permissions/grants";
import { useFloatingIps, useRouters } from "@/routing/routing";

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

/** 라우터·유동 IP의 이름이 필요한 grant 목록인지(그 종류의 객체 grant가 있음 — 리뷰 2 웹 m6) */
export function routingGrants(grants: readonly Grant[] | undefined): {
  routers: boolean;
  fips: boolean;
} {
  const kinds = new Set(
    (grants ?? []).map((g) => (g.spec.target.type === "object" ? g.spec.target.kind : "")),
  );
  return { routers: kinds.has("router"), fips: kinds.has("floating-ip") };
}

/**
 * 객체 id → 이름(VM·네트워크·풀·라우터·유동 IP — 계획 66 리뷰 1 웹 M5). 라우터·유동 IP는 보기 목록이라 따로 읽으므로
 * `routing`이 그 종류를 원할 때만 읽는다(리뷰 2 웹 m6 — VM·풀 권한 패널마다 읽지 않게).
 */
export function useObjectNames(
  routing: { routers: boolean; fips: boolean } = { routers: false, fips: false },
): (kind: string, id: string) => string | undefined {
  const vms = useList("vm");
  const networks = useList("network");
  const pools = useList("storage-pool");
  // 라우터는 NB를 읽지 않는 목록이다. 둘 다 watch 사건이 오면 다시 읽는다.
  const routers = useRouters(routing.routers);
  const fips = useFloatingIps(routing.fips);
  return useMemo(() => {
    const names = new Map<string, string>();
    for (const [kind, list] of [
      ["vm", vms.data],
      ["network", networks.data],
      ["storage-pool", pools.data],
      ["router", routers.data],
      ["floating-ip", fips.data],
    ] as const) {
      for (const item of list ?? []) {
        names.set(`${kind}/${item.id}`, item.name);
      }
    }
    return (kind: string, id: string) => names.get(`${kind}/${id}`);
  }, [vms.data, networks.data, pools.data, routers.data, fips.data]);
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
  const grants = (who.data?.grants ?? []) as Grant[];
  const objectName = useObjectNames(routingGrants(grants));
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<Grant | null>(null);
  const headingId = useId();
  const changed = () => queryClient.invalidateQueries({ queryKey: ["permissions"] });
  const me = usePrincipal();
  // 내 접근의 근거를 빼면 이 탭과 grant 더하기를 잃을 수 있다. 이름을 넣어 확인한다.
  const self = removing !== null && revokesSelf(removing, me.user, { mine: mine.data?.grants });

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
                // 빼기도 서버가 위임 규칙(역할의 권한이 grant가 닿는 곳에 다 있을 것)으로 본다(리뷰 2).
                const removable = grantable(
                  perms,
                  rolePrivileges(g.spec.role, roles.data),
                  asTarget(g.spec.target, target),
                  g.spec.propagate !== false,
                );
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
          {/* 전역 역할 사용자 목록은 관리자에게만 온다(서버가 비워 보낸다, 리뷰 1). */}
          {!perms.has("cluster.admin") && (
            <p className="text-sm text-muted-foreground">{t("permissions.globalHidden")}</p>
          )}
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
        description={t(self ? "permissions.removeSelf" : "permissions.removeDescription")}
        typeToConfirm={
          self && removing ? targetName(removing.spec.target, folderPath, objectName, t) : undefined
        }
        confirmLabel={t("permissions.remove")}
        onConfirm={async () => {
          const grant = removing!;
          try {
            await call(api.DELETE("/grants/{id}", { params: { path: { id: grant.id } } }));
          } catch (err) {
            throw grantError(t, err);
          }
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
 * 상승을 막고(가진 권한보다 넓은 역할은 403, 그 설명을 보인다), 화면은 줄 수 있는 역할만 보인다.
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
  const [chosenRole, setRole] = useState<string | null>(null);
  const [propagate, setPropagate] = useState(true);
  const [error, setError] = useState<string | undefined>();
  const [targetIndex, setTargetIndex] = useState(0);
  const target = targets?.[targetIndex]?.target ?? initial;
  // 줄 수 있는 역할만(내가 이 대상에서 가진 권한 안의 역할, 서버가 같은 셈으로 막는다)
  const spread = target.type === "object" ? true : propagate;
  const offered = (roles.data ?? [])
    .filter((r) => r.builtin || r.id)
    .filter((r) => grantable(perms, r.privileges, target, spread))
    .map((r) => ({
      value: r.builtin ? `builtin:${r.name}` : `custom:${r.id}`,
      label: r.builtin ? t(`roles.${r.name}`) : r.name,
    }));
  // 고른 역할이 대상·propagate를 바꿔 줄 수 없게 되면 줄 수 있는 첫 역할로(운영자가 있으면 운영자)
  const role =
    chosenRole && offered.some((r) => r.value === chosenRole)
      ? chosenRole
      : ((offered.find((r) => r.value === "builtin:operator") ?? offered[0])?.value ?? "");
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
    if (!role) {
      return false;
    }
    const [type, value] = role.split(":") as ["builtin" | "custom", string];
    const roleRef: RoleRef =
      type === "builtin"
        ? { type: "builtin", role: value as "admin" | "operator" | "viewer" }
        : { type: "custom", id: value };
    try {
      await call(
        api.POST("/grants", {
          body: {
            principal: { type: kind, id },
            role: roleRef,
            target: grantTarget(target),
            propagate: spread,
          },
        }),
      );
    } catch (err) {
      throw grantError(t, err);
    }
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
      <Field
        id="grant-role"
        label={t("permissions.role")}
        error={roles.data && offered.length === 0 ? "permissions.noGrantableRole" : undefined}
      >
        {(a11y) => (
          <NativeSelect
            id="grant-role"
            className="w-full"
            value={role}
            onChange={(e) => setRole(e.target.value)}
            {...a11y}
          >
            {offered.map((r) => (
              <NativeSelectOption key={r.value} value={r.value}>
                {r.label}
              </NativeSelectOption>
            ))}
            {!roles.data && <NativeSelectOption value="">{t("common.loading")}</NativeSelectOption>}
          </NativeSelect>
        )}
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
