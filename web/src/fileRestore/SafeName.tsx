import { nameSegments } from "@/fileRestore/fileRestore";

/**
 * 백업 안 이름(게스트가 정한 것 — 파일 이름·파일시스템 레이블·마운트 덧붙임)을 보인다: 양방향 서식은 `<bdi>`로 가두고, 숨은
 * 문자(제어·양방향·폭 없는 문자)는 `‮` 같은 글로 바꿔 드러낸다. 원래 이름은 API로만 간다.
 */
export function SafeName({ name }: { name: string }) {
  return (
    <bdi>
      {nameSegments(name).map((s, i) =>
        s.escaped ? (
          <span key={i} className="rounded-sm bg-warning-soft px-0.5 font-mono text-warning-fg">
            {s.text}
          </span>
        ) : (
          s.text
        ),
      )}
    </bdi>
  );
}
