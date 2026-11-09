# opLite 1차 하위 프로젝트 설계: 노드 코어 (클러스터 상태 + VM 라이프사이클)

- 작성일: 2026-10-20
- 상태: 설계 승인됨, 구현 계획 작성 전
- 범위: 전체 제품 중 첫 번째 하위 프로젝트

## 1. 제품 맥락

opLite는 Rust로 작성하는 온프레미스 가상화·클라우드 플랫폼이다. Proxmox VE, VMware vSphere와 같은 영역의 제품이다.

| 항목 | 결정 |
|---|---|
| 1차 목표 시장 | VMware 이탈 기업(중소·중견), 3~32노드 온프레미스 클러스터 |
| 사업 모델 | 오픈코어: 코어 공개 + 엔터프라이즈 플러그인과 기술지원 구독 |
| 팀과 일정 | 소규모 팀(3~8명), 12개월 안에 파일럿 고객 확보 |
| 개발 환경 | 당분간 macOS(Apple Silicon)만 사용. Linux/KVM 환경은 나중에 확보 |
| 아키텍처 | 대칭형 노드 + 내장 Raft (별도 관리 서버 없음) |
| 하이퍼바이저 | QEMU/KVM을 QMP로 직접 제어 (libvirt 사용 안 함) |

### 1.1 전체 제품 분해

| # | 하위 프로젝트 | 대응 기능 (Proxmox / VMware) |
|---|---|---|
| 1 | **노드 코어: 클러스터 상태 + VM 라이프사이클 (이 문서)** | qemu-server, pmxcfs / hostd |
| 2 | 웹 UI | PVE GUI / vSphere Client |
| 3 | 공유 스토리지와 스냅샷 (NFS, iSCSI, Ceph, LVM, ZFS) | pve-storage / VMFS, vSAN |
| 4 | 라이브 마이그레이션 | qm migrate / vMotion |
| 5 | HA와 펜싱, 2노드 위트니스 | ha-manager / vSphere HA |
| 6 | 네트워크와 SDN (브리지 관리, VXLAN, 방화벽) | pve-network / NSX |
| 7 | 백업 | PBS / VADP |
| 8 | VMware 마이그레이션 마법사 | Import Wizard |
| 9 | 설치 ISO, 업데이트 채널, 라이선스, 텔레메트리 | 구독 저장소 / vLCM |
| 10 | 엔터프라이즈 플러그인 (LDAP/SSO, 감사, 다중 클러스터 관리, 고급 DR) | — / vCenter 기능 |
| 11 | 노드 유지보수 모드(노드 비우기) — 1~10 뒤에 더함 | 노드 유지보수 / Maintenance Mode |
| 12 | 증분 백업과 증분 복제 — 1~10 뒤에 더함 | PBS 증분 백업(dirty bitmap) / CBT |

하위 프로젝트마다 별도로 설계 → 계획 → 구현 사이클을 거친다. 위 순서가 구현 권장 순서다.

구현: 하위 프로젝트 4는 계획 20(공유 풀 위의 라이브 마이그레이션, QEMU TLS 마이그레이션·노드 사이 RPC·`VmMigrate` Task)과 계획 21(UEFI 변수가 멈춘 채 옮긴 VM을 따라감, 디렉터리 풀의 `shared` 바꾸기)로 나눴다(`2026-10-30-live-migration-design.md`). 디스크까지 복사하는 마이그레이션과 macOS(HVF) 노드는 넣지 않았다.

구현: 하위 프로젝트 5는 계획 22(위트니스 노드: VM을 돌리지 않는 Raft 투표 멤버, 완료), 계획 23(HA 리스와 watchdog 자기 펜싱: `VmSpec.ha`, 리더의 리스 표, 노드의 `HaGuard`, 리스를 잃으면 HA VM을 끔, 완료), 계획 24(리더의 HA VM 복구: `HaManager`가 리스가 끝난 노드의 HA VM을 다른 노드로 옮기고 기록 Task `vm-ha-recover`를 남김, 이미지 잠금 기다리기, HA는 멈춘 VM에서만 켬, 실제 시험 `ha-it`, 완료)로 나눴다(`2026-11-02-ha-fencing-witness-design.md`). 하위 프로젝트 5는 끝났다. 외부 펜싱 장치, 유지보수 때 모두 옮기기, HA 그룹·우선순위, HA 아닌 VM의 자동 복구는 넣지 않았다.

구현: 하위 프로젝트 6은 계획 25(VXLAN 네트워크: opld가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만들고 다른 노드의 underlay를 FDB에 넣음, 노드 underlay, tap MTU와 `host_mtu`, 완료)와 계획 26(VM 방화벽: 보안 그룹 `SecurityGroup`을 VM NIC에 걸고 노드마다 nftables `bridge opl_<태그>` 테이블로 tap에서 거름, 노드 능력 `firewall`과 스케줄러 조건, 켜기 직전 규칙 넣기, 완료)으로 나눴다(`2026-11-03-network-sdn-design.md`). 하위 프로젝트 6은 끝났다. IPAM·DHCP·라우팅·SNAT, EVPN·BGP, VXLAN 암호화, 그룹을 원격으로 가리키는 규칙, 방화벽 로그, IP·ARP 위조 막기는 넣지 않았다.

구현: 하위 프로젝트 7은 계획 27(백업과 복원: dir·NFS 풀의 `backups` 표시, `Backup` 리소스, 실행 중·꺼진 VM의 crash-consistent 전체 백업을 디스크마다 압축 qcow2와 `manifest.json`으로, 새 VM으로 복원, 지우기, API·oplctl·웹, 실제 시험, 완료)과 계획 28(백업 작업과 보존: `BackupJob`(대상 VM 고르기·UTC 일정·보존 규칙), 리더의 `BackupJobScheduler`, 노드당 동시 백업 제한, Proxmox식 보존 규칙으로 지우기, API·oplctl·웹, 실제 시험, 완료)으로 나눴다(`2026-11-04-backup-design.md`). 하위 프로젝트 7은 끝났다. 증분·변경 블록 추적, 중복 제거, 암호화, 원격(S3) 저장소, 게스트 에이전트로 얼리기, 파일 단위 복원, 기존 VM 덮어쓰기는 넣지 않았다.

