import type { PlacementRule, RuleKind } from "@/api/types";
import { validName } from "@/vms/form";

type Spec = PlacementRule["spec"];

/** 화면에 보이는 순서: VM끼리 규칙, VM과 노드 규칙 */
export const RULE_KINDS: readonly RuleKind[] = [
  "affinity",
  "anti-affinity",
  "soft-affinity",
  "soft-anti-affinity",
  "node-affinity",
  "node-anti-affinity",
  "soft-node-affinity",
  "soft-node-anti-affinity",
];

/** 서버의 `MAX_WEIGHT`·`DEFAULT_WEIGHT` */
export const MAX_WEIGHT = 100;
export const DEFAULT_WEIGHT = 50;

/** 서버의 `RuleKind::is_hard`: `soft-`가 아니면 반드시 지키는 규칙 */
export function isHard(kind: RuleKind): boolean {
  return !kind.startsWith("soft-");
}

/** 서버의 `RuleKind::is_vm_rule`: VM끼리 규칙(노드를 주지 않는다) */
export function isVmRule(kind: RuleKind): boolean {
  return !kind.includes("node");
}

/** 서버의 `parse_tag`: `key=value`이고 key가 비지 않았는지 */
export function validTag(tag: string): boolean {
  const at = tag.indexOf("=");
  return at > 0;
}

function parseNames(text: string): string[] {
  return text
    .split(",")
    .map((n) => n.trim())
    .filter((n) => n !== "");
}

/** 만들기·바꾸기 폼의 값. 숫자도 입력 그대로 문자열로 둔다. */
export interface RuleForm {
  name: string;
  kind: RuleKind;
  /** VM을 이름(`names`)으로 고르는지 레이블(`tag`)로 고르는지 */
  vmBy: "names" | "tag";
  /** 쉼표로 나눈 VM 이름 */
  vms: string;
  vmTag: string;
  nodeBy: "names" | "tag";
  /** 고른 노드 이름 */
  nodes: string[];
  nodeTag: string;
  /** soft 규칙의 벌점. 비면 서버 기본(50)이다. */
  weight: string;
  haMayViolate: boolean;
  enabled: boolean;
}

export function emptyRuleForm(): RuleForm {
  return {
    name: "",
    kind: "anti-affinity",
    vmBy: "names",
    vms: "",
    vmTag: "",
    nodeBy: "names",
    nodes: [],
    nodeTag: "",
    weight: "",
    haMayViolate: false,
    enabled: true,
  };
}

export function ruleFormOf(rule: PlacementRule): RuleForm {
  const s = rule.spec;
  return {
    name: rule.name,
    kind: s.kind,
    vmBy: s.vm_tag != null ? "tag" : "names",
    vms: (s.vms ?? []).join(", "),
    vmTag: s.vm_tag ?? "",
    nodeBy: s.node_tag != null ? "tag" : "names",
    nodes: s.nodes ?? [],
    nodeTag: s.node_tag ?? "",
    weight: s.weight == null ? "" : String(s.weight),
    haMayViolate: s.ha_may_violate ?? false,
    enabled: s.enabled,
  };
}

/** 서버와 같은 검사. 오류는 필드별 번역 키다. `taken`은 다른 규칙의 이름이고 바꾸기면 null(이름 없음). */
export function checkRuleForm(form: RuleForm, taken: string[] | null): Record<string, string> {
  const found: Record<string, string> = {};
  if (taken !== null) {
    if (!validName(form.name)) {
      found.name = "forms.name";
    } else if (taken.includes(form.name)) {
      found.name = "placementRules.errors.exists";
    }
  }
  if (form.vmBy === "tag") {
    if (!validTag(form.vmTag.trim())) {
      found.vms = "placementRules.errors.tag";
    }
  } else {
    const names = parseNames(form.vms);
    if (names.length < (isVmRule(form.kind) ? 2 : 1)) {
      found.vms = isVmRule(form.kind)
        ? "placementRules.errors.twoVms"
        : "placementRules.errors.oneVm";
    } else if (names.some((n) => !validName(n)) || new Set(names).size !== names.length) {
      found.vms = "placementRules.errors.vmNames";
    }
  }
  if (!isVmRule(form.kind)) {
    if (form.nodeBy === "tag") {
      if (!validTag(form.nodeTag.trim())) {
        found.nodes = "placementRules.errors.tag";
      }
    } else if (form.nodes.length === 0) {
      found.nodes = "placementRules.errors.nodes";
    }
  }
  const weight = form.weight.trim();
  if (
    !isHard(form.kind) &&
    weight !== "" &&
    !(/^\d+$/.test(weight) && Number(weight) >= 1 && Number(weight) <= MAX_WEIGHT)
  ) {
    found.weight = "placementRules.errors.weight";
  }
  return found;
}

/** 검사한 폼의 스펙. 그 종류에 없는 값(VM끼리 규칙의 노드, hard 규칙의 벌점 등)은 빼고 보낸다. */
export function ruleSpecOf(form: RuleForm): Spec {
  const weight = form.weight.trim();
  return {
    kind: form.kind,
    ...(form.vmBy === "tag" ? { vm_tag: form.vmTag.trim() } : { vms: parseNames(form.vms) }),
    ...(!isVmRule(form.kind) &&
      (form.nodeBy === "tag"
        ? { node_tag: form.nodeTag.trim() }
        : { nodes: [...form.nodes].sort() })),
    ...(!isHard(form.kind) && weight !== "" && { weight: Number(weight) }),
    ...(isHard(form.kind) && form.haMayViolate && { ha_may_violate: true }),
    enabled: form.enabled,
  };
}

/** 같은 스펙인지(빠진 값과 기본값은 같게 본다). 바꾼 것이 없으면 저장 버튼을 막는다. */
export function sameRuleSpec(a: Spec, b: Spec): boolean {
  const norm = (s: Spec) =>
    JSON.stringify([
      s.kind,
      s.vms ?? [],
      s.vm_tag ?? null,
      [...(s.nodes ?? [])].sort(),
      s.node_tag ?? null,
      s.weight ?? null,
      s.ha_may_violate ?? false,
      s.enabled,
    ]);
  return norm(a) === norm(b);
}
