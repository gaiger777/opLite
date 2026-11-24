// 놓기(계획 71 설계 §7.4): 부품·블록을 칸에 놓은 다음 상태. 놓기 규칙(`accepts`)을 통과한 것만 부른다(통과하지 않은
// 놓기는 상태를 그대로 둔다). 값은 폼과 같은 `VmFormValues`이고 프리셋은 폼의 `applyPreset`이다.
import type { Vm } from "@/api/types";
import { accepts, type Thing } from "@/vmBuilder/model/accepts";
import { NEW_DISK_GIB, SIZES } from "@/vmBuilder/model/parts";
import { type ListKind, listOf, slotOf } from "@/vmBuilder/model/slots";
import {
  type BuilderCtx,
  type BuilderState,
  formOnly,
  NOT_DEPLOYED,
  renumber,
} from "@/vmBuilder/model/state";
import { DEFAULT_NET, emptyCloudInit, type NetRow } from "@/vms/cloudInit";
import { defaultMachine, emptyDisk, emptyForm, type NicRow, type VmFormValues } from "@/vms/form";
import { applyPreset } from "@/vms/form/steps";

/** 패키지 조각이 비어 있는 user-data에 넣는 글(게스트 에이전트 꾸러미) */
export const PACKAGES_USER_DATA =
  "#cloud-config\npackage_update: true\npackages: [qemu-guest-agent]\n";

function move<T>(list: readonly T[], from: number, to: number): T[] {
  const out = [...list];
  const [item] = out.splice(from, 1);
  out.splice(to, 0, item!);
  return out;
}

/** cloud-init NIC 주소 줄을 NIC 수만큼 채운다(빈 자리는 기본 — 순서를 옮기려면 자리가 있어야 한다). */
function padNets(nets: readonly NetRow[], n: number): NetRow[] {
  return Array.from({ length: Math.max(n, nets.length) }, (_, i) => nets[i] ?? DEFAULT_NET);
}

/** 목록 블록을 옮긴다(디스크 순서가 부팅 순서, NIC면 cloud-init 주소도 따라감). */
export function moveBlock(s: BuilderState, list: ListKind, from: number, to: number): BuilderState {
  const v = s.values;
  if (from === to || from < 0 || from >= v[list].length || to < 0 || to >= v[list].length) {
    return s;
  }
  const values: VmFormValues = { ...v, [list]: move(v[list] as unknown[], from, to) };
  let autoNames = s.autoNames;
  if (list === "disks") {
    autoNames = move(padBool(s.autoNames, v.disks.length), from, to);
  }
  if (list === "nics" && v.cloudInit.networks.length > Math.min(from, to)) {
    values.cloudInit = {
      ...v.cloudInit,
      networks: move(padNets(v.cloudInit.networks, v.nics.length), from, to),
    };
  }
  return renumber({ ...s, values, autoNames });
}

function padBool(list: readonly boolean[], n: number): boolean[] {
  return Array.from({ length: n }, (_, i) => list[i] ?? false);
}

/** 목록 블록을 뺀다(NIC면 그 NIC의 cloud-init 주소도 — 폼의 `changeNics` 규칙). */
export function removeBlock(s: BuilderState, list: ListKind, index: number): BuilderState {
  const v = s.values;
  const values: VmFormValues = { ...v, [list]: v[list].filter((_, i) => i !== index) };
  let autoNames = s.autoNames;
  if (list === "disks") {
    autoNames = s.autoNames.filter((_, i) => i !== index);
  }
  if (list === "nics") {
    values.cloudInit = {
      ...v.cloudInit,
      networks: v.cloudInit.networks.filter((_, i) => i !== index),
    };
  }
  return renumber({ ...s, values, autoNames });
}

/**
 * 템플릿 바탕: 틀의 디스크·NIC를 템플릿 것으로(바꿀 수 없는 줄), 계산·CPU·머신은 템플릿 값에서 시작한다. 배포 요청이
 * 싣지 못하는 폼 칸(`NOT_DEPLOYED` — 복제·스냅샷 예약·NUMA 등)은 처음 값으로 비운다(보내지 않는 값을 들고 있지 않게).
 */
export function templateValues(v: VmFormValues, t: Vm): VmFormValues {
  const s = t.spec;
  const empty = emptyForm(s.machine, v.folder);
  return {
    ...v,
    ...(Object.fromEntries(NOT_DEPLOYED.map((k) => [k, empty[k]])) as Partial<VmFormValues>),
    sockets: s.cpu.sockets ?? 1,
    cores: String(s.cpu.cores),
    model: s.cpu.model ?? "host",
    cpuFlags: s.cpu.flags ?? [],
    memory: String(s.memory_mib),
    machine: s.machine,
    // 디스크는 템플릿 볼륨(크기 칸은 늘릴 때만 — 비면 그대로)
    disks: (s.disks ?? []).map((d) => ({
      kind: "existing",
      volume: d.volume,
      pool: "",
      sizeGib: "",
      bus: d.bus,
      bootOrder: "",
    })),
    cdroms: [],
    nics: (s.nics ?? []).map((n): NicRow => ({
      network: n.network,
      model: n.model,
      mac: "",
      securityGroups: [],
      ips: "",
    })),
    // 배포의 cloud-init은 덮어쓰기다(비면 템플릿 것 그대로 — 배포 대화상자와 같다).
    cloudInit: emptyCloudInit(),
    // 배포 요청이 싣지 못하는 칸(HA·노드 고정)은 끈다 — 틀이 켜졌다고 보이지 않게.
    ha: false,
    haGroup: "",
    pinned: false,
  };
}

