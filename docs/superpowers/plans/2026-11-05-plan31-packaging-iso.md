# opLite 계획 31: 패키지와 설치 ISO 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** opLite를 `cargo run` 없이 서버에 올릴 수 있게 한다. Debian 13 패키지 `oplite`(opld·oplctl·웹 UI·systemd 유닛)와 설치 ISO를 만든다. ISO는 Debian netinst에 preseed와 패키지를 더한 것이고, 설치가 `vmbr0` 브리지와 `opld.toml`까지 만든다. CI에서 패키지 설치를 시험하고, 별도 워크플로에서 ISO를 KVM에 무인 설치해 첫 부팅의 클러스터까지 확인한다. 하위 프로젝트 9(설치·업데이트·구독)의 첫 계획이다.

**Architecture:**
- **패키지:** 이미 빌드한 릴리스 바이너리를 `dpkg-deb`로 묶는다. 손으로 쓴 control과 maintainer 스크립트를 쓰며, 새 빌드 도구는 없다.
- **설치 ISO:** netinst ISO에서 부트 설정만 꺼내 고친 뒤 `xorriso -boot_image any replay`로 다시 묶는다. 고친 파일과 새 디렉터리만 `-map`해서 원래의 BIOS·UEFI 하이브리드 부트를 그대로 쓴다.
- **설치 후 설정:** Debian 설치 프로그램의 화면(네트워크·디스크·비밀번호)을 그대로 쓰고, 마지막에 late_command 스크립트가 패키지 설치·브리지·설정을 한다. 이 스크립트는 설치 환경의 busybox sh·awk에서 돈다.
- **Rust 변경:** 빌드 버전(`OPL_VERSION`)뿐이다.

**Tech Stack:** Rust 2024(MSRV 1.88), clap. 셸은 POSIX sh, bash(빌드 도구·시험), awk. 패키지와 ISO에는 dpkg-deb, xorriso, Debian 13(trixie) netinst·debian-installer preseed, systemd, ifupdown2를 쓴다. CI는 GitHub Actions(ubuntu-latest, Docker `debian:13`, KVM·OVMF)다. 새 Rust 크레이트는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-05-install-update-subscription-design.md` (§2와 §2.5 "구현(계획 31)")
**선행 계획:** 계획 1~30 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| 빌드 버전, `opld`·`oplctl --version` | 노드별 버전 표시, 합류 버전 확인 (계획 32) |
| Debian 패키지 `oplite`, systemd 유닛, maintainer 스크립트 | APT 저장소·소스 파일·키링, 업데이트 확인·업그레이드 (계획 32) |
| 설치 ISO(amd64, BIOS·UEFI), preseed, 설치 후 브리지·설정 | 오프라인 ISO, arm64, ZFS·RAID 루트, 설치 화면에서 클러스터 만들기 |
| CI `shell`·`package` 작업, ISO 무인 설치 E2E 워크플로 | 구독·텔레메트리 (계획 33) |

## 실행 환경

- **이 Mac에서 하는 것:** Rust 변경(Task 1)과 셸 시험(`packaging/tests/*-test.sh` 가운데 `deb-test.sh` 밖의 것)을 돌린다. 이 시험들은 bash만 있으면 된다.
- **이 Mac에서 못 하는 것:** `dpkg-deb`·`xorriso`·Docker·KVM이 필요한 확인은 하지 않는다. 이 확인은 CI(`ci.yml`의 `shell`·`package`, `iso.yml`)가 한다.
- **shellcheck:** 없으면 CI `shell` 작업이 본다.

## 설계 결정

- **설치 프로그램의 한계:** late_command는 netcfg가 `/etc/network/interfaces`·`/etc/hostname`을 대상에 복사하는 단계보다 먼저 돈다. 그래서 설치 환경 쪽 파일을 원본으로 읽고, 바꾼 결과를 설치 환경과 대상 둘 다에 쓴다.
- **busybox:** 설치 환경의 busybox `tr`은 `[:upper:]` 같은 문자 집합을 모른다. 알파벳을 직접 나열해 바꾼다(시험이 문자 집합을 쓰지 않는지 본다).
- **질문 없는 패키지 설치:** in-target의 debconf가 질문을 띄우면 무인 설치가 멈춘다. 그래서 `DEBIAN_FRONTEND=noninteractive`, conffile 옵션, `</dev/null`, `policy-rc.d`로 막는다.
- **주소를 바꿀 수 없음:** opld의 `address`는 노드 인증서에 들어가므로 클러스터에 들어간 뒤 바꿀 수 없다. DHCP로 설치하면 예약하거나 고정 주소를 쓴다.
- **설치 후 서비스:** 패키지는 설정이 없으면 opld를 시작하지 않는다(`ConditionPathExists`). 지워도 데이터는 남는다.

## 파일 구조

```
crates/opl-types/src/version.rs, crates/{opld,oplctl}/tests/version.rs
packaging/deb/{build-deb.sh, control.in, copyright, opld.service, opld.toml.example, postinst, prerm, postrm}
packaging/iso/{build-iso.sh, debian.lock, preseed/{oplite.cfg, unattended.cfg, oplite-setup.sh, oplite-ci-init.service}}
packaging/tests/{maint-test.sh, build-deb-test.sh, deb-test.sh, setup-test.sh, iso-test.sh, fixtures/…}
.github/workflows/{ci.yml, iso.yml}, .github/actions/build-deb/action.yml
README.md, docs/superpowers/specs/{2026-11-05-install-update-subscription-design.md, 2026-10-20-opl-node-core-design.md}
```

## 편집 블록 읽는 법

코드는 Task마다 세 가지 모양으로 싣는다.
- "`파일`을 만든다": 전체 내용이다.
- "`파일` 전체": 바꾼 뒤의 전체 내용이다.
- "`파일`에서 … 을 다음으로 바꾼다": 앞 블록을 찾아 뒤 블록으로 바꾼다.

바꾸는 블록은 위에서부터 차례로 적용한다. 찾을 코드는 그 시점의 파일에서 줄 단위로 한 번만 나온다.

셸 스크립트의 들여쓰기 탭은 그대로 옮긴다. 실행 파일은 Task의 "실행 권한" 단계에서 `chmod 0755`를 한다.

---

### Task 1: 빌드 버전

`opl_types::version::BUILD`는 빌드할 때 환경 변수 `OPL_VERSION`(비었으면 무시), 없으면 크레이트 버전이다. opld·oplctl의 `--version`이 이 값을 보인다(패키지 버전과 같게). oplctl이 opl-types에 의존한다. 시험은 기대값을 `option_env!("OPL_VERSION")`에서 직접 계산하므로 `OPL_VERSION`을 주고 돌리면 clap 기본 버전으로 되돌린 코드를 잡는다(CI `package` 작업이 그렇게 돌린다).

**Files:**
- Create: `crates/opl-types/src/version.rs`
- Create: `crates/oplctl/tests/version.rs`
- Create: `crates/opld/tests/version.rs`
- Modify: `crates/opl-types/src/lib.rs`
- Modify: `crates/oplctl/Cargo.toml`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/opld/src/main.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-types/src/lib.rs`에서

```rust
pub mod task;
pub mod vm;
```

을 다음으로 바꾼다:

```rust
pub mod task;
pub mod version;
pub mod vm;
```

`crates/opl-types/src/version.rs`를 만든다:

```rust
//! 빌드 버전.

/// 이 빌드의 버전. 패키지 빌드는 환경 변수 `OPL_VERSION`으로 넣고, 없거나 비었으면 크레이트 버전이다.
pub const BUILD: &str = match option_env!("OPL_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_is_opl_version_or_crate_version() {
        let want = match option_env!("OPL_VERSION") {
            Some(v) if !v.is_empty() => v,
            _ => env!("CARGO_PKG_VERSION"),
        };
        assert!(!BUILD.is_empty());
        assert_eq!(BUILD, want);
    }
}
```

`crates/oplctl/Cargo.toml`에서

```toml
native-tls.workspace = true
rustls-pki-types.workspace = true
```

을 다음으로 바꾼다:

```toml
native-tls.workspace = true
opl-types.workspace = true
rustls-pki-types.workspace = true
```

`crates/oplctl/src/cli.rs`에서

```rust
#[derive(Parser)]
#[command(name = "oplctl", version, about = "opLite command-line client")]
struct Cli {
```

을 다음으로 바꾼다:

```rust
#[derive(Parser)]
#[command(name = "oplctl", version = opl_types::version::BUILD, about = "opLite command-line client")]
struct Cli {
```

`crates/oplctl/tests/version.rs`를 만든다:

```rust
//! `oplctl --version`이 빌드 버전을 보이는지 본다. 기대값은 이 시험을 빌드할 때의 `OPL_VERSION`(비었으면
//! 크레이트 버전)으로 따로 구해, clap 기본 `version`(크레이트 버전)으로 되돌리면 OPL_VERSION을 준 시험이 실패한다.

use std::process::Command;

#[test]
fn version_flag_prints_build_version() {
    let want = match option_env!("OPL_VERSION") {
        Some(v) if !v.is_empty() => v,
        _ => env!("CARGO_PKG_VERSION"),
    };
    let out = Command::new(env!("CARGO_BIN_EXE_oplctl"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("oplctl {want}\n")
    );
}
```

`crates/opld/src/main.rs`에서

```rust
#[derive(Parser)]
#[command(version, about = "opLite node daemon")]
struct Args {
```

을 다음으로 바꾼다:

```rust
#[derive(Parser)]
#[command(version = opl_types::version::BUILD, about = "opLite node daemon")]
struct Args {
```

`crates/opld/tests/version.rs`를 만든다:

```rust
//! `opld --version`이 빌드 버전을 보이는지 본다. 기대값은 이 시험을 빌드할 때의 `OPL_VERSION`(비었으면
//! 크레이트 버전)으로 따로 구해, clap 기본 `version`(크레이트 버전)으로 되돌리면 OPL_VERSION을 준 시험이 실패한다.

use std::process::Command;

#[test]
fn version_flag_prints_build_version() {
    let want = match option_env!("OPL_VERSION") {
        Some(v) if !v.is_empty() => v,
        _ => env!("CARGO_PKG_VERSION"),
    };
    let out = Command::new(env!("CARGO_BIN_EXE_opld"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("opld {want}\n")
    );
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1133개, 실패 0

Run: `OPL_VERSION=9.9.9~t cargo test -p opl-types -p opld -p oplctl --lib --test version`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add Cargo.lock crates
git commit -m "feat(opl-types): 빌드 버전 — opl_types::version::BUILD(빌드할 때 환경 변수 OPL_VERSION, 없으면 크레이트 버전), opld·oplctl --version이 이 값을 보임, oplctl이 opl-types에 의존, 단위 시험과 opld·oplctl 바이너리 --version 시험"
```

---

### Task 2: Debian 패키지와 CI shell·package 작업

`packaging/deb/build-deb.sh <version> <out dir>`가 릴리스 바이너리(기본 `target/release`, `OPL_BIN_DIR`)로 `oplite_<version>_amd64.deb`를 만든다(바이너리 `--version`이 패키지 버전과 같아야 함, `umask 022`, `DEBIAN/md5sums`, `dpkg-deb --root-owner-group -Zxz`). 유닛은 `/usr/lib/systemd/system/opld.service`(설정이 없으면 `ConditionPathExists`로 건너뜀, `KillMode=process` — VM의 QEMU는 자기 scope라 opld를 멈춰도 돈다). maintainer 스크립트: 처음 설치면 enable·start, 업그레이드·재설치·abort면 enable돼 있을 때 restart, systemd가 돌지 않으면(설치 프로그램 chroot·컨테이너) enable만, remove·deconfigure에서 stop, purge에서 enable 링크까지 지우되 `/var/lib/opl`·`/etc/opl`은 남긴다. 시험: `maint-test.sh`(가짜 `systemctl`로 인자별 호출), `build-deb-test.sh`(가짜 `dpkg-deb`), `deb-test.sh`(CI의 `debian:13` 컨테이너에서 설치·`--version`·`systemd-analyze verify`·md5sums·purge). CI에 `shell`(shellcheck와 셸 시험)·`package`(웹·릴리스 빌드 `OPL_VERSION=<버전>~ci<run>`, 버전 시험, 패키지, 컨테이너 설치 시험, 아티팩트) 작업.

**Files:**
- Create: `packaging/deb/build-deb.sh`
- Create: `packaging/deb/control.in`
- Create: `packaging/deb/copyright`
- Create: `packaging/deb/opld.service`
- Create: `packaging/deb/opld.toml.example`
- Create: `packaging/deb/postinst`
- Create: `packaging/deb/postrm`
- Create: `packaging/deb/prerm`
- Create: `packaging/tests/build-deb-test.sh`
- Create: `packaging/tests/deb-test.sh`
- Create: `packaging/tests/maint-test.sh`
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml
      - run: pnpm build
```

을 다음으로 바꾼다:

```yaml
      - run: pnpm build

  # 패키징 셸 스크립트: shellcheck, build-deb.sh 버전 확인 시험, maintainer 스크립트 시험(가짜 systemctl)
  shell:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: shellcheck
        run: |
          shopt -s globstar
          shellcheck packaging/**/*.sh packaging/deb/postinst packaging/deb/prerm packaging/deb/postrm
      - run: bash packaging/tests/build-deb-test.sh
      - run: bash packaging/tests/maint-test.sh

  # Debian 패키지 oplite: 웹 UI를 넣은 릴리스 빌드 → build-deb.sh → Debian 13 컨테이너에서 설치 시험
  package:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: pnpm/action-setup@v4
        with:
          package_json_file: web/package.json
      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: pnpm
          cache-dependency-path: web/pnpm-lock.yaml
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: OpenSSL headers (native-tls)
        run: |
          sudo apt-get update
          sudo apt-get install -y libssl-dev pkg-config
      - name: Web UI
        run: |
          pnpm -C web install --frozen-lockfile
          pnpm -C web build
      # 패키지 버전 = 워크스페이스 버전~ci<run 번호>. 바이너리의 --version도 이 값이다.
      - name: Package version
        run: |
          WS_VERSION=$(grep '^version' Cargo.toml | head -n1 | cut -d'"' -f2)
          echo "OPL_VERSION=${WS_VERSION}~ci${{ github.run_number }}" >> "$GITHUB_ENV"
      - run: cargo build --release -p opld --features web-ui -p oplctl
      - run: packaging/deb/build-deb.sh "$OPL_VERSION" dist
      - name: Package info and contents
        run: |
          DEB="dist/oplite_${OPL_VERSION}_amd64.deb"
          dpkg-deb --info "$DEB"
          dpkg-deb --contents "$DEB"
      - name: Install test in Debian 13
        run: |
          docker run --rm -v "$PWD:/work" debian:13 \
            bash /work/packaging/tests/deb-test.sh "/work/dist/oplite_${OPL_VERSION}_amd64.deb"
      # 빌드 버전 시험을 OPL_VERSION을 준 빌드로 다시 돌린다(clap 기본 version으로 되돌리면 실패한다).
      - name: Version tests with OPL_VERSION
        run: OPL_VERSION=9.9.9~t cargo test -p opl-types -p opld -p oplctl --lib --test version
      - uses: actions/upload-artifact@v4
        with:
          name: oplite-deb
          path: dist/oplite_*_amd64.deb
```

`packaging/deb/build-deb.sh`를 만든다:

```sh
#!/bin/sh
# 이미 빌드한 릴리스 바이너리로 Debian 패키지 oplite_<version>_amd64.deb를 만든다.
# 쓰는 법: packaging/deb/build-deb.sh <version> <out dir>
# 바이너리는 OPL_VERSION=<version>으로 빌드해 둔다(--version이 그 버전이 아니면 실패한다):
#   pnpm -C web build && OPL_VERSION=<version> cargo build --release -p opld --features web-ui -p oplctl
# 바이너리 위치는 기본 target/release이고 OPL_BIN_DIR로 바꾼다.
set -eu
# 만드는 파일·디렉터리 권한이 호출한 셸의 umask에 휘둘리지 않게 한다.
umask 022

usage() {
	echo "usage: $0 <version> <out dir>" >&2
	exit 2
}

[ $# -eq 2 ] || usage
VERSION=$1
OUT_DIR=$2
[ -n "$VERSION" ] || usage

DEB_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$DEB_DIR/../.." && pwd)
BIN_DIR=${OPL_BIN_DIR:-$ROOT/target/release}

for bin in opld oplctl; do
	if [ ! -x "$BIN_DIR/$bin" ]; then
		echo "build-deb: $BIN_DIR/$bin 이 없다(먼저 릴리스로 빌드한다)" >&2
		exit 1
	fi
	# 바이너리가 보이는 버전(--version)이 패키지 버전과 같아야 한다.
	got=$("$BIN_DIR/$bin" --version)
	if [ "$got" != "$bin $VERSION" ]; then
		echo "build-deb: $BIN_DIR/$bin --version이 '$got'이다(기대 '$bin $VERSION')." >&2
		echo "build-deb: OPL_VERSION=$VERSION cargo build --release -p opld --features web-ui -p oplctl 로 다시 빌드한다" >&2
		exit 1
	fi
done

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
# mktemp -d는 0700이다. 패키지 루트(/)의 권한이 되므로 0755로 한다.
chmod 0755 "$STAGE"

# 파일 트리
mkdir -p "$STAGE/usr/bin" "$STAGE/usr/lib/systemd/system" \
	"$STAGE/usr/share/oplite" "$STAGE/usr/share/doc/oplite"
install -m 0755 "$BIN_DIR/opld" "$BIN_DIR/oplctl" "$STAGE/usr/bin/"
install -m 0644 "$DEB_DIR/opld.service" "$STAGE/usr/lib/systemd/system/opld.service"
install -m 0644 "$DEB_DIR/opld.toml.example" "$STAGE/usr/share/oplite/opld.toml.example"
install -m 0644 "$DEB_DIR/copyright" "$STAGE/usr/share/doc/oplite/copyright"

# DEBIAN: control과 maintainer 스크립트. Installed-Size(KiB)는 DEBIAN을 넣기 전에 잰다.
INSTALLED_SIZE=$(du -sk "$STAGE" | cut -f1)
mkdir -p "$STAGE/DEBIAN"
sed -e "s/@VERSION@/$VERSION/" -e "s/@INSTALLED_SIZE@/$INSTALLED_SIZE/" \
	"$DEB_DIR/control.in" >"$STAGE/DEBIAN/control"
chmod 0644 "$STAGE/DEBIAN/control"
# dpkg --verify가 보는 md5sums(usr 아래 파일, 정렬)
(cd "$STAGE" && find usr -type f | LC_ALL=C sort | xargs md5sum) >"$STAGE/DEBIAN/md5sums"
chmod 0644 "$STAGE/DEBIAN/md5sums"
for script in postinst prerm postrm; do
	install -m 0755 "$DEB_DIR/$script" "$STAGE/DEBIAN/$script"
done

mkdir -p "$OUT_DIR"
DEB="$OUT_DIR/oplite_${VERSION}_amd64.deb"
dpkg-deb --build --root-owner-group -Zxz "$STAGE" "$DEB"
echo "$DEB"
```

`packaging/deb/control.in`를 만든다:

```
Package: oplite
Version: @VERSION@
Architecture: amd64
Maintainer: opLite developers <gaiger777@gmail.com>
Installed-Size: @INSTALLED_SIZE@
Depends: libc6 (>= 2.39), libssl3t64, qemu-system-x86, qemu-utils, ovmf, iproute2, nftables, systemd
Recommends: nfs-common, lvm2, thin-provisioning-tools, open-iscsi, lvm2-lockd, sanlock, ceph-common
Section: admin
Priority: optional
Homepage: https://github.com/gaiger777/opLite
Description: opLite virtualization platform node
 opLite runs QEMU/KVM virtual machines on a cluster of Linux nodes.
 This package installs the node daemon opld (with the web UI), the
 command line tool oplctl and the opld systemd unit.
```

`packaging/deb/copyright`를 만든다:

```
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: opLite
Source: https://github.com/gaiger777/opLite

Files: *
Copyright: opLite developers
License: Apache-2.0

License: Apache-2.0
 Licensed under the Apache License, Version 2.0 (the "License");
 you may not use this file except in compliance with the License.
 You may obtain a copy of the License at
 .
 http://www.apache.org/licenses/LICENSE-2.0
 .
 Unless required by applicable law or agreed to in writing, software
 distributed under the License is distributed on an "AS IS" BASIS,
 WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 See the License for the specific language governing permissions and
 limitations under the License.
 .
 On Debian systems, the complete text of the Apache License, Version 2.0
 can be found in "/usr/share/common-licenses/Apache-2.0".
```

`packaging/deb/opld.service`를 만든다:

```ini
[Unit]
Description=opLite node daemon
Documentation=https://github.com/gaiger777/opLite
After=network-online.target
Wants=network-online.target
ConditionPathExists=/etc/opl/opld.toml

[Service]
Type=simple
ExecStart=/usr/bin/opld --config /etc/opl/opld.toml
Restart=on-failure
RestartSec=5
KillMode=process
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
```

`packaging/deb/opld.toml.example`를 만든다:

```
# /etc/opl/opld.toml로 복사해 name·address를 고친다
name = "node1"
address = "10.0.0.11"
# [hypervisor]
# accel = "auto"          # auto | kvm | tcg
# systemd_scope = true
# [janitor]
# task_max_age_days = 7     # 끝난 작업(Task)을 남기는 날수(만든 때부터)
# task_max_finished = 1000  # 끝난 작업을 이보다 많이 남기지 않는다(오래된 것부터 지운다)
# [backup]
# tick_secs = 60          # 백업 작업을 확인하는 간격(초, 리더 노드)
# max_concurrent = 2      # 노드마다 동시에 도는 백업 작업(Task)의 최대 수
# [pools]
# refresh_secs = 15         # 노드별 풀 상태를 확인하는 간격(초, NFS 마운트 포함)
```

`packaging/deb/postinst`를 만든다:

```
#!/bin/sh
# oplite postinst. 처음 설치면 opld를 켜고 시작하고, 업그레이드·재설정(abort-remove 등 포함)이면
# enable돼 있을 때 다시 시작한다(debhelper와 같다).
# systemd가 돌지 않으면(컨테이너, 설치 프로그램의 chroot) enable(링크 만들기)만 한다.
set -eu

# 시험(packaging/tests/maint-test.sh)이 바꿀 수 있게 환경 변수로 둔다.
SYSTEMD_DIR="${OPL_SYSTEMD_DIR:-/run/systemd/system}"

case "$1" in
configure | abort-remove | abort-deconfigure)
	# configure의 $2: 이전에 설정한 버전. 비었으면 처음 설치다.
	first=no
	if [ "$1" = configure ] && [ -z "${2:-}" ]; then
		first=yes
	fi
	if [ -d "$SYSTEMD_DIR" ]; then
		systemctl daemon-reload || true
		if [ "$first" = yes ]; then
			systemctl enable opld.service || true
			# 설정(/etc/opl/opld.toml)이 없으면 유닛 조건이 시작을 건너뛴다.
			systemctl start opld.service || true
		elif systemctl is-enabled --quiet opld.service; then
			# VM의 QEMU는 자기 scope에 있어 opld를 다시 시작해도 계속 돈다.
			systemctl restart opld.service || true
		fi
	elif [ "$first" = yes ]; then
		systemctl enable opld.service || true
	fi
	;;
esac

exit 0
```

`packaging/deb/postrm`를 만든다:

```
#!/bin/sh
# oplite postrm. /var/lib/opl(Raft 데이터, 풀, 볼륨)과 /etc/opl은 purge여도 지우지 않는다.
set -eu

SYSTEMD_DIR="${OPL_SYSTEMD_DIR:-/run/systemd/system}"
SYSTEMD_ETC="${OPL_SYSTEMD_ETC:-/etc/systemd/system}"

case "$1" in
remove | purge)
	if [ "$1" = purge ]; then
		# enable이 만든 링크를 정리한다(systemd가 돌지 않아도 된다).
		systemctl disable opld.service || true
		# systemctl이 없거나 실패해도 링크가 남지 않게 한다.
		rm -f "$SYSTEMD_ETC/multi-user.target.wants/opld.service"
	fi
	if [ -d "$SYSTEMD_DIR" ]; then
		systemctl daemon-reload || true
	fi
	;;
esac

exit 0
```

`packaging/deb/prerm`를 만든다:

```
#!/bin/sh
# oplite prerm. 지울 때만 opld를 멈춘다(업그레이드는 postinst가 다시 시작한다).
# VM의 QEMU는 자기 scope에 있어 계속 돌고, 다시 설치한 opld가 이어 맡는다.
set -eu

SYSTEMD_DIR="${OPL_SYSTEMD_DIR:-/run/systemd/system}"

case "$1" in
remove | deconfigure)
	if [ -d "$SYSTEMD_DIR" ]; then
		systemctl stop opld.service || true
	fi
	;;
esac

exit 0
```

`packaging/tests/build-deb-test.sh`를 만든다:

```sh
#!/bin/bash
# build-deb.sh가 바이너리의 --version이 패키지 버전과 같은지 확인하는지 본다.
# 가짜 opld·oplctl(버전을 찍는 스크립트)과 가짜 dpkg-deb(만들 파일만 만듦)를 쓴다. bash만 있으면 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/build-deb-test.sh
set -eu

