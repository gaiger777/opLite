import {
  cloudInitErrors,
  cloudInitFrom,
  cloudInitSummary,
  DEFAULT_NET,
  emptyCloudInit,
  ipConfigFrom,
  isCryptHash,
  PASSWORD_MASK,
  toCloudInit,
  userDataWarning,
  validateKeys,
  validHostname,
} from "@/vms/cloudInit";

const KEY = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIK0 me@laptop";
const on = { ...emptyCloudInit(), enabled: true };

it("builds address settings", () => {
  expect(ipConfigFrom("dhcp", "", "")).toEqual({ mode: "dhcp" });
  expect(ipConfigFrom("none", "", "")).toBeUndefined();
  expect(ipConfigFrom("slaac", "", "")).toEqual({ mode: "slaac" });
  expect(ipConfigFrom("static", " 10.0.0.5/24 ", "")).toEqual({
    mode: "static",
    address: "10.0.0.5/24",
  });
  expect(ipConfigFrom("static", "10.0.0.5/24", "10.0.0.1")).toEqual({
    mode: "static",
    address: "10.0.0.5/24",
    gateway: "10.0.0.1",
  });
});

it("leaves out empty fields and network-config when every NIC is default", () => {
  expect(toCloudInit(emptyCloudInit(), 1)).toBeUndefined();
  expect(toCloudInit(on, 2)).toEqual({});
  expect(
    toCloudInit(
      {
        ...on,
        hostname: "web.lab",
        user: "orbi",
        password: "secret",
        sshKeys: `\n${KEY}\n\n`,
        networks: [{ ...DEFAULT_NET, ipv4: "static", ipv4Address: "10.0.0.5/24" }],
        dns: "10.0.0.2, 2001:db8::53",
        search: "lab",
        vendorData: "#cloud-config\nruncmd: [true]\n",
      },
      2,
    ),
  ).toEqual({
    hostname: "web.lab",
    user: "orbi",
    password: "secret",
    ssh_keys: [KEY],
    // 주소를 정하면 모든 NIC를 적는다(정하지 않은 NIC는 DHCP).
    networks: [{ ipv4: { mode: "static", address: "10.0.0.5/24" } }, { ipv4: { mode: "dhcp" } }],
    dns: { servers: ["10.0.0.2", "2001:db8::53"], search: ["lab"] },
    vendor_data: "#cloud-config\nruncmd: [true]\n",
  });
});

it("sends the mask back for a stored password it did not change", () => {
  const saved = cloudInitFrom({ user: "orbi", password: PASSWORD_MASK, ssh_keys: [KEY] });
  expect(saved).toMatchObject({ enabled: true, passwordSet: true, password: "", sshKeys: KEY });
  expect(toCloudInit(saved, 1)).toEqual({ user: "orbi", password: PASSWORD_MASK, ssh_keys: [KEY] });
  // 새 비밀번호를 넣으면 그것을 보내고, 지우면(설정됨을 끄고 비움) 보내지 않는다.
  expect(toCloudInit({ ...saved, password: "new" }, 1)?.password).toBe("new");
  expect(toCloudInit({ ...saved, passwordSet: false }, 1)).not.toHaveProperty("password");
});

it("keeps saved networks even when they are all DHCP", () => {
  // 저장된 설정에 networks가 있으면(network-config를 만든다) 그대로 둔다 — instance-id가 바뀌지 않게.
  const saved = cloudInitFrom({ networks: [{ ipv4: { mode: "dhcp" } }] });
  expect(toCloudInit(saved, 1)).toEqual({ networks: [{ ipv4: { mode: "dhcp" } }] });
  expect(
    cloudInitFrom({ networks: [{ ipv6: { mode: "static", address: "2001:db8::5/64" } }] })
      .networks[0],
  ).toMatchObject({ ipv4: "none", ipv6: "static", ipv6Address: "2001:db8::5/64" });
  // IPv6만 쓰는 NIC는 그대로 되보낸다(서버는 DHCPv4를 넣지 않는다). 빈 항목은 DHCP다.
  const v6only = cloudInitFrom({ networks: [{ ipv6: { mode: "slaac" } }] });
  expect(toCloudInit(v6only, 1)).toEqual({ networks: [{ ipv6: { mode: "slaac" } }] });
  expect(cloudInitFrom({ networks: [{}] }).networks[0]).toMatchObject({
    ipv4: "dhcp",
    ipv6: "none",
  });
  expect(
    cloudInitErrors({ ...v6only, networks: [{ ...v6only.networks[0]!, ipv6: "none" }] }, 1),
  ).toEqual({
    "ci.networks.0.ipv4": "vmForm.errors.ciIpv4None",
  });
});

it("checks SSH keys like the server", () => {
  expect(validateKeys(`${KEY}\n\nssh-rsa AAAAB3 x`)).toBeNull();
  expect(validateKeys(`${KEY}\nnot-a-key AAA`)).toEqual({
    key: "vmForm.errors.ciKey",
    values: { n: "2" },
  });
  expect(validateKeys(Array(33).fill(KEY).join("\n"))?.key).toBe("vmForm.errors.ciKeyCount");
  expect(validateKeys(`ssh-rsa ${"A".repeat(16 * 1024)}`)?.key).toBe("vmForm.errors.ciKeysSize");
});

