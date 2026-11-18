# 개발

빌드와 시험 명령. 처음 보는 사람은 [README](../../README.md)의 빠른 시작부터.

## 개발

Linux에서 빌드하려면 `libssl-dev`와 `pkg-config`가 필요하다(클라이언트 TLS가 native-tls다).

```bash
cargo test --workspace                                              # 단위 테스트
brew install qemu                                                   # macOS 통합 테스트용 (Linux: 위 apt 패키지)
cargo test -p opl-driver --features storage-it --test dir_storage   # qemu-img 스토리지 테스트
cargo test -p opl-qemu --features qemu-it --test qemu_boot          # 실제 QEMU 기동 (macOS hvf, Linux KVM/TCG)
cargo test -p opl-driver --features qemu-it --test qemu_driver      # 드라이버: 기동, 재부착, 크래시 감지
cargo test -p opl-reconciler --features qemu-it --test qemu_restart # reconciler: orbivisord 재시작 후 재연결
cargo test -p opl-api --features storage-it --test volumes          # API: 볼륨 생성·가져오기 (qemu-img)
cargo test -p opld --features qemu-it --test e2e                    # E2E: CLI → VM → 콘솔 → kill -9 → 재연결
cargo test -p opld --features qemu-it --test backup                 # E2E: 실행 중 백업 → VM 지우기 → 복원(qemu-img, qemu-io 필요)
cargo test -p opld --features qemu-it --test vm_import              # E2E: OVA 올리기 → 제안 → 가져오기 → 볼륨 내용(qemu-img 필요)
cargo test -p opld --test cluster                                   # E2E: orbivisord 프로세스 3개로 클러스터 구성
# 폴더·권한·할당량·2단계 인증(QEMU 없음, `cargo test --workspace`에도 든다): 폴더 사용자의 목록·404, 위임, 할당량 409,
# 범위 토큰, TOTP 둘째 단계·복구 코드·관리자 초기화, 노드 둘에서 동시에 만들어도 할당량을 지킴
cargo test -p opld --test permissions
cargo test -p opl-api --test permissions --test quotas --test two_factor --test webauthn   # API: 경로 시행·watch 거르기, 할당량, 2FA, 소프트웨어 인증기로 WebAuthn
cargo test -p opl-api --lib authz                                   # 권한 색인: 옛 역할과의 동치, insta 행렬, proptest(색인 = 참조 구현)
cargo test -p opl-cluster --test three_nodes concurrent_creates_on_two_nodes_keep_the_folder_quota  # memnet 세 노드 할당량 경쟁
cargo test --release -p opl-cluster --features scale-it --lib quota_apply_cost -- --nocapture       # VM 1만 개에서 할당량 apply 시간
cargo test -p opld --features starve-it --test starve              # E2E: 리더의 API 실행기를 막아도 리스·Raft 리더 그대로
# 지표 기록·경보(QEMU 없음, [test] 고리가 있는 metrics-it 빌드): SIGKILL 뒤 기록, Prometheus 긁기(엄격한 텍스트 형식 파서),
# 127.0.0.1 webhook으로 시험 보내기·경보 발생·해제·보낸 기록
cargo test -p opld --features metrics-it --test metrics_history --test alerts
OPL_TEST_PROMTOOL=promtool cargo test -p opld --features metrics-it --test metrics_history   # promtool check metrics로도 본다(CI linux-kvm)
cargo test -p opld --features qemu-it --test metrics_history --test alerts   # 위에 더해 VM 줄과 멈춘 채 옮긴 VM의 기록(QEMU 필요)
cargo test -p opld --features migration-it,qemu-it --test migration metrics_history_follows_a_live_migration  # 라이브 마이그레이션 뒤 VM 기록
cargo test -p opld --features qemu-it --test templates              # E2E: 템플릿 → 연결 배포(qcow2 겹층) → 평탄화, 카탈로그 받기 → 템플릿(qemu-img·qemu-io 필요)
# x86_64 Linux, KVM: 실제 게스트 — Ubuntu 24.04 클라우드 이미지 템플릿 → 연결 복제 2대 켜기·보고 → 평탄화(CI install-media 작업)
OPL_TEST_CLOUD_IMAGE=/tmp/ubuntu-24.04-server-cloudimg-amd64.img OPL_TEST_CLOUD_SUMS=/tmp/SHA256SUMS \
  cargo test -p opld --features templates-it,qemu-it --test templates -- --nocapture
cargo test -p oplctl --features catalog-it --test catalog -- --nocapture  # 내장 이미지 카탈로그 링크·체크섬 파일 확인(인터넷 필요)
cargo test -p opld --features qemu-it --test vtpm                   # E2E: TPM·Secure Boot·게스트 에이전트 CLI 흐름(swtpm 없어도 됨 — 노드 능력대로)
# Linux, swtpm·swtpm-tools: 실제 swtpm으로 TPM VM의 상태 만들기·해시·초기화, 드라이버의 swtpm 수명과 TPM VM 옮기기
cargo test -p opld --features vtpm-it --test vtpm
cargo test -p opl-driver --features vtpm-it --test qemu_driver --test qemu_migration
# x86_64 Linux, KVM, swtpm·ovmf: 실제 게스트 — Ubuntu 24.04 클라우드 이미지를 Secure Boot·TPM·게스트 에이전트로 켜 /dev/tpm0·
# SecureBoot 변수·EK 해시 보고, 게스트 정보·얼린 스냅샷·에이전트 끄기, 다시 켜기·복제의 EK(CI install-media 작업)
OPL_TEST_CLOUD_IMAGE=/tmp/ubuntu-24.04-server-cloudimg-amd64.img \
  cargo test -p opld --features vtpm-it,install-media-it,qemu-it --test vtpm -- --nocapture
# Linux, root: 실제 tap과 브리지
# root가 만든 빌드 결과가 ./target에 섞이지 않게 따로 둔다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opl-driver --features qemu-it,net-it   # tap·브리지·VLAN, 이름공간 사이 VXLAN, nft 방화벽(nftables·python3 필요)
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test bridge  # E2E: 브리지 네트워크
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test vxlan   # E2E: VXLAN 네트워크
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test firewall  # E2E: VM 방화벽(nftables 필요)
# Linux, root: 실제 NFS 풀. nfs-kernel-server로 127.0.0.1에 rw,no_root_squash로 내보낸 디렉터리가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_NFS_EXPORT=/srv/opl-nfs cargo test -p opld --features nfs-it --test nfs
# Linux, root: 실제 LVM-thin 풀. 테스트가 LV를 만들고 지워도 되는 VG/thin pool과 lvm2, qemu-io가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_LVM=opltest/tp cargo test -p opl-driver --features lvm-it --test lvm_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_LVM=opltest/tp cargo test -p opld --features lvm-it --test lvm
# Linux, root: 실제 ZFS 풀. 테스트가 zvol을 만들고 지워도 되는 데이터셋과 zfsutils-linux, qemu-io가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_ZFS=opltestz/opl cargo test -p opl-driver --features zfs-it --test zfs_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_ZFS=opltestz/opl cargo test -p opld --features zfs-it --test zfs
# Linux, root: 실제 Ceph RBD 풀. 테스트가 이미지를 만들고 지워도 되는 Ceph 풀과 ceph-common, qemu-block-extra, qemu-io,
# client.admin으로 접속할 수 있는 /etc/ceph/ceph.conf·키링이 필요하다. 제한된 사용자 시험은 아래 세 변수가 있을 때만 돈다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_RBD=rbd \
  OPL_TEST_RBD_USER=opltest OPL_TEST_RBD_NAMESPACE=opl OPL_TEST_RBD_CONF=/etc/opl-ceph/ceph.conf \
  cargo test -p opl-driver --features rbd-it --test rbd_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_RBD=rbd cargo test -p opld --features rbd-it --test rbd
# Linux(Ubuntu 24.04), root, Ceph가 없는 버려도 되는 기계: orbivisord가 apt로 Ceph를 설치하고 노드 하나에 mon·mgr, 시험이 만든
# 루프 장치에 OSD, 풀 등록, 그 풀의 디스크로 VM 켜기(Ceph 꾸러미와 데몬이 남는다 — CI `ceph` 작업)
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features ceph-it,qemu-it --test ceph -- --nocapture
# Linux, root: 실제 공유 LVM 풀. 테스트가 LV를 만들고 지워도 되는, 잠금을 시작한 공유(lvmlockd) VG와 lvm2-lockd, sanlock,
# qemu-io가 필요하다. OPL_TEST_ISCSI(포털,타깃)가 있으면 opld 시험은 VG가 그 타깃의 LUN 위에 있다고 본다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_LVM_SHARED=oplshared \
  cargo test -p opl-driver --features lvm-shared-it --test lvm_shared_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_LVM_SHARED=oplshared \
  OPL_TEST_ISCSI=127.0.0.1,iqn.2003-01.org.linux-iscsi.opl:shared cargo test -p opld --features lvm-shared-it --test lvm_shared
# 실제 라이브 마이그레이션: 한 호스트의 orbivisord 둘, 공유 디렉터리 풀과 노드마다 다른 경로의 로컬 디렉터리 풀(디스크 복사,
# macOS는 TCG로 돈다). nfs-it·rbd-it·lvm-shared-it와 그 환경 변수를 함께 주면(Linux, root) 그 풀로도 옮기고, lvm-it·zfs-it와
# 함께 주면 로컬 디스크를 디렉터리 풀 → LVM-thin·ZFS 풀 → 디렉터리 풀로 옮긴다.
cargo test -p opld --features migration-it --test migration
# 실제 HA 복구: 한 호스트의 orbivisord 둘(process 펜싱)과 위트니스, 공유 디렉터리 풀(macOS는 TCG로 돈다).
cargo test -p opld --features ha-it --test ha
# Linux(Ubuntu·Debian), root: 실제 OpenLDAP. ldap-setup.sh가 이 기계의 slapd 설정과 디렉터리를 지우고 늘 새 데이터베이스로
# 다시 설정한다(다시 돌려도 된다 — 옛 데이터베이스는 /var/backups로 옮겨짐). 버려도 되는 기계에서만: CI(`CI=true`)가 아니면
# OPL_LDAP_SETUP_WIPE=1을 주어야 돌고, 없으면 설명을 찍고 끝난다(CI `ldap` 작업).
# 스크립트와 시험이 같은 값을 읽는다(아래는 CI ldap 작업의 값. 스크립트는 없으면 이 값을 쓰지만 시험은 모두 있어야 한다).
export OPL_TEST_LDAP_URL=ldaps://127.0.0.1:636 OPL_TEST_LDAP_STARTTLS_URL=ldap://127.0.0.1:389 \
  OPL_TEST_LDAP_CA=/tmp/opl-test-ldap-ca.pem OPL_TEST_LDAP_BASE_DN=dc=example,dc=com \
  OPL_TEST_LDAP_BIND_DN=cn=admin,dc=example,dc=com OPL_TEST_LDAP_BIND_PASSWORD=opl-admin-pw \
  OPL_TEST_LDAP_USER=alice OPL_TEST_LDAP_USER_PASSWORD=alice-pw OPL_TEST_LDAP_OTHER_USER=bob \
  OPL_TEST_LDAP_OTHER_PASSWORD=bob-pw OPL_TEST_LDAP_GROUP=cn=admins,ou=groups,dc=example,dc=com
sudo -E OPL_LDAP_SETUP_WIPE=1 sh .github/scripts/ldap-setup.sh
cargo test -p opl-api --features ldap-it --test ldap_it
```

