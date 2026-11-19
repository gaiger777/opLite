import { useQueryClient } from "@tanstack/react-query";
import { cn } from "cn";
import { type RefObject, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Node, Vm } from "@/api/types";
import { useAuth, usePrincipal } from "@/auth/AuthProvider";
import { objectTarget, type Privilege } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { ACTION_ICONS } from "@/components/icons";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Kbd } from "@/components/ui/kbd";
import { FOLDER_PATHS } from "@/folders/folders";
import { useFolderPaths } from "@/folders/useFolderPaths";
import { createItems } from "@/layout/create";
import { AREAS, visibleTabs } from "@/layout/nav";
import { useDensity } from "@/lib/density";
import { alive, type ResourceRef, useRefs } from "@/lib/recent";
import { keyLabel } from "@/lib/shortcuts";
import { useTheme } from "@/lib/theme";
import { parseCommand, type Verb } from "@/palette/parse";
import { type Candidate, matchScore, PREFIX, rank } from "@/palette/rank";
import { availability, isBusy, useVmActions } from "@/vms/actions";
import { type GuestInfoResponse, guestAddresses } from "@/vms/guest";
import { useVmActionHost } from "@/vms/actionHost";
import { migrationTargets } from "@/vms/VmMigrateDialog";

/** 그룹 순서. 즐겨찾기가 맨 위다(설계 §5.7). */
const GROUPS = [
  "favorites",
  "recent",
  "actions",
  "vms",
  "nodes",
  "pools",
  "volumes",
  "networks",
  "securityGroups",
  "backupJobs",
  "haGroups",
  "folders",
  "navigation",
  "commands",
] as const;
type Group = (typeof GROUPS)[number];

interface Item extends Candidate {
  group: Group;
  /** 이름 뒤 흐린 글(상태·노드·IP) */
  detail?: string;
  /** 막힌 이유(번역한 글). 있으면 흐리게 보이고 실행하지 않는다. */
  disabled?: string;
  /** 경로로 가는 항목의 주소(⌘↵는 새 탭) */
  to?: string;
  /** VM 항목: ⇥가 이 VM의 동작을 보인다. */
  vm?: string;
  run(): void;
}

/** 확인 없이 바로 하는 동작(설계 §5.2, 시작은 이름이 정확히 같을 때만). 나머지는 같은 확인 대화상자를 연다. */
const DIRECT: ReadonlySet<Verb> = new Set(["start", "console", "open"]);
/** VM 이름만 쳤을 때 보이는 동작의 순서 */
const VM_VERBS: readonly Verb[] = [
  "start",
  "stop",
  "reboot",
  "force-stop",
  "migrate",
  "snapshot",
  "console",
];

/** 동작마다 VM에 있어야 하는 권한(서버 경로표와 같다). 열기는 보이는 VM이면 된다. */
const VERB_PRIVILEGE: Partial<Record<Verb, Privilege>> = {
  start: "vm.power",
  stop: "vm.power",
  reboot: "vm.power",
  "force-stop": "vm.power",
  migrate: "vm.migrate",
  snapshot: "vm.snapshot",
  console: "vm.console",
};

/** 노드 API 주소(`host:port`, `[v6]:port`)의 호스트 */
function hostOf(address: string): string {
  return address.replace(/:\d+$/, "").replace(/^\[|\]$/g, "");
}

/** VM의 IP·MAC: cloud-init 고정 IP, NIC MAC, 이미 열어 본 게스트 정보(설계 §5.2 — DHCP IP는 게스트 탭을 연 VM만) */
function vmAddresses(vm: Vm, guest: ReadonlyMap<string, string[]>): string[] {
  const statics = (vm.spec.cloud_init?.networks ?? []).flatMap((net) =>
    [net.ipv4, net.ipv6].flatMap((c) => (c?.mode === "static" ? [c.address] : [])),
  );
  const macs = (vm.spec.nics ?? []).flatMap((nic) => (nic.mac ? [nic.mac] : []));
  const ips = [...statics, ...(guest.get(vm.id) ?? [])].map((a) => a.split("/")[0] ?? a);
  return [...new Set([...ips, ...macs])];
}

/** 이름 접두사로 찾기(정확이 앞) */
function byPrefix<T extends { name: string }>(list: readonly T[], prefix: string): T[] {
  return list
    .map((item) => ({ item, score: matchScore(prefix, item.name) }))
    .filter((m) => m.score >= PREFIX)
    .sort((a, b) => b.score - a.score)
    .map((m) => m.item);
}