/** 부품·블록을 칸 `at`에 놓은 다음 상태. 놓을 수 없으면 그대로다. */
export function drop(s: BuilderState, t: Thing, at: string, ctx: BuilderCtx): BuilderState {
  if (!accepts(t, at, s, ctx).ok) {
    return s;
  }
  const slot = slotOf(at)!;
  const index = "index" in slot ? slot.index : null;
  const v = s.values;
  const windows = s.preset === "windows";
  const set = (patch: Partial<VmFormValues>, extra: Partial<BuilderState> = {}): BuilderState => ({
    ...s,
    ...extra,
    values: { ...v, ...patch },
  });
  switch (t.kind) {
    case "block": {
      if (slot.kind === "trash") {
        return removeBlock(s, t.list, t.index);
      }
      const to = listOf(slot)!.index ?? v[t.list].length - 1;
      return moveBlock(s, t.list, t.index, to);
    }
    case "size":
      return set({
        sockets: 1,
        cores: String(SIZES[t.size].cores),
        memory: String(SIZES[t.size].memory),
      });
    case "os": {
      const next = applyPreset(t.os, s.preset, v);
      return { ...s, values: next, preset: t.os };
    }
    case "cpu":
      return set({ model: t.model });
    case "template": {
      const vm = ctx.vms.find((x) => x.id === t.vm)!;
      return {
        ...s,
        base: { kind: "template", id: vm.id, linked: null, pool: "" },
        values: templateValues(v, vm),
        // 비운 칸(앞 템플릿이 비운 것도)을 틀 위 칩이 알린다.
        cleared: [
          ...new Set([
            ...(s.cleared ?? []),
            ...formOnly(v).filter((k) => NOT_DEPLOYED.includes(k)),
          ]),
        ],
        autoNames: [],
        preset: "blank",
      };
    }
    case "pool":
      if (index !== null) {
        return set({ disks: v.disks.map((d, i) => (i === index ? { ...d, pool: t.pool } : d)) });
      }
      return renumber(
        set(
          {
            disks: [
              ...v.disks,
              {
                ...emptyDisk(v.name, v.disks.length),
                pool: t.pool,
                sizeGib: NEW_DISK_GIB,
                bus: windows ? "sata" : "virtio-blk",
                // 명시 부팅 순서는 비운다(기본 규칙: 디스크 차례, CD는 뒤).
                bootOrder: "",
              },
            ],
          },
          { autoNames: [...padBool(s.autoNames, v.disks.length), true] },
        ),
      );
    case "volume":
      return set(
        {
          disks: [
            ...v.disks,
            {
              kind: "existing",
              volume: t.volume,
              pool: "",
              sizeGib: "",
              bus: windows ? "sata" : "virtio-blk",
              bootOrder: "",
            },
          ],
        },
        { autoNames: [...padBool(s.autoNames, v.disks.length), false] },
      );
    case "iso":
      return index === null
        ? set({ cdroms: [...v.cdroms, { iso: t.iso, bootOrder: "" }] })
        : set({ cdroms: v.cdroms.map((c, i) => (i === index ? { ...c, iso: t.iso } : c)) });
    case "network": {
      if (index === null) {
        return set({
          nics: [
            ...v.nics,
            {
              network: t.network,
              model: windows ? "e1000e" : "virtio",
              mac: "",
              securityGroups: [],
              ips: "",
            },
          ],
        });
      }
      // 네트워크만 바꾼다: 그 NIC의 SDN 주소와 cloud-init 주소는 지우고, `user`에는 보안 그룹을 걸 수 없다.
      const nets = v.cloudInit.networks;
      return set({
        nics: v.nics.map((n, i) =>
          i === index
            ? {
                ...n,
                network: t.network,
                ips: "",
                securityGroups: t.network === "user" ? [] : n.securityGroups,
              }
            : n,
        ),
        cloudInit:
          nets.length > index
            ? { ...v.cloudInit, networks: nets.map((r, i) => (i === index ? DEFAULT_NET : r)) }
            : v.cloudInit,
      });
    }
    case "sg":
      return set({
        nics: v.nics.map((n, i) =>
          i === index && !n.securityGroups.includes(t.sg)
            ? { ...n, securityGroups: [...n.securityGroups, t.sg] }
            : n,
        ),
      });
    case "addr": {
      const nets = padNets(v.cloudInit.networks, index! + 1);
      nets[index!] = t.mode === "static" ? { ...DEFAULT_NET, ipv4: "static" } : DEFAULT_NET;
      return set({ cloudInit: { ...v.cloudInit, enabled: true, networks: nets } });
    }
    case "ci": {
      const ci = { ...v.cloudInit, enabled: true };
      if (t.snippet === "packages" && ci.userData === "") {
        // 게스트 에이전트 장치는 템플릿 것 그대로다(배포 요청이 싣지 못함).
        return set({
          cloudInit: { ...ci, userData: PACKAGES_USER_DATA },
          ...(s.base.kind !== "template" && { guestAgent: true }),
        });
      }
      return set({ cloudInit: ci });
    }
    case "auto":
      return set({ node: "", pinned: false }, { zone: null });
    case "node": {
      // 머신은 노드의 아키텍처를 따른다(폼의 자동 머신 — Windows·템플릿은 정해진 머신).
      const machine =
        windows || s.base.kind === "template" || v.machine === "" || !ctx.nodes
          ? v.machine
          : defaultMachine(ctx.nodes, t.node);
      return set({ node: t.node, machine }, { zone: null });
    }
    case "zone":
      return set({ node: "" }, { zone: { key: "zone", value: t.value, hard: true } });
    case "ha":
      return set({ ha: true, haState: "started" });
    case "haGroup":
      return set({ ha: true, haGroup: t.haGroup });
    case "folder":
      return set({ folder: t.folder });
  }
}
