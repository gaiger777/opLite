import type { TFunction } from "i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import type { VmImport } from "@/api/types";
import { runUpload, sendFile, type SendOptions } from "@/storage/upload";

/** OVA를 보내기 전에 서버가 받을 수 있는지 묻는다(`/vm-imports/ova/check`, 204). 거절하면 `ApiError`. */
export async function checkOva(name: string, size: number, signal?: AbortSignal) {
  await call(api.POST("/vm-imports/ova/check", { params: { query: { name, size } }, signal }));
}

/**
 * OVA를 `/vm-imports/ova`로 올린다. 서버가 받아 해석한 가져오기(201)로 끝난다. 파일 이름은 표시용으로
 * 함께 보낸다. 중간에 끊기면 서버가 받은 파일을 지운다.
 */
export function uploadOva(file: File, name: string, options: SendOptions): Promise<VmImport> {
  const query = new URLSearchParams({ name, file_name: file.name });
  return sendFile("/vm-imports/ova", query, file, 201, isImport, options);
}

function isImport(body: unknown): body is VmImport {
  return typeof (body as Partial<VmImport> | null)?.id === "string";
}

/**
 * OVA 업로드를 시작한다(이미지 올리기와 같은 진행률 알림). 다 올리면 가져오기 화면을 여는 버튼을
 * 알림에 보인다. `open`은 가져오기 이름을 받는다.
 */
export function startOvaUpload(
  file: File,
  name: string,
  t: TFunction,
  open: (name: string) => void,
) {
  runUpload(
    file,
    {
      name,
      check: (signal) => checkOva(name, file.size, signal),
      send: (options) => uploadOva(file, name, options),
      done: (imp, id) => {
        toast.success(t("imports.uploaded", { name: imp.name }), {
          id,
          duration: 10_000,
          closeButton: true,
          action: { label: t("imports.review"), onClick: () => open(imp.name) },
        });
      },
      noResponse: t("imports.noResponse", { name }),
    },
    t,
  );
}
