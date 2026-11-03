import { Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { SecurityGroup, Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { MAX_DESCRIPTION } from "@/security-groups/rules";
import { validName } from "@/vms/form";

/** 그룹을 NIC에 건 VM 수 */
export function groupUsers(group: SecurityGroup, vms: Vm[]): number {
  return vms.filter((vm) => vm.spec.nics?.some((nic) => nic.security_groups?.includes(group.name)))
    .length;
}

export function SecurityGroupsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const groups = useList("security-group");
  const vms = useList("vm");
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<SecurityGroup | null>(null);
  const write = can(me.role, "write", "security-group");

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="security-groups-title" className="text-2xl font-semibold">
          {t("nav.securityGroups")}
        </h1>
        {write && (
          <Button onClick={() => setCreating(true)}>
            <Plus />
            {t("common.create")}
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">{t("securityGroups.note")}</p>
      <Table aria-labelledby="security-groups-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("securityGroups.rules")}</TableHead>
            <TableHead>{t("securityGroups.description")}</TableHead>
            <TableHead>{t("securityGroups.vms")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(groups.data ?? []).length === 0 && (
            <TableMessage
              colSpan={write ? 5 : 4}
              query={groups}
              empty={t("securityGroups.empty")}
            />
          )}
          {(groups.data ?? []).map((group) => {
            // VM 목록을 다 읽기 전에는 모른다.
            const used = vms.isSuccess ? groupUsers(group, vms.data) : undefined;
            return (
              <TableRow key={group.id}>
                <TableCell>
                  <Link
                    to={`/security-groups/${encodeURIComponent(group.name)}`}
                    className="font-medium underline-offset-4 hover:underline"
                  >
                    {group.name}
                  </Link>
                </TableCell>
                <TableCell>{group.spec.rules?.length ?? 0}</TableCell>
                <TableCell className="max-w-xs truncate">{group.spec.description ?? "—"}</TableCell>
                <TableCell>
                  {used ?? "—"}
                  {write && used !== undefined && used > 0 && (
                    <div className="text-xs text-muted-foreground">{t("securityGroups.inUse")}</div>
                  )}
                </TableCell>
                {write && (
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: group.name })}
                      // VM NIC가 쓰는 그룹은 서버가 지우지 않는다.
                      disabled={used !== 0}
                      onClick={() => setDeleting(group)}
                    >
                      <Trash2 />
                    </Button>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && <CreateGroupDialog onClose={() => setCreating(false)} />}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("securityGroups.deleteTitle", { name: deleting?.name })}
        description={t("securityGroups.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const group = deleting!;
          await call(api.DELETE("/security-groups/{key}", { params: { path: { key: group.id } } }));
          toast.success(t("securityGroups.deleted", { name: group.name }));
        }}
      />
    </div>
  );
}

/** 이름과 설명으로 빈 그룹을 만들고, 규칙을 적도록 그 그룹의 화면으로 간다. */
function CreateGroupDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    if (!validName(name)) {
      found.name = "forms.name";
    }
    if (description.length > MAX_DESCRIPTION) {
      found.description = "securityGroups.errors.description";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const spec = { rules: [], ...(description !== "" && { description }) };
    await call(api.POST("/security-groups", { body: { name, spec } }));
    toast.success(t("securityGroups.created", { name }));
    void navigate(`/security-groups/${encodeURIComponent(name)}`);
  }

  return (
    <FormDialog
      title={t("securityGroups.createTitle")}
      description={t("securityGroups.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="group-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input id="group-name" value={name} onChange={(e) => setName(e.target.value)} {...a11y} />
        )}
      </Field>
      <Field
        id="group-description"
        label={t("securityGroups.descriptionOptional")}
        error={errors.description}
      >
        {(a11y) => (
          <Input
            id="group-description"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}
