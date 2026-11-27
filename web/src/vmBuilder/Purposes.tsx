// VM 만들기 첫 단계 "무엇을 돌릴 VM인가요?"(계획 77-5, 진단 §4.3 1단계): 용도 카드 — 그림·이름·한 줄 설명·기본 크기. 누르면
// 그 용도로 채운 VM 그림 화면이 열린다. 템플릿 카드는 안에서 템플릿을 고르고, 이미지 라이브러리는 받는 화면으로 간다.
import {
  Copy,
  Database,
  Disc,
  Globe,
  Library,
  type LucideIcon,
  Monitor,
  Server,
  Square,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { formatMib } from "@/lib/format";
import { cn } from "@/lib/utils";
import { useBuilder } from "@/vmBuilder/context";
import { parts } from "@/vmBuilder/model/parts";
import {
  noDiskReason,
  type Purpose,
  PURPOSE_SIZE,
  PURPOSES,
  purposeBlocked,
  templates,
} from "@/vmBuilder/model/purposes";
import type { BuilderState } from "@/vmBuilder/model/state";

const ICON: Record<Purpose, LucideIcon> = {
  linux: Server,
  web: Globe,
  database: Database,
  windows: Monitor,
  iso: Disc,
  template: Copy,
  image: Library,
  blank: Square,
};

export function Purposes({
  state,
  loading,
  last,
  onChoose,
}: {
  /** 처음 틀(막힌 카드를 셈) */
  state: BuilderState;
  /** 풀·볼륨·네트워크·VM을 다 읽기 전(카드를 누르면 기본 부품이 빠지므로 누를 수 없음) */
  loading: boolean;
  /** 지난번에 고른 카드("바꾸기"로 돌아오면 그 카드에 초점) */
  last: Purpose | null;
  onChoose(p: Purpose, template?: string): void;
}) {
  const { t } = useTranslation();
  const b = useBuilder();
  const [picking, setPicking] = useState(false);
  const list = useRef<HTMLUListElement>(null);
  // "바꾸기"로 돌아오면 지난 카드(없으면 첫 카드)에 초점 — 읽은 뒤에(누를 수 있을 때)
  const focused = useRef(false);
  useEffect(() => {
    if (focused.current || loading || !last) {
      return;
    }
    focused.current = true;
    list.current?.querySelector<HTMLElement>(`[data-purpose="${last}"]:not(:disabled)`)?.focus();
  }, [loading, last]);
  const p = (key: string, values?: Record<string, unknown>) =>
    t(`vmBuilder.purpose.${key}`, values);
  // 이 폴더에도 다른 폴더에도 VM을 만들 권한이 없다(만들기를 눌러 403을 받기 전에 알림 — 리뷰 S5)
  const cannot = !parts(state, b.ctx).some((x) => x.kind === "folder");
  const noDisk = loading ? undefined : noDiskReason(state, b.ctx);
  return (
    <section
      aria-labelledby="vb-purpose"
      aria-busy={loading || undefined}
      className="flex flex-col gap-3"
    >
      <h2 id="vb-purpose" className="text-base font-medium">
        {p("question")}
      </h2>
      <p className="text-sm text-muted-foreground">{p("intro")}</p>
      {cannot && !loading && (
        <p role="alert" className="text-sm" data-level="warn">
          {p("noPermission")}
        </p>
      )}
      {loading && <p className="text-sm text-muted-foreground">{p("loading")}</p>}
      <ul ref={list} className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
        {PURPOSES.map((id) => {
          const Icon = ICON[id];
          const size = PURPOSE_SIZE[id];
          const blocked = loading ? undefined : purposeBlocked(id, state, b.ctx);
          const disks =
            size && size.disks.length > 0
              ? noDisk
                ? p(`noDisk.${noDisk}`)
                : p("disks", { disks: size.disks.map((g) => `${g} GiB`).join(" + ") })
              : "";
          const body = (
            <>
              <span
                aria-hidden
                className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-brand/10 text-brand"
              >
                <Icon className="size-5" />
              </span>
              <span className="flex min-w-0 flex-col gap-0.5">
                <span className="font-medium">{p(`${id}.title`)}</span>
                <span className="text-xs text-muted-foreground">{p(`${id}.text`)}</span>
                {size && id !== "blank" && (
                  <span className="text-xs">
                    {[p("size", { cores: size.cores, memory: formatMib(size.memory) }), disks]
                      .filter(Boolean)
                      .join(" · ")}
                  </span>
                )}
                {blocked && (
                  <span className="text-xs text-muted-foreground">
                    {t(blocked.key, blocked.values)}
                  </span>
                )}
              </span>
            </>
          );
          const card =
            "flex h-full w-full items-start gap-3 rounded-xl border bg-card p-3 text-left text-sm hover:border-brand disabled:opacity-60 disabled:hover:border-border";
          return (
            <li key={id}>
              {id === "image" ? (
                <Link to="/images" className={card} data-purpose={id}>
                  {body}
                </Link>
              ) : (
                <button
                  type="button"
                  className={cn(card, id === "template" && picking && "border-brand")}
                  data-purpose={id}
                  disabled={!!blocked || loading}
                  aria-expanded={id === "template" ? picking : undefined}
                  onClick={() => (id === "template" ? setPicking(!picking) : onChoose(id))}
                >
                  {body}
                </button>
              )}
            </li>
          );
        })}
      </ul>
      {picking && (
        <section aria-labelledby="vb-templates" className="flex flex-col gap-2">
          <h3 id="vb-templates" className="text-sm font-medium">
            {p("pickTemplate")}
          </h3>
          <ul className="flex flex-wrap gap-2">
            {templates(b.ctx).map((vm) => (
              <li key={vm.id}>
                <button
                  type="button"
                  className="flex flex-col rounded-lg border bg-card px-3 py-2 text-left text-sm hover:border-brand"
                  onClick={() => onChoose("template", vm.id)}
                >
                  <span className="font-medium">{vm.name}</span>
                  <span className="text-xs text-muted-foreground">
                    {t("vmBuilder.info.template", {
                      vcpus: (vm.spec.cpu.sockets ?? 1) * vm.spec.cpu.cores,
                      memory: formatMib(vm.spec.memory_mib),
                      count: (vm.spec.disks ?? []).length,
                    })}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
    </section>
  );
}
