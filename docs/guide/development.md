# 개발

빌드와 시험 명령. 처음 보는 사람은 [README](../../README.md)의 빠른 시작부터.

## 개발

Linux에서 빌드하려면 `libssl-dev`와 `pkg-config`가 필요하다(클라이언트 TLS가 native-tls다).

시험·빌드 환경 변수는 계획 74에서 이름이 바뀌었다(`ORBIVISOR_TEST_*` 등 — 옛 이름은 받지 않으므로 시험 서버의 셸 설정도 바꾼다).

```bash
cargo test --workspace                                              # 단위 테스트
brew install qemu                                                   # macOS 통합 테스트용 (Linux: 위 apt 패키지)
cargo test -p orbivisor-driver --features storage-it --test dir_storage   # qemu-img 스토리지 테스트
cargo test -p orbivisor-qemu --features qemu-it --test qemu_boot          # 실제 QEMU 기동 (macOS hvf, Linux KVM/TCG)
cargo test -p orbivisor-driver --features qemu-it --test qemu_driver      # 드라이버: 기동, 재부착, 크래시 감지
cargo test -p orbivisor-reconciler --features qemu-it --test qemu_restart # reconciler: orbivisord 재시작 후 재연결
cargo test -p orbivisor-api --features storage-it --test volumes          # API: 볼륨 생성·가져오기 (qemu-img)
cargo test -p orbivisord --features qemu-it --test e2e                    # E2E: CLI → VM → 콘솔 → kill -9 → 재연결
cargo test -p orbivisord --features qemu-it --test backup                 # E2E: 실행 중 백업 → VM 지우기 → 복원(qemu-img, qemu-io 필요)
cargo test -p orbivisord --features qemu-it --test vm_import              # E2E: OVA 올리기 → 제안 → 가져오기 → 볼륨 내용(qemu-img 필요)
cargo test -p orbivisord --test cluster                                   # E2E: orbivisord 프로세스 3개로 클러스터 구성
# 폴더·권한·할당량·2단계 인증(QEMU 없음, `cargo test --workspace`에도 든다): 폴더 사용자의 목록·404, 위임, 할당량 409,
# 범위 토큰, TOTP 둘째 단계·복구 코드·관리자 초기화, 노드 둘에서 동시에 만들어도 할당량을 지킴
cargo test -p orbivisord --test permissions
cargo test -p orbivisor-api --test permissions --test quotas --test two_factor --test webauthn   # API: 경로 시행·watch 거르기, 할당량, 2FA, 소프트웨어 인증기로 WebAuthn
cargo test -p orbivisor-api --lib authz                                   # 권한 색인: 옛 역할과의 동치, insta 행렬, proptest(색인 = 참조 구현)
cargo test -p orbivisor-cluster --test three_nodes concurrent_creates_on_two_nodes_keep_the_folder_quota  # memnet 세 노드 할당량 경쟁
cargo test --release -p orbivisor-cluster --features scale-it --lib quota_apply_cost -- --nocapture       # VM 1만 개에서 할당량 apply 시간
cargo test -p orbivisord --features starve-it --test starve              # E2E: 리더의 API 실행기를 막아도 리스·Raft 리더 그대로
# 지표 기록·경보(QEMU 없음, [test] 고리가 있는 metrics-it 빌드): SIGKILL 뒤 기록, Prometheus 긁기(엄격한 텍스트 형식 파서),
# 127.0.0.1 webhook으로 시험 보내기·경보 발생·해제·보낸 기록
cargo test -p orbivisord --features metrics-it --test metrics_history --test alerts
ORBIVISOR_TEST_PROMTOOL=promtool cargo test -p orbivisord --features metrics-it --test metrics_history   # promtool check metrics로도 본다(CI linux-kvm)
cargo test -p orbivisord --features qemu-it --test metrics_history --test alerts   # 위에 더해 VM 줄과 멈춘 채 옮긴 VM의 기록(QEMU 필요)
cargo test -p orbivisord --features migration-it,qemu-it --test migration metrics_history_follows_a_live_migration  # 라이브 마이그레이션 뒤 VM 기록
cargo test -p orbivisord --features qemu-it --test templates              # E2E: 템플릿 → 연결 배포(qcow2 겹층) → 평탄화, 카탈로그 받기 → 템플릿(qemu-img·qemu-io 필요)
# x86_64 Linux, KVM: 실제 게스트 — Ubuntu 24.04 클라우드 이미지 템플릿 → 연결 복제 2대 켜기·보고 → 평탄화(CI install-media 작업)
ORBIVISOR_TEST_CLOUD_IMAGE=/tmp/ubuntu-24.04-server-cloudimg-amd64.img ORBIVISOR_TEST_CLOUD_SUMS=/tmp/SHA256SUMS \
  cargo test -p orbivisord --features templates-it,qemu-it --test templates -- --nocapture
cargo test -p orbivisorctl --features catalog-it --test catalog -- --nocapture  # 내장 이미지 카탈로그 링크·체크섬 파일 확인(인터넷 필요)
cargo test -p orbivisord --features qemu-it --test vtpm                   # E2E: TPM·Secure Boot·게스트 에이전트 CLI 흐름(swtpm 없어도 됨 — 노드 능력대로)
# Linux, swtpm·swtpm-tools: 실제 swtpm으로 TPM VM의 상태 만들기·해시·초기화, 드라이버의 swtpm 수명과 TPM VM 옮기기
cargo test -p orbivisord --features vtpm-it --test vtpm
cargo test -p orbivisor-driver --features vtpm-it --test qemu_driver --test qemu_migration
# x86_64 Linux, KVM, swtpm·ovmf: 실제 게스트 — Ubuntu 24.04 클라우드 이미지를 Secure Boot·TPM·게스트 에이전트로 켜 /dev/tpm0·
# SecureBoot 변수·EK 해시 보고, 게스트 정보·얼린 스냅샷·에이전트 끄기, 다시 켜기·복제의 EK(CI install-media 작업)
ORBIVISOR_TEST_CLOUD_IMAGE=/tmp/ubuntu-24.04-server-cloudimg-amd64.img \
  cargo test -p orbivisord --features vtpm-it,install-media-it,qemu-it --test vtpm -- --nocapture
# 게스트 CPU 모델·플래그(계획 63)
cargo test -p orbivisor-types cpu                                         # 순수: 모델·플래그 serde 하위 호환, resolve·can_run·baseline 표, 보류 cpu
cargo test -p orbivisor-api --test cpu_models                             # API(Mock 노드 셋·카탈로그 둘): 별칭 고정·모호, 기준 auto·고정 409, 약한 노드 합류, 마이그레이션 409, host 경고
cargo test -p orbivisor-qemu --features qemu-it --test cpu_probe -- --nocapture   # 노드 CPU 탐침: 실제 QEMU에 QMP로 묻기(이 Mac은 HVF·TCG, KVM이면 응답을 로그에)
ORBIVISOR_RECORD_CPU_FIXTURES=$PWD/crates/orbivisor-qemu/tests/fixtures/cpu \
  cargo test -p orbivisor-qemu --features qemu-it --test cpu_probe -- --nocapture  # 탐침 고정물(tests/fixtures/cpu/{arch}-{accel}.json) 다시 기록
# 실제 QEMU(게스트 OS 없음): 숨긴 플래그가 모든 vCPU에서 꺼짐(QMP qom-get), 숨김 목록 → 검사 루프 보류 → 전원 껐다 켜기, 명명 모델,
# 받는 쪽은 기록된 CPU 그대로(노드 둘). x86_64만: vCPU 핫플러그 뒤 새 vCPU도 꺼짐, 못 주는 기능이 있는 모델은 enforce로 거절.
# 이 Mac은 TCG로 aarch64(neoverse-n1)를 돌리고, x86 시험은 CI linux-kvm(FEATURES에 orbivisord/cpu-it)
cargo test -p orbivisord --features cpu-it --test cpu
# x86_64 Linux, KVM: 실제 게스트 — 러너 CPU에 있는 플래그(avx512vbmi → avx2 → pcid → sse4_2)를 --cpu-flags=-FLAG로 숨긴 Ubuntu 24.04
# 클라우드 이미지의 /proc/cpuinfo에 없음, 실행 중 --no-cpu-flags는 보류로 남고 vm reboot 뒤 다시 보임(CI install-media 작업)
ORBIVISOR_TEST_CLOUD_IMAGE=/tmp/ubuntu-24.04-server-cloudimg-amd64.img \
  cargo test -p orbivisord --features install-media-it,qemu-it --test install_media a_hidden_cpu_flag_is_absent_in_a_real_guest -- --nocapture
# NUMA·전용 CPU·hugepages(계획 64)
cargo test -p orbivisor-types numa                                        # 순수: serde 하위 호환(옛 JSON 바이트 그대로), CpuSet, layout·fit·fit_fixed·demand·pinning 표, 장부 exceeded, 보류 numa
cargo test -p orbivisor-qemu cmdline                                      # 명령줄: 안 쓰는 VM은 바이트 그대로, 공유 vNUMA, 전용 1G 묶기·거리, 노드마다 virtio-mem, aarch64
cargo test -p orbivisord --lib topology                                   # 노드 탐침 고정물(crates/orbivisord/tests/fixtures/topology/{x86-2s,aarch64-1n,x86-no-numa,x86-offline}), 경고, 크기 4 KB 안
cargo test -p orbivisor-cluster hosts                                     # Raft 받아들이기 검사: 노드마다 셈, 늘지 않는 쓰기, 마이그레이션 확정, any
cargo test -p orbivisor-cluster --test three_nodes concurrent_creates_and_starts_on_two_nodes_keep_the_dedicated_cpus   # 두 팔로워가 동시에 만들고 켜도 전용 CPU 4개에 4번만
cargo test -p orbivisor-reconciler --test numa                            # 노드 할당기(Mock): 전용 배치·기록·고정, /sys 빈 hugepages, 켜는 중 예약의 기다림, 다시 고정
cargo test -p orbivisor-api --test numa --test incoming                   # API(Mock 노드 셋): 검증·기능 게이트, vm.dedicated·dedicated_vcpus, host_resources 409·force, 켜기 배치, 받는 쪽 다시 고르기
# 실제 Linux(QEMU 필요): 이 호스트 탐침(JSON을 찍음), 2M hugepages 실행 중 예약·되돌리기, 스레드 친화, 실제 VM의 vCPU·에뮬레이터 고정과
# 스코프 cpuset, 2M hugepages 백엔드, 공유 VM, 동시에 켠 전용 VM 둘의 409, 실행 중 붙인 vCPU, 한 호스트의 orbivisord 둘 사이 마이그레이션 뒤
# 다시 고정. /sys에 쓰는 시험(hugepages 예약)은 root와 ORBIVISOR_TEST_NUMA_SYS=1일 때만 돈다 — 배포 서버에서 기능만 켜고 돌려도 호스트 설정을
# 바꾸지 않게. 시험이 예약한 hugepages는 끝에 원래 수로 되돌린다. CI linux-kvm은 아래처럼 root로(FEATURES에 orbivisord/numa-it, env에
# ORBIVISOR_TEST_NUMA_SYS=1, ORBIVISOR_TEST_SYSTEMD=1) 돈다. 러너는 4 vCPU·NUMA 노드 하나라 호스트 노드 둘 이상의 맞춤·1G·SMT는 고정물·Mock만 본다.
cargo test -p orbivisord --features numa-it --lib topology::real -- --nocapture
sudo -E env "PATH=$PATH" ORBIVISOR_TEST_SYSTEMD=1 ORBIVISOR_TEST_NUMA_SYS=1 CARGO_TARGET_DIR=/tmp/opl-root-target \
  cargo test -p orbivisord --features qemu-it,numa-it --lib --test numa -- --nocapture
# x86_64 Linux, KVM: 실제 게스트 — Ubuntu 24.04 클라우드 이미지를 --sockets 2 --numa 2로 켜 게스트의 /sys/devices/system/node/node*/
# cpulist·MemTotal을 보고(노드 둘, 소켓마다 CPU 하나, 메모리 반씩), 기록 numa의 모양(CI install-media 작업)
ORBIVISOR_TEST_CLOUD_IMAGE=/tmp/ubuntu-24.04-server-cloudimg-amd64.img \
  cargo test -p orbivisord --features install-media-it,qemu-it --test install_media a_guest_sees_its_numa_nodes -- --nocapture
# SDN(OVN, 계획 65)
cargo test -p orbivisor-types -- sdn ipam network                         # 순수: serde 하위 호환(옛 NetworkSpec·NicSpec·NodeStatus·RuntimeNic 바이트 그대로), 서브넷 검증 표, IPAM allocate(/16에 1만 개)
cargo test -p orbivisor-driver ovn                                        # OvnHost 단계(가짜 CommandRunner, 실제 출력 고정물 src/ovn/fixtures/), 단위 파일 기대 글, OVSDB 클라이언트(가짜 서버·실제 RPC 고정물)
cargo test -p orbivisor-cluster --test single_node an_sdn_address_belongs_to_one_nic_per_network   # Raft 주소 겹침 검사
cargo test -p orbivisor-api --test ovn_sync --test ipam --test sdn --test sdn_tasks   # OvnSync(가짜 NB: 다른 것만·펜싱·나누기·5천 포트), IPAM 경합·복제·복원, /sdn·PATCH·ips·권한, Task
cargo test -p orbivisorctl sdn                                            # sdn·network subnet·network ips·--ip 명령
pnpm -C web test src/sdn src/networks                               # 마법사·상태·서브넷 편집·IP 서랍·NIC 주소(msw)
# Linux(Ubuntu 24.04), root, OVN이 없는 버려도 되는 기계: orbivisord가 apt로 OVN을 설치하고 노드 하나에 NB·SB·northd와 chassis,
# mTLS(다른 CA·인증서 없음·평문 거절), 127.0.0.2의 두 번째 ovsdb 합류·떠나기, OvnSync·펜싱, 이름공간 포트의 busybox udhcpc와 ping·위조
# 막기, SDN NIC VM의 br-int 바인딩(ORBIVISOR_TEST_CLOUD_IMAGE가 있으면 게스트가 OVN DHCP로 IPAM 주소를 받음). OVN 꾸러미와 데몬이 남는다
# (CI `ovn` 작업). busybox와 QEMU가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target \
  ORBIVISOR_TEST_CLOUD_IMAGE=/tmp/ubuntu-24.04-server-cloudimg-amd64.img \
  cargo test -p orbivisord --features sdn-it --test sdn -- --nocapture
# SDN 라우팅(계획 66)
cargo test -p orbivisor-types -- router floating_ip sdn security_group     # 순수: 공급자·유동 IP 풀·ipv6_mode·라우터·노드 sdn 검증, eui64, gateway_ranking(HRW), nic_policy
cargo test -p orbivisord firewall::tests::the_ruleset_text_is_unchanged    # nic_policy로 옮긴 뒤에도 nftables 규칙 글이 바이트 그대로
cargo test -p orbivisor-driver physnet                                    # physnet 명령·매핑 합치기(남의 항목 보존·충돌)·빼는 차례
cargo test -p orbivisor-api ovn_sync                                      # 라우터·NAT·HA 그룹·RA·DHCPv6 행, 그룹 목록 Port_Group·ACL 띠, 실패하면 닫힘, 5천 포트 × ACL 2650
cargo test -p orbivisor-api --test sdn_routing                            # 라우터·유동 IP·공급자·노드 sdn API, 권한, 할당량, IPAM(EUI-64·풀), 409들
cargo test -p orbivisorctl routing                                        # router·fip·node sdn 명령
pnpm -C web test src/routing src/nodes src/security-groups          # 라우터 서랍·유동 IP 붙이기·노드 physnet 카드·NIC 수(msw)
# SDN 서비스(계획 67)
cargo test -p orbivisor-types -- load_balancer qos network_migration alert::sdn sdn   # 순수: LB·QoS·옮기기 검증 표, lb vips·ip_port_mappings, qos nic_rows·tc_commands(남의 필터 보존), migration plan(방식·주소 출처·막힘), allow_sets, SDN 경보 평가 표
cargo test -p orbivisor-cluster                                           # ip 겹침(LB 셋)·lb-listener·migration-source·migration-vm·migration-target, 할당량 networks·load_balancers와 바깥 VIP의 floating_ips
cargo test -p orbivisor-api ovn_sync                                      # LB·건강 검사·QoS·l2gateway 행(가짜 NB), 같은 차례 붙이기, 계획 66 고정물에 연산 0개
cargo test -p orbivisor-api --test sdn_services --test network_migrate    # 기능 게이트, 권한, LB·QoS·할당량·유동 IP 떼기·잠금 미리 확인·토폴로지 감추기, 옮기기 상태 머신(라이브 실패·재시작·이어 하기·다리 노드 지움)
cargo test -p orbivisor-driver -- relink tc ovn::lockdown units           # tap 옮기기 명령, tc 명령, nftables 잠금 글·ss 파싱, sb.json·northd 단위(끔은 계획 66 글 그대로)
cargo test -p orbivisorctl -- lb migration sdn alerts                     # lb·vm nic-qos·network migrate·sdn lockdown·alert rule add-sdn-defaults 명령
pnpm -C web test src/components/iso src/topology src/lb src/migration src/sdn src/alerts   # 투영·배치(200/50/2000 기록)·키보드·axe, LB 서랍, 마법사, 잠금 카드·SDN 경보(msw)
# 백업 저장소(계획 68) — 가짜 S3(orbivisor-store `fake-s3`)와 MockHypervisor로 S3·QEMU 없이
cargo test -p orbivisor-store                                             # 형식(청크·인덱스·매니페스트 v3 고정물), 키 계층·복구 키·escrow, 로컬·메모리·가짜 S3 백엔드 공통 시험, SigV4 공개 벡터, GC 안전 모형 proptest, Bloom 오탐률, 검증, NBD 클라이언트·서버
cargo test -p orbivisor-types backup_store                                # 스펙 검증 표, chunk_ranges·merge_index, verify::due, serde 하위 호환
cargo test -p orbivisor-api --test backup_stores --test backup_store_types   # 저장소 API·권한·비밀 가리기·읽기 전용 붙이기·scan·넘겨받기, 복사, 두 Env 원격 복제(없는 청크만), 기능 게이트
cargo test -p orbivisor-api --features storage-it --test backup_store_tasks --test live_restore_tasks   # 백업·증분·GC·검증·옮기기 Task, 라이브 복원 상태 머신(qemu-img·qemu-nbd)
cargo test -p orbivisor-store --features storage-it --test nbd_qemu       # 우리 NBD ↔ qemu-nbd·qemu-img·qemu-io, 재연결
cargo test -p orbivisor-driver --features qemu-it --test qemu_fleece --test qemu_live_restore   # 실제 QEMU 플리싱·비트맵 문맥·ENOSPC, 라이브 복원 배선·갈아타기·재연결
cargo test -p orbivisord --features backup-it --test backup_store         # E2E(orbivisorctl): 로컬 저장소 실행 중 VM 전체 → 증분 → GC·검증 → 복원·라이브 복원이 원본과 같음
cargo test -p orbivisorctl -- backup_store backup                         # backup-store·backup verify/convert/copy·vm live-restore 인자와 표
pnpm -C web test src/backupStores src/backups                       # 저장소 목록·마법사(복구 키 확인)·서랍·키 카드, 백업 표 대상·검증 배지, 라이브 복원 띠(msw, axe)
# 실제 S3 호환 저장소(아래 "백업 저장소 S3 시험"): QEMU + qemu-nbd, S3 끝점 둘(HTTP·사설 CA HTTPS)과 docker 컨테이너 이름
ORBIVISOR_S3_ENDPOINT=http://127.0.0.1:9000 ORBIVISOR_S3_TLS_ENDPOINT=https://127.0.0.1:9443 ORBIVISOR_S3_CA=/tmp/opl-s3-certs/ca.pem \
  ORBIVISOR_S3_ACCESS_KEY=… ORBIVISOR_S3_SECRET_KEY=… ORBIVISOR_S3_BUCKET=orbi ORBIVISOR_S3_CONTAINER=opl-s3 \
  cargo test -p orbivisord --features s3-it --test backup_s3 -- --nocapture --test-threads=1
# 파일 단위 복원(계획 69) — 가짜 도우미(MockHypervisor: 에이전트 몸통 + 메모리 나무)와 가짜 게스트 에이전트로 QEMU 없이
cargo test -p orbivisor-restore                                           # 통신 규약(프레임·이름·흐름 검사, 해석기 proptest), 에이전트 몸통(소켓 쌍 + 임시 디렉터리: 쪽 나눔·링크 탈출·하드 링크·취소·동시 흐름), 탐지 고정물·마운트 표, zip·tar.gz 작성기(무작위 나무 왕복)
cargo test -p orbivisor-restore --features storage-it --test archive_tools   # 만든 zip·tar.gz를 unzip -t·bsdtar·GNU tar·Python zipfile로 읽음(Debian: unzip libarchive-tools), 7만 항목 zip64
cargo test -p orbivisor-types file_restore                                # EntryPath·볼륨 id·guest_path_ok·guest_target_name·content_disposition·노드 고르기, serde 하위 호환
cargo test -p orbivisor-api --test file_restore --test file_restore_types --test two_nodes   # 세션 API·권한·토큰·감사·backup_in_use·보존·되돌려 쓰기(가짜 QGA), 기능 게이트, 다른 노드의 세션
cargo test -p orbivisorctl -- file_restore                                # backup files 경로 나누기·volumes 표·ls 쪽 넘김·ls -l
pnpm -C web test src/fileRestore src/backups                        # 볼륨 나무·파일 표·키보드·내려받기 링크·VM으로 복원 대화상자, 백업 표 동작(msw, axe)
bash packaging/tests/restore-image-test.sh                          # 이미지 빌더(가짜 deb·ELF·.ko — 네트워크·cargo 없이), 시험 게스트 갈래, 두 번 빌드 같음
# 실제 도우미 VM(아래 "파일 단위 복원 시험"): 이미지(rustup의 <arch>-unknown-linux-musl 타깃, bsdtar·zstd·python3)와 고정물 디스크
packaging/restore-image/build.sh --arch aarch64 --out /tmp/fr/img/aarch64       # x86_64 호스트면 --arch x86_64 --out /tmp/fr/img/x86_64
crates/orbivisor-restore/tests/fixtures/make-disks.sh /tmp/fr/fx                       # 있는 도구만(macOS: Homebrew e2fsprogs·dosfstools·mtools), lvm.img는 root
ORBIVISOR_RESTORE_IMAGE_DIR=/tmp/fr/img ORBIVISOR_RESTORE_FIXTURES=/tmp/fr/fx \
  cargo test -p orbivisor-driver --features restore-it --test restore_helper -- --nocapture --test-threads 1   # 탐지·SHA-256·저널 재생·덮개 한도·kill -9·치우기, PROBE 줄
ORBIVISOR_RESTORE_IMAGE_DIR=/tmp/fr/img ORBIVISOR_RESTORE_FIXTURES=/tmp/fr/fx \
  cargo test -p orbivisord --features restore-it --test file_restore -- --nocapture   # E2E (1)~(5)(orbivisorctl backup files — (5)는 ORBIVISOR_RESTORE_TEST_GUEST가 있을 때)
pnpm -C web build && ORBIVISOR_RESTORE_IMAGE_DIR=/tmp/fr/img ORBIVISOR_RESTORE_FIXTURES=/tmp/fr/fx \
  cargo test -p orbivisord --features web-it --test web browser_browses_backup_files   # 브라우저로 찾아보기·내려받기
# VM 빌더·만들기 미리 확인(계획 71)
cargo test -p orbivisor-api --test vm_checks                              # POST /vm-checks: 통과(고를 노드·아무것도 안 씀)·문제 모음과 자리·이름·머신·만들 볼륨·영역 규칙·403/404·folder_required·409·감사 없음·OpenAPI
cargo test -p orbivisor-api vm_checks                                     # area_of 코드 → 자리 표, slot_of
cargo test -p orbivisorctl -- vm_create_dry_run web_cross                 # vm create --dry-run(표·-o json·끝 코드), 웹 CLI 미리 보기 교차 시험(아래)
pnpm -C web test src/dnd src/vmBuilder src/placement/hard.test.ts   # 끌기 틀(상태 기계 사건열 성질 시험·자동 스크롤·제공자), 빌더 모델(부품·놓기 규칙 표·놓기·문제·요청·JSON·CLI·무작위 놓기)과 화면(msw, axe), hard 배치 규칙 예상
pnpm -C web test src/clusterMap/drop.test.ts src/clusterMap/ClusterMapDrag.test.tsx   # 지도 끌어 옮기기: 놓을 자리 판정 표, 마우스·터치·m 키·권한·가장자리 보기 옮기기
pnpm -C web build && cargo test -p orbivisord --features web-it --test web browser_builds_vms_with_the_builder   # 브라우저로 빌더 만들기 셋(QEMU를 켜지 않음)
# VMware 이전 2·OVA 내보내기(계획 72) — 가짜 vSphere(orbivisor-api 기능 `fake-vsphere`: 스냅샷 사슬·CBT·게스트 쓰기·호스트 티켓·결함 주입)
cargo test -p orbivisor-types -- vmware vm_export                         # 순수: serde 하위 호환(빈 값 안 씀), 비밀번호 가림, 검증 표, 항목·스냅샷 이름
cargo test -p orbivisor-api --lib -- vmware vm_imports::ovf_write vm_exports   # SOAP 봉투·결함, 인벤토리·CBT 구역, 매핑·VMware 쪽 표, 스케줄러 순수 plan, OVF 쓰개 ↔ 계획 29 읽개, ustar
# API 시험(가짜 스토리지 — qemu 없이). PATH에 qemu-img·qemu-nbd가 없어야 가짜 길을 탄다(CI check에도 qemu-img가 없다).
PATH=/usr/bin:/bin cargo test -p orbivisor-api --features fake-vsphere \
  --test vsphere --test vmware_api --test vmware_sync --test vmware_cutover   # 클라이언트·원본/계획 API·회차/스케줄러·전환·되돌림·예약 전환, 끝에 원본 안전 단언
cargo test -p orbivisor-api --features fake-vsphere,storage-it --test vmware_sync qemu_nbd_writes_the_target_volumes   # 실제 qemu-nbd 쓰기 대상
cargo test -p orbivisor-api --features storage-it --test vm_import_tasks an_exported_ova_imports_back_with_the_same_disk   # 내보내기 → OVA 가져오기 왕복(qemu-img compare)
cargo test -p orbivisor-api --test vm_exports                             # 내보내기 API·링크·권한(가짜 스토리지)
cargo run -p orbivisor-api --features fake-vsphere --bin fake-vsphere -- --port 8989   # 손으로 띄우는 가짜 vCenter(연결 정보 JSON 한 줄 — svc01 시연·웹 개발)
# govmomi vcsim(SOAP 꼴, GitHub `vmware-sim` 작업 — vcsim·govc v0.56.2 실행 파일, nbdkit). vcsim은 QueryChangedDiskAreas가 없어 CBT는 가짜만 본다.
vcsim -l 127.0.0.1:8989 -username user -password pass &
ORBIVISOR_TEST_VCSIM=127.0.0.1:8989 ORBIVISOR_TEST_VCSIM_USER=user ORBIVISOR_TEST_VCSIM_PASSWORD=pass ORBIVISOR_TEST_NBDKIT=$(command -v nbdkit) \
  cargo test -p orbivisor-api --features vcsim-it --test vcsim -- --nocapture --test-threads=1
# 실제 orbivisord(QEMU·qemu-img·qemu-nbd): 따뜻한 이전(회차 사이 쓰기·델타 둘·전환·볼륨 바이트 같음·원본 꺼짐·NIC 끊김·revert), 차가운 한 대·
# 마무리, 내보내기 → 내려받은 OVA(ustar) → OVA 가져오기 왕복. `[test] vmware_tick_ms`·`vmware_sync_secs`로 스케줄러·델타 주기를 줄인다.
cargo test -p orbivisord --features qemu-it,fake-vsphere --test vmware_migration
pnpm -C web test src/vmware src/exports                             # 원본 지문 흐름·비밀번호가 저장소에 없음, 나무·찾기, 마법사, 상세·서랍·전환·되돌리기, 내보내기(msw, axe)
pnpm -C web build && cargo test -p orbivisord --features web-it --test web browser_migrates_a_vmware_vm_and_exports_it   # 브라우저: 원본 → 마법사 → 전환 → VM 링크 → OVA 내려받기
# 선언형 운영(계획 73) — 적용·설정 기록·되돌리기·드리프트·ACME. API 시험은 FakeStorage(PATH에 qemu-img가 없어야 가짜 길)
cargo test -p orbivisor-types -- manifest apply_set history acme                # 순수: 매니페스트·비밀 값, 이력 가림(history_mask)·사유·출처, ACME 스펙 검증·가림
cargo test -p orbivisor-cluster --test history                                  # 출처를 단 명령(Attributed)·이력 표·보존·스냅샷 칸
cargo test --release -p orbivisor-cluster --test history -- --ignored --nocapture   # 쓰기 길 비용(노드 하나, update_spec 300번 — 기록 +1.3%)
cargo test --release -p orbivisor-cluster --lib -- --ignored history_overhead --nocapture   # 상태 머신 1000번 바꾸기(파일 +1.0%)
PATH=/usr/bin:/bin cargo test -p orbivisor-api --test declarative --test declarative_kinds_a --test declarative_kinds_b \
  --test history_api --test config_history --test acme_api               # 계획·막힘·저장한 계획·가지치기·충돌, 종류 스물다섯 왕복, 이력·되돌리기·드리프트, ACME API·HTTP-01·SNI
cargo test -p orbivisor-api --lib -- declarative history drift acme            # diff·overlay proptest, 시점 판단, JWS·EAB·ARI id·DNS 메시지·TSIG·훅 검사·due·plan
cargo test -p orbivisorctl -- declarative history cert web_cross               # apply·plan·diff 끝 코드·export 파일·크기·secretFrom, history·restore·drift, cert, 웹 YAML 쓰개 ↔ CLI 읽개
# 실제 QEMU, 노드 둘: examples/declarative/web/ apply → diff 0 → plan(restart 표시) → apply --reason → 다른 노드에서 웹처럼 바꿈 →
# drift(누가) → restore --revision → export 왕복 diff 0 → --minimal을 다른 묶음으로 → restore --at(폴더) → apply --prune --prune-data
cargo test -p orbivisord --features qemu-it --test declarative
pnpm -C web test src/history src/declarative src/certs                    # 설정 기록·서랍·되돌리기·시점, 선언 묶음·adopt, ⋯ 메뉴·YAML·Terraform 칩, 인증서(msw, axe)
pnpm -C web build && cargo test -p orbivisord --features web-it --test web browser_restores_a_vm_from_its_config_history   # 브라우저: 메모리 바꿈 → ⋯ 설정 기록 → 되돌리기
# ACME 실제(아래 "ACME 시험"): Pebble·challtestsrv(v2.10.1), 있으면 BIND(RFC 2136)
ORBIVISOR_TEST_PEBBLE_DIR=/tmp/pebble-bin cargo test -p orbivisor-api --features acme-it --test acme -- --nocapture
eval "$(.github/scripts/acme-bind.sh "$(command -v named)" /tmp/opl-bind 15353)"   # ORBIVISOR_TEST_BIND·ORBIVISOR_TEST_BIND_KEY를 내보낸다
ORBIVISOR_TEST_PEBBLE_DIR=/tmp/pebble-bin ORBIVISOR_TEST_BIND=$ORBIVISOR_TEST_BIND ORBIVISOR_TEST_BIND_KEY=$ORBIVISOR_TEST_BIND_KEY \
  cargo test -p orbivisor-api --features acme-it --test acme -- --nocapture
# 개발 서버(Terraform 수용 시험·웹 데모): 노드 하나, 가짜 하이퍼바이저·메모리 스토리지, 연결 정보 JSON 한 줄
cargo run -p orbivisord --features devserver --bin orbivisor-devserver -- --port 8006   # --dir DIR(없으면 임시, 끝날 때 지움), --exit-on-stdin-eof
cargo test -p orbivisord --features devserver --test devserver
# Terraform/OpenTofu 공급자(Go, 아래 "Terraform 공급자 시험")
cargo run -q -p orbivisor-api --bin orbivisor-api-openapi > /tmp/openapi.json && cargo build -p orbivisord --features devserver --bin orbivisor-devserver
cd terraform && go vet ./... && ORBIVISOR_OPENAPI=/tmp/openapi.json go test ./...    # 단위와 스키마 맞춤(OpenAPI의 Manifest{Kind})
TF_ACC=1 TF_ACC_TERRAFORM_PATH=$(command -v terraform) ORBIVISOR_DEVSERVER=$PWD/../target/debug/orbivisor-devserver \
  go test ./internal/provider/ -count=1 -v -timeout 30m                 # 수용 시험 여덟(terraform)
TF_ACC=1 TF_ACC_TERRAFORM_PATH=$(command -v tofu) TF_ACC_PROVIDER_HOST=registry.opentofu.org ORBIVISOR_DEVSERVER=$PWD/../target/debug/orbivisor-devserver \
  go test ./internal/provider/ -count=1 -v -timeout 30m                 # 같은 시험을 OpenTofu로
# 여러 노드 OVN(아래 "여러 노드 SDN 시험"): Linux, root, docker, 커널 모듈 openvswitch·geneve·vxlan
sudo modprobe -a openvswitch geneve vxlan && docker build -t opl-sdn-node ci/sdn-node
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target ORBIVISOR_MULTINODE_IMAGE=opl-sdn-node \
  cargo test -p orbivisord --features sdn-multinode-it --test sdn_multinode -- --nocapture
# Linux, root: 실제 tap과 브리지
# root가 만든 빌드 결과가 ./target에 섞이지 않게 따로 둔다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p orbivisor-driver --features qemu-it,net-it   # tap·브리지·VLAN, 이름공간 사이 VXLAN, nft 방화벽(nftables·python3 필요)
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p orbivisord --features qemu-it,net-it --test bridge  # E2E: 브리지 네트워크
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p orbivisord --features qemu-it,net-it --test vxlan   # E2E: VXLAN 네트워크
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p orbivisord --features qemu-it,net-it --test firewall  # E2E: VM 방화벽(nftables 필요)
# Linux, root: 실제 NFS 풀. nfs-kernel-server로 127.0.0.1에 rw,no_root_squash로 내보낸 디렉터리가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target ORBIVISOR_TEST_NFS_EXPORT=/srv/opl-nfs cargo test -p orbivisord --features nfs-it --test nfs
# Linux, root: 실제 LVM-thin 풀. 테스트가 LV를 만들고 지워도 되는 VG/thin pool과 lvm2, qemu-io가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target ORBIVISOR_TEST_LVM=opltest/tp cargo test -p orbivisor-driver --features lvm-it --test lvm_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target ORBIVISOR_TEST_LVM=opltest/tp cargo test -p orbivisord --features lvm-it --test lvm
# Linux, root: 실제 ZFS 풀. 테스트가 zvol을 만들고 지워도 되는 데이터셋과 zfsutils-linux, qemu-io가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target ORBIVISOR_TEST_ZFS=opltestz/opl cargo test -p orbivisor-driver --features zfs-it --test zfs_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target ORBIVISOR_TEST_ZFS=opltestz/opl cargo test -p orbivisord --features zfs-it --test zfs
# Linux, root: 실제 Ceph RBD 풀. 테스트가 이미지를 만들고 지워도 되는 Ceph 풀과 ceph-common, qemu-block-extra, qemu-io,
# client.admin으로 접속할 수 있는 /etc/ceph/ceph.conf·키링이 필요하다. 제한된 사용자 시험은 아래 세 변수가 있을 때만 돈다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target ORBIVISOR_TEST_RBD=rbd \
  ORBIVISOR_TEST_RBD_USER=opltest ORBIVISOR_TEST_RBD_NAMESPACE=opl ORBIVISOR_TEST_RBD_CONF=/etc/orbivisor-ceph/ceph.conf \
  cargo test -p orbivisor-driver --features rbd-it --test rbd_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target ORBIVISOR_TEST_RBD=rbd cargo test -p orbivisord --features rbd-it --test rbd
# Linux(Ubuntu 24.04), root, Ceph가 없는 버려도 되는 기계: orbivisord가 apt로 Ceph를 설치하고 노드 하나에 mon·mgr, 시험이 만든
# 루프 장치에 OSD, 풀 등록, 그 풀의 디스크로 VM 켜기(Ceph 꾸러미와 데몬이 남는다 — CI `ceph` 작업)
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p orbivisord --features ceph-it,qemu-it --test ceph -- --nocapture
# Linux, root: 실제 공유 LVM 풀. 테스트가 LV를 만들고 지워도 되는, 잠금을 시작한 공유(lvmlockd) VG와 lvm2-lockd, sanlock,
# qemu-io가 필요하다. ORBIVISOR_TEST_ISCSI(포털,타깃)가 있으면 orbivisord 시험은 VG가 그 타깃의 LUN 위에 있다고 본다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target ORBIVISOR_TEST_LVM_SHARED=oplshared \
  cargo test -p orbivisor-driver --features lvm-shared-it --test lvm_shared_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target ORBIVISOR_TEST_LVM_SHARED=oplshared \
  ORBIVISOR_TEST_ISCSI=127.0.0.1,iqn.2003-01.org.linux-iscsi.opl:shared cargo test -p orbivisord --features lvm-shared-it --test lvm_shared
# 실제 라이브 마이그레이션: 한 호스트의 orbivisord 둘, 공유 디렉터리 풀과 노드마다 다른 경로의 로컬 디렉터리 풀(디스크 복사,
# macOS는 TCG로 돈다). nfs-it·rbd-it·lvm-shared-it와 그 환경 변수를 함께 주면(Linux, root) 그 풀로도 옮기고, lvm-it·zfs-it와
# 함께 주면 로컬 디스크를 디렉터리 풀 → LVM-thin·ZFS 풀 → 디렉터리 풀로 옮긴다.
cargo test -p orbivisord --features migration-it --test migration
# 실제 HA 복구: 한 호스트의 orbivisord 둘(process 펜싱)과 위트니스, 공유 디렉터리 풀(macOS는 TCG로 돈다).
cargo test -p orbivisord --features ha-it --test ha
# Linux(Ubuntu·Debian), root: 실제 OpenLDAP. ldap-setup.sh가 이 기계의 slapd 설정과 디렉터리를 지우고 늘 새 데이터베이스로
# 다시 설정한다(다시 돌려도 된다 — 옛 데이터베이스는 /var/backups로 옮겨짐). 버려도 되는 기계에서만: CI(`CI=true`)가 아니면
# ORBIVISOR_LDAP_SETUP_WIPE=1을 주어야 돌고, 없으면 설명을 찍고 끝난다(CI `ldap` 작업).
# 스크립트와 시험이 같은 값을 읽는다(아래는 CI ldap 작업의 값. 스크립트는 없으면 이 값을 쓰지만 시험은 모두 있어야 한다).
export ORBIVISOR_TEST_LDAP_URL=ldaps://127.0.0.1:636 ORBIVISOR_TEST_LDAP_STARTTLS_URL=ldap://127.0.0.1:389 \
  ORBIVISOR_TEST_LDAP_CA=/tmp/opl-test-ldap-ca.pem ORBIVISOR_TEST_LDAP_BASE_DN=dc=example,dc=com \
  ORBIVISOR_TEST_LDAP_BIND_DN=cn=admin,dc=example,dc=com ORBIVISOR_TEST_LDAP_BIND_PASSWORD=opl-admin-pw \
  ORBIVISOR_TEST_LDAP_USER=alice ORBIVISOR_TEST_LDAP_USER_PASSWORD=alice-pw ORBIVISOR_TEST_LDAP_OTHER_USER=bob \
  ORBIVISOR_TEST_LDAP_OTHER_PASSWORD=bob-pw ORBIVISOR_TEST_LDAP_GROUP=cn=admins,ou=groups,dc=example,dc=com
sudo -E ORBIVISOR_LDAP_SETUP_WIPE=1 sh .github/scripts/ldap-setup.sh
cargo test -p orbivisor-api --features ldap-it --test ldap_it
```

