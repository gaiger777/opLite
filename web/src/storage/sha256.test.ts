import { createHash } from "node:crypto";

import { hashRange, headSha256, Sha256 } from "@/storage/sha256";

const node = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex");

function bytes(n: number): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i++) {
    out[i] = (i * 31 + 7) & 0xff;
  }
  return out;
}

it("matches SHA-256 at block edges", () => {
  for (const n of [0, 1, 3, 55, 56, 63, 64, 65, 119, 120, 128, 1000]) {
    expect(new Sha256().update(bytes(n)).hex(), `${n} bytes`).toBe(node(bytes(n)));
  }
  expect(new Sha256().update(new TextEncoder().encode("abc")).hex()).toBe(
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
  );
});

it("goes on piece by piece and clones keep their own state", () => {
  const all = bytes(5000);
  const h = new Sha256();
  for (let at = 0; at < all.length; at += 777) {
    h.update(all.subarray(at, at + 777));
  }
  expect(h.length).toBe(5000);
  expect(h.hex()).toBe(node(all));
  // hex()는 상태를 바꾸지 않는다.
  expect(h.hex()).toBe(node(all));
  const half = new Sha256().update(all.subarray(0, 2500));
  const copy = half.clone().update(all.subarray(2500));
  expect(copy.hex()).toBe(node(all));
  expect(half.hex()).toBe(node(all.subarray(0, 2500)));
});

it("hashes a part of a file and the fingerprint head", async () => {
  const data = bytes(3 * 1024 * 1024 + 5);
  const file = new Blob([data]);
  const h = await hashRange(new Sha256(), file, 0, 1_500_000);
  await hashRange(h, file, 1_500_000, data.length);
  expect(h.hex()).toBe(node(data));
  expect(await headSha256(file)).toBe(node(data.subarray(0, 1024 * 1024)));
  expect(await headSha256(new Blob([data.slice(0, 10)]))).toBe(node(data.subarray(0, 10)));
});
