// 조각마다 이어 셀 수 있는 SHA-256(계획 77-8 리뷰 I6). Web Crypto의 `digest`는 한 번에 다 주어야 해서 수십 GB 파일을 조각으로
// 올리며 셀 수 없다 — 새 의존성 없이 작은 순수 구현을 둔다. `clone()`으로 조각을 보내는 동안 앞서 세 두고, 서버가 받은 데가
// 다르면 버린다.

const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

const INIT = [
  0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const w = new Uint32Array(64);

export class Sha256 {
  private h = new Uint32Array(INIT);
  private buf = new Uint8Array(64);
  private used = 0;
  /** 넣은 바이트 수 */
  length = 0;

  update(data: Uint8Array): this {
    let i = 0;
    this.length += data.length;
    if (this.used > 0) {
      const n = Math.min(64 - this.used, data.length);
      this.buf.set(data.subarray(0, n), this.used);
      this.used += n;
      i = n;
      if (this.used < 64) {
        return this;
      }
      this.block(this.buf, 0);
      this.used = 0;
    }
    for (; i + 64 <= data.length; i += 64) {
      this.block(data, i);
    }
    if (i < data.length) {
      this.buf.set(data.subarray(i));
      this.used = data.length - i;
    }
    return this;
  }

  clone(): Sha256 {
    const c = new Sha256();
    c.h.set(this.h);
    c.buf.set(this.buf);
    c.used = this.used;
    c.length = this.length;
    return c;
  }

  /** 16진 소문자 다이제스트(이 객체는 그대로 둔다) */
  hex(): string {
    const c = this.clone();
    const bits = c.length * 8;
    const pad = new Uint8Array((c.used < 56 ? 56 : 120) - c.used + 8);
    pad[0] = 0x80;
    const view = new DataView(pad.buffer);
    view.setUint32(pad.length - 8, Math.floor(bits / 0x1_0000_0000));
    view.setUint32(pad.length - 4, bits >>> 0);
    c.update(pad);
    return Array.from(c.h, (x) => x.toString(16).padStart(8, "0")).join("");
  }

  private block(d: Uint8Array, at: number) {
    for (let t = 0; t < 16; t++) {
      const j = at + t * 4;
      w[t] = (d[j]! << 24) | (d[j + 1]! << 16) | (d[j + 2]! << 8) | d[j + 3]!;
    }
    for (let t = 16; t < 64; t++) {
      const a = w[t - 15]!;
      const b = w[t - 2]!;
      const s0 = ((a >>> 7) | (a << 25)) ^ ((a >>> 18) | (a << 14)) ^ (a >>> 3);
      const s1 = ((b >>> 17) | (b << 15)) ^ ((b >>> 19) | (b << 13)) ^ (b >>> 10);
      w[t] = (w[t - 16]! + s0 + w[t - 7]! + s1) | 0;
    }
    const h = this.h;
    let a = h[0]!;
    let b = h[1]!;
    let c = h[2]!;
    let e = h[4]!;
    let f = h[5]!;
    let g = h[6]!;
    let dd = h[3]!;
    let hh = h[7]!;
    for (let t = 0; t < 64; t++) {
      const S1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
      const ch = (e & f) ^ (~e & g);
      const t1 = (hh + S1 + ch + K[t]! + w[t]!) | 0;
      const S0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (S0 + maj) | 0;
      hh = g;
      g = f;
      f = e;
      e = (dd + t1) | 0;
      dd = c;
      c = b;
      b = a;
      a = (t1 + t2) | 0;
    }
    h[0] = (h[0]! + a) | 0;
    h[1] = (h[1]! + b) | 0;
    h[2] = (h[2]! + c) | 0;
    h[3] = (h[3]! + dd) | 0;
    h[4] = (h[4]! + e) | 0;
    h[5] = (h[5]! + f) | 0;
    h[6] = (h[6]! + g) | 0;
    h[7] = (h[7]! + hh) | 0;
  }
}

/** 파일 조각을 읽는 크기 — 한 번에 너무 오래 화면을 잡지 않게 이만큼씩 읽고 센다 */
const PIECE = 1024 * 1024;

async function bytesOf(blob: Blob): Promise<Uint8Array> {
  if (typeof blob.arrayBuffer === "function") {
    return new Uint8Array(await blob.arrayBuffer());
  }
  // 오래된 환경(jsdom 등)
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(new Uint8Array(reader.result as ArrayBuffer));
    reader.onerror = () => reject(reader.error);
    reader.readAsArrayBuffer(blob);
  });
}

/** `hash`에 `blob`의 `from..to`를 더한다(멈추면 — `signal` — 거기서 그친다). */
export async function hashRange(
  hash: Sha256,
  blob: Blob,
  from: number,
  to: number,
  signal?: AbortSignal,
): Promise<Sha256> {
  for (let at = from; at < to && !signal?.aborted; at += PIECE) {
    hash.update(await bytesOf(blob.slice(at, Math.min(at + PIECE, to))));
  }
  return hash;
}

/** 파일 앞 1 MiB(작으면 전체)의 SHA-256 — 이어 올리기 지문 */
export async function headSha256(file: Blob): Promise<string> {
  return (await hashRange(new Sha256(), file, 0, Math.min(file.size, PIECE))).hex();
}
