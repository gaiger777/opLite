import { Pencil, Plus, Trash2 } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { HaGroup } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
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
import {
  byPriority,
  checkGroupForm,
  emptyGroupForm,
  type GroupForm,
  groupFormOf,
  groupSpecOf,
  groupVms,
  sameGroupSpec,
} from "@/ha-groups/groups";

/** HA 그룹: 목록과 만들기·바꾸기·지우기. 누구나 읽고 admin만 바꾼다. */
export function HaGroupsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const groups = useList("ha-group");
  const nodes = useList("node");
  const vms = useList("vm");
  // 만들기는 `null`, 바꾸기는 그 그룹
  const [editing, setEditing] = useState<HaGroup | null | undefined>(undefined);
  const [deleting, setDeleting] = useState<HaGroup | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const write = can(me.role, "write", "ha-group");
  const list = [...(groups.data ?? [])].sort((a, b) => a.name.localeCompare(b.name));

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="ha-groups-title" className="text-2xl font-semibold">
          {t("nav.haGroups")}
        </h1>
        {write && (
          <div className="flex flex-wrap items-center gap-2 text-sm">
            {/* 노드 목록을 읽어야 그룹 노드를 고른다. */}
            {!nodes.isSuccess && <ListPending error={nodes.error} />}
            <Button
              disabled={!nodes.isSuccess || !groups.isSuccess}
              onClick={(e) => {
                opener.current = e.currentTarget;
                setEditing(null);
              }}
            >
              <Plus />
              {t("common.create")}
            </Button>
          </div>
        )}
      </div>
      <p className="text-sm text-muted-foreground">{t("haGroups.pageHint")}</p>
      <Table aria-labelledby="ha-groups-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("haGroups.nodes")}</TableHead>
            <TableHead>{t("haGroups.options")}</TableHead>
            <TableHead>{t("haGroups.vms")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 && (
            <TableMessage colSpan={write ? 5 : 4} query={groups} empty={t("haGroups.empty")} />
          )}
          {list.map((group) => {
            const users = groupVms(group, vms.data ?? []);
            return (
              <TableRow key={group.id}>
                <TableCell className="font-medium">{group.name}</TableCell>
                <TableCell>
                  {byPriority(group.spec.nodes)
                    .map((n) => t("haGroups.nodePriority", { node: n.node, n: n.priority ?? 0 }))
                    .join(", ")}
                </TableCell>
                <TableCell>
                  <div className="flex flex-wrap gap-1">
                    {group.spec.restricted && (
                      <Badge variant="outline">{t("haGroups.restricted")}</Badge>
                    )}
                    {group.spec.nofailback && (
                      <Badge variant="outline">{t("haGroups.nofailback")}</Badge>
                    )}
                    {!group.spec.restricted && !group.spec.nofailback && "—"}
                  </div>
                </TableCell>
                {/* VM 목록을 다 읽기 전에는 모른다. */}
                <TableCell>{vms.isSuccess ? users.join(", ") || "—" : "—"}</TableCell>
                {write && (
                  <TableCell className="text-right whitespace-nowrap">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("haGroups.editNamed", { name: group.name })}
                      onClick={(e) => {
                        opener.current = e.currentTarget;
                        setEditing(group);
                      }}
                    >
                      <Pencil />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("haGroups.deleteNamed", { name: group.name })}
                      onClick={(e) => {
                        opener.current = e.currentTarget;
                        setDeleting(group);
                        setDeleteOpen(true);
                      }}
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
      {editing !== undefined && (
        <GroupDialog
          group={editing}
          taken={(groups.data ?? []).map((g) => g.name)}
          returnFocus={opener}
          onClose={() => setEditing(undefined)}
        />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("haGroups.deleteTitle", { name: deleting?.name })}
        description={t("haGroups.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const group = deleting!;
          await call(api.DELETE("/ha-groups/{key}", { params: { path: { key: group.id } } }));
          toast.success(t("haGroups.deleted", { name: group.name }));
        }}
      />
    </div>
  );
}

/** 그룹 만들기(`group`이 null)와 바꾸기. 열 때만 그린다. */
function GroupDialog({
  group,
  taken,
  returnFocus,
  onClose,
}: {
  group: HaGroup | null;
  /** 있는 그룹 이름 */
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  // 위트니스는 VM을 돌리지 않는다. 지운 노드가 그룹에 남아 있으면 그 이름도 보인다(서버가 저장할 때 거절한다).
  const names = (useList("node").data ?? []).filter((n) => !n.spec.witness).map((n) => n.name);
  for (const n of group?.spec.nodes ?? []) {
    if (!names.includes(n.node)) {
      names.push(n.node);
    }
  }
  names.sort();
  const [form, setForm] = useState<GroupForm>(() =>
    group ? groupFormOf(group) : emptyGroupForm(),
  );
  const [errors, setErrors] = useState<Record<string, string>>({});
  const change = (part: Partial<GroupForm>) => setForm((f) => ({ ...f, ...part }));
  const unchanged = group !== null && sameGroupSpec(group.spec, groupSpecOf(form));

  function toggle(node: string, on: boolean) {
    const nodes = { ...form.nodes };
    if (on) {
      nodes[node] = "0";
    } else {
      delete nodes[node];
    }
    change({ nodes });
  }

  async function submit() {
    const found = checkGroupForm(form, group ? null : taken);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    if (group) {
      await call(
        api.PUT("/ha-groups/{key}", {
          params: { path: { key: group.id }, header: { "If-Match": group.revision } },
          body: { labels: group.labels, spec: groupSpecOf(form) },
        }),
      );
      toast.success(t("haGroups.saved", { name: group.name }));
    } else {
      await call(api.POST("/ha-groups", { body: { name: form.name, spec: groupSpecOf(form) } }));
      toast.success(t("haGroups.created", { name: form.name }));
    }
  }

  return (
    <FormDialog
      title={group ? t("haGroups.editTitle", { name: group.name }) : t("haGroups.createTitle")}
      description={t("haGroups.dialogDescription")}
      submitLabel={group ? t("common.save") : t("common.create")}
      submitDisabled={unchanged}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {!group && (
        <Field id="group-name" label={t("common.name")} error={errors.name}>
          {(a11y) => (
            <Input
              id="group-name"
              value={form.name}
              onChange={(e) => change({ name: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      <fieldset className="flex flex-col gap-1.5">
        <legend className="mb-1.5 text-sm font-medium">{t("haGroups.nodes")}</legend>
        {names.map((node) => {
          const on = node in form.nodes;
          return (
            <div key={node} className="flex items-center gap-2">
              <Checkbox
                id={`group-node-${node}`}
                checked={on}
                onCheckedChange={(v) => toggle(node, v === true)}
              />
              <Label htmlFor={`group-node-${node}`} className="min-w-24">
                {node}
              </Label>
              {on && (
                <Input
                  className="w-24"
                  inputMode="numeric"
                  placeholder="0"
                  aria-label={t("haGroups.priorityOf", { node })}
                  aria-invalid={errors.nodes ? true : undefined}
                  value={form.nodes[node]}
                  onChange={(e) => change({ nodes: { ...form.nodes, [node]: e.target.value } })}
                />
              )}
            </div>
          );
        })}
        {errors.nodes ? (
          <p className="text-xs text-destructive">{t(errors.nodes)}</p>
        ) : (
          <p className="text-xs text-muted-foreground">{t("haGroups.nodesHint")}</p>
        )}
      </fieldset>
      <div className="flex flex-col gap-1.5">
        <div className="flex items-center gap-2">
          <Checkbox
            id="group-restricted"
            checked={form.restricted}
            onCheckedChange={(on) => change({ restricted: on === true })}
          />
          <Label htmlFor="group-restricted">{t("haGroups.restrictedLabel")}</Label>
        </div>
        <p className="text-xs text-muted-foreground">{t("haGroups.restrictedHint")}</p>
      </div>
      <div className="flex flex-col gap-1.5">
        <div className="flex items-center gap-2">
          <Checkbox
            id="group-nofailback"
            checked={form.nofailback}
            onCheckedChange={(on) => change({ nofailback: on === true })}
          />
          <Label htmlFor="group-nofailback">{t("haGroups.nofailbackLabel")}</Label>
        </div>
        <p className="text-xs text-muted-foreground">{t("haGroups.nofailbackHint")}</p>
      </div>
    </FormDialog>
  );
}
