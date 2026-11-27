// 설정 › 권한·역할·그룹(계획 62 설계 §13 웹): 모든 grant 표, 사용자 정의 역할(권한 체크 표), 로컬 그룹과 외부 그룹 연결.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus, ShieldCheck, Trash2, Users } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useUsers } from "@/accounts/UsersPage";
import { api, ApiError, call } from "@/api/client";
import { useList } from "@/api/live";
import type { AccessRole, ExternalGroupRef, Grant, Group } from "@/api/types";
import { PRIVILEGES, type Privilege, type Target } from "@/auth/permissions";
import { usePrincipal } from "@/auth/AuthProvider";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { EmptyState } from "@/components/EmptyState";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { InfoButton } from "@/components/InfoButton";
import { PageHeader } from "@/components/PageHeader";
import { TableMessage } from "@/components/TableMessage";
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
import { folderPath, folderTree } from "@/folders/folders";
import { useFolderPaths } from "@/folders/useFolderPaths";
import {
  grantError,
  principalName,
  revokesSelf,
  roleName,
  targetLabel,
  targetName,
  useAccessRoles,
  useGroups,
} from "@/permissions/grants";
import {
  AddGrantDialog,
  routingGrants,
  useObjectNames,
  usePrincipalNames,
} from "@/permissions/PermissionsPanel";
import { validName } from "@/vms/form";

/** 디렉터리 그룹 이름의 최대 길이(서버 `MAX_EXTERNAL_GROUP`) */
const MAX_EXTERNAL_GROUP = 1024;

/** 관리자가 아니면 읽지 않고 안내한다(주소로 바로 들어올 수 있다). */
function AdminOnly({ title }: { title: string }) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={title} />
      <p className="text-sm text-muted-foreground">{t("users.adminOnly")}</p>
    </div>
  );
}

