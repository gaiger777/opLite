import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { NodeDisk } from "@/api/types";
import { CEPH_KEY, confirmWipe, diskChoice } from "@/ceph/ceph";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Badge } from "@/components/ui/badge";
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
import { formatBytes } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";

/** 노드의 디스크 목록(`GET /nodes/{key}/disks`). 노드에 묻는 것이라 다시 시도하지 않고 오류를 곧바로 보인다. */
export function useNodeDisks(node: string) {
  return useQuery({
    queryKey: ["node-disks", node],
    queryFn: () => call(api.GET("/nodes/{key}/disks", { params: { path: { key: node } } })),
    enabled: node !== "",
    retry: false,
  });
}

/** 디스크 상태의 글: 쓸 수 있음, 지워야 함(이유들), 쓰는 중(이유) */
export function DiskStateText({ disk }: { disk: NodeDisk }) {
  const { t } = useTranslation();
  const choice = diskChoice(disk);
  return (
    <>
      <Badge
        variant={
          choice.kind === "available" ? "default" : choice.kind === "wipe" ? "secondary" : "outline"
        }
      >
        {t(`ceph.disk.${choice.kind}`)}
      </Badge>
      {choice.kind !== "available" && (
        <span className="ml-2 text-xs text-muted-foreground">
          {choice.kind === "wipe" ? choice.reasons.join(", ") : choice.reason}
        </span>
      )}
    </>
  );
}

/**
 * 디스크 표. `onChoose`가 있으면 쓸 수 있거나 지워야 하는 디스크를 고르는 단추를 둔다(쓰는 중인 디스크는 이유만).
 */
export function DiskTable({
  disks,
  label,
  chosen,
  onChoose,
}: {
  disks: NodeDisk[];
  label: string;
  chosen?: string;
  onChoose?(disk: NodeDisk): void;
}) {
  const { t } = useTranslation();
  if (disks.length === 0) {
    return <p className="text-sm text-muted-foreground">{t("ceph.disk.none")}</p>;
  }
  return (
    <Table aria-label={label}>
      <TableHeader>
        <TableRow>
          {onChoose && (
            <TableHead>
              <span className="sr-only">{t("ceph.disk.choose")}</span>
            </TableHead>
          )}
          <TableHead>{t("ceph.disk.device")}</TableHead>
          <TableHead>{t("ceph.disk.size")}</TableHead>
          <TableHead>{t("ceph.disk.type")}</TableHead>
          <TableHead>{t("ceph.disk.model")}</TableHead>
          <TableHead>{t("common.state")}</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {disks.map((disk) => (
          <TableRow key={disk.path}>
            {onChoose && (
              <TableCell>
                {disk.state !== "in-use" && (
                  <input
                    type="radio"
                    name="ceph-osd-disk"
                    aria-label={disk.path}
                    checked={chosen === disk.path}
                    onChange={() => onChoose(disk)}
                  />
                )}
              </TableCell>
            )}
            <TableCell className="font-mono text-xs">
              {disk.path}
              {disk.by_id && (
                <span className="block text-muted-foreground">
                  {disk.by_id.replace(/^\/dev\/disk\/by-id\//, "")}
                </span>
              )}
            </TableCell>
            <TableCell className="whitespace-nowrap">{formatBytes(disk.size_bytes)}</TableCell>
            <TableCell>{disk.rotational ? "HDD" : "SSD"}</TableCell>
            <TableCell>{[disk.model, disk.serial].filter(Boolean).join(" · ") || "—"}</TableCell>
            <TableCell>
              <DiskStateText disk={disk} />
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

/**
 * 노드의 디스크 하나에 OSD를 만든다(`ceph-osd-add` Task). 비지 않은 디스크는 장치 이름을 다시 쳐야 지우고 쓴다.
 * 열 때만 그린다.
 */
export function AddOsdDialog({
  returnFocus,
  onClose,
}: {
  returnFocus?: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const choices = (nodes.data ?? []).filter((n) => !n.spec.witness && n.status.online);
  const [node, setNode] = useState("");
  const chosenNode = node || (choices[0]?.id ?? "");
  const disks = useNodeDisks(chosenNode);
  const [path, setPath] = useState("");
  const [typed, setTyped] = useState("");
  const disk = disks.data?.find((d) => d.path === path);
  const wipe = disk?.state === "wipeable";
  const ready = disk !== undefined && (!wipe || confirmWipe(disk, typed));

  async function submit() {
    const target = choices.find((n) => n.id === chosenNode);
    if (!target || !disk) {
      return false;
    }
    // 확인한 디스크를 by-id 경로로 보낸다(그사이 장치 이름이 바뀌어도 같은 디스크).
    const device = disk.by_id ?? disk.path;
    const task = await call(api.POST("/ceph/osds", { body: { node: target.id, device, wipe } }));
    trackTask(task, t("tasks.op.ceph-osd-add", { node: target.name, device: disk.path }));
    await queryClient.invalidateQueries({ queryKey: CEPH_KEY });
  }

  return (
    <FormDialog
      title={t("ceph.osd.addTitle")}
      description={t("ceph.osd.addDescription")}
      submitLabel={t(wipe ? "ceph.osd.wipeAndAdd" : "ceph.osd.add")}
      destructive={wipe}
      submitDisabled={!ready}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {choices.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("ceph.noNodes")}</p>
      ) : (
        <>
          <Field id="ceph-osd-node" label={t("common.node")}>
            <NativeSelect
              id="ceph-osd-node"
              value={chosenNode}
              onChange={(e) => {
                setNode(e.target.value);
                setPath("");
                setTyped("");
              }}
            >
              {choices.map((n) => (
                <NativeSelectOption key={n.id} value={n.id}>
                  {n.name}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          </Field>
          <div className="max-h-[40vh] overflow-y-auto">
            {disks.data ? (
              <DiskTable
                disks={disks.data}
                label={t("ceph.disk.table")}
                chosen={path}
                onChoose={(d) => {
                  setPath(d.path);
                  setTyped("");
                }}
              />
            ) : (
              <ListPending error={disks.error} />
            )}
          </div>
          {disk && wipe && (
            <Field
              id="ceph-osd-confirm"
              label={t("ceph.osd.typeDevice", { device: disk.path })}
              help={t("ceph.osd.wipeWarning", { reasons: diskReasons(disk) })}
            >
              {(a11y) => (
                <Input
                  id="ceph-osd-confirm"
                  autoComplete="off"
                  value={typed}
                  onChange={(e) => setTyped(e.target.value)}
                  {...a11y}
                />
              )}
            </Field>
          )}
        </>
      )}
    </FormDialog>
  );
}

function diskReasons(disk: NodeDisk): string {
  return disk.state === "wipeable" ? disk.reasons.join(", ") : "";
}