구현: 하위 프로젝트 8은 계획 29(OVA 가져오기: `VmImport` 리소스, OVA 올리기와 손으로 읽는 tar·OVF 해석, 원본 VM에서 opLite VM 제안(게스트 OS로 디스크 버스·NIC 모델, 코어 나누기, 경고), 사용자가 고친 값으로 디스크를 `.mf` 해시를 보며 추출해 볼륨으로 옮기고 꺼진 VM을 만드는 `VmImport` Task, 지우기, API·oplctl·웹, 실제 시험, 완료)과 계획 30(ESXi·vCenter에서 바로 가져오기: 서버 인증서 지문을 고정한 HTTPS 연결과 데이터스토어 브라우저(`/folder`) 목록, 데이터스토어의 `.vmx`·VMDK 디스크립터 해석, 비밀번호는 받은 노드의 메모리에만, 같은 `VmImport`와 Task에 extent 내려받기 단계, API·oplctl·웹, 가짜 ESXi로 실제 시험, 완료)으로 나눴다(`2026-11-04-vmware-import-design.md`). 하위 프로젝트 8은 끝났다. 실행 중인 VM 옮기기, 증분 동기화, VMware 스냅샷 체인, VDDK·NFC, 게스트 안 드라이버 바꾸기는 넣지 않았다.

구현: 하위 프로젝트 9는 계획 31(Debian 패키지와 설치 ISO: 빌드 버전 `opl_types::version::BUILD`와 `--version`, 패키지 `oplite`(opld·oplctl·웹 UI·systemd 유닛·maintainer 스크립트), Debian 13 netinst에 preseed와 패키지를 더한 설치 ISO(설치가 기본 경로 인터페이스를 VLAN-aware `vmbr0`으로 옮기고 `/etc/opl/opld.toml`을 씀), CI `shell`·`package` 작업과 ISO 무인 설치 E2E 워크플로 `iso.yml`, 완료), 계획 32(버전과 업데이트: Debian 규칙의 버전 비교, `GET /api/v1/version`, 노드 버전과 낮은 버전의 합류 거절, 클러스터 설정 `Settings`의 업데이트 채널 `stable`·`testing`, 노드마다 opld가 관리하는 APT 소스 파일과 주기적인 업데이트 확인(`NodeStatus.updates`), 다시 확인 Task와 한 번에 한 노드씩 `systemd-run`으로 도는 업그레이드 Task(opld가 다시 시작해도 이어 봄), 서명한 APT 저장소 스크립트 `build-repo.sh`와 패키지 키링, oplctl·웹, 완료)과 계획 33(구독과 텔레메트리: Ed25519로 서명한 오프라인 구독 키와 발급 도구 `opl-license`, 빌드 값 `OPL_LICENSE_KEYS`(없으면 개발 키, 정식 패키지는 반드시), 읽을 때 계산하는 구독 상태(none·active·expired·invalid·over_limit), 구독이 있어야 하는 `enterprise` 채널과 노드의 APT 인증 파일, 이 빌드보다 낮은 노드가 있으면 새 설정 값을 쓰지 않는 게이트 `require_current`, 옵트인 텔레메트리(미리 보기, 리더의 `TelemetryReporter`가 하루 한 번, 수만 보냄), 웹 설정 화면과 대시보드 구독 안내, oplctl, 완료)으로 나눴다(`2026-11-05-install-update-subscription-design.md`). 하위 프로젝트 9는 끝났다. 오프라인 설치, ZFS 루트·RAID 설치, arm64 ISO, 기능 잠금, 온라인 구독 확인, 구독 키의 클러스터 묶기(하드웨어 지문), 텔레메트리 수집 서버, 자동·롤링 업그레이드, 노드 비우기(유지보수 모드), 커널·펌웨어 관리, 업데이트 되돌리기는 넣지 않았다.

구현: 하위 프로젝트 10은 계획 34(감사 로그: 노드마다 라우터 미들웨어가 바꾸는 API 요청·로그인·로그아웃·콘솔 열기를 `{data_dir}/audit/audit.log`(JSON Lines, 0600, 크기·개수 회전, 1초 묶음 fsync)와 tracing 대상 `opl_audit`(journald)에 적음, 동작 이름은 `"<METHOD> <라우트 템플릿>"`, 노드 사이 RPC `ReadAudit`로 모든 노드를 모아 읽는 `GET /api/v1/audit`(admin), opld `[audit]`, `oplctl audit`, 웹 `/audit`, 완료), 계획 35(인증 영역과 LDAP: 새 리소스 `Realm`(LDAP, admin만, 서비스 계정 비밀번호는 응답·watch에 없음), `ldap3`(rustls)로 StartTLS·LDAPS만 쓰는 바인드·사용자 찾기·그룹 속성으로 역할 매핑, 영역 로그인이 외부 사용자 `<사용자>@<영역>`을 만들거나 역할을 고침(`no_role`·`realm_unavailable`·`external_user`), 영역을 지우면 그 사용자의 세션·토큰이 멈춤, 시험 동작, `oplctl realm`·`login --realm`, 웹 로그인 영역 고르기와 `/realms`, 가짜 LDAP 서버 시험과 CI `ldap` 작업의 실제 slapd 시험, 완료), 계획 36(OIDC SSO: `Realm` 종류 `oidc`(issuer·client_id·client_secret(응답·watch에 없음, 공개 클라이언트도 됨)·scope·이름·그룹 클레임·CA), reqwest와 ring으로 직접 만든 인가 코드 흐름(PKCE S256, state·nonce·검증자를 클러스터 CA 키에서 HKDF로 만든 키로 HMAC한 쿠키 `opl_oidc`에 담아 어느 노드나 콜백을 받음), discovery·JWKS 노드 캐시와 모르는 kid 다시 받기, ID 토큰 RS256·ES256 확인과 iss·aud·azp·exp·iat·nbf·nonce, 인가 응답 iss(RFC 9207), userinfo로 빠진 클레임, 그룹 클레임으로 역할과 계획 35의 외부 사용자(이메일 모양 이름, IdP 사용자 sub에 묶음), `/login?error=` 코드와 콜백 감사(시도 제한 없음), IdP 시험 동작, `oplctl realm add-oidc|set-oidc`·`login --token-file`, 웹 로그인 SSO 버튼과 OIDC 영역 화면, 가짜 IdP 시험, 완료), 계획 37(원격 클러스터: 새 리소스 `Remote`(URL·CA 지문·원격 클러스터의 API 토큰 — 토큰은 응답·watch에 없고 id만 보임), 원격 CA 하나만 믿고 호스트 이름은 보지 않는 hyper·rustls 클라이언트(CA는 검증 없이 받아 지문으로 확인, 노드 메모리 캐시), 등록 때 지문·자기 클러스터·토큰(`whoami`) 확인과 `remote_*` 오류, 웹용 `probe`, 리더의 `RemotePoller`가 원격의 `/cluster`·`/nodes`·`/vms`로 상태를 모아 바뀔 때만 씀(opld `[remotes]`), API 중계 `/remotes/{key}/api/{*path}`(GET은 viewer 이상·나머지 admin, 원격 쪽은 토큰 주인의 역할, watch·콘솔 404, 스트리밍, OpenAPI 밖, 감사 detail), `oplctl remote`, 웹 `/remotes` 카드와 원격 VM 목록·동작, 두 `Env`(HTTPS)·opld 두 클러스터 E2E 시험, 완료), 계획 38(원격 복제·DR: 백업의 출처 `origin`·받는 중 `receiving`·원격 사본 기록 `replicas`, 받는 쪽 API `POST /backups/receive`·`PUT /backups/{key}/files/{file}`(스트리밍, SHA-256과 크기를 돌려줌)·`complete`(매니페스트를 받는 쪽 id로 쓰고 `ready`)와 24시간 넘게 받는 중인 사본을 지우는 janitor, 보내는 쪽 `backup-replicate` Task(계획 37의 원격 클라이언트로 파일마다 해시를 비교하며 흘려 보냄, 같은 출처의 사본이면 이어 감)와 수동 복제 `POST /backups/{key}/actions/replicate`, 백업 작업의 `replicate`(스케줄러가 ready 백업을 보내고 실패한 사본은 실행마다 다시, 노드마다 하나), 원본을 지우면(사용자·보존 규칙) 원격 사본도 지움(실패는 경고), 받은 사본은 그 클러스터의 보통 백업이라 그곳에서 복원, `oplctl backup replicate`·목록의 `ORIGIN`·`REPLICAS`·`backup-job --replicate`, 웹 출처 배지·사본 상태·"원격으로 복제"·작업의 원격 복제, README DR 절차, 완료)으로 나눴다(`2026-11-06-enterprise-design.md`). 하위 프로젝트 10은 끝났다. 감사 로그 변조 방지와 원격 syslog 직접 전송·요청 본문 기록, SAML·Kerberos, LDAP 사용자·그룹 동기화, 세분화된 RBAC, 외부 사용자의 2단계 인증, oplctl의 SSO 로그인, 원격 클러스터의 전체 웹 화면·콘솔 중계·클러스터 사이 라이브 마이그레이션, 디스크 단위 연속 복제, 자동 장애 조치와 복구 계획 실행, 복제의 대역폭 제한·증분 전송은 넣지 않았다. 이로써 처음 분해한 하위 프로젝트 1~10이 모두 끝났다.

