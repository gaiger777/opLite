/** UUIDv7 id에 담긴 생성 시각. 리소스 id는 만들 때의 시각을 담는다. */
export function uuidTime(id: string): Date {
  return new Date(parseInt(id.replace(/-/g, "").slice(0, 12), 16));
}

/** 날짜와 시각. 연도는 늘 네 자리다(`short`는 두 자리). */
export function formatTime(date: Date, language: string): string {
  return date.toLocaleString(language, { dateStyle: "medium", timeStyle: "medium" });
}

/** MiB를 읽기 좋게: 512 MiB, 4 GiB */
export function formatMib(mib: number): string {
  return mib >= 1024 && mib % 1024 === 0 ? `${mib / 1024} GiB` : `${mib} MiB`;
}

/** 바이트 수를 1024 단위로 소수 한 자리까지: 512 MiB, 1.5 GiB */
export function formatBytes(bytes: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = 0;
  // 반올림해서 1024가 되어도 다음 단위로 넘긴다(1024 KiB가 아니라 1 MiB).
  while (+value.toFixed(1) >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${+value.toFixed(1)} ${units[unit]}`;
}

/** 노드 용량처럼 GiB로 나누어떨어지지 않는 크기: 1.5 GiB, 15.5 GiB */
export function formatCapacity(mib: number): string {
  return mib >= 1024 ? `${+(mib / 1024).toFixed(1)} GiB` : `${mib} MiB`;
}