`nfs-it` 시험은 orbivisord가 내보내기를 마운트하고, 그 풀에 볼륨을 만들고 qcow2 스냅샷을 찍고 지운 뒤, 풀을 지우면
언마운트하는지 본다(CI `linux-kvm` 작업이 `/srv/opl-nfs`를 내보낸다). **내보내기는 버려도 되는 디렉터리**를 쓴다.
시험은 실패해도 임시 디렉터리를 지우기 전에 orbivisord를 멈추고 마운트를 풀며, 풀지 못하면 임시 디렉터리를 지우지 않고
남긴다(그 경로를 출력한다). NFS 서버를 막 시작했으면 잠금 유예(약 90초)
때문에 첫 실행이 느릴 수 있다.

`lvm-it` 시험은 `ORBIVISOR_TEST_LVM`의 thin pool에 `opl-` LV를 만들고 지운다. 드라이버 시험은 만들기·가져오기·스냅샷·
되돌리기·복사·지우기를, orbivisord 시험은 LVM 디스크로 VM을 띄워 실행 중 스냅샷, 멈춘 뒤 되돌리기, 디렉터리 풀로 복제,
지우기를 본다. **버려도 되는 VG**를 쓴다. CI `linux-kvm` 작업은 루프 장치 위에 VG `opltest`와 thin pool `tp`를 둔다:

```bash
truncate -s 2G /tmp/opltest.img
LOOP=$(sudo losetup --find --show /tmp/opltest.img)
sudo pvcreate -y "$LOOP" && sudo vgcreate opltest "$LOOP" && sudo lvcreate -y -T -L 1G opltest/tp
# 끝나면: sudo vgremove -y opltest && sudo pvremove "$LOOP" && sudo losetup -d "$LOOP"
```

`zfs-it` 시험은 `ORBIVISOR_TEST_ZFS`의 데이터셋에 `opl-` zvol을 만들고 지운다. 드라이버 시험은 만들기·정보·크기·
가져오기·스냅샷(가장 최근으로만 되돌리기, 여러 볼륨을 한 명령으로)·복사·지우기를, orbivisord 시험은 ZFS 디스크로 VM을
띄워 실행 중 스냅샷, 최근이 아닌 스냅샷 되돌리기의 409, 멈춘 뒤 최근 스냅샷으로 되돌리기, 디렉터리 풀로 복제,
지우기를 본다. **버려도 되는 데이터셋**을 쓴다. CI `linux-kvm` 작업은 파일 위에 zpool `opltestz`와 데이터셋
`opltestz/opl`을 둔다:

```bash
sudo modprobe zfs
truncate -s 2G /tmp/opltestz.img
sudo zpool create opltestz /tmp/opltestz.img && sudo zfs create opltestz/opl
# 끝나면: sudo zpool destroy opltestz && rm /tmp/opltestz.img
```