구현: 하위 프로젝트 11은 처음 분해한 하위 프로젝트 1~10이 끝난 뒤 라이브 마이그레이션·HA·업데이트 설계가 넣지 않은 것으로 남긴 "유지보수 때 모두 옮기기"·"노드 비우기(유지보수 모드)"를 채운다. 계획 39(노드 비우기: `POST /api/v1/nodes/{key}/drain`(admin)이 노드를 유지보수로 바꾸고 `node-drain` Task를 그 노드에 만듦(위트니스·오프라인 409 `node_unavailable`, 이미 비우는 중 409 `node_busy`), Task가 실행 중인 VM을 이름 순으로 하나씩 스케줄러가 고른 노드로 `vms::start_migration`(마이그레이션 API와 같은 확인)의 `VmMigrate` Task로 옮기고 끝나기를 기다림, 로컬 풀 디스크·HVF·자리 없음·busy·마이그레이션 실패는 남기고 "N VM(s) were not moved: …"로 실패, 유지보수를 끄면 멈춤, 일반 600초 제한 없이 노드의 줄에 섬; 유지보수 노드에 고정된 꺼진 VM을 켜면 다른 노드에 놓고 못 하면 409, 만들기·고치기에서 유지보수 노드를 새로 고르면 409; `oplctl node maintenance on|off`·`node drain [--wait]`, 웹 노드 화면 "노드 비우기…", README 절차, 실제 QEMU 시험, 완료)로 했다(`2026-11-09-node-maintenance-design.md`). 하위 프로젝트 11은 끝났다. 디스크까지 복사하는 마이그레이션(로컬 디스크 VM 옮기기), 유지보수가 끝난 뒤 VM 되돌리기, 옮기지 못한 VM을 끄는 선택, 롤링 업그레이드 자동화와 노드 재부팅은 넣지 않았다.

구현: 하위 프로젝트 12는 백업·엔터프라이즈 설계가 넣지 않은 것으로 남긴 "증분·변경 블록 추적"과 "복제의 증분 전송"을 채운다. 계획 40(증분 백업: 실행 중인 VM의 디스크마다 QEMU dirty bitmap `opl-{백업 id}`(qcow2는 persistent)를 백업마다 그 시점에 바꿔 두고, `incremental` 요청이면 같은 풀의 바로 앞 백업을 부모로 `blockdev-backup`(`sync: bitmap`, `bitmap-mode: never`, 압축)이 바뀐 블록만 backing 없는 qcow2에 씀, 매니페스트 버전 2의 `parent`, 할 수 없으면 전체로 하고 `full_reason`, 스냅샷 되돌리기 전에 비트맵 지우기, `json:` 체인으로 복원, 부모 지우기 409 `backup_in_use`, 보존 규칙의 조상 보호와 자식부터 지우기, 백업 작업의 `full_every`, 기능 `incremental-backup`과 롤링 업그레이드 중 비트맵 만들지 않기, 증분 백업은 아직 복제하지 않음; `oplctl backup create --incremental`·`TYPE` 열·`chain`·`backup-job --full-every`, 웹 "증분(가능하면)"·종류·체인 크기·작업의 전체 백업 주기, README, 실제 QEMU 시험, 완료)로 했다(`2026-11-09-incremental-backup-design.md`). 계획 41(증분 복제)이 남았다. 꺼진 VM의 증분, 중복 제거 청크 저장소, 증분 병합, 바이트 단위 이어 보내기, 대역폭 제한, 백업 암호화는 넣지 않는다.

### 1.2 검토했지만 채택하지 않은 대안

- **중앙 관리 서버 + 경량 에이전트 (vCenter 방식):** 관리 서버 자체의 HA를 따로 풀어야 하고 설치 단계가 늘어난다. 이 복잡함이 VMware를 떠나는 이유 중 하나다.
- **Kubernetes 기반 (KubeVirt 방식):** 운영 복잡도가 목표 고객과 맞지 않는다.
- **Cloud Hypervisor (Rust VMM):** Windows와 레거시 장치 지원이 부족하다. 대신 `Hypervisor` 트레이트를 통해 나중에 경량 VM 백엔드로 추가할 수 있게 열어 둔다.

## 2. 아키텍처와 프로세스 모델

