/**
 * 화면 머리: 제목(h1, 20px)과 오른쪽 동작 한 줄, 밑에 짧은 설명. 영역 탭·이동 경로는 셸이 그리므로 화면은
 * 이것만 둔다(설계 §6·§8.2 5).
 */
export function PageHeader({
  title,
  description,
  actions,
  id,
}: {
  title: React.ReactNode;
  description?: React.ReactNode;
  actions?: React.ReactNode;
  id?: string;
}) {
  return (
    <div className="flex flex-col gap-1">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id={id} className="text-xl font-semibold">
          {title}
        </h1>
        {actions && <div className="flex flex-wrap items-center gap-2">{actions}</div>}
      </div>
      {description && <p className="text-sm text-muted-foreground">{description}</p>}
    </div>
  );
}
