// OS 고르기(계획 77-5 리뷰 I1): 용도 카드는 디스크에 OS를 넣지 않는다. 리눅스·웹·DB 카드는 고른 직후 그림 위에 이 카드를
// 열어 템플릿(이미지 라이브러리에서 받은 이미지는 템플릿이 된다 — 디스크를 그 이미지에서 복사)이나 설치 ISO(CD 드라이브,
// 먼저 부팅)를 고르게 한다. Windows는 설치 ISO와 선택으로 virtio 드라이버 ISO(둘째 CD)다. 고르지 않아도 되고 그때는 검사가
// "부팅할 OS가 없습니다"라고 경고한다. 값은 놓기(`drop`)로 바꾸므로 놓기 규칙(아키텍처·HA·SATA 포트)이 같다.
import { X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { Button } from "@/components/ui/button";
import { formatMib } from "@/lib/format";
import { useDnd } from "@/dnd/DndProvider";
import { useBuilder } from "@/vmBuilder/context";
import type { Thing } from "@/vmBuilder/labels";
import { drop } from "@/vmBuilder/model/drop";
import { type Part, type PartOf, parts } from "@/vmBuilder/model/parts";
import { insertIso } from "@/vmBuilder/model/purposes";

export function OsPicker() {
  const { t } = useTranslation();
  const b = useBuilder();
  const dnd = useDnd();
  const s = b.s;
  const all = parts(s, b.ctx);
  const isos = all.filter((p): p is PartOf<"iso"> => p.kind === "iso");
  const templates = all.filter((p): p is PartOf<"template"> => p.kind === "template");
  const windows = s.preset === "windows";
  const o = (key: string, values?: Record<string, unknown>) => t(`vmBuilder.os.${key}`, values);
  const done = (said: string) => {
    b.select(null);
    dnd.announce(said);
  };
  /** 템플릿에서: 바탕을 템플릿으로 바꾸고 카드의 vCPU·메모리는 지킨다. */
  const fromTemplate = (part: PartOf<"template">) => {
    const v = s.values;
    const next = drop(s, { ...part, label: part.name } as Thing, "vm", b.ctx);
    if (next !== s) {
      b.record({
        ...next,
        values: { ...next.values, sockets: v.sockets, cores: v.cores, memory: v.memory },
      });
      done(o("fromTemplate", { name: part.name }));
    }
  };
  const iso = (part: PartOf<"iso">, which: "install" | "drivers") => {
    const next = insertIso(s, b.ctx, part.iso, which);
    if (next !== s) {
      b.record(next);
      if (which === "install" && !windows) {
        done(o("inserted", { name: part.iso }));
      } else {
        dnd.announce(o("inserted", { name: part.iso }));
      }
    }
  };
  const cd = (i: number) => s.values.cdroms[i]?.iso ?? "";
  return (
    <section aria-labelledby="vb-os" className="flex flex-col gap-3 rounded-lg border bg-card p-3">
      <div className="flex items-center justify-between gap-2">
        <h3 id="vb-os" tabIndex={-1} className="text-sm font-medium">
          {o("title")}
        </h3>
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          aria-label={o("close")}
          onClick={() => b.select(null)}
        >
          <X />
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">{o(windows ? "windowsText" : "text")}</p>
      {!windows && (
        <Choices
          title={o("templates")}
          empty={o("noTemplates")}
          list={templates}
          detail={(p) =>
            t("vmBuilder.info.template", {
              vcpus: p.vcpus,
              memory: formatMib(p.memory),
              count: p.disks,
            })
          }
          onPick={fromTemplate}
        />
      )}
      <Choices
        title={o(windows ? "installIso" : "iso")}
        empty={o("noIsos")}
        list={isos}
        current={cd(0)}
        onPick={(p) => iso(p, "install")}
      />
      {windows && (
        <Choices
          title={o("driversIso")}
          empty={o("noIsos")}
          list={isos}
          current={cd(1)}
          onPick={(p) => iso(p, "drivers")}
        />
      )}
      <div className="flex flex-wrap items-center gap-3">
        <Button type="button" size="sm" variant="outline" onClick={() => b.select(null)}>
          {windows ? o("done") : o("later")}
        </Button>
        <Button type="button" variant="link" size="sm" className="h-auto px-0" asChild>
          <Link to="/images">{o("library")}</Link>
        </Button>
      </div>
    </section>
  );
}

function Choices<P extends Part>({
  title,
  empty,
  list,
  current,
  detail,
  onPick,
}: {
  title: string;
  empty: string;
  list: P[];
  current?: string;
  detail?(p: P): string;
  onPick(p: P): void;
}) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-1">
      <h4 className="text-xs font-medium text-muted-foreground">{title}</h4>
      {list.length === 0 ? (
        <p className="text-xs text-muted-foreground">{empty}</p>
      ) : (
        <ul className="flex max-h-40 flex-col gap-1 overflow-y-auto">
          {list.map((p) => (
            <li key={p.id}>
              <button
                type="button"
                disabled={!!p.blocked}
                aria-pressed={current !== undefined ? current === p.name : undefined}
                onClick={() => onPick(p)}
                className="flex w-full flex-col rounded-md border px-2 py-1 text-left text-sm hover:border-brand disabled:opacity-60 aria-pressed:border-brand"
              >
                <span className="truncate font-medium">{p.name}</span>
                {(p.blocked || detail) && (
                  <span className="text-xs text-muted-foreground">
                    {p.blocked ? t(p.blocked.key, p.blocked.values) : detail!(p)}
                  </span>
                )}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
