import { useQueryClient } from "@tanstack/react-query";
import { FolderPlus, Plus } from "lucide-react";
import { Suspense, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";

import { useList } from "@/api/live";
import type { Folder, FolderKind, Network, StoragePool, Vm } from "@/api/types";
import { objectTarget, type Target } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { type Column, DataTable } from "@/components/data-table/DataTable";
import { useTableState } from "@/components/data-table/state";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { ViewToggle } from "@/layout/PageTabs";
import { ResourceHeader } from "@/components/ResourceHeader";
import { useConfigMenu } from "@/declarative/ConfigMenu";
import { Button } from "@/components/ui/button";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  CreateFolderDialog,
  DeleteFolderDialog,
  NetworkQuotaDialog,
  QuotaDialog,
  RenameFolderDialog,
} from "@/folders/FolderDialogs";
import {
  type DragPayload,
  FOLDER_PATHS,
  type Foldered,
  folderPath,
  folderTree,
  type FolderTree as Tree,
  MEMBER_KIND,
  MEMBER_KINDS,
  memberKindOf,
  membersOf,
  moveMembers,
  ROOT_ID,
} from "@/folders/folders";
import { endDrag, FolderTree, startDrag } from "@/folders/FolderTree";
import { announceMove, MoveButton, MoveDialog } from "@/folders/MoveDialog";
import { DIMENSIONS, formatDimension, UsageBars, useFolderUsage } from "@/folders/UsageBars";
import { refreshLbs, useLoadBalancers } from "@/lb/lb";
import { PermissionsPanel } from "@/permissions/lazyPanel";
import { refreshRouting, useFloatingIps, useRouters } from "@/routing/routing";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";

type Member = (Vm | Network | StoragePool) & Pick<Foldered, "memberKind">;

/** 이 트리의 객체 목록 */
function useMembers(kind: FolderKind) {
  const vms = useList("vm");
  const networks = useList("network");
  const pools = useList("storage-pool");
  return kind === "vm" ? vms : kind === "network" ? networks : pools;
}

/**
 * 네트워크 폴더에 함께 드는 라우터·유동 IP(계획 66 — 리뷰 1 웹 M4). watch가 아니어서 따로 읽는다(라우터는 NB를 읽지
 * 않는 목록). 행 모양은 다른 객체와 같고 `memberKind`로 가른다.
 */
function useNetworkExtras(enabled: boolean): Member[] {
  const routers = useRouters(enabled);
  const fips = useFloatingIps(enabled);
  // 부하 분산기(계획 67)도 네트워크 폴더에 든다.
  const lbs = useLoadBalancers(enabled, false);
  return useMemo(
    () =>
      [
        ...(routers.data ?? []).map((r) => ({ ...r, memberKind: "router" as const })),
        ...(fips.data ?? []).map((f) => ({ ...f, memberKind: "floating-ip" as const })),
        ...(lbs.data ?? []).map((l) => ({ ...l, memberKind: "load-balancer" as const })),
      ] as unknown as Member[],
    [routers.data, fips.data, lbs.data],
  );
}

function folderTarget(id: string): Target {
  return id === ROOT_ID ? { type: "root" } : { type: "folder", id };
}

/** 폴더 트리의 끌어 놓기: 놓을 수 없는 이유와 놓으면 옮기고 묶음 알림 */
function useDrop(kind: FolderKind, tree: Tree) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const label = (id: string) => (id === ROOT_ID ? t("folders.root") : folderPath(tree, id));
  return {
    canDrop(id: string, payload: DragPayload) {
      if (!MEMBER_KINDS[kind].includes(payload.kind)) {
        return "folders.reason.kind";
      }
      if (!perms.has("inventory.move", folderTarget(id))) {
        return "folders.reason.denied";
      }
      return payload.from.every((from) => from === id) ? "folders.reason.already" : null;
    },
    onDrop(id: string, payload: DragPayload) {
      void moveMembers(id === ROOT_ID ? null : id, payload.kind, payload.names).then(
        (results) => {
          // 라우터·유동 IP·부하 분산기는 보기 목록이라 다시 읽는다.
          if (payload.kind !== MEMBER_KIND[kind]) {
            void refreshRouting(queryClient);
            void refreshLbs(queryClient);
          }
          announceMove(t, label(id), results);
        },
        (error: unknown) =>
          announceMove(
            t,
            label(id),
            payload.names.map((key) => ({
              ok: false,
              key,
              error: { message: error instanceof Error ? error.message : String(error) },
            })),
          ),
      );
    },
  };
}

