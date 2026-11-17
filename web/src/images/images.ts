import type { components } from "@/api/schema";
import type { Task } from "@/api/types";

type Schemas = components["schemas"];
export type CatalogEntry = Schemas["CatalogEntry"];
export type CatalogImage = Schemas["CatalogImage"];
export type Arch = Schemas["Arch"];

export const ARCHES: readonly Arch[] = ["x86_64", "aarch64"];

/** 서버의 기본 볼륨 이름: id의 `.`을 `-`로, aarch64면 `-arm64`를 붙인다(`ubuntu-24-04`). */
export function defaultVolumeName(entry: Pick<CatalogImage, "id" | "arch">): string {
  const base = entry.id.replaceAll(".", "-");
  return entry.arch === "aarch64" ? `${base}-arm64` : base;
}

/** 이 항목의 URL에서 받는 중인(끝나지 않은) `volume-download` Task */
export function activeDownloads(entry: Pick<CatalogImage, "url">, tasks: readonly Task[]): Task[] {
  return tasks.filter(
    (task) =>
      task.spec.op.kind === "volume-download" &&
      task.spec.op.url === entry.url &&
      task.status.state !== "succeeded" &&
      task.status.state !== "failed",
  );
}

/**
 * 서버의 `valid_id`: 소문자·숫자·`.`·`-`의 57자까지이고 소문자·숫자로 시작하고 끝난다(기본 볼륨 이름이 리소스 이름이 되게).
 * UUID 모양은 안 된다.
 */
export function validCatalogId(id: string): boolean {
  return (
    /^[a-z0-9]([a-z0-9.-]{0,55}[a-z0-9])?$/.test(id) &&
    !/^([0-9a-f]{8}[.-][0-9a-f]{4}[.-][0-9a-f]{4}[.-][0-9a-f]{4}[.-][0-9a-f]{12}|[0-9a-f]{32})$/.test(
      id,
    )
  );
}

/** 고정 해시 입력의 오류(번역 키): SHA-256은 16진 64자, SHA-512는 128자 */
export function digestError(kind: "sha256" | "sha512", value: string): string | undefined {
  const len = kind === "sha256" ? 64 : 128;
  return new RegExp(`^[0-9a-fA-F]{${len}}$`).test(value) ? undefined : `images.errors.${kind}`;
}