`nfs-it` 시험은 orbivisord가 내보내기를 마운트하고, 그 풀에 볼륨을 만들고 qcow2 스냅샷을 찍고 지운 뒤, 풀을 지우면
언마운트하는지 본다(CI `linux-kvm` 작업이 `/srv/opl-nfs`를 내보낸다). **내보내기는 버려도 되는 디렉터리**를 쓴다.
시험은 실패해도 임시 디렉터리를 지우기 전에 orbivisord를 멈추고 마운트를 풀며, 풀지 못하면 임시 디렉터리를 지우지 않고
남긴다(그 경로를 출력한다). NFS 서버를 막 시작했으면 잠금 유예(약 90초)
때문에 첫 실행이 느릴 수 있다.

`lvm-it` 시험은 `OPL_TEST_LVM`의 thin pool에 `opl-` LV를 만들고 지운다. 드라이버 시험은 만들기·가져오기·스냅샷·
되돌리기·복사·지우기를, opld 시험은 LVM 디스크로 VM을 띄워 실행 중 스냅샷, 멈춘 뒤 되돌리기, 디렉터리 풀로 복제,
지우기를 본다. **버려도 되는 VG**를 쓴다. CI `linux-kvm` 작업은 루프 장치 위에 VG `opltest`와 thin pool `tp`를 둔다:

```bash
truncate -s 2G /tmp/opltest.img
LOOP=$(sudo losetup --find --show /tmp/opltest.img)
sudo pvcreate -y "$LOOP" && sudo vgcreate opltest "$LOOP" && sudo lvcreate -y -T -L 1G opltest/tp
# 끝나면: sudo vgremove -y opltest && sudo pvremove "$LOOP" && sudo losetup -d "$LOOP"
```

`zfs-it` 시험은 `OPL_TEST_ZFS`의 데이터셋에 `opl-` zvol을 만들고 지운다. 드라이버 시험은 만들기·정보·크기·
가져오기·스냅샷(가장 최근으로만 되돌리기, 여러 볼륨을 한 명령으로)·복사·지우기를, opld 시험은 ZFS 디스크로 VM을
띄워 실행 중 스냅샷, 최근이 아닌 스냅샷 되돌리기의 409, 멈춘 뒤 최근 스냅샷으로 되돌리기, 디렉터리 풀로 복제,
지우기를 본다. **버려도 되는 데이터셋**을 쓴다. CI `linux-kvm` 작업은 파일 위에 zpool `opltestz`와 데이터셋
`opltestz/opl`을 둔다:

```bash
sudo modprobe zfs
truncate -s 2G /tmp/opltestz.img
sudo zpool create opltestz /tmp/opltestz.img && sudo zfs create opltestz/opl
# 끝나면: sudo zpool destroy opltestz && rm /tmp/opltestz.img
```