DEB_DIR="$(cd "$(dirname "$0")/../deb" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/fakebin"
# 가짜 dpkg-deb: 마지막 인자(만들 .deb)를 만든다.
cat >"$WORK/fakebin/dpkg-deb" <<'EOF'
#!/bin/sh
for a; do out=$a; done
echo deb >"$out"
EOF
chmod 0755 "$WORK/fakebin/dpkg-deb"

FAILED=0

ok() { echo "ok   $1"; }
fail() {
	echo "FAIL $1"
	FAILED=1
}

# fake_bins <opld 버전> <oplctl 버전>: <bin> --version이 "<bin> <버전>"을 찍는 가짜 바이너리
fake_bins() {
	rm -rf "${WORK:?}/bin"
	mkdir -p "$WORK/bin"
	printf '#!/bin/sh\necho "opld %s"\n' "$1" >"$WORK/bin/opld"
	printf '#!/bin/sh\necho "oplctl %s"\n' "$2" >"$WORK/bin/oplctl"
	chmod 0755 "$WORK/bin/opld" "$WORK/bin/oplctl"
}

# run <버전>: build-deb.sh를 돌려 종료 코드(code)·stdout·stderr를 남긴다.
run() {
	rm -rf "$WORK/out"
	code=0
	PATH="$WORK/fakebin:$PATH" OPL_BIN_DIR="$WORK/bin" \
		sh "$DEB_DIR/build-deb.sh" "$1" "$WORK/out" >"$WORK/stdout" 2>"$WORK/stderr" || code=$?
}

fake_bins 0.1.0~ci7 0.1.0~ci7
run 0.1.0~ci7
if [ "$code" -eq 0 ] && [ "$(tail -n1 "$WORK/stdout")" = "$WORK/out/oplite_0.1.0~ci7_amd64.deb" ] &&
	[ -f "$WORK/out/oplite_0.1.0~ci7_amd64.deb" ]; then
	ok "버전이 같으면 패키지를 만든다"
else
	fail "버전이 같은데 종료 코드 $code"
	cat "$WORK/stderr"
fi

# check_mismatch <이름>: 종료 코드 1, OPL_VERSION 안내, 패키지 없음
check_mismatch() {
	if [ "$code" -eq 1 ] && grep -q 'OPL_VERSION=0.2.0' "$WORK/stderr" && [ ! -e "$WORK/out" ]; then
		ok "$1"
	else
		fail "$1: 종료 코드 $code"
		cat "$WORK/stderr"
	fi
}

fake_bins 0.1.0 0.2.0
run 0.2.0
check_mismatch "opld 버전이 다르면 실패"
fake_bins 0.2.0 0.1.0
run 0.2.0
check_mismatch "oplctl 버전이 다르면 실패"
# OPL_VERSION 없이 빌드한 바이너리는 크레이트 버전을 보인다. 앞부분만 같아도 안 된다.
fake_bins 0.2.0~ci1 0.2.0~ci1
run 0.2.0
check_mismatch "버전 앞부분만 같으면 실패"

if [ "$FAILED" -ne 0 ]; then
	echo "build-deb-test: 실패"
	exit 1
fi
echo "build-deb-test: 모두 통과"
```

`packaging/tests/deb-test.sh`를 만든다:

```sh
#!/bin/bash
# oplite 패키지를 Debian 13 컨테이너 안에서 설치해 본다(root, systemd는 돌지 않음).
# 쓰는 법(CI): docker run --rm -v "$PWD:/work" debian:13 bash /work/packaging/tests/deb-test.sh /work/<deb>
set -eu

[ $# -eq 1 ] || {
	echo "usage: $0 <oplite deb>" >&2
	exit 2
}
DEB=$1

fail() {
	echo "deb-test: FAIL: $*" >&2
	exit 1
}

# 설치: 의존성은 미러에서 받는다.
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends "$DEB"

VERSION=$(dpkg-query -W -f '${Version}' oplite)
echo "deb-test: oplite $VERSION"

# 바이너리가 패키지 버전을 보인다(OPL_VERSION으로 빌드).
[ "$(opld --version)" = "opld $VERSION" ] || fail "opld --version: $(opld --version)"
[ "$(oplctl --version)" = "oplctl $VERSION" ] || fail "oplctl --version: $(oplctl --version)"

# 경로와 권한
UNIT=/usr/lib/systemd/system/opld.service
for path in /usr/bin/opld /usr/bin/oplctl; do
	[ "$(stat -c '%a %U' "$path")" = "755 root" ] || fail "$path: $(stat -c '%a %U' "$path")"
done
for path in "$UNIT" /usr/share/oplite/opld.toml.example /usr/share/doc/oplite/copyright; do
	[ "$(stat -c '%a %U' "$path")" = "644 root" ] || fail "$path: $(stat -c '%a %U' "$path")"
done

# md5sums(dpkg --verify)
[ -s /var/lib/dpkg/info/oplite.md5sums ] || fail "md5sums 없음"
dpkg --verify oplite || fail "dpkg --verify oplite"

# 유닛 파일 검사
systemd-analyze verify "$UNIT" || fail "systemd-analyze verify $UNIT"

# 설정이 없으면 유닛 조건이 거짓이다(설치가 /etc/opl/opld.toml을 만들지 않는다).
grep -qx 'ConditionPathExists=/etc/opl/opld.toml' "$UNIT" || fail "ConditionPathExists 없음"
[ ! -e /etc/opl/opld.toml ] || fail "/etc/opl/opld.toml이 있다"

# systemd가 돌지 않아도 postinst가 enable 링크를 만든다.
[ -L /etc/systemd/system/multi-user.target.wants/opld.service ] || fail "enable 링크 없음"

# purge: enable 링크는 지우고, 데이터(/var/lib/opl)와 설정(/etc/opl)은 남긴다.
mkdir -p /var/lib/opl /etc/opl
echo keep >/var/lib/opl/deb-test.marker
echo keep >/etc/opl/deb-test.marker
dpkg --purge oplite
LINK=/etc/systemd/system/multi-user.target.wants/opld.service
if [ -e "$LINK" ] || [ -L "$LINK" ]; then fail "purge 뒤 enable 링크가 남음"; fi
[ -f /var/lib/opl/deb-test.marker ] || fail "purge가 /var/lib/opl을 지웠다"
[ -f /etc/opl/deb-test.marker ] || fail "purge가 /etc/opl을 지웠다"

echo "deb-test: 모두 통과"
```

`packaging/tests/maint-test.sh`를 만든다:

```sh
#!/bin/bash
# maintainer 스크립트(postinst·prerm·postrm)가 인자마다 어떤 systemctl 호출을 하는지 본다.
# PATH 앞에 둔 가짜 systemctl이 인자를 한 줄씩 기록한다. bash만 있으면 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/maint-test.sh
set -eu

DEB_DIR="$(cd "$(dirname "$0")/../deb" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/bin" "$WORK/systemd-running"
# is-enabled는 OPL_FAKE_ENABLED(기본 yes)가 no면 실패한다.
cat >"$WORK/bin/systemctl" <<'EOF'
#!/bin/sh
echo "$*" >>"$OPL_FAKE_LOG"
[ "$1" != is-enabled ] || [ "${OPL_FAKE_ENABLED:-yes}" = yes ]
EOF
chmod 0755 "$WORK/bin/systemctl"

FAILED=0

# check <이름> <systemd 돎: yes|no> <기대 호출(줄마다 하나)> <스크립트> [인자...]
check() {
	name=$1 running=$2 expected=$3
	shift 3
	log="$WORK/log"
	: >"$log"
	if [ "$running" = yes ]; then
		systemd_dir="$WORK/systemd-running"
	else
		systemd_dir="$WORK/no-systemd"
	fi
	# OPL_SYSTEMD_ETC: postrm purge가 지우는 enable 링크가 시험 디렉터리 안에 있게 한다.
	if ! PATH="$WORK/bin:$PATH" OPL_FAKE_LOG="$log" OPL_SYSTEMD_DIR="$systemd_dir" \
		OPL_SYSTEMD_ETC="$WORK/etc-systemd" "$DEB_DIR/$1" "${@:2}"; then
		echo "FAIL $name: 스크립트가 0이 아닌 코드로 끝났다"
		FAILED=1
		return
	fi
	if [ "$(cat "$log")" = "$expected" ]; then
		echo "ok   $name"
	else
		echo "FAIL $name"
		echo "  기대:"
		printf '%s\n' "$expected" | sed 's/^/    /'
		echo "  실제:"
		sed 's/^/    /' "$log"
		FAILED=1
	fi
}

check "처음 설치(systemd 돎)" yes "daemon-reload
enable opld.service
start opld.service" postinst configure
check "처음 설치(systemd 없음: 링크만)" no "enable opld.service" postinst configure
check "처음 설치(빈 이전 버전)" yes "daemon-reload
enable opld.service
start opld.service" postinst configure ""
# 업그레이드·재설정: enable돼 있으면 restart한다(debhelper와 같음). remove 뒤 다시 설치한 경우도 같다.
check "업그레이드(systemd 돎)" yes "daemon-reload
is-enabled --quiet opld.service
restart opld.service" postinst configure 0.1.0
OPL_FAKE_ENABLED=no check "업그레이드(enable 안 됨)" yes "daemon-reload
is-enabled --quiet opld.service" postinst configure 0.1.0
check "업그레이드(systemd 없음)" no "" postinst configure 0.1.0
check "remove 뒤 configure <ver>" yes "daemon-reload
is-enabled --quiet opld.service
restart opld.service" postinst configure 0.1.0
check "postinst abort-remove" yes "daemon-reload
is-enabled --quiet opld.service
restart opld.service" postinst abort-remove
check "postinst abort-deconfigure" yes "daemon-reload
is-enabled --quiet opld.service
restart opld.service" postinst abort-deconfigure in-favour other 1.0
check "postinst abort-remove(systemd 없음)" no "" postinst abort-remove
check "postinst abort-upgrade" yes "" postinst abort-upgrade 0.1.0

check "prerm remove(systemd 돎)" yes "stop opld.service" prerm remove
check "prerm remove(systemd 없음)" no "" prerm remove
check "prerm deconfigure" yes "stop opld.service" prerm deconfigure
check "prerm upgrade(멈추지 않음)" yes "" prerm upgrade 0.2.0

check "postrm remove(systemd 돎)" yes "daemon-reload" postrm remove
check "postrm remove(systemd 없음)" no "" postrm remove
check "postrm purge(systemd 돎)" yes "disable opld.service
daemon-reload" postrm purge
check "postrm purge(systemd 없음)" no "disable opld.service" postrm purge
check "postrm upgrade" yes "" postrm upgrade 0.2.0

# purge는 enable 링크를 지운다(systemctl disable이 못 지운 경우에도). remove는 남긴다.
WANTS="$WORK/etc-systemd/multi-user.target.wants"
mkdir -p "$WANTS"
ln -sf /usr/lib/systemd/system/opld.service "$WANTS/opld.service"
check "postrm remove(링크 둠)" no "" postrm remove
if [ -L "$WANTS/opld.service" ]; then echo "ok   remove 뒤 enable 링크 남음"; else
	echo "FAIL remove가 enable 링크를 지웠다"
	FAILED=1
fi
check "postrm purge(링크 지움)" no "disable opld.service" postrm purge
if [ ! -e "$WANTS/opld.service" ] && [ ! -L "$WANTS/opld.service" ]; then echo "ok   purge 뒤 enable 링크 없음"; else
	echo "FAIL purge 뒤 enable 링크가 남았다"
	FAILED=1
fi

# 가짜 systemctl이 실패해도 maintainer 스크립트는 성공해야 한다(|| true).
cat >"$WORK/bin/systemctl" <<'EOF'
#!/bin/sh
echo "$*" >>"$OPL_FAKE_LOG"
exit 1
EOF
check "systemctl 실패를 무시(postinst)" yes "daemon-reload
enable opld.service
start opld.service" postinst configure
check "systemctl 실패를 무시(postrm purge)" yes "disable opld.service
daemon-reload" postrm purge

if [ "$FAILED" -ne 0 ]; then
	echo "maint-test: 실패"
	exit 1
fi
echo "maint-test: 모두 통과"
```

- [ ] **Step 2: 실행 권한**

Run: `chmod 0755 packaging/deb/build-deb.sh packaging/deb/postinst packaging/deb/postrm packaging/deb/prerm packaging/tests/deb-test.sh packaging/tests/maint-test.sh`
Expected: 출력 없음

- [ ] **Step 3: 확인**

Run: `bash packaging/tests/maint-test.sh && bash packaging/tests/build-deb-test.sh`
Expected: 모두 통과

Run: `shellcheck packaging/**/*.sh packaging/deb/postinst packaging/deb/prerm packaging/deb/postrm`
Expected: 경고 없음 (bash는 `shopt -s globstar` 뒤에, shellcheck가 없으면 CI `shell` 작업이 본다)

Run: `ruby -ryaml -e 'ARGV.each { |f| YAML.load_file(f) }' .github/workflows/*.yml .github/actions/build-deb/action.yml`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add .github packaging
git commit -m "feat(packaging): Debian 패키지 oplite와 CI shell·package 작업 — packaging/deb/build-deb.sh <version> <out dir>가 umask 022로 릴리스 바이너리(기본 target/release, OPL_BIN_DIR)로 oplite_<version>_amd64.deb를 만듦(dpkg-deb --root-owner-group -Zxz, Installed-Size는 du -sk, DEBIAN/md5sums), control 템플릿(Depends에 libc6 (>= 2.39)·libssl3t64), opld.service(설정이 없으면 ConditionPathExists로 건너뜀, KillMode=process), postinst(처음 설치면 enable·start, 업그레이드·재설치·abort면 enable돼 있을 때 restart, systemd가 돌지 않으면 enable만)·prerm(remove·deconfigure에서 stop)·postrm(purge에서 disable과 enable 링크 지우기, /var/lib/opl·/etc/opl은 남김), opld.toml.example, copyright, maint-test.sh(가짜 systemctl로 인자별 호출 비교, OPL_SYSTEMD_DIR), deb-test.sh(Debian 13 컨테이너에서 설치·--version·systemd-analyze verify·경로·enable 링크·md5sums·purge 뒤 링크 없음과 데이터 남음), OPL_VERSION을 준 version 시험, ci.yml shell(shellcheck·maint-test)·package(웹·릴리스 빌드 OPL_VERSION=<워크스페이스 버전>~ci<run 번호>, dpkg-deb --info·--contents, debian:13 설치 시험, 아티팩트 oplite-deb)"
```

---

### Task 3: 설치 ISO 스크립트

`packaging/iso/build-iso.sh --netinst <iso> --deb <oplite_*_amd64.deb> --out <iso> [--unattended]`: netinst 해시를 `debian.lock`(Debian 13.7.0 amd64)과 비교하고, 고칠 부트 설정만 꺼내 BIOS(`txt.cfg`의 기본 항목 "Install opLite", unattended면 vesamenu 없이 직렬 콘솔로 바로)·UEFI(`grub.cfg` 첫 항목) 메뉴를 고친 뒤 확인하고, `md5sum.txt`(있으면 `sha256sum.txt`)를 고쳐 바뀐 파일과 `/oplite`·`/preseed`만 `-map`해 `-boot_image any replay`로 다시 묶는다(BIOS·UEFI 하이브리드 부트 유지). preseed `oplite.cfg`(언어·키보드·시간대 UTC는 정하고 네트워크·호스트 이름·도메인·root 비밀번호·디스크는 묻는다, LVM `atomic`, CD를 apt 소스로 쓰지 않음)와 시험용 `unattended.cfg`. late_command `oplite-setup.sh`: 질문 없이 `ifupdown2`·`isc-dhcp-client`·패키지 설치, 기본 경로 인터페이스를 VLAN-aware `vmbr0`으로(설치 환경과 대상 둘 다에 써서 뒤의 netcfg 복사에도 남게), 호스트 이름을 소문자로 한 `name`과 주소로 `/etc/opl/opld.toml`, 영어 콘솔 안내 `/etc/issue.d/oplite.issue`, 단계마다 `oplite-setup:` 줄을 콘솔에. 설치 환경의 busybox를 고려한다(`tr` 문자 집합 없음). 시험: `setup-test.sh`(interfaces 견본·opld.toml·issue, CI에서 busybox로도), `iso-test.sh`(인자·해시·실제 13.7.0 부트 설정 견본으로 메뉴 패치·가짜 xorriso로 전체 흐름).

**Files:**
- Create: `packaging/iso/build-iso.sh`
- Create: `packaging/iso/debian.lock`
- Create: `packaging/iso/preseed/oplite-ci-init.service`
- Create: `packaging/iso/preseed/oplite-setup.sh`
- Create: `packaging/iso/preseed/oplite.cfg`
- Create: `packaging/iso/preseed/unattended.cfg`
- Create: `packaging/tests/fixtures/interfaces-dhcp.in`
- Create: `packaging/tests/fixtures/interfaces-dhcp.out`
- Create: `packaging/tests/fixtures/interfaces-hotplug.in`
- Create: `packaging/tests/fixtures/interfaces-hotplug.out`
- Create: `packaging/tests/fixtures/interfaces-ipv6.in`
- Create: `packaging/tests/fixtures/interfaces-ipv6.out`
- Create: `packaging/tests/fixtures/interfaces-none.in`
- Create: `packaging/tests/fixtures/interfaces-none.out`
- Create: `packaging/tests/fixtures/interfaces-static-nogw.in`
- Create: `packaging/tests/fixtures/interfaces-static-nogw.out`
- Create: `packaging/tests/fixtures/interfaces-static.in`
- Create: `packaging/tests/fixtures/interfaces-static.out`
- Create: `packaging/tests/fixtures/iso/netinst/boot/grub/grub.cfg`
- Create: `packaging/tests/fixtures/iso/netinst/isolinux/gtk.cfg`
- Create: `packaging/tests/fixtures/iso/netinst/isolinux/isolinux.cfg`
- Create: `packaging/tests/fixtures/iso/netinst/isolinux/txt.cfg`
- Create: `packaging/tests/fixtures/iso/normal/boot/grub/grub.cfg`
- Create: `packaging/tests/fixtures/iso/normal/isolinux/gtk.cfg`
- Create: `packaging/tests/fixtures/iso/normal/isolinux/isolinux.cfg`
- Create: `packaging/tests/fixtures/iso/normal/isolinux/txt.cfg`
- Create: `packaging/tests/fixtures/iso/unattended/boot/grub/grub.cfg`
- Create: `packaging/tests/fixtures/iso/unattended/isolinux/gtk.cfg`
- Create: `packaging/tests/fixtures/iso/unattended/isolinux/isolinux.cfg`
- Create: `packaging/tests/fixtures/iso/unattended/isolinux/txt.cfg`
- Create: `packaging/tests/fixtures/opld-badname.toml`
- Create: `packaging/tests/fixtures/opld-dhcp.toml`
- Create: `packaging/tests/fixtures/opld-noaddr.toml`
- Create: `packaging/tests/fixtures/opld-static.toml`
- Create: `packaging/tests/fixtures/oplite.issue`
- Create: `packaging/tests/iso-test.sh`
- Create: `packaging/tests/setup-test.sh`
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml
      - run: bash packaging/tests/maint-test.sh
```

을 다음으로 바꾼다:

```yaml
      - run: bash packaging/tests/maint-test.sh
      - run: bash packaging/tests/setup-test.sh
      # 설치 프로그램 환경(busybox sh·awk)에서 oplite-setup.sh 함수가 같은 결과를 내는지
      - name: setup-test with busybox
        run: |
          sudo apt-get update
          sudo apt-get install -y busybox
          OPL_SH="busybox sh" OPL_AWK="busybox awk" bash packaging/tests/setup-test.sh
      - run: bash packaging/tests/iso-test.sh
```

`packaging/iso/build-iso.sh`를 만든다:

```sh
#!/bin/bash
# Debian netinst ISO에 oplite 패키지와 preseed를 넣고 "Install opLite"를 기본 부트 항목으로 해 다시 묶는다.
# 쓰는 법: packaging/iso/build-iso.sh --netinst <debian netinst.iso> --deb <oplite.deb> --out <oplite.iso> [--unattended]
#   --unattended: 시험 전용. 묻는 값을 모두 정한 preseed(unattended.cfg)와 첫 부팅 cluster init 유닛을 넣고,
#                 BIOS는 메뉴 없이(vesamenu 없이 직렬 콘솔로) 바로, UEFI는 메뉴를 1초 뒤에 고르고 직렬 콘솔(ttyS0)을 쓴다.
# netinst는 debian.lock의 SHA-256과 같아야 한다. Linux에서 xorriso가 필요하다.
# OPLITE_ISO_LIB=1로 source하면 함수만 정의한다(packaging/tests/iso-test.sh).
set -euo pipefail

ISO_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PRESEED_ARGS="auto=true priority=high preseed/file=/cdrom/preseed/oplite.cfg"
UNATTENDED_ARGS="auto=true priority=critical preseed/file=/cdrom/preseed/unattended.cfg console=ttyS0,115200n8"
# ISO에서 꺼내 고치는 파일. 다시 묶을 때는 원래 ISO 위에 이 파일들과 새 디렉터리(/oplite, /preseed)만
# 하나씩 -map으로 얹는다(Debian 위키 RepackBootableISO). 부트 이미지와 boot.cat은 원래 것을 그대로 둔다.
EDIT_FILES=(isolinux/txt.cfg isolinux/gtk.cfg isolinux/isolinux.cfg boot/grub/grub.cfg md5sum.txt)
# 작업 디렉터리. EXIT trap이 main 밖에서도 볼 수 있게 전역이다(set -u).
ISO_WORK=""

usage() {
	echo "usage: $0 --netinst <debian netinst.iso> --deb <oplite.deb> --out <oplite.iso> [--unattended]" >&2
	exit 2
}

# parse_args <인자...>: NETINST·DEB·OUT·UNATTENDED(0|1)를 정한다. 잘못된 인자는 2, 없는 파일은 1로 끝낸다.
parse_args() {
	NETINST="" DEB="" OUT="" UNATTENDED=0
	while [ $# -gt 0 ]; do
		case $1 in
		--netinst | --deb | --out)
			[ $# -ge 2 ] || usage
			case $1 in
			--netinst) NETINST=$2 ;;
			--deb) DEB=$2 ;;
			--out) OUT=$2 ;;
			esac
			shift 2
			;;
		--unattended)
			UNATTENDED=1
			shift
			;;
		*)
			echo "build-iso: 모르는 인자: $1" >&2
			usage
			;;
		esac
	done
	if [ -z "$NETINST" ] || [ -z "$DEB" ] || [ -z "$OUT" ]; then
		usage
	fi
	# 설치는 /cdrom/oplite/oplite_*.deb를 찾는다. build-deb.sh가 만든 이름이어야 한다.
	case $(basename "$DEB") in
	oplite_*_amd64.deb) ;;
	*)
		echo "build-iso: --deb는 oplite_<version>_amd64.deb여야 한다: $DEB" >&2
		exit 2
		;;
	esac
	for f in "$NETINST" "$DEB"; do
		if [ ! -f "$f" ]; then
			echo "build-iso: 파일이 없다: $f" >&2
			exit 1
		fi
	done
}

# lock_value <lock> <KEY>: debian.lock(KEY=value)의 값
lock_value() {
	sed -n "s/^$2=//p" "$1" | head -n1
}

# sha256_file <file>: sha256sum이 없으면(macOS) shasum -a 256
sha256_file() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | cut -d' ' -f1
	else
		shasum -a 256 "$1" | cut -d' ' -f1
	fi
}

# md5_file <file>: md5sum이 없으면(macOS) md5 -q
md5_file() {
	if command -v md5sum >/dev/null 2>&1; then
		md5sum "$1" | cut -d' ' -f1
	else
		md5 -q "$1"
	fi
}

# verify_netinst <iso> <lock>: ISO의 SHA-256이 lock의 DEBIAN_NETINST_SHA256과 다르면 1로 끝낸다.
verify_netinst() {
	local want got
	want=$(lock_value "$2" DEBIAN_NETINST_SHA256)
	if [ -z "$want" ]; then
		echo "build-iso: $2 에 DEBIAN_NETINST_SHA256이 없다" >&2
		exit 1
	fi
	got=$(sha256_file "$1")
	if [ "$got" != "$want" ]; then
		echo "build-iso: $1 의 SHA-256이 debian.lock과 다르다 (기대 $want, 실제 $got)" >&2
		exit 1
	fi
	echo "build-iso: netinst SHA-256 확인 $got"
}

# patch_isolinux <tree> <커널 인자> <unattended 0|1>: BIOS 메뉴(vesamenu) 맨 위에 opLite 항목을 기본으로 넣는다.
# 메뉴의 기본 항목은 `menu default`가 정한다(gtk.cfg의 Graphical install). isolinux.cfg의
# `default vesamenu.c32`를 바꾸면 메뉴가 사라지므로, 그 표시를 opLite 항목으로 옮긴다.
# unattended면 isolinux.cfg를 메뉴 없이 새로 쓴다: vesamenu는 직렬 콘솔에 보이지 않고 화면 없이(-display none)
# 기다릴 수 있다. 직렬 콘솔에 isolinux부터 찍히고 opLite 항목으로 바로 부팅한다.
patch_isolinux() {
	local tree=$1 args=$2 unattended=$3 txt kernel append
	txt="$tree/isolinux/txt.cfg"
	# 첫 label(install)의 kernel과 append를 복사하고 append의 `---` 앞에 preseed 인자를 넣는다.
	kernel=$(awk '$1 == "kernel" { print $2; exit }' "$txt")
	append=$(awk '$1 == "append" { sub(/^[ \t]*append[ \t]+/, ""); sub(/[ \t]+$/, ""); print; exit }' "$txt")
	if [ -z "$kernel" ] || [[ $append != *" ---"* ]]; then
		echo "build-iso: $txt 에서 install 항목의 kernel·append를 찾지 못했다" >&2
		exit 1
	fi
	append="${append%% ---*} $args ---${append#* ---}"
	awk -v kernel="$kernel" -v append="$append" '
	!done && $1 == "label" {
		print "label oplite"
		print "\tmenu label ^Install opLite"
		print "\tmenu default"
		print "\tkernel " kernel
		print "\tappend " append
		done = 1
	}
	{ print }
	' "$txt" >"$txt.new"
	mv "$txt.new" "$txt"
	sed '/^[[:space:]]*menu default[[:space:]]*$/d' "$tree/isolinux/gtk.cfg" >"$tree/isolinux/gtk.cfg.new"
	mv "$tree/isolinux/gtk.cfg.new" "$tree/isolinux/gtk.cfg"
	if [ "$unattended" = 1 ]; then
		# serial은 맨 앞에 있어야 한다. timeout 단위는 1/10초다.
		printf 'serial 0 115200\ndefault oplite\nprompt 0\ntimeout 1\nlabel oplite\n\tkernel %s\n\tappend %s\n' \
			"$kernel" "$append" >"$tree/isolinux/isolinux.cfg"
	fi
}

# patch_grub <tree> <커널 인자> <unattended 0|1>: UEFI 메뉴의 첫 menuentry 앞에 opLite 항목을 넣고 기본으로 한다.
# kernel·initrd·인자는 isolinux txt.cfg의 install 항목(텍스트 설치)에서 가져오므로 patch_isolinux보다 먼저 부른다.
patch_grub() {
	local tree=$1 args=$2 unattended=$3 grub kernel append initrd timeout=""
	grub="$tree/boot/grub/grub.cfg"
	kernel=$(awk '$1 == "kernel" { print $2; exit }' "$tree/isolinux/txt.cfg")
	append=$(awk '$1 == "append" { sub(/^[ \t]*append[ \t]+/, ""); sub(/[ \t]+$/, ""); print; exit }' "$tree/isolinux/txt.cfg")
	initrd=$(sed -n 's/.*initrd=\([^ ]*\).*/\1/p' <<<"$append")
	append=${append/initrd=$initrd /}
	append="${append%% ---*} $args ---${append#* ---}"
	[ "$unattended" = 1 ] && timeout=1
	awk -v kernel="$kernel" -v append="$append" -v initrd="$initrd" -v timeout="$timeout" '
	!done && /^menuentry/ {
		print "set default=0"
		if (timeout != "") print "set timeout=" timeout
		print "menuentry --hotkey=o '\''Install opLite'\'' {"
		print "    set background_color=black"
		print "    linux    " kernel " " append
		print "    initrd   " initrd
		print "}"
		done = 1
	}
	{ print }
	' "$grub" >"$grub.new"
	mv "$grub.new" "$grub"
}

