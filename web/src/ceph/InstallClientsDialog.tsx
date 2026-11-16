import { useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { CephView } from "@/api/types";
import { cephRoles } from "@/ceph/ceph";
import { FormDialog } from "@/components/FormDialog";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { trackTask } from "@/tasks/tasks";

/**
 * Ceph 데몬이 없는 노드에 클라이언트 꾸러미(`ceph-common`·`qemu-block-extra`)를 설치한다(노드마다
 * `ceph-client-install` Task). 데몬 노드는 데몬 Task가 이미 설치했다. 위트니스·오프라인 노드는 빼고, 처음에는 모두
 * 고른다. 열 때만 그린다.
 */
export function InstallClientsDialog({
  ceph,
  returnFocus,
  onClose,
}: {
  ceph: CephView;
  returnFocus?: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const nodes = useList("node");
  const choices = (nodes.data ?? []).filter(
    (n) => !n.spec.witness && n.status.online && cephRoles(ceph, n.id).length === 0,
  );
  const [skipped, setSkipped] = useState<string[]>([]);
  const chosen = choices.filter((n) => !skipped.includes(n.id));

  async function submit() {
    // 하나가 실패하면 거기서 멈추고 그 오류를 보인다(앞의 Task는 이미 시작했다).
    for (const node of chosen) {
      const task = await call(api.POST("/ceph/clients", { body: { node: node.id } }));
      trackTask(task, t("tasks.op.ceph-client-install", { node: node.name }));
      setSkipped((s) => [...s, node.id]);
    }
  }

  return (
    <FormDialog
      title={t("ceph.clients.title")}
      description={t("ceph.clients.description")}
      submitLabel={t("ceph.clients.install")}
      submitDisabled={chosen.length === 0}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {choices.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("ceph.clients.none")}</p>
      ) : (
        <ul className="flex flex-col gap-2">
          {choices.map((n) => (
            <li key={n.id} className="flex items-center gap-2">
              <Checkbox
                id={`ceph-client-${n.id}`}
                checked={!skipped.includes(n.id)}
                onCheckedChange={(v) =>
                  setSkipped((s) => (v === true ? s.filter((id) => id !== n.id) : [...s, n.id]))
                }
              />
              <Label htmlFor={`ceph-client-${n.id}`}>{n.name}</Label>
            </li>
          ))}
        </ul>
      )}
    </FormDialog>
  );
}