```
┌──────────────────── 노드 (Linux 호스트, Debian 기반 어플라이언스) ────────────────────┐
│                                                                                      │
│  opld (단일 바이너리, tokio)                                                          │
│  ├─ api        : HTTPS REST + WebSocket (axum)   ← UI/CLI/Terraform이 접속             │
│  ├─ cluster    : openraft 상태 머신 (redb 저장)  ← 클러스터 설정의 단일 진실 원천      │
│  ├─ reconciler : 원하는 상태와 실제 상태를 맞추는 조정 루프 (노드별)                   │
│  └─ drivers    : Hypervisor / Storage / Network 트레이트 구현체                        │
│                                                                                      │
│  qemu-system-* (VM 1개당 프로세스 1개, systemd scope로 격리, QMP 소켓으로 제어)       │
└──────────────────────────────────────────────────────────────────────────────────────┘
          ▲ 노드 간 통신: gRPC(tonic) over mTLS — Raft 메시지 + 노드 간 작업 전달
```

### 2.1 핵심 원칙

1. **선언형 + 조정 루프.** API는 원하는 상태(spec)를 Raft에 기록하기만 한다. 각 노드의 reconciler가 자기 노드에 배정된 리소스를 실제 상태로 맞춘다.
2. **작업(Task) 모델.** 오래 걸리는 작업은 모두 Task ID를 반환하고, 진행률과 로그를 WebSocket으로 스트리밍한다.
3. **Raft에는 설정만 넣는다.** 메트릭 같은 고빈도 데이터는 노드 로컬에 저장하고, 노드 간 조회는 gRPC로 한다.
4. **단일 노드도 1인 클러스터다.** 단일 노드 모드라는 별도 코드 경로를 두지 않는다.

### 2.2 크레이트 구조 (Cargo 워크스페이스)

| 크레이트 | 역할 | 공개 여부 |
|---|---|---|
| `opl-types` | 리소스 모델(VmSpec, VolumeSpec 등), serde 스키마 | 공개 |
| `opl-qemu` | QMP 클라이언트 + QEMU 커맨드라인 생성기 | 공개 |
| `opl-driver` | `Hypervisor`/`Storage`/`HostNet`(tap·브리지) 트레이트, Mock·QEMU(hvf/KVM/TCG)·iproute2 구현 | 공개 |
| `opl-cluster` | openraft 상태 머신, 리소스 저장소, watch 스트림, 리더 전달 | 공개 |
| `opl-peer` | 노드 간 통신(gRPC over mTLS: Raft RPC, 리더 전달, 생존 확인, 콘솔 중계), 클러스터 PKI | 공개 |
| `opl-reconciler` | 조정 루프, VM별 작업 큐 | 공개 |
| `opl-api` | REST/WebSocket, 인증, `Authorizer` 훅 | 공개 |
| `opld` | 데몬 바이너리 조립 | 공개 |
| `oplctl` | CLI | 공개 |
| `opl-ent-*` | 엔터프라이즈 플러그인 | 비공개 (별도 저장소) |

오픈코어 경계는 크레이트 단위로 나눈다. 엔터프라이즈 기능은 공개 크레이트가 정의한 트레이트(`Authorizer`, `AuthProvider` 등)를 구현하는 비공개 크레이트로 제공하고, `opld` 빌드 시 기능 플래그로 연결한다. 공개 크레이트는 비공개 크레이트에 의존하지 않는다.

## 3. 리소스 데이터 모델

### 3.1 공통 봉투

```rust
struct Resource<S, T> {
    id: Uuid,                          // UUIDv7, 변경 불가
    name: String,                      // 종류별로 클러스터 내 유일
    revision: u64,                     // 마지막 수정의 Raft 커밋 인덱스
    labels: BTreeMap<String, String>,
    spec: S,                           // 원하는 상태: API로만 수정
    status: T,                         // 실제 상태: reconciler만 기록
}
```

- spec과 status를 분리해 사용자 쓰기와 시스템 쓰기가 충돌하지 않게 한다.
- `revision`은 낙관적 동시성 제어에 쓴다. 클라이언트가 `If-Match: <revision>`을 보냈는데 값이 다르면 412를 반환한다.
- 저장 형식 버전(`schema_version`)은 봉투가 아니라 저장 계층에 둔다. 노드의 redb 파일(`meta` 테이블)과 Raft 스냅샷 본문에 기록하고, 지원하지 않는 버전의 파일이나 스냅샷은 열지 않는다. 형식이 바뀌면 버전을 올리고 마이그레이션을 추가한다.
- 식별자는 숫자 VMID가 아니라 UUID를 쓴다. 다중 클러스터를 병합해도 충돌하지 않게 하기 위해서다.
- **저장 형식 호환성 규칙:** Raft에 저장되는 타입(spec/status)에 필드를 추가할 때는 반드시 `#[serde(default)]`를 붙인다. 새 필드는 클러스터의 모든 노드가 업그레이드되어 클러스터 기능 레벨이 올라간 뒤에만 기록할 수 있다. 이 규칙 덕분에 구 버전 노드는 새 필드를 만나지 않으므로, spec 타입의 `deny_unknown_fields`(API 오타 거부)를 유지할 수 있다.
- Raft 로그 엔트리(`Command`)는 버전 필드 없이 serde 태그로 구분한다. 모르는 명령은 무시하지 않고 오류로 처리하므로, 새 명령도 클러스터 기능 레벨이 올라간 뒤에만 기록한다.
- Raft 노드 id는 `Node` 리소스 id와 같은 UUID다(`opl_cluster::types::NodeId`).

### 3.2 리소스 목록

| 리소스 | spec | status |
|---|---|---|
| `Node` | 이름, 주소, 유지보수 모드 | 온라인 여부, CPU/메모리 용량, 하이퍼바이저 종류(`qemu-kvm`/`qemu-tcg`/`qemu-hvf`), Linux 브리지 목록(VLAN-aware 여부) |
| `Vm` | vCPU, 메모리, 펌웨어(bios/uefi), 머신 타입, 디스크·NIC, `power`, `node`(선택), `restart` 정책 | 실제 전원 상태, 실행 노드, QEMU PID, 오류 조건 |
| `StoragePool` | 종류(`dir`), 경로, 사용할 노드, `shared: bool` | 용량, 사용량 |
| `Volume` | 소속 풀, 크기, 형식(qcow2/raw) | 준비 상태(Pending/Ready), 할당 크기, 파일을 만드는 노드 |
| `Network` | 브리지 이름, VLAN 태그(선택). 이름 `user`는 예약(QEMU NAT, 리소스 없음) | — |
| `Task` | 작업 종류, 대상 리소스, 실행 노드 | 상태(Pending/Running/Succeeded/Failed), 오류 |
| `User`, `ApiToken` | 사용자 이름, 비밀번호 해시(argon2), 역할 / 주인, 토큰 비밀 값의 SHA-256 | — |
| `Cluster` | 클러스터 이름(리소스 이름), CA 인증서와 개인키(API로 내보내지 않는다) | — |
| `JoinToken` | 비밀 값의 SHA-256, 만료 시각 | — |