/**
 * 폴더에 든 객체 표(폴더 보기의 오른쪽, 폴더 상세의 내용 탭). 행을 끌어 트리에 놓거나, 골라서 "폴더로 옮기기…"를
 * 쓴다(키보드·휴대폰). VM은 이름이 상세 링크다.
 */
export function FolderMembers({
  kind,
  folder,
  deep,
  tree,
  labelledBy,
  toolbar,
}: {
  kind: FolderKind;
  folder: string;
  deep: boolean;
  tree: Tree;
  labelledBy?: string;
  toolbar?: React.ReactNode;
}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const list = useMembers(kind);
  const extras = useNetworkExtras(kind === "network");
  const state = useTableState(`folder-${kind}`);
  const [moving, setMoving] = useState<{ items: Foldered[]; clear(): void } | null>(null);
  const kindOf = (item: Member) => memberKindOf(item, kind);
  const rows = useMemo(
    () => membersOf(tree, [...((list.data ?? []) as Member[]), ...extras], folder, deep),
    [tree, list.data, extras, folder, deep],
  );
  const mover = perms.anywhere("inventory.move");
  const where = (item: Member) => item.spec.folder ?? ROOT_ID;

  const columns: Column<Member>[] = [
    {
      id: "name",
      header: t("common.name"),
      value: (item) => item.name,
      cell: (item) =>
        kind === "vm" ? (
          <Link
            to={`/vms/${encodeURIComponent(item.name)}`}
            className="font-medium underline-offset-4 hover:underline"
          >
            {item.name}
          </Link>
        ) : (
          <span className="font-medium">{item.name}</span>
        ),
    },
    ...(kind === "network"
      ? [
          {
            id: "type",
            header: t("folders.type"),
            value: (item: Member) => t(`folders.memberKind.${kindOf(item)}`),
            cell: (item: Member) => t(`folders.memberKind.${kindOf(item)}`),
          },
        ]
      : []),
    ...(kind === "vm"
      ? [
          {
            id: "phase",
            header: t("common.state"),
            value: (item: Member) => (item as Vm).status.phase,
            cell: (item: Member) => <VmPhaseBadge vm={item as Vm} />,
          },
        ]
      : []),
    {
      id: "folder",
      header: t("folders.folder"),
      value: (item) => folderPath(tree, item.spec.folder),
      cell: (item) => folderPath(tree, item.spec.folder),
      priority: 2,
    },
  ];

  return (
    <>
      <DataTable
        id={`folder-${kind}`}
        rows={rows}
        columns={columns}
        rowId={(item) => item.id}
        rowName={(item) => item.name}
        query={list}
        empty={t("folders.emptyFolder")}
        labelledBy={labelledBy}
        state={state}
        search={(item) => item.name}
        toolbar={toolbar}
        bulk={
          mover
            ? (chosen, clear) => (
                <MoveButton
                  items={chosen}
                  allowed={(item) => perms.has("inventory.move", objectTarget(kindOf(item), item))}
                  onClick={() => setMoving({ items: chosen, clear })}
                />
              )
            : undefined
        }
        rowProps={(item, selected) => {
          const memberKind = kindOf(item);
          if (!perms.has("inventory.move", objectTarget(memberKind, item))) {
            return {};
          }
          // 고른 행을 끌면 고른 것 중 같은 종류 모두(옮기기 끌기는 한 종류), 아니면 그 행 하나
          const dragged = selected.some((s) => s.id === item.id)
            ? selected.filter((s) => kindOf(s) === memberKind)
            : [item];
          return {
            draggable: true,
            "aria-roledescription": t("folders.draggable"),
            onDragStart: (e) =>
              startDrag(e, {
                kind: memberKind,
                ids: dragged.map((d) => d.id),
                names: dragged.map((d) => d.name),
                from: dragged.map(where),
              }),
            onDragEnd: endDrag,
          };
        }}
      />
      {moving && (
        <MoveDialog
          kind={kind}
          items={moving.items}
          onClose={() => setMoving(null)}
          onDone={moving.clear}
        />
      )}
    </>
  );
}

