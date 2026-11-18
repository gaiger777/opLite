import { useTranslation } from "react-i18next";

import { useList } from "@/api/live";
import type { Node, Vm } from "@/api/types";
import { WidgetCard } from "@/dashboard/WidgetCard";
import { errorMessage } from "@/i18n/errors";
import { formatCapacity } from "@/lib/format";
import { vcpus } from "@/vms/VmListPage";

/** QEMU가 떠 있는 단계. 실패한 VM도 `status.node`가 남으므로 단계로 거른다. */
const ACTIVE: Vm["status"]["phase"][] = ["starting", "running", "stopping", "migrating"];
/** 이만큼(%) 넘게 나눠 준 자원은 경고색이다(풀 사용률과 같은 선). */
const WARN_PERCENT = 85;

/** 노드에서 시작 중·실행 중·정지 중·옮기는 중(원본 노드)인 VM이 쓰는 vCPU와 메모리 */
export function usage(node: Node, vms: Vm[]) {
  const here = vms.filter((vm) => vm.status.node === node.id && ACTIVE.includes(vm.status.phase));
  return {
    vms: here.length,
    vcpus: here.reduce((sum, vm) => sum + vcpus(vm), 0),
    memoryMib: here.reduce((sum, vm) => sum + vm.spec.memory_mib, 0),
  };
}

/** 온라인 노드(위트니스 빼고)의 CPU·메모리와 거기서 도는 VM이 나눠 받은 양 */
export function clusterCapacity(nodes: Node[], vms: Vm[]) {
  const hosts = nodes.filter((n) => n.status.online && !n.spec.witness);
  const used = hosts.map((n) => usage(n, vms));
  return {
    cpus: hosts.reduce((sum, n) => sum + n.status.cpus, 0),
    vcpus: used.reduce((sum, u) => sum + u.vcpus, 0),
    memoryMib: hosts.reduce((sum, n) => sum + n.status.memory_mib, 0),
    usedMib: used.reduce((sum, u) => sum + u.memoryMib, 0),
  };
}

function Meter({
  label,
  used,
  total,
  text,
}: {
  label: string;
  used: number;
  total: number;
  text: string;
}) {
  const { t } = useTranslation();
  const percent = total > 0 ? Math.round((used / total) * 100) : 0;
  const warn = percent > WARN_PERCENT;
  return (
    <li className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between gap-2 text-sm">
        <span className="font-medium">{label}</span>
        <span className="text-xs text-muted-foreground tabular-nums">
          {text} · {percent} %
        </span>
      </div>
      <div
        role="meter"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.min(percent, 100)}
        aria-valuetext={warn ? t("capacity.high", { text, percent }) : `${text}, ${percent} %`}
        className="h-2 w-full overflow-hidden rounded-full bg-muted"
      >
        <div
          className={`h-full rounded-full ${warn ? "bg-warning" : "bg-chart-1"}`}
          style={{ width: `${Math.min(100, percent)}%` }}
        />
      </div>
    </li>
  );
}

/**
 * 클러스터 용량(설계 §8.1): 온라인 노드의 CPU 코어와 메모리 가운데 도는 VM에 나눠 준 몫. 풀 사용률은
 * `PoolUsageWidget`이 따로 그린다. 노드·VM 목록은 watch라 새 요청이 없다.
 */
export function CapacityWidget() {
  const { t } = useTranslation();
  const nodes = useList("node");
  const vms = useList("vm");
  let body: React.ReactNode;
  if (!nodes.data || !vms.data) {
    const failed = [nodes, vms].find((q) => q.isError);
    body = (
      <p className="text-sm text-muted-foreground">
        {failed ? errorMessage(t, failed.error) : t("common.loading")}
      </p>
    );
  } else {
    const cap = clusterCapacity(nodes.data, vms.data);
    body = (
      <ul className="flex flex-col gap-3">
        <Meter
          label={t("capacity.vcpus")}
          used={cap.vcpus}
          total={cap.cpus}
          text={t("capacity.vcpusOf", { used: cap.vcpus, total: cap.cpus })}
        />
        <Meter
          label={t("capacity.memory")}
          used={cap.usedMib}
          total={cap.memoryMib}
          text={`${formatCapacity(cap.usedMib)} / ${formatCapacity(cap.memoryMib)}`}
        />
      </ul>
    );
  }
  return <WidgetCard title={t("capacity.title")}>{body}</WidgetCard>;
}
