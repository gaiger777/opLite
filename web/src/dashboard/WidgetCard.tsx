import { useId } from "react";

import { Card, CardAction, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

/** 대시보드 위젯 하나의 카드: 제목(영역 이름), 오른쪽 동작, 본문. 바깥 격자·여백은 `DashboardPage`가 정한다. */
export function WidgetCard({
  title,
  titleId,
  action,
  children,
}: {
  title: string;
  /** 제목의 id(안의 표가 `aria-labelledby`로 가리킬 때). 없으면 만든다. */
  titleId?: string;
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  const made = useId();
  const id = titleId ?? made;
  return (
    <Card role="region" aria-labelledby={id} className="min-w-0">
      <CardHeader>
        <CardTitle id={id}>
          <h2>{title}</h2>
        </CardTitle>
        {action && <CardAction>{action}</CardAction>}
      </CardHeader>
      <CardContent className="flex flex-col gap-3">{children}</CardContent>
    </Card>
  );
}
