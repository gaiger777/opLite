# Orbivisor

Rust로 작성하는 온프레미스 가상화·클라우드 플랫폼 (Proxmox VE / VMware vSphere 대안).

- 아키텍처: 대칭형 노드 + 내장 Raft, QEMU/KVM을 QMP로 직접 제어
- 라이선스: Apache-2.0
- 상태: 계획 1~61 완료. 진행 내역과 설계·계획 문서는 [docs/index.md](docs/index.md)

## 주요 기능

| 영역 | 기능 |
|---|---|
| 가상 머신 | 스냅샷·복제·예약 스냅샷, 설치 ISO와 cloud-init, 템플릿·링크드 클론·이미지 라이브러리, vTPM·Secure Boot·게스트 에이전트, 무중단 유형 변경(CPU·메모리·디스크·NIC), 라이브 마이그레이션(로컬 디스크 포함) |
| 스토리지 | 디렉터리, NFS, LVM-thin, ZFS, Ceph RBD(자동 구성 포함), 공유 LVM(iSCSI) |
| 네트워크 | 브리지·VLAN, VXLAN 오버레이, VM 방화벽(보안 그룹) |
| 고가용성 | HA VM과 watchdog·전원 펜싱, 위트니스 노드, 배치 규칙·HA 정책, 상태 검사, 최적 배치, 로컬 디스크 복제 HA |
| 클러스터 | 제어부 가용성, 헬스 체크·내장 VIP, 수백 노드 규모, 다중 링크, 노드 유지보수, 지표 기록·대시보드·알림·Prometheus |
| 백업·DR | 전체·증분 백업, 백업 작업, 원격 클러스터·원격 복제, VMware 가져오기 |
| 웹 UI | 영역만 둔 최소 사이드바와 영역 안 탭, ⌘K 명령 팔레트(이름·IP·동작), 저장 보기·일괄 동작 목록, 작업·알림 센터, 단축키, 휴대폰 당직 보기 |
| 접근과 보안 | 역할 기반 권한, LDAP·OIDC, 감사 로그 |
| 배포 | Debian 패키지와 설치 ISO, 업데이트 채널, 구독과 텔레메트리 |

## 빠른 시작 (macOS, Apple Silicon)

```bash
brew install qemu
mkdir -p /tmp/opl
cat > /tmp/opl/node1.toml <<'TOML'
name = "node1"
address = "127.0.0.1"
data_dir = "/tmp/opl/node1"
run_dir = "/tmp/opl/run1"
TOML
cargo run -p opld -- --config /tmp/opl/node1.toml &
cargo run -p oplctl -- cluster init --name dev --socket /tmp/opl/run1/opld.sock
# 출력된 admin 비밀번호와 CA 지문으로 로그인한다(API·웹: https://127.0.0.1:8006).
cargo run -p oplctl -- login --server https://127.0.0.1:8006 --ca-fingerprint sha256:...
```

풀·볼륨·VM 만들기, 여러 노드 클러스터, Linux(KVM) 노드와 설치 ISO는 [시작하기](docs/guide/getting-started.md)에 있다.

## 사용 안내

| 문서 | 내용 |
|---|---|
| [시작하기](docs/guide/getting-started.md) | macOS·Linux에서 실행, 설치 ISO와 패키지, 업데이트, 이름 바꾸기 |
| [가상 머신](docs/guide/vms.md) | 설치 매체·cloud-init, 템플릿·링크드 클론, 이미지, vTPM·Secure Boot, 게스트 에이전트, 무중단 유형 변경, 라이브 마이그레이션 |
| [스토리지](docs/guide/storage.md) | NFS, LVM-thin, ZFS, Ceph 자동 구성과 RBD, 공유 LVM(iSCSI) |
| [네트워크](docs/guide/network.md) | VXLAN 네트워크와 VM 방화벽 |
| [고가용성(HA)](docs/guide/ha.md) | HA VM과 펜싱, 위트니스, 배치 규칙, 상태 검사, 최적 배치, 로컬 디스크 복제 |
| [클러스터 운영](docs/guide/cluster.md) | 제어부 가용성, 앞단 프록시·VIP, 대규모 클러스터, 다중 링크, 노드 유지보수, 지표 기록·대시보드, 알림(메일·webhook), Prometheus |
| [백업·복제·가져오기](docs/guide/backup.md) | 백업과 복원, 원격 클러스터, 원격 복제(DR), VMware 가져오기 |
| [웹 화면 둘러보기](docs/guide/web-ui.md) | 영역과 탭, 명령 팔레트, 목록·일괄 동작, 알림 센터, 단축키, 휴대폰 보기 |
| [접근과 보안](docs/guide/access.md) | 웹 UI, 로그인과 세션, LDAP, SSO(OIDC), 감사 로그 |
| [구독과 텔레메트리](docs/guide/subscription.md) | 구독 키 발급·적용과 텔레메트리 |
| [개발](docs/guide/development.md) | 빌드와 시험 명령 |
| [설계와 계획](docs/index.md) | 하위 프로젝트 설계와 계획 1~61, 진행 내역 |
