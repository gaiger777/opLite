// 끌어 놓기 VM 빌더 화면(계획 71 설계 §8): VM 탭의 만들기 화면 안 보기 "빌더"(`/vms/-new/builder`). 왼쪽 부품 상자, 가운데
// VM 틀과 살피기, 오른쪽 확인·JSON·CLI 판과 만들기. 값은 폼과 같은 `VmFormValues`이고 요청도 같은 함수(`requests`)다.
// 1024px 밑에서는 한 칸이 되고 부품 상자는 아래 서랍이다.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useSearchParams } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { usePrincipal } from "@/auth/AuthProvider";
import { usePermissions, usePermissionsReady } from "@/auth/usePermissions";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { archOf, useCpuModels } from "@/cpu/cpu";
import { DndProvider } from "@/dnd/DndProvider";
import { folderTree } from "@/folders/folders";
import { useFolderUsage } from "@/folders/UsageBars";
import { errorMessage } from "@/i18n/errors";
import { trackTask } from "@/tasks/tasks";
import { Chassis } from "@/vmBuilder/Chassis";
import { BuilderContext, type BuilderApi } from "@/vmBuilder/context";
import { clearDraft, defaultFolder, putDraft, readDraft, writeMode } from "@/vmBuilder/draft";
import type { Thing } from "@/vmBuilder/labels";
import { accepts } from "@/vmBuilder/model/accepts";
import { drop } from "@/vmBuilder/model/drop";
import { blocking, browserProblems, problems } from "@/vmBuilder/model/problems";
import { checkBody, requests } from "@/vmBuilder/model/requests";
import { listOf, slotOf } from "@/vmBuilder/model/slots";
import {
  type BuilderCtx,
  type BuilderState,
  fromValues,
  type History,
  initialState,
  record,
  undo,
} from "@/vmBuilder/model/state";
import { PartsPanel } from "@/vmBuilder/PartsPanel";
import { SidePanel } from "@/vmBuilder/SidePanel";
import { useServerCheck } from "@/vmBuilder/useServerCheck";
import { defaultMachine, type VmFormValues } from "@/vms/form";
import { postVolume } from "@/vms/form/submit";
import { ViewSwitch } from "@/vmBuilder/ViewSwitch";

/** 서버 미리 확인 기능(`opl_types::node::VM_CHECK`) */
const VM_CHECK = "vm-check";

// 빌더만의 모양: 첫 화면 CSS(Tailwind)에 넣지 않고 이 조각 안에 둔다(설계 §4).
const STYLE = `
.vb-grid{display:grid;gap:1rem;grid-template-columns:minmax(0,1fr)}
@media(min-width:1024px){.vb-grid{grid-template-columns:16rem minmax(0,1fr) 20rem;align-items:start}
.vb-side{position:sticky;top:1rem}}
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
`;