`rbd-it` 시험은 `OPL_TEST_RBD`의 Ceph 풀에 `opl-` 이미지를 만들고 지운다. 드라이버 시험은 만들기·정보·크기·
가져오기·스냅샷(더 오래된 스냅샷으로 되돌리기)·백엔드 안팎의 복사·지우기와 제한된 사용자·네임스페이스·명시적
conf로 접속하기를, opld 시험은 RBD 디스크로 VM을 띄워
실행 중 스냅샷, 메모리 스냅샷의 409, 멈춘 뒤 앞 스냅샷으로 되돌렸다가 다시 최근 스냅샷으로 되돌리기, 디렉터리
풀로 복제, 지우기를 본다. **버려도 되는 Ceph 풀**을 쓴다. CI `linux-kvm` 작업은 microceph로 노드 하나짜리 Ceph를
띄우고 풀 `rbd`를 둔다(준비에 1–2분):

```bash
sudo apt install ceph-common qemu-block-extra
sudo snap install microceph --channel squid/stable && sudo microceph cluster bootstrap
sudo snap unalias microceph                          # 별칭(/snap/bin/rbd 등)은 snap에 갇혀 /etc/opl-ceph·/tmp를 못 본다
sudo mkdir -p /etc/ceph && sudo ln -sf /var/snap/microceph/current/conf/ceph.conf /etc/ceph/ceph.conf
sudo sh -c 'ln -sf /var/snap/microceph/current/conf/*.keyring /etc/ceph/'
sudo ceph config set global mon_allow_pool_size_one true && sudo ceph config set global osd_pool_default_size 1
sudo microceph disk add loop,4G,1                     # OSD가 올라올 때까지 기다린다(sudo ceph osd stat)
sudo ceph osd pool create rbd 32 && sudo ceph osd pool set rbd size 1 --yes-i-really-mean-it
sudo rbd pool init rbd                                # PG가 active가 된 뒤(sudo ceph -s)
# 제한된 사용자 시험: 네임스페이스와 사용자, 그 키링만 가리키는 conf
sudo rbd namespace create rbd/opl && sudo mkdir -p /etc/opl-ceph
sudo ceph auth get-or-create client.opltest mon 'profile rbd' osd 'profile rbd pool=rbd' mgr 'profile rbd pool=rbd' \
  | sudo tee /etc/opl-ceph/opltest.keyring >/dev/null
{ sudo cat /etc/ceph/ceph.conf; printf '\n[client.opltest]\nkeyring = /etc/opl-ceph/opltest.keyring\n'; } \
  | sudo tee /etc/opl-ceph/ceph.conf >/dev/null
# 끝나면: sudo snap remove --purge microceph
```

