import { RESERVED_VOLUME, validName } from "@/vms/form";

/** 새 볼륨 이름의 입력 오류(번역 키). `taken`은 이미 있는 볼륨의 이름이다. */
export function volumeNameError(name: string, taken: readonly string[]): string | undefined {
  if (!validName(name)) {
    return "forms.name";
  }
  if (name === RESERVED_VOLUME) {
    return "volumes.errors.reserved";
  }
  if (taken.includes(name)) {
    return "volumes.errors.exists";
  }
  return undefined;
}
