import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";

import { TableCell, TableRow } from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";

/** 보일 행이 없는 표의 한 줄: 불러오는 중, 읽지 못한 이유, 빈 목록의 문장 */
export function TableMessage({
  colSpan,
  query,
  empty,
}: {
  colSpan: number;
  query: { isPending: boolean; isError: boolean; error: unknown };
  empty: string;
}) {
  const { t } = useTranslation();
  return (
    <TableRow>
      <TableCell colSpan={colSpan} className="text-center text-muted-foreground">
        {tableMessage(t, query, empty)}
      </TableCell>
    </TableRow>
  );
}

/** 보일 행이 없을 때의 문장. 표(`TableMessage`)와 휴대폰 카드 목록이 같이 쓴다. */
export function tableMessage(
  t: TFunction,
  query: { isPending: boolean; isError: boolean; error: unknown },
  empty: string,
): string {
  return query.isPending
    ? t("common.loading")
    : query.isError
      ? errorMessage(t, query.error)
      : empty;
}