VM이 붙인 볼륨과 NIC의 MAC 주소는 클러스터 안에서 VM 하나만 쓸 수 있다. 여러 노드의 API가 동시에 써도 겹치지 않도록 상태 머신이 커밋 순서대로 확인한다(`Rejection::InUse`, API는 409).

`Vm` spec 예시:

```yaml
name: web-01
spec:
  cpu: { sockets: 1, cores: 4, model: host }
  memory_mib: 8192
  firmware: uefi
  machine: q35            # macOS(hvf)에서는 virt
  disks:
    - { volume: web-01-root, bus: virtio-blk, boot_order: 1 }
  nics:
    - { network: lan, model: virtio }   # mac을 생략하면 자동 할당
  power: running          # running | stopped
  restart: on-failure     # never | on-failure
  node: null              # null이면 스케줄러가 배치
```

### 3.3 스케줄러 (1차)

`node`가 비어 있는 VM을 시작하면 스케줄러가 노드를 고른다. 대상은 온라인 상태이고, 유지보수 모드가 아니며, 디스크가 있는 풀에 접근할 수 있고, NIC가 쓰는 네트워크의 브리지가 있는(VLAN을 쓰면 VLAN-aware인) 노드다. 이 중 미할당 메모리가 가장 많은 노드를 선택한다. 선택 결과는 `spec.node`에 기록한다. 리소스 오버커밋 정책과 DRS급 재배치는 범위 밖이다.

## 4. API

- 경로는 `/api/v1/{resource}`이며, 목록·조회·생성·수정·삭제를 제공한다. 경로의 `{id}` 자리에는 id나 이름을 쓸 수 있다(UUID 모양의 이름은 받지 않는다). 목록은 `{revision, items}`를 돌려준다.
- 동작은 `POST /api/v1/vms/{id}/actions/{start|stop|reboot}`이다. 응답은 `202 Accepted`와 `Task`다.
  - `start`/`stop`은 `spec.power`를 바꾸고, 그 결과를 지켜볼 Task를 만든다. 대상 노드의 Task 실행기가 VM이 원하는 상태에 도달하면 Task를 끝낸다.
  - `stop?mode=force`는 reconciler의 VM별 작업자 큐로 강제 종료를 보낸다. `reboot`은 원하는 상태를 stopped로 바꿨다가 꺼지면 running으로 되돌린다. 그 사이 같은 VM에 새 요청이 오면 그것을 따른다.
- 볼륨 생성·삭제와 이미지 가져오기(`POST /api/v1/volumes/import`, 본문이 이미지)도 Task다. 파일이 준비되기 전의 볼륨은 VM에 붙일 수 없다.
- 감시: `GET /api/v1/watch?kinds=vm,task&since=<revision>` (WebSocket). 목록의 revision 다음 변경부터 받는다. 이어 받을 수 없으면 오류 메시지를 보내고 닫으며, 클라이언트는 목록을 다시 읽는다. 사용자와 토큰은 감시할 수 없다.
- 콘솔: `GET /api/v1/vms/{id}/console` (WebSocket). 실행 노드의 QEMU VNC로 프록시한다. 다른 노드에서 요청하면 노드 간 gRPC로 중계한다.
- 클러스터: `GET /api/v1/cluster`(이름, CA 지문, 리더, 멤버), `GET /api/v1/cluster/ca`(인증 없음), `POST /api/v1/cluster/join-tokens`(관리자), `POST /api/v1/cluster/join`(조인 토큰으로 인증), `DELETE /api/v1/nodes/{id}`(오프라인 노드만).
- 팔로워가 받은 쓰기는 리더로 전달된다. 응답은 그 쓰기가 이 노드에도 적용된 뒤에 오므로, 같은 노드에서 곧바로 읽으면 결과가 보인다.
- OpenAPI 스펙은 `utoipa`로 코드에서 생성해 `GET /api/v1/openapi.json`으로 내보낸다. 라우터와 같은 정의에서 만든다.
- 인증: 로컬 사용자(세션 쿠키, 웹 UI용)와 API 토큰(Bearer, CLI용)을 지원한다. 토큰은 `opl_<id>_<비밀 값>`이고 비밀 값의 SHA-256만 저장한다. 쿠키로 인증한 요청에 `Origin`이 있으면 서버와 같아야 한다. 자기 비밀번호를 바꿀 때는 현재 비밀번호가 필요하고, 바꾸면 그 사용자의 세션이 끝난다. 모든 핸들러는 `Authorizer::check(principal, action, resource)`를 거친다. 1차 구현은 `admin`/`operator`/`viewer` 고정 역할이다.
- 오류 응답은 `{ "code": "...", "message": "...", "details": {...} }` 형식으로 통일한다. Raft 과반수가 없으면 `503 cluster_no_quorum`, revision이 다르면 `412 revision_mismatch`, 이름이 겹치면 `409 name_taken`을 반환한다.

## 5. VM 라이프사이클

### 5.1 시작 흐름

```
클라이언트 ──POST /vms/web-01/actions/start──▶ opl-api (임의의 노드)
  1. 인증·권한 확인 → spec.power = running, node가 비어 있으면 스케줄러가 선택
  2. spec 변경을 Raft에 커밋 → Task(Pending, node = 대상 노드)를 따로 커밋 → 202 + task
                                │ 커밋 스트림
                                ▼
대상 노드의 reconciler
  3. desired(running) ≠ actual(stopped) → Hypervisor::start
  4. 커맨드라인 생성 → QEMU 실행(-S, -daemonize) → QMP 연결 → qmp_capabilities → cont
  5. status(running, pid)를 Raft에 기록
대상 노드의 Task 실행기
  6. VM의 power와 phase가 모두 running이 되면 Task Succeeded (reconciler가 포기하면 Failed)
```

### 5.2 드라이버 트레이트

```rust
#[async_trait]
trait Hypervisor: Send + Sync {
    async fn start(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError>;       // 이미 실행 중이면 성공
    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError>;  // Acpi | Force, 실행 중이 아니면 성공
    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError>;
    async fn list(&self) -> Result<Vec<ActualState>, DriverError>;
    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError>; // VNC 유닉스 소켓
    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent>;                // Started | Stopped | Crashed
}
```

| 구현 | 플랫폼 | 용도 |
|---|---|---|
| `QemuHypervisor` | Linux(KVM, 없으면 TCG), macOS(hvf) — `HostProfile`로 구분 | 제품, 개발·통합 테스트 |
| `MockHypervisor` | 전체 | 단위·컴포넌트 테스트, 장애 주입(`fail_next_start`, `crash`, `guest_shutdown`) |

QEMU 커맨드라인 생성은 순수 함수 `fn build_cmdline(&VmRuntimeSpec, &HostProfile, &RuntimePaths) -> Result<Vec<String>, CmdlineError>`로 만들고, `insta` 스냅샷으로 검증한다.