`rbd-it` 시험은 `ORBIVISOR_TEST_RBD`의 Ceph 풀에 `opl-` 이미지를 만들고 지운다. 드라이버 시험은 만들기·정보·크기·
가져오기·스냅샷(더 오래된 스냅샷으로 되돌리기)·백엔드 안팎의 복사·지우기와 제한된 사용자·네임스페이스·명시적
conf로 접속하기를, orbivisord 시험은 RBD 디스크로 VM을 띄워
실행 중 스냅샷, 메모리 스냅샷의 409, 멈춘 뒤 앞 스냅샷으로 되돌렸다가 다시 최근 스냅샷으로 되돌리기, 디렉터리
풀로 복제, 지우기를 본다. **버려도 되는 Ceph 풀**을 쓴다. CI `linux-kvm` 작업은 microceph로 노드 하나짜리 Ceph를
띄우고 풀 `rbd`를 둔다(준비에 1–2분):

```bash
sudo apt install ceph-common qemu-block-extra
sudo snap install microceph --channel squid/stable && sudo microceph cluster bootstrap
sudo snap unalias microceph                          # 별칭(/snap/bin/rbd 등)은 snap에 갇혀 /etc/orbivisor-ceph·/tmp를 못 본다
sudo mkdir -p /etc/ceph && sudo ln -sf /var/snap/microceph/current/conf/ceph.conf /etc/ceph/ceph.conf
sudo sh -c 'ln -sf /var/snap/microceph/current/conf/*.keyring /etc/ceph/'
sudo ceph config set global mon_allow_pool_size_one true && sudo ceph config set global osd_pool_default_size 1
sudo microceph disk add loop,4G,1                     # OSD가 올라올 때까지 기다린다(sudo ceph osd stat)
sudo ceph osd pool create rbd 32 && sudo ceph osd pool set rbd size 1 --yes-i-really-mean-it
sudo rbd pool init rbd                                # PG가 active가 된 뒤(sudo ceph -s)
# 제한된 사용자 시험: 네임스페이스와 사용자, 그 키링만 가리키는 conf
sudo rbd namespace create rbd/opl && sudo mkdir -p /etc/orbivisor-ceph
sudo ceph auth get-or-create client.opltest mon 'profile rbd' osd 'profile rbd pool=rbd' mgr 'profile rbd pool=rbd' \
  | sudo tee /etc/orbivisor-ceph/opltest.keyring >/dev/null
{ sudo cat /etc/ceph/ceph.conf; printf '\n[client.opltest]\nkeyring = /etc/orbivisor-ceph/opltest.keyring\n'; } \
  | sudo tee /etc/orbivisor-ceph/ceph.conf >/dev/null
# 끝나면: sudo snap remove --purge microceph
```

