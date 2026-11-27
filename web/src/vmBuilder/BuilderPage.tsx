// VM 만들기 한 화면(계획 77-5 — 계획 71 빌더와 폼을 합침, 진단 §4.3): `/vms/-new`. 첫 단계는 용도 카드(`?purpose=` 없음),
// 고르면 VM 그림 화면이다 — 왼쪽 짧은 부품 상자(누르면 붙음, 끌어도 됨), 가운데 VM 그림(본체 둘레에 CPU·메모리·디스크·CD·
// NIC·첫 부팅·보호·배치 칸, 붙은 부품은 그림 위 카드로 고침), 오른쪽 고정 요약·검사(이름·폴더·노드와 여유 막대·서버 미리
// 확인·만들기), 고급은 오른쪽 서랍. 값은 폼과 같은 `VmFormValues`이고 요청도 같은 함수(`requests`)다. 1280px 이상은 한 화면
// (부품 상자만 안에서 스크롤), 그 밑은 차례대로 쌓고(그림 → 요약) 만들기는 아래 고정 띠다.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useSearchParams } from "react-router";

import { ApiError, api, call } from "@/api/client";
import { useList } from "@/api/live";
import { usePermissions, usePermissionsReady } from "@/auth/usePermissions";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { archOf, useCpuModels } from "@/cpu/cpu";
import { DndProvider, useDnd } from "@/dnd/DndProvider";
import { folderTree } from "@/folders/folders";
import { useFolderUsage } from "@/folders/UsageBars";
import { errorMessage } from "@/i18n/errors";
import { trackTask } from "@/tasks/tasks";
import { Chassis } from "@/vmBuilder/Chassis";
import { BuilderContext, type BuilderApi } from "@/vmBuilder/context";
import { Advanced } from "@/vmBuilder/Advanced";
import { defaultFolder } from "@/vmBuilder/draft";
import type { Thing } from "@/vmBuilder/labels";
import { accepts } from "@/vmBuilder/model/accepts";
import { drop } from "@/vmBuilder/model/drop";
import { blocking, browserProblems, type Problem, problems } from "@/vmBuilder/model/problems";
import { checkBody, requests, type RuleRequest, zoneRule } from "@/vmBuilder/model/requests";
import { type Purpose, PURPOSES, applyPurpose } from "@/vmBuilder/model/purposes";
import { listOf, slotOf } from "@/vmBuilder/model/slots";
import {
  type BuilderCtx,
  type BuilderState,
  type History,
  initialState,
  record,
  undo,
} from "@/vmBuilder/model/state";
import { PartsPanel } from "@/vmBuilder/PartsPanel";
import { Purposes } from "@/vmBuilder/Purposes";
import { focusProblem, Summary } from "@/vmBuilder/Summary";
import { useServerCheck } from "@/vmBuilder/useServerCheck";
import { defaultMachine, type VmFormValues } from "@/vms/form";
import { postVolume } from "@/vms/form/submit";

/** 서버 미리 확인 기능(`orbivisor_types::node::VM_CHECK`) */
const VM_CHECK = "vm-check";