### 5.3 reconciler 규칙

- **트리거:** Raft 커밋 이벤트, QMP 이벤트, 30초 주기 전체 재동기화.
- **VM별 직렬화:** VM마다 작업 큐를 하나 두고, 서로 다른 VM은 병렬로 처리한다.
- **멱등성:** 모든 단계는 다시 실행해도 안전하다. 이미 원하는 상태면 아무것도 하지 않는다.
- **종료:** 원하는 상태가 `stopped`면 ACPI 종료를 요청하고, `acpi_timeout`(기본 60초) 안에 꺼지지 않으면 강제 종료한다.
- **게스트 내부 종료:** reconciler가 요청하지 않은 종료 이벤트가 오면(원하는 상태가 `running`인데 꺼짐) `spec.power`를 `stopped`로 바꾼다. QMP의 `guest_initiated`는 opld가 요청한 ACPI 종료에서도 true이므로 그것만으로 판단하지 않는다. 우리가 요청하지 않은 `guest_initiated: false` 종료(호스트의 SIGTERM 등)는 게스트의 결정이 아니므로 원하는 상태를 바꾸지 않는다. 방금까지 돌던 VM이 사라졌는데 이벤트가 아직 없으면 잠시 기다려, 게스트가 끈 VM을 다시 켜지 않게 한다.
- **크래시와 기동 실패:** `restart: on-failure`이면 지수 백오프로 재시작하고, 10분 안에 5회 실패하면 포기한다. `restart: never`의 크래시, 영구 기동 오류(이미지 없음, 잘못된 참조 등), 포기는 모두 `spec.power`를 `stopped`로 바꾸고 status를 `Failed`와 오류 메시지로 남긴다. 사용자가 다시 시작하면 처음부터 센다.
- **응답 없는 VM:** 살아 있지만 QMP로 다룰 수 없는 VM은 끄지 않고 `start()`로 다시 붙기를 계속 시도하며 status에 그 사실을 남긴다. 원하는 상태가 stopped면 ACPI 요청이 실패해도 `acpi_timeout` 뒤 강제 종료한다.
- **배정되지 않은 VM:** 이 노드에서 돌지만 이 노드에 배정되지 않은(삭제되었거나 다른 노드로 옮겨진) VM은 끄지 않고 경고만 남긴다. 저장소가 비거나 뒤처져도 VM을 죽이지 않기 위해서다. 실행 중인 VM의 삭제와 노드 변경은 API가 거부한다.
- **status 쓰기:** reconciler는 자기 노드의 VM status만 기록한다. Raft 과반수가 없어 쓰지 못하면 실제 상태에서 status를 다시 계산해 회복될 때까지 다시 쓴다(디스크 버퍼는 두지 않는다).

### 5.4 opld 재시작과 크래시 복구

- QEMU는 `-daemonize`로 실행해 `opld`와 세션을 분리한다(Proxmox와 같은 방식). 초기화에 실패하면 부모 프로세스의 종료 코드와 stderr로 바로 알 수 있다. Linux에서는 `systemd-run --scope --unit=opl-vm-<id>`로 감싸 VM마다 cgroup을 나눈다(`[hypervisor] systemd_scope`, systemd가 없는 환경에서는 끈다). cgroup 자원 제한은 아직 걸지 않는다.
- 런타임 파일은 `/run/opl/vm/{uuid}/`(macOS는 설정한 run_dir) 아래에 `qmp.sock`, `vnc.sock`, `qemu.pid`로 둔다. UEFI 변수처럼 재부팅 후에도 남아야 하는 파일은 `/var/lib/opl/vm/{uuid}/uefi-vars.fd`에 둔다. 라이브 마이그레이션(하위 프로젝트 4)에서 공유 스토리지로 옮기는 것을 다시 검토한다. 구현: 변수 파일은 노드 로컬에 그대로 둔다. 라이브 마이그레이션에서는 QEMU가 pflash 내용을 옮겨 대상 노드의 파일에 쓰고(계획 20, CI에서 확인), 멈춘 채 옮긴 VM은 시작할 때 변수를 가진 노드(`VmStatus.firmware_node`)에서 노드 사이 RPC로 받아 온다(계획 21에서 구현, 라이브 마이그레이션 설계 §5). 그 노드가 죽었으면 템플릿으로 켠다.
- VM마다 모니터 태스크가 QMP 이벤트를 지켜본다. `SHUTDOWN` 이벤트 뒤에 연결이 끊기면 정상 종료(`Stopped`), 이벤트 없이 끊기면 `Crashed`로 판정하고 런타임 디렉터리를 정리한다.
- `opld`가 시작되면 런타임 디렉터리를 훑어 살아 있는 QEMU에 QMP로 다시 연결한다. 그 뒤 Raft의 원하는 상태와 비교해 조정한다.
- QEMU의 생존 여부는 QEMU가 수명 내내 잡고 있는 pidfile의 fcntl 잠금으로 판정한다(QMP 응답 여부와 무관하며, SIGSTOP 상태에서도 정확하다). 잠금이 풀린 런타임 디렉터리만 지우고, 잠금 소유자 pid(F_GETLK)를 확인한 뒤에만 강제 종료한다. 살아 있지만 QMP에 붙지 못한 VM은 등록하지 않고 오류를 기록하며, 이후 `start()`가 재부착을 시도한다.
- macOS에서는 `-daemonize`의 fork가 Objective-C 런타임 검사에 걸려 크래시할 수 있으므로 `OBJC_DISABLE_INITIALIZE_FORK_SAFETY=YES`로 실행한다.
- 이 설계 덕분에 `opld`를 업그레이드하거나 재시작해도 실행 중인 VM은 중단되지 않는다.

### 5.5 오류 처리

- `DriverError`는 `thiserror` 열거형이다. `is_transient()`로 재시도 가능 여부를 구분한다.
  - 일시적 오류(QMP 타임아웃, 소켓 일시 오류): 지수 백오프로 재시도한다.
  - 영구 오류(이미지 없음, 메모리 부족, 잘못된 spec): status 조건에 기록하고 Task를 Failed로 끝낸다.
- Raft 과반수를 잃으면 API 쓰기는 503이 되지만, 실행 중인 VM은 유지된다. 클러스터 장애가 워크로드 장애로 번지지 않게 하기 위해서다.

## 6. 스토리지 (1차: `dir` 풀)

```rust
#[async_trait]
trait Storage: Send + Sync {
    async fn create_volume(&self, pool: &PoolSpec, vol: &VolumeSpec) -> Result<VolumePath, DriverError>;
    async fn delete_volume(&self, pool: &PoolSpec, id: Uuid) -> Result<(), DriverError>;
    async fn import(&self, pool: &PoolSpec, src: &Path, fmt: ImageFormat) -> Result<VolumePath, DriverError>;
    async fn capacity(&self, pool: &PoolSpec) -> Result<Capacity, DriverError>;
}
```