/** 목록 | 폴더 보기의 "폴더" 쪽: 왼쪽 트리, 오른쪽 그 폴더의 객체(설계 §13 웹) */
export function FolderViewPage({ kind }: { kind: FolderKind }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const folders = useList("folder");
  const tree = useMemo(() => folderTree(folders.data, kind), [folders.data, kind]);
  const [params, setParams] = useSearchParams();
  const asked = params.get("folder") ?? ROOT_ID;
  // 지운 폴더를 가리키는 주소는 루트를 보인다.
  const selected = asked === ROOT_ID || tree.byId.has(asked) ? asked : ROOT_ID;
  const deep = params.get("deep") === "1";
  const [creating, setCreating] = useState(false);
  const drop = useDrop(kind, tree);
  const set = (key: string, value: string | null) =>
    setParams(
      (p) => {
        if (value === null) {
          p.delete(key);
        } else {
          p.set(key, value);
        }
        return p;
      },
      { replace: true },
    );
  const path = selected === ROOT_ID ? "/" : folderPath(tree, selected);
  const rootLabel = t(`folders.rootOf.${kind}`);
  const canCreateFolder = perms.has("inventory.folder", folderTarget(selected));
  const canCreateVm =
    kind === "vm" &&
    perms.has("vm.create", {
      type: "object",
      kind: "vm",
      folder: selected === ROOT_ID ? null : selected,
    });

  if (!folders.data) {
    return <ListPending error={folders.error} />;
  }
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="folder-view-title"
        title={t(`nav.${kind}Folders`)}
        actions={
          <>
            <ViewToggle />
            {canCreateFolder && (
              <Button variant="outline" onClick={() => setCreating(true)}>
                <FolderPlus />
                {t("folders.new")}
              </Button>
            )}
            {canCreateVm && (
              <Button asChild>
                <Link to={selected === ROOT_ID ? "/vms/-new" : `/vms/-new?folder=${selected}`}>
                  <Plus />
                  {t("folders.createVmHere")}
                </Link>
              </Button>
            )}
          </>
        }
      />
      <div className="grid gap-4 md:grid-cols-[16rem_minmax(0,1fr)]">
        <div className="rounded-lg border p-2 md:max-h-[70vh] md:overflow-y-auto">
          <FolderTree
            tree={tree}
            rootLabel={rootLabel}
            label={t(`nav.${kind}Folders`)}
            selected={selected}
            onSelect={(id) => set("folder", id === ROOT_ID ? null : id)}
            canDrop={drop.canDrop}
            onDrop={drop.onDrop}
          />
        </div>
        <div className="flex min-w-0 flex-col gap-2">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h2 id="folder-view-path" className="font-medium break-all">
              {path}
            </h2>
            {selected !== ROOT_ID && (
              <Button variant="link" size="sm" asChild>
                <Link to={`${FOLDER_PATHS[kind]}/${selected}`}>{t("folders.details")}</Link>
              </Button>
            )}
          </div>
          <FolderMembers
            kind={kind}
            folder={selected}
            deep={deep}
            tree={tree}
            labelledBy="folder-view-path"
            toolbar={
              <Button
                variant={deep ? "secondary" : "outline"}
                size="sm"
                aria-pressed={deep}
                onClick={() => set("deep", deep ? null : "1")}
              >
                {t("folders.includeSubfolders")}
              </Button>
            }
          />
        </div>
      </div>
      {creating && (
        <CreateFolderDialog
          kind={kind}
          parent={selected === ROOT_ID ? null : selected}
          parentPath={path}
          onClose={() => setCreating(false)}
          onCreated={(f) => set("folder", f.id)}
        />
      )}
    </div>
  );
}