/**
 * ⌘K 명령 팔레트(설계 §5.2). 리소스 이름·IP, 모든 하위 화면, 만들기·테마·밀도 명령, VM 동작 문법을 한 입력에서
 * 찾는다. 확인이 필요한 동작은 `VmActionHost`로 같은 대화상자를 연다. 닫으면 연 곳으로 포커스를 돌려준다.
 */
export function CommandPalette({
  open,
  onOpenChange,
  returnFocus,
  onHelp,
}: {
  open: boolean;
  onOpenChange(open: boolean): void;
  /** 팔레트를 열 때 포커스가 있던 곳 */
  returnFocus: RefObject<HTMLElement | null>;
  onHelp(): void;
}) {
  const { t } = useTranslation();
  // 항목을 실행해 다른 화면·대화상자로 갔으면 포커스를 돌려주지 않는다(그쪽이 가져간다).
  const restore = useRef(true);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        data-palette=""
        showCloseButton={false}
        aria-describedby={undefined}
        className="top-[12vh] translate-y-0 gap-0 overflow-hidden p-0 sm:max-w-xl"
        onOpenAutoFocus={() => {
          restore.current = true;
        }}
        onCloseAutoFocus={(e) => {
          e.preventDefault();
          const target = returnFocus.current;
          if (restore.current && target?.isConnected) {
            target.focus();
          }
        }}
      >
        <DialogTitle className="sr-only">{t("palette.title")}</DialogTitle>
        <PaletteBody
          returnFocus={returnFocus}
          onRun={(item) => {
            restore.current = false;
            onOpenChange(false);
            item.run();
          }}
          onHelp={onHelp}
        />
      </DialogContent>
    </Dialog>
  );
}

function PaletteBody({
  returnFocus,
  onRun,
  onHelp,
}: {
  returnFocus: RefObject<HTMLElement | null>;
  onRun(item: Item): void;
  onHelp(): void;
}) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const items = usePaletteItems(query, returnFocus, onHelp);
  const listId = useId();
  const optionId = (i: number) => `${listId}-o${i}`;
  const current = items.length > 0 ? Math.min(active, items.length - 1) : -1;

  const move = (to: number) => {
    const next = (to + items.length) % items.length;
    setActive(next);
    document.getElementById(optionId(next))?.scrollIntoView?.({ block: "nearest" });
  };
  const choose = (item: Item | undefined, newTab = false) => {
    if (!item || item.disabled) {
      return;
    }
    if (newTab && item.to) {
      window.open(item.to, "_blank", "noopener");
      return;
    }
    onRun(item);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    // 한글 입력의 마지막 글자를 끝내는 Enter는 항목을 실행하지 않는다.
    // Safari는 compositionend를 keydown보다 먼저 보내 그 Enter의 isComposing이 false이고 keyCode만 229다.
    if (items.length === 0 || e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) {
      return;
    }
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        move(current + 1);
        break;
      case "ArrowUp":
        e.preventDefault();
        move(current - 1);
        break;
      case "Home":
      case "End":
        if (e.ctrlKey) {
          e.preventDefault();
          move(e.key === "Home" ? 0 : items.length - 1);
        }
        break;
      case "Enter":
        e.preventDefault();
        choose(items[current], e.metaKey || e.ctrlKey);
        break;
      case "Tab": {
        const vm = items[current]?.vm;
        if (vm && !e.shiftKey) {
          e.preventDefault();
          setQuery(vm);
          setActive(0);
        }
        break;
      }
    }
  };

  return (
    <>
      <div className="flex items-center gap-2 border-b px-3">
        <ACTION_ICONS.search className="size-4 shrink-0 text-muted-foreground" aria-hidden />
        <input
          role="combobox"
          aria-expanded={items.length > 0}
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={current >= 0 ? optionId(current) : undefined}
          aria-label={t("palette.title")}
          placeholder={t("palette.placeholder")}
          autoComplete="off"
          spellCheck={false}
          className="h-12 min-w-0 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setActive(0);
          }}
          onKeyDown={onKeyDown}
        />
        <Kbd>esc</Kbd>
      </div>
      <div
        id={listId}
        role="listbox"
        aria-label={t("palette.results")}
        className="max-h-[min(60vh,28rem)] overflow-y-auto p-1"
      >
        {items.length === 0 && (
          <p role="presentation" className="px-3 py-6 text-center text-sm text-muted-foreground">
            {t("palette.empty")}
          </p>
        )}
        {GROUPS.map((group) => {
          const members = items.flatMap((item, i) => (item.group === group ? [{ item, i }] : []));
          if (members.length === 0) {
            return null;
          }
          const headId = `${listId}-${group}`;
          return (
            <div key={group} role="group" aria-labelledby={headId} className="py-1">
              <div
                id={headId}
                role="presentation"
                className="px-2 py-1 text-xs font-medium text-muted-foreground"
              >
                {t(`palette.group.${group}`)}
              </div>
              {members.map(({ item, i }) => {
                return (
                  <div
                    key={item.id}
                    id={optionId(i)}
                    role="option"
                    aria-selected={i === current}
                    aria-disabled={item.disabled ? true : undefined}
                    className={cn(
                      "flex min-h-9 cursor-default items-center gap-2 rounded-md px-2 py-1.5 text-sm",
                      i === current && "bg-accent text-accent-foreground",
                      item.disabled && "opacity-60",
                    )}
                    onMouseMove={() => i !== current && setActive(i)}
                    onMouseDown={(e) => e.preventDefault()}
                    onClick={(e) => choose(item, e.metaKey || e.ctrlKey)}
                  >
                    <span className="truncate">{item.label}</span>
                    {item.detail && (
                      <span className="truncate text-xs text-muted-foreground">{item.detail}</span>
                    )}
                    {item.disabled && (
                      <span className="ml-auto shrink-0 text-xs text-muted-foreground">
                        {item.disabled}
                      </span>
                    )}
                  </div>
                );
              })}
            </div>
          );
        })}
      </div>
      <div
        aria-hidden
        className="flex flex-wrap gap-x-3 gap-y-1 border-t px-3 py-2 text-xs text-muted-foreground"
      >
        <span>
          <Kbd>↑↓</Kbd> {t("palette.hint.select")}
        </span>
        <span>
          <Kbd>↵</Kbd> {t("palette.hint.run")}
        </span>
        <span>
          <Kbd>{keyLabel("mod+k").replace("K", "↵")}</Kbd> {t("palette.hint.newTab")}
        </span>
        <span>
          <Kbd>⇥</Kbd> {t("palette.hint.actions")}
        </span>
      </div>
    </>
  );
}