- `dir` 풀은 로컬 디렉터리에 qcow2/raw 파일을 둔다. 작업은 `qemu-img`로 처리한다.
- `import`는 `qemu-img convert`로 VMDK, qcow2, raw 이미지를 가져온다.
- `PoolSpec.shared`는 라이브 마이그레이션(하위 프로젝트 4)에서 디스크 복사가 필요한지 판단하는 데 쓴다. 1차에서는 값을 저장하기만 한다. 구현: 공유 스토리지 설계(계획 15~19)부터 배치와 Task 노드 선택에 쓰고(`is_shared`, NFS·RBD·공유 LVM은 늘 공유), 라이브 마이그레이션(계획 20)은 디스크가 모두 공유 풀일 때만 한다(아니면 409 `migration_unsupported`, 디스크 복사는 하지 않는다). 디렉터리 풀의 `shared` 바꾸기는 계획 21에서 구현했다(`PATCH /api/v1/storage-pools/{key}`, `oplctl pool set`, 공유 → 로컬은 다른 노드의 VM이 쓰면 409 `pool_in_use`).
- 사용 중인 Volume은 삭제할 수 없다(409).

## 7. 네트워크 (1차)

- `user`: QEMU user-mode NAT. macOS 개발과 데모용이다. 리소스 없이 예약된 이름으로 쓴다.
- 브리지 네트워크(`Network` 리소스): 관리자가 미리 만든 Linux 브리지에 tap 장치를 연결한다. VLAN 태그는 선택 사항이고, 쓰려면 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다. tap은 그 VLAN만 태그 없이 주고받는다(pvid untagged, 브리지 기본 VLAN 제거).
- tap은 opld가 QEMU를 띄우기 전에 iproute2(`ip`, `bridge`)로 만들고, QEMU가 끝나면(정상 종료, 크래시, 강제 종료, 기동 실패) 지운다. 이름은 VM id의 뒷부분과 NIC 순번(`op<10자리>n<순번>`, 15자 이하)이라 opld가 다시 시작해도 VM의 tap을 찾을 수 있다. 그래서 Linux의 opld는 root(CAP_NET_ADMIN)로 돈다. VM 하나의 NIC는 16개까지다.
- 노드는 브리지 목록을 시작할 때와 1분마다 알리고(바뀔 때만 기록), 스케줄러가 쓴다.
- 브리지 생성과 관리, SDN, 방화벽은 하위 프로젝트 6에서 다룬다.

구현(계획 25): VXLAN 네트워크(`spec: { vxlan: { vni }, mtu }`)는 관리자가 만든 브리지 대신 opld가 노드마다 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만든다. 노드의 오버레이 에이전트가 장치를 만들거나 지우면 1분을 기다리지 않고 곧바로 브리지 목록을 알린다. tap은 붙을 브리지의 MTU를 따른다. 물리 NIC·업링크 브리지 관리는 여전히 관리자 몫이다(`2026-11-03-network-sdn-design.md` §2).

구현(계획 26): VM 방화벽은 NIC의 보안 그룹(`NicSpec.security_groups`)으로 켠다. 노드의 opld가 클러스터의 모든 방화벽 NIC를 nftables bridge family 테이블 `bridge opl_<클러스터 태그>` 하나로 만들어 `nft -f -`로 한 번에 바꾸고(tap 이름 → NIC 체인 판정 맵, `forward`·`input`·`output` 훅), 워커는 그런 VM을 켜기 직전에 규칙을 넣지 못하면 켜지 않는다. 노드는 `NodeStatus.firewall`로 능력을 알리고 스케줄러가 쓴다(`2026-11-03-network-sdn-design.md` §3).

## 8. 클러스터 부트스트랩과 보안

```
node1$ oplctl cluster init --name prod          # 이 노드의 opld에 제어 소켓으로 요청한다
       → 클러스터 CA 생성, node1 인증서 발급, 1인 Raft 클러스터 시작, admin 계정 생성
       → admin 비밀번호(한 번만), CA 지문, 조인 URL 출력: opl-join://node1:8006?token=…&ca=sha256:…

node2$ oplctl cluster join 'opl-join://…'
       → CA 지문 검증 → 토큰으로 CSR 서명 받기 → learner로 합류 → 로그 따라잡은 뒤 voter로 승격
```

- 노드 인증서(`{data_dir}/pki`)가 없는 opld는 클러스터에 속하지 않은 노드다. 제어 소켓(`{run_dir}/opld.sock`, 0600)만 열고 `init`이나 `join`을 기다린다. init은 인증서를 마지막에 저장하므로, 도중에 실패하면 노드는 미초기화로 남고 남은 클러스터 저장소는 다음 시도에서 지운다. join은 learner가 되자마자(승격 전에) 저장하므로, 그 뒤에 실패한 노드는 멤버로 남고 다시 시작할 때마다 승격을 요청한다.
- 조인 토큰은 일회용이고 유효 시간이 정해져 있다(기본 1시간, 최대 7일). Raft에는 해시만 저장한다. 더 필요하면 관리자가 `oplctl cluster join-token`으로 만든다. 합류를 받는 노드는 토큰을 지워 한 번만 쓰게 하고, CSR에서 공개키만 써서 서명한다(이름, 용도, 기간은 서명하는 쪽이 정한다). 노드 인증서의 CN은 노드 id다.
- CA 개인키는 Raft에 복제한다(`Cluster` 리소스). 어느 노드든 합류를 받을 수 있고, 첫 노드가 죽어도 노드를 더할 수 있다. Proxmox(pmxcfs)와 같은 방식이다.
- 노드 간 통신(기본 8007)은 gRPC(tonic) over mTLS(rustls)다. `.proto` 대신 Rust 타입을 serde JSON으로 보내므로 `protoc`가 필요 없다. 서비스는 Raft RPC, 리더 전달, 생존 확인(Ping), 콘솔 중계다. 서버는 상대 인증서의 노드 id가 지금 멤버인지 확인해 제거된 노드를 막는다.
- 외부 API(기본 8006)는 클러스터 CA가 서명한 노드 인증서로 HTTPS를 연다. `oplctl`은 처음 로그인할 때 CA 지문(`--ca-fingerprint`)으로 서버를 확인하고 CA를 저장한다. 클라이언트 쪽 TLS는 OS 구현(native-tls)이다. macOS는 유효 기간이 825일을 넘는 서버 인증서를 거부하므로 노드 인증서는 824일짜리이고, 만료 전에 갱신해야 한다. 사용자 인증서로 교체하는 것은 나중에 한다. 세션 쿠키에는 `Secure`를 붙인다.
- 리더는 2초마다 다른 노드에 Ping을 보내 10초 동안 응답이 없으면 `NodeStatus.online`을 false로 기록한다(바뀔 때만 기록한다). 스케줄러가 이 값을 본다.
- `oplctl cluster leave`(정상 탈퇴)는 이 노드에 배정된 VM이 있거나 이 노드가 유일한 투표 멤버면 거부한다. Node 리소스를 지우고 멤버십에서 빠진 뒤, 클러스터 저장소와 노드 인증서를 지우고 미초기화 상태로 돌아간다. `oplctl cluster remove-node --force`(죽은 노드 제거)는 오프라인 노드만 뺀다. 멤버십 변경은 openraft의 joint consensus로 처리한다.
- 2노드 클러스터는 만들 수 있지만 `oplctl cluster status`가 경고를 표시한다. 위트니스는 하위 프로젝트 5에서 다룬다. 구현: 계획 22로 위트니스 노드(설정 `witness = true`, VM을 돌리지 않고 선거에 나서지 않는 투표 멤버)를 더했다. 일반 노드 둘과 위트니스면 투표 멤버가 셋이라 경고가 없고, 한 노드가 죽어도 과반수가 남는다(`2026-11-02-ha-fencing-witness-design.md` §2).