`lvm-shared-it` 시험은 `OPL_TEST_LVM_SHARED`의 공유 VG에 `opl-` LV를 만들고 지운다. 드라이버 시험은 만들기(0
채우기)·배타 활성화와 끄기·연 장치는 끌 수 없음·크기(늘린 부분의 0 채우기)·가져오기와 실패한 가져오기 정리·백엔드 안팎의 복사·지우기를,
opld 시험은 orbivisord가 iSCSI 로그인과 잠금 시작부터 해서 풀을 준비하는지(`OPL_TEST_ISCSI`가 있으면 시험이 먼저 잠금을
멈추고 로그아웃한다), 공유 LVM 디스크로 VM을 켜면 LV가 켜지고 멈추면 꺼지는지, 스냅샷의 409, orbivisord가 다시 뜰 때 남은
활성화 정리(실행 중인 VM의 LV는 그대로), 디렉터리 풀과 같은 풀로 복제, 지우기를 본다. **버려도 되는 VG**를 쓴다. CI
`linux-kvm` 작업은 targetcli(LIO)로 루프백 iSCSI 타깃을 만들어 로그인하고 그 LUN 위에 공유 VG `oplshared`를 둔다
(lvmlockd는 호스트 전체에 켜지지만 로컬 VG인 LVM-thin 시험의 `opltest`는 그대로 쓴다):

```bash
sudo apt install targetcli-fb open-iscsi lvm2-lockd sanlock
IQN=iqn.2003-01.org.linux-iscsi.opl:shared
sudo modprobe target_core_mod && sudo modprobe iscsi_target_mod && sudo systemctl start iscsid
truncate -s 2G /tmp/oplshared.img
sudo targetcli /backstores/fileio create oplshared /tmp/oplshared.img
sudo targetcli /iscsi create "$IQN"                  # 포털 0.0.0.0:3260이 함께 생긴다
sudo targetcli "/iscsi/$IQN/tpg1/luns" create /backstores/fileio/oplshared
sudo targetcli "/iscsi/$IQN/tpg1" set attribute authentication=0 demo_mode_write_protect=0 generate_node_acls=1 cache_dynamic_acls=1
sudo iscsiadm -m discovery -t sendtargets -p 127.0.0.1 && sudo iscsiadm -m node -T "$IQN" -p 127.0.0.1 --login
sudo udevadm settle
sudo modprobe softdog
sudo sed -i 's/^\s*#\?\s*use_lvmlockd = .*/\tuse_lvmlockd = 1/' /etc/lvm/lvm.conf
printf 'local {\n\thost_id = 1\n}\n' | sudo tee /etc/lvm/lvmlocal.conf
sudo systemctl start wdmd sanlock lvmlockd
sudo vgcreate --shared oplshared "/dev/disk/by-path/ip-127.0.0.1:3260-iscsi-$IQN-lun-0"   # 잠금 시작까지 20–30초
# 끝나면: sudo vgremove -y oplshared && sudo iscsiadm -m node -T "$IQN" -p 127.0.0.1 --logout
#         && sudo targetcli /iscsi delete "$IQN" && sudo targetcli /backstores/fileio delete oplshared
#         (lvm.conf의 use_lvmlockd를 0으로 되돌린다)
```