# check_patched <tree> <unattended 0|1>: 메뉴 패치가 들어갔는지 본다. 아니면 1로 끝낸다.
check_patched() {
	local tree=$1 unattended=$2 txt grub isolinux
	txt="$tree/isolinux/txt.cfg" grub="$tree/boot/grub/grub.cfg" isolinux="$tree/isolinux/isolinux.cfg"
	if ! grep -q '^label oplite' "$txt" || ! grep -q 'Install opLite' "$grub"; then
		echo "build-iso: 부트 메뉴에 opLite 항목을 넣지 못했다($txt, $grub)" >&2
		exit 1
	fi
	if [ "$unattended" = 1 ] && { [ "$(head -n1 "$isolinux")" != 'serial 0 115200' ] ||
		! grep -qx 'default oplite' "$isolinux" || ! grep -qx 'set timeout=1' "$grub"; }; then
		echo "build-iso: unattended 부트 설정(직렬 isolinux.cfg, grub timeout)을 쓰지 못했다($isolinux, $grub)" >&2
		exit 1
	fi
}

# sums_update <tree> <목록 파일> <합 함수> <ISO 안 경로...>: <tree>/<목록 파일>에서 그 경로의 합을 새로 하고,
# 없던 경로는 끝에 더한다. 원래 목록에 없는 isolinux/는 부르지 않는다(원래처럼 빠진다).
sums_update() {
	local tree=$1 list="$1/$2" hash=$3 rel sum
	shift 3
	for rel in "$@"; do
		sum=$("$hash" "$tree/$rel")
		awk -v p="./$rel" -v s="$sum" '
		$2 == p { print s "  " p; found = 1; next }
		{ print }
		END { if (!found) print s "  " p }
		' "$list" >"$list.new"
		mv "$list.new" "$list"
	done
}

# md5_update·sha256_update <tree> <ISO 안 경로...>: 설치 프로그램의 CD 검사가 보는 md5sum.txt·sha256sum.txt
md5_update() {
	sums_update "$1" md5sum.txt md5_file "${@:2}"
}
sha256_update() {
	sums_update "$1" sha256sum.txt sha256_file "${@:2}"
}

cleanup() {
	if [ -n "$ISO_WORK" ]; then
		# 꺼낸 파일은 읽기 전용이라 지우기 전에 쓰기 권한을 준다.
		chmod -R u+w "$ISO_WORK" 2>/dev/null || true
		rm -rf "$ISO_WORK"
	fi
}

main() {
	parse_args "$@"
	if ! command -v xorriso >/dev/null 2>&1; then
		echo "build-iso: xorriso가 없다 (apt-get install xorriso)" >&2
		exit 1
	fi
	verify_netinst "$NETINST" "$ISO_DIR/debian.lock"

	local tree args f new=() extract=() map=()
	trap cleanup EXIT
	ISO_WORK=$(mktemp -d)
	tree="$ISO_WORK/iso"

	# 고칠 파일만 꺼낸다.
	for f in "${EDIT_FILES[@]}"; do
		mkdir -p "$(dirname "$tree/$f")"
		extract+=(-extract "/$f" "$tree/$f")
		map+=(-map "$tree/$f" "/$f")
	done
	xorriso -osirrox on -indev "$NETINST" "${extract[@]}"
	# sha256sum.txt는 판에 따라 없을 수 있다. 꺼내 보고 있으면 md5sum.txt처럼 고친다.
	if xorriso -osirrox on -indev "$NETINST" -extract /sha256sum.txt "$tree/sha256sum.txt" >/dev/null 2>&1 &&
		[ -f "$tree/sha256sum.txt" ]; then
		map+=(-map "$tree/sha256sum.txt" /sha256sum.txt)
	else
		rm -f "$tree/sha256sum.txt"
		echo "build-iso: netinst에 sha256sum.txt가 없다(건너뜀)"
	fi
	chmod -R u+w "$tree"

	mkdir -p "$tree/oplite" "$tree/preseed"
	cp "$DEB" "$tree/oplite/"
	new=("oplite/$(basename "$DEB")" preseed/oplite.cfg preseed/oplite-setup.sh)
	cp "$ISO_DIR/preseed/oplite.cfg" "$ISO_DIR/preseed/oplite-setup.sh" "$tree/preseed/"
	args=$PRESEED_ARGS
	if [ "$UNATTENDED" = 1 ]; then
		cp "$ISO_DIR/preseed/unattended.cfg" "$ISO_DIR/preseed/oplite-ci-init.service" "$tree/preseed/"
		new+=(preseed/unattended.cfg preseed/oplite-ci-init.service)
		args=$UNATTENDED_ARGS
	fi

	# patch_grub이 원래 install 항목을 읽으므로 먼저 한다.
	patch_grub "$tree" "$args" "$UNATTENDED"
	patch_isolinux "$tree" "$args" "$UNATTENDED"
	check_patched "$tree" "$UNATTENDED"

	# 설치 프로그램의 CD 검사가 보는 md5sum.txt(와 sha256sum.txt): 고친 grub.cfg와 새 파일의 합(isolinux/는 원래처럼 없다)
	md5_update "$tree" boot/grub/grub.cfg "${new[@]}"
	if [ -f "$tree/sha256sum.txt" ]; then
		sha256_update "$tree" boot/grub/grub.cfg "${new[@]}"
	fi

	rm -f "$OUT"
	xorriso -indev "$NETINST" -outdev "$OUT" -overwrite on \
		-map "$tree/oplite" /oplite -map "$tree/preseed" /preseed "${map[@]}" \
		-boot_image any replay -volid OPLITE
	echo "$OUT"
}

if [ "${OPLITE_ISO_LIB:-}" = 1 ]; then
	return 0
fi
main "$@"
```

`packaging/iso/debian.lock`를 만든다:

```
# 설치 ISO의 바탕인 Debian 13 amd64 netinst 한 판. build-iso.sh와 CI가 읽는다(셸 KEY=value).
# 해시는 cdimage.debian.org의 SHA256SUMS에서 확인했다(2026-11-05).
# 13.7.0은 지금 판이라 아직 cdimage/archive/에 없어 release/ URL을 쓴다. 다음 판이 나오면
# cdimage/archive/13.7.0/로 옮겨지므로 그때 URL을 archive로 바꾸거나 새 판으로 올린다.
DEBIAN_RELEASE=13.7.0
DEBIAN_CODENAME=trixie
DEBIAN_NETINST_URL=https://cdimage.debian.org/cdimage/release/13.7.0/amd64/iso-cd/debian-13.7.0-amd64-netinst.iso
DEBIAN_NETINST_SHA256=a7ef94ac2fb9a7fec454552abd629b7cc9d5155c886165a45649f5ce6167e355
```

`packaging/iso/preseed/oplite-ci-init.service`를 만든다:

```ini
# 시험 전용(build-iso.sh --unattended): 첫 부팅에 한 번 클러스터를 만들고 그 출력(관리자 비밀번호,
# CA 지문)을 직렬 콘솔에 쓴다. ISO E2E가 직렬 로그에서 읽는다.
[Unit]
Description=opLite CI first-boot cluster init
After=opld.service
Requires=opld.service
ConditionPathExists=!/var/lib/oplite-ci-init.done

[Service]
Type=oneshot
# 실패를 진단할 수 있게 설치 때의 oplite-setup.sh 로그를 먼저 직렬 콘솔에 찍는다.
ExecStartPre=-/bin/cat /root/oplite-setup.log
# 제어 소켓은 opld의 기본 run_dir(/run/opl)에 있다(oplctl --socket 기본값과 같다).
# 완료 표시는 init이 성공했을 때만 남긴다. $$는 systemd가 $로 바꾼다.
ExecStart=/bin/sh -c 'for i in $$(seq 1 60); do [ -S /run/opl/opld.sock ] && break; sleep 2; done; [ -S /run/opl/opld.sock ] && oplctl cluster init --name ci && touch /var/lib/oplite-ci-init.done'
StandardOutput=tty
TTYPath=/dev/ttyS0

[Install]
WantedBy=multi-user.target
```

`packaging/iso/preseed/oplite-setup.sh`를 만든다:

```sh
#!/bin/sh
# 설치 ISO의 preseed/late_command가 설치 프로그램 환경(busybox sh)에서 돌린다.
#   sh /cdrom/preseed/oplite-setup.sh /target
# oplite 패키지(와 ifupdown2, isc-dhcp-client)를 설치하고, 기본 경로 인터페이스를 브리지 vmbr0으로 옮기고,
# /etc/opl/opld.toml과 콘솔 로그인 안내(/etc/issue.d/oplite.issue)를 쓴다. 로그: <target>/root/oplite-setup.log
# 단계마다 시작·끝(실패면 출력 끝 30줄)을 설치 환경의 콘솔(무인 설치에서는 직렬)에도 "oplite-setup: "로 쓴다.
# OPLITE_SETUP_LIB=1로 source하면 함수만 정의한다(packaging/tests/setup-test.sh).
set -eu

CDROM=${OPLITE_CDROM:-/cdrom}
# 단계 시작·끝을 쓰는 콘솔과 단계 출력을 잠시 두는 파일. 시험이 바꾼다.
CONSOLE=${OPLITE_CONSOLE:-/dev/console}
STEP_OUT="${TMPDIR:-/tmp}/oplite-setup.step"
STEP_NAME=""
# 설치 프로그램 환경의 루트. late_command(finish-install 07preseed)는 netcfg가 interfaces·hostname을
# /target으로 복사하는 단계(55netcfg-copy-config)보다 먼저 돌므로 원본은 이쪽에 있다. 시험이 바꾼다.
INSTALLER_ROOT=${OPLITE_INSTALLER_ROOT:-}

# default_nic <interfaces>: 기본 경로 인터페이스(inet dhcp 또는 gateway가 있는 inet 블록, lo 빼고 첫 번째).
# 그런 블록이 없으면 lo가 아닌 첫 inet static 블록(게이트웨이 없는 고정 주소).
default_nic() {
	awk '
	function end_stanza() {
		if (cand != "" && found == "" && ok) found = cand
		cand = ""
		ok = 0
	}
	/^[ \t]+[^ \t]/ && cand != "" { if ($1 == "gateway") ok = 1; next }
	{ end_stanza() }
	$1 == "iface" && $3 == "inet" && $2 != "lo" {
		cand = $2
		ok = ($4 == "dhcp")
		if ($4 == "static" && first_static == "") first_static = $2
	}
	END { end_stanza(); print (found != "" ? found : first_static) }
	' "$1"
}

# has_iface <interfaces>: lo가 아닌 iface 블록이 있으면 참
has_iface() {
	[ -f "$1" ] && awk '$1 == "iface" && $2 != "lo" { f = 1 } END { exit !f }' "$1"
}

# convert_interfaces <in> <out>: 기본 경로 인터페이스의 inet(과 inet6) 블록을 vmbr0으로 옮기고
# NIC는 auto <nic> / iface <nic> inet manual로 남긴다. 찾지 못하면 out에 in을 그대로 쓰고 2를 낸다.
convert_interfaces() {
	nic=$(default_nic "$1")
	if [ -z "$nic" ]; then
		cat "$1" >"$2"
		return 2
	fi
	awk -v nic="$nic" '
	# 빈 줄은 다음 줄을 낼 때 함께 낸다(옮긴 블록 뒤에 빈 줄이 겹치지 않게).
	function emit(line) {
		while (blank > 0) { print ""; blank-- }
		print line
	}
	function rest(from,   s, i) {
		s = ""
		for (i = from; i <= NF; i++) s = s " " $i
		return s
	}
	/^[ \t]+[^ \t]/ {
		if (mode == "inet") v4 = v4 "\n" $0
		else if (mode == "inet6") v6 = v6 "\n" $0
		else emit($0)
		next
	}
	{ mode = "" }
	/^[ \t]*$/ { blank++; next }
	($1 == "auto" || $1 == "allow-hotplug") && NF == 2 && $2 == nic { emit("auto " nic); next }
	$1 == "iface" && $2 == nic && $3 == "inet" {
		emit("iface " nic " inet manual")
		mode = "inet"
		v4 = "iface vmbr0 inet" rest(4)
		next
	}
	$1 == "iface" && $2 == nic && $3 == "inet6" { mode = "inet6"; v6 = "iface vmbr0 inet6" rest(4); next }
	{ emit($0) }
	END {
		print ""
		print "auto vmbr0"
		print v4
		print "\tbridge-ports " nic
		print "\tbridge-stp off"
		print "\tbridge-fd 0"
		print "\tbridge-vlan-aware yes"
		print "\tbridge-vids 2-4094"
		if (v6 != "") { print ""; print v6 }
	}
	' "$1" >"$2"
}

# primary_address <interfaces>: vmbr0 inet 블록의 정적 address(/prefix 뗌). DHCP면 빈 값.
primary_address() {
	awk '
	$1 == "iface" { in_br = ($2 == "vmbr0" && $3 == "inet"); next }
	/^[^ \t]/ { in_br = 0 }
	in_br && $1 == "address" { a = $2; sub(/\/.*/, "", a); print a; exit }
	' "$1"
}

# current_address <nic>: 설치 프로그램 환경에서 그 NIC가 지금 가진 IPv4 주소(DHCP로 받은 것)
current_address() {
	ip -4 -o addr show dev "$1" </dev/null | awk '{ a = $4; sub(/\/.*/, "", a); print a; exit }'
}

# valid_name <name>: opld 노드 이름 규칙(소문자·숫자·-, 1~63자, 앞뒤 - 아님)에 맞으면 참
valid_name() {
	case $1 in
	# 범위(a-z)는 로캘에 따라 대문자도 받으므로 글자를 다 적는다.
	"" | -* | *- | *[!abcdefghijklmnopqrstuvwxyz0123456789-]*) return 1 ;;
	esac
	[ "${#1}" -le 63 ]
}

# write_opld_toml <out> <name> <address> [dhcp]: 영어 ASCII만 쓴다(콘솔·편집기 어디서나 보이게).
# name이 규칙에 맞지 않거나 address가 비면 그 줄을 주석으로 둔다(opld는 설정 오류로 멈춘다).
write_opld_toml() {
	{
		echo '# Written by the opLite installer. Example with all settings: /usr/share/oplite/opld.toml.example'
		echo '# The address cannot change once this node is in a cluster (it is in the node certificate). Reserve the DHCP address or use a static one; before cluster init/join you may still edit it.'
		if valid_name "$2"; then
			echo "name = \"$2\""
		else
			echo "# Set name: 1-63 lowercase letters, digits and '-', not starting or ending with '-'."
			echo "# name = \"$2\""
		fi
		if [ -z "$3" ]; then
			echo "# Set address to this node's IP address (none was found during install)."
			echo '# address = ""'
		else
			if [ "${4:-}" = dhcp ]; then
				echo '# This address came from DHCP.'
			fi
			echo "address = \"$3\""
		fi
	} >"$1"
}

# write_issue <out>: 콘솔 로그인 화면 안내(영어 ASCII). \4{vmbr0}는 agetty가 vmbr0의 IPv4 주소로 바꾼다.
write_issue() {
	cat >"$1" <<'EOF'
opLite: https://\4{vmbr0}:8006/ (web UI and API, after the node is in a cluster)
  First node:  oplctl cluster init --name <cluster-name>
  Other nodes: on a logged-in client run `oplctl cluster join-token`,
               then here run `oplctl cluster join '<opl-join://... URL>'`

EOF
}

log() {
	echo "oplite-setup: $*"
}

# say <문구>: 로그와 설치 환경 콘솔에 쓴다(영어 ASCII). 콘솔에 쓰지 못해도 무시한다.
say() {
	echo "oplite-setup: $*"
	{ echo "oplite-setup: $*" >>"$CONSOLE"; } 2>/dev/null || true
}

# step <이름> <명령...>: 단계 시작·끝을 로그와 콘솔에 쓰고 명령 출력은 로그에 남긴다. 명령은 이 셸에서 돌아
# 변수(NIC 등)가 남는다. 실패하면 set -e로 끝나고 EXIT trap(on_exit)이 이름과 출력 끝을 콘솔에 쓴다.
step() {
	STEP_NAME=$1
	shift
	say "start $STEP_NAME"
	"$@" >"$STEP_OUT" 2>&1
	cat "$STEP_OUT"
	rm -f "$STEP_OUT"
	say "done $STEP_NAME"
	STEP_NAME=""
}

# on_exit <종료 코드>: 실패로 끝나면 실패한 단계와 그 출력(콘솔에는 끝 30줄)을 남긴다.
on_exit() {
	[ "$1" -ne 0 ] || return 0
	if [ -z "$STEP_NAME" ]; then
		say "FAILED (exit $1)"
		return 0
	fi
	cat "$STEP_OUT" 2>/dev/null || true
	say "FAILED $STEP_NAME (exit $1)"
	{ tail -n 30 "$STEP_OUT" >>"$CONSOLE"; } 2>/dev/null || true
}

# install_package <target>: CD의 oplite 패키지를 대상 시스템에 설치한다(의존성은 미러에서).
# apt-get은 질문 없이(noninteractive, conffile은 기본값) stdin 없이 돈다. in-target의 stdin은 설치 프로그램의
# debconf 통로라 dpkg·apt가 거기서 답을 기다리면 무인 설치가 멈춘다. 출력은 --pass-stdout으로 이 로그에 남긴다.
# 그동안 대상에 policy-rc.d(exit 101)를 두어 패키지(ifupdown2 등)가 서비스를 시작하지 못하게 한다.
install_package() {
	mkdir -p "$1/var/cache/oplite"
	if [ -d "$1/run/systemd/system" ]; then
		log "$1/run/systemd/system이 있다: oplite postinst가 opld를 시작하려 할 수 있다"
	fi
	for deb in "$CDROM"/oplite/oplite_*.deb; do
		[ -f "$deb" ] || {
			log "패키지가 없다: $CDROM/oplite/oplite_*.deb"
			return 1
		}
		cp "$deb" "$1/var/cache/oplite/"
		log "설치: $(basename "$deb")"
		mkdir -p "$1/usr/sbin"
		printf '#!/bin/sh\nexit 101\n' >"$1/usr/sbin/policy-rc.d"
		chmod 0755 "$1/usr/sbin/policy-rc.d"
		code=0
		# isc-dhcp-client: ifupdown2가 vmbr0의 DHCP에 dhclient를 쓴다.
		in-target --pass-stdout sh -c "DEBIAN_FRONTEND=noninteractive apt-get -y -q -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold install ifupdown2 isc-dhcp-client /var/cache/oplite/$(basename "$deb") </dev/null 2>&1" || code=$?
		rm -f "$1/usr/sbin/policy-rc.d"
		[ "$code" -eq 0 ] || return "$code"
	done
}

# setup_network <target>: 기본 경로 인터페이스를 vmbr0으로 옮긴다. 원본은 설치 환경의 interfaces이고,
# 거기에 lo가 아닌 iface가 없으면 대상의 것이다. 결과는 대상과 설치 환경 둘 다에 써서 뒤에
# netcfg가 설치 환경의 파일을 대상으로 복사해도 바뀐 파일이 남게 한다. NIC 이름은 NIC에 둔다(못 찾으면 빈 값).
setup_network() {
	target_ifaces="$1/etc/network/interfaces"
	installer_ifaces="$INSTALLER_ROOT/etc/network/interfaces"
	src=$installer_ifaces
	has_iface "$src" || src=$target_ifaces
	NIC=""
	if [ ! -f "$src" ]; then
		log "interfaces가 없다($installer_ifaces, $target_ifaces). 네트워크를 바꾸지 않았다"
		return 0
	fi
	log "원래 $src:"
	cat "$src"
	code=0
	convert_interfaces "$src" "$src.oplite" || code=$?
	if [ "$code" -ne 0 ]; then
		rm -f "$src.oplite"
		log "기본 경로 인터페이스를 찾지 못해 interfaces를 바꾸지 않았다"
		return 0
	fi
	NIC=$(default_nic "$src")
	mkdir -p "$1/etc/network" "$INSTALLER_ROOT/etc/network"
	cp "$src.oplite" "$target_ifaces"
	cp "$src.oplite" "$installer_ifaces"
	rm -f "$src.oplite"
	log "$NIC 을 vmbr0으로 옮겼다($target_ifaces, $installer_ifaces):"
	cat "$target_ifaces"
}

# node_name <target>: 호스트 이름(설치 환경, 없으면 대상의 /etc/hostname)을 소문자로
node_name() {
	f="$INSTALLER_ROOT/etc/hostname"
	[ -s "$f" ] || f="$1/etc/hostname"
	[ -s "$f" ] || return 0
	# 설치 환경의 busybox tr은 [:upper:] 같은 문자 집합을 몰라 글자 그대로 바꾼다("opl" → "owl").
	head -n1 "$f" | tr ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz
}

# write_config <target>: /etc/opl/opld.toml. 정적이면 그 주소, DHCP면 지금 받은 주소
write_config() {
	name=$(node_name "$1")
	valid_name "$name" || log "호스트 이름 '$name'이 노드 이름 규칙에 맞지 않아 /etc/opl/opld.toml의 name을 주석으로 둔다"
	address=""
	if [ -n "$NIC" ]; then
		address=$(primary_address "$1/etc/network/interfaces")
	fi
	dhcp=""
	if [ -z "$address" ] && [ -n "$NIC" ]; then
		address=$(current_address "$NIC" || true)
		dhcp=dhcp
	fi
	[ -n "$address" ] || log "주소를 찾지 못해 /etc/opl/opld.toml의 address를 주석으로 둔다. 고친 뒤 opld를 다시 시작한다"
	mkdir -p "$1/etc/opl"
	write_opld_toml "$1/etc/opl/opld.toml" "$name" "$address" "$dhcp"
	log "/etc/opl/opld.toml:"
	cat "$1/etc/opl/opld.toml"

	mkdir -p "$1/etc/issue.d"
	write_issue "$1/etc/issue.d/oplite.issue"
}