## 9. 범위 밖 (1차)

HA와 펜싱, 라이브 마이그레이션, 스냅샷과 백업, 공유 스토리지, 브리지 관리와 SDN, 웹 UI, LDAP/SSO, 세분화된 RBAC, 라이선스 시스템, 설치 ISO, 메트릭 수집과 저장.

## 10. 테스트 전략

| 계층 | 대상 | 도구 | 환경 |
|---|---|---|---|
| 단위 | 커맨드라인 생성, QMP 파싱, 스케줄러, 상태 머신 적용 | `cargo test`, `insta` | macOS, CI |
| 컴포넌트 | reconciler + Mock 드라이버: 이벤트 누락·중복·순서 바뀜 | tokio 테스트, 짧게 줄인 시간 설정 | macOS, CI |
| 클러스터 | 한 프로세스 안의 3노드(인메모리 네트워크), 리더 장애, 네트워크 분할 | openraft 테스트 하네스 | macOS, CI |
| 통합 | 실제 QEMU: 기동, QMP, 종료, opld 재시작 후 재연결 | `--features qemu-it` | macOS(hvf), Linux(KVM/TCG) |
| 통합 | 실제 tap·브리지·VLAN, QEMU NIC 연결과 정리, systemd 스코프 | `--features qemu-it,net-it`(root), `OPL_TEST_SYSTEMD=1` | Linux |
| E2E | oplctl로 VM 생성 → 콘솔 → opld kill -9 → 재연결 (opld 프로세스 + oplctl 라이브러리) | `--features qemu-it` | macOS(hvf) (M5) |
| E2E | oplctl로 클러스터 구성 → 아무 노드로 쓰기 → 리더 kill -9 → 노드 제거·탈퇴·재합류 (opld 프로세스 3개) | `cargo test` | macOS, CI (M6) |
| E2E | oplctl로 네트워크 생성 → 브리지 NIC VM 시작 → tap·VLAN 확인 → 끄면 tap 제거, 위 E2E를 Linux에서 | `--features qemu-it,net-it`(root) | Linux: Docker 컨테이너(TCG), CI(KVM) (M7) |

반드시 회귀 테스트로 고정할 불변식:

1. API가 성공(2xx)을 반환했다면 해당 변경은 Raft에 커밋된 것이다.
2. `opld`를 재시작해도(`kill -9` 포함) 실행 중인 VM은 중단되지 않고, 재시작 후 다시 연결된다.
3. 같은 VM에 대한 드라이버 호출은 동시에 실행되지 않는다.
4. Raft 과반수를 잃어도 실행 중인 VM은 중지되지 않는다.

## 11. 마일스톤

| M | 결과물 | 완료 기준 |
|---|---|---|
| M0 | 워크스페이스 뼈대, CI(fmt/clippy/test), `opl-types` | macOS에서 `cargo test` 통과 |
| M1 | `opl-qemu`: QMP 클라이언트 + 커맨드라인 생성기 | macOS에서 aarch64 게스트 기동, 상태 조회, 종료 |
| M2 | `opl-driver`: 트레이트 + Mock + QemuHvf + `dir` 풀 | Volume 생성, VMDK 가져오기 통합 테스트 통과 |
| M3 | `opl-cluster`: openraft + redb, 리소스 CRUD, watch | 3노드 인메모리 클러스터에서 리더 장애 후에도 쓰기 지속 |
| M4 | `opl-reconciler`: 조정 루프, VM별 큐, 재연결 | `opld` kill -9 후 VM 유지와 재연결 확인 |
| M5 | `opl-api` + `oplctl`: REST, Task, 콘솔, 토큰 인증 | CLI로 VM 생성부터 콘솔 접속까지 수행 |
| M6 | 클러스터 부트스트랩: CA, join, leave, mTLS | macOS에서 프로세스 3개로 클러스터 구성 |
| M7 | `QemuKvm` + bridge 네트워크 | Linux에서 E2E 통과 |

M0부터 M6까지는 macOS에서 진행한다. M7은 Linux에서 확인한다. 개발 Mac(M1)은 중첩 가상화가 없어 로컬은 privileged Docker 컨테이너에서 TCG로, KVM은 CI(GitHub Linux 러너)에서 확인한다.

## 12. 주요 의존성

| 용도 | 크레이트 |
|---|---|
| 비동기 런타임 | `tokio` |
| HTTP/WebSocket | `axum`, `tower` |
| 노드 간 RPC | `tonic`(수동 서비스 정의, serde JSON 메시지) |
| 합의 | `openraft` |
| 내장 저장소 | `redb` |
| TLS/PKI | `rustls`(ring), `rcgen`, 클라이언트는 `native-tls` |
| 직렬화 | `serde`, `serde_json`, `serde_yaml` |
| OpenAPI | `utoipa` |
| 오류 | `thiserror`(라이브러리), `anyhow`(바이너리) |
| CLI | `clap` |
| 로깅 | `tracing`, `tracing-subscriber` |
| 비밀번호 해시 | `argon2` |
| 테스트 | `insta`, `proptest` |

라이선스 정책: 공개 코어는 **Apache-2.0**으로 배포한다(2026-10-20 결정). 의존성은 Apache-2.0과 호환되는 라이선스(MIT, BSD, ISC 등)만 허용하고, CI에서 `cargo-deny`로 검사한다. QEMU(GPL)는 별도 프로세스로 실행하므로 링크하지 않는다.