`lvm-shared-it` 시험은 `ORBIVISOR_TEST_LVM_SHARED`의 공유 VG에 `opl-` LV를 만들고 지운다. 드라이버 시험은 만들기(0
채우기)·배타 활성화와 끄기·연 장치는 끌 수 없음·크기(늘린 부분의 0 채우기)·가져오기와 실패한 가져오기 정리·백엔드 안팎의 복사·지우기를,
orbivisord 시험은 orbivisord가 iSCSI 로그인과 잠금 시작부터 해서 풀을 준비하는지(`ORBIVISOR_TEST_ISCSI`가 있으면 시험이 먼저 잠금을
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

`sdn-it` 시험(`crates/orbivisord/tests/sdn.rs`, 계획 65)은 **OVN이 깔린 적 없는 기계**에서 root로 돈다. orbivisord가 `sdn init`·
`sdn chassis install`(API와 orbivisorctl)로 `ovn-central`·`ovn-host`·`openvswitch-switch`를 apt로 깔고, 배포판 중앙 단위를 끄고
가린 뒤 Orbivisor 단위(`orbivisor-ovn-nb`·`-sb`·`-northd`)를 띄운다. 시험은 NB·SB가 노드 주소의 `pssl`에만 듣는지(`ss -ltn`),
클러스터 CA 인증서로만 붙는지, SB `Chassis`가 노드 UUID·Geneve·underlay인지, 127.0.0.2의 `ovsdb-server` 한 쌍의 합류·떠나기,
리더 OvnSync의 NB 행·펜싱·다시 맞추기·지운 노드의 SB chassis 청소, IPAM 주소와 OVN DHCP(이름공간 둘의 OVS 내부 포트에서
busybox `udhcpc`, 서로 ping, 위조 주소는 막힘), SDN NIC VM의 `br-int` tap·`ovn-installed`·SB 바인딩, `network ips`의 포트 상태,
orbivisord 다시 띄우기를 본다. 시험은 **호스트에 OVN 꾸러미·데몬·`/var/lib/orbivisor/ovn`을 남긴다** — 그래서 GitHub의 깨끗한 Ubuntu
24.04 러너에서만 돌린다. CI `ovn` 작업은 QEMU·OVMF·busybox만 미리 깔고 `/etc/ovn`·`/etc/openvswitch`와 OVN 꾸러미가 없는지
확인한 뒤(OVN은 orbivisord가 설치), Ubuntu 24.04 클라우드 이미지를 받아 `ORBIVISOR_TEST_CLOUD_IMAGE`로 넘기고
`cargo test --features orbivisord/sdn-it -p orbivisord --test sdn`을 root로 돌린다(빌드 2분쯤, 시험 2~3분). 러너 하나는 OVS 하나라 chassis
둘 사이의 Geneve와 라이브 마이그레이션의 두 chassis 바인딩은 여기서 보지 않는다(아래 `sdn-multinode`가 본다).
계획 66부터 같은 시험이 러너 호스트에 Linux 브리지 `oplext0`와 "물리 라우터" 이름공간 `ext`(203.0.113.1)를 만들고 노드 physnet
`ext=oplext0`·공급자 네트워크(localnet ping), 라우터의 DHCP 기본 경로·동서 ping·SNAT(`tcpdump`의 보낸 주소)·중앙 유동 IP,
바깥에서 들어오는 DF 1500 패킷(조용히 버려짐), RA SLAAC(EUI-64)·상태 있는 DHCPv6(busybox `udhcpc6`)·클라우드 이미지 게스트의
`ipv6_slaac`, 그리고 **보안 그룹의 같은 뜻**(같은 그룹을 건 nftables 이름공간과 OVN 이름공간의 탐침 표, 처음 맞는 규칙, 목록을
바꾸는 동안의 연속 탐침, 허용을 빼면 OVN만 맺은 연결이 끊김)을 본다. 작업이 `tcpdump`·`nftables`·`python3`를 깐다.
계획 67부터 `ovn` 작업은 세운 직후 잠금 모양(SB `sb.json`·6645)을 보고 `sdn lockdown off`로 이전 절을 돈 뒤(127.0.0.2의 두 번째 ovsdb는
Orbivisor 중앙이 아니라 허용 목록에 막히므로), 새 절 넷을 CLI·API로 돈다: **부하 분산**(`services_section` — 이름공간 멤버 둘·같은 서브넷과
다른 서브넷 클라이언트가 VIP로 두 멤버에 나뉨, 바깥을 뗀 라우터, 출발지 고정, 건강 검사 넘김 시간·되살림·둘 다 죽으면 거절, `lb get`의
건강, 바깥 VIP와 멤버가 본 출발 주소, `lb_in_use`), **QoS**(같은 절 — iperf3 `-O 3`으로 `out`·`in`·실행 중 바꿈 ±15%, DSCP 46의 `tos 0xb8`;
`plain_tc_section` — veth를 tap 자리로 드라이버 tc 명령의 속도·다시 맞추기·남의 필터), **옮기기**(`migration_section` — Linux 브리지 `oplmig0`의
`lan` → `t-lan`, `--dry-run` 계획, QEMU VM 라이브 tap 옮기기(PID 그대로)·이름공간 VM `offline`, 다리 너머 ping 두 방향, 되돌리기, 끝내기로
`l2gateway`·`mig…` 매핑·veth가 사라짐), **잠금**(`lockdown_section` — 노드 인증서로 역할 포트에서 남의 `Chassis`·`SB_Global` 쓰기가 RBAC 오류,
자기 행·6645는 됨, `--wait=hv`, 127.0.0.3 출발은 6641·6642·6645 시간 초과, 끄면 단위 글이 계획 66 그대로). 작업이 `iperf3`도 깐다.
`linux-kvm`의 `net-it`는 실제 tc로 `tap_rate_limits_use_tc_police`를 본다.

**여러 노드 SDN 시험**(`crates/orbivisord/tests/sdn_multinode.rs`, 기능 `sdn-multinode-it`, 계획 66)은 한 Linux 호스트에 **privileged
systemd 컨테이너 셋**(`opl-c1`~`c3`)을 Orbivisor 노드로 띄운다. 이미지 `ci/sdn-node/Dockerfile`은 `ubuntu:24.04`에 systemd·
iproute2·nftables·qemu-system-x86·ovmf·qemu-utils·tcpdump·busybox·python3만 넣고 **OVN·OVS는 넣지 않는다** — orbivisord가
`sdn init`·`sdn central add`·`sdn chassis install`로 apt에서 깐다(시험만을 위한 코드 길이 없다). 컨테이너마다 자기 netns·systemd·
OVS·ovn-controller가 돌아 진짜 chassis 셋이 되고, 커널 모듈(`openvswitch`·`geneve`)만 호스트와 함께 쓴다 — 그래서 호스트가 미리
`modprobe`한다(컨테이너 안 `ovs-ctl`은 올라온 모듈을 보고 넘어간다). 컨테이너는 `--privileged --cgroupns=private --tmpfs /run`,
`/lib/modules` 읽기 전용, 호스트에서 빌드한 orbivisord 바이너리와 노드 디렉터리(`/var/tmp/oplmn/<노드>` — 설정·데이터·제어 소켓·
로그)를 같은 경로로 바인드 마운트하고 `systemd-run --unit=orbivisord`로 띄운다(설정: `[sdn] refresh_secs = 30`, 마이그레이션
`bandwidth_mib = 1`, `systemd_scope = false`). 망은 도커 망 `oplmgmt`(172.30.66.0/24, 노드 .11~.13 — Raft·OVN·Geneve), 호스트
브리지 `oplext0` + 노드마다 veth(호스트 쪽 `oplv<i>`, 컨테이너 안 Linux 브리지 `vmbr1`) + 호스트 이름공간 `ext`(203.0.113.1)다.
호스트의 orbivisorctl이 바인드 마운트된 제어 소켓(`cluster init`·`join`)과 노드 API로 모든 것을 만든다. 차례: 클러스터 셋 →
OVN(NB·SB Raft 서버 셋, SB chassis 셋, Geneve) → 라우팅(physnet, 게이트웨이 c2=200·c3=100, `ext`·`t1`·`t2`·`r1`, chassis 사이 동서가
c2의 Geneve에 없음, SNAT가 c2 veth로만, 게스트 없는 QEMU VM의 분산 유동 IP와 physnet을 빼면 중앙) → 두 chassis 마이그레이션
(c1 → c3, NB `requested-chassis="c1,c3"`·`rarp`, SB 바인딩 c3, 유동 IP가 c3 veth로) → 게이트웨이 넘김(`systemctl stop
ovn-controller`, `Restart=no` 덧붙이기를 얹은 `kill -9`, 되돌아감 — 시간을 찍음) → 노드 통째 죽음(`docker kill c2`, 남은 둘로 새
유동 IP, `sdn central remove c2` → 실제 `cluster/kick`, 서버 둘). 계획 67부터 세우기·중앙 더하기는 **잠금을 켠 채** 돌고, 새 절이 더해졌다:
잠금 끄기·켜기 동안 VM ping(잃은 패킷 0), **여러 chassis 보안 그룹**(c1 VM → c3 VM이 그룹대로 막히고 허용 그룹이면 됨, `ext` → 유동 IP도 그룹대로),
경보(호스트의 작은 webhook 서버 — 도커 망 게이트웨이 172.30.66.1 — 로 받고 지속 시간 5초), 게이트웨이 후보 모두 빼기(`sdn_gateway_unavailable`·
해제), **여러 chassis LB**(멤버 c1·c3, 클라이언트 c2 — 나눔, 멤버 듣기 멈춤 넘김과 `sdn_lb_member_down`, 바깥 VIP의 게이트웨이 넘김과 GARP,
`sdn_gateway_failover`·`sdn_chassis_down`, Geneve 너머 QoS 측정), **VXLAN 원본 옮기기**(다리 c1 → 남은 c3 이름공간 ping, 모두 되돌린 뒤
`--bridge-node c2`로 다시, 끝내기), 중앙 죽이기의 `sdn_central_down`, 끝에 **지운 노드 끊기**(죽인 c2를 orbivisord 없이 다시 띄워 `cluster
remove-node` 뒤 c2 인증서가 c1·c3의 6641·6642에 붙지 못하기까지 시간). 이미지에 `iperf3`, 호스트가 `vxlan` 모듈도 올린다. 실패하면 진단(노드마다 `ovs-vsctl`·`ovn-sbctl`·`ovn-nbctl show`, 단위 상태, journal, orbivisord 로그 끝)을 찍고,
끝나거나 실패해도 컨테이너·도커 망·브리지·이름공간을 지운다. 필요한 것: Linux(Ubuntu 24.04) root, docker, tcpdump, 컨테이너가 apt에
닿는 망. `/dev/kvm`이 있으면 컨테이너의 QEMU가 쓴다(없으면 TCG). CI `sdn-multinode` 작업(Ubuntu 24.04 러너, `timeout-minutes: 60`)은
`modprobe -a openvswitch geneve`, `docker build -t opl-sdn-node ci/sdn-node`, 위 `cargo test`를 root로(캐시 없이 `$RUNNER_TEMP/target`)
돌린다. 시험은 계획 66 때 약 170초였고 계획 67의 절로 늘었다(설계 어림 약 6분 더 — `timeout-minutes: 60` 그대로, OVN 24.03).

**백업 저장소 S3 시험**(`crates/orbivisord/tests/backup_s3.rs`, 기능 `s3-it`, 계획 68)은 실제 QEMU VM을 실제 S3 호환 저장소에 orbivisorctl로
백업한다: (1) 암호화 S3 저장소에 실행 중 VM 백업 다섯 번과 GC 세 번을 함께 → 모든 백업 검증 통과, 저장소의 모든 객체에 디스크의
평문 표식이 없음, 복원이 같음, 처리량 `PROBE`, (2) 사설 CA HTTPS — CA 없이는 초기화 실패, `--ca-file`이면 백업·복원, (3) 512 MiB
디스크: 백업 도중 저장소 `docker pause` 20초 → 백업 성공, 라이브 복원 도중 멈춤 → 헬퍼 `kill -9` → orbivisord 다시 시작 → 풀면
끝나고 대상이 원본과 같음, 90초 멈춤 뒤에도 끝남, (4) 두 클러스터 DR — prod 암호화 저장소(escrow) → dr 읽기 전용 붙이기·scan·라이브
복원, 읽기 전용 백업 거절, 넘겨받기(하트비트 새로움 거절 → `--force`), prod GC가 "taken over"로 멈춤. 제품은 어떤 S3 호환 저장소와도
쓰지만, CI `backup-s3` 작업(Ubuntu 24.04 러너, KVM, root 아님, `timeout-minutes: 75`)은 **SeaweedFS**(`chrislusf/seaweedfs:4.50`)
컨테이너 둘(HTTP `:9000`, 시험 CA로 서명한 `127.0.0.1` 인증서의 HTTPS `:9443`)을 띄우고 버킷을 러너의 AWS CLI로 만든 뒤 위
`cargo test`를 돌린다(MinIO는 2025년 가을부터 이미지·바이너리를 내지 않아 바꿨다). 로컬에서 돌리려면 같은 컨테이너를 띄우고
`ORBIVISOR_S3_CONTAINER`에 HTTP 쪽 컨테이너 이름을 준다(멈춤 시험이 `docker pause`한다). svc 배포 서버에는 S3가 없어 이 시험은 GitHub에서만 돈다.

**파일 단위 복원 시험**(계획 69)은 실제 도우미 이미지와 고정물 디스크가 있어야 돈다 — `ORBIVISOR_RESTORE_IMAGE_DIR`(그 아래
`<arch>/manifest.json`, `packaging/restore-image/build.sh`가 만듦)와 `ORBIVISOR_RESTORE_FIXTURES`(`make-disks.sh`가 만든 이미지·`expected.sha256`)가
없으면 건너뛴다. 이미지 판은 시험 빌드의 orbivisord 판과 같아야 하므로 같은 소스 트리에서 만든다. `crates/orbivisor-driver/tests/restore_helper.rs`
(`restore-it` = `qemu-it`)는 도우미를 직접 띄워 고정물 11개(ext4는 orbivisor-store NBD 서버 원본, 나머지 qcow2 원본)의 탐지 표·파일마다 SHA-256·
더러운 ext4·XFS 저널 재생·같은 UUID XFS 둘·MBR·GPT·LUKS "not supported"·LVM 선형·씬, 덮개 한도, `kill -9`, 버려 둔 도우미의 치우기를 보고,
부팅 시간·흘리기 MB/s를 `PROBE` 줄로 찍는다. 고르는 것: `ORBIVISOR_RESTORE_QEMU`·`ORBIVISOR_RESTORE_QEMU_IMG`(바이너리), `ORBIVISOR_RESTORE_ACCEL=tcg`(가속
강제), `ORBIVISOR_RESTORE_BIG_MIB`(흘리기 크기, 기본 256), `ORBIVISOR_RESTORE_SCOPE=1`(systemd scope). Linux에서 root로 돌리면 `-run-with user=nobody`
뒤 QEMU uid와 seccomp도 본다. XFS 더러운 로그 고정물(`xfs-dirty.img`)은 root가 만든다(CI `file-restore` 작업의 "Fixture disks" 걸음 —
루프 마운트, 파일 쓰기·fsync, `xfs_io -x -c shutdown`). `crates/orbivisord/tests/file_restore.rs`(`restore-it` = `backup-it`, unzip·tar 필요)는
orbivisorctl `backup files`로 (1) 고정물 ext4 VM의 암호화 로컬 저장소 백업 — 나열·raw·zip·tar.gz를 고정물과 견주고 닫은 뒤 도우미·세션
디렉터리가 남지 않음, (2) 옛 풀 전체 → 더러운 저널 증분 체인, (3) 도우미 `kill -9`, (4) 세션이 열린 채 orbivisord `SIGKILL` 뒤 치우기, (5) 실제
게스트 에이전트로 되돌려 쓰기를 본다. (5)는 `build.sh --test-guest --out <dir>`(같은 커널에 Debian `qemu-guest-agent`·정적 busybox,
`test-guest.lock`)로 만든 시험 게스트를 `ORBIVISOR_RESTORE_TEST_GUEST`로 주면 돈다 — 시험이 QEMU 감싸개로 그 VM에만 `-kernel`·`-initrd`를 더한다.
QEMU가 PATH 밖이면(svc01의 `/opt/qemu`) `ORBIVISOR_TEST_QEMU`·`ORBIVISOR_TEST_ACCEL`(기본 `tcg`)로, systemd scope는 `ORBIVISOR_TEST_SYSTEMD=1`로, 배포판
펌웨어가 `-kernel` 게스트를 띄우지 못하면(svc01 Rocky AAVMF) `ORBIVISOR_TEST_UEFI_CODE`·`ORBIVISOR_TEST_UEFI_VARS`로 QEMU에 딸린 edk2를 준다.
macOS 유닉스 소켓 경로는 104바이트 안이어야 하므로 이미지·고정물·임시 디렉터리를 짧은 경로에 둔다. 웹 E2E `browser_browses_backup_files`
(`web/e2e/file-restore.spec.ts`)도 같은 두 변수가 있을 때만 돌고, `ORBIVISOR_SCREENSHOTS=<디렉터리>`면 화면 `file-restore.png`를 남긴다.
CI **`file-restore`** 작업(Ubuntu 24.04 러너, KVM, root, `timeout-minutes: 60`)은 도구를 깔고 x86_64 이미지를 **두 번** 빌드해 세 파일의
해시가 같은지 보고(재현), aarch64 이미지는 빌드만(부팅은 svc01), x86_64 시험 게스트를 만든 뒤, 고정물 전체(btrfs·ntfs·LVM·XFS 더러운 로그)를
root로 만들고, root로 `restore_helper`(`ORBIVISOR_RESTORE_SCOPE=1`)와 orbivisord E2E (1)~(5)를, 마지막에 `ORBIVISOR_RESTORE_ACCEL=tcg`로 도우미 시험을 한 번
더 돌린다. `check (ubuntu)`는 musl 두 타깃의 `cargo tree -p orbivisor-restore`에 C 빌드 스크립트(`cc`·`ring`·`openssl-sys`)가 없는지와
`archive_tools`를, `shell`은 `restore-image-test.sh`를, `package`는 `orbivisor-restore-image` deb 빌드·설치를 본다.

**ACME 시험**(`crates/orbivisor-api/tests/acme.rs`, 기능 `acme-it`, 계획 73)은 Let's Encrypt의 시험 서버 Pebble과 `pebble-challtestsrv`
실행 파일이 든 디렉터리 `ORBIVISOR_TEST_PEBBLE_DIR`이 있어야 돈다(없으면 건너뜀 — GitHub 릴리스 v2.10.1, 체크섬 파일이 없어 CI가 SHA-256을
핀한다). Pebble(`-strict`, 무작위 badNonce 5% 그대로)로 HTTP-01 노드마다 받기·키 파일 0600·끼우기·`replaces` 갱신 뒤 새 일련번호(열린
연결 그대로)·ARI 창, DNS-01 훅 공유 와일드카드·짧은 프로필, EAB, 실패 물러서기를 본다. RFC 2136은 `ORBIVISOR_TEST_BIND`(`ip:port`)·
`ORBIVISOR_TEST_BIND_KEY`(base64, hmac-sha256, 키 이름 `acme-key`, 영역 `bind.test`)가 있을 때만 돈다 — `.github/scripts/acme-bind.sh NAMED DIR
PORT`가 그 BIND를 띄우고 두 변수를 `KEY=VALUE` 줄로 찍는다(macOS는 `brew install bind`). `ORBIVISOR_TEST_PEBBLE_LOG=1`이면 Pebble 출력을 보인다.
CI **`acme`** 작업(Ubuntu 24.04)은 Pebble·challtestsrv 리눅스 판(SHA-256 핀)과 `bind9`(AppArmor 프로필을 비껴가게 `named`를 복사해
띄움)로 이 시험을 돌린다.

**Terraform 공급자 시험**(`terraform/`, Go 1.26 — `go.mod`의 `toolchain go1.26.11`)은 `go vet`·`gofmt`·`go mod verify`·staticcheck·
govulncheck, 단위 시험과 스키마 맞춤(`TestSchemasMatchTheServer` — `ORBIVISOR_OPENAPI`의 매니페스트 스키마와 리소스 맨 위 칸의 이름·타입·필수),
그리고 `TF_ACC=1`의 수용 시험 여덟이다(VM 묶음·할당량을 넘는 VM이 plan에서 오류·네트워크·권한·백업·인증서·데이터 소스·템플릿).
수용 시험의 `TestMain`이 `ORBIVISOR_DEVSERVER`의 `orbivisor-devserver`를 `--exit-on-stdin-eof`로 띄우고, terraform 실행 파일은
`TF_ACC_TERRAFORM_PATH`(OpenTofu면 `TF_ACC_PROVIDER_HOST=registry.opentofu.org`도)로 준다. `orbivisor-devserver`는 메모리 스토리지 노드 하나
(풀 `local`·`backup`, SDN을 세운 것처럼 적음 — OVN 없음, ACME 계정 `dev` — CA에 등록하지 않음, 노드 기능 모두와 `http01`)를 클러스터 CA가
서명한 HTTPS(127.0.0.1)로 띄우고 `{"url","token","ca_pem","ca_fingerprint","admin_password","node","dir"}` 한 줄을 찍는다(admin 비밀번호는
실행마다 무작위, `--address`는 루프백만 — 다른 주소는 `--insecure-expose`가 있어야 한다). 스키마를 바꾸면
`terraform/docs/`를 `tfplugindocs`로 다시 만든다(명령은 `terraform/main.go` 머리). CI **`terraform`** 작업(행렬 terraform 1.17.2·tofu 1.13.9,
SHA-256 핀, setup-go 1.26.11, staticcheck v0.8.2, govulncheck v1.9.0)이 `orbivisor-devserver`·OpenAPI를 cargo로 만들고 위 검사 전부와 수용 시험을
돌린다.

웹 UI (`web/`):

```bash
pnpm -C web install
pnpm -C web dev              # http://localhost:5173, /api는 ORBIVISOR_URL(기본 https://localhost:8006)로 넘긴다
pnpm -C web test             # 단위·컴포넌트 테스트 (Vitest) — axe 접근성 검사와 색 토큰 대비 시험 포함
pnpm -C web test src/tokens.test.ts   # 색 토큰 대비만: index.css의 밝음·어두움 oklch 값으로 글 4.5:1, 상태 칩 4.5:1, 포커스 링·차트 3:1
pnpm -C web test src/cpu           # CPU 모델 고르기·플래그 칩·보류 표시·설정의 CPU 칸(msw)
pnpm -C web test src/numa          # CPU 배치·NUMA 칸, 들어갈 노드 어림, vCPU 고정 표, 노드 토폴로지 격자, 호스트 자원 대화상자 409(msw)
pnpm -C web test src/clusterMap src/components/iso   # 클러스터 지도(계획 70): 묶기·배치·단계·합친 경로·적중·키보드 이웃(순수), 100 × 50 배치 시간과 원소 수 예산, 그린 원소 수, 화면·서랍·목록·휴대폰·axe·대비(msw)
pnpm -C web typecheck && pnpm -C web lint
pnpm -C web gen:api          # API를 바꾼 뒤 src/api/schema.d.ts를 다시 만든다(CI가 확인한다)
pnpm -C web exec playwright install chromium
pnpm -C web build && cargo test -p orbivisord --features web-it --test web   # E2E: 브라우저 → 업로드 → VM → 콘솔 → 정리, 폴더 사용자·2FA
pnpm -C web build && cargo test -p orbivisord --features web-it --test web browser_shows_a_folder_user_only_their_folder   # 폴더 사용자만(QEMU 없이 돈다)
```

브라우저 E2E(`web/e2e/vm.spec.ts`, Playwright Chromium)는 2단 사이드바(`web/e2e/nav.ts`)로 화면을 옮기고, ⌘K 팔레트로 VM을 열고, VM 목록의
일괄 막대로 강제 정지하고, VM 동작 메뉴(⋯)로 복제·편집·지우기를 하고, 노드 상세의 "CPU 모델" 칸(실제 탐침이 읽은 QEMU와
다시 읽기 단추)과 관리 › 일반·업데이트의 CPU 기준 칸을 보고, 꺼진 VM을 소켓 2·게스트 NUMA 노드 2로 고쳐 요약을 보며, 노드 상세의 "NUMA
토폴로지"(Linux는 탐침 요약, macOS는 "알리지 않았습니다")를 보고, VM이 켜진 뒤 개요 › 인프라 지도(계획 70 — 노드 블록·VM 타일,
키보드 `]`·`Enter` 서랍·`Esc`·`[`, 실제 브라우저 가리키기 카드와 타일 누르기, 목록 보기, 동작 줄이기·어둠 테마, 390 × 844 폭의 "보기 설정"과
가로 스크롤 없음, 기억한 보기)와 지도 `m`(계획 71 — 한 노드라 "Moving web: no node can take it. Escape cancels."와 `Esc` → "Cancelled.",
초점은 노드)을 본다. 이 Mac(Homebrew QEMU)에서도 돈다(약 25초). jsdom
시험이 잡지 못하는 경로 이동·렌더 문제가 있으므로 **웹을 바꾸면 커밋 전에 이 Mac에서 돌린다.** 실패하면
`web/test-results/*/error-context.md`(화면 스냅숏)와 `trace.zip`이 남는다.

