/**
 * 실제 부팅 순서(서버의 `effective_boot`와 같은 규칙). CD-ROM이 없으면 그대로다. CD-ROM이 있으면: 아무 장치에도 값이
 * 없을 때 디스크를 순서대로 1, 2, …, 그 뒤 CD-ROM; 값이 CD-ROM에만 있으면 디스크를 가장 큰 값 뒤에 차례로, 그 뒤 값
 * 없는 CD-ROM; 디스크에 값이 있으면 값 없는 디스크는 부팅하지 않고 값 없는 CD-ROM은 가장 큰 값 + 1부터 드라이브 순.
 * 255를 넘는 자리는 없음(null)이다.
 */
export function effectiveBoot(
  disks: readonly (number | null)[],
  cdroms: readonly (number | null)[],
): { disks: (number | null)[]; cdroms: (number | null)[] } {
  if (cdroms.length === 0) {
    return { disks: [...disks], cdroms: [] };
  }
  const given = [...disks, ...cdroms].filter((o): o is number => o !== null);
  // 디스크에 값이 하나도 없으면 디스크를 가장 큰 값(없으면 0) 뒤에 차례로
  const numbered = disks.every((o) => o === null);
  const start = given.length === 0 ? 1 : Math.max(...given) + 1;
  const outDisks = numbered ? disks.map((_, i) => start + i) : [...disks];
  let next = numbered ? start + disks.length : start;
  const outCdroms = cdroms.map((o) => o ?? next++);
  const narrow = (o: number | null) => (o !== null && o > MAX_BOOT_ORDER ? null : o);
  return { disks: outDisks.map(narrow), cdroms: outCdroms.map(narrow) };
}

/** 서버의 `boot_order`(u8) 최댓값 */
export const MAX_BOOT_ORDER = 255;

/** 부팅 장치 하나: 디스크·CD-ROM과 그 순번 */
export interface BootDevice {
  kind: "disk" | "cdrom";
  index: number;
}

/**
 * 부팅 순서대로 놓은 장치(값이 같으면 디스크 먼저, 드라이브 순)와 부팅하지 않는 장치. `effectiveBoot`의 결과를 쓴다.
 */
export function bootList(
  disks: readonly (number | null)[],
  cdroms: readonly (number | null)[],
): { booting: BootDevice[]; off: BootDevice[] } {
  const boot = effectiveBoot(disks, cdroms);
  const all = [
    ...boot.disks.map((order, index) => ({ kind: "disk" as const, index, order })),
    ...boot.cdroms.map((order, index) => ({ kind: "cdrom" as const, index, order })),
  ];
  const booting = all
    .filter((d) => d.order !== null)
    .sort((a, b) => a.order! - b.order!)
    .map(({ kind, index }) => ({ kind, index }));
  const off = all.filter((d) => d.order === null).map(({ kind, index }) => ({ kind, index }));
  return { booting, off };
}

/** 장치 이름(`disk0`, `cdrom1` — CLI `--boot`와 같은 이름) */
export function deviceName(d: BootDevice): string {
  return `${d.kind}${d.index}`;
}