// 만들기 화면만의 모양: 첫 화면 CSS(Tailwind)에 넣지 않고 이 조각 안에 둔다(계획 71 설계 §4). 1280px 이상은 머리줄(3.5rem)과
// 본문 여백(위아래 1.5rem)을 뺀 높이에 셋 칸이 들어가고 부품 상자만 안에서 스크롤한다.
const STYLE = `
.vb-grid{display:grid;gap:1rem;grid-template-columns:minmax(0,1fr)}
@media(min-width:1280px){.vb-page{height:calc(100dvh - 6.5rem);min-height:36rem}
.vb-grid{flex:1;min-height:0;grid-template-columns:14rem minmax(0,1fr) 19rem;align-items:stretch}
.vb-col{min-height:0;display:flex;flex-direction:column;overflow-y:auto}
.vb-scroll{min-height:0;flex:1;overflow-y:auto;overscroll-behavior:contain;padding-right:.25rem}}
.vb [data-drop=ok]{outline:2px solid var(--brand);outline-offset:2px}
.vb [data-drop=ok][data-over]{background:color-mix(in oklab,var(--brand) 12%,transparent)}
.vb [data-held]{outline:2px dashed var(--brand);outline-offset:1px}
.vb-part{touch-action:pan-y;user-select:none;-webkit-user-select:none;-webkit-touch-callout:none}
.vb-bar{position:sticky;bottom:0;padding-bottom:max(.5rem,env(safe-area-inset-bottom))}
.vb-range{height:44px;width:100%;accent-color:var(--brand)}
.vb-pre{max-height:60vh}
.vb [data-level=error]{border-color:var(--danger)}
.vb [data-level=warn]{border-color:var(--warning)}
.vb p[data-level=error],.vb button[data-level=error]{color:var(--danger-fg)}
.vb p[data-level=warn],.vb button[data-level=warn]{color:var(--warning-fg)}
.vb-pic{display:grid;gap:.75rem;grid-template-columns:minmax(0,1fr)}
@media(min-width:640px){.vb-pic{grid-template-columns:minmax(0,1fr) minmax(9.5rem,32%);grid-template-areas:"cmp cmp" "box nic";align-items:start}
.vb-cmp{grid-area:cmp}.vb-chassis{grid-area:box}.vb-nic{grid-area:nic}
.vb-port{position:relative}
.vb-port-t::after{content:"";position:absolute;bottom:-.8rem;left:35%;height:.75rem;border-left:3px solid var(--border)}
.vb-port-r::after{content:"";position:absolute;top:1.4rem;left:-.8rem;width:.75rem;border-top:3px solid var(--border)}}
.vb-box{border-width:2px;border-radius:1rem;background:linear-gradient(var(--card),var(--card)) padding-box}
.vb-led{width:.5rem;height:.5rem;border-radius:9999px;background:var(--border)}
.vb-bars{height:.5rem;border-radius:9999px;background:var(--muted);overflow:hidden;display:flex}
.vb-bars>span{display:block;height:100%}
`;

export function BuilderPage() {
  const perms = usePermissions();
  const ready = usePermissionsReady();
  const nodes = useList("node");
  const folders = useList("folder");
  const [params] = useSearchParams();
  const readsNodes = perms.reads("node");
  if (!ready || (readsNodes && !nodes.data) || !folders.data) {
    return <ListPending error={nodes.error ?? folders.error} />;
  }
  const folder = defaultFolder(perms, folderTree(folders.data, "vm"), params.get("folder"));
  return <Builder machine={readsNodes ? defaultMachine(nodes.data ?? []) : ""} folder={folder} />;
}

/** 주소의 용도(`?purpose=`) — 모르는 값은 없음(카드부터) */
function purposeOf(params: URLSearchParams): Purpose | null {
  const p = params.get("purpose");
  return PURPOSES.find((x) => x === p) ?? null;
}

/** 규칙 요청의 비교 열쇠(이름과 내용) */
const ruleKey = (r: RuleRequest) => JSON.stringify(r);

/** 이 화면이 만든 규칙: 서버 id, 요청 열쇠, 걸린 VM 이름 */
interface MadeRule {
  id: string;
  key: string;
  vm: string;
}

/** 새로 만든 볼륨을 있는 볼륨으로(다시 보낼 때 또 만들지 않음 — 폼 규칙) */
function markExisting(s: BuilderState, volume: string): BuilderState {
  return {
    ...s,
    values: {
      ...s.values,
      disks: s.values.disks.map((d) =>
        d.kind === "new" && d.volume === volume ? { ...d, kind: "existing" } : d,
      ),
    },
  };
}

