import { Plus, Trash2 } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, ApiError, call } from "@/api/client";
import { useList } from "@/api/live";
import type { StoragePool } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { codeMessage } from "@/i18n/errors";
import { formatBytes } from "@/lib/format";
import {
  cephNameError,
  cephUserError,
  iscsiPortalError,
  iscsiTargetError,
  isShared,
  lvmNameError,
  nfsExportError,
  nfsOptionsError,
  nfsServerError,
  poolCapacity,
  poolLocation,
  poolNodes,
  rbdConfError,
  zfsDatasetError,
} from "@/storage/pools";
import { PoolStatus } from "@/storage/PoolStatus";
import { validName } from "@/vms/form";

export function PoolsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const nodes = useList("node");
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<StoragePool | null>(null);
  // 공유 여부를 바꿀 디렉터리 풀과 바꿀 값. 닫히는 동안에도 남겨 두어 대화상자의 글이 바뀌지 않게 한다.
  const [sharing, setSharing] = useState<{ pool: StoragePool; shared: boolean } | null>(null);
  const [sharingOpen, setSharingOpen] = useState(false);
  const write = can(me.role, "write", "storage-pool");

  const nodeList = useMemo(() => nodes.data ?? [], [nodes.data]);
  // 볼륨 목록을 다 읽기 전에는 모른다(undefined).
  const volumeCount = (pool: StoragePool) =>
    volumes.isSuccess ? volumes.data.filter((v) => v.spec.pool === pool.name).length : undefined;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="pools-title" className="text-2xl font-semibold">
          {t("nav.pools")}
        </h1>
        {write && (
          <Button onClick={() => setCreating(true)}>
            <Plus />
            {t("common.create")}
          </Button>
        )}
      </div>
      <Table aria-labelledby="pools-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("pools.kind")}</TableHead>
            <TableHead>{t("pools.location")}</TableHead>
            <TableHead>{t("pools.sharing")}</TableHead>
            <TableHead>{t("pools.nodes")}</TableHead>
            <TableHead>{t("pools.free")}</TableHead>
            <TableHead>{t("pools.volumes")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(pools.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 8 : 7} query={pools} empty={t("pools.empty")} />
          )}
          {(pools.data ?? []).map((pool) => {
            const capacity = poolCapacity(pool, nodeList);
            const used = volumeCount(pool);
            return (
              <TableRow key={pool.id}>
                <TableCell className="font-medium">{pool.name}</TableCell>
                <TableCell>{t(`pools.kinds.${pool.spec.backend.kind}`)}</TableCell>
                <TableCell className="font-mono text-xs">{poolLocation(pool)}</TableCell>
                <TableCell>
                  {/* 디렉터리 풀만 바꿀 수 있다. 다른 풀은 종류가 정한다(NFS·RBD·공유 LVM은 늘 공유). */}
                  {write && pool.spec.backend.kind === "dir" ? (
                    <Switch
                      aria-label={t("pools.sharedNamed", { name: pool.name })}
                      checked={isShared(pool)}
                      onCheckedChange={(shared) => {
                        setSharing({ pool, shared });
                        setSharingOpen(true);
                      }}
                    />
                  ) : isShared(pool) ? (
                    t("pools.sharedState")
                  ) : (
                    t("pools.localState")
                  )}
                </TableCell>
                <TableCell>
                  {/* 노드를 정하지 않은 풀은 모든 노드에서 쓴다. */}
                  {(pool.spec.nodes ?? []).length === 0 && (
                    <div className="text-xs text-muted-foreground">{t("pools.allNodes")}</div>
                  )}
                  {poolNodes(pool, nodeList).map((n) => (
                    <PoolStatus key={n.id} name={n.name} state={n.state} offline={n.offline} />
                  ))}
                </TableCell>
                <TableCell>
                  {/* 공유 풀은 값 하나, 로컬 풀은 준비된 노드마다(노드가 둘 이상이면 이름을 붙인다). */}
                  {capacity.length === 0
                    ? "—"
                    : capacity.map((c) => (
                        <div key={c.node ?? ""}>
                          {capacity.length > 1 && `${c.node}: `}
                          {`${formatBytes(c.free)} / ${formatBytes(c.total)}`}
                        </div>
                      ))}
                </TableCell>
                <TableCell>
                  {used ?? "—"}
                  {write && used !== undefined && used > 0 && (
                    <div className="text-xs text-muted-foreground">{t("pools.inUse")}</div>
                  )}
                </TableCell>
                {write && (
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: pool.name })}
                      // 볼륨이 남은 풀은 서버가 지우지 않는다.
                      disabled={used !== 0}
                      onClick={() => setDeleting(pool)}
                    >
                      <Trash2 />
                    </Button>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && <CreatePoolDialog onClose={() => setCreating(false)} />}
      <ConfirmDialog
        open={sharingOpen}
        onOpenChange={setSharingOpen}
        title={t(sharing?.shared ? "pools.makeSharedTitle" : "pools.makeLocalTitle", {
          name: sharing?.pool.name,
        })}
        description={
          sharing?.shared
            ? t("pools.makeSharedDescription", { path: sharing ? poolLocation(sharing.pool) : "" })
            : t("pools.makeLocalDescription")
        }
        confirmLabel={t(sharing?.shared ? "pools.makeShared" : "pools.makeLocal")}
        onConfirm={async () => {
          const { pool, shared } = sharing!;
          try {
            await call(
              api.PATCH("/storage-pools/{key}", {
                params: { path: { key: pool.id } },
                body: { shared },
              }),
            );
          } catch (err) {
            // pool_in_use는 지우기와 뜻이 달라 이 동작의 문구로 보인다.
            throw err instanceof ApiError
              ? new Error(codeMessage(t, err.code, err.message, "pools.sharingErrors"))
              : err;
          }
          toast.success(t(shared ? "pools.madeShared" : "pools.madeLocal", { name: pool.name }));
        }}
      />
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("pools.deleteTitle", { name: deleting?.name })}
        description={t("pools.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const pool = deleting!;
          await call(api.DELETE("/storage-pools/{key}", { params: { path: { key: pool.id } } }));
          toast.success(t("pools.deleted", { name: pool.name }));
        }}
      />
    </div>
  );
}