웹 UI (`web/`):

```bash
pnpm -C web install
pnpm -C web dev              # http://localhost:5173, /api는 OPLD_URL(기본 https://localhost:8006)로 넘긴다
pnpm -C web test             # 단위·컴포넌트 테스트 (Vitest) — axe 접근성 검사와 색 토큰 대비 시험 포함
pnpm -C web test src/tokens.test.ts   # 색 토큰 대비만: index.css의 밝음·어두움 oklch 값으로 글 4.5:1, 상태 칩 4.5:1, 포커스 링·차트 3:1
pnpm -C web typecheck && pnpm -C web lint
pnpm -C web gen:api          # API를 바꾼 뒤 src/api/schema.d.ts를 다시 만든다(CI가 확인한다)
pnpm -C web exec playwright install chromium
pnpm -C web build && cargo test -p opld --features web-it --test web   # E2E: 브라우저 → 업로드 → VM → 콘솔 → 정리, 폴더 사용자·2FA
pnpm -C web build && cargo test -p opld --features web-it --test web browser_shows_a_folder_user_only_their_folder   # 폴더 사용자만(QEMU 없이 돈다)
```

브라우저 E2E(`web/e2e/vm.spec.ts`, Playwright Chromium)는 영역 탭으로 화면을 옮기고, ⌘K 팔레트로 VM을 열고, VM 목록의
일괄 막대로 강제 정지하고, VM 동작 메뉴(⋯)로 복제·편집·지우기를 한다. 이 Mac(Homebrew QEMU)에서도 돈다(약 25초). jsdom
시험이 잡지 못하는 경로 이동·렌더 문제가 있으므로 **웹을 바꾸면 커밋 전에 이 Mac에서 돌린다.** 실패하면
`web/test-results/*/error-context.md`(화면 스냅숏)와 `trace.zip`이 남는다.

둘째 브라우저 E2E(`web/e2e/permissions.spec.ts`)는 `web.rs`의 `browser_shows_a_folder_user_only_their_folder`가 돌린다. Rust 쪽이
`/teamA`·`/teamB`와 VM, 그룹 grant로 `/teamA` operator를 받은 `none` 사용자를 준비하고, 브라우저는 그 사용자로 VM 목록·폴더
트리·팔레트에 `/teamA`만 보이는지, + 만들기가 폴더를 골라 두고 노드 칸 없이 VM을 만드는지, 설정 › 2단계 인증에서 인증 앱을 켠
뒤 둘째 단계로 다시 로그인하는지 본다. 두 시험은 spec 하나씩을 돌리고 하나씩 차례로 돈다.

접근성 검사는 `src/test/axe.ts`의 `expectNoAxeViolations(container)`가 `axe-core`를 직접 부른다(jsdom에서 안 되는
`color-contrast` 규칙은 끄고 `src/tokens.test.ts`가 대신한다). 머리줄·팔레트·서랍·목록·상세·설정·홈 시험에서 돈다. 새 색
토큰 짝은 `tokens.test.ts`의 표에 더한다.

`cargo test --workspace --all-features`는 `web-it`도 켜므로 위 준비(빌드한 `web/dist`, pnpm, Chromium)가
필요하다.

systemd가 있는 Linux에서는 `OPL_TEST_SYSTEMD=1`을 주면 QEMU 테스트가 systemd 스코프로 돈다(CI의
`linux-kvm` 작업). macOS에서는 Docker의 privileged Linux 컨테이너(`--init`)로 Linux 테스트를 TCG로
돌릴 수 있다.