it("checks the settings like the server", () => {
  expect(cloudInitErrors({ ...emptyCloudInit(), hostname: "Bad Name" }, 1)).toEqual({});
  expect(
    cloudInitErrors(
      {
        ...on,
        hostname: "-web",
        user: "root",
        networks: [
          { ...DEFAULT_NET, ipv4: "static", ipv4Address: "10.0.0.5", ipv4Gateway: "2001:db8::1" },
          { ...DEFAULT_NET, ipv6: "static", ipv6Address: "10.0.0.5/24" },
        ],
        dns: "1.1.1.1 8.8.8.8 9.9.9.9 8.8.4.4",
        search: "a b c d e f g",
        userData: "x".repeat(64 * 1024 + 1),
      },
      2,
    ),
  ).toEqual({
    "ci.hostname": "vmForm.errors.ciHostname",
    "ci.user": "vmForm.errors.ciRoot",
    "ci.networks.0.ipv4Address": "vmForm.errors.ciAddress",
    "ci.networks.0.ipv4Gateway": "vmForm.errors.ciGateway",
    "ci.networks.1.ipv6Address": "vmForm.errors.ciAddress",
    "ci.dns": "vmForm.errors.ciDns",
    "ci.search": "vmForm.errors.ciSearch",
    "ci.userData": "vmForm.errors.ciData",
  });
  expect(cloudInitErrors({ ...on, user: "Orbi", password: "x".repeat(257) }, 1)).toEqual({
    "ci.user": "vmForm.errors.ciUser",
    "ci.password": "vmForm.errors.ciPassword",
  });
  // 해시 모양이어도 엄격한 모양이 아니면 평문이라 길이 상한을 받는다.
  expect(cloudInitErrors({ ...on, password: `$6$salt$${"x".repeat(300)}` }, 1)).toEqual({
    "ci.password": "vmForm.errors.ciPassword",
  });
  expect(cloudInitErrors({ ...on, password: "$y$j9T$salt$hash" }, 1)).toEqual({
    "ci.password": "vmForm.errors.ciPasswordHash",
  });
  expect(cloudInitErrors({ ...on, password: "$2b$10$abc$def" }, 1)).toEqual({
    "ci.password": "vmForm.errors.ciPasswordHash",
  });
  const sha512 =
    "svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1";
  // 서버 `crypt_hashes_are_recognised`와 같은 값
  for (const hash of [
    `$6$saltstring$${sha512}`,
    `$6$rounds=10000$saltstringsaltst$${sha512}`,
    "$5$saltstring$C3o4O1TC6aRHF4FI.QSZMXtHbaj2gSXr4sUc/3NcUi.",
    "$1$saltsalt$9xy1btjgzLYfb7hivXtC//",
    "$2a$10$N9qo8uLOickgx2ZMRZoMyeIjZAgcfl7p92ldGxad68LJZdL17lhWy",
    "$2y$10$N9qo8uLOickgx2ZMRZoMyeIjZAgcfl7p92ldGxad68LJZdL17lhWy",
  ]) {
    expect(isCryptHash(hash)).toBe(true);
  }
  for (const plain of [
    "$6$saltstring$abc",
    "$6$abc",
    "$y$j9T$salt$hash",
    "$2b$10$N9qo8uLOickgx2ZMRZoMyeIjZAgcfl7p92ldGxad68LJZdL17lhWy",
    `$6$saltstringsaltstr$${sha512}`,
    `$6$saltstring$${sha512}x`,
  ]) {
    expect(isCryptHash(plain)).toBe(false);
  }
  expect(validHostname("web-01.lab.example")).toBe(true);
  expect(validHostname("10.0.0.1")).toBe(false);
});

it("warns when the user-data replaces the user, password and keys", () => {
  expect(userDataWarning({ ...on, userData: "#cloud-config\n" })).toBe(false);
  expect(userDataWarning({ ...on, userData: "#cloud-config\n", user: "orbi" })).toBe(true);
  expect(userDataWarning({ ...on, userData: "#cloud-config\n", passwordSet: true })).toBe(true);
  expect(userDataWarning({ ...on, sshKeys: KEY })).toBe(false);
});

it("summarizes the settings", () => {
  expect(
    cloudInitSummary({
      user: "orbi",
      password: PASSWORD_MASK,
      ssh_keys: [KEY, KEY],
      networks: [
        { ipv4: { mode: "static", address: "10.0.0.5/24", gateway: "10.0.0.1" } },
        { ipv4: { mode: "dhcp" }, ipv6: { mode: "slaac" } },
      ],
    }),
  ).toEqual({
    user: "orbi",
    keys: 2,
    password: true,
    addresses: ["nic0 10.0.0.5/24 gw 10.0.0.1", "nic1 ipv6 slaac"],
  });
});