function Builder({ machine, folder }: { machine: VmFormValues["machine"]; folder: string }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const perms = usePermissions();
  const readsNodes = perms.reads("node");
  const nodes = useList("node");
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const networks = useList("network");
  const securityGroups = useList("security-group");
  const haGroups = useList("ha-group");
  const folders = useList("folder");
  const vms = useList("vm");
  const rules = useList("placement-rule");
  const [params, setParams] = useSearchParams();
  const purpose = purposeOf(params);
  const [hist, setHist] = useState<History>(() => ({
    past: [],
    present: initialState(machine, folder),
  }));
  const apply = (next: BuilderState) => setHist((h) => record(h, next));
  // 주소로 바로 연 용도(`?purpose=linux`)는 세상을 읽은 뒤 한 번 얹는다(카드에서 고르면 그때 얹음).
  const [applied, setApplied] = useState<Purpose | null>(null);
  const s = hist.present;
  const cpu = useCpuModels(archOf(s.values.machine), readsNodes);
  // 클러스터 CPU 기준이 있으면 새 VM의 기본 모델은 `cluster`다(계획 63 §13 — 폼과 같음). 한 번만, 사용자가 고르기 전에.
  const clusterNow = cpu.data?.models.find((m) => m.kind === "cluster")?.current;
  const [clustered, setClustered] = useState(false);
  if (clusterNow && !clustered && s.values.model === "host" && s.base.kind === "blank") {
    setClustered(true);
    setHist((h) => ({
      ...h,
      present: { ...h.present, values: { ...h.present.values, model: "cluster" } },
    }));
  }
  // 노드 부하(메모리 여유) — 지도와 같은 질의 열쇠(`placement/balance.ts` `PLACEMENT_KEY`)
  const placement = useQuery({
    queryKey: ["placement"],
    queryFn: () => call(api.GET("/placement")),
    retry: false,
    enabled: readsNodes,
  });
  const usage = useFolderUsage(s.values.folder, s.values.folder !== "");
  const ctx = useMemo<BuilderCtx>(
    () => ({
      nodes: readsNodes ? (nodes.data ?? []) : null,
      pools: pools.data ?? [],
      volumes: volumes.data ?? [],
      networks: networks.data ?? [],
      securityGroups: securityGroups.data ?? [],
      haGroups: haGroups.data ?? [],
      folders: folders.data ?? [],
      vms: vms.data ?? [],
      rules: rules.data ?? [],
      cpuModels: cpu.data?.models,
      loads: placement.data?.nodes,
      usage: usage.data,
      perms,
    }),
    [
      readsNodes,
      nodes.data,
      pools.data,
      volumes.data,
      networks.data,
      securityGroups.data,
      haGroups.data,
      folders.data,
      vms.data,
      rules.data,
      cpu.data,
      placement.data,
      usage.data,
      perms,
    ],
  );

  // 세상(풀·네트워크·템플릿)을 다 읽었는지 — 주소의 용도를 얹을 때 기본 풀·네트워크를 고르려면 필요하다.
  const loaded = [pools, volumes, networks, vms].every((q) => q.data !== undefined);
  const templateId = params.get("template") ?? undefined;
  if (purpose && purpose !== applied && loaded) {
    setApplied(purpose);
    setHist((h) =>
      record(h, applyPurpose(initialState(machine, folder), purpose, ctx, templateId)),
    );
  }
  /** 카드를 골랐다: 처음 틀에 그 용도를 얹고(되돌리기에 쌓음) 주소에 적는다(뒤로 가면 카드). */
  const choose = (p: Purpose, template?: string) => {
    setApplied(p);
    apply(applyPurpose(initialState(machine, folder), p, ctx, template));
    const next = new URLSearchParams(params);
    next.set("purpose", p);
    if (template) {
      next.set("template", template);
    } else {
      next.delete("template");
    }
    setParams(next);
  };
  // 만들기를 누른 뒤에만, 또는 이름 칸을 떠난 뒤에만 이름 오류를 빨갛게 한다(설계 77-5 — 치기 전에는 조용히).
  const [attempted, setAttempted] = useState(false);
  const [touched, setTouched] = useState(false);
  const [advanced, setAdvanced] = useState(false);

  // 이번 화면에서 만든 배치 규칙(이름 → id·열쇠·VM 이름). VM을 만들지 못한 채 남았다: 다시 만들 때 지금 영역 규칙과
  // 같으면 다시 쓰고, 영역·hard/soft·VM 이름이 바뀌었거나 영역을 뺐으면 곧 지운다(이 화면이 방금 만든, 아직 VM 없는 규칙).
  const madeRules = useRef(new Map<string, MadeRule>());
  const dropping = useRef(new Map<string, Promise<void>>());
  // 만든 규칙(그리기용 — `madeRules`를 바꿀 때마다 맞춤)
  const [made, setMade] = useState<ReadonlyMap<string, MadeRule>>(new Map());
  const syncMade = () => setMade(new Map(madeRules.current));
  // 이번 화면에서 만든 볼륨(다시 실패해도 알림에 남김)
  const madeVolumes = useRef<string[]>([]);
  const want = zoneRule(s);
  const wantKey = want && ruleKey(want);

  const browser = useMemo(() => browserProblems(s, ctx), [s, ctx]);
  // 같은 것(이름·내용)을 만든 규칙은 이미 있다 — 확인 본문의 "만들 규칙"에서 뺀다(넣으면 서버가 name_taken).
  const body = useMemo(
    () => checkBody(s, ctx, (r) => made.get(r.name)?.key === ruleKey(r)),
    [s, ctx, made],
  );
  // 지울 만든 규칙이 아직 이 VM에 걸려 있으면(지우는 중·지우지 못함) 서버는 지난 규칙으로 확인한다 — 지운 뒤에 묻는다.
  const held = [...made.values()].some((r) => r.key !== wantKey && r.vm === s.values.name);
  // 기능 `vm-check`가 없는 노드(위트니스 제외)가 있으면 묻지 않는다. 노드를 읽지 못하면 묻고 409를 받는다.
  const supported = !(ctx.nodes ?? []).some(
    (n) => !n.spec.witness && !(n.status.features ?? []).includes(VM_CHECK),
  );
  const server = useServerCheck(body, !blocking(browser), supported, held);
  const all = useMemo(
    () => problems(s, ctx, server.stale ? null : server.check),
    [s, ctx, server.stale, server.check],
  );
  // 보이는 문제: 이름을 아직 건드리지 않았으면 이름 문제와 이름을 따라갈 새 디스크의 빈 볼륨 이름은 숨긴다(만들기는
  // 그래도 막힌다 — 누르면 보인다).
  const quiet = !attempted && !touched;
  const list = useMemo(
    () => (quiet ? all.filter((p) => !waitsForName(p, s)) : all),
    [all, quiet, s],
  );

  const [selected, select] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // 알림: 만들기가 실패했으면(`volumes`는 그때까지 만든 볼륨) "이미 만든 것"에 지금 남은 만든 규칙을 더해 그린다.
  const [failure, setFailure] = useState<{ message: string; volumes?: string[] } | null>(null);
  const leftAlert = useCallback(
    (name: string, err: unknown) =>
      setFailure({ message: t("vmBuilder.page.ruleLeft", { name, error: errorMessage(t, err) }) }),
    [t],
  );
  // 놓은 뒤 초점을 줄 곳(블록 `data-block` 또는 칸 제목 id — 그린 뒤에 준다)
  const focusTo = useRef<{ block: string } | { heading: string } | null>(null);
  const [said, setSaid] = useState("");
  const abort = useRef<AbortController | null>(null);
  useEffect(() => () => abort.current?.abort(), []);

  // 놓은 뒤 초점(설계 §6.1 "끝난 뒤 초점"): 새·옮긴 블록, 아니면 놓은 칸의 제목
  useEffect(() => {
    const to = focusTo.current;
    if (to) {
      const el =
        "block" in to
          ? document.querySelector<HTMLElement>(`[data-block="${to.block}"]`)
          : document.getElementById(`vb-${to.heading}`);
      el?.focus();
      focusTo.current = null;
    }
  });

  /** 만든 규칙을 지운다(같은 이름을 지우는 중이면 그것을 기다림). 이미 없으면(404) 지운 셈이다. */
  const dropRule = useCallback((name: string): Promise<void> => {
    const had = madeRules.current.get(name);
    if (!had) {
      return Promise.resolve();
    }
    let p = dropping.current.get(name);
    if (!p) {
      p = call(api.DELETE("/placement-rules/{key}", { params: { path: { key: had.id } } }))
        .catch((err: unknown) => {
          if (!(err instanceof ApiError && err.status === 404)) {
            throw err;
          }
        })
        .then(() => {
          madeRules.current.delete(name);
          setMade(new Map(madeRules.current));
        })
        .finally(() => dropping.current.delete(name));
      dropping.current.set(name, p);
    }
    return p;
  }, []);
  useEffect(() => {
    // 만드는 중에는 두지 않는다(VM 요청이 날고 있는 동안 그 규칙을 지우지 않게).
    if (busy) {
      return;
    }
    for (const [name, r] of madeRules.current) {
      if (r.key !== wantKey) {
        dropRule(name).catch((err: unknown) => leftAlert(name, err));
      }
    }
  }, [wantKey, made, busy, dropRule, leftAlert]);

  const builder: BuilderApi = {
    s,
    ctx,
    list,
    busy,
    selected,
    select,
    touch: () => setTouched(true),
    showName: !quiet,
    check: server.stale ? null : server.check,
    openAdvanced: () => setAdvanced(true),
    put(thing, slot) {
      const next = drop(s, thing, slot, ctx);
      if (next === s) {
        return undefined;
      }
      apply(next);
      const target = slotOf(slot);
      const list = target && listOf(target);
      if (thing.kind === "block") {
        // 옮긴 블록으로, 뺐으면 그 목록 제목으로 초점
        focusTo.current = list
          ? { block: `${list.list}.${list.index ?? next.values[list.list].length - 1}` }
          : { heading: thing.list };
      } else if (list && list.index === null) {
        // 목록에 더함: 새 블록으로 초점, 개수를 읽는다.
        const n = next.values[list.list].length;
        focusTo.current = { block: `${list.list}.${n - 1}` };
        return t(`vmBuilder.page.count.${list.list}`, { count: n });
      } else {
        // 블록 줄(디스크 1 등)이면 그 블록, 아니면 칸 제목
        focusTo.current = list ? { block: slot } : { heading: slot };
      }
      if (thing.kind === "ci") {
        select("cloudInit");
      }
      if (thing.kind === "block" && slot === "trash") {
        select(null);
      }
      return undefined;
    },
    edit: (patch) =>
      setHist((h) => ({
        ...h,
        present: { ...h.present, values: { ...h.present.values, ...patch } },
      })),
    set: (next) => setHist((h) => ({ ...h, present: next })),
    record: apply,
    blank() {
      const before = [...hist.past].reverse().find((p) => p.base.kind === "blank");
      apply(before ?? initialState(machine, folder));
    },
  };

  function back() {
    // 든 채·만드는 중에는 되돌리지 않는다(든 블록의 자리가 사라지거나 보내는 값이 바뀜). 든 채는 틀이 `<html>`에 다는
    // `data-dnd-held`로 안다(`lib/shortcuts` `dndHeld`와 같음 — 그것을 가져오면 첫 화면 묶음이 나뉜다).
    const held = document.documentElement.hasAttribute("data-dnd-held");
    if (hist.past.length > 0 && !busy && !held) {
      setHist(undo);
      setSaid(t("vmBuilder.page.undone"));
    }
  }
  // ⌘Z·Ctrl+Z(입력 칸 밖)
  const backRef = useRef(back);
  useEffect(() => {
    backRef.current = back;
  });
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (
        (e.metaKey || e.ctrlKey) &&
        !e.shiftKey &&
        e.key.toLowerCase() === "z" &&
        !(e.target as Element | null)?.closest?.("input, textarea, select, [contenteditable]")
      ) {
        e.preventDefault();
        backRef.current();
      }
    };
    document.addEventListener("keydown", key);
    return () => document.removeEventListener("keydown", key);
  }, []);

  /** 만들기: 막는 문제가 있으면 모두 보이고 첫 문제로 초점, 없으면 만든다. */
  function tryCreate() {
    setAttempted(true);
    const first = blocking(all);
    if (first) {
      focusProblem(first);
      return;
    }
    void create();
  }

  async function create() {
    setBusy(true);
    setFailure(null);
    const controller = new AbortController();
    abort.current = controller;
    const { signal } = controller;
    try {
      // 지우는 중인 지난 규칙을 기다린다(같은 이름을 다시 만들 수 있게).
      await Promise.allSettled([...dropping.current.values()]);
      // 지금 영역 규칙과 다른 만든 규칙(지우지 못했던 것 — 이름이 다른 것도)을 다시 지운다. 못 지우면 멈추고 알린다.
      for (const [name, r] of [...madeRules.current]) {
        if (r.key !== wantKey) {
          try {
            await dropRule(name);
          } catch (err) {
            leftAlert(name, err);
            return;
          }
        }
      }
      // 폼과 같은 차례: 새 볼륨(Task 기다림) → 영역 규칙 → VM(또는 템플릿 배포 Task)
      for (const r of requests(s, ctx)) {
        if (signal.aborted) {
          return;
        }
        if (r.path === "/volumes") {
          const name = r.body.name;
          await postVolume(
            queryClient,
            r.body,
            t("tasks.op.volume-create", { target: name }),
            signal,
          );
          madeVolumes.current.push(name);
          // 되돌리기 쌓기에서도(되돌린 뒤 다시 만들 때 또 보내지 않게)
          setHist((h) => ({
            past: h.past.map((p) => markExisting(p, name)),
            present: markExisting(h.present, name),
          }));
        } else if (r.path === "/placement-rules") {
          const key = ruleKey(r.body);
          const had = madeRules.current.get(r.body.name);
          if (had?.key !== key) {
            // 지난 것과 다르면(지우기가 실패했었음) 먼저 지운다.
            await dropRule(r.body.name);
            const rule = await call(api.POST("/placement-rules", { body: r.body }));
            madeRules.current.set(r.body.name, { id: rule.id, key, vm: s.values.name });
            syncMade();
          }
        } else if (r.path === "/vms") {
          await call(api.POST("/vms", { body: r.body }));
          if (!signal.aborted) {
            void navigate(`/vms/${encodeURIComponent(r.body.name)}`);
          }
        } else {
          const task = await call(
            api.POST("/vms/{key}/clone", { params: { path: { key: r.key } }, body: r.body }),
          );
          const template = ctx.vms.find((v) => v.id === r.key)?.name ?? r.key;
          trackTask(task, t("templates.deployOp", { template, name: r.body.name }));
          if (!signal.aborted) {
            void navigate("/vms");
          }
        }
      }
    } catch (err) {
      if (signal.aborted) {
        return;
      }
      setFailure({ message: errorMessage(t, err), volumes: [...madeVolumes.current] });
    } finally {
      if (!signal.aborted) {
        setBusy(false);
      }
    }
  }

  // 자동으로 지운 규칙은 "이미 만든 것"에서 빠진다.
  const createdNames = failure?.volumes && [...failure.volumes, ...made.keys()];
  const alert =
    failure &&
    (createdNames && createdNames.length > 0
      ? t("vmBuilder.page.created", { names: createdNames.join(", "), error: failure.message })
      : failure.message);
  const narrow = useNarrow();
  const blocked = blocking(all);
  return (
    <BuilderContext.Provider value={builder}>
      <DndProvider<Thing>
        accepts={(item, slot) => accepts(item, slot, s, ctx)}
        onDrop={(item, slot) => builder.put(item, slot)}
      >
        <style>{STYLE}</style>
        <div className="vb vb-page flex flex-col gap-3">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="text-xl font-semibold">{t("vmForm.createTitle")}</h1>
            {purpose && (
              <p className="flex items-center gap-1 text-sm text-muted-foreground">
                <span>
                  {t("vmBuilder.purpose.chosen", { name: t(`vmBuilder.purpose.${purpose}.title`) })}
                </span>
                <Button variant="link" size="sm" className="h-auto px-1" asChild>
                  <Link to={{ search: withoutPurpose(params) }}>
                    {t("vmBuilder.purpose.change")}
                  </Link>
                </Button>
              </p>
            )}
            <div className="ml-auto flex items-center gap-2">
              {purpose && <UndoButton disabled={hist.past.length === 0 || busy} onClick={back} />}
              <Button variant="ghost" size="sm" asChild>
                <Link to="/vms">{t("common.cancel")}</Link>
              </Button>
            </div>
          </div>
          <p role="status" className="sr-only">
            {said}
          </p>
          {!purpose ? (
            <Purposes state={initialState(machine, folder)} onChoose={choose} />
          ) : (
            <div className="vb-grid">
              <PartsPanel narrow={narrow} />
              <Chassis narrow={narrow} />
              <Summary
                server={server}
                failure={alert}
                blocked={attempted ? blocked : null}
                onCreate={tryCreate}
              />
            </div>
          )}
          {advanced && <Advanced onClose={() => setAdvanced(false)} attempted={attempted} />}
        </div>
      </DndProvider>
    </BuilderContext.Provider>
  );
}