둘째 브라우저 E2E(`web/e2e/permissions.spec.ts`)는 `web.rs`의 `browser_shows_a_folder_user_only_their_folder`가 돌린다. Rust 쪽이
`/teamA`·`/teamB`와 VM, 그룹 grant로 `/teamA` operator를 받은 `none` 사용자를 준비하고, 브라우저는 그 사용자로 VM 목록·폴더
트리·팔레트에 `/teamA`만 보이는지, + 만들기가 폴더를 골라 두고 노드 칸 없이 VM을 만드는지, 사용자 메뉴 › 내 2단계 인증에서 인증 앱을 켠
뒤 둘째 단계로 다시 로그인하는지 본다.

셋째 브라우저 E2E(`web/e2e/builder.spec.ts`, 계획 71)는 `web.rs`의 `browser_builds_vms_with_the_builder`가 돌린다. Rust 쪽이 dir 풀
`local`, 이름이 겹칠 볼륨 `b1-disk0`, vCPU 할당량 1인 폴더 `/q`를 준비하고, 브라우저는 VM 빌더로 꺼진 VM 셋을 만든다: (1) 1440 × 1000
마우스 — 폼에서 넘어온 NIC 블록을 "빼기"로 끌고, 크기·풀·네트워크를 끌어 놓고, 이름이 겹치는 볼륨의 브라우저 오류, 4096 GiB 디스크의 서버
경고 `pool_space`(디스크 줄), 폴더 `/q`의 서버 오류 `quota_exceeded`(VM 틀, 만들기 막힘), JSON·CLI 탭 글, 만들기 → `/vms/b1`, (2) 키보드만
(`Space`·`Tab`·`Space`, 알림 줄, `Esc` 초점), (3) 390 × 844 "Add parts" 서랍 → 누르기 → "Drop here", 가로 스크롤 없음. 끝에 Rust가 CLI로
만든 VM의 스펙·볼륨·폴더를 본다. 터치 길게 누르기는 vitest에만 있다(Playwright Desktop Chrome에 터치가 없음). 세 시험은 spec 하나씩을 돌리고
하나씩 차례로 돈다.

