import { useTranslation } from "react-i18next";

import { Diagram } from "@/help/Diagram";
import { DIAGRAM_IDS, type DiagramId } from "@/help/diagramIds";
import type { HelpTopic } from "@/help/help";

/** 패널의 구역 차례: 무엇인가요 → 어떻게 동작하나요 → 언제 쓰나요 → (그림) → 예 → 주의 */
const SECTIONS = ["what", "how", "when"] as const;
const AFTER = ["example", "caution"] as const;

/** 도움말 패널 본문(늦게 읽기): 주제 `helpInfo.<topic>`의 구역들과 그림. 없는 주제는 칸의 한 줄 설명만 보인다. */
export default function HelpBody({ topic }: { topic: HelpTopic }) {
  const { t, i18n } = useTranslation();
  const base = `helpInfo.${topic.topic}`;
  const has = (key: string) => i18n.exists(`${base}.${key}`);
  const diagram = has("diagram") ? (t(`${base}.diagram`) as DiagramId) : null;
  const known = [...SECTIONS, ...AFTER].some(has) || diagram !== null;
  const section = (key: (typeof SECTIONS)[number] | (typeof AFTER)[number]) =>
    has(key) && (
      <section key={key} className="flex flex-col gap-1">
        <h3
          className={
            key === "caution"
              ? "text-sm font-medium text-warning-fg"
              : "text-sm font-medium text-foreground"
          }
        >
          {t(`helpInfo.panel.${key}`)}
        </h3>
        <p className="text-sm whitespace-pre-line text-muted-foreground">{t(`${base}.${key}`)}</p>
      </section>
    );
  return (
    <div data-help-topic={topic.topic} className="flex flex-col gap-4 p-4">
      {topic.line && <p className="text-sm">{topic.line}</p>}
      {SECTIONS.map(section)}
      {diagram && DIAGRAM_IDS.includes(diagram) && (
        <figure className="flex flex-col items-center gap-1 rounded-lg border bg-surface-1 p-3">
          <Diagram id={diagram} className="w-full" />
        </figure>
      )}
      {AFTER.map(section)}
      {!known && !topic.line && (
        <p className="text-sm text-muted-foreground">{t("helpInfo.panel.noMore")}</p>
      )}
    </div>
  );
}
