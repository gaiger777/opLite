import type { HaGroup, Vm } from "@/api/types";
import { validName } from "@/vms/form";

type Spec = HaGroup["spec"];

/** 서버의 `MAX_PRIORITY` */
export const MAX_PRIORITY = 1000;

/** 만들기·바꾸기 폼의 값. 우선순위는 입력 그대로 문자열로 둔다. */
export interface GroupForm {
  name: string;
  /** 노드 이름 → 우선순위 입력. 고른 노드만 들어 있다. */
  nodes: Record<string, string>;
  restricted: boolean;
  nofailback: boolean;
}

export function emptyGroupForm(): GroupForm {
  return { name: "", nodes: {}, restricted: false, nofailback: false };
}

export function groupFormOf(group: HaGroup): GroupForm {
  return {
    name: group.name,
    nodes: Object.fromEntries(group.spec.nodes.map((n) => [n.node, String(n.priority ?? 0)])),
    restricted: group.spec.restricted ?? false,
    nofailback: group.spec.nofailback ?? false,
  };
}

/** 서버와 같은 검사. 오류는 필드별 번역 키다. `taken`은 다른 그룹의 이름이고 바꾸기면 null(이름 없음). */
export function checkGroupForm(form: GroupForm, taken: string[] | null): Record<string, string> {
  const found: Record<string, string> = {};
  if (taken !== null) {
    if (!validName(form.name)) {
      found.name = "forms.name";
    } else if (taken.includes(form.name)) {
      found.name = "haGroups.errors.exists";
    }
  }
  const priorities = Object.values(form.nodes);
  if (priorities.length === 0) {
    found.nodes = "haGroups.errors.nodes";
  } else if (
    priorities.some(
      (p) => p.trim() !== "" && !(/^\d+$/.test(p.trim()) && Number(p) <= MAX_PRIORITY),
    )
  ) {
    found.nodes = "haGroups.errors.priority";
  }
  return found;
}

/** 우선순위가 높은 노드부터(같으면 이름 순). 화면과 저장이 같은 순서를 쓴다. */
export function byPriority(nodes: Spec["nodes"]): Spec["nodes"] {
  return [...nodes].sort(
    (a, b) => (b.priority ?? 0) - (a.priority ?? 0) || a.node.localeCompare(b.node),
  );
}

/** 검사한 폼의 스펙. 빈 우선순위는 0이다. */
export function groupSpecOf(form: GroupForm): Spec {
  const nodes = Object.entries(form.nodes).map(([node, p]) => ({
    node,
    priority: Number(p.trim() || "0"),
  }));
  return { nodes: byPriority(nodes), restricted: form.restricted, nofailback: form.nofailback };
}

/** 같은 스펙인지(노드 순서는 보지 않는다). 바꾼 것이 없으면 저장 버튼을 막는다. */
export function sameGroupSpec(a: Spec, b: Spec): boolean {
  const norm = (s: Spec) =>
    JSON.stringify([
      byPriority(s.nodes).map((n) => [n.node, n.priority ?? 0]),
      s.restricted ?? false,
      s.nofailback ?? false,
    ]);
  return norm(a) === norm(b);
}

/** 그룹을 쓰는 VM(`ha.group`) 이름. 쓰는 VM이 있으면 서버가 삭제를 거절한다(409 `group_in_use`). */
export function groupVms(group: HaGroup, vms: readonly Vm[]): string[] {
  return vms
    .filter((vm) => typeof vm.spec.ha === "object" && vm.spec.ha.group === group.name)
    .map((vm) => vm.name)
    .sort();
}