넷째 브라우저 E2E(`web/e2e/history.spec.ts`, 계획 73)는 `web.rs`의 `browser_restores_a_vm_from_its_config_history`가 돌린다(QEMU를 켜지
않음). Rust 쪽이 꺼진 VM `hist`(메모리 256 MiB)를 CLI로 만들고, 브라우저는 웹 편집으로 메모리를 512로 바꾼 뒤 VM ⋯ "설정 기록…" 서랍에서
줄 둘과 전·후 표를 보고 만든 줄로 되돌린다. 이어서 관리 › 보안·인증서 › 인증서 보기를 열어 빈 목록을 본다. 끝에 Rust가 메모리 256과 이력 맨 위 줄(`restored_from`)을 본다. 선언 묶음·설정 기록·인증서 보기의
390px 폭 가로 넘침 없음은 `builder.spec.ts`가 본다.

**웹 CLI 미리 보기 교차 시험.** 빌더 모델 시험(`web/src/vmBuilder/model/cli.test.ts`)이 대표 상태 열둘의 CLI 명령(셸을 거치지 않는 인자 배열)·
user-data 파일·웹 요청 본문을 `web/src/vmBuilder/model/__fixtures__/cli.json`과 견주고(다르면 실패), `crates/orbivisorctl/src/web_cross.rs`가 같은
파일의 명령을 진짜 CLI(`run`)로 가짜 API에 보내 받은 `POST /vms`(템플릿은 `POST /vms/{key}/clone`) 본문이 웹 본문과 같은지 본다(CLI가 보내지
않는 서버 기본값은 고르게 한 뒤). 사례마다 `before`(그 앞의 `POST /volumes`·`POST /placement-rules`, 차례대로)도 CLI가 보낸 요청과
차례·본문을 맞춰 보고, 웹 `power: running`은 CLI 명령의 `--start`와 그 켜기 요청(`POST /vms/<이름>/actions/start`)이 있을 때만이다. CLI 플래그나 웹 본문을 바꾸면 한쪽이 깨진다 — 의도한 바뀜이면 `UPDATE_FIXTURES=1 pnpm -C web test
src/vmBuilder`로 고정 파일을 다시 쓰고 `cargo test -p orbivisorctl web_cross`를 돌린다. 고정 파일은 prettier가 건드리지 않는다(`.prettierignore`).