# enable_ci_init <target>: 시험 ISO(--unattended)면 첫 부팅에 cluster init을 하는 유닛을 켠다.
enable_ci_init() {
	if [ -f "$CDROM/preseed/oplite-ci-init.service" ]; then
		cp "$CDROM/preseed/oplite-ci-init.service" "$1/etc/systemd/system/"
		in-target systemctl enable oplite-ci-init.service
		log "oplite-ci-init.service를 켰다"
	fi
}

main() {
	target=$1
	mkdir -p "$target/root"
	exec >>"$target/root/oplite-setup.log" 2>&1
	trap 'on_exit $?' EXIT
	log "시작 $(date)"

	step install-package install_package "$target"
	# 네트워크: 기본 경로 인터페이스를 vmbr0으로
	step network setup_network "$target"
	step config write_config "$target"
	step ci-init enable_ci_init "$target"

	log "끝"
}

if [ "${OPLITE_SETUP_LIB:-}" = 1 ]; then
	return 0
fi
main "$@"
```

`packaging/iso/preseed/oplite.cfg`를 만든다:

```
# opLite 설치 preseed. 부트 항목 "Install opLite"가 auto=true priority=high로 부른다.
# 묻는 것: 네트워크(DHCP가 실패하면 수동), 호스트 이름, 도메인, root 비밀번호, 설치 디스크
# (디스크가 여럿이면 GRUB을 넣을 디스크도). 언어·키보드·시간대와 나머지는 여기서 정한다.

# 언어·키보드
d-i debian-installer/locale string en_US.UTF-8
d-i keyboard-configuration/xkb-keymap select us

# 네트워크
d-i netcfg/choose_interface select auto

# 시간대: UTC(설치 뒤 timedatectl set-timezone으로 바꾼다)
d-i time/zone string Etc/UTC
d-i clock-setup/utc boolean true

# 미러
d-i mirror/country string manual
d-i mirror/http/hostname string deb.debian.org
d-i mirror/http/directory string /debian
d-i mirror/http/proxy string
# APT 소스에 설치 CD를 넣지 않는다(다른 CD도 찾지 않는다). 설치 뒤 apt가 CD를 기다리지 않게 한다.
d-i apt-setup/cdrom/set-first boolean false
d-i apt-setup/cdrom/set-next boolean false
d-i apt-setup/cdrom/set-failed boolean false

# 계정: 일반 사용자는 만들지 않는다(root 비밀번호는 묻는다).
d-i passwd/make-user boolean false

# 파티션: 고른 디스크 전체에 LVM(atomic: / 와 swap)
d-i partman-auto/method string lvm
d-i partman-auto/choose_recipe select atomic
d-i partman-auto-lvm/guided_size string max
d-i partman-lvm/device_remove_lvm boolean true
d-i partman-md/device_remove_md boolean true
d-i partman-lvm/confirm boolean true
d-i partman-lvm/confirm_nooverwrite boolean true
d-i partman-partitioning/confirm_write_new_label boolean true
d-i partman/choose_partition select finish
d-i partman/confirm boolean true
d-i partman/confirm_nooverwrite boolean true

# 패키지
tasksel tasksel/first multiselect standard, ssh-server
popularity-contest popularity-contest/participate boolean false

# 부트로더: 넣을 디스크는 정하지 않는다(설치 프로그램이 정하거나 묻는다).
d-i grub-installer/only_debian boolean true

d-i finish-install/reboot_in_progress note

# oplite 패키지 설치, vmbr0 브리지, /etc/opl/opld.toml (로그: /root/oplite-setup.log)
d-i preseed/late_command string sh /cdrom/preseed/oplite-setup.sh /target
```

`packaging/iso/preseed/unattended.cfg`를 만든다:

```
d-i preseed/include string oplite.cfg
# 시험 전용(build-iso.sh --unattended): oplite.cfg가 묻는 값을 모두 정해 사람 없이 설치한다.
# 부트 인자: auto=true priority=critical preseed/file=/cdrom/preseed/unattended.cfg console=ttyS0,115200n8
# include한 파일은 이 파일 다음에 읽혀 같은 키면 그 값이 이긴다. 그래서 여기에는 oplite.cfg에 없는 키만 둔다.

# 호스트 이름(DHCP가 주는 이름보다 앞선다)
d-i netcfg/get_hostname string opl-ci
d-i netcfg/get_domain string
d-i netcfg/hostname string opl-ci

# root 비밀번호(시험 전용)
d-i passwd/root-password password opl-ci-root
d-i passwd/root-password-again password opl-ci-root

# 설치 디스크와 GRUB을 넣을 디스크(QEMU virtio)
d-i partman-auto/disk string /dev/vda
d-i grub-installer/bootdev string /dev/vda

# 시간(시간대 UTC는 oplite.cfg가 정한다)
d-i clock-setup/ntp boolean false

# 설치가 끝나면 전원을 끈다(QEMU -no-reboot와 함께 설치 단계를 끝낸다).
d-i debian-installer/exit/poweroff boolean true
```

`packaging/tests/fixtures/interfaces-dhcp.in`를 만든다:

```
# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

# The primary network interface
auto ens3
iface ens3 inet dhcp
```

`packaging/tests/fixtures/interfaces-dhcp.out`를 만든다:

```
# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

# The primary network interface
auto ens3
iface ens3 inet manual

auto vmbr0
iface vmbr0 inet dhcp
	bridge-ports ens3
	bridge-stp off
	bridge-fd 0
	bridge-vlan-aware yes
	bridge-vids 2-4094
```

`packaging/tests/fixtures/interfaces-hotplug.in`를 만든다:

```
# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

# The primary network interface
allow-hotplug enp1s0
iface enp1s0 inet dhcp

allow-hotplug enp2s0
iface enp2s0 inet dhcp
```

`packaging/tests/fixtures/interfaces-hotplug.out`를 만든다:

```
# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

# The primary network interface
auto enp1s0
iface enp1s0 inet manual

allow-hotplug enp2s0
iface enp2s0 inet dhcp

auto vmbr0
iface vmbr0 inet dhcp
	bridge-ports enp1s0
	bridge-stp off
	bridge-fd 0
	bridge-vlan-aware yes
	bridge-vids 2-4094
```

`packaging/tests/fixtures/interfaces-ipv6.in`를 만든다:

```
# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

# The primary network interface
allow-hotplug eno1
iface eno1 inet static
	address 10.0.0.11
	netmask 255.255.255.0
	gateway 10.0.0.1

# IPv6
iface eno1 inet6 static
	address 2001:db8::11/64
	gateway 2001:db8::1
```

`packaging/tests/fixtures/interfaces-ipv6.out`를 만든다:

```
# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

# The primary network interface
auto eno1
iface eno1 inet manual

# IPv6

auto vmbr0
iface vmbr0 inet static
	address 10.0.0.11
	netmask 255.255.255.0
	gateway 10.0.0.1
	bridge-ports eno1
	bridge-stp off
	bridge-fd 0
	bridge-vlan-aware yes
	bridge-vids 2-4094

iface vmbr0 inet6 static
	address 2001:db8::11/64
	gateway 2001:db8::1
```

`packaging/tests/fixtures/interfaces-none.in`를 만든다:

```
# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

auto eno1
iface eno1 inet manual
```

`packaging/tests/fixtures/interfaces-none.out`를 만든다:

```
# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

auto eno1
iface eno1 inet manual
```

`packaging/tests/fixtures/interfaces-static-nogw.in`를 만든다:

```
# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

# The primary network interface
auto enp2s0
iface enp2s0 inet static
	address 10.20.0.5/24
```

`packaging/tests/fixtures/interfaces-static-nogw.out`를 만든다:

```
# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

# The primary network interface
auto enp2s0
iface enp2s0 inet manual

auto vmbr0
iface vmbr0 inet static
	address 10.20.0.5/24
	bridge-ports enp2s0
	bridge-stp off
	bridge-fd 0
	bridge-vlan-aware yes
	bridge-vids 2-4094
```

`packaging/tests/fixtures/interfaces-static.in`를 만든다:

```
# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

# The primary network interface
auto enp1s0
iface enp1s0 inet static
	address 192.168.10.21/24
	gateway 192.168.10.1
	# dns-* options are implemented by the resolvconf package, if installed
	dns-nameservers 192.168.10.1 1.1.1.1
	dns-search example.com
```

`packaging/tests/fixtures/interfaces-static.out`를 만든다:

```
# This file describes the network interfaces available on your system
# and how to activate them. For more information, see interfaces(5).

source /etc/network/interfaces.d/*

# The loopback network interface
auto lo
iface lo inet loopback

# The primary network interface
auto enp1s0
iface enp1s0 inet manual

auto vmbr0
iface vmbr0 inet static
	address 192.168.10.21/24
	gateway 192.168.10.1
	# dns-* options are implemented by the resolvconf package, if installed
	dns-nameservers 192.168.10.1 1.1.1.1
	dns-search example.com
	bridge-ports enp1s0
	bridge-stp off
	bridge-fd 0
	bridge-vlan-aware yes
	bridge-vids 2-4094
```

`packaging/tests/fixtures/iso/netinst/boot/grub/grub.cfg`를 만든다:

```
if [ x$feature_default_font_path = xy ] ; then
   font=unicode
else
   font=$prefix/font.pf2
fi

if loadfont $font ; then
  set gfxmode=800x600
  set gfxpayload=keep
  insmod efi_gop
  insmod efi_uga
  insmod video_bochs
  insmod video_cirrus
  insmod gfxterm
  insmod png
  # Added as a workaround for #1110759
  echo "Loading bootloader..."
  terminal_output gfxterm
fi

if background_image /isolinux/splash.png; then
  set color_normal=light-gray/black
  set color_highlight=white/black
elif background_image /splash.png; then
  set color_normal=light-gray/black
  set color_highlight=white/black
else
  set menu_color_normal=cyan/blue
  set menu_color_highlight=white/blue
fi

insmod play
play 960 440 1 0 4 440 1
set theme=/boot/grub/theme/1
menuentry --hotkey=g 'Graphical install' {
    set background_color=black
    linux    /install.amd/vmlinuz vga=788 --- quiet 
    initrd   /install.amd/gtk/initrd.gz
}
menuentry --hotkey=i 'Install' {
    set background_color=black
    linux    /install.amd/vmlinuz vga=788 --- quiet 
    initrd   /install.amd/initrd.gz
}
submenu --hotkey=a 'Advanced options ...' {
    set menu_color_normal=cyan/blue
    set menu_color_highlight=white/blue
    set theme=/boot/grub/theme/1-1
    set gfxpayload=keep
    menuentry '... Graphical expert install' {
        set background_color=black
        linux    /install.amd/vmlinuz priority=low vga=788 --- 
        initrd   /install.amd/gtk/initrd.gz
    }
    menuentry '... Graphical rescue mode' {
        set background_color=black
        linux    /install.amd/vmlinuz vga=788 rescue/enable=true --- quiet  
        initrd   /install.amd/gtk/initrd.gz
    }
    menuentry '... Graphical automated install' {
        set background_color=black
        linux    /install.amd/vmlinuz auto=true priority=critical vga=788 --- quiet 
        initrd   /install.amd/gtk/initrd.gz
    }
    menuentry --hotkey=x '... Expert install' {
        set background_color=black
        linux    /install.amd/vmlinuz priority=low vga=788 --- 
        initrd   /install.amd/initrd.gz
    }
    menuentry --hotkey=r '... Rescue mode' {
        set background_color=black
        linux    /install.amd/vmlinuz vga=788 rescue/enable=true --- quiet 
        initrd   /install.amd/initrd.gz
    }
    menuentry --hotkey=a '... Automated install' {
        set background_color=black
        linux    /install.amd/vmlinuz auto=true priority=critical vga=788 --- quiet 
        initrd   /install.amd/initrd.gz
    }
    submenu --hotkey=s '... Speech-enabled advanced options ...' {
        set menu_color_normal=cyan/blue
        set menu_color_highlight=white/blue
        set theme=/boot/grub/theme/1-1-1
        set gfxpayload=keep
        menuentry --hotkey=x '... Expert speech install' {
            set background_color=black
            linux    /install.amd/vmlinuz priority=low vga=788 speakup.synth=soft --- 
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry --hotkey=r '... Rescue speech mode' {
            set background_color=black
            linux    /install.amd/vmlinuz vga=788 rescue/enable=true speakup.synth=soft --- quiet  
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry --hotkey=a '... Automated speech install' {
            set background_color=black
            linux    /install.amd/vmlinuz auto=true priority=critical vga=788 speakup.synth=soft --- quiet 
            initrd   /install.amd/gtk/initrd.gz
        }
    }
}
submenu --hotkey=d 'Accessible dark contrast installer menu ...' {
    set menu_color_normal=white/black
    set menu_color_highlight=yellow/black
    set color_normal=white/black
    set color_highlight=yellow/black
    background_image
    set theme=/boot/grub/theme/dark-1-2
    set gfxpayload=keep
    menuentry --hotkey=g '... Graphical install' {
        set background_color=black
        linux    /install.amd/vmlinuz vga=788 theme=dark --- quiet 
        initrd   /install.amd/gtk/initrd.gz
    }
    menuentry --hotkey=i '... Install' {
        set background_color=black
        linux    /install.amd/vmlinuz vga=788 theme=dark --- quiet 
        initrd   /install.amd/initrd.gz
    }
    submenu --hotkey=a '... Advanced options ...' {
        set menu_color_normal=white/black
        set menu_color_highlight=yellow/black
        set color_normal=white/black
        set color_highlight=yellow/black
        background_image
        set theme=/boot/grub/theme/dark-1-2-1
        set gfxpayload=keep
        menuentry '... Graphical expert install' {
            set background_color=black
            linux    /install.amd/vmlinuz priority=low vga=788 theme=dark --- 
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry '... Graphical rescue mode' {
            set background_color=black
            linux    /install.amd/vmlinuz vga=788 rescue/enable=true theme=dark --- quiet 
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry '... Graphical automated install' {
            set background_color=black
            linux    /install.amd/vmlinuz auto=true priority=critical vga=788 theme=dark --- quiet 
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry --hotkey=x '... Expert install' {
            set background_color=black
            linux    /install.amd/vmlinuz priority=low vga=788 theme=dark --- 
            initrd   /install.amd/initrd.gz
        }
        menuentry --hotkey=r '... Rescue mode' {
            set background_color=black
            linux    /install.amd/vmlinuz vga=788 rescue/enable=true theme=dark --- quiet 
            initrd   /install.amd/initrd.gz
        }
        menuentry --hotkey=a '... Automated install' {
            set background_color=black
            linux    /install.amd/vmlinuz auto=true priority=critical vga=788 theme=dark --- quiet 
            initrd   /install.amd/initrd.gz
        }
    }
}
menuentry --hotkey=s 'Install with speech synthesis' {
    set background_color=black
    linux    /install.amd/vmlinuz vga=788 speakup.synth=soft --- quiet 
    initrd   /install.amd/gtk/initrd.gz
}
```

`packaging/tests/fixtures/iso/netinst/isolinux/gtk.cfg`를 만든다:

```
default installgui
label installgui
	menu label ^Graphical install
	menu default
	kernel /install.amd/vmlinuz
	append vga=788 initrd=/install.amd/gtk/initrd.gz --- quiet 
```

`packaging/tests/fixtures/iso/netinst/isolinux/isolinux.cfg`를 만든다:

```
# D-I config version 2.0
# search path for the c32 support libraries (libcom32, libutil etc.)
path 
prompt 0
timeout 0
include menu.cfg
default vesamenu.c32
```

`packaging/tests/fixtures/iso/netinst/isolinux/txt.cfg`를 만든다:

```
label install
	menu label ^Install
	kernel /install.amd/vmlinuz
	append vga=788 initrd=/install.amd/initrd.gz --- quiet 
```

`packaging/tests/fixtures/iso/normal/boot/grub/grub.cfg`를 만든다:

```
if [ x$feature_default_font_path = xy ] ; then
   font=unicode
else
   font=$prefix/font.pf2
fi

if loadfont $font ; then
  set gfxmode=800x600
  set gfxpayload=keep
  insmod efi_gop
  insmod efi_uga
  insmod video_bochs
  insmod video_cirrus
  insmod gfxterm
  insmod png
  # Added as a workaround for #1110759
  echo "Loading bootloader..."
  terminal_output gfxterm
fi

if background_image /isolinux/splash.png; then
  set color_normal=light-gray/black
  set color_highlight=white/black
elif background_image /splash.png; then
  set color_normal=light-gray/black
  set color_highlight=white/black
else
  set menu_color_normal=cyan/blue
  set menu_color_highlight=white/blue
fi

insmod play
play 960 440 1 0 4 440 1
set theme=/boot/grub/theme/1
set default=0
menuentry --hotkey=o 'Install opLite' {
    set background_color=black
    linux    /install.amd/vmlinuz vga=788 auto=true priority=high preseed/file=/cdrom/preseed/oplite.cfg --- quiet
    initrd   /install.amd/initrd.gz
}
menuentry --hotkey=g 'Graphical install' {
    set background_color=black
    linux    /install.amd/vmlinuz vga=788 --- quiet 
    initrd   /install.amd/gtk/initrd.gz
}
menuentry --hotkey=i 'Install' {
    set background_color=black
    linux    /install.amd/vmlinuz vga=788 --- quiet 
    initrd   /install.amd/initrd.gz
}
submenu --hotkey=a 'Advanced options ...' {
    set menu_color_normal=cyan/blue
    set menu_color_highlight=white/blue
    set theme=/boot/grub/theme/1-1
    set gfxpayload=keep
    menuentry '... Graphical expert install' {
        set background_color=black
        linux    /install.amd/vmlinuz priority=low vga=788 --- 
        initrd   /install.amd/gtk/initrd.gz
    }
    menuentry '... Graphical rescue mode' {
        set background_color=black
        linux    /install.amd/vmlinuz vga=788 rescue/enable=true --- quiet  
        initrd   /install.amd/gtk/initrd.gz
    }
    menuentry '... Graphical automated install' {
        set background_color=black
        linux    /install.amd/vmlinuz auto=true priority=critical vga=788 --- quiet 
        initrd   /install.amd/gtk/initrd.gz
    }
    menuentry --hotkey=x '... Expert install' {
        set background_color=black
        linux    /install.amd/vmlinuz priority=low vga=788 --- 
        initrd   /install.amd/initrd.gz
    }
    menuentry --hotkey=r '... Rescue mode' {
        set background_color=black
        linux    /install.amd/vmlinuz vga=788 rescue/enable=true --- quiet 
        initrd   /install.amd/initrd.gz
    }
    menuentry --hotkey=a '... Automated install' {
        set background_color=black
        linux    /install.amd/vmlinuz auto=true priority=critical vga=788 --- quiet 
        initrd   /install.amd/initrd.gz
    }
    submenu --hotkey=s '... Speech-enabled advanced options ...' {
        set menu_color_normal=cyan/blue
        set menu_color_highlight=white/blue
        set theme=/boot/grub/theme/1-1-1
        set gfxpayload=keep
        menuentry --hotkey=x '... Expert speech install' {
            set background_color=black
            linux    /install.amd/vmlinuz priority=low vga=788 speakup.synth=soft --- 
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry --hotkey=r '... Rescue speech mode' {
            set background_color=black
            linux    /install.amd/vmlinuz vga=788 rescue/enable=true speakup.synth=soft --- quiet  
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry --hotkey=a '... Automated speech install' {
            set background_color=black
            linux    /install.amd/vmlinuz auto=true priority=critical vga=788 speakup.synth=soft --- quiet 
            initrd   /install.amd/gtk/initrd.gz
        }
    }
}
submenu --hotkey=d 'Accessible dark contrast installer menu ...' {
    set menu_color_normal=white/black
    set menu_color_highlight=yellow/black
    set color_normal=white/black
    set color_highlight=yellow/black
    background_image
    set theme=/boot/grub/theme/dark-1-2
    set gfxpayload=keep
    menuentry --hotkey=g '... Graphical install' {
        set background_color=black
        linux    /install.amd/vmlinuz vga=788 theme=dark --- quiet 
        initrd   /install.amd/gtk/initrd.gz
    }
    menuentry --hotkey=i '... Install' {
        set background_color=black
        linux    /install.amd/vmlinuz vga=788 theme=dark --- quiet 
        initrd   /install.amd/initrd.gz
    }
    submenu --hotkey=a '... Advanced options ...' {
        set menu_color_normal=white/black
        set menu_color_highlight=yellow/black
        set color_normal=white/black
        set color_highlight=yellow/black
        background_image
        set theme=/boot/grub/theme/dark-1-2-1
        set gfxpayload=keep
        menuentry '... Graphical expert install' {
            set background_color=black
            linux    /install.amd/vmlinuz priority=low vga=788 theme=dark --- 
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry '... Graphical rescue mode' {
            set background_color=black
            linux    /install.amd/vmlinuz vga=788 rescue/enable=true theme=dark --- quiet 
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry '... Graphical automated install' {
            set background_color=black
            linux    /install.amd/vmlinuz auto=true priority=critical vga=788 theme=dark --- quiet 
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry --hotkey=x '... Expert install' {
            set background_color=black
            linux    /install.amd/vmlinuz priority=low vga=788 theme=dark --- 
            initrd   /install.amd/initrd.gz
        }
        menuentry --hotkey=r '... Rescue mode' {
            set background_color=black
            linux    /install.amd/vmlinuz vga=788 rescue/enable=true theme=dark --- quiet 
            initrd   /install.amd/initrd.gz
        }
        menuentry --hotkey=a '... Automated install' {
            set background_color=black
            linux    /install.amd/vmlinuz auto=true priority=critical vga=788 theme=dark --- quiet 
            initrd   /install.amd/initrd.gz
        }
    }
}
menuentry --hotkey=s 'Install with speech synthesis' {
    set background_color=black
    linux    /install.amd/vmlinuz vga=788 speakup.synth=soft --- quiet 
    initrd   /install.amd/gtk/initrd.gz
}
```

`packaging/tests/fixtures/iso/normal/isolinux/gtk.cfg`를 만든다:

```
default installgui
label installgui
	menu label ^Graphical install
	kernel /install.amd/vmlinuz
	append vga=788 initrd=/install.amd/gtk/initrd.gz --- quiet 
```

`packaging/tests/fixtures/iso/normal/isolinux/isolinux.cfg`를 만든다:

```
# D-I config version 2.0
# search path for the c32 support libraries (libcom32, libutil etc.)
path 
prompt 0
timeout 0
include menu.cfg
default vesamenu.c32
```

`packaging/tests/fixtures/iso/normal/isolinux/txt.cfg`를 만든다:

```
label oplite
	menu label ^Install opLite
	menu default
	kernel /install.amd/vmlinuz
	append vga=788 initrd=/install.amd/initrd.gz auto=true priority=high preseed/file=/cdrom/preseed/oplite.cfg --- quiet
label install
	menu label ^Install
	kernel /install.amd/vmlinuz
	append vga=788 initrd=/install.amd/initrd.gz --- quiet 
```

`packaging/tests/fixtures/iso/unattended/boot/grub/grub.cfg`를 만든다:

```
if [ x$feature_default_font_path = xy ] ; then
   font=unicode
else
   font=$prefix/font.pf2
fi

if loadfont $font ; then
  set gfxmode=800x600
  set gfxpayload=keep
  insmod efi_gop
  insmod efi_uga
  insmod video_bochs
  insmod video_cirrus
  insmod gfxterm
  insmod png
  # Added as a workaround for #1110759
  echo "Loading bootloader..."
  terminal_output gfxterm
fi

if background_image /isolinux/splash.png; then
  set color_normal=light-gray/black
  set color_highlight=white/black
elif background_image /splash.png; then
  set color_normal=light-gray/black
  set color_highlight=white/black
else
  set menu_color_normal=cyan/blue
  set menu_color_highlight=white/blue
fi

insmod play
play 960 440 1 0 4 440 1
set theme=/boot/grub/theme/1
set default=0
set timeout=1
menuentry --hotkey=o 'Install opLite' {
    set background_color=black
    linux    /install.amd/vmlinuz vga=788 auto=true priority=critical preseed/file=/cdrom/preseed/unattended.cfg console=ttyS0,115200n8 --- quiet
    initrd   /install.amd/initrd.gz
}
menuentry --hotkey=g 'Graphical install' {
    set background_color=black
    linux    /install.amd/vmlinuz vga=788 --- quiet 
    initrd   /install.amd/gtk/initrd.gz
}
menuentry --hotkey=i 'Install' {
    set background_color=black
    linux    /install.amd/vmlinuz vga=788 --- quiet 
    initrd   /install.amd/initrd.gz
}
submenu --hotkey=a 'Advanced options ...' {
    set menu_color_normal=cyan/blue
    set menu_color_highlight=white/blue
    set theme=/boot/grub/theme/1-1
    set gfxpayload=keep
    menuentry '... Graphical expert install' {
        set background_color=black
        linux    /install.amd/vmlinuz priority=low vga=788 --- 
        initrd   /install.amd/gtk/initrd.gz
    }
    menuentry '... Graphical rescue mode' {
        set background_color=black
        linux    /install.amd/vmlinuz vga=788 rescue/enable=true --- quiet  
        initrd   /install.amd/gtk/initrd.gz
    }
    menuentry '... Graphical automated install' {
        set background_color=black
        linux    /install.amd/vmlinuz auto=true priority=critical vga=788 --- quiet 
        initrd   /install.amd/gtk/initrd.gz
    }
    menuentry --hotkey=x '... Expert install' {
        set background_color=black
        linux    /install.amd/vmlinuz priority=low vga=788 --- 
        initrd   /install.amd/initrd.gz
    }
    menuentry --hotkey=r '... Rescue mode' {
        set background_color=black
        linux    /install.amd/vmlinuz vga=788 rescue/enable=true --- quiet 
        initrd   /install.amd/initrd.gz
    }
    menuentry --hotkey=a '... Automated install' {
        set background_color=black
        linux    /install.amd/vmlinuz auto=true priority=critical vga=788 --- quiet 
        initrd   /install.amd/initrd.gz
    }
    submenu --hotkey=s '... Speech-enabled advanced options ...' {
        set menu_color_normal=cyan/blue
        set menu_color_highlight=white/blue
        set theme=/boot/grub/theme/1-1-1
        set gfxpayload=keep
        menuentry --hotkey=x '... Expert speech install' {
            set background_color=black
            linux    /install.amd/vmlinuz priority=low vga=788 speakup.synth=soft --- 
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry --hotkey=r '... Rescue speech mode' {
            set background_color=black
            linux    /install.amd/vmlinuz vga=788 rescue/enable=true speakup.synth=soft --- quiet  
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry --hotkey=a '... Automated speech install' {
            set background_color=black
            linux    /install.amd/vmlinuz auto=true priority=critical vga=788 speakup.synth=soft --- quiet 
            initrd   /install.amd/gtk/initrd.gz
        }
    }
}
submenu --hotkey=d 'Accessible dark contrast installer menu ...' {
    set menu_color_normal=white/black
    set menu_color_highlight=yellow/black
    set color_normal=white/black
    set color_highlight=yellow/black
    background_image
    set theme=/boot/grub/theme/dark-1-2
    set gfxpayload=keep
    menuentry --hotkey=g '... Graphical install' {
        set background_color=black
        linux    /install.amd/vmlinuz vga=788 theme=dark --- quiet 
        initrd   /install.amd/gtk/initrd.gz
    }
    menuentry --hotkey=i '... Install' {
        set background_color=black
        linux    /install.amd/vmlinuz vga=788 theme=dark --- quiet 
        initrd   /install.amd/initrd.gz
    }
    submenu --hotkey=a '... Advanced options ...' {
        set menu_color_normal=white/black
        set menu_color_highlight=yellow/black
        set color_normal=white/black
        set color_highlight=yellow/black
        background_image
        set theme=/boot/grub/theme/dark-1-2-1
        set gfxpayload=keep
        menuentry '... Graphical expert install' {
            set background_color=black
            linux    /install.amd/vmlinuz priority=low vga=788 theme=dark --- 
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry '... Graphical rescue mode' {
            set background_color=black
            linux    /install.amd/vmlinuz vga=788 rescue/enable=true theme=dark --- quiet 
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry '... Graphical automated install' {
            set background_color=black
            linux    /install.amd/vmlinuz auto=true priority=critical vga=788 theme=dark --- quiet 
            initrd   /install.amd/gtk/initrd.gz
        }
        menuentry --hotkey=x '... Expert install' {
            set background_color=black
            linux    /install.amd/vmlinuz priority=low vga=788 theme=dark --- 
            initrd   /install.amd/initrd.gz
        }
        menuentry --hotkey=r '... Rescue mode' {
            set background_color=black
            linux    /install.amd/vmlinuz vga=788 rescue/enable=true theme=dark --- quiet 
            initrd   /install.amd/initrd.gz
        }
        menuentry --hotkey=a '... Automated install' {
            set background_color=black
            linux    /install.amd/vmlinuz auto=true priority=critical vga=788 theme=dark --- quiet 
            initrd   /install.amd/initrd.gz
        }
    }
}
menuentry --hotkey=s 'Install with speech synthesis' {
    set background_color=black
    linux    /install.amd/vmlinuz vga=788 speakup.synth=soft --- quiet 
    initrd   /install.amd/gtk/initrd.gz
}
```

`packaging/tests/fixtures/iso/unattended/isolinux/gtk.cfg`를 만든다:

```
default installgui
label installgui
	menu label ^Graphical install
	kernel /install.amd/vmlinuz
	append vga=788 initrd=/install.amd/gtk/initrd.gz --- quiet 
```

`packaging/tests/fixtures/iso/unattended/isolinux/isolinux.cfg`를 만든다:

```
serial 0 115200
default oplite
prompt 0
timeout 1
label oplite
	kernel /install.amd/vmlinuz
	append vga=788 initrd=/install.amd/initrd.gz auto=true priority=critical preseed/file=/cdrom/preseed/unattended.cfg console=ttyS0,115200n8 --- quiet
```

`packaging/tests/fixtures/iso/unattended/isolinux/txt.cfg`를 만든다:

```
label oplite
	menu label ^Install opLite
	menu default
	kernel /install.amd/vmlinuz
	append vga=788 initrd=/install.amd/initrd.gz auto=true priority=critical preseed/file=/cdrom/preseed/unattended.cfg console=ttyS0,115200n8 --- quiet
label install
	menu label ^Install
	kernel /install.amd/vmlinuz
	append vga=788 initrd=/install.amd/initrd.gz --- quiet 
```

`packaging/tests/fixtures/opld-badname.toml`를 만든다:

```toml
# Written by the opLite installer. Example with all settings: /usr/share/oplite/opld.toml.example
# The address cannot change once this node is in a cluster (it is in the node certificate). Reserve the DHCP address or use a static one; before cluster init/join you may still edit it.
# Set name: 1-63 lowercase letters, digits and '-', not starting or ending with '-'.
# name = "my_host"
address = "192.168.10.21"
```

`packaging/tests/fixtures/opld-dhcp.toml`를 만든다:

```toml
# Written by the opLite installer. Example with all settings: /usr/share/oplite/opld.toml.example
# The address cannot change once this node is in a cluster (it is in the node certificate). Reserve the DHCP address or use a static one; before cluster init/join you may still edit it.
name = "opl-ci"
# This address came from DHCP.
address = "10.0.2.15"
```

`packaging/tests/fixtures/opld-noaddr.toml`를 만든다:

```toml
# Written by the opLite installer. Example with all settings: /usr/share/oplite/opld.toml.example
# The address cannot change once this node is in a cluster (it is in the node certificate). Reserve the DHCP address or use a static one; before cluster init/join you may still edit it.
name = "node1"
# Set address to this node's IP address (none was found during install).
# address = ""
```

`packaging/tests/fixtures/opld-static.toml`를 만든다:

```toml
# Written by the opLite installer. Example with all settings: /usr/share/oplite/opld.toml.example
# The address cannot change once this node is in a cluster (it is in the node certificate). Reserve the DHCP address or use a static one; before cluster init/join you may still edit it.
name = "node1"
address = "192.168.10.21"
```

`packaging/tests/fixtures/oplite.issue`를 만든다:

```
opLite: https://\4{vmbr0}:8006/ (web UI and API, after the node is in a cluster)
  First node:  oplctl cluster init --name <cluster-name>
  Other nodes: on a logged-in client run `oplctl cluster join-token`,
               then here run `oplctl cluster join '<opl-join://... URL>'`

```

`packaging/tests/iso-test.sh`를 만든다:

```sh
#!/bin/bash
# build-iso.sh의 인자 검사, netinst 해시 확인, debian.lock 모양, 부트 메뉴 패치(13.7.0 ISO의 실제 파일 견본),
# md5sum.txt·sha256sum.txt 갱신, 가짜 xorriso로 돌린 main(꺼내기·다시 묶기 인자, 실패 때 정리)을 본다.
# xorriso가 없어도 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/iso-test.sh
set -eu

ISO_DIR="$(cd "$(dirname "$0")/../iso" && pwd)"
ISO_FIXTURES="$(cd "$(dirname "$0")/fixtures/iso" && pwd)"

# 함수만 정의한다(ISO는 만들지 않는다). trap보다 먼저 해서 source 실패가 종료 코드로 남게 한다.
# shellcheck source=packaging/iso/build-iso.sh
OPLITE_ISO_LIB=1 . "$ISO_DIR/build-iso.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

FAILED=0

ok() { echo "ok   $1"; }
fail() {
	echo "FAIL $1"
	FAILED=1
}

printf 'netinst' >"$WORK/netinst.iso"
printf 'deb' >"$WORK/oplite_0.1.0_amd64.deb"
N="$WORK/netinst.iso"
D="$WORK/oplite_0.1.0_amd64.deb"
O="$WORK/out.iso"

# args_exit <이름> <기대 종료 코드> <인자...>: parse_args를 서브셸에서 돌려 종료 코드를 본다.
args_exit() {
	name=$1 want=$2
	shift 2
	code=0
	(parse_args "$@") 2>/dev/null || code=$?
	if [ "$code" -eq "$want" ]; then ok "인자 $name"; else fail "인자 $name: 종료 코드 $code (기대 $want)"; fi
}

args_exit "없음" 2
args_exit "--deb 빠짐" 2 --netinst "$N" --out "$O"
args_exit "--out 빠짐" 2 --netinst "$N" --deb "$D"
args_exit "값 없는 --netinst" 2 --deb "$D" --out "$O" --netinst
args_exit "모르는 인자" 2 --netinst "$N" --deb "$D" --out "$O" --skip-hash
args_exit "없는 netinst 파일" 1 --netinst "$WORK/none.iso" --deb "$D" --out "$O"
args_exit "없는 deb 파일" 1 --netinst "$N" --deb "$WORK/oplite_9.9.9_amd64.deb" --out "$O"
# --deb는 build-deb.sh가 만든 이름(oplite_<version>_amd64.deb)이어야 한다.
printf 'deb' >"$WORK/other.deb"
printf 'deb' >"$WORK/oplite_0.1.0_arm64.deb"
args_exit "deb 이름이 oplite_가 아님" 2 --netinst "$N" --deb "$WORK/other.deb" --out "$O"
args_exit "deb 이름이 amd64가 아님" 2 --netinst "$N" --deb "$WORK/oplite_0.1.0_arm64.deb" --out "$O"
args_exit "맞음" 0 --netinst "$N" --deb "$D" --out "$O"
args_exit "맞음(--unattended)" 0 --unattended --netinst "$N" --deb "$D" --out "$O"

parse_args --netinst "$N" --deb "$D" --out "$O" --unattended
if [ "$NETINST" = "$N" ] && [ "$DEB" = "$D" ] && [ "$OUT" = "$O" ] && [ "$UNATTENDED" = 1 ]; then
	ok "인자 값"
else
	fail "인자 값: $NETINST $DEB $OUT $UNATTENDED"
fi
parse_args --netinst "$N" --deb "$D" --out "$O"
if [ "$UNATTENDED" = 0 ]; then ok "인자 기본 unattended=0"; else fail "인자 기본 unattended=$UNATTENDED"; fi

# 해시: lock과 같으면 통과, 다르거나 lock에 해시가 없으면 실패
hash=$(sha256_file "$N")
printf 'DEBIAN_NETINST_URL=https://example.invalid/x.iso\nDEBIAN_NETINST_SHA256=%s\n' "$hash" >"$WORK/good.lock"
echo 'DEBIAN_NETINST_SHA256=0000000000000000000000000000000000000000000000000000000000000000' >"$WORK/bad.lock"
printf 'DEBIAN_NETINST_URL=https://example.invalid/x.iso\n' >"$WORK/empty.lock"
hash_exit() {
	name=$1 want=$2 lock=$3
	code=0
	(verify_netinst "$N" "$lock") >/dev/null 2>&1 || code=$?
	if [ "$code" -eq "$want" ]; then ok "해시 $name"; else fail "해시 $name: 종료 코드 $code (기대 $want)"; fi
}
hash_exit "같음" 0 "$WORK/good.lock"
hash_exit "다름" 1 "$WORK/bad.lock"
hash_exit "lock에 해시 없음" 1 "$WORK/empty.lock"

# 저장소의 debian.lock: 값이 모두 있고, URL이 그 판의 amd64 netinst를 가리킨다.
LOCK="$ISO_DIR/debian.lock"
release=$(lock_value "$LOCK" DEBIAN_RELEASE)
url=$(lock_value "$LOCK" DEBIAN_NETINST_URL)
sha=$(lock_value "$LOCK" DEBIAN_NETINST_SHA256)
codename=$(lock_value "$LOCK" DEBIAN_CODENAME)
if [ "$codename" = trixie ] &&
	[[ $release =~ ^13\.[0-9]+\.[0-9]+$ ]] &&
	[[ $url == https://cdimage.debian.org/cdimage/*/$release/amd64/iso-cd/debian-$release-amd64-netinst.iso ]] &&
	[[ $sha =~ ^[0-9a-f]{64}$ ]]; then
	ok "debian.lock"
