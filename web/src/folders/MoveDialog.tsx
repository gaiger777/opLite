import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useList } from "@/api/live";
import type { FolderKind } from "@/api/types";
import { objectTarget, type Permissions } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Button } from "@/components/ui/button";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  type Foldered,
  folderPath,
  folderTree,
  type FolderTree,
  MEMBER_KIND,
  moveMembers,
  ROOT_ID,
} from "@/folders/folders";

/** 옮길 수 있는 곳: 루트(루트의 `inventory.move`)와 `inventory.move`가 있는 폴더 */
export function moveTargets(tree: FolderTree, perms: Pick<Permissions, "has">): string[] {
  return [
    ...(perms.has("inventory.move") ? [ROOT_ID] : []),
    ...tree.folders
      .filter((f) => perms.has("inventory.move", { type: "folder", id: f.id }))
      .map((f) => f.id),
  ];
}

/** 이 객체를 이 폴더로 옮길 수 없는 이유(번역 키). 옮길 수 있으면 null */
export function moveReason(
  item: Foldered,
  dest: string,
  kind: FolderKind,
  perms: Pick<Permissions, "has">,
): string | null {
  if ((item.spec.folder ?? ROOT_ID) === dest) {
    return "folders.reason.already";
  }
  return perms.has("inventory.move", objectTarget(MEMBER_KIND[kind], item))
    ? null
    : "folders.reason.denied";
}

/**
 * 고른 객체를 폴더로 옮기는 결과를 알린다(계획 61 묶음 알림과 같은 꼴). 실패가 있으면 알림이 남는다.
 * `errors`는 실패한 항목 이름 → 서버 오류 글이다.
 */
export function announceMove(
  t: (key: string, values?: Record<string, unknown>) => string,
  path: string,
  results: { ok: boolean; key: string; error?: { message: string } }[],
) {
  const ok = results.filter((r) => r.ok).length;
  const failed = results.filter((r) => !r.ok);
  const message = t("folders.moved", { path, ok, total: results.length, failed: failed.length });
  if (failed.length === 0) {
    toast.success(message);
  } else {
    toast.error(message, {
      description: failed.map((r) => `${r.key}: ${r.error?.message ?? ""}`).join("\n"),
      duration: Infinity,
    });
  }
}

/** 일괄 막대의 "폴더로 옮기기…"(가능 n / 불가 m). 끌어 놓기의 키보드·휴대폰 대안이다. */
export function MoveButton<T extends Foldered>({
  items,
  allowed,
  onClick,
}: {
  items: T[];
  allowed(item: T): boolean;
  onClick(): void;
}) {
  const { t } = useTranslation();
  const able = items.filter(allowed).length;
  return (
    <Button
      size="sm"
      variant="outline"
      disabled={able === 0}
      aria-label={t("folders.moveButton", { able, blocked: items.length - able })}
      onClick={onClick}
    >
      {t("folders.moveTo")}
      <span aria-hidden className="text-xs tabular-nums opacity-70">
        {able}/{items.length}
      </span>
    </Button>
  );
}

/** 고른 객체를 폴더로 옮기는 확인 창: 대상 폴더, 옮길 수 있는 수와 없는 까닭, 결과 알림 */
export function MoveDialog({
  kind,
  items,
  initial,
  onClose,
  onDone,
}: {
  kind: FolderKind;
  items: Foldered[];
  /** 처음 고른 대상(끌어 놓기) */
  initial?: string;
  onClose(): void;
  /** 옮긴 뒤(고르기 지우기) */
  onDone?(): void;
}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const folders = useList("folder");
  const tree = useMemo(() => folderTree(folders.data, kind), [folders.data, kind]);
  const targets = moveTargets(tree, perms);
  const [dest, setDest] = useState(initial ?? targets[0] ?? "");
  const reasons = items.map((item) => ({
    item,
    reason: dest ? moveReason(item, dest, kind, perms) : null,
  }));
  const able = reasons.filter((r) => r.reason === null).map((r) => r.item);
  const label = (id: string) => (id === ROOT_ID ? t("folders.root") : folderPath(tree, id));

  async function submit() {
    const results = await moveMembers(
      dest === ROOT_ID ? null : dest,
      MEMBER_KIND[kind],
      able.map((item) => item.name),
    );
    announceMove(t, label(dest), results);
    onDone?.();
  }

  return (
    <FormDialog
      title={t("folders.moveTitle", { count: items.length })}
      submitLabel={t("folders.move")}
      submitDisabled={!dest || able.length === 0}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="move-dest" label={t("folders.destination")}>
        <NativeSelect
          id="move-dest"
          className="w-full"
          value={dest}
          onChange={(e) => setDest(e.target.value)}
        >
          {targets.length === 0 && <NativeSelectOption value="">—</NativeSelectOption>}
          {targets.map((id) => (
            <NativeSelectOption key={id} value={id}>
              {label(id)}
            </NativeSelectOption>
          ))}
        </NativeSelect>
      </Field>
      <p className="text-sm">
        {t("folders.moveCounts", { able: able.length, blocked: items.length - able.length })}
      </p>
      <ul className="flex max-h-48 flex-col gap-1 overflow-y-auto text-sm">
        {reasons.map(({ item, reason }) => (
          <li key={item.id} className="flex justify-between gap-2">
            <span className="truncate font-medium">{item.name}</span>
            <span className="text-muted-foreground">
              {reason ? t(reason) : t("folders.willMove")}
            </span>
          </li>
        ))}
      </ul>
    </FormDialog>
  );
}