/** 설정 › 권한: 클러스터의 모든 grant(루트·폴더·객체)와 더하기·빼기 */
export function GrantsPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const admin = perms.has("cluster.admin");
  const queryClient = useQueryClient();
  const grants = useQuery({
    queryKey: ["grants"],
    queryFn: () => call(api.GET("/grants")),
    select: (list) => list.items,
    enabled: admin,
  });
  const roles = useAccessRoles();
  const names = usePrincipalNames();
  const paths = useFolderPaths();
  const objectName = useObjectNames(routingGrants(grants.data));
  const folders = useList("folder");
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<Grant | null>(null);
  const me = usePrincipal();
  const groups = useGroups(admin);
  // 루트에서 내 권한의 근거(디렉터리 그룹 연결로 든 그룹의 grant도 들어 있다, 리뷰 2)
  const mine = useQuery({
    queryKey: ["permissions", "effective", "root"],
    queryFn: () =>
      call(api.GET("/permissions/effective", { params: { query: { object: "root" } } })),
    enabled: admin,
  });
  // 나(나의 그룹)에게 건 grant를 빼면 대상 이름을 넣어 확인한다.
  const self =
    removing !== null &&
    revokesSelf(removing, me.user, { mine: mine.data?.grants, groups: groups.data });
  const changed = () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: ["grants"] }),
      queryClient.invalidateQueries({ queryKey: ["permissions"] }),
    ]);
  // 더할 대상: 루트와 모든 폴더(객체 grant는 그 객체의 권한 탭에서)
  const choices = useMemo(() => {
    const out: { target: Target; label: string }[] = [
      { target: { type: "root" }, label: t("permissions.rootTarget") },
    ];
    for (const kind of ["vm", "network", "storage"] as const) {
      const tree = folderTree(folders.data, kind);
      for (const f of tree.folders) {
        const path = folderPath(tree, f.id);
        out.push({
          target: { type: "folder", id: f.id },
          label: kind === "vm" ? path : `${kind}:${path}`,
        });
      }
    }
    return out;
  }, [folders.data, t]);

  if (!admin) {
    return <AdminOnly title={t("nav.permissions")} />;
  }
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="grants-title"
        title={t("nav.permissions")}
        help="permissions"
        intro={t("help.permissions.intro")}
        actions={
          <Button onClick={() => setAdding(true)}>
            <Plus />
            {t("permissions.add")}
          </Button>
        }
      />
      {grants.isSuccess && grants.data.length === 0 ? (
        <EmptyState
          icon={ShieldCheck}
          title={t("permissions.emptyAll")}
          description={t("help.permissions.empty")}
          help="permissions.page"
          action={
            <Button onClick={() => setAdding(true)}>
              <Plus />
              {t("permissions.add")}
            </Button>
          }
        />
      ) : (
        <Table aria-labelledby="grants-title">
          <TableHeader>
            <TableRow>
              <TableHead>{t("permissions.principal")}</TableHead>
              <TableHead>{t("permissions.role")}</TableHead>
              <TableHead>{t("permissions.target")}</TableHead>
              <TableHead>{t("permissions.propagateShort")}</TableHead>
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {(grants.data ?? []).length === 0 && (
              <TableMessage colSpan={5} query={grants} empty={t("permissions.emptyAll")} />
            )}
            {(grants.data ?? []).map((g) => {
              const who = principalName(g.spec.principal, names);
              return (
                <TableRow key={g.id}>
                  <TableCell className="font-medium">
                    {who}
                    {g.spec.principal.type === "group" && (
                      <span className="ml-2 text-xs text-muted-foreground">
                        {t("permissions.group")}
                      </span>
                    )}
                  </TableCell>
                  <TableCell>{roleName(g.spec.role, roles.data, t)}</TableCell>
                  <TableCell>{targetLabel(g.spec.target, paths, objectName, t)}</TableCell>
                  <TableCell>
                    {g.spec.propagate === false ? t("common.no") : t("common.yes")}
                  </TableCell>
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("permissions.removeNamed", { name: who })}
                      onClick={() => setRemoving(g)}
                    >
                      <Trash2 />
                    </Button>
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
      {adding && (
        <AddGrantDialog
          target={{ type: "root" }}
          label={t("permissions.rootTarget")}
          choices={choices}
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
          self && removing ? targetName(removing.spec.target, paths, objectName, t) : undefined
        }
        confirmLabel={t("permissions.remove")}
        onConfirm={async () => {
          try {
            await call(api.DELETE("/grants/{id}", { params: { path: { id: removing!.id } } }));
          } catch (err) {
            throw grantError(t, err);
          }
          toast.success(t("permissions.removed"));
          await changed();
        }}
      />
    </div>
  );
}

/** 권한을 묶음(접두사)별로 */
const PRIVILEGE_GROUPS: { id: string; privileges: Privilege[] }[] = (() => {
  const groups = new Map<string, Privilege[]>();
  for (const p of PRIVILEGES) {
    const head = p.split(".")[0]!;
    groups.set(head, [...(groups.get(head) ?? []), p]);
  }
  return [...groups].map(([id, privileges]) => ({ id, privileges }));
})();

/** 설정 › 역할: 내장 역할(읽기만)과 사용자 정의 역할(권한 체크 표) */
export function RolesPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const admin = perms.has("cluster.admin");
  const queryClient = useQueryClient();
  const roles = useAccessRoles();
  const [editing, setEditing] = useState<AccessRole | "new" | null>(null);
  const [removing, setRemoving] = useState<AccessRole | null>(null);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["access-roles"] });

  if (!admin) {
    return <AdminOnly title={t("nav.roles")} />;
  }
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="roles-title"
        title={t("nav.roles")}
        help="roles"
        intro={t("help.roles.intro")}
        actions={
          <Button onClick={() => setEditing("new")}>
            <Plus />
            {t("common.create")}
          </Button>
        }
      />
      {roles.isSuccess && roles.data.length === 0 ? (
        <EmptyState
          icon={ShieldCheck}
          title={t("roleEditor.empty")}
          description={t("help.roles.empty")}
          help="roles.page"
          action={
            <Button onClick={() => setEditing("new")}>
              <Plus />
              {t("roleEditor.createTitle")}
            </Button>
          }
        />
      ) : (
        <Table aria-labelledby="roles-title">
          <TableHeader>
            <TableRow>
              <TableHead>{t("common.name")}</TableHead>
              <TableHead>{t("roleEditor.privileges")}</TableHead>
              <TableHead>{t("roleEditor.description")}</TableHead>
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {(roles.data ?? []).length === 0 && (
              <TableMessage colSpan={4} query={roles} empty={t("roleEditor.empty")} />
            )}
            {(roles.data ?? []).map((role) => (
              <TableRow key={role.id ?? role.name}>
                <TableCell className="font-medium">
                  {role.builtin ? t(`roles.${role.name}`) : role.name}
                  {role.builtin && (
                    <span className="ml-2 text-xs text-muted-foreground">
                      {t("roleEditor.builtin")}
                    </span>
                  )}
                </TableCell>
                <TableCell className="max-w-md text-xs break-words">
                  {role.privileges.join(", ")}
                </TableCell>
                <TableCell className="text-sm">{role.description}</TableCell>
                <TableCell className="text-right whitespace-nowrap">
                  {!role.builtin && (
                    <>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("roleEditor.editNamed", { name: role.name })}
                        onClick={() => setEditing(role)}
                      >
                        <Pencil />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("common.deleteNamed", { name: role.name })}
                        onClick={() => setRemoving(role)}
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
      )}
      {editing && (
        <RoleDialog
          role={editing === "new" ? null : editing}
          onClose={() => setEditing(null)}
          onDone={changed}
        />
      )}
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={t("roleEditor.deleteTitle", { name: removing?.name ?? "" })}
        description={t("roleEditor.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          await call(
            api.DELETE("/access-roles/{key}", { params: { path: { key: removing!.id! } } }),
          );
          toast.success(t("roleEditor.deleted", { name: removing!.name }));
          await changed();
        }}
      />
    </div>
  );
}

function RoleDialog({
  role,
  onClose,
  onDone,
}: {
  role: AccessRole | null;
  onClose(): void;
  onDone(): Promise<unknown>;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState(role?.name ?? "");
  const [description, setDescription] = useState(role?.description ?? "");
  const [chosen, setChosen] = useState<ReadonlySet<string>>(() => new Set(role?.privileges ?? []));
  const [errors, setErrors] = useState<Record<string, string>>({});
  // 이 웹이 모르는 권한(새 서버가 더한 것). 고를 칸이 없으니 그대로 두고 함께 보낸다(PUT은 목록을 바꿔 쓴다).
  const unknown = (role?.privileges ?? []).filter(
    (p) => !(PRIVILEGES as readonly string[]).includes(p),
  );
  const toggle = (p: string, on: boolean) =>
    setChosen((prev) => {
      const next = new Set(prev);
      if (on) {
        next.add(p);
      } else {
        next.delete(p);
      }
      return next;
    });

  async function submit() {
    const found: Record<string, string> = {};
    if (!role && (!validName(name) || ["admin", "operator", "viewer", "none"].includes(name))) {
      found.name = "roleEditor.badName";
    }
    if (chosen.size === 0) {
      found.privileges = "roleEditor.noPrivileges";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const privileges = [...PRIVILEGES.filter((p) => chosen.has(p)), ...unknown];
    if (role) {
      await call(
        api.PUT("/access-roles/{key}", {
          params: { path: { key: role.id! }, header: { "If-Match": role.revision } },
          body: { privileges, description },
        }),
      );
    } else {
      await call(api.POST("/access-roles", { body: { name, privileges, description } }));
    }
    toast.success(t("roleEditor.saved", { name }));
    await onDone();
  }

  return (
    <FormDialog
      title={role ? t("roleEditor.editTitle", { name: role.name }) : t("roleEditor.createTitle")}
      submitLabel={role ? t("common.save") : t("common.create")}
      className="sm:max-w-2xl"
      onClose={onClose}
      onSubmit={submit}
    >
      {!role && (
        <Field
          id="role-name"
          label={t("common.name")}
          error={errors.name}
          help={t("help.roles.name")}
        >
          {(a11y) => (
            <Input
              id="role-name"
              autoComplete="off"
              value={name}
              onChange={(e) => setName(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
      )}
      <Field
        id="role-description"
        label={t("roleEditor.description")}
        help={t("help.roles.description")}
      >
        {(a11y) => (
          <Input
            id="role-description"
            autoComplete="off"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <fieldset className="flex flex-col gap-3">
        <legend className="mb-2 flex items-center gap-1 text-sm font-medium">
          {t("roleEditor.privileges")}
          <InfoButton topic="roles.page" label={t("roleEditor.privileges")} />
        </legend>
        {errors.privileges && (
          <p role="alert" className="text-xs text-destructive">
            {t(errors.privileges)}
          </p>
        )}
        <div className="grid gap-3 sm:grid-cols-2">
          {PRIVILEGE_GROUPS.map((group) => (
            <div key={group.id} className="flex flex-col gap-1.5">
              <span className="text-xs font-medium text-muted-foreground uppercase">
                {group.id}
              </span>
              {group.privileges.map((p) => (
                <div key={p} className="flex items-center gap-2">
                  <Checkbox
                    id={`priv-${p}`}
                    checked={chosen.has(p)}
                    onCheckedChange={(v) => toggle(p, v === true)}
                  />
                  <Label htmlFor={`priv-${p}`} className="font-mono text-xs">
                    {p}
                  </Label>
                </div>
              ))}
            </div>
          ))}
        </div>
        {unknown.length > 0 && (
          <p className="text-xs text-muted-foreground">
            {t("roleEditor.kept", { privileges: unknown.join(", ") })}
          </p>
        )}
      </fieldset>
    </FormDialog>
  );
}

/** 설정 › 그룹: 로컬 멤버와 연결한 디렉터리 그룹(LDAP·OIDC 영역의 그룹 이름) */
export function GroupsPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const admin = perms.has("cluster.admin");
  const queryClient = useQueryClient();
  const groups = useGroups(admin);
  const users = useUsers(admin);
  const [editing, setEditing] = useState<Group | "new" | null>(null);
  const [removing, setRemoving] = useState<Group | null>(null);
  const userName = useMemo(() => new Map(users.data?.map((u) => [u.id, u.name])), [users.data]);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["groups"] });

  if (!admin) {
    return <AdminOnly title={t("nav.groups")} />;
  }
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="groups-title"
        title={t("nav.groups")}
        help="groups"
        intro={t("help.groups.intro")}
        actions={
          <Button onClick={() => setEditing("new")}>
            <Plus />
            {t("common.create")}
          </Button>
        }
      />
      {groups.isSuccess && groups.data.length === 0 ? (
        <EmptyState
          icon={Users}
          title={t("groupEditor.empty")}
          description={t("help.groups.empty")}
          help="groups.page"
          action={
            <Button onClick={() => setEditing("new")}>
              <Plus />
              {t("groupEditor.createTitle")}
            </Button>
          }
        />
      ) : (
        <Table aria-labelledby="groups-title">
          <TableHeader>
            <TableRow>
              <TableHead>{t("common.name")}</TableHead>
              <TableHead>{t("groupEditor.members")}</TableHead>
              <TableHead>{t("groupEditor.external")}</TableHead>
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {(groups.data ?? []).length === 0 && (
              <TableMessage colSpan={4} query={groups} empty={t("groupEditor.empty")} />
            )}
            {(groups.data ?? []).map((g) => (
              <TableRow key={g.id}>
                <TableCell className="font-medium">
                  {g.name}
                  {g.spec.description && (
                    <div className="text-xs text-muted-foreground">{g.spec.description}</div>
                  )}
                </TableCell>
                <TableCell className="text-sm">
                  {(g.spec.members ?? [])
                    .map((id) => userName.get(id) ?? id.slice(-8))
                    .join(", ") || "—"}
                </TableCell>
                <TableCell className="text-sm">
                  {(g.spec.external ?? []).map((e) => `${e.realm}: ${e.group}`).join(", ") || "—"}
                </TableCell>
                <TableCell className="text-right whitespace-nowrap">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("groupEditor.editNamed", { name: g.name })}
                    onClick={() => setEditing(g)}
                  >
                    <Pencil />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("common.deleteNamed", { name: g.name })}
                    onClick={() => setRemoving(g)}
                  >
                    <Trash2 />
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {editing && (
        <GroupDialog
          group={editing === "new" ? null : editing}
          onClose={() => setEditing(null)}
          onDone={changed}
        />
      )}
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={t("groupEditor.deleteTitle", { name: removing?.name ?? "" })}
        description={t("groupEditor.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          // `group_in_use`는 HA 그룹과 같은 코드라 여기서 문구를 고른다.
          await call(
            api.DELETE("/groups/{key}", { params: { path: { key: removing!.id } } }),
          ).catch((e: unknown) => {
            throw e instanceof ApiError && e.code === "group_in_use"
              ? new Error(t("groupEditor.inUse", { message: e.message }))
              : e;
          });
          toast.success(t("groupEditor.deleted", { name: removing!.name }));
          await changed();
        }}
      />
    </div>
  );
}

function GroupDialog({
  group,
  onClose,
  onDone,
}: {
  group: Group | null;
  onClose(): void;
  onDone(): Promise<unknown>;
}) {
  const { t } = useTranslation();
  const users = useUsers();
  const realms = useQuery({
    queryKey: ["realms"],
    queryFn: () => call(api.GET("/realms")),
    select: (list) => list.items,
  });
  const [name, setName] = useState(group?.name ?? "");
  const [description, setDescription] = useState(group?.spec.description ?? "");
  const [members, setMembers] = useState<ReadonlySet<string>>(
    () => new Set(group?.spec.members ?? []),
  );
  const [external, setExternal] = useState<ExternalGroupRef[]>(group?.spec.external ?? []);
  const [errors, setErrors] = useState<Record<string, string>>({});
  // 지운 영역(목록을 읽은 뒤에만 안다). 선택 칸에 "(지워짐)"으로 보이고 저장을 막는다(리뷰 2).
  const deleted = (realm: string) =>
    realm !== "" && realms.isSuccess && !realms.data.some((r) => r.name === realm);

  async function submit() {
    const found: Record<string, string> = {};
    if (!group && !validName(name)) {
      found.name = "forms.name";
    }
    if (external.some((e) => !e.realm || !e.group.trim())) {
      found.external = "groupEditor.badExternal";
    } else if (external.some((e) => deleted(e.realm))) {
      found.external = "groupEditor.deletedRealm";
    } else if (external.some((e) => e.group.trim().length > MAX_EXTERNAL_GROUP)) {
      found.external = "groupEditor.longExternal";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const spec = {
      members: [...members],
      external: external.map((e) => ({ realm: e.realm, group: e.group.trim() })),
      description,
    };
    if (group) {
      await call(
        api.PUT("/groups/{key}", {
          params: { path: { key: group.id }, header: { "If-Match": group.revision } },
          body: { spec },
        }),
      );
    } else {
      await call(api.POST("/groups", { body: { name, spec } }));
    }
    toast.success(t("groupEditor.saved", { name }));
    await onDone();
  }

  return (
    <FormDialog
      title={
        group ? t("groupEditor.editTitle", { name: group.name }) : t("groupEditor.createTitle")
      }
      submitLabel={group ? t("common.save") : t("common.create")}
      className="sm:max-w-xl"
      onClose={onClose}
      onSubmit={submit}
    >
      {!group && (
        <Field
          id="group-name"
          label={t("common.name")}
          error={errors.name}
          help={t("help.groups.name")}
        >
          {(a11y) => (
            <Input
              id="group-name"
              autoComplete="off"
              value={name}
              onChange={(e) => setName(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
      )}
      <Field
        id="group-description"
        label={t("roleEditor.description")}
        help={t("help.groups.description")}
      >
        {(a11y) => (
          <Input
            id="group-description"
            autoComplete="off"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <fieldset className="flex flex-col gap-1.5">
        <legend className="mb-1 text-sm font-medium">{t("groupEditor.members")}</legend>
        <div className="flex max-h-40 flex-col gap-1.5 overflow-y-auto">
          {(users.data ?? []).map((u) => (
            <div key={u.id} className="flex items-center gap-2">
              <Checkbox
                id={`member-${u.id}`}
                checked={members.has(u.id)}
                onCheckedChange={(v) =>
                  setMembers((prev) => {
                    const next = new Set(prev);
                    if (v === true) {
                      next.add(u.id);
                    } else {
                      next.delete(u.id);
                    }
                    return next;
                  })
                }
              />
              <Label htmlFor={`member-${u.id}`}>{u.name}</Label>
            </div>
          ))}
        </div>
      </fieldset>
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-1 text-sm font-medium">{t("groupEditor.external")}</legend>
        <p className="text-xs text-muted-foreground">{t("groupEditor.externalHint")}</p>
        {errors.external && (
          <p role="alert" className="text-xs text-destructive">
            {t(errors.external)}
          </p>
        )}
        {external.map((e, i) => (
          <div key={i} className="flex items-center gap-2">
            <NativeSelect
              aria-label={t("groupEditor.realm")}
              value={e.realm}
              onChange={(ev) =>
                setExternal(
                  external.map((x, j) => (j === i ? { ...x, realm: ev.target.value } : x)),
                )
              }
            >
              <NativeSelectOption value="">{t("groupEditor.realm")}</NativeSelectOption>
              {deleted(e.realm) && (
                <NativeSelectOption value={e.realm}>
                  {t("common.deletedName", { name: e.realm })}
                </NativeSelectOption>
              )}
              {(realms.data ?? []).map((r) => (
                <NativeSelectOption key={r.name} value={r.name}>
                  {r.name}
                </NativeSelectOption>
              ))}
            </NativeSelect>
            <Input
              aria-label={t("groupEditor.groupName")}
              placeholder="cn=team-a,ou=groups,dc=example,dc=com"
              value={e.group}
              onChange={(ev) =>
                setExternal(
                  external.map((x, j) => (j === i ? { ...x, group: ev.target.value } : x)),
                )
              }
            />
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={t("groupEditor.removeExternal")}
              onClick={() => setExternal(external.filter((_, j) => j !== i))}
            >
              <Trash2 />
            </Button>
          </div>
        ))}
        <div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setExternal([...external, { realm: "", group: "" }])}
          >
            <Plus />
            {t("groupEditor.addExternal")}
          </Button>
        </div>
      </fieldset>
    </FormDialog>
  );
}
