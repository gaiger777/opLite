import { useTranslation } from "react-i18next";

import type { Topology, TopoNetwork, TopoVm } from "@/topology/topology";

/** 목록 항목을 누르면 그림과 같은 서랍·화면을 연다. */
export type OpenTarget =
  | { kind: "network" | "simple" | "provider"; id: string }
  | { kind: "router" | "lb" | "fip"; id: string }
  | { kind: "vm"; name: string };

const byName = <T extends { name: string }>(a: T, b: T) => a.name.localeCompare(b.name);

/**
 * "목록으로 보기"(계획 67 §12.2): 그림과 같은 정보를 트리 목록으로 — 공급자 네트워크(유동 IP), 라우터(인터페이스 네트워크 →
 * VM·부하 분산기), 라우터 없는 오버레이, 단순 네트워크. 화면 읽기 프로그램과 인쇄용이다.
 */
export function TopologyList({
  graph,
  hideSimple,
  hideVms,
  onOpen,
}: {
  graph: Topology;
  hideSimple: boolean;
  hideVms: boolean;
  onOpen(target: OpenTarget): void;
}) {
  const { t } = useTranslation();
  const nets = [...graph.networks].sort(byName);
  const netById = new Map(nets.map((n) => [n.id, n]));
  const routed = new Set(graph.routers.flatMap((r) => r.interfaces));
  const providers = nets.filter((n) => n.kind === "provider");
  const loose = nets.filter((n) => n.kind === "sdn" && !routed.has(n.id));
  const simple = hideSimple ? [] : nets.filter((n) => n.kind !== "sdn" && n.kind !== "provider");

  const vmsOn = (id: string): TopoVm[] =>
    hideVms ? [] : graph.vms.filter((vm) => vm.nics.some((n) => n.network === id)).sort(byName);

  const network = (n: TopoNetwork) => (
    <li key={n.id}>
      <button
        type="button"
        className="font-medium underline-offset-4 hover:underline"
        onClick={() =>
          onOpen({
            kind: n.kind === "sdn" ? "network" : n.kind === "provider" ? "provider" : "simple",
            id: n.id,
          })
        }
      >
        {n.name}
      </button>{" "}
      <span className="text-muted-foreground">
        {t(`topology.kind.${n.kind}`)}
        {n.cidrs.length > 0 && ` · ${n.cidrs.join(", ")}`}
      </span>
      {n.migration && (
        <span className="ml-2 text-warning-fg">
          {t("topology.migrating", { moved: n.migration.moved, total: n.migration.total })}
        </span>
      )}
      <ul className="ml-5 list-disc">
        {graph.load_balancers
          .filter((l) => l.network === n.id)
          .sort(byName)
          .map((l) => (
            <li key={l.id}>
              <button
                type="button"
                className="underline-offset-4 hover:underline"
                onClick={() => onOpen({ kind: "lb", id: l.id })}
              >
                {t("topology.lbItem", { name: l.name, vip: l.vip ?? "—" })}
              </button>{" "}
              <span className="text-muted-foreground">
                {t("topology.healthy", {
                  online: l.members.filter((m) => m.health === "online").length,
                  total: l.members.length,
                })}
              </span>
            </li>
          ))}
        {vmsOn(n.id).map((vm) => (
          <li key={vm.id}>
            <button
              type="button"
              className="underline-offset-4 hover:underline"
              onClick={() => onOpen({ kind: "vm", name: vm.name })}
            >
              {vm.name}
            </button>{" "}
            <span className="text-muted-foreground">
              {t(`vms.phase.${vm.power}`)}
              {vm.nics.find((x) => x.network === n.id)?.ips.length
                ? ` · ${vm.nics.find((x) => x.network === n.id)!.ips.join(", ")}`
                : ""}
              {vm.node && ` · ${vm.node}`}
            </span>
          </li>
        ))}
      </ul>
    </li>
  );

  return (
    <div className="flex flex-col gap-4 text-sm" aria-label={t("topology.listLabel")} role="region">
      {providers.length > 0 && (
        <section aria-labelledby="topo-providers">
          <h2 id="topo-providers" className="mb-1 font-medium">
            {t("topology.providers")}
          </h2>
          <ul className="ml-5 list-disc">
            {providers.map((p) => (
              <li key={p.id}>
                <button
                  type="button"
                  className="font-medium underline-offset-4 hover:underline"
                  onClick={() => onOpen({ kind: "provider", id: p.id })}
                >
                  {p.name}
                </button>{" "}
                <span className="text-muted-foreground">{p.cidrs.join(", ")}</span>
                <ul className="ml-5 list-disc">
                  {graph.floating_ips
                    .filter((f) => f.network === p.id)
                    .map((f) => (
                      <li key={f.id}>
                        <button
                          type="button"
                          className="underline-offset-4 hover:underline"
                          onClick={() => onOpen({ kind: "fip", id: f.id })}
                        >
                          {t("topology.fipItem", { address: f.address ?? f.id })}
                        </button>{" "}
                        <span className="text-muted-foreground">
                          {t(`fip.state.${f.state}`, { defaultValue: f.state })}
                          {f.vm && ` → ${graph.vms.find((v) => v.id === f.vm)?.name ?? ""}`}
                        </span>
                      </li>
                    ))}
                </ul>
              </li>
            ))}
          </ul>
        </section>
      )}
      {graph.routers.length > 0 && (
        <section aria-labelledby="topo-routers">
          <h2 id="topo-routers" className="mb-1 font-medium">
            {t("nav.routers")}
          </h2>
          <ul className="ml-5 list-disc">
            {[...graph.routers].sort(byName).map((r) => (
              <li key={r.id}>
                <button
                  type="button"
                  className="font-medium underline-offset-4 hover:underline"
                  onClick={() => onOpen({ kind: "router", id: r.id })}
                >
                  {r.name}
                </button>{" "}
                <span
                  className={
                    r.gateway.state === "no-candidate" ? "text-warning-fg" : "text-muted-foreground"
                  }
                >
                  {gatewayText(t, r.gateway)}
                  {r.external &&
                    ` · ${netById.get(r.external.network)?.name ?? ""} ${r.external.ips.join(", ")}`}
                </span>
                <ul className="ml-5 list-disc">
                  {r.interfaces
                    .map((id) => netById.get(id))
                    .filter((n): n is TopoNetwork => n !== undefined)
                    .sort(byName)
                    .map(network)}
                </ul>
              </li>
            ))}
          </ul>
        </section>
      )}
      {loose.length > 0 && (
        <section aria-labelledby="topo-loose">
          <h2 id="topo-loose" className="mb-1 font-medium">
            {t("topology.unrouted")}
          </h2>
          <ul className="ml-5 list-disc">{loose.map(network)}</ul>
        </section>
      )}
      {simple.length > 0 && (
        <section aria-labelledby="topo-simple">
          <h2 id="topo-simple" className="mb-1 font-medium">
            {t("topology.simple")}
          </h2>
          <ul className="ml-5 list-disc">{simple.map(network)}</ul>
        </section>
      )}
    </div>
  );
}

export function gatewayText(
  t: (key: string, values?: Record<string, unknown>) => string,
  gateway: Topology["routers"][number]["gateway"],
): string {
  if (gateway.state === "active") {
    return gateway.node
      ? t("topology.gatewayOn", { node: gateway.node })
      : t("topology.gatewayActive");
  }
  return t(gateway.state === "no-candidate" ? "topology.noGateway" : "topology.gatewayUnknown");
}