export function VmFoldersPage() {
  return <FolderViewPage kind="vm" />;
}
export function NetworkFoldersPage() {
  return <FolderViewPage kind="network" />;
}
export function StorageFoldersPage() {
  return <FolderViewPage kind="storage" />;
}

const DETAIL_TABS = ["contents", "quota", "permissions"] as const;

/** 폴더 상세 `/{종류}-folders/:id`: 머리(경로·동작), 요약(객체 수·사용량), 탭 내용 | 할당량 | 권한 */
export function FolderDetailPage({ kind }: { kind: FolderKind }) {
  const { t } = useTranslation();
  const { id = "" } = useParams();
  const navigate = useNavigate();
  const perms = usePermissions();
  const folders = useList("folder");
  const list = useMembers(kind);
  const tree = useMemo(() => folderTree(folders.data, kind), [folders.data, kind]);
  const folder = tree.byId.get(id);
  const [params, setParams] = useSearchParams();
  const [dialog, setDialog] = useState<"create" | "rename" | "quota" | null>(null);
  const [deleting, setDeleting] = useState(false);
  const kebab = useRef<HTMLButtonElement>(null);
  // 설정 기록·YAML 내보내기(계획 73): 폴더 id가 이력·매니페스트 열쇠다.
  const config = useConfigMenu(folder && { kind: "folder", id: folder.id, name: folder.spec.name });
  // 할당량은 VM 폴더와(계획 62) 네트워크 폴더(라우터·유동 IP, 계획 66)
  const quotas = kind === "vm" || kind === "network";
  const usage = useFolderUsage(id, quotas && folder !== undefined);
  // 읽을 수 없는 조상의 할당량은 서버가 빼고 보낸다(리뷰 1). 그런 조상이 있으면 안내한다.
  const hiddenAbove = useMemo(() => {
    let at = folder?.spec.parent;
    for (let depth = 0; at && depth < 16; depth++) {
      if (!perms.has("system.read", { type: "folder", id: at })) {
        return true;
      }
      at = tree.byId.get(at)?.spec.parent;
    }
    return false;
  }, [folder, perms, tree]);
  const tabs = DETAIL_TABS.filter((tab) => tab !== "quota" || quotas);
  const tab = tabs.find((x) => x === params.get("tab")) ?? "contents";

  if (!folders.data) {
    return <ListPending error={folders.error} />;
  }
  if (!folder) {
    return (
      <div className="flex flex-col items-start gap-2">
        <h1 className="text-xl font-semibold">{t("folders.notFound")}</h1>
        <Link to={FOLDER_PATHS[kind]} className="text-sm underline">
          {t("folders.backToTree")}
        </Link>
      </div>
    );
  }
  const path = folderPath(tree, folder.id);
  const parent: Target = folder.spec.parent ? folderTarget(folder.spec.parent) : { type: "root" };
  const here = folderTarget(folder.id);
  const canFolder = perms.has("inventory.folder", here);
  const canQuota = quotas && perms.has("inventory.quota", parent);
  const members = membersOf(tree, (list.data ?? []) as Member[], folder.id, true);
  const children = tree.children.get(folder.id) ?? [];
  const menu = (
    <>
      {canFolder && (
        <DropdownMenuItem onSelect={() => setDialog("create")}>{t("folders.new")}</DropdownMenuItem>
      )}
      {canFolder && (
        <DropdownMenuItem onSelect={() => setDialog("rename")}>
          {t("folders.rename")}
        </DropdownMenuItem>
      )}
      {canQuota && (
        <DropdownMenuItem onSelect={() => setDialog("quota")}>
          {t("folders.setQuota")}
        </DropdownMenuItem>
      )}
      {config.items}
      {canFolder && (
        <>
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onSelect={() => setDeleting(true)}>
            {t("common.delete")}
          </DropdownMenuItem>
        </>
      )}
    </>
  );

  return (
    <div className="flex flex-col gap-4">
      <ResourceHeader
        name={folder.spec.name}
        meta={path}
        menu={menu}
        menuRef={kebab}
        summary={[
          { label: t(`folders.count.${kind}`), value: members.length },
          { label: t("folders.subfolders"), value: children.length },
        ]}
      >
        {config.host}
      </ResourceHeader>
      {quotas && usage.data && <UsageBars view={usage.data} kind={kind} />}
      <Tabs
        value={tab}
        onValueChange={(value) =>
          setParams(
            (p) => {
              p.set("tab", value);
              return p;
            },
            { replace: true },
          )
        }
      >
        <TabsList>
          {tabs.map((x) => (
            <TabsTrigger key={x} value={x}>
              {t(`folders.tab.${x}`)}
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value="contents">
          <FolderMembers kind={kind} folder={folder.id} deep tree={tree} />
        </TabsContent>
        <TabsContent value="quota">
          {tab === "quota" && (
            <QuotaTab
              folder={folder}
              view={usage.data}
              hiddenAbove={hiddenAbove}
              canEdit={canQuota}
              onEdit={() => setDialog("quota")}
            />
          )}
        </TabsContent>
        <TabsContent value="permissions">
          {tab === "permissions" && (
            <Suspense fallback={null}>
              <PermissionsPanel target={here} label={path} />
            </Suspense>
          )}
        </TabsContent>
      </Tabs>
      {dialog === "create" && (
        <CreateFolderDialog
          kind={kind}
          parent={folder.id}
          parentPath={path}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog === "rename" && (
        <RenameFolderDialog folder={folder} onClose={() => setDialog(null)} />
      )}
      {dialog === "quota" &&
        (kind === "network" ? (
          <NetworkQuotaDialog folder={folder} onClose={() => setDialog(null)} />
        ) : (
          <QuotaDialog folder={folder} onClose={() => setDialog(null)} />
        ))}
      <DeleteFolderDialog
        folder={folder}
        path={path}
        open={deleting}
        onOpenChange={setDeleting}
        onDeleted={() => void navigate(FOLDER_PATHS[kind])}
      />
    </div>
  );
}

function QuotaTab({
  folder,
  view,
  hiddenAbove,
  canEdit,
  onEdit,
}: {
  folder: Folder;
  view: ReturnType<typeof useFolderUsage>["data"];
  /** 읽을 수 없는 조상이 있다(그 할당량은 보이지 않는다) */
  hiddenAbove: boolean;
  canEdit: boolean;
  onEdit(): void;
}) {
  const { t } = useTranslation();
  const quota = folder.spec.quota;
  return (
    <section className="flex flex-col gap-4 pt-2">
      <p className="text-sm text-muted-foreground">{t("folders.quotaHint")}</p>
      {!quota && <p className="text-sm">{t("folders.noQuota")}</p>}
      {hiddenAbove && <p className="text-sm text-muted-foreground">{t("folders.hiddenLimits")}</p>}
      {view && view.limits.length > 0 && (
        <ul className="flex flex-col gap-1 text-sm">
          {view.limits.map((l) => (
            <li key={l.folder}>
              <span className="font-medium">{l.path}</span>:{" "}
              {DIMENSIONS.filter((d) => l.quota[d] !== undefined && l.quota[d] !== null)
                .map((d) => `${t(`folders.dim.${d}`)} ${formatDimension(d, l.quota[d]!)}`)
                .join(" · ")}
            </li>
          ))}
        </ul>
      )}
      {canEdit && (
        <div>
          <Button variant="outline" onClick={onEdit}>
            {t("folders.setQuota")}
          </Button>
        </div>
      )}
    </section>
  );
}

export function VmFolderDetailPage() {
  return <FolderDetailPage kind="vm" />;
}
export function NetworkFolderDetailPage() {
  return <FolderDetailPage kind="network" />;
}
export function StorageFolderDetailPage() {
  return <FolderDetailPage kind="storage" />;
}