type Kind = StoragePool["spec"]["backend"]["kind"];

function CreatePoolDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const nodes = useList("node");
  const [name, setName] = useState("");
  const [kind, setKind] = useState<Kind>("dir");
  const [path, setPath] = useState("");
  const [shared, setShared] = useState(false);
  const [nfsServer, setNfsServer] = useState("");
  const [nfsExport, setNfsExport] = useState("");
  const [nfsOptions, setNfsOptions] = useState("");
  const [vg, setVg] = useState("");
  const [thinpool, setThinpool] = useState("");
  const [dataset, setDataset] = useState("");
  const [cephPool, setCephPool] = useState("");
  const [namespace, setNamespace] = useState("");
  const [cephUser, setCephUser] = useState("");
  const [cephConf, setCephConf] = useState("");
  const [iscsi, setIscsi] = useState(false);
  const [portal, setPortal] = useState("");
  const [target, setTarget] = useState("");
  const [chosen, setChosen] = useState<string[]>([]);
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    if (!validName(name)) {
      found.name = "forms.name";
    }
    if (kind === "dir" && !path.startsWith("/")) {
      found.path = "pools.errors.path";
    }
    if (kind === "nfs") {
      const server = nfsServerError(nfsServer);
      if (server) {
        found.server = server;
      }
      const exportError = nfsExportError(nfsExport);
      if (exportError) {
        found.export = exportError;
      }
      const optionsError = nfsOptions === "" ? undefined : nfsOptionsError(nfsOptions);
      if (optionsError) {
        found.options = optionsError;
      }
    }
    if (kind === "lvm-thin") {
      const vgError = lvmNameError(vg);
      if (vgError) {
        found.vg = vgError;
      }
      const thinpoolError = lvmNameError(thinpool);
      if (thinpoolError) {
        found.thinpool = thinpoolError;
      }
    }
    if (kind === "lvm-shared") {
      const vgError = lvmNameError(vg);
      if (vgError) {
        found.vg = vgError;
      }
      // iSCSI를 고르지 않았으면 포털·타깃은 보지 않고 보내지도 않는다.
      const portalError = iscsi ? iscsiPortalError(portal) : undefined;
      if (portalError) {
        found.portal = portalError;
      }
      const targetError = iscsi ? iscsiTargetError(target) : undefined;
      if (targetError) {
        found.target = targetError;
      }
    }
    if (kind === "zfs") {
      const datasetError = zfsDatasetError(dataset);
      if (datasetError) {
        found.dataset = datasetError;
      }
    }
    if (kind === "rbd") {
      const poolError = cephNameError(cephPool);
      if (poolError) {
        found.cephPool = poolError;
      }
      // 네임스페이스·사용자·conf는 비우면 보내지 않는다(Ceph 기본값).
      const namespaceError = namespace === "" ? undefined : cephNameError(namespace);
      if (namespaceError) {
        found.namespace = namespaceError;
      }
      const userError = cephUser === "" ? undefined : cephUserError(cephUser);
      if (userError) {
        found.cephUser = userError;
      }
      const confError = cephConf === "" ? undefined : rbdConfError(cephConf);
      if (confError) {
        found.cephConf = confError;
      }
    }
    // 고른 뒤에 목록에서 빠진 노드는 보내지 않는다.
    const known = chosen.filter((id) => nodes.data?.some((n) => n.id === id));
    // 고른 노드가 모두 빠졌으면 빈 목록(모든 노드)으로 보내지 않는다.
    // 빠진 노드는 고른 목록에서도 지워, 다시 보내면 모든 노드로 만들 수 있게 한다.
    if (chosen.length > 0 && known.length === 0) {
      found.nodes = "pools.errors.nodesGone";
      setChosen(known);
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    // NFS·RBD·공유 LVM 풀의 `shared`는 보내지 않는다(서버가 늘 공유로 둔다). 빈 옵션은 커널 기본값이다.
    // LVM-thin·ZFS 풀은 노드 로컬이라 `shared`를 보내지 않는다.
    const sharedDir = kind === "dir" && shared;
    const backend =
      kind === "dir"
        ? { kind, path }
        : kind === "lvm-thin"
          ? { kind, vg, thinpool }
          : kind === "zfs"
            ? { kind, dataset }
            : kind === "lvm-shared"
              ? { kind, vg, ...(iscsi && { iscsi: { portal, target } }) }
              : kind === "rbd"
                ? {
                    kind,
                    pool: cephPool,
                    ...(namespace !== "" && { namespace }),
                    ...(cephUser !== "" && { user: cephUser }),
                    ...(cephConf !== "" && { conf: cephConf }),
                  }
                : {
                    kind,
                    server: nfsServer,
                    export: nfsExport,
                    ...(nfsOptions !== "" && { options: nfsOptions }),
                  };
    const spec = { backend, nodes: known, ...(sharedDir && { shared: true }) };
    await call(api.POST("/storage-pools", { body: { name, spec } }));
    toast.success(t("pools.created", { name }));
  }

  return (
    <FormDialog
      title={t("pools.createTitle")}
      description={t("pools.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="pool-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input id="pool-name" value={name} onChange={(e) => setName(e.target.value)} {...a11y} />
        )}
      </Field>
      <Field id="pool-kind" label={t("pools.kind")} hint={t(`pools.kindHint.${kind}`)}>
        {(a11y) => (
          <NativeSelect
            id="pool-kind"
            className="w-full"
            value={kind}
            onChange={(e) => setKind(e.target.value as Kind)}
            {...a11y}
          >
            <NativeSelectOption value="dir">{t("pools.kinds.dir")}</NativeSelectOption>
            <NativeSelectOption value="nfs">{t("pools.kinds.nfs")}</NativeSelectOption>
            <NativeSelectOption value="lvm-thin">{t("pools.kinds.lvm-thin")}</NativeSelectOption>
            <NativeSelectOption value="zfs">{t("pools.kinds.zfs")}</NativeSelectOption>
            <NativeSelectOption value="rbd">{t("pools.kinds.rbd")}</NativeSelectOption>
            <NativeSelectOption value="lvm-shared">
              {t("pools.kinds.lvm-shared")}
            </NativeSelectOption>
          </NativeSelect>
        )}
      </Field>
      {kind === "lvm-thin" && (
        <>
          <Field id="pool-vg" label={t("pools.vg")} error={errors.vg}>
            {(a11y) => (
              <Input
                id="pool-vg"
                placeholder="vg0"
                value={vg}
                onChange={(e) => setVg(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field id="pool-thinpool" label={t("pools.thinpool")} error={errors.thinpool}>
            {(a11y) => (
              <Input
                id="pool-thinpool"
                placeholder="thinpool"
                value={thinpool}
                onChange={(e) => setThinpool(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </>
      )}
      {kind === "lvm-shared" && (
        <>
          <Field id="pool-vg" label={t("pools.vg")} error={errors.vg}>
            {(a11y) => (
              <Input
                id="pool-vg"
                placeholder="san"
                value={vg}
                onChange={(e) => setVg(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          {/* FC·멀티패스처럼 노드가 이미 LUN을 보면 로그인하지 않는다. */}
          <div className="flex flex-col gap-1.5">
            <div className="flex items-center gap-2">
              <Checkbox
                id="pool-iscsi"
                checked={iscsi}
                onCheckedChange={(v) => {
                  setIscsi(v === true);
                  // 바꾸면 포털·타깃의 앞 오류는 맞지 않으므로 지운다.
                  setErrors((prev) => {
                    const next = { ...prev };
                    delete next.portal;
                    delete next.target;
                    return next;
                  });
                }}
                aria-describedby="pool-iscsi-hint"
              />
              <Label htmlFor="pool-iscsi">{t("pools.iscsi")}</Label>
            </div>
            <p id="pool-iscsi-hint" className="text-xs text-muted-foreground">
              {t("pools.iscsiHint")}
            </p>
          </div>
          {iscsi && (
            <>
              <Field
                id="pool-portal"
                label={t("pools.portal")}
                error={errors.portal}
                hint={t("pools.portalHint")}
              >
                {(a11y) => (
                  <Input
                    id="pool-portal"
                    placeholder="san.lan:3260"
                    value={portal}
                    onChange={(e) => setPortal(e.target.value)}
                    {...a11y}
                  />
                )}
              </Field>
              <Field
                id="pool-target"
                label={t("pools.target")}
                error={errors.target}
                hint={t("pools.targetHint")}
              >
                {(a11y) => (
                  <Input
                    id="pool-target"
                    placeholder="iqn.2003-01.org.example:storage"
                    value={target}
                    onChange={(e) => setTarget(e.target.value)}
                    {...a11y}
                  />
                )}
              </Field>
            </>
          )}
        </>
      )}
      {kind === "zfs" && (
        <Field id="pool-dataset" label={t("pools.dataset")} error={errors.dataset}>
          {(a11y) => (
            <Input
              id="pool-dataset"
              placeholder="tank/opl"
              value={dataset}
              onChange={(e) => setDataset(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
      )}
      {kind === "rbd" && (
        <>
          <Field id="pool-ceph-pool" label={t("pools.cephPool")} error={errors.cephPool}>
            {(a11y) => (
              <Input
                id="pool-ceph-pool"
                placeholder="rbd"
                value={cephPool}
                onChange={(e) => setCephPool(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field id="pool-namespace" label={t("pools.namespace")} error={errors.namespace}>
            {(a11y) => (
              <Input
                id="pool-namespace"
                value={namespace}
                onChange={(e) => setNamespace(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="pool-ceph-user"
            label={t("pools.cephUser")}
            error={errors.cephUser}
            hint={t("pools.cephUserHint")}
          >
            {(a11y) => (
              <Input
                id="pool-ceph-user"
                value={cephUser}
                onChange={(e) => setCephUser(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="pool-ceph-conf"
            label={t("pools.cephConf")}
            error={errors.cephConf}
            hint={t("pools.cephConfHint")}
          >
            {(a11y) => (
              <Input
                id="pool-ceph-conf"
                value={cephConf}
                onChange={(e) => setCephConf(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </>
      )}
      {kind === "dir" && (
        <>
          <Field
            id="pool-path"
            label={t("pools.path")}
            error={errors.path}
            hint={t("pools.pathHint")}
          >
            {(a11y) => (
              <Input
                id="pool-path"
                placeholder="/var/lib/opl/pool"
                value={path}
                onChange={(e) => setPath(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          {/* 밖에서 마운트한 공유 디렉터리. 공유가 아닌 풀의 VM은 볼륨이 있는 노드에서만 돈다. */}
          <div className="flex flex-col gap-1.5">
            <div className="flex items-center gap-2">
              <Checkbox
                id="pool-shared"
                checked={shared}
                onCheckedChange={(v) => setShared(v === true)}
                aria-describedby="pool-shared-hint"
              />
              <Label htmlFor="pool-shared">{t("pools.shared")}</Label>
            </div>
            <p id="pool-shared-hint" className="text-xs text-muted-foreground">
              {t("pools.sharedHint")}
            </p>
          </div>
        </>
      )}
      {kind === "nfs" && (
        <>
          <Field
            id="pool-server"
            label={t("pools.server")}
            error={errors.server}
            hint={t("pools.serverHint")}
          >
            {(a11y) => (
              <Input
                id="pool-server"
                placeholder="nas.lan"
                value={nfsServer}
                onChange={(e) => setNfsServer(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="pool-export"
            label={t("pools.export")}
            error={errors.export}
            hint={t("pools.exportHint")}
          >
            {(a11y) => (
              <Input
                id="pool-export"
                placeholder="/srv/vm"
                value={nfsExport}
                onChange={(e) => setNfsExport(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="pool-options"
            label={t("pools.options")}
            error={errors.options}
            hint={t("pools.optionsHint")}
          >
            {(a11y) => (
              <Input
                id="pool-options"
                value={nfsOptions}
                onChange={(e) => setNfsOptions(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </>
      )}
      <fieldset
        className="flex flex-col gap-2"
        aria-describedby={errors.nodes ? "pool-nodes-error" : undefined}
      >
        <legend className="mb-2 text-sm font-medium">{t("pools.nodes")}</legend>
        {!nodes.isSuccess && <ListPending error={nodes.error} />}
        {(nodes.data ?? []).map((node) => (
          <div key={node.id} className="flex items-center gap-2">
            <Checkbox
              id={`pool-node-${node.id}`}
              checked={chosen.includes(node.id)}
              onCheckedChange={(v) =>
                setChosen((c) => (v === true ? [...c, node.id] : c.filter((id) => id !== node.id)))
              }
            />
            <Label htmlFor={`pool-node-${node.id}`}>{node.name}</Label>
          </div>
        ))}
        <p className="text-xs text-muted-foreground">{t("pools.nodesHint")}</p>
        {errors.nodes && (
          <p id="pool-nodes-error" className="text-xs text-destructive">
            {t(errors.nodes)}
          </p>
        )}
      </fieldset>
    </FormDialog>
  );
}
