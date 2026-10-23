/** UUIDv7 id에 담긴 생성 시각. 리소스 id는 만들 때의 시각을 담는다. */
export function uuidTime(id: string): Date {
  return new Date(parseInt(id.replace(/-/g, "").slice(0, 12), 16));
}

export function formatTime(date: Date, language: string): string {
  return date.toLocaleString(language, { dateStyle: "short", timeStyle: "medium" });
}

/** MiB를 읽기 좋게: 512 MiB, 4 GiB */
export function formatMib(mib: number): string {
  return mib >= 1024 && mib % 1024 === 0 ? `${mib / 1024} GiB` : `${mib} MiB`;
}