else
	fail "debian.lock: release=$release codename=$codename url=$url sha=$sha"
fi

# 부트 메뉴 패치: 13.7.0 netinst의 isolinux·grub 설정에 패치한 결과가 기대 파일과 같다.
for mode in normal unattended; do
	cp -R "$ISO_FIXTURES/netinst" "$WORK/tree-$mode"
	if [ "$mode" = unattended ]; then a=$UNATTENDED_ARGS u=1; else a=$PRESEED_ARGS u=0; fi
	if (
		patch_grub "$WORK/tree-$mode" "$a" "$u"
		patch_isolinux "$WORK/tree-$mode" "$a" "$u"
	) && diff -ru "$ISO_FIXTURES/$mode" "$WORK/tree-$mode"; then
		ok "메뉴 패치 $mode"
	else
		fail "메뉴 패치 $mode"
	fi
	code=0
	(check_patched "$WORK/tree-$mode" "$u") 2>/dev/null || code=$?
	if [ "$code" -eq 0 ]; then ok "패치 확인 $mode"; else fail "패치 확인 $mode: 종료 코드 $code"; fi
done
# 패치하지 않은 메뉴, timeout을 바꾸지 않은 메뉴는 확인에서 걸린다.
code=0
(check_patched "$ISO_FIXTURES/netinst" 0) 2>/dev/null || code=$?
if [ "$code" -eq 1 ]; then ok "패치 확인: 패치 안 됨"; else fail "패치 확인: 패치 안 됨, 종료 코드 $code"; fi
code=0
(check_patched "$ISO_FIXTURES/normal" 1) 2>/dev/null || code=$?
if [ "$code" -eq 1 ]; then ok "패치 확인: unattended isolinux.cfg·timeout 없음"; else
	fail "패치 확인: unattended isolinux.cfg·timeout 없음, 종료 코드 $code"
fi

# md5sum.txt: 있던 경로는 합을 바꾸고 없던 경로는 끝에 더한다. 다른 줄은 그대로다.
mkdir -p "$WORK/md5/boot/grub" "$WORK/md5/preseed"
echo new-grub >"$WORK/md5/boot/grub/grub.cfg"
echo seed >"$WORK/md5/preseed/oplite.cfg"
printf '%s  ./README.txt\n%s  ./boot/grub/grub.cfg\n%s  ./pool/a.deb\n' \
	11111111111111111111111111111111 22222222222222222222222222222222 33333333333333333333333333333333 \
	>"$WORK/md5/md5sum.txt"
md5_update "$WORK/md5" boot/grub/grub.cfg preseed/oplite.cfg
printf '%s  ./README.txt\n%s  ./boot/grub/grub.cfg\n%s  ./pool/a.deb\n%s  ./preseed/oplite.cfg\n' \
	11111111111111111111111111111111 "$(md5_file "$WORK/md5/boot/grub/grub.cfg")" \
	33333333333333333333333333333333 "$(md5_file "$WORK/md5/preseed/oplite.cfg")" >"$WORK/md5-want.txt"
if diff -u "$WORK/md5-want.txt" "$WORK/md5/md5sum.txt"; then ok "md5sum.txt 갱신"; else fail "md5sum.txt 갱신"; fi

# sha256sum.txt도 같은 방식이다.
printf '%s  ./README.txt\n%s  ./boot/grub/grub.cfg\n' "$(printf '1%.0s' $(seq 64))" "$(printf '2%.0s' $(seq 64))" \
	>"$WORK/md5/sha256sum.txt"
sha256_update "$WORK/md5" boot/grub/grub.cfg preseed/oplite.cfg
printf '%s  ./README.txt\n%s  ./boot/grub/grub.cfg\n%s  ./preseed/oplite.cfg\n' "$(printf '1%.0s' $(seq 64))" \
	"$(sha256_file "$WORK/md5/boot/grub/grub.cfg")" "$(sha256_file "$WORK/md5/preseed/oplite.cfg")" >"$WORK/sha-want.txt"
if diff -u "$WORK/sha-want.txt" "$WORK/md5/sha256sum.txt"; then ok "sha256sum.txt 갱신"; else fail "sha256sum.txt 갱신"; fi

# main을 가짜 xorriso·sha256sum으로 돌린다. 가짜 sha256sum은 netinst에는 저장소 debian.lock의 해시를 낸다.
# 가짜 xorriso는 인자를 기록하고, -extract는 견본 ISO 루트에서 복사하고(없는 파일이면 xorriso처럼 실패),
# -map은 얹는 파일·디렉터리를 $FAKE_ISO_OUT에 복사하고, -outdev는 파일을 만든다.
mkdir -p "$WORK/fakebin" "$WORK/isoroot"
cp -R "$ISO_FIXTURES/netinst/." "$WORK/isoroot/"
printf '%s  ./boot/grub/grub.cfg\n' 22222222222222222222222222222222 >"$WORK/isoroot/md5sum.txt"
cat >"$WORK/fakebin/sha256sum" <<EOF
#!/bin/sh
if [ "\$1" = "$N" ]; then
	echo "$(lock_value "$ISO_DIR/debian.lock" DEBIAN_NETINST_SHA256)  \$1"
else
	exec shasum -a 256 "\$1"
fi
EOF
cat >"$WORK/fakebin/xorriso" <<'EOF'
#!/bin/bash
echo "$*" >>"$FAKE_XORRISO_LOG"
if [ -n "${FAKE_XORRISO_FAIL:-}" ]; then
	echo "xorriso : FAILURE : fake failure" >&2
	exit 5
fi
while [ $# -gt 0 ]; do
	case $1 in
	-extract)
		if [ ! -e "$FAKE_ISO_ROOT$2" ]; then
			echo "xorriso : FAILURE : Cannot determine attributes of (ISO) source file '$2'" >&2
			exit 5
		fi
		cp "$FAKE_ISO_ROOT$2" "$3"
		chmod a-w "$3"
		shift 3
		;;
	-map)
		mkdir -p "$(dirname "$FAKE_ISO_OUT$3")"
		cp -R "$2" "$FAKE_ISO_OUT$3"
		shift 3
		;;
	-outdev)
		echo iso >"$2"
		shift 2
		;;
	*) shift ;;
	esac
done
EOF
chmod 0755 "$WORK/fakebin/sha256sum" "$WORK/fakebin/xorriso"

# run_main <이름> [xorriso 실패 1]: main을 따로 bash로 돌려 종료 코드·stderr·남은 임시 파일을 본다.
run_main() {
	rm -rf "$WORK/tmp" "$WORK/xorriso.log" "$O" "$WORK/isoout"
	mkdir -p "$WORK/tmp"
	code=0
	PATH="$WORK/fakebin:$PATH" TMPDIR="$WORK/tmp" FAKE_XORRISO_LOG="$WORK/xorriso.log" \
		FAKE_ISO_ROOT="$WORK/isoroot" FAKE_ISO_OUT="$WORK/isoout" FAKE_XORRISO_FAIL="${2:-}" \
		bash "$ISO_DIR/build-iso.sh" --netinst "$N" --deb "$D" --out "$O" --unattended \
		>"$WORK/main.out" 2>"$WORK/main.err" || code=$?
	if [ -n "$(ls -A "$WORK/tmp")" ]; then fail "$1: 작업 디렉터리가 남음"; else ok "$1: 작업 디렉터리 지움"; fi
}

run_main "main"
if [ "$code" -eq 0 ] && [ -f "$O" ] && [ "$(tail -n1 "$WORK/main.out")" = "$O" ]; then ok "main 성공"; else
	fail "main: 종료 코드 $code"
	cat "$WORK/main.err"
fi
extract_line=$(sed -n 1p "$WORK/xorriso.log")
repack_line=$(grep -e '-outdev' "$WORK/xorriso.log")
for f in isolinux/txt.cfg isolinux/gtk.cfg isolinux/isolinux.cfg boot/grub/grub.cfg md5sum.txt; do
	if [[ $extract_line == *"-extract /$f "* ]] && [[ $repack_line == *"/iso/$f /$f"* ]]; then ok "main 꺼내기·얹기 $f"; else
		fail "main 꺼내기·얹기 $f"
	fi
done
if [[ $extract_line == "-osirrox on -indev $N "* ]] &&
	[[ $repack_line == "-indev $N -outdev $O "* ]] &&
	[[ $repack_line == *"/iso/oplite /oplite -map "*"/iso/preseed /preseed "* ]] &&
	[[ $repack_line == *" -boot_image any replay "* ]] &&
	[[ $repack_line != *"/iso / "* ]]; then
	ok "main 다시 묶기 인자"
else
	fail "main 다시 묶기 인자: $repack_line"
fi
# netinst에 sha256sum.txt가 없으면 꺼내 보기만 하고 얹지 않는다.
if grep -q -e '-extract /sha256sum.txt ' "$WORK/xorriso.log" && [[ $repack_line != *sha256sum.txt* ]]; then
	ok "main sha256sum.txt 없음"
else
	fail "main sha256sum.txt 없음: $(cat "$WORK/xorriso.log")"
fi
# 얹은 md5sum.txt: grub.cfg와 새 파일의 합
# sums_want <목록> <합 함수>: 견본 목록에 얹은 grub.cfg·새 파일의 합을 반영한 기대 내용
sums_want() {
	local f
	sed '/boot\/grub\/grub.cfg$/d' "$WORK/isoroot/$1"
	echo "$("$2" "$WORK/isoout/boot/grub/grub.cfg")  ./boot/grub/grub.cfg"
	for f in oplite/oplite_0.1.0_amd64.deb preseed/oplite.cfg preseed/oplite-setup.sh preseed/unattended.cfg \
		preseed/oplite-ci-init.service; do
		echo "$("$2" "$WORK/isoout/$f")  ./$f"
	done
}
if diff -u <(sums_want md5sum.txt md5_file | sort) <(sort "$WORK/isoout/md5sum.txt"); then ok "main md5sum.txt"; else
	fail "main md5sum.txt"
fi

# netinst에 sha256sum.txt가 있으면 md5sum.txt처럼 고쳐 얹는다.
printf '%s  ./README.txt\n%s  ./boot/grub/grub.cfg\n' "$(printf '1%.0s' $(seq 64))" "$(printf '2%.0s' $(seq 64))" \
	>"$WORK/isoroot/sha256sum.txt"
run_main "main(sha256sum.txt)"
repack_line=$(grep -e '-outdev' "$WORK/xorriso.log" || true)
if [ "$code" -eq 0 ] && [[ $repack_line == *"/iso/sha256sum.txt /sha256sum.txt"* ]] &&
	diff -u <(sums_want sha256sum.txt sha256_file | sort) <(sort "$WORK/isoout/sha256sum.txt"); then
	ok "main sha256sum.txt"
else
	fail "main sha256sum.txt: 종료 코드 $code, $repack_line"
	cat "$WORK/main.err"
fi
rm -f "$WORK/isoroot/sha256sum.txt"

# xorriso가 실패하면 그 종료 코드로 끝나고, EXIT trap이 set -u에 걸리지 않는다.
run_main "main(xorriso 실패)" 1
if [ "$code" -eq 5 ] && ! grep -q 'unbound variable' "$WORK/main.err"; then ok "main xorriso 실패"; else
	fail "main xorriso 실패: 종료 코드 $code"
	cat "$WORK/main.err"
fi

if [ "$FAILED" -ne 0 ]; then
	echo "iso-test: 실패"
	exit 1
fi
echo "iso-test: 모두 통과"
```

`packaging/tests/setup-test.sh`를 만든다:

```sh
#!/bin/bash
# 설치 ISO의 oplite-setup.sh 함수(interfaces 바꾸기, 주소 찾기, opld.toml·issue 쓰기, 설치 환경·대상
# 두 곳의 interfaces)를 견본과 비교한다. bash만 있으면 되고 macOS에서도 돈다.
# 함수는 셸 OPL_SH(기본 sh)로 source해 부른다. 설치 프로그램 환경과 같게 보려면(CI):
#   OPL_SH="busybox sh" OPL_AWK="busybox awk" bash packaging/tests/setup-test.sh
# 쓰는 법: bash packaging/tests/setup-test.sh
set -eu

