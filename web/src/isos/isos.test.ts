import {
  choosableIsos,
  isoPools,
  isoVolumes,
  nameFromUrl,
  sha256Error,
  urlError,
  usedBy,
} from "@/isos/isos";
import { hardware, isoVolume, pool, vm, volume } from "@/test/fixtures";

it("keeps only ISO volumes", () => {
  expect(
    isoVolumes([volume("root", "local"), isoVolume("alpine", "local")]).map((v) => v.name),
  ).toEqual(["alpine"]);
});

it("finds the VMs with an ISO in a drive, also media still being ejected", () => {
  const web = vm("web", "stopped", {
    spec: { ...vm("web", "stopped").spec, cdroms: [{}, { iso: "alpine" }] },
  });
  const db = vm("db", "running", {
    status: {
      ...vm("db", "running").status,
      hardware: hardware({ cdroms: [{ iso: "alpine" }] }),
    },
  });
  expect(usedBy([web, db, vm("app", "stopped")], "alpine")).toEqual(["web", "db"]);
  expect(usedBy([web], "ubuntu")).toEqual([]);
});

it("keeps ISOs only in directory and NFS pools", () => {
  const pools = [
    pool("local"),
    pool("nas", { backend: { kind: "nfs", server: "nas", export: "/x" } }),
    pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } }),
  ];
  expect(isoPools(pools).map((p) => p.name)).toEqual(["local", "nas"]);
});

it("offers only shared-pool ISOs to HA and replicated VMs", () => {
  const pools = [
    pool("local"),
    pool("nas", { backend: { kind: "nfs", server: "nas", export: "/x" } }),
  ];
  const isos = [
    isoVolume("a", "local"),
    isoVolume("b", "nas"),
    isoVolume("c", "nas", { phase: "pending" }),
  ];
  expect(choosableIsos(isos, pools, false).map((v) => v.name)).toEqual(["a", "b"]);
  expect(choosableIsos(isos, pools, true).map((v) => v.name)).toEqual(["b"]);
  expect(choosableIsos(isos, undefined, true).map((v) => v.name)).toEqual(["a", "b"]);
});

it("suggests a name from the URL", () => {
  expect(nameFromUrl("https://cdn.example/alpine/alpine-virt-3.20.3-x86_64.iso?x=1")).toBe(
    "alpine-virt-3-20-3-x86-64",
  );
  expect(nameFromUrl("https://cloud-images.ubuntu.com/noble/current/noble-server.img")).toBe(
    "noble-server",
  );
  expect(nameFromUrl("https://example.com/")).toBe("");
  expect(nameFromUrl("not a url/Ubuntu 24.04.iso")).toBe("ubuntu-24-04");
  expect(nameFromUrl("https://example.com/Ubuntu%2024.04.iso")).toBe("ubuntu-24-04");
  // 잘못된 퍼센트 인코딩에도 던지지 않는다.
  expect(nameFromUrl("https://example.com/alpine%E0%A4%A.iso")).toBe("alpine-e0-a4-a");
  expect(nameFromUrl("https://example.com/a%")).toBe("a");
});

it("checks URLs and checksums like the server", () => {
  expect(urlError("https://example.com/a.iso")).toBeUndefined();
  expect(urlError("http://10.0.0.1:8080/a.iso")).toBeUndefined();
  expect(urlError("ftp://example.com/a.iso")).toBe("downloads.errors.url");
  expect(urlError("example.com/a.iso")).toBe("downloads.errors.url");
  expect(sha256Error("")).toBeUndefined();
  expect(sha256Error("A".repeat(64))).toBeUndefined();
  expect(sha256Error("a".repeat(63))).toBe("downloads.errors.sha256");
});
