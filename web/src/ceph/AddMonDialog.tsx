import { useQueryClient } from "@tanstack/react-query";
import { useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { CephView } from "@/api/types";
import { CEPH_KEY, MAX_MONS, monWarning } from "@/ceph/ceph";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { trackTask } from "@/tasks/tasks";

/**
 * mon이나 mgr을 노드 하나에 더한다(`ceph-mon-add`·`ceph-mgr-add` Task). 그 역할이 이미 있는 노드와 위트니스는
 * 고를 수 없고 오프라인 노드는 막는다. mon은 5개까지이고, 더한 뒤 짝수가 되면 경고한다. 열 때만 그린다.
 */
export function AddMonDialog({
  ceph,
  role,
  returnFocus,
  onClose,
}: {
  ceph: CephView;
  role: "mon" | "mgr";
  returnFocus?: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const has = (id: string) =>
    role === "mon" ? ceph.mons.some((m) => m.node === id) : ceph.mgrs.includes(id);
  const choices = (nodes.data ?? []).filter((n) => !n.spec.witness && !has(n.id));
  const [node, setNode] = useState("");
  const chosen = node || (choices.find((n) => n.status.online)?.id ?? "");
  const full = role === "mon" && ceph.mons.length >= MAX_MONS;
  const warning = role === "mon" ? monWarning(ceph.mons.length + 1) : null;

  async function submit() {
    const target = choices.find((n) => n.id === chosen);
    if (!target) {
      return false;
    }
    const task = await call(
      api.POST(role === "mon" ? "/ceph/mons" : "/ceph/mgrs", { body: { node: target.id } }),
    );
    trackTask(task, t(`tasks.op.ceph-${role}-add`, { node: target.name }));
    await queryClient.invalidateQueries({ queryKey: CEPH_KEY });
  }

  return (
    <FormDialog
      title={t(`ceph.${role}.addTitle`)}
      description={t(`ceph.${role}.addDescription`)}
      submitLabel={t(`ceph.${role}.add`)}
      submitDisabled={full || chosen === ""}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {full ? (
        <p className="text-sm text-muted-foreground">{t("ceph.mon.limit", { n: MAX_MONS })}</p>
      ) : choices.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("ceph.noNodes")}</p>
      ) : (
        <Field id="ceph-role-node" label={t("common.node")}>
          <NativeSelect
            id="ceph-role-node"
            value={chosen}
            onChange={(e) => setNode(e.target.value)}
          >
            {choices.map((n) => (
              <NativeSelectOption key={n.id} value={n.id} disabled={!n.status.online}>
                {n.status.online ? n.name : t("ceph.offlineNode", { name: n.name })}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
      )}
      {!full && warning && (
        <p className="text-sm text-amber-700 dark:text-amber-400">{t(warning)}</p>
      )}
    </FormDialog>
  );
}