접근성 검사는 `src/test/axe.ts`의 `expectNoAxeViolations(container)`가 `axe-core`를 직접 부른다(jsdom에서 안 되는
`color-contrast` 규칙은 끄고 `src/tokens.test.ts`가 대신한다). 머리줄·팔레트·서랍·목록·상세·설정·홈 시험에서 돈다. 새 색
토큰 짝은 `tokens.test.ts`의 표에 더한다.

늦게 읽는 화면의 문구는 첫 화면 묶음에 넣지 않을 수 있다: 클러스터 지도는 `clusterMap.*`를 `web/src/clusterMap/i18n/{en,ko}.json`에 두고
경로를 열 때 `clusterMap/strings.ts`가 `addResourceBundle`로 얕게 더한다(첫 화면에는 `nav.clusterMap`·팔레트 별칭·단축키 글만). 시험은
`src/test/setup.ts`가 미리 더하고 `i18n.test`가 두 파일의 키를 맞춘다. 화면 안에서만 받는 키는 `lib/shortcuts.ts`의 `SHORTCUTS`에 따로
묶음(`map`)으로 적어 `?` 도움말에만 보이고 전역 해석기는 받지 않는다. 첫 화면 묶음 크기는 `pnpm -C web build` 뒤 진입 조각 +
modulepreload + index CSS의 gzip -9 합으로 잰다(계획 70 끝 478,317 B). 계획 71은 빌더 문구(`vmBuilder/i18n`)와 끌기 틀 문구(`dnd/i18n`)를 같은
규약으로 늦게 받는다(`loadBuilderStrings`·`loadDndStrings`). 계획 71 끝은 이 도구로 479,457 B(시작 478,636 B에서 +821 B — 잰 도구에 따라 앞의 수와
다르다).

`cargo test --workspace --all-features`는 `web-it`도 켜므로 위 준비(빌드한 `web/dist`, pnpm, Chromium)가
필요하다.

systemd가 있는 Linux에서는 `ORBIVISOR_TEST_SYSTEMD=1`을 주면 QEMU 테스트가 systemd 스코프로 돈다(CI의
`linux-kvm` 작업). macOS에서는 Docker의 privileged Linux 컨테이너(`--init`)로 Linux 테스트를 TCG로
돌릴 수 있다.
