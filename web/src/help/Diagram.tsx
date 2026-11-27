import { lazy, Suspense } from "react";

import type { DiagramId } from "@/help/diagramIds";
import { loadHelpStrings } from "@/help/strings";

// 그림과 그 문구는 처음 그릴 때 함께 받는다(첫 화면 묶음에 넣지 않음).
const Drawn = lazy(async () => {
  const [module] = await Promise.all([import("@/help/diagrams"), loadHelpStrings()]);
  return module;
});

/** 도움말 그림 하나(늦게 읽기). 받는 동안은 그 자리만 비워 둔다. */
export function Diagram({ id, className }: { id: DiagramId; className?: string }) {
  return (
    <div className={className} data-slot="diagram">
      <Suspense fallback={<div aria-hidden className="h-32" />}>
        <Drawn id={id} />
      </Suspense>
    </div>
  );
}
