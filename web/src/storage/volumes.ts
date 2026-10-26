import { validName } from "@/vms/form";

/** `/volumes/import` 경로와 겹쳐 서버가 볼륨 이름으로 받지 않는다. */
const RESERVED = "import";

/** 새 볼륨 이름의 입력 오류(번역 키). `taken`은 이미 있는 볼륨의 이름이다. */
export function volumeNameError(name: string, taken: readonly string[]): string | undefined {
  if (!validName(name)) {
    return "forms.name";
  }
  if (name === RESERVED) {
    return "volumes.errors.reserved";
  }
  if (taken.includes(name)) {
    return "volumes.errors.exists";
  }
  return undefined;
}