TESTS_DIR="$(cd "$(dirname "$0")" && pwd)"
FIXTURES="$TESTS_DIR/fixtures"
SETUP="$TESTS_DIR/../iso/preseed/oplite-setup.sh"
read -r -a SH <<<"${OPL_SH:-sh}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# OPL_AWK가 있으면 함수의 awk가 그것을 쓰게 PATH 앞에 awk를 둔다.
if [ -n "${OPL_AWK:-}" ]; then
	mkdir -p "$WORK/bin"
	printf '#!/bin/sh\nexec %s "$@"\n' "$OPL_AWK" >"$WORK/bin/awk"
	chmod 0755 "$WORK/bin/awk"
	PATH="$WORK/bin:$PATH"
fi

# lib <함수> [인자...]: oplite-setup.sh를 함수만 정의하게 source하고 그 함수를 부른다.
lib() {
	# shellcheck disable=SC2016 # $0·$@는 안쪽 셸이 푼다.
	OPLITE_SETUP_LIB=1 "${SH[@]}" -c '. "$0"; "$@"' "$SETUP" "$@"
}

# 함수 라이브러리를 그 셸로 읽을 수 있어야 한다.
lib true || {
	echo "setup-test: ${SH[*]}로 $SETUP 을 읽지 못했다"
	exit 1
}

FAILED=0

ok() { echo "ok   $1"; }
fail() {
	echo "FAIL $1"
	FAILED=1
}

# same <이름> <기대 파일> <실제 파일>
same() {
	if diff -u "$2" "$3"; then ok "$1"; else fail "$1"; fi
}

# ascii <이름> <파일>: 영어 ASCII(출력 가능한 문자와 공백)만 있어야 한다.
ascii() {
	if LC_ALL=C grep -n '[^ -~[:space:]]' "$2"; then fail "$1: ASCII가 아닌 문자"; else ok "$1 ASCII"; fi
}

# interfaces 견본: <이름> <기대 종료 코드> <기대 주소>
check_interfaces() {
	name=$1 want_code=$2 want_addr=$3
	out="$WORK/interfaces-$name"
	code=0
	lib convert_interfaces "$FIXTURES/interfaces-$name.in" "$out" || code=$?
	if [ "$code" -ne "$want_code" ]; then
		fail "interfaces-$name: 종료 코드 $code (기대 $want_code)"
	fi
	same "interfaces-$name" "$FIXTURES/interfaces-$name.out" "$out"
	addr=$(lib primary_address "$out")
	if [ "$addr" = "$want_addr" ]; then
		ok "interfaces-$name 주소 '$addr'"
	else
		fail "interfaces-$name 주소 '$addr' (기대 '$want_addr')"
	fi
}

check_interfaces dhcp 0 ""
check_interfaces static 0 192.168.10.21
check_interfaces hotplug 0 ""
check_interfaces none 2 ""
check_interfaces ipv6 0 10.0.0.11
# 게이트웨이 없는 고정 주소: 기본 경로 블록이 없으면 첫 inet static 블록
check_interfaces static-nogw 0 10.20.0.5

# 기본 경로 인터페이스 이름
nic=$(lib default_nic "$FIXTURES/interfaces-hotplug.in")
if [ "$nic" = enp1s0 ]; then ok "default_nic hotplug"; else fail "default_nic hotplug: '$nic'"; fi
nic=$(lib default_nic "$FIXTURES/interfaces-static-nogw.in")
if [ "$nic" = enp2s0 ]; then ok "default_nic static-nogw"; else fail "default_nic static-nogw: '$nic'"; fi

# 노드 이름 규칙
for n in node1 opl-ci a 0 "$(printf 'a%.0s' $(seq 63))"; do
	if lib valid_name "$n"; then ok "valid_name '$n'"; else fail "valid_name '$n' 거짓"; fi
done
for n in "" -a a- My-Host my_host my.host "$(printf 'a%.0s' $(seq 64))"; do
	if lib valid_name "$n"; then fail "valid_name '$n' 참"; else ok "valid_name '$n' 거짓"; fi
done

# opld.toml: 정적 주소, DHCP 주소, 규칙에 맞지 않는 이름, 주소 없음(그 줄은 주석)
lib write_opld_toml "$WORK/opld-static.toml" node1 192.168.10.21
same "opld.toml 정적" "$FIXTURES/opld-static.toml" "$WORK/opld-static.toml"
lib write_opld_toml "$WORK/opld-dhcp.toml" opl-ci 10.0.2.15 dhcp
same "opld.toml DHCP" "$FIXTURES/opld-dhcp.toml" "$WORK/opld-dhcp.toml"
lib write_opld_toml "$WORK/opld-badname.toml" my_host 192.168.10.21
same "opld.toml 이름 규칙 밖" "$FIXTURES/opld-badname.toml" "$WORK/opld-badname.toml"
lib write_opld_toml "$WORK/opld-noaddr.toml" node1 "" dhcp
same "opld.toml 주소 없음" "$FIXTURES/opld-noaddr.toml" "$WORK/opld-noaddr.toml"

# 콘솔 로그인 화면: agetty 이스케이프 \4{vmbr0}가 그대로 남아야 한다.
lib write_issue "$WORK/oplite.issue"
same "issue" "$FIXTURES/oplite.issue" "$WORK/oplite.issue"

for f in opld-static.toml opld-dhcp.toml opld-badname.toml opld-noaddr.toml oplite.issue; do
	ascii "$f" "$WORK/$f"
done

# setup_network <이름> <설치 환경 interfaces 견본|-> <대상 interfaces 견본|-> <기대 견본> <기대 NIC>:
# 설치 환경 파일(lo가 아닌 iface가 없으면 대상 파일)을 바꿔 두 곳 모두에 쓴다.
check_network() {
	name=$1 inst_in=$2 target_in=$3 want=$4 want_nic=$5
	inst="$WORK/net-$name/installer" target="$WORK/net-$name/target"
	mkdir -p "$inst/etc/network" "$target/etc/network"
	[ "$inst_in" = - ] || cp "$inst_in" "$inst/etc/network/interfaces"
	[ "$target_in" = - ] || cp "$target_in" "$target/etc/network/interfaces"
	# shellcheck disable=SC2016 # $TARGET·$NIC는 안쪽 셸이 푼다.
	got_nic=$(OPLITE_INSTALLER_ROOT="$inst" TARGET="$target" lib eval 'setup_network "$TARGET" >/dev/null; echo "$NIC"')
	if [ "$got_nic" = "$want_nic" ]; then ok "setup_network $name NIC '$got_nic'"; else
		fail "setup_network $name NIC '$got_nic' (기대 '$want_nic')"
	fi
	same "setup_network $name 대상" "$want" "$target/etc/network/interfaces"
	same "setup_network $name 설치 환경" "$want" "$inst/etc/network/interfaces"
}

printf 'auto lo\niface lo inet loopback\n' >"$WORK/interfaces-lo"
check_network "설치 환경 원본" "$FIXTURES/interfaces-dhcp.in" - "$FIXTURES/interfaces-dhcp.out" ens3
check_network "설치 환경 우선" "$FIXTURES/interfaces-static.in" "$FIXTURES/interfaces-dhcp.in" \
	"$FIXTURES/interfaces-static.out" enp1s0
check_network "설치 환경에 lo만: 대상 원본" "$WORK/interfaces-lo" "$FIXTURES/interfaces-static.in" \
	"$FIXTURES/interfaces-static.out" enp1s0
check_network "설치 환경 없음: 대상 원본" - "$FIXTURES/interfaces-dhcp.in" "$FIXTURES/interfaces-dhcp.out" ens3

# 노드 이름: 설치 환경(없으면 대상)의 호스트 이름을 소문자로
mkdir -p "$WORK/name/installer/etc" "$WORK/name/target/etc"
echo OPL-Node1 >"$WORK/name/target/etc/hostname"
got=$(OPLITE_INSTALLER_ROOT="$WORK/name/installer" lib node_name "$WORK/name/target")
if [ "$got" = opl-node1 ]; then ok "node_name 대상"; else fail "node_name 대상: '$got'"; fi
echo Opl-CI >"$WORK/name/installer/etc/hostname"
got=$(OPLITE_INSTALLER_ROOT="$WORK/name/installer" lib node_name "$WORK/name/target")
if [ "$got" = opl-ci ]; then ok "node_name 설치 환경"; else fail "node_name 설치 환경: '$got'"; fi

# 설치 환경의 busybox tr은 문자 집합([:upper:])을 모른다. 스크립트가 그런 tr을 쓰지 않는지 본다.
if grep -n "tr '\[:" "$SETUP" >/dev/null; then fail "oplite-setup.sh가 tr 문자 집합을 쓴다"; else ok "tr 문자 집합 없음"; fi

# 단계 기록: 시작·끝은 로그(stdout)와 콘솔에 쓴다. 실패하면 단계 이름과 출력 끝 30줄을 콘솔에 쓰고 그 코드로 끝난다.
mkdir -p "$WORK/steps"
code=0
# shellcheck disable=SC2016 # $?는 안쪽 셸이 푼다.
OPLITE_CONSOLE="$WORK/steps/console" TMPDIR="$WORK/steps" \
	lib eval 'trap "on_exit \$?" EXIT; step one true; step two sh -c "seq 1 40; exit 3"; step three true' \
	>"$WORK/steps/log" 2>&1 || code=$?
{
	printf 'oplite-setup: %s\n' "start one" "done one" "start two" "FAILED two (exit 3)"
	seq 11 40
} >"$WORK/steps/console-want"
if [ "$code" -eq 3 ]; then ok "step 실패 종료 코드"; else fail "step 실패 종료 코드 $code (기대 3)"; fi
same "step 콘솔" "$WORK/steps/console-want" "$WORK/steps/console"
if grep -qx 1 "$WORK/steps/log" && grep -qx 40 "$WORK/steps/log" && grep -qx 'oplite-setup: start one' "$WORK/steps/log" &&
	grep -qx 'oplite-setup: FAILED two (exit 3)' "$WORK/steps/log"; then
	ok "step 로그"
else
	fail "step 로그"
	cat "$WORK/steps/log"
fi
# 콘솔에 쓰지 못해도(없는 디렉터리) 단계는 이어진다.
code=0
OPLITE_CONSOLE="$WORK/none/console" TMPDIR="$WORK/steps" lib eval 'step one true' >/dev/null 2>&1 || code=$?
if [ "$code" -eq 0 ]; then ok "step 콘솔 쓰기 실패 무시"; else fail "step 콘솔 쓰기 실패 무시: 종료 코드 $code"; fi

# install_package: CD의 패키지를 대상에 복사하고 in-target으로 apt-get을 무인(질문 없이, stdin 없음)으로 부른다.
# 그동안 대상에 policy-rc.d(exit 101)를 두고 끝나면(실패해도) 지운다.
mkdir -p "$WORK/pkg/cdrom/oplite" "$WORK/pkg/target/usr/sbin" "$WORK/pkg/bin"
printf 'deb' >"$WORK/pkg/cdrom/oplite/oplite_0.1.0_amd64.deb"
cat >"$WORK/pkg/bin/in-target" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$FAKE_LOG"
[ ! -f "$FAKE_TARGET/usr/sbin/policy-rc.d" ] || echo "policy-rc.d: $(tail -n1 "$FAKE_TARGET/usr/sbin/policy-rc.d")" >>"$FAKE_LOG"
exit "${FAKE_CODE:-0}"
EOF
chmod 0755 "$WORK/pkg/bin/in-target"
# install_with <in-target 종료 코드>
install_with() {
	: >"$WORK/pkg/log"
	code=0
	PATH="$WORK/pkg/bin:$PATH" FAKE_LOG="$WORK/pkg/log" FAKE_TARGET="$WORK/pkg/target" FAKE_CODE=$1 \
		OPLITE_CDROM="$WORK/pkg/cdrom" lib install_package "$WORK/pkg/target" >/dev/null 2>&1 || code=$?
}
cat >"$WORK/pkg/want" <<'EOF'
--pass-stdout sh -c DEBIAN_FRONTEND=noninteractive apt-get -y -q -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold install ifupdown2 isc-dhcp-client /var/cache/oplite/oplite_0.1.0_amd64.deb </dev/null 2>&1
policy-rc.d: exit 101
EOF
install_with 0
if [ "$code" -eq 0 ]; then ok "install_package"; else fail "install_package: 종료 코드 $code"; fi
same "install_package in-target" "$WORK/pkg/want" "$WORK/pkg/log"
if [ -f "$WORK/pkg/target/var/cache/oplite/oplite_0.1.0_amd64.deb" ]; then ok "install_package 복사"; else fail "install_package 복사"; fi
if [ ! -e "$WORK/pkg/target/usr/sbin/policy-rc.d" ]; then ok "install_package policy-rc.d 지움"; else
	fail "install_package policy-rc.d 남음"
fi
install_with 100
if [ "$code" -eq 100 ] && [ ! -e "$WORK/pkg/target/usr/sbin/policy-rc.d" ]; then ok "install_package 실패"; else
	fail "install_package 실패: 종료 코드 $code"
fi

# main 전체(가짜 in-target·ip): 단계가 차례로 콘솔에 찍히고 설정 파일이 생긴다.
M="$WORK/main"
mkdir -p "$M/installer/etc/network" "$M/target/etc/systemd/system" "$M/cdrom/preseed"
cp -R "$WORK/pkg/cdrom/oplite" "$M/cdrom/"
cp "$FIXTURES/interfaces-dhcp.in" "$M/installer/etc/network/interfaces"
echo opl-ci >"$M/installer/etc/hostname"
echo '[Unit]' >"$M/cdrom/preseed/oplite-ci-init.service"
cat >"$WORK/pkg/bin/ip" <<'EOF'
#!/bin/sh
echo "2: ens3    inet 10.0.2.15/24 brd 10.0.2.255 scope global dynamic ens3"
EOF
chmod 0755 "$WORK/pkg/bin/ip"
code=0
PATH="$WORK/pkg/bin:$PATH" FAKE_LOG="$M/in-target.log" FAKE_TARGET="$M/target" OPLITE_CDROM="$M/cdrom" \
	OPLITE_INSTALLER_ROOT="$M/installer" OPLITE_CONSOLE="$M/console" TMPDIR="$M" \
	"${SH[@]}" "$SETUP" "$M/target" || code=$?
printf 'oplite-setup: %s\n' "start install-package" "done install-package" "start network" "done network" \
	"start config" "done config" "start ci-init" "done ci-init" >"$M/console-want"
if [ "$code" -eq 0 ]; then ok "main"; else fail "main: 종료 코드 $code"; fi
same "main 콘솔" "$M/console-want" "$M/console"
same "main interfaces" "$FIXTURES/interfaces-dhcp.out" "$M/target/etc/network/interfaces"
same "main opld.toml" "$FIXTURES/opld-dhcp.toml" "$M/target/etc/opl/opld.toml"
if [ -f "$M/target/etc/issue.d/oplite.issue" ] && [ -f "$M/target/etc/systemd/system/oplite-ci-init.service" ] &&
	grep -qx 'systemctl enable oplite-ci-init.service' "$M/in-target.log" && [ -s "$M/target/root/oplite-setup.log" ]; then
	ok "main issue·ci-init·로그"
else
	fail "main issue·ci-init·로그"
fi

if [ "$FAILED" -ne 0 ]; then
	echo "setup-test: 실패"
	exit 1
fi
echo "setup-test: 모두 통과"
```

- [ ] **Step 2: 실행 권한**

Run: `chmod 0755 packaging/iso/build-iso.sh packaging/iso/preseed/oplite-setup.sh packaging/tests/iso-test.sh packaging/tests/setup-test.sh`
Expected: 출력 없음

- [ ] **Step 3: 확인**

Run: `bash packaging/tests/maint-test.sh && bash packaging/tests/build-deb-test.sh && bash packaging/tests/setup-test.sh && bash packaging/tests/iso-test.sh`
Expected: 모두 통과

Run: `shellcheck packaging/**/*.sh packaging/deb/postinst packaging/deb/prerm packaging/deb/postrm`
Expected: 경고 없음 (bash는 `shopt -s globstar` 뒤에, shellcheck가 없으면 CI `shell` 작업이 본다)

Run: `ruby -ryaml -e 'ARGV.each { |f| YAML.load_file(f) }' .github/workflows/*.yml .github/actions/build-deb/action.yml`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add .github packaging
git commit -m "feat(packaging): 설치 ISO 스크립트 — packaging/iso/build-iso.sh --netinst <iso> --deb <deb> --out <iso> [--unattended]가 debian.lock(Debian 13.7.0 amd64 netinst URL·SHA-256)과 해시를 비교하고 xorriso로 고칠 부트 설정만 꺼내고 /oplite/에 패키지, /preseed/에 oplite.cfg·oplite-setup.sh(unattended면 unattended.cfg·oplite-ci-init.service)를 넣고, BIOS(txt.cfg 맨 앞 label oplite, menu default를 gtk.cfg에서 옮김, unattended면 timeout 10)·UEFI(grub.cfg 첫 menuentry 앞 Install opLite, set default=0, unattended면 set timeout=1) 메뉴를 고치고 패치를 확인하고(없으면 실패) md5sum.txt를 고쳐 바뀐 파일과 새 디렉터리만 -map해 -boot_image any replay -volid OPLITE로 다시 묶음, preseed oplite.cfg(LVM atomic, deb.debian.org, standard·ssh-server, late_command)와 unattended.cfg(opl-ci, /dev/vda, UTC, 끝나면 전원 끔), oplite-setup.sh(패키지·ifupdown2·isc-dhcp-client 설치, 호스트 이름은 소문자로, 기본 경로 인터페이스를 vlan-aware vmbr0으로(설치 환경과 대상 둘 다에 씀, 게이트웨이 없는 고정 주소도), /etc/opl/opld.toml, 영어 /etc/issue.d/oplite.issue, OPLITE_SETUP_LIB=1이면 함수만), oplite-ci-init.service(첫 부팅 cluster init 출력을 ttyS0에), setup-test.sh(interfaces 견본·opld.toml·issue 비교, busybox로도), iso-test.sh(인자 검사·해시 확인·debian.lock 모양·실제 13.7.0 부트 설정 견본으로 메뉴 패치·가짜 xorriso로 main), ci.yml shell 작업이 두 시험을 돌림"
```

---

### Task 4: 설치 ISO E2E 워크플로

`.github/workflows/iso.yml`(수동 실행, 주 1회, `ci-probe/**` 브랜치·`v*` 태그 push), 작업 `iso-e2e`를 `firmware: [bios, uefi]`로: 패키지(composite action `.github/actions/build-deb`, ci.yml `package`도 같은 것을 씀) → netinst 내려받기(release URL이 404면 archive URL, 해시 확인, 캐시) → `build-iso.sh --unattended` → KVM으로 빈 디스크에 무인 설치(설치 스크립트 실패면 바로 실패, 40분, 실패 때 화면 덤프) → 디스크로 부팅 → 직렬 콘솔의 첫 부팅 `cluster init` 출력에서 CA 지문·admin 비밀번호 → `oplctl login`, `node list`에서 `opl-ci`가 online, `node get`의 브리지에 `vmbr0`, `network list`. 로그를 아티팩트로 올린다. 이 워크플로는 Linux CI에서만 돈다.

**Files:**
- Create: `.github/actions/build-deb/action.yml`
- Create: `.github/workflows/iso.yml`
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/actions/build-deb/action.yml`를 만든다:

```yaml
# Debian 패키지 oplite 만들기: 웹 UI → 릴리스 빌드(opld web-ui, oplctl) → build-deb.sh.
# ci.yml package 작업과 iso.yml이 같이 쓴다. 저장소를 checkout한 뒤 부른다.
# 패키지 버전 = 워크스페이스 버전~ci<run 번호>. 바이너리의 --version도 이 값이고, 환경 변수 OPL_VERSION으로 남긴다.
name: Build oplite deb
description: Build the web UI, release binaries and the oplite Debian package
outputs:
  deb:
    description: Path of the built package (dist/oplite_<version>_amd64.deb)
    value: ${{ steps.deb.outputs.path }}
runs:
  using: composite
  steps:
    - uses: pnpm/action-setup@v4
      with:
        package_json_file: web/package.json
    - uses: actions/setup-node@v4
      with:
        node-version: 22
        cache: pnpm
        cache-dependency-path: web/pnpm-lock.yaml
    - uses: dtolnay/rust-toolchain@stable
    - uses: Swatinem/rust-cache@v2
    - name: OpenSSL headers (native-tls)
      shell: bash
      run: |
        sudo apt-get update
        sudo apt-get install -y libssl-dev pkg-config
    - name: Web UI
      shell: bash
      run: |
        pnpm -C web install --frozen-lockfile
        pnpm -C web build
    - name: Package version
      shell: bash
      run: |
        WS_VERSION=$(grep '^version' Cargo.toml | head -n1 | cut -d'"' -f2)
        echo "OPL_VERSION=${WS_VERSION}~ci${{ github.run_number }}" >> "$GITHUB_ENV"
    - shell: bash
      run: cargo build --release -p opld --features web-ui -p oplctl
    - id: deb
      shell: bash
      run: |
        packaging/deb/build-deb.sh "$OPL_VERSION" dist
        echo "path=dist/oplite_${OPL_VERSION}_amd64.deb" >> "$GITHUB_OUTPUT"
```

`.github/workflows/ci.yml`에서

```yaml

  # Debian 패키지 oplite: 웹 UI를 넣은 릴리스 빌드 → build-deb.sh → Debian 13 컨테이너에서 설치 시험
  package:
```

을 다음으로 바꾼다:

```yaml

  # Debian 패키지 oplite: 웹 UI를 넣은 릴리스 빌드 → build-deb.sh(.github/actions/build-deb) → Debian 13 컨테이너에서 설치 시험
  package:
```

`.github/workflows/ci.yml`에서

```yaml
      - uses: actions/checkout@v4
      - uses: pnpm/action-setup@v4
        with:
          package_json_file: web/package.json
      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: pnpm
          cache-dependency-path: web/pnpm-lock.yaml
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: OpenSSL headers (native-tls)
        run: |
          sudo apt-get update
          sudo apt-get install -y libssl-dev pkg-config
      - name: Web UI
        run: |
          pnpm -C web install --frozen-lockfile
          pnpm -C web build
      # 패키지 버전 = 워크스페이스 버전~ci<run 번호>. 바이너리의 --version도 이 값이다.
      - name: Package version
        run: |
          WS_VERSION=$(grep '^version' Cargo.toml | head -n1 | cut -d'"' -f2)
          echo "OPL_VERSION=${WS_VERSION}~ci${{ github.run_number }}" >> "$GITHUB_ENV"
      - run: cargo build --release -p opld --features web-ui -p oplctl
      - run: packaging/deb/build-deb.sh "$OPL_VERSION" dist
      - name: Package info and contents
        run: |
          DEB="dist/oplite_${OPL_VERSION}_amd64.deb"
          dpkg-deb --info "$DEB"
          dpkg-deb --contents "$DEB"
      - name: Install test in Debian 13
        run: |
          docker run --rm -v "$PWD:/work" debian:13 \
            bash /work/packaging/tests/deb-test.sh "/work/dist/oplite_${OPL_VERSION}_amd64.deb"
      # 빌드 버전 시험을 OPL_VERSION을 준 빌드로 다시 돌린다(clap 기본 version으로 되돌리면 실패한다).
```

을 다음으로 바꾼다:

```yaml
      - uses: actions/checkout@v4
      - uses: ./.github/actions/build-deb
        id: build
      - name: Package info and contents
        env:
          DEB: ${{ steps.build.outputs.deb }}
        run: |
          dpkg-deb --info "$DEB"
          dpkg-deb --contents "$DEB"
      - name: Install test in Debian 13
        env:
          DEB: ${{ steps.build.outputs.deb }}
        run: |
          docker run --rm -v "$PWD:/work" debian:13 \
            bash /work/packaging/tests/deb-test.sh "/work/$DEB"
      # 빌드 버전 시험을 OPL_VERSION을 준 빌드로 다시 돌린다(clap 기본 version으로 되돌리면 실패한다).
```

`.github/workflows/iso.yml`를 만든다:

```yaml
name: ISO

# 설치 ISO E2E: 패키지 → netinst(debian.lock) → build-iso.sh --unattended → KVM으로 무인 설치 →
# 디스크로 부팅 → 첫 부팅 cluster init 출력(직렬 콘솔)으로 oplctl login·node list·node get.
# BIOS(isolinux 메뉴)와 UEFI(OVMF, grub 메뉴) 두 경로를 따로 돌린다.
# 오래 걸리므로(설치에 미러가 필요하다) PR마다 돌리지 않는다.
on:
  workflow_dispatch:
  schedule:
    - cron: "0 3 * * 1"
  push:
    branches: ["ci-probe/**"]
    tags: ["v*"]

env:
  CARGO_TERM_COLOR: always

jobs:
  iso-e2e:
    runs-on: ubuntu-latest
    timeout-minutes: 100
    strategy:
      fail-fast: false
      matrix:
        firmware: [bios, uefi]
    steps:
      - uses: actions/checkout@v4
      - uses: ./.github/actions/build-deb
        id: build
      - name: KVM, QEMU, xorriso
        run: |
          echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' \
            | sudo tee /etc/udev/rules.d/99-kvm.rules
          sudo udevadm control --reload-rules
          sudo udevadm trigger --name-match=kvm
          sudo apt-get update
          sudo apt-get install -y xorriso qemu-system-x86 qemu-utils ovmf jq
          ls -l /dev/kvm
      # UEFI: OVMF 코드(읽기 전용)와 쓰기 가능한 VARS 복사본. 설치가 VARS에 남긴 부트 항목으로 디스크 부팅을 한다.
      # ubuntu 24.04의 ovmf는 *_4M.fd다. 없으면 예전 이름을 쓴다.
      - name: Firmware
        if: matrix.firmware == 'uefi'
        run: |
          code=/usr/share/OVMF/OVMF_CODE_4M.fd vars=/usr/share/OVMF/OVMF_VARS_4M.fd
          if [ ! -f "$code" ]; then
            code=/usr/share/OVMF/OVMF_CODE.fd vars=/usr/share/OVMF/OVMF_VARS.fd
          fi
          ls -l "$code" "$vars"
          cp "$vars" "$RUNNER_TEMP/OVMF_VARS.fd"
          echo "FW_ARGS=-drive if=pflash,format=raw,readonly=on,file=$code -drive if=pflash,format=raw,file=$RUNNER_TEMP/OVMF_VARS.fd" >> "$GITHUB_ENV"
      - name: netinst version
        id: lock
        run: |
          sha=$(sed -n 's/^DEBIAN_NETINST_SHA256=//p' packaging/iso/debian.lock)
          echo "sha256=$sha" >> "$GITHUB_OUTPUT"
      - uses: actions/cache@v4
        with:
          path: ${{ runner.temp }}/netinst
          key: debian-netinst-${{ steps.lock.outputs.sha256 }}
      # debian.lock의 release/ URL은 다음 판이 나오면 cdimage/archive/<판>/로 옮겨진다. 404면 archive에서 받는다.
      # 어느 쪽이든 debian.lock의 SHA-256과 같아야 한다(build-iso.sh도 다시 확인한다).
      - name: Download Debian netinst
        env:
          NETINST_DIR: ${{ runner.temp }}/netinst
        run: |
          lock() { sed -n "s/^$1=//p" packaging/iso/debian.lock; }
          url=$(lock DEBIAN_NETINST_URL)
          release=$(lock DEBIAN_RELEASE)
          iso="$NETINST_DIR/netinst.iso"
          mkdir -p "$NETINST_DIR"
          if [ ! -f "$iso" ]; then
            code=$(curl -sSL -o "$iso.part" -w '%{http_code}' "$url")
            if [ "$code" = 404 ]; then
              url="https://cdimage.debian.org/cdimage/archive/$release/amd64/iso-cd/${url##*/}"
              echo "release URL 404, archive에서 받는다: $url"
              code=$(curl -sSL -o "$iso.part" -w '%{http_code}' "$url")
            fi
            if [ "$code" != 200 ]; then
              echo "netinst를 받지 못했다: $url (HTTP $code)" >&2
              exit 1
            fi
            mv "$iso.part" "$iso"
          fi
          echo "$(lock DEBIAN_NETINST_SHA256)  $iso" | sha256sum -c -
          echo "NETINST=$iso" >> "$GITHUB_ENV"
      - name: Build the unattended ISO
        run: |
          packaging/iso/build-iso.sh --netinst "$NETINST" --deb "$DEB" --out "$RUNNER_TEMP/oplite.iso" --unattended
          ls -l "$RUNNER_TEMP/oplite.iso"
        env:
          DEB: ${{ steps.build.outputs.deb }}
      # 설치가 끝나면 preseed가 전원을 끄고(-no-reboot) QEMU가 끝난다. QEMU를 백그라운드로 띄우고 30초마다 본다:
      # 끝났으면 다음 단계, 직렬 로그에 oplite-setup 실패가 보이거나 40분이 지나면 실패.
      # 실패하면 QEMU 모니터로 화면(install-screen.ppm, 아티팩트)을 남기고 직렬 로그의 oplite-setup: 줄을 보인다.
      - name: Unattended install
        working-directory: ${{ runner.temp }}
        run: |
          qemu-img create -f qcow2 disk.qcow2 20G
          # FW_ARGS는 UEFI일 때만 있고 단어로 나눠 쓴다.
          # shellcheck disable=SC2086
          qemu-system-x86_64 -enable-kvm -m 4096 -smp 2 ${FW_ARGS:-} \
            -drive file=disk.qcow2,if=virtio -cdrom oplite.iso -boot d \
            -serial file:install.log -display none \
            -monitor unix:"$RUNNER_TEMP/mon.sock",server,nowait \
            -netdev user,id=n0 -device virtio-net-pci,netdev=n0 -no-reboot &
          qemu=$!
          fail() {
            echo "$1" >&2
            python3 - "$RUNNER_TEMP/mon.sock" "$RUNNER_TEMP/install-screen.ppm" <<'EOF' || true
          import socket, sys, time
          s = socket.socket(socket.AF_UNIX)
          s.connect(sys.argv[1])
          s.sendall(("screendump " + sys.argv[2] + "\n").encode())
          time.sleep(3)
          s.close()
          EOF
            grep -a 'oplite-setup:' install.log || true
            kill "$qemu" 2>/dev/null || true
            exit 1
          }
          start=$(date +%s)
          while kill -0 "$qemu" 2>/dev/null; do
            if grep -aq 'oplite-setup: FAILED' install.log 2>/dev/null; then
              fail "oplite-setup(late_command)이 실패했다"
            fi
            if [ $(($(date +%s) - start)) -ge 2400 ]; then
              fail "40분 안에 설치가 끝나지 않았다"
            fi
            sleep 30
          done
          code=0
          wait "$qemu" || code=$?
          grep -a 'oplite-setup:' install.log || true
          if [ "$code" != 0 ]; then
            echo "QEMU가 $code로 끝났다" >&2
            exit 1
          fi
          tail -n 20 install.log
      - name: Boot the installed node
        working-directory: ${{ runner.temp }}
        run: |
          qemu-system-x86_64 -enable-kvm -m 4096 -smp 2 ${FW_ARGS:-} \
            -drive file=disk.qcow2,if=virtio \
            -serial file:boot.log -display none \
            -netdev user,id=n0,hostfwd=tcp::18006-:8006 -device virtio-net-pci,netdev=n0 \
            -daemonize -pidfile qemu.pid
      # oplite-ci-init.service가 첫 부팅에 cluster init 출력을 직렬 콘솔에 쓴다.
      - name: Wait for cluster init
        timeout-minutes: 15
        working-directory: ${{ runner.temp }}
        run: |
          # CA 지문 다음 줄(join URL)까지 나오면 지문 줄이 다 쓰인 것이다.
          # 그동안 QEMU가 죽거나 init이 실패하면(oplctl의 error: 줄, 유닛 실패) 바로 실패한다.
          until grep -q 'join URL for the next node' boot.log 2>/dev/null; do
            if ! kill -0 "$(cat qemu.pid)" 2>/dev/null; then
              echo "QEMU가 끝났다" >&2
              tail -n 50 boot.log >&2 || true
              exit 1
            fi
            if tr -d '\r' < boot.log 2>/dev/null | grep -E '^error: |Failed to start .*opLite CI first-boot' >&2; then
              echo "첫 부팅 cluster init이 실패했다" >&2
              exit 1
            fi
            sleep 5
          done
          fp=$(tr -d '\r' < boot.log | sed -n 's/.*CA fingerprint: \([^[:space:]]*\).*/\1/p' | head -n1)
          pw=$(tr -d '\r' < boot.log | sed -n 's/.*admin password (shown only once): \([^[:space:]]*\).*/\1/p' | head -n1)
          if [ -z "$fp" ] || [ -z "$pw" ]; then
            echo "boot.log에서 CA 지문이나 admin 비밀번호를 찾지 못했다" >&2
            exit 1
          fi
          echo "CA_FINGERPRINT=$fp" >> "$GITHUB_ENV"
          printf '%s\n' "$pw" > admin-password
      - name: oplctl login, node list, node get
        timeout-minutes: 8
        env:
          OPL_CONFIG: ${{ runner.temp }}/oplctl.json
        run: |
          oplctl=target/release/oplctl
          # init 직후 API가 HTTPS로 다시 열릴 때까지 잠깐 걸릴 수 있다.
          until $oplctl login --server https://127.0.0.1:18006 --ca-fingerprint "$CA_FINGERPRINT" \
            --password-file "$RUNNER_TEMP/admin-password"; do sleep 5; done
          # 노드가 상태(online, 브리지)를 기록할 때까지 기다린다.
          # 끝까지 나타나지 않으면 마지막 목록을 보이고 실패한다(원인을 보려고 30초마다도 찍는다).
          i=0
          until $oplctl -o json node list | jq -e '.[] | select(.name == "opl-ci" and .status.online == true)' >/dev/null; do
            i=$((i + 1))
            if [ $((i % 6)) -eq 0 ]; then $oplctl -o json node list || true; fi
            if [ "$i" -ge 48 ]; then
              echo "노드 opl-ci가 online이 되지 않았다" >&2
              $oplctl -o json node list >&2 || true
              exit 1
            fi
            sleep 5
          done
          $oplctl node list
          $oplctl node get opl-ci | jq -e '.status.bridges[] | select(.name == "vmbr0")'
          $oplctl network list
      - name: Stop the node
        if: always()
        working-directory: ${{ runner.temp }}
        run: |
          if [ -f qemu.pid ]; then kill "$(cat qemu.pid)" || true; fi
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: iso-e2e-logs-${{ matrix.firmware }}
          path: |
            ${{ runner.temp }}/install.log
            ${{ runner.temp }}/install-screen.ppm
            ${{ runner.temp }}/boot.log
          if-no-files-found: ignore
