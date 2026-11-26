// 사라진 포트 "잊기"(계획 75 리뷰 2): 노드가 기억하는 포트(`known_ports`) 가운데 NIC를 빼거나 바꾼 것을 지운다(서버
// `POST /nodes/{key}/forget-ports`). 그 포트의 링크 경보는 해제 규칙대로 풀린다. 자세히 판과 지도 포트 카드가 쓴다.
// 노드를 바꿀 수 있는(`node` 쓰기) 사용자에게만 보인다.
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { PHYSICAL_KEY } from "@/physical/api";

export function ForgetPort({ node, port }: { node: string; port: string }) {
  const { t } = useTranslation();
  const write = usePermissions().can("write", "node");
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  if (!write) {
    return null;
  }
  return (
    <>
      <button type="button" className="text-xs underline" onClick={() => setOpen(true)}>
        {t("physical.forget.action")}
      </button>
      <ConfirmDialog
        open={open}
        onOpenChange={setOpen}
        title={t("physical.forget.title", { port, node })}
        description={t("physical.forget.description")}
        confirmLabel={t("physical.forget.action")}
        onConfirm={async () => {
          await call(
            api.POST("/nodes/{key}/forget-ports", {
              params: { path: { key: node } },
              body: { ports: [port] },
            }),
          );
          await queryClient.invalidateQueries({ queryKey: PHYSICAL_KEY });
        }}
      />
    </>
  );
}