export function BuilderPage() {
  const perms = usePermissions();
  const ready = usePermissionsReady();
  const nodes = useList("node");
  const folders = useList("folder");
  const [params] = useSearchParams();
  const { user } = usePrincipal();
  // 이 보기를 열면 다음 "VM 만들기"도 빌더다(설계 §8.1).
  useEffect(() => writeMode(user, "builder"), [user]);
  const readsNodes = perms.reads("node");
  if (!ready || (readsNodes && !nodes.data) || !folders.data) {
    return <ListPending error={nodes.error ?? folders.error} />;
  }
  const folder = defaultFolder(perms, folderTree(folders.data, "vm"), params.get("folder"));
  return <Builder machine={readsNodes ? defaultMachine(nodes.data ?? []) : ""} folder={folder} />;
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
  const [hist, setHist] = useState<History>(() => {
    const d = readDraft();
    return { past: [], present: d ? fromValues(d.values) : initialState(machine, folder) };
  });
  useEffect(() => clearDraft(), []);
  const s = hist.present;
  const cpu = useCpuModels(archOf(s.values.machine), readsNodes);
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

  const browser = useMemo(() => browserProblems(s, ctx), [s, ctx]);
  const body = useMemo(() => checkBody(s, ctx), [s, ctx]);
  // 기능 `vm-check`가 없는 노드(위트니스 제외)가 있으면 묻지 않는다. 노드를 읽지 못하면 묻고 409를 받는다.
  const supported = !(ctx.nodes ?? []).some(
    (n) => !n.spec.witness && !(n.status.features ?? []).includes(VM_CHECK),
  );
  const server = useServerCheck(body, !blocking(browser), supported);
  const list = useMemo(
    () => problems(s, ctx, server.stale ? null : server.check),
    [s, ctx, server.stale, server.check],
  );

  const [selected, select] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  // 놓은 뒤 초점을 줄 새 블록(그린 뒤에 준다)
  const focusTo = useRef<string | null>(null);
  const [said, setSaid] = useState("");
  const abort = useRef<AbortController | null>(null);
  useEffect(() => () => abort.current?.abort(), []);
  // 이번 화면에서 만든 배치 규칙(다시 보낼 때 또 만들지 않음)
  const madeRules = useRef(new Set<string>());

  // 놓은 뒤 새 블록으로 초점(설계 §6.1 "끝난 뒤 초점")
  useEffect(() => {
    if (focusTo.current) {
      document.querySelector<HTMLElement>(`[data-block="${focusTo.current}"]`)?.focus();
      focusTo.current = null;
    }
  });

  const apply = (next: BuilderState) => setHist((h) => record(h, next));
  const builder: BuilderApi = {
    s,
    ctx,
    list,
    busy,
    selected,
    select,
    put(thing, slot) {
      const next = drop(s, thing, slot, ctx);
      if (next === s) {
        return undefined;
      }
      apply(next);
      const target = slotOf(slot);
      const list = target && listOf(target);
      if (thing.kind !== "block" && list && list.index === null) {
        // 목록에 더함: 새 블록으로 초점, 개수를 읽는다.
        const n = next.values[list.list].length;
        focusTo.current = `${list.list}.${n - 1}`;
        return t(`vmBuilder.page.count.${list.list}`, { count: n });
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
    if (hist.past.length > 0) {
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

  async function create() {
    setBusy(true);
    setFailure(null);
    const controller = new AbortController();
    abort.current = controller;
    const { signal } = controller;
    const made: string[] = [];
    try {
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
          made.push(name);
          setHist((h) => ({ ...h, present: markExisting(h.present, name) }));
        } else if (r.path === "/placement-rules") {
          if (!madeRules.current.has(r.body.name)) {
            await call(api.POST("/placement-rules", { body: r.body }));
            madeRules.current.add(r.body.name);
            made.push(r.body.name);
          }
        } else if (r.path === "/vms") {
          await call(api.POST("/vms", { body: r.body }));
          void navigate(`/vms/${encodeURIComponent(r.body.name)}`);
        } else {
          const task = await call(
            api.POST("/vms/{key}/clone", { params: { path: { key: r.key } }, body: r.body }),
          );
          const template = ctx.vms.find((v) => v.id === r.key)?.name ?? r.key;
          trackTask(task, t("templates.deployOp", { template, name: r.body.name }));
          void navigate("/vms");
        }
      }
    } catch (err) {
      if (signal.aborted) {
        return;
      }
      const message = errorMessage(t, err);
      setFailure(
        made.length > 0
          ? t("vmBuilder.page.created", { names: made.join(", "), error: message })
          : message,
      );
    } finally {
      if (!signal.aborted) {
        setBusy(false);
      }
    }
  }

  // 폼으로: 지금 값을 초안으로(템플릿 바탕은 폼의 "템플릿에서")
  const toForm = () =>
    s.base.kind === "template"
      ? putDraft({ values: initialState(machine, folder).values, preset: "template" })
      : putDraft({ values: s.values });

  const narrow = useNarrow();
  return (
    <BuilderContext.Provider value={builder}>
      <DndProvider<Thing>
        accepts={(item, slot) => accepts(item, slot, s, ctx)}
        onDrop={(item, slot) => builder.put(item, slot)}
      >
        <style>{STYLE}</style>
        <div className="vb flex flex-col gap-4">
          <div className="flex flex-wrap items-center gap-3">
            <h1 className="text-xl font-semibold">{t("vmForm.createTitle")}</h1>
            <ViewSwitch mode="builder" onLeave={toForm} />
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="ml-auto"
              disabled={hist.past.length === 0 || busy}
              onClick={back}
            >
              {t("vmBuilder.page.undo")}
            </Button>
          </div>
          <p role="status" className="sr-only">
            {said}
          </p>
          <div className="vb-grid">
            <PartsPanel narrow={narrow} />
            <Chassis />
            <SidePanel server={server} failure={failure} onCreate={() => void create()} />
          </div>
          <div>
            <Button variant="ghost" asChild>
              <Link to="/vms">{t("common.cancel")}</Link>
            </Button>
          </div>
        </div>
      </DndProvider>
    </BuilderContext.Provider>
  );
}

/** 1024px 밑(빌더가 한 칸이 되고 부품 상자가 서랍이 됨, 설계 §8.7) */
export function useNarrow(): boolean {
  const query = "(max-width: 1023px)";
  const [narrow, setNarrow] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const media = window.matchMedia(query);
    const change = () => setNarrow(media.matches);
    media.addEventListener("change", change);
    return () => media.removeEventListener("change", change);
  }, []);
  return narrow;
}