```

- [ ] **Step 2: 확인**

Run: `ruby -ryaml -e 'ARGV.each { |f| YAML.load_file(f) }' .github/workflows/*.yml .github/actions/build-deb/action.yml`
Expected: 오류 없음

- [ ] **Step 3: 커밋**

```bash
git add .github
git commit -m "ci: 설치 ISO E2E 워크플로 — .github/workflows/iso.yml(수동 실행, 주 1회, ci-probe/**·v* 태그 push)이 패키지를 만들고 debian.lock의 netinst를 받아(actions/cache, release/ URL이 404면 cdimage/archive/<판>/에서, 어느 쪽이든 SHA-256 확인) build-iso.sh --unattended로 ISO를 만들고 KVM으로 빈 20 GiB 디스크에 무인 설치(시간 제한 40분, 끝나면 전원 끔), 디스크로 부팅해 직렬 로그에서 첫 부팅 cluster init의 CA 지문·admin 비밀번호를 기다린 뒤(15분) hostfwd 18006으로 oplctl login·node list(opl-ci online)·node get(bridges에 vmbr0)·network list를 확인하고 직렬 로그를 아티팩트로 올림, 패키지 빌드를 composite action .github/actions/build-deb로 뽑아 ci.yml package 작업도 그것을 씀"
```

---

### Task 5: 문서

README 새 절 "설치 (ISO와 패키지)"(ISO 만들기, 설치 화면이 묻는 것, 설치가 하는 일, 첫 노드 init·다른 노드 join, 주소는 클러스터에 들어간 뒤 바꿀 수 없음, 시간대, root SSH, 패키지만 설치, 업그레이드·삭제, 한계)와 상단 상태 줄, 설계 §2.5 "구현(계획 31)"(실제 파일 이름과 설계와 다른 점), 처음 분해 문서의 하위 프로젝트 9 메모.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션, 위트니스 노드)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션, 위트니스 노드, Debian 패키지·설치 ISO)
```

`README.md`에서

```markdown
API 문서(OpenAPI)는 `https://127.0.0.1:8006/api/v1/openapi.json`에 있다.
```

을 다음으로 바꾼다:

````markdown
API 문서(OpenAPI)는 `https://127.0.0.1:8006/api/v1/openapi.json`에 있다.

## 설치 (ISO와 패키지)

서버에는 설치 ISO로 Debian 13(trixie)과 opLite를 함께 설치한다. ISO는 Debian netinst에 패키지 `oplite`와
preseed를 더한 것이다. 만들기는 Linux(amd64)에서 한다(`dpkg-deb`, `xorriso`가 필요하다).

```bash
sudo apt install xorriso libssl-dev pkg-config     # 그리고 Rust, Node.js 22와 pnpm
# 1. 패키지. 버전은 바이너리(--version)와 패키지에 같은 값을 쓴다(다르면 build-deb.sh가 멈춘다).
pnpm -C web install --frozen-lockfile && pnpm -C web build
OPL_VERSION=0.1.0 cargo build --release -p opld --features web-ui -p oplctl
packaging/deb/build-deb.sh 0.1.0 dist              # dist/oplite_0.1.0_amd64.deb
# 2. netinst. 판과 SHA-256은 packaging/iso/debian.lock에 있고, build-iso.sh가 해시를 확인한다.
#    그 판보다 새 판이 나와 URL이 404면 같은 파일을 cdimage/archive/<판>/amd64/iso-cd/에서 받는다.
. packaging/iso/debian.lock && curl -fLo netinst.iso "$DEBIAN_NETINST_URL"
# 3. ISO. 기본 항목 "Install opLite"가 더해진다(BIOS·UEFI, 원래 항목은 남는다).
packaging/iso/build-iso.sh --netinst netinst.iso --deb dist/oplite_0.1.0_amd64.deb --out oplite.iso
```

`oplite.iso`를 USB(`dd`)나 가상 CD로 부팅한다. "Install opLite"가 기본으로 골라져 있다. UEFI 메뉴에서는 맨 위
줄이고, BIOS 메뉴에서는 "Graphical install" 다음 두 번째 줄이다. 설치 화면은 다음만 묻는다: 네트워크(DHCP가
실패하면 수동), 호스트 이름, 도메인, root 비밀번호, 설치 디스크(디스크가 여럿이면 GRUB을 넣을 디스크도).
나머지는 정해져 있다: 언어·키보드 en_US·us, 시간대 UTC(설치한 뒤 `timedatectl set-timezone Asia/Seoul`처럼 바꾼다),
고른 디스크 전체에 LVM(`/`와 swap), 미러 `deb.debian.org`, 패키지 standard·ssh-server, 일반 사용자 없음.
**고른 디스크의 내용은 묻지 않고 지운다.**

설치 끝에 `oplite-setup.sh`가 돈다(로그는 대상 시스템의 `/root/oplite-setup.log`).

- 패키지 `oplite`, `ifupdown2`, `isc-dhcp-client`(`vmbr0`의 DHCP)를 설치한다(의존 패키지는 미러에서 받는다).
- 기본 경로 인터페이스(DHCP이거나 게이트웨이가 있는 것, 없으면 첫 고정 주소)를 VLAN-aware 브리지 `vmbr0`으로 옮긴다.
  주소·게이트웨이·DNS는 `vmbr0`으로 가고, NIC는 `iface <nic> inet manual`로 `bridge-ports`가 된다(`bridge-vids 2-4094`).
  설치 프로그램이 쓴 `/etc/network/interfaces`를 바꿔 설치 환경과 대상 시스템 두 곳에 쓴다(설치 프로그램은 이 파일을
  나중에 대상으로 복사한다). 인터페이스를 찾지 못하면 바꾸지 않는다.
- `/etc/opl/opld.toml`에 `name`(호스트 이름을 소문자로)과 `address`(`vmbr0`의 주소)를 쓴다. 호스트 이름이 노드 이름
  규칙(소문자·숫자·`-`, 1~63자)에 맞지 않거나 주소를 찾지 못하면 그 줄을 주석으로 둔다. 그때 opld는 설정 오류로
  시작하지 못하므로(`journalctl -u opld`) 값을 채우고 `systemctl restart opld`를 한다. 다른 설정은
  `/usr/share/oplite/opld.toml.example`을 본다.
- 콘솔 로그인 화면에 웹 UI 주소와 init·join 명령을 영어로 보인다(`/etc/issue.d/oplite.issue`). `opld.toml`의 주석도 영어다.
- `opld.service`를 켠다. 다시 부팅하면 opld가 돈다(`systemctl status opld`, `journalctl -u opld`).

설치한 시스템의 root는 SSH로 비밀번호 로그인을 할 수 없다(Debian 기본 `PermitRootLogin prohibit-password`).
콘솔에서 root로 로그인해 `adduser <이름>`으로 일반 사용자를 만들거나 `/root/.ssh/authorized_keys`에 공개 키를 넣는다.

클러스터는 설치한 뒤에 만든다. 첫 노드에서 `cluster init`을 하고, 다른 노드는 그 클러스터에 합류한다.

```bash
# 첫 노드(root)
oplctl cluster init --name prod                  # admin 비밀번호(한 번만 나온다), CA 지문, 합류 URL
oplctl login --server https://10.0.0.11:8006 --ca-fingerprint sha256:...
oplctl cluster join-token                        # 합류 URL은 한 번 쓰고 1시간 간다. 노드마다 새로 만든다
# 다른 노드(root)
oplctl cluster join 'opl-join://…'
```

웹 UI는 `https://<노드 주소>:8006/`이다(클러스터 화면이 합류 명령을 만들어 준다).

**DHCP 주소.** 설치를 DHCP로 하면 `address`는 설치할 때 받은 주소이고 파일에 그렇다는 주석이 붙는다. 다른 노드와
클라이언트가 이 주소로 노드에 닿고, 노드가 클러스터에 들어간 뒤에는 주소를 바꿀 수 없다(노드 인증서에 들어 있다).
DHCP 서버에서 주소를 예약하거나 고정 주소를 쓴다. `cluster init`·`join` 전이라면 `/etc/network/interfaces`의
`vmbr0`과 `opld.toml`의 `address`를 고치고 `systemctl restart opld`를 해도 된다.

**패키지만 설치하기.** Debian 13(amd64)이 이미 있으면 패키지만 넣는다. 브리지는 관리자가 만든다("실행 (Linux, KVM)").

```bash
sudo apt install ./oplite_0.1.0_amd64.deb         # 의존 패키지(QEMU, OVMF, nftables 등)도 받는다
sudo mkdir -p /etc/opl
sudo cp /usr/share/oplite/opld.toml.example /etc/opl/opld.toml   # name·address를 고친다
sudo systemctl start opld                         # 설정이 없으면 유닛은 시작하지 않고 건너뛴다
```

패키지는 `/usr/bin/opld`·`oplctl`(웹 UI 포함), `/usr/lib/systemd/system/opld.service`,
`/usr/share/oplite/opld.toml.example`을 설치한다. 업그레이드(`apt install`로 새 판)는 opld가 enable돼 있으면 다시 시작한다.
VM의 QEMU는 VM마다 자기 scope에 있어 opld가 멈추거나 다시 시작해도 꺼지지 않는다.

**지우기.** `apt remove oplite`는 opld를 멈추고, `apt purge oplite`는 서비스 링크까지 지운다. 어느 쪽도
`/var/lib/opl`(클러스터 데이터, 풀, 볼륨)과 `/etc/opl`을 지우지 않는다. 데이터를 지우는 것은 관리자의 일이다.

한계:

- 설치에 인터넷(Debian 미러)이 필요하다. 모든 패키지를 담은 오프라인 ISO는 없다.
- amd64만 있다(ISO와 패키지).
- 루트 파일 시스템은 LVM 하나다. ZFS 루트와 소프트웨어 RAID 설치 선택지는 없다.
- 설치 화면에서 클러스터를 만들거나 합류하지 않는다(설치한 뒤 위 명령으로).
````

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown
구현: 하위 프로젝트 8은 계획 29(OVA 가져오기: `VmImport` 리소스, OVA 올리기와 손으로 읽는 tar·OVF 해석, 원본 VM에서 opLite VM 제안(게스트 OS로 디스크 버스·NIC 모델, 코어 나누기, 경고), 사용자가 고친 값으로 디스크를 `.mf` 해시를 보며 추출해 볼륨으로 옮기고 꺼진 VM을 만드는 `VmImport` Task, 지우기, API·oplctl·웹, 실제 시험, 완료)과 계획 30(ESXi·vCenter에서 바로 가져오기: 서버 인증서 지문을 고정한 HTTPS 연결과 데이터스토어 브라우저(`/folder`) 목록, 데이터스토어의 `.vmx`·VMDK 디스크립터 해석, 비밀번호는 받은 노드의 메모리에만, 같은 `VmImport`와 Task에 extent 내려받기 단계, API·oplctl·웹, 가짜 ESXi로 실제 시험, 완료)으로 나눴다(`2026-11-04-vmware-import-design.md`). 하위 프로젝트 8은 끝났다. 실행 중인 VM 옮기기, 증분 동기화, VMware 스냅샷 체인, VDDK·NFC, 게스트 안 드라이버 바꾸기는 넣지 않았다.
```

을 다음으로 바꾼다:

```markdown
구현: 하위 프로젝트 8은 계획 29(OVA 가져오기: `VmImport` 리소스, OVA 올리기와 손으로 읽는 tar·OVF 해석, 원본 VM에서 opLite VM 제안(게스트 OS로 디스크 버스·NIC 모델, 코어 나누기, 경고), 사용자가 고친 값으로 디스크를 `.mf` 해시를 보며 추출해 볼륨으로 옮기고 꺼진 VM을 만드는 `VmImport` Task, 지우기, API·oplctl·웹, 실제 시험, 완료)과 계획 30(ESXi·vCenter에서 바로 가져오기: 서버 인증서 지문을 고정한 HTTPS 연결과 데이터스토어 브라우저(`/folder`) 목록, 데이터스토어의 `.vmx`·VMDK 디스크립터 해석, 비밀번호는 받은 노드의 메모리에만, 같은 `VmImport`와 Task에 extent 내려받기 단계, API·oplctl·웹, 가짜 ESXi로 실제 시험, 완료)으로 나눴다(`2026-11-04-vmware-import-design.md`). 하위 프로젝트 8은 끝났다. 실행 중인 VM 옮기기, 증분 동기화, VMware 스냅샷 체인, VDDK·NFC, 게스트 안 드라이버 바꾸기는 넣지 않았다.

구현: 하위 프로젝트 9는 계획 31(Debian 패키지와 설치 ISO: 빌드 버전 `opl_types::version::BUILD`와 `--version`, 패키지 `oplite`(opld·oplctl·웹 UI·systemd 유닛·maintainer 스크립트), Debian 13 netinst에 preseed와 패키지를 더한 설치 ISO(설치가 기본 경로 인터페이스를 VLAN-aware `vmbr0`으로 옮기고 `/etc/opl/opld.toml`을 씀), CI `shell`·`package` 작업과 ISO 무인 설치 E2E 워크플로 `iso.yml`, 완료)부터 했다(`2026-11-05-install-update-subscription-design.md`). 계획 32(버전과 업데이트)·33(구독과 텔레메트리)이 남았다.
```

`docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`에서

```markdown
- control: `Package: oplite`, `Architecture: amd64`, `Depends: qemu-system-x86, qemu-utils, ovmf, iproute2, nftables, systemd`, `Recommends: nfs-common, lvm2, thin-provisioning-tools, open-iscsi, lvm2-lockd, sanlock, ceph-common`, `Section: admin`, `Priority: optional`, `Maintainer`·`Homepage`는 저장소 값. `Version`은 인자 그대로(개발 빌드는 `0.1.0~git20261105.abc1234`처럼 `~`로 정식 버전보다 낮게).
- postinst `configure`: `systemctl daemon-reload`, 처음 설치면 `systemctl enable opld.service`(시작은 설정이 있을 때 유닛 조건이 정함 — `systemctl start`를 부르고 실패는 무시), 업그레이드면 `systemctl try-restart opld.service`.
- prerm `remove`: `systemctl stop opld.service`(VM은 scope에 남는다 — 패키지를 지워도 VM은 돌고, 다시 설치한 opld가 이어 맡는다), `upgrade`에서는 멈추지 않는다(postinst가 다시 시작).
```

을 다음으로 바꾼다:

```markdown
- control: `Package: oplite`, `Architecture: amd64`, `Depends: qemu-system-x86, qemu-utils, ovmf, iproute2, nftables, systemd`, `Recommends: nfs-common, lvm2, thin-provisioning-tools, open-iscsi, lvm2-lockd, sanlock, ceph-common`, `Section: admin`, `Priority: optional`, `Maintainer`·`Homepage`는 저장소 값. `Version`은 인자 그대로(개발 빌드는 `0.1.0~git20261105.abc1234`처럼 `~`로 정식 버전보다 낮게).
- postinst `configure`: `systemctl daemon-reload`, 처음 설치면 `systemctl enable opld.service`(시작은 설정이 있을 때 유닛 조건이 정함 — `systemctl start`를 부르고 실패는 무시), 업그레이드·재설정(`abort-remove`·`abort-deconfigure` 포함)이면 `systemctl is-enabled`일 때 `systemctl restart opld.service`(debhelper와 같다).
- prerm `remove`: `systemctl stop opld.service`(VM은 scope에 남는다 — 패키지를 지워도 VM은 돌고, 다시 설치한 opld가 이어 맡는다), `upgrade`에서는 멈추지 않는다(postinst가 다시 시작).
```

`docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`에서

```markdown

preseed(`oplite.cfg`)는 질문을 줄이되 다음은 사람에게 묻는다: 언어·키보드(기본 en_US·us), 네트워크(DHCP, 실패하면 수동), 호스트 이름, root 비밀번호, 설치 디스크. 나머지는 정한다: 파티션은 고른 디스크 전체에 LVM(`atomic` 레시피 — `/`, swap), 미러 `deb.debian.org`, 패키지 `standard`·`ssh-server`, 일반 사용자 만들지 않음, `popularity-contest` 끔, GRUB은 고른 디스크에.
```

을 다음으로 바꾼다:

```markdown

preseed(`oplite.cfg`)는 질문을 줄이되 다음은 사람에게 묻는다: 네트워크(필요하면 — DHCP가 실패하면 수동), 호스트 이름, 도메인, root 비밀번호, 설치 디스크. 나머지는 정한다: 언어·키보드(en_US·us), 시간대 UTC(설치 뒤 `timedatectl set-timezone`), 파티션은 고른 디스크 전체에 LVM(`atomic` 레시피 — `/`, swap), 미러 `deb.debian.org`, 패키지 `standard`·`ssh-server`, 일반 사용자 만들지 않음, `popularity-contest` 끔, GRUB은 고른 디스크에.
```

`docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`에서

```markdown

`--unattended <cfg>`(시험 전용)는 묻는 값을 모두 정한 preseed(호스트 이름 `opl-ci`, root 비밀번호, 첫 디스크, DHCP)를 추가로 넣고, 부트 메뉴 시간 제한을 1초로, 그리고 첫 부팅에 한 번 도는 유닛 `oplite-ci-init.service`가 `oplctl cluster init --name ci`의 출력을 직렬 콘솔(`/dev/ttyS0`)에 쓰게 한다. 커널 명령줄에 `console=ttyS0`를 더한다.
```

을 다음으로 바꾼다:

```markdown

`--unattended <cfg>`(시험 전용)는 묻는 값을 모두 정한 preseed(호스트 이름 `opl-ci`, root 비밀번호, 첫 디스크, DHCP)를 추가로 넣고, BIOS는 메뉴(vesamenu) 없이 직렬 콘솔로 opLite 항목을 바로 부팅하고 UEFI는 부트 메뉴 시간 제한을 1초로, 그리고 첫 부팅에 한 번 도는 유닛 `oplite-ci-init.service`가 `oplctl cluster init --name ci`의 출력을 직렬 콘솔(`/dev/ttyS0`)에 쓰게 한다. 커널 명령줄에 `console=ttyS0`를 더한다.
```

`docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`에서

```markdown
설치에 인터넷(Debian 미러)이 필요하다. DHCP로 설치하면 `address`가 그 순간의 주소다. ZFS·RAID 루트, arm64 ISO, 설치 화면에서 클러스터 만들기·합류는 없다. 패키지를 지워도 데이터는 남는다.
```

을 다음으로 바꾼다:

```markdown
설치에 인터넷(Debian 미러)이 필요하다. DHCP로 설치하면 `address`가 그 순간의 주소다. ZFS·RAID 루트, arm64 ISO, 설치 화면에서 클러스터 만들기·합류는 없다. 패키지를 지워도 데이터는 남는다.

### 2.5 구현(계획 31)

- 빌드 버전: `opl_types::version::BUILD`(빌드할 때 `OPL_VERSION`, 없거나 비었으면 `CARGO_PKG_VERSION`), opld·oplctl `--version`이 `<바이너리> <BUILD>`. oplctl이 opl-types에 의존한다. 시험은 opl-types 단위와 각 크레이트 `tests/version.rs`이고, 기대값을 `BUILD`가 아니라 시험을 빌드할 때의 `OPL_VERSION`으로 따로 구한다. CI `package`가 `OPL_VERSION=9.9.9~t`로 이 시험을 다시 돌려 clap 기본 `version`으로 되돌리면 실패한다.
- 패키지 `packaging/deb/`: `build-deb.sh <version> <out dir>`(`umask 022`, `DEBIAN/md5sums`, `opld --version`·`oplctl --version`이 `<바이너리> <version>`이 아니면 `OPL_VERSION`으로 다시 빌드하라는 안내와 함께 1, 바이너리는 기본 `target/release`, `OPL_BIN_DIR`로 바꿈, 만든 경로 `<out dir>/oplite_<version>_amd64.deb`를 출력, `dpkg-deb --build --root-owner-group -Zxz`, `Installed-Size`는 `du -sk`), `control.in`(`@VERSION@`·`@INSTALLED_SIZE@`), `copyright`, `opld.service`, `opld.toml.example`, `postinst`·`prerm`·`postrm`. 설계와 다른 점:
  - 유닛 경로는 `/usr/lib/systemd/system/opld.service`(merged /usr인 Debian 13의 자리). 유닛에 `Description=opLite node daemon`, `Documentation=https://github.com/gaiger777/opLite`, `WantedBy=multi-user.target`.
  - `Depends` 앞에 `libc6 (>= 2.39), libssl3t64`(ubuntu-24.04의 glibc로 빌드하고 native-tls로 OpenSSL에 링크한다). `Maintainer: opLite developers <gaiger777@gmail.com>`, `Homepage: https://github.com/gaiger777/opLite`.
  - maintainer 스크립트는 systemd가 도는지를 `${OPL_SYSTEMD_DIR:-/run/systemd/system}`로 본다. 돌지 않으면(컨테이너, 설치 프로그램의 in-target chroot) postinst는 처음 설치일 때 `systemctl enable`(링크만)만 하고 나머지 `systemctl` 호출을 건너뛴다. 업그레이드·재설정(remove 뒤 다시 설치, `abort-remove`·`abort-deconfigure`)은 `try-restart`가 아니라 enable돼 있으면 `restart`다(멈춘 opld도 다시 시작한다. VM은 scope에 있어 영향이 없다). 모든 호출은 `|| true`. prerm은 `remove`와 `deconfigure`에서 stop. postrm은 `purge`에서 `systemctl disable`과 `rm -f ${OPL_SYSTEMD_ETC:-/etc/systemd/system}/multi-user.target.wants/opld.service`(링크 정리)도 한다.
  - `opld.toml.example`은 README "실행 (Linux, KVM)"의 설정과 같고 맨 위에 복사 안내 주석.
- ISO `packaging/iso/`: `build-iso.sh --netinst <iso> --deb <deb> --out <iso> [--unattended]`(bash). `--unattended`는 인자 없는 플래그이고 저장소의 `preseed/unattended.cfg`를 쓴다. `--deb`의 파일 이름이 `oplite_*_amd64.deb`가 아니면 2(설치가 그 이름을 찾는다). 끝에 출력 경로를 찍는다. `OPLITE_ISO_LIB=1`로 source하면 함수만(`lock_value`, `verify_netinst`, `patch_isolinux`, `patch_grub`, `check_patched`, `sums_update`, `md5_update`, `sha256_update`). `debian.lock`은 셸 `KEY=value`(`DEBIAN_RELEASE=13.7.0`, `DEBIAN_CODENAME=trixie`, `DEBIAN_NETINST_URL`, `DEBIAN_NETINST_SHA256`)이고, 13.7.0이 아직 `cdimage/archive/`에 없어 `release/` URL이다(다음 판이 나오면 404가 되므로 `cdimage/archive/<판>/`에서 받는다 — CI가 그렇게 한다. lock은 archive URL로 바꾸거나 판을 올린다). 해시는 `sha256sum`, 없으면 `shasum -a 256`.
  - BIOS 메뉴: `isolinux/txt.cfg`의 첫 `label` 앞에 `label oplite`(kernel·append는 install 항목에서 복사, preseed 인자는 `---` 앞)와 `menu default`를 넣고, `gtk.cfg`의 `menu default`를 뺀다. `menu.cfg`가 `gtk.cfg`를 먼저 include하므로 opLite 항목은 기본이지만 두 번째 줄이다. `isolinux.cfg`의 `default vesamenu.c32`는 바꾸면 메뉴가 사라지므로 그대로 둔다. unattended면 `isolinux.cfg`를 메뉴 없이 새로 쓴다: `serial 0 115200`, `default oplite`, `prompt 0`, `timeout 1`, `label oplite`(txt.cfg 항목과 같은 kernel·append). CI 프로브에서 vesamenu가 화면 없이(`-display none`) 기다려 BIOS 설치가 직렬 출력 하나 없이 시간 제한에 걸렸다.
  - UEFI 메뉴: `boot/grub/grub.cfg`의 첫 `menuentry` 앞에 `set default=0`, unattended면 `set timeout=1`, `menuentry 'Install opLite'`(kernel·initrd는 텍스트 install 항목에서).
  - 패치 뒤 `check_patched`가 `label oplite`·`Install opLite`(unattended면 `isolinux.cfg` 첫 줄 `serial 0 115200`·`default oplite`와 `set timeout=1`)를 확인하고 없으면 실패한다. 13.7.0 ISO의 실제 `isolinux.cfg`·`txt.cfg`·`gtk.cfg`·`grub.cfg`가 시험 견본(`packaging/tests/fixtures/iso/`)이다.
  - 다시 묶기(Debian 위키 RepackBootableISO): ISO 전체를 풀지 않고 고칠 파일(`isolinux/txt.cfg`·`gtk.cfg`·`isolinux.cfg`, `boot/grub/grub.cfg`, `md5sum.txt`)만 `-extract`로 꺼낸다. `md5sum.txt`는 원래 목록에서 `grub.cfg`의 합을 바꾸고 새 파일(`oplite/`, `preseed/`)을 더한다(`isolinux/`는 원래처럼 없다). `sha256sum.txt`는 따로 꺼내 보고 있으면 같은 방식으로 고쳐 얹고, 없으면 건너뛴다. 그다음 `xorriso -indev <netinst> -outdev <out> -overwrite on -map <dir>/oplite /oplite -map <dir>/preseed /preseed -map <고친 파일> /<경로> … -boot_image any replay -volid OPLITE`. 부트 이미지와 `boot.cat`은 원래 것을 그대로 둔다(트리 전체를 `/`에 `-map`하면 El Torito 부트 이미지가 ISO의 데이터 파일이 아니게 되어 실패했다). EXIT trap은 전역 `ISO_WORK`를 지운다.
  - 부트 인자: 일반 `auto=true priority=high preseed/file=/cdrom/preseed/oplite.cfg`, unattended `auto=true priority=critical preseed/file=/cdrom/preseed/unattended.cfg console=ttyS0,115200n8`.
- preseed: `preseed/oplite.cfg`(위 §2.2, `partman-auto-lvm/guided_size max`, 시간대 `Etc/UTC`·`clock-setup/utc`, `apt-setup/cdrom/set-first`·`set-next`·`set-failed` false(APT 소스에 설치 CD를 넣지 않는다), 마지막 `preseed/late_command string sh /cdrom/preseed/oplite-setup.sh /target`), `grub-installer/bootdev`는 정하지 않는다. `preseed/unattended.cfg`(첫 줄 `preseed/include string oplite.cfg`, 호스트 `opl-ci`, root 비밀번호 `opl-ci-root`(시험 전용), `partman-auto/disk`·`grub-installer/bootdev` `/dev/vda`, NTP 끔, `debian-installer/exit/poweroff`로 끝나면 전원 끔). 설계와 다른 점:
  - 언어·키보드(en_US·us)는 묻지 않고 preseed로 정한다.
  - 묻는 것은 네트워크(필요하면)·호스트 이름·도메인·root 비밀번호·디스크다.
  - 시간대는 UTC다(설치 뒤 `timedatectl set-timezone`).
  - unattended BIOS는 vesamenu 없이 직렬 콘솔로 뜬다.
- `preseed/oplite-setup.sh <target>`(POSIX sh, 로그 `<target>/root/oplite-setup.log`): `install_package`(`/cdrom/oplite/oplite_*.deb`를 `<target>/var/cache/oplite/`로 복사해 `in-target --pass-stdout sh -c 'DEBIAN_FRONTEND=noninteractive apt-get -y -q -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold install ifupdown2 isc-dhcp-client /var/cache/oplite/<deb> </dev/null 2>&1'`. in-target의 stdin은 설치 프로그램의 debconf 통로라 apt·dpkg가 거기서 답을 기다리면 무인 설치가 멈추므로 질문 없이 stdin 없이 돌린다. 그동안 대상에 `policy-rc.d`(exit 101)를 두고 끝나면(실패해도) 지운다. `<target>/run/systemd/system`이 있으면 로그에 남긴다), `default_nic`·`convert_interfaces <in> <out>`(awk; `inet dhcp`이거나 `gateway`가 있는 lo 아닌 첫 `inet` 블록, 없으면 lo 아닌 첫 `inet static` 블록, 그 블록과 같은 NIC의 `inet6` 블록을 `vmbr0`으로, `allow-hotplug`는 `auto`로, 못 찾으면 그대로 쓰고 2), `setup_network <target>`(late_command는 netcfg가 설정을 `/target`으로 복사하는 `55netcfg-copy-config`보다 먼저 돌므로 원본은 설치 환경의 `/etc/network/interfaces`이고 lo 아닌 iface가 없으면 대상의 것, 결과는 두 곳 모두에 쓴다. 설치 환경 루트는 `OPLITE_INSTALLER_ROOT`), `node_name <target>`(설치 환경, 없으면 대상의 `/etc/hostname`을 소문자로), `valid_name`(소문자·숫자·`-`, 1~63자, 앞뒤 `-` 아님), `primary_address`(vmbr0의 정적 주소, `/prefix` 뗌), `current_address <nic>`(DHCP면 설치 환경의 `ip -4 -o addr show dev`), `write_opld_toml <out> <name> <address> [dhcp]`(영어 주석: 설치가 쓴 파일과 예시 위치, 클러스터에 들어간 뒤에는 주소를 바꿀 수 없다는 것, DHCP면 `# This address came from DHCP.`. 이름이 규칙에 맞지 않거나 주소가 비면 그 줄을 주석으로 두고 로그를 남긴다), `write_issue`(`/etc/issue.d/oplite.issue`, 영어 ASCII 네 줄: 웹 UI 주소 `https://\4{vmbr0}:8006/`, 첫 노드 `oplctl cluster init`, 다른 노드 `oplctl cluster join-token`과 `oplctl cluster join`). `/cdrom/preseed/oplite-ci-init.service`가 있으면 복사하고 `in-target systemctl enable`. `main`은 단계(`install-package`, `network`, `config`, `ci-init`)마다 `step`으로 시작·끝을 로그와 설치 환경 콘솔(`/dev/console`, 무인 설치에서는 직렬)에 `oplite-setup: start|done <단계>`로 쓰고, 실패하면 EXIT trap이 `oplite-setup: FAILED <단계> (exit N)`과 그 단계 출력 끝 30줄을 콘솔에 쓴다(CI 프로브에서 late_command가 직렬에 아무것도 남기지 않고 멈췄다). 시험이 바꾸는 경로: `OPLITE_CDROM`, `OPLITE_CONSOLE`, `OPLITE_INSTALLER_ROOT`. `OPLITE_SETUP_LIB=1`이면 함수만.
- `preseed/oplite-ci-init.service`(시험 전용 oneshot, `After=`·`Requires=opld.service`, `ConditionPathExists=!/var/lib/oplite-ci-init.done`): 제어 소켓 `/run/opl/opld.sock`(opld 기본 `run_dir`, oplctl `--socket` 기본값)을 2초 간격 60번까지 기다리고 `oplctl cluster init --name ci`, 성공했을 때만 완료 표시(`&&` 사슬, 셸의 `$`는 `$$`). 먼저 `ExecStartPre=-/bin/cat /root/oplite-setup.log`로 설치 로그를 찍는다. 출력은 `/dev/ttyS0`.
- 시험(`packaging/tests/`, bash, macOS에서도 돈다): `build-deb-test.sh`(가짜 `opld`·`oplctl`·`dpkg-deb`로 버전이 같으면 만들고 다르면 1, 4경우), `maint-test.sh`(가짜 `systemctl`로 처음 설치(systemd 있음·없음)·업그레이드(enable 여부)·remove 뒤 configure·abort-remove·abort-deconfigure·remove·deconfigure·purge(링크 지움) 등의 호출 목록, 26경우), `setup-test.sh`(함수를 `OPL_SH`(기본 `sh`)로 source해 부른다. `fixtures/interfaces-{dhcp,static,static-nogw,hotplug,none,ipv6}.{in,out}`, `fixtures/opld-{static,dhcp,badname,noaddr}.toml`, `fixtures/oplite.issue`를 `diff -u`로 비교, ASCII 확인, 이름 규칙, 설치 환경·대상 interfaces와 호스트 이름, 단계 콘솔 기록과 실패, 가짜 `in-target`으로 `install_package`(apt 명령·`policy-rc.d`), 가짜 `in-target`·`ip`로 `main` 전체, 64경우), `iso-test.sh`(인자 검사(`--deb` 이름 포함)·해시 확인·`debian.lock` 모양, 메뉴 패치 견본 비교(일반·unattended), `md5_update`·`sha256_update`, 가짜 `xorriso`·`sha256sum`으로 `main`의 꺼내기·다시 묶기 인자, 얹은 `md5sum.txt`, `sha256sum.txt`가 있을 때와 없을 때, 실패 때 종료 코드·작업 디렉터리 정리, 39경우). `deb-test.sh <deb>`는 Debian 13 컨테이너 안에서만 돈다(설치, `--version`, `md5sums`·`dpkg --verify`, `systemd-analyze verify`, 경로, enable 링크, `dpkg --purge` 뒤 링크 없음·`/var/lib/opl`·`/etc/opl` 남음).
- CI: `ci.yml`에 작업 `shell`(`shopt -s globstar`로 `shellcheck packaging/**/*.sh`와 maintainer 스크립트, 위 네 시험, busybox를 설치해 `OPL_SH="busybox sh" OPL_AWK="busybox awk"`로 `setup-test.sh`를 한 번 더)과 `package`(composite action `.github/actions/build-deb`(`id: build`, 경로는 `steps.build.outputs.deb`) → `dpkg-deb --info`·`--contents` → `docker run debian:13 … deb-test.sh` → `OPL_VERSION=9.9.9~t cargo test -p opl-types -p opld -p oplctl --lib --test version` → 아티팩트 `oplite-deb`). `.github/actions/build-deb`는 pnpm·node 22로 웹 빌드, `OPL_VERSION=<Cargo.toml 워크스페이스 버전>~ci<run 번호>`를 `GITHUB_ENV`에 두고 `cargo build --release -p opld --features web-ui -p oplctl`, `build-deb.sh "$OPL_VERSION" dist`, 출력 `deb`. `.github/workflows/iso.yml`의 작업 `iso-e2e`(행렬 `firmware: [bios, uefi]`, `fail-fast: false`, 100분; 수동 실행, 월요일 03:00 UTC, `ci-probe/**`·`v*` 태그 push. uefi는 설치와 부팅 둘 다 `OVMF_CODE_4M.fd`(없으면 `OVMF_CODE.fd`)를 읽기 전용 pflash로, VARS는 작업 디렉터리의 복사본으로): 패키지 → netinst(`actions/cache`, `release/` URL이 404면 `cdimage/archive/<판>/`, `sha256sum -c`) → `build-iso.sh --unattended` → 20 GiB qcow2에 KVM 무인 설치(`-no-reboot`, 직렬 `install.log`. QEMU를 백그라운드로 띄우고 30초마다 보아 끝나면 다음, 직렬에 `oplite-setup: FAILED`가 보이거나 40분이 지나면 QEMU 모니터로 `screendump`(`install-screen.ppm`)를 남기고 실패) → 디스크로 부팅(`hostfwd=tcp::18006-:8006`, 직렬 `boot.log`) → 직렬 로그에서 CA 지문·admin 비밀번호(15분, 그동안 QEMU가 끝나거나 `error:` 줄·유닛 실패가 보이면 바로 실패) → `oplctl login`·`node list`(`opl-ci` online)·`node get opl-ci`의 `bridges`에 `vmbr0`·`network list` → 로그 아티팩트 `iso-e2e-logs-<firmware>`.
```

`docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`에서

```markdown
- 그 노드의 Task 실행기가: `systemd-run --unit=opl-upgrade-<task id 앞 8자> --collect --property=Type=exec --setenv=DEBIAN_FRONTEND=noninteractive --setenv=LANG=C /bin/sh -c 'apt-get update -q && apt-get -y -q -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold dist-upgrade' `, 출력은 `StandardOutput=append:<log_dir>/upgrade-<task id>.log`. 그 뒤 2초마다 `systemctl show -p ActiveState -p Result <unit>`를 본다. `inactive`·`Result=success`면 성공, `failed`면 실패(로그 끝 20줄을 Task 오류에).
- opld가 업그레이드로 다시 시작하면(postinst `try-restart`): 노드 시작 때 이 노드의 끝나지 않은 `NodeUpgrade` Task는 실패로 정리하지 않고(다른 Task 종류와 다른 점) 실행기가 같은 유닛을 다시 지켜본다. 유닛이 이미 사라졌으면(`--collect`) 로그 파일의 마지막 줄과 `dpkg --audit`가 비었는지로 판단한다 — 비었으면 성공, 아니면 "the upgrade was interrupted; run `dpkg --configure -a` on the node" 실패.
- 끝나면 업데이트 확인을 한 번 돌리고 `NodeStatus.version`이 새 값이 된다.
```

을 다음으로 바꾼다:

```markdown
- 그 노드의 Task 실행기가: `systemd-run --unit=opl-upgrade-<task id 앞 8자> --collect --property=Type=exec --setenv=DEBIAN_FRONTEND=noninteractive --setenv=LANG=C /bin/sh -c 'apt-get update -q && apt-get -y -q -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold dist-upgrade' `, 출력은 `StandardOutput=append:<log_dir>/upgrade-<task id>.log`. 그 뒤 2초마다 `systemctl show -p ActiveState -p Result <unit>`를 본다. `inactive`·`Result=success`면 성공, `failed`면 실패(로그 끝 20줄을 Task 오류에).
- opld가 업그레이드로 다시 시작하면(postinst `restart`): 노드 시작 때 이 노드의 끝나지 않은 `NodeUpgrade` Task는 실패로 정리하지 않고(다른 Task 종류와 다른 점) 실행기가 같은 유닛을 다시 지켜본다. 유닛이 이미 사라졌으면(`--collect`) 로그 파일의 마지막 줄과 `dpkg --audit`가 비었는지로 판단한다 — 비었으면 성공, 아니면 "the upgrade was interrupted; run `dpkg --configure -a` on the node" 실패.
- 끝나면 업데이트 확인을 한 번 돌리고 `NodeStatus.version`이 새 값이 된다.
```

- [ ] **Step 2: 커밋**

```bash
git add README.md docs
git commit -m "docs: 설치 ISO와 패키지 — README 새 절 \"설치 (ISO와 패키지)\"(패키지·netinst·build-iso.sh로 ISO 만들기, 설치 화면이 묻는 것, 설치가 하는 일: 패키지·vmbr0·opld.toml·콘솔 안내·opld 서비스, 첫 노드 cluster init과 다른 노드 join-token·join, DHCP 주소 주의, 패키지만 설치하기와 설정 예시 복사, 업그레이드는 opld를 다시 시작하고 VM은 계속 돎, remove·purge여도 /var/lib/opl·/etc/opl은 남음, 한계: 미러 필요·amd64만·LVM 루트만·설치 화면에서 클러스터 없음), 상단 상태에 Debian 패키지·설치 ISO, 설계 §2.5 \"구현(계획 31)\"(실제 파일 이름, 설계와 다른 점: /usr/lib/systemd/system 유닛, libc6·libssl3t64 의존, OPL_SYSTEMD_DIR와 systemd가 없을 때 enable만, --unattended 플래그, menu default로 BIOS 기본 항목, debian.lock release URL; 시험과 CI 작업 shell·package·iso-e2e, composite action build-deb), 노드 코어 설계 §1.1에 하위 프로젝트 9 구현 메모"
```

---

## 완료 기준 (계획 31)

- `opld --version`과 `oplctl --version`이 빌드할 때 준 `OPL_VERSION`을 보인다.
- CI `package` 작업이 `oplite_<버전>_amd64.deb`를 만든다. 이 패키지는 `debian:13`에 의존성과 함께 설치되고, 바이너리 버전·유닛·md5sums가 맞고, purge 뒤 링크가 없고 데이터는 남는다.
- CI `shell` 작업에서 shellcheck와 셸 시험(busybox 포함)이 통과한다.
- `iso.yml`의 `iso-e2e`가 BIOS와 UEFI에서 모두 통과한다. 무인 설치, 디스크 부팅, 첫 부팅 `cluster init`, `oplctl login`을 거쳐 노드 `opl-ci`가 online이고 `vmbr0` 브리지가 있다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`가 통과한다.

## 알고 받아들인 제한

- 설치에 인터넷(Debian 미러)이 필요하다. 패키지와 ISO는 amd64뿐이다.
- 사람이 하는 대화형 설치는 CI에서 시험하지 않는다. 무인 preseed로만 시험한다.
- DHCP로 설치하면 그때 받은 주소가 opld 주소가 된다. 클러스터에 들어간 뒤에는 바꿀 수 없다.
- `debian.lock`의 판이 archive로 옮겨지면 워크플로는 archive URL을 쓴다. README의 수동 명령은 사람이 URL을 고쳐야 한다.
- 그 밖의 제한은 설계 문서 §2.4·§2.5와 README "설치 (ISO와 패키지)" 절에 있다.