/** 질의에 맞는 항목들(그룹 순서대로). 목록은 실시간 캐시에서 읽고 새 요청을 하지 않는다(게스트 정보도 캐시만). */
function usePaletteItems(
  query: string,
  returnFocus: RefObject<HTMLElement | null>,
  onHelp: () => void,
): Item[] {
  const { t } = useTranslation();
  const me = usePrincipal();
  const { logout } = useAuth();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const { setTheme } = useTheme();
  const { setDensity } = useDensity();
  const host = useVmActionHost();
  const { run } = useVmActions();
  const taskIndex = useTaskIndex();
  const refs = useRefs(me.user);
  const vms = useList("vm");
  const nodes = useList("node");
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const networks = useList("network");
  const groups = useList("security-group");
  const jobs = useList("backup-job");
  const ha = useList("ha-group");
  // 읽을 수 있는 폴더만 온다(서버가 거른다). 이름은 경로다.
  const folders = useList("folder");
  const folderPath = useFolderPaths();

  const go = (to: string) => () => void navigate(to);
  const perms = usePermissions();
  // VM마다 권한이 다르다(폴더·객체 grant). 어디서든 동작 권한이 하나라도 있어야 동작 항목을 찾는다.
  const operate = (Object.values(VERB_PRIVILEGE) as Privilege[]).some((p) => perms.anywhere(p));

  const resources = useMemo(() => {
    const guest = new Map<string, string[]>();
    for (const [key, data] of queryClient.getQueriesData<GuestInfoResponse>({
      queryKey: ["guest"],
    })) {
      if (data?.info && typeof key[1] === "string") {
        guest.set(key[1], guestAddresses(data.info));
      }
    }
    const nodeName = new Map((nodes.data ?? []).map((n) => [n.id, n.name]));
    const list: Omit<Item, "run" | "pinned" | "recent">[] = [];
    for (const vm of vms.data ?? []) {
      const node = nodeName.get(vm.spec.node ?? vm.status.node ?? "");
      const addresses = vmAddresses(vm, guest);
      const ip = addresses.find((a) => !a.includes(":") || a.includes("."));
      list.push({
        id: `vm:${vm.id}`,
        group: "vms",
        label: vm.name,
        detail: [t(`vms.phase.${vm.status.phase}`), node, ip].filter(Boolean).join(" · "),
        keywords: node ? [node] : [],
        addresses,
        to: `/vms/${encodeURIComponent(vm.name)}`,
        vm: vm.name,
      });
    }
    for (const node of nodes.data ?? []) {
      list.push({
        id: `node:${node.id}`,
        group: "nodes",
        label: node.name,
        detail: node.spec.address,
        addresses: [hostOf(node.spec.address)],
        to: `/nodes/${encodeURIComponent(node.name)}`,
      });
    }
    const simple = (
      group: Group,
      kind: string,
      items: readonly { id: string; name: string }[] | undefined,
      to: (name: string) => string,
    ) => {
      for (const item of items ?? []) {
        list.push({ id: `${kind}:${item.id}`, group, label: item.name, to: to(item.name) });
      }
    };
    simple("pools", "storage-pool", pools.data, () => "/storage-pools");
    simple("volumes", "volume", volumes.data, () => "/volumes");
    simple("networks", "network", networks.data, () => "/networks");
    simple(
      "securityGroups",
      "security-group",
      groups.data,
      (name) => `/security-groups/${encodeURIComponent(name)}`,
    );
    simple("backupJobs", "backup-job", jobs.data, () => "/backup-jobs");
    simple("haGroups", "ha-group", ha.data, () => "/ha-groups");
    for (const f of folders.data ?? []) {
      list.push({
        id: `folder:${f.id}`,
        group: "folders",
        label: folderPath(f.id),
        to: `${FOLDER_PATHS[f.spec.kind]}/${f.id}`,
      });
    }
    return list;
  }, [
    t,
    queryClient,
    vms.data,
    nodes.data,
    pools.data,
    volumes.data,
    networks.data,
    groups.data,
    jobs.data,
    ha.data,
    folders.data,
    folderPath,
  ]);

  const navigation = useMemo(() => {
    const list: Omit<Item, "run">[] = [];
    for (const area of AREAS) {
      for (const tab of visibleTabs(area, perms)) {
        const leaves = tab.views ?? [{ to: tab.to, key: tab.key }];
        for (const leaf of leaves) {
          const label = area.id === "home" ? t(area.key) : `${t(area.key)} › ${t(leaf.key)}`;
          list.push({ id: `nav:${leaf.to}`, group: "navigation", label, to: leaf.to });
        }
      }
    }
    return list;
  }, [t, perms]);

  // 지운 리소스는 빼고 이름은 지금 이름으로
  const lists = {
    vm: vms.data,
    node: nodes.data,
    "storage-pool": pools.data,
    "security-group": groups.data,
  };
  const pins = alive(refs.pins, lists);
  const recent = alive(refs.recent, lists);
  const refKey = (ref: ResourceRef) => `${ref.kind}:${ref.id}`;
  const pinned = new Set(pins.map(refKey));
  const recentAt = new Map(recent.map((ref, i) => [refKey(ref), i]));

  const q = query.trim();
  const out: Item[] = [];

  if (!q) {
    const byId = new Map(resources.map((r) => [r.id, r]));
    const refItem = (ref: ResourceRef, group: Group, i: number): Item[] => {
      const found = byId.get(refKey(ref));
      return found ? [{ ...found, id: `${group}:${i}`, group, run: go(found.to!) }] : [];
    };
    out.push(...pins.flatMap((ref, i) => refItem(ref, "favorites", i)).slice(0, 8));
    out.push(
      ...recent
        .filter((ref) => !pinned.has(refKey(ref)))
        .flatMap((ref, i) => refItem(ref, "recent", i))
        .slice(0, 8),
    );
  }

  if (operate && q) {
    out.push(...vmActions(q));
  }

  const candidates: Item[] = [
    ...(q
      ? resources.map((r) => ({
          ...r,
          pinned: pinned.has(r.id),
          recent: recentAt.get(r.id),
          run: go(r.to!),
        }))
      : []),
    ...navigation.map((n) => ({ ...n, run: go(n.to!) })),
    ...commands(),
  ];
  out.push(...rank(candidates, q));
  // 보이는 차례(그룹 순서)대로. 화살표 키도 이 차례로 옮긴다.
  return out
    .map((item, i) => ({ item, i }))
    .sort((a, b) => GROUPS.indexOf(a.item.group) - GROUPS.indexOf(b.item.group) || a.i - b.i)
    .map(({ item }) => item);

  /** VM 동작 항목: 문법(`migrate web to node2`, `web 시작`)이거나 VM 이름 그대로면 그 VM의 동작들 */
  function vmActions(text: string): Item[] {
    const command = parseCommand(text);
    const all = vms.data ?? [];
    if (command) {
      const target = command.target.toLowerCase();
      return byPrefix(
        all.filter((vm) => allowed(vm, command.verb)),
        command.target,
      )
        .slice(0, 8)
        .map((vm) => actionItem(vm, command.verb, vm.name.toLowerCase() === target, command.node));
    }
    const exact = all.find((vm) => vm.name.toLowerCase() === text.toLowerCase());
    return exact
      ? VM_VERBS.filter((verb) => allowed(exact, verb)).map((verb) => actionItem(exact, verb, true))
      : [];
  }

  /** 이 VM에 이 동작의 권한이 있다(없는 동작은 항목을 보이지 않는다). 옮기기는 대상 노드를 알아야 한다. */
  function allowed(vm: Vm, verb: Verb): boolean {
    const privilege = VERB_PRIVILEGE[verb];
    if (verb === "migrate" && !perms.reads("node")) {
      return false;
    }
    return !privilege || perms.has(privilege, objectTarget("vm", vm));
  }

  /**
   * `exact`: 친 이름이 이 VM의 이름과 정확히 같다. 시작은 그때만 바로 하고(`start db`가 `db-prod`를 켜지 않게),
   * 앞부분만 맞으면 확인 대화상자를 연다.
   */
  function actionItem(vm: Vm, verb: Verb, exact: boolean, nodeText?: string): Item {
    let node: Node | undefined;
    let disabled: string | undefined;
    if (verb === "migrate" && nodeText !== undefined) {
      node = byPrefix(nodes.data ?? [], nodeText)[0];
      if (!node) {
        disabled = t("palette.noNode", { node: nodeText });
      } else if (!migrationTargets(vm, nodes.data ?? []).some((n) => n.id === node!.id)) {
        disabled = t("palette.notTarget", { node: node.name });
      }
    }
    const vmTasks = taskIndex.get(vm.id) ?? [];
    if (!disabled && verb === "snapshot" && isBusy(vm)) {
      disabled = t("vms.reason.busy");
    } else if (!disabled && verb !== "snapshot" && verb !== "open") {
      const able = availability(vm, verb, vmTasks);
      if (!able.ok) {
        disabled = t(able.reason);
      }
    }
    const direct = DIRECT.has(verb) && (verb !== "start" || exact);
    const path = `/vms/${encodeURIComponent(vm.name)}`;
    const label = node
      ? t("palette.verb.migrateTo", { name: vm.name, node: node.name })
      : t(`palette.verb.${verb}`, { name: vm.name });
    return {
      id: `action:${verb}:${vm.id}`,
      group: "actions",
      label: direct ? label : `${label}…`,
      disabled,
      to: verb === "open" ? path : verb === "console" ? `${path}/console` : undefined,
      run: () => {
        if (verb === "open") {
          void navigate(path);
        } else if (verb === "console") {
          void navigate(`${path}/console`);
        } else if (direct) {
          void run(vm, "start");
        } else {
          host.open({ vm, which: verb, node: node?.id, returnFocus });
        }
      },
    };
  }

  function commands(): Item[] {
    const list: Item[] = createItems(perms).map((item) => ({
      id: `create:${item.id}`,
      group: "commands",
      label: t("palette.create", { what: t(`create.${item.id}`) }),
      keywords: [t("common.create"), "new"],
      to: item.to,
      run: go(item.to),
    }));
    for (const theme of ["light", "dark", "system"] as const) {
      list.push({
        id: `theme:${theme}`,
        group: "commands",
        label: t("palette.theme", {
          theme: t(`header.theme${theme[0]!.toUpperCase()}${theme.slice(1)}`),
        }),
        keywords: [t("header.theme"), "theme"],
        run: () => setTheme(theme),
      });
    }
    for (const density of ["comfortable", "compact"] as const) {
      list.push({
        id: `density:${density}`,
        group: "commands",
        label: t("palette.density", {
          density: t(`header.density${density[0]!.toUpperCase()}${density.slice(1)}`),
        }),
        keywords: [t("header.density"), "density"],
        run: () => setDensity(density),
      });
    }
    list.push(
      {
        id: "help",
        group: "commands",
        label: t("shortcuts.title"),
        keywords: ["shortcuts", "help", "?"],
        run: onHelp,
      },
      {
        id: "logout",
        group: "commands",
        label: t("header.logout"),
        keywords: ["logout", "sign out"],
        run: () => void logout(),
      },
    );
    return list;
  }
}