/** 이름을 넣기 전이라 생긴 문제(이름 칸, 이름을 따라가는 새 디스크의 빈 볼륨 이름) */
function waitsForName(p: Problem, s: BuilderState): boolean {
  if (p.at === "name") {
    return true;
  }
  const disk = /^disks\.(\d+)$/.exec(p.at);
  const i = disk ? Number(disk[1]) : -1;
  return (
    p.key === "vmForm.errors.volume" && s.autoNames[i] === true && s.values.disks[i]?.volume === ""
  );
}

/** 용도 없는 주소(카드로 돌아가기 — 폴더 등 다른 쿼리는 지킴) */
function withoutPurpose(params: URLSearchParams): string {
  const next = new URLSearchParams(params);
  next.delete("purpose");
  next.delete("template");
  const q = next.toString();
  return q ? `?${q}` : "";
}

/** 되돌리기 단추: 든 채(끌기·누르기·키보드)에는 꺼진다. */
function UndoButton({ disabled, onClick }: { disabled: boolean; onClick(): void }) {
  const { t } = useTranslation();
  const phase = useDnd().state.phase;
  return (
    <Button
      type="button"
      variant="outline"
      size="sm"
      className="ml-auto"
      disabled={disabled || phase === "dragging" || phase === "picked"}
      onClick={onClick}
    >
      {t("vmBuilder.page.undo")}
    </Button>
  );
}

/** 1280px 밑(한 칸으로 쌓고 부품 상자가 서랍이 됨 — 계획 77-5) */
export function useNarrow(): boolean {
  const query = "(max-width: 1279px)";
  const [narrow, setNarrow] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const media = window.matchMedia(query);
    const change = () => setNarrow(media.matches);
    media.addEventListener("change", change);
    return () => media.removeEventListener("change", change);
  }, []);
  return narrow;
}
