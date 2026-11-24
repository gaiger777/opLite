// hard 배치 규칙 예상(계획 71 설계 §10.2): VM을 이 노드에 두면 어기는 hard 규칙의 이름. 서버 `opl-api` `placement.rs`의
// `hard_violations`를 그대로 따라 셈한다(같은 입력 → 같은 이름, 시험이 서버 시험 표를 옮겨 맞춘다). 빌더의 노드 부품과
// 지도 끌어 옮기기가 함께 쓰는 "예상"이고, 최종은 서버(만들기·옮기기의 409 `placement_rule`)다.
import type { HaGroup, Node, PlacementRule, Vm } from "@/api/types";
import { isHard } from "@/placement/rules";
import { haPolicy } from "@/vms/ha";

/** 규칙에 비춰 볼 VM(아직 없는 VM이면 `id`가 없다) */
export interface RuleSubject {
  id?: string;
  name: string;
  labels?: Record<string, string>;
  /** HA 정책이 있고 `ignored`가 아님(서버 `ha_active`) */
  haActive: boolean;
  haGroup?: string;
}

/** 있는 VM의 규칙 대상 */
export function subjectOf(vm: Vm): RuleSubject {
  const ha = haPolicy(vm.spec);
  return {
    id: vm.id,
    name: vm.name,
    labels: vm.labels,
    haActive: ha !== null && ha.state !== "ignored",
    haGroup: ha?.group ?? undefined,
  };
}

/** 서버의 `tag_matches`: 레이블에 `key=value`의 키가 있고 값이 같은지 */
function tagMatches(tag: string, labels: Record<string, string> | undefined): boolean {
  const at = tag.indexOf("=");
  return at > 0 && labels?.[tag.slice(0, at)] === tag.slice(at + 1);
}

/** 서버의 `has_vm` */
function hasVm(spec: PlacementRule["spec"], name: string, labels?: Record<string, string>) {
  return spec.vm_tag != null ? tagMatches(spec.vm_tag, labels) : (spec.vms ?? []).includes(name);
}

/** 서버의 `has_node` */
function hasNode(spec: PlacementRule["spec"], node: Node) {
  return spec.node_tag != null
    ? tagMatches(spec.node_tag, node.labels)
    : (spec.nodes ?? []).includes(node.name);
}

/** 서버의 `occupied`: 켜기로 된 VM의 노드(옮기는 중이면 대상 노드도) */
function occupied(vm: Vm): string[] {
  const node = vm.spec.node;
  if (!node || vm.spec.power !== "running") {
    return [];
  }
  const to = vm.status.migration?.node;
  return to && to !== node ? [node, to] : [node];
}

/** VM을 `node`에 두면 어기는 켜진 hard 규칙 이름과 제한 HA 그룹(`ha-group:<이름>`), 이름 순 */
export function hardViolations(
  vm: RuleSubject,
  node: Node,
  rules: readonly PlacementRule[],
  vms: readonly Vm[],
  groups: readonly HaGroup[] = [],
): string[] {
  const names = new Set<string>();
  for (const rule of rules) {
    const s = rule.spec;
    if (!s.enabled || !isHard(s.kind) || !hasVm(s, vm.name, vm.labels)) {
      continue;
    }
    const others = () =>
      vms.filter((v) => v.id !== vm.id && hasVm(s, v.name, v.labels)).flatMap(occupied);
    const broken =
      s.kind === "affinity"
        ? others().some((n) => n !== node.id)
        : s.kind === "anti-affinity"
          ? others().includes(node.id)
          : s.kind === "node-affinity"
            ? !hasNode(s, node)
            : s.kind === "node-anti-affinity" && hasNode(s, node);
    if (broken) {
      names.add(rule.name);
    }
  }
  const group = vm.haActive && vm.haGroup ? groups.find((g) => g.name === vm.haGroup) : undefined;
  if (group?.spec.restricted && !group.spec.nodes.some((n) => n.node === node.name)) {
    names.add(`ha-group:${group.name}`);
  }
  return [...names].sort();
}
