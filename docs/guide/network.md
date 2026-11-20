# 네트워크

VXLAN 네트워크, VM 방화벽(보안 그룹), SDN(OVN) 네트워크. 처음 보는 사람은 [README](../../README.md)의 빠른 시작부터.

- [VXLAN 네트워크](#vxlan-네트워크)
- [VM 방화벽 (보안 그룹)](#vm-방화벽-보안-그룹)
- [SDN (OVN)](#sdn-ovn)

## VXLAN 네트워크

노드 사이를 L2로 잇는 네트워크를 orbivisord가 만든다(Proxmox SDN의 VXLAN 영역과 같은 자리). 물리 스위치에 VLAN을
설정하지 않아도 여러 노드의 VM이 한 네트워크에 있다. 네트워크마다 모든 노드(위트니스와 macOS 노드 빼고)에 브리지
`oplv<VNI>`와 VXLAN 장치 `oplx<VNI>`(UDP 4789)를 만들고, 다른 노드의 underlay 주소를 FDB에 넣어 브로드캐스트를
복제한다(유니캐스트 헤드엔드 복제, 멀티캐스트·EVPN 없음). VM NIC의 tap은 그 브리지에 붙는다.

```bash
orbivisorctl network create ov --vxlan 1000        # MTU 기본 1450
orbivisorctl network create jumbo --vxlan 1001 --mtu 8950 # underlay가 9000을 실을 때
orbivisorctl network list
# NAME   TYPE    BRIDGE    VLAN/VNI  MTU
# lan    bridge  vmbr0     10        -
# ov     vxlan   oplv1000  1000      1450
# jumbo  vxlan   oplv1001  1001      8950
orbivisorctl node list                             # UNDERLAY 열: VXLAN 패킷을 주고받는 노드 주소
orbivisorctl vm create web --memory 1024 --disk root --nic ov --start
```

웹은 네트워크 만들기에서 종류(브리지·VXLAN)를 고르고, 목록에 종류·브리지·VLAN/VNI·MTU를, 노드 목록에
"언더레이 주소"를 보인다. API는 `POST /api/v1/networks`의 `{"spec": {"vxlan": {"vni": 1000}, "mtu": 1450}}`
(`mtu`는 선택), Node `status.underlay`다.

```toml
# /etc/opl/opld.toml (노드마다, 선택)
[network]
underlay = "10.1.0.11"    # VXLAN 패킷을 주고받을 이 노드의 주소. 없으면 address가 IP면 그것, 이름이면 푼 주소(loopback·link-local 빼고 IPv4 먼저)
```

- **규칙.** `bridge`(와 `vlan`)과 `vxlan`(과 `mtu`) 중 하나만 쓴다(400). VNI는 1~16777215이고 클러스터 안에서
  겹치면 **409** `vni_in_use`("VNI 1000 is used by network \"ov\"", 같은 VNI를 동시에 만드는 두 요청은 막지 않는다
  — 그러면 두 네트워크의 VM이 같은 `oplv<VNI>`에 붙어 L2를 나누고, 브리지·VXLAN 장치·tap의 MTU는 이름이 앞선
  네트워크의 것이다). MTU는 1280~9000. `oplv<숫자>`·`oplx<숫자>` 모양의 이름은
  `--bridge`로 쓸 수 없다(400, "… is reserved for Orbivisor VXLAN networks"). 네트워크는 바꿀 수 없고(지우고 다시 만든다),
  VM이 쓰는 네트워크는 지울 수 없다(409 `network_in_use`).
- **업그레이드 먼저.** VXLAN 네트워크를 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다.
  예전 orbivisord는 새 모양의 네트워크를 읽지 못해 네트워크 목록부터 실패한다.
- **언제 생기나.** orbivisord는 시작할 때, Network·Node가 바뀔 때(1초 모아 한 번), 그리고 60초마다 장치를 맞춘다. 장치를
  만들거나 지우면 곧바로 브리지 목록을 알리므로 새 네트워크를 몇 초 안에 쓸 수 있다. 스케줄러는 브리지 네트워크와
  같이 `oplv<VNI>`가 있는 노드에만 VM을 둔다. underlay가 없는 노드(이름을 풀지 못하면 경고)에는 장치를 만들지
  않으므로 그 네트워크의 VM이 가지 않는다. 다시 시작할 때 이름을 풀지 못하면 기록된 underlay를 그대로 쓰고, 이 노드의
  underlay를 모르는 동안에는 장치를 고치지도 지우지도 않는다. 장치 만들기가 실패하면 경고를 남기고 다음 차례에 다시
  하며, 한 네트워크의 실패가 다른 네트워크를 막지 않는다. 지운 네트워크의 장치는 orbivisord가 지운다(브리지에 tap 따위
  포트가 남아 있으면 두 장치를 남기고 경고한다). orbivisord를 멈춰도 장치는 남는다(도는 VM이 계속 쓴다). `cluster leave`로
  클러스터에서 빠지면 이 노드의 장치를 지운다.
- **소유 표시.** orbivisord는 자기가 만든 두 장치의 alias를 `opl:<클러스터 태그>`(클러스터 CA 지문의 앞 12자)로 두고
  그런 장치만 고치거나 지운다. `oplv<숫자>`·`oplx<숫자>` 모양의 이름은 Orbivisor가 쓰는 예약 이름이다. 그 모양이라도
  alias가 다른 장치(관리자가 만든 것, 같은 호스트의 다른 클러스터 것)나 포트가 있거나 올라가 있는 alias 없는 장치는
  건드리지 않는다(그 네트워크만 오류로 남는다). alias 없이 내려가 있고 포트도 없는 장치는 orbivisord가 만들다 멈춘 것으로
  보고 다시 만든다. 노드의 브리지 목록에는 이 클러스터의 `oplv<VNI>`만 오르고, 다른 `oplv…`에는 VM tap을 붙이지
  않는다. `ip -d link show oplx1000`으로 id·local·dstport·alias를 볼 수 있다.
- **MTU.** 기본 1450은 underlay 1500에서 VXLAN 머리 50바이트를 뺀 값이다. underlay는 `MTU + 50`을 실어야 한다
  (점보 프레임이면 `--mtu 8950`). IPv6 underlay는 바깥 머리가 70바이트라 `MTU + 70`(기본 1450이면 1520)이다. 브리지·
  VXLAN 장치·tap이 그 MTU를 쓰고, virtio NIC는 `host_mtu`로 게스트에게도 알린다. e1000e NIC는 게스트 안에서 MTU를
  맞춘다. tap MTU는 관리자가 만든 브리지에서도 그 브리지의 MTU를 따른다(예: MTU 9000인 `vmbr0`이면 tap도 9000).
- **underlay.** 노드 사이에 **UDP 4789**를 연다. VXLAN은 암호화·인증이 없으므로 underlay는 믿을 수 있는 전용 망이어야
  한다(아무나 UDP 4789로 VM 네트워크에 패킷을 넣을 수 있다). 모든 노드의 underlay는 같은 주소 체계(IPv4끼리나 IPv6끼리)
  여야 한다. 다른 체계의 노드는 FDB에 넣지 않고 경고한다. underlay를 바꾸고 orbivisord를 다시 띄우면 장치를 다시 만들고
  다른 노드가 FDB를 고친다.
- **한계.** IPAM·DHCP·라우팅·SNAT, EVPN·BGP, 암호화는 없다(게스트 주소는 게스트나 VM이 둔 DHCP 서버가 정한다).
  노드 사이 브로드캐스트를 노드 수만큼 복제하므로 수십 노드까지가 알맞다. 브리지 `oplv<VNI>`를 다시 만들면(누가
  지웠을 때 등) 이미 도는 VM의 tap은 다시 붙지 않는다 — VM을 껐다 켠다. 한 호스트에서 두 클러스터가 같은 VNI를
  쓰면 서로 방해할 수 있다(장치 이름이 겹쳐 나중 쪽의 그 네트워크는 장치를 만들지 못한다) — 클러스터마다 VNI를 나눈다.

## VM 방화벽 (보안 그룹)

보안 그룹은 VM NIC에 거는 규칙 목록이다(Proxmox의 VM 방화벽, 클라우드의 security group과 같은 자리). 그룹을 건
NIC는 방화벽이 켜져 규칙에 맞는 것만 들어오고, 노드의 orbivisord가 nftables bridge family 규칙으로 tap에서 거른다.
그룹을 걸지 않은 NIC는 지금처럼 거르지 않는다.

```bash
orbivisorctl security-group create web --description "web servers" \
  --rule "in accept tcp 80" --rule "in accept tcp 22 10.0.0.0/8" --rule "out drop tcp 25"
orbivisorctl security-group rule add web "in accept icmp"    # 맨 끝에 더한다
orbivisorctl security-group rule add web "in drop tcp 22 10.0.9.0/24" --at 1  # 1번 자리에 넣는다
orbivisorctl security-group rule rm web 3                    # 3번 규칙(1부터)을 뺀다
orbivisorctl security-group list
# NAME  RULES  DESCRIPTION
# web   4      web servers
orbivisorctl security-group get web                          # 규칙을 순서대로(JSON)
orbivisorctl vm create web1 --memory 1024 --disk root --nic lan,sg=web+ssh --start  # NIC에 그룹 둘(web, ssh)
orbivisorctl vm nic-sg web1 0 web                            # 0번 NIC의 그룹을 web 하나로(실행 중에도 된다)
orbivisorctl vm nic-sg web1 0                                # 0번 NIC의 그룹을 모두 뺀다(방화벽 끔)
orbivisorctl security-group delete web                       # 그룹을 쓰는 NIC가 없어야 한다
```

웹은 인프라 › 보안 그룹 탭에서 그룹을 만들고 지우며, 그룹 화면의 규칙 표에서 방향·동작·프로토콜·포트·원격 CIDR을
고치고 순서를 바꿔 저장한다. VM 편집의 NIC마다 그룹을 고른다. API는 `/api/v1/security-groups`(목록·만들기·읽기·
지우기)와 `PATCH /api/v1/security-groups/{name}`(`{"description": …, "rules": […]}` 중 보낸 것만, `If-Match`로
revision), VM NIC의 `security_groups: ["web"]`, Node `status.firewall`이다.

- **규칙.** `in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]`. `in`은 VM으로 들어오는 것, `out`은 VM에서
  나가는 것이다. 프로토콜을 빼면 모든 프로토콜이다. `PORTS`는 목적지 포트 `22`나 `8000-8080`(tcp·udp에서만),
  `CIDR`은 `in`이면 보낸 쪽, `out`이면 받는 쪽 주소다(`10.0.0.0/8`, `2001:db8::/32`, 접두사를 빼면 주소 하나,
  `0.0.0.0/0`은 IPv4 전체). IPv4 CIDR은 IPv4 패킷에만, IPv6 CIDR은 IPv6에만 맞는다. `icmp`는 IPv4 CIDR과만,
  `icmpv6`는 IPv6 CIDR과만 쓴다. 그룹당 규칙 200개, NIC당 그룹 8개(겹치면 안 된다)까지다. 잘못된 규칙은 400
  (`rule 3: …`처럼 번호를 붙인다), VM이 쓰는 그룹 지우기는 409 `security_group_in_use`다.
- **순서와 기본 정책.** NIC의 그룹 순서대로, 그룹 안에서는 적힌 순서대로 보고 **처음 맞는 규칙**을 따른다. 맞는
  규칙이 없으면 **들어오기는 막고 나가기는 보낸다**. 그래서 규칙 없는 그룹 하나만 걸면 "들어오기 모두 막기, 나가기
  모두 허용"이다. 같은 브리지의 방화벽 VM 둘 사이 패킷은 보낸 VM의 `out`과 받는 VM의 `in`을 모두 통과해야 한다
  (`out accept`는 끝이 아니라 받는 쪽 검사로 넘긴다). 노드 자신과 주고받는 것(브리지에 노드 주소가 있을 때)도 같다.
- **늘 허용되는 것.** 이미 맺은 연결의 응답(conntrack `established,related`, 그래서 나가는 연결의 응답은 `in`
  규칙이 없어도 들어온다), ARP, IPv6 이웃 찾기(neighbor solicit·advert, router advert), DHCP 응답(UDP 67→68,
  547→546). 잘못된 연결 상태(`invalid`)의 패킷과 **NIC의 MAC이 아닌 보낸 MAC**(위조)은 늘 막는다.
- **NIC에 걸기.** 브리지·VXLAN 네트워크 NIC에만 건다(`user` 네트워크는 400). 없는 그룹은 400이다(VM 복제도
  원본 스펙의 그룹이 모두 있어야 한다). 노드를 정해 둔 VM에 그룹을 걸면 그 노드에 방화벽이 있어야 한다(409
  `firewall_unsupported`). 실행 중인 VM의 그룹을 바꾸거나(`vm nic-sg`, 웹 VM 편집, `PUT /api/v1/vms/{name}`) 그룹
  규칙을 바꾸면 VM을 다시 켜지 않아도 몇 초 안에 적용된다. 다만 실행 중인 VM은 그룹이 없어도 NIC를 더하거나 빼거나
  순서·MAC을 바꿀 수 없고, 그룹을 건 NIC가 있는(바꾸기 전이나 뒤) VM은 실행 중에 **그룹만** 바꿀 수 있다 — 먼저
  끈다(409 `vm_running`; 규칙은 NIC 순번과 MAC으로 tap을 찾으므로 도는 tap과 어긋나지 않게). 도는 NIC의 네트워크·모델
  바꾸기는 그룹이 없어도 409다(이 버전의 노드가 켠 VM — 마이그레이션 대상이 도는 NIC로 켠다). 모든 노드가 `hotplug`를
  알면 실행 중에도 NIC를 끝에 붙이고 실행 중 붙인 것을 끝에서부터 뗄 수 있다(그룹을 건 NIC는 붙이기 전에 규칙을 먼저
  넣는다) — [VM 무중단 유형 변경](vms.md#vm-무중단-유형-변경).
- **누가 바꾸나.** 그룹을 만들고 바꾸고 지우는 것은 admin만이다. VM NIC에 그룹을 걸거나 떼는 것은 VM 쓰기
  권한(operator 포함)이라 operator도 VM의 방화벽을 끌 수 있다(그룹을 모두 떼면 거르지 않는다).
- **실시간 적용.** 노드마다 orbivisord가 클러스터의 모든 방화벽 NIC를 테이블 `bridge opl_<클러스터 태그>`(CA 지문 앞 12자)
  하나로 만들어 `nft -f`로 한 번에 바꾼다(바꾸는 동안 규칙이 빈 순간이 없다). orbivisord 시작, VM·보안 그룹이 바뀔 때
  (0.5초 모아 한 번), 그리고 60초마다(누가 테이블을 지웠어도 되살린다) 넣는다. 그룹을 건 VM은 켜기(마이그레이션
  받기 포함) 직전에 규칙을 넣고, 넣지 못하면 켜지 않는다("failed to start: firewall: …", 방화벽 없이 도는 순간이
  없다). 이미 도는 VM에 다시 붙거나 멈춘 채 남은 VM을 이어서 실행할 때(orbivisord를 다시 시작한 뒤 등)도 먼저 넣는다. 방화벽 NIC가 하나도
  없으면 테이블을 지운다. orbivisord를 멈춰도 규칙은 남고(도는 VM을 계속 지킨다), `cluster leave`로 빠지면 지운다.
  `nft list table bridge opl_<태그>`로 볼 수 있다(NIC마다 체인 `o_<tap>`·`i_<tap>`). 없는 그룹을 건 NIC(그룹을
  저장소에서 바로 지운 때 등)는 들어오기와 나가기를 모두 막는다.
- **비는 때와 실패.** 누가 `nft flush ruleset`을 하거나 `nftables.service`를 다시 읽으면(`systemctl reload
  nftables`, 보통 `flush ruleset`으로 시작한다) 테이블이 사라져 **다음 60초 차례까지 그 노드의 방화벽 VM이 열린다.**
  nftables 설정을 다시 읽은 뒤에는 orbivisord를 다시 시작하면 곧바로 되살린다. 에이전트의 규칙 넣기 실패(VM 켜기 직전
  확인 말고)는 orbivisord 로그에 경고로만 남고 API·웹에는 보이지 않는다 — 그동안은 마지막으로 넣은 규칙이 남는다.
- **노드 요구.** Linux, `nftables`(`nft` 명령), bridge family conntrack이 되는 커널 5.3 이상, root로 도는 orbivisord.
  orbivisord는 시작할 때 이 클러스터의 테이블을 실제로 넣어 보아(`table bridge opl_<태그> {}` — 있는 규칙은 그대로 둔다)
  되면 `node get`의 `status.firewall: true`를 알리고, 안 되면(nftables 없음, root 아님) 경고와 함께 `false`다.
  이 시험은 시작 때 한 번만 하므로, 실패했다면 nftables를 고친 뒤 **orbivisord를 다시 시작해야** 방화벽이 켜진다.
  스케줄러는 그룹을 건 VM을 `true`인 노드에만 둔다. 위트니스와 macOS 노드는 방화벽이 없다.
- **업그레이드 먼저.** 보안 그룹을 VM NIC에 걸기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다.
  예전 orbivisord는 `security_groups`가 있는 NIC를 읽지 못한다.
- **br_netfilter와 호스트 iptables.** 이 규칙은 브리지 단계에서 걸러 br_netfilter가 켜져 있어도 그대로 적용된다.
  다만 br_netfilter가 켜져 있고 `net.bridge.bridge-nf-call-iptables`가 1이면 브리지를 지나는 VM 트래픽이 호스트의
  iptables(`FORWARD`)도 지난다. Docker처럼 `FORWARD` 기본 정책을 DROP으로 두는 프로그램이 있으면 보안 그룹이 허용한
  VM 트래픽도 막힐 수 있다 — 그 sysctl을 0으로 두거나 iptables에서 VM 브리지를 허용한다.
- **한계.** 걸러내는 것은 L3·L4 주소·포트와 보낸 MAC이다. 보낸 IP 위조(MAC은 맞고 IP만 다른 것)와 ARP 위조는
  막지 않는다. 규칙에서 허용을 빼도 이미 맺은 연결은 끊기지 않는다(conntrack 항목이 끝날 때까지 간다). 다른 그룹을
  원격으로 가리키는 규칙, 로그·카운터, 속도 제한, 기본 정책 바꾸기는 없다.

## SDN (OVN)

SDN 네트워크는 [OVN](https://www.ovn.org/)의 논리 스위치다(계획 65). 서브넷을 두면 **Orbivisor가 VM NIC의 IP 주소를 정하고
(IPAM) OVN 내장 DHCP가 그 주소를 준다.** OVN은 orbivisord가 apt로 설치하고 세우며, Orbivisor는 OpenStack Neutron
ML2/OVN이 하는 자리(OVN에 원하는 상태를 쓰는 관리 시스템)를 맡는다. 지금의 브리지·VLAN·VXLAN 네트워크와 보안 그룹은
"단순 네트워크"로 그대로 있고, 한 VM에 단순 NIC와 SDN NIC를 섞을 수 있다. 관리자가 `sdn init`을 하기 전에는 아무것도
설치하지 않고 OVN에 붙지도 않는다.

| | Proxmox VE SDN | OpenStack Neutron ML2/OVN | Orbivisor 계획 65 |
|---|---|---|---|
| 데이터 평면 | Linux 브리지 + VLAN·QinQ·VXLAN·EVPN(FRR) | OVS + OVN(Geneve) | OVS + OVN(Geneve) |
| 제어 | `/etc/pve/sdn` 설정을 노드마다 ifupdown2로 적용 | API가 NB에 쓰고 주기적으로 고침 | Raft의 원하는 상태를 리더 `OvnSync`가 NB에 맞춤 |
| OVN 설치 | 해당 없음 | 배포 도구(kolla·charm 따위)가 따로 | orbivisord가 Task로(중앙 1·3·5, 노드마다 chassis) |
| 서브넷·IPAM | VNet › 서브넷, pve·NetBox·phpIPAM | 서브넷, Neutron DB | 네트워크 안 서브넷, Raft(NIC 스펙의 `ips`) |
| DHCP | dnsmasq(simple 영역) | OVN 내장 | OVN 내장 DHCPv4 |
| 라우터·NAT·플로팅 IP | EVPN 출구·SNAT | 분산 라우터, 게이트웨이 HA | 계획 66 |
| 테넌트 | 없음(권한 경로) | 프로젝트 | [네트워크 폴더](folders.md) |

### 개념

- **중앙(central).** OVN의 northbound DB(NB, 원하는 논리 망), southbound DB(SB, chassis와 바인딩), `ovn-northd`(NB를 SB의
  흐름으로 바꿈)가 도는 노드다. NB·SB는 ovsdb의 Raft 클러스터이고, northd는 SB 잠금으로 한 번에 하나만 일한다. 중앙은
  관리자가 고른 **고정 집합**(1·3·5개)이고 Orbivisor의 투표 멤버를 저절로 따라가지 않는다(Ceph mon과 같은 방식).
- **chassis.** VM을 돌리는 노드의 `ovn-controller`와 Open vSwitch다. SB에서 흐름을 받아 `br-int`에 깔고, 다른 chassis와
  Geneve(UDP 6081)로 잇는다. chassis 이름은 **노드 UUID**(노드 인증서 CN과 같음), Geneve 주소는 노드의 underlay 주소
  (VXLAN과 같은 값, `node list`의 UNDERLAY)다. 위트니스와 macOS 노드는 chassis가 될 수 없다.
- **논리 스위치.** SDN 네트워크 하나가 NB의 `Logical_Switch` `opl-<네트워크 id>` 하나다. 네트워크끼리는 L2가 섞이지 않고,
  계획 65에는 라우터가 없으므로 L3로도 닿지 않는다(고립 망). 그래서 폴더(테넌트)가 다른 네트워크는 같은 CIDR을 써도 된다.
- **서브넷.** 네트워크마다 IPv4 하나·IPv6 하나까지. CIDR(IPv4 `/8`~`/29`, IPv6 `/48`~`/124`), 게이트웨이(없으면 첫 호스트
  주소 — 늘 예약해 두고 계획 66의 라우터가 쓴다), DHCP(IPv4 기본 켬, IPv6는 끔만), DNS 서버 4개까지, 할당 범위(pool) 16개까지,
  설명.
- **IPAM.** 주소의 출처는 Raft의 VM 스펙(`nics[].ips`)이다. API가 MAC처럼 새 NIC에 서브넷의 가장 낮은 빈 주소를 주고,
  직접 고른 주소(`ip=`)면 그것을 검사한다. 한 네트워크 안에서 같은 주소를 두 NIC가 갖지 못하게 Raft 상태 머신이 막는다
  (MAC과 같은 겹침 검사 — 두 API 노드가 동시에 골라도 하나만 되고 다른 쪽은 다음 빈 주소로 다시 고른다).
- **DHCP.** OVN 내장이라 DHCP 서버 VM·에이전트가 없고 노드마다 ovn-controller가 답한다. 논리 포트의 주소만 답한다(Orbivisor가
  정한 주소만 나감). 임대 86400초, MTU, DNS를 주고 **기본 경로(`router`)는 주지 않는다** — 라우터가 없고, 단순 NIC와 함께
  쓰는 VM의 기본 경로를 빼앗지 않게.
- **포트 보안.** NIC마다 논리 포트 `opl-<VM id>-<MAC 12자리>`가 생기고 `port_security`가 늘 켜져 있다. NIC는 자기 MAC과
  받은 주소로만 보낼 수 있다(MAC·IP 위조와 ARP 위조를 OVN이 막는다). 꺼진 VM의 포트도 NB에 남아 주소를 잡아 둔다.

### 설정하기

**노드 준비.** 중앙·chassis는 Linux(Ubuntu 24.04·Debian 13 — OVN 24.03 이상)이고 apt 저장소에 닿아야 한다. OVN 꾸러미
(`ovn-central`·`ovn-host`·`openvswitch-switch`, chassis는 뒤의 둘)는 **설정 Task가 깐다** — 미리 깔 필요가 없고, 빠진 것만
`apt-get install`한다. 노드에 underlay 주소가 있어야 한다(없으면 400 "node has no underlay address", `[network] underlay`는
[VXLAN 네트워크](#vxlan-네트워크)). 다음 포트를 관리망 안에서 연다:

| 포트 | 어디 사이 | 쓰임 |
|---|---|---|
| UDP 6081 | 모든 chassis 사이(underlay) | Geneve |
| TCP 6641·6642 | 모든 노드 → 중앙 | NB·SB(클러스터 CA mTLS) |
| TCP 6643·6644 | 중앙끼리 | NB·SB의 Raft |

**중앙은 셋을 고른다.** 하나면 그 노드가 죽을 때 OVN 제어 평면이 멈추고(도는 VM의 흐름은 남는다), 셋이면 하나를, 다섯이면
둘을 견딘다. 짝수는 하나 적은 홀수보다 더 견디지 못하므로 경고가 나온다. 장애 영역이 다른 투표 멤버 셋이 좋다.

```bash
orbivisorctl sdn init n1                  # n1에 꾸러미 설치, NB·SB Raft 클러스터와 northd, n1을 chassis로
# sdn: installing-packages on n1  … (단계마다 stderr에 한 줄)
# sdn: OVN is ready (central and chassis on n1)
orbivisorctl sdn central add n2           # 중앙은 한 번에 하나씩 바뀐다(둘이면 짝수 경고)
orbivisorctl sdn central add n3
orbivisorctl sdn chassis install --all    # 나머지 Linux 노드(이미 준비된 chassis·위트니스·macOS 빼고, 오프라인은 건너뛰고 알림)
orbivisorctl sdn chassis install n4 n5    # 노드를 골라서
orbivisorctl sdn status
# phase: ready
# OVN: 24.03.8
# sync: term 7 index 4182 at … (nb_cfg 12, chassis caught up)
#
# CENTRAL  ADDRESS   NB      SB        NORTHD
# n1       10.1.0.11 leader  follower  active
# n2       10.1.0.12 follower leader   standby
# n3       10.1.0.13 follower follower standby
#
# NODE  CHASSIS  ENCAP-IP   OVN      ERROR
# n1    ready    10.1.0.11  24.03.8  -
# …
orbivisorctl sdn central remove n3        # 노드가 온라인이어야 한다. 마지막 중앙은 뺄 수 없다(409 sdn_last_central)
```

웹은 **인프라 › 네트워크 › SDN** 보기의 "SDN 설정 시작" 마법사다(① 중앙 노드 — 투표 멤버 셋을 미리 고름 ② 설치 진행과
나머지 중앙의 합류 ③ chassis — 노드마다·"모든 노드에 설치"). 자세한 것은 [웹 화면 둘러보기](web-ui.md#sdn-화면). API는
`GET/POST /api/v1/sdn`(`{"node": "n1", "address": "10.1.0.11"}`), `POST /api/v1/sdn/centrals`(`{"node": "n2"}`),
`DELETE /api/v1/sdn/centrals/{node}`, `POST /api/v1/sdn/chassis`(`{"nodes": [...]}` 또는 `{"all": true}`, 노드마다 Task 목록)이다.
모두 루트 `cluster.admin`(읽기는 `cluster.read`)이다.

- **무엇을 하나.** 중앙 Task는 빠진 꾸러미를 깔고, 노드 TLS를 `/etc/opl/ovn/`에 베끼고, 배포판의 중앙 단위(`ovn-central`·
  `ovn-northd`·`ovn-ovsdb-server-nb`·`-sb`)를 끄고 가린 뒤(끈 목록은 `/etc/opl/ovn/distro-units`) Orbivisor 단위
  `opl-ovn-nb`·`opl-ovn-sb`·`opl-ovn-northd`를 띄운다. DB는 `/var/lib/opl/ovn/{nb,sb}.db`, 로그는 `/var/log/opl/ovn-*.log`다.
  NB·SB는 노드 클러스터 주소 하나에만 듣는다(`--address`로 노드 인증서 안의 다른 주소를 고를 수 있다). chassis 구성은
  로컬 OVS의 `external_ids`(`system-id`=노드 UUID, `ovn-remote`=모든 중앙의 `ssl:…:6642`, `ovn-encap-type=geneve`,
  `ovn-encap-ip`=underlay)와 `ovs-vsctl set-ssl`이다.
- **다시 하기.** Task는 단계마다 지금 상태를 먼저 보므로 실패한 것을 같은 노드로 다시 부르면 이어서 한다(DB를 다시 만들지
  않는다). 중앙 Task는 클러스터에서 한 번에 하나다(바쁘면 409 `sdn_busy`). apt 때문에 시간 제한이 없다.
- **아무것도 몰래 지우지 않는다.** 다른 OVN·OpenStack이 쓰는 노드(`ovn-remote`가 이미 있고 이 클러스터 표시가 아님),
  Raft가 모르는 클러스터 id의 DB, 논리 스위치가 든 배포판 NB DB가 있으면 거절한다(400, 까닭과 함께). 중앙에서 빠진 노드의
  DB는 `*.left-<시각>`으로 이름만 바꾼다.
- **새 노드에는 저절로 깔지 않는다.** 나중에 합류한 노드는 `sdn chassis install`로 고른다(꾸러미 설치는 늘 명시적).
- **노드 SDN 에이전트.** 노드마다 orbivisord가 시작할 때, 중앙 목록·underlay가 바뀔 때, 60초마다(`[sdn] refresh_secs`)
  `ovn-remote`·`encap-ip`·TLS 사본을 맞추고, 중앙이면 멈춘 단위를 다시 켜고 OVN 꾸러미를 올린 뒤의 DB 스키마 변환을 한다.
  재부팅 뒤 장치가 없는 이 클러스터의 OVS 포트를 지운다(남의 포트는 둔다). 노드 상태 `status.sdn`(chassis 상태
  `missing`·`configuring`·`ready`·`down`, OVN·OVS 판, encap 주소, 중앙이면 NB·SB Raft 역할과 northd 활성)을 알린다.
  `cluster leave`면 chassis 구성의 이 클러스터 칸만 지우고 ovn-controller를 멈춘다(꾸러미·DB는 남김).
- **노드 지우기.** 중앙인 노드는 먼저 `sdn central remove`(409 `sdn_central_on_node`). chassis만인 노드의 SB `Chassis` 행은
  리더가 지운다.

```toml
# /etc/opl/opld.toml (노드마다, 선택 — 기본값)
[sdn]
central_packages = ["ovn-central", "ovn-host", "openvswitch-switch"]
host_packages = ["ovn-host", "openvswitch-switch"]
install_timeout_secs = 1800   # apt-get update·install 하나의 한도
refresh_secs = 60             # 에이전트가 다시 맞추는 간격
```

### 네트워크와 서브넷

```bash
orbivisorctl network create tenant-a --sdn --subnet 10.10.0.0/24,dns=10.0.0.53
orbivisorctl network create tenant-b --sdn --mtu 1400 \
  --subnet 10.20.0.0/24,gateway=10.20.0.254,pool=10.20.0.10-10.20.0.199 --subnet fd00:20::/64 --folder /teamB
orbivisorctl network list
# NAME      TYPE  BRIDGE  VLAN/VNI  MTU   SUBNETS
# tenant-a  sdn   br-int  -         1442  10.10.0.0/24
# tenant-b  sdn   br-int  -         1400  10.20.0.0/24,fd00:20::/64
orbivisorctl network ips tenant-a                                    # VM을 붙인 뒤
# SUBNET        SIZE  USED  FREE
# 10.10.0.0/24  253   2     251
#
# VM    NIC  MAC                ADDRESSES  UP   NODE
# web1  0    52:54:00:12:34:56  10.10.0.2  yes  n1
# db1   0    52:54:00:ab:cd:ef  10.10.0.3  no   -
orbivisorctl network subnet add tenant-a fd00:10::/64                # 체계마다 하나(있던 NIC는 --fill로)
orbivisorctl network subnet set tenant-a 10.10.0.0/24,dns=1.1.1.1+8.8.8.8,pool=10.10.0.100-10.10.0.199
orbivisorctl network subnet remove tenant-b fd00:20::/64             # 그 주소를 쓰는 NIC가 없어야 한다
```

`--subnet`은 `CIDR[,gateway=IP][,dhcp=on|off][,dns=IP+IP][,pool=A-B+C-D][,description=TEXT]`이다. 웹은 네트워크 만들기의
종류 "SDN(OVN)"과 서브넷 표, 목록에서 이름을 누르면 여는 서랍(서브넷 편집과 IP 표)이다. API는 `POST /api/v1/networks`의
`{"spec": {"sdn": {}, "mtu": 1442, "subnets": [{"cidr": "10.10.0.0/24", "dns_servers": ["10.0.0.53"]}]}}`,
`PATCH /api/v1/networks/{name}`(`{"subnets": [...]}` — 목록 전체, `If-Match`), `GET /api/v1/networks/{name}/ips`다.

- **MTU.** 기본 1442(underlay 1500에서 Geneve 머리 58바이트를 뺀 값, Neutron과 같음), 1280~9000. IPv6 underlay면 1422 이하로
  둔다. tap MTU와 virtio `host_mtu`, DHCP의 MTU 옵션이 이 값이다.
- **먼저 SDN을 세운다.** SDN이 준비되지 않았으면 409 `sdn_not_ready`. 서브넷 오류는 400 `invalid_subnet`(`subnet 2: …`처럼
  번호), 단순 네트워크 바꾸기는 409 `network_immutable`. 브리지 이름 `br-int`는 브리지 네트워크에 쓸 수 없다.
- **서브넷 바꾸기.** DHCP·DNS·할당 범위·설명은 바꿀 수 있고 CIDR·게이트웨이는 바꿀 수 없다(빼고 다시 더한다). 할당 범위를
  줄여도 이미 준 범위 밖 주소는 남는다. 서브넷을 빼려면 그 주소를 쓰는 NIC가 없어야 한다(409 `subnet_in_use`와 VM 이름들).
- **서브넷을 더해도 있던 NIC에는 주소를 주지 않는다.** VM 스펙을 몰래 바꾸지 않으려는 것이다. 꺼진 VM의 빠진 주소는
  `orbivisorctl network ips tenant-a --fill`로 채운다(CLI만 — 켜진 VM은 건너뛰고 알린다). 새로 붙는 NIC는 곧바로 받는다.
- **`network ips`의 UP·NODE**는 API 노드가 OVN에서 읽은 논리 포트의 `up`과 바인딩한 chassis다. OVN을 읽지 못하면 `-`이고
  까닭이 경고로 나온다(주소 표는 Raft에서 오므로 그대로 나온다).
- **권한.** SDN 네트워크는 VXLAN처럼 노드 자원이 아니라서 네트워크 폴더의 `network.config`로 만들고 바꾸고 지운다. NIC
  붙이기와 `ip=` 고르기는 그 네트워크의 `network.assign`, `ips` 보기는 네트워크 읽기다. 네트워크 폴더가 곧 테넌트다
  ([폴더와 권한](folders.md)).

### VM NIC 주소

```bash
orbivisorctl vm create web1 --memory 1024 --disk root --nic tenant-a --start            # 10.10.0.2를 받는다
orbivisorctl vm create db2 --memory 2048 --disk root --nic tenant-a,ip=10.10.0.50+fd00:10::50
orbivisorctl vm create gw --memory 512 --disk root --nic lan --nic tenant-a              # 단순 NIC와 섞기
orbivisorctl vm get db2                     # nic 0 (tenant-a): 10.10.0.50, fd00:10::50
orbivisorctl vm clone web1 web2 --ip 10.10.0.20                 # 복제는 새 주소(주지 않으면 서브넷에서)
orbivisorctl template deploy ubuntu --name app1 --ip nic1=10.10.0.30   # NIC 1에. --count 2 이상과는 함께 못 씀
orbivisorctl import run esx-db --ip 10.10.0.40                # 가져오기도 같은 --ip
```

- **`ip=`**는 체계마다 하나까지(`A+B`)이고 서브넷 안이어야 한다. 할당 범위 밖이어도 서브넷 안이면 받는다(Neutron의 고정 IP).
  예약 주소(망·브로드캐스트·게이트웨이)나 서브넷 밖은 400, 다른 NIC가 쓰는 주소는 409 `ip_in_use`, 빈 주소가 없으면 409
  `ip_exhausted`, SDN이 아닌 네트워크에 주면 400 `ips_without_sdn`.
- **NIC 일생.** 새 NIC(새 VM, 붙인 NIC, 네트워크를 바꾼 NIC)는 서브넷마다 주소를 받는다. 있던 NIC를 주소 없이 보내면(주소를
  모르는 클라이언트의 `PUT`) 지금 주소를 둔다 — 그래서 `PUT`으로 주소를 비울 수는 없다. NIC를 빼거나 VM을 지우면 주소가
  풀린다. 템플릿으로 바꾸면 주소를 지우고, 템플릿에서 되돌리면 새로 받는다. 복제·템플릿 배포·가져오기는 새 주소(`--ip`로
  고름). 백업 복원은 원래 주소가 비어 있으면 그대로, 아니면 새로 받고 Task 경고에 적는다. 스냅샷 되돌리기는 주소도
  되돌리고, 그 사이 다른 VM이 그 주소를 잡았으면 409 `ip_in_use`(몰래 바꾸지 않는다). 라이브 마이그레이션·HA는 그대로.
- **켜진 VM의 주소는 바꿀 수 없다**(409 `vm_running`). 포트 보안이 새 주소만 받아 게스트의 옛 주소가 곧바로 막히기 때문이다.
  켜진 VM에 새로 붙이는 NIC는 주소를 받는다(핫플러그).
- **게스트 쪽.** IPv4 서브넷의 DHCP가 켜져 있으면 게스트가 DHCP로 받는다(cloud-init이 없는 이미지도). cloud-init이 있는 VM은
  network-config에 SDN NIC를 IPv4 `dhcp`(DHCP를 끈 서브넷이면 `static` 주소/접두사·DNS), IPv6 `static`으로 적는다 — 게이트웨이는
  적지 않는다(라우터가 없음). SDN NIC 자리의 `cloud_init.networks[i]`는 빈 `{}`만 받는다(400 `cloud_init_sdn_nic` — 주소는
  IPAM이 정한다). NIC 주소가 바뀌면 instance-id가 바뀌어 게스트가 다시 적용하고, 서브넷의 DNS·DHCP만 바꾸면 그대로다(DHCP
  NIC는 임대 갱신 때 받는다).
- **보안 그룹.** SDN NIC에는 걸 수 없다(400 `sdn_security_groups`). 포트 보안이 위조만 막고 들어오는 것을 거르지 않으니,
  같은 SDN 네트워크의 VM끼리는 서로 닿는다. 네트워크가 밖과 이어지지 않으므로 밖에서는 닿지 않는다.

### 켜기·마이그레이션·HA

- **스케줄러.** SDN NIC가 있는 VM은 chassis가 `ready`인 노드에만 간다(마이그레이션 대상·HA 복구·비우기도 같다). 둘 곳이
  없으면 "… is an SDN (OVN) chassis for VMs on SDN networks"가 든 409다.
- **켜기.** tap을 `br-int`에 붙이고(OVS `external_ids:iface-id`=논리 포트) QEMU를 띄운다. 리더가 그 포트의
  `requested-chassis`를 이 노드로 바꾸면 ovn-controller가 바인딩한다(보통 1초 안). 바인딩을 기다리느라 켜기를 막지 않는다
  — 게스트 DHCP가 다시 묻는다. 10초 안에 바인딩되지 않으면 orbivisord 로그에 경고만 남는다.
- **라이브 마이그레이션.** 옮기는 동안 리더가 `requested-chassis`를 "원본,대상"으로, `activation-strategy=rarp`로 둔다. 받는
  쪽은 받을 QEMU를 띄우기 전에 tap을 붙이고 OVN이 포트를 깔기(`ovn-installed`)를 30초까지 기다린다 — 안 오면 받기를 거절해
  마이그레이션이 시작 전에 깨끗이 실패하고 VM은 원본에서 그대로 돈다. QEMU가 끝에 보내는 RARP가 대상 쪽 포트를 열고, 끝나면
  리더가 대상 하나로 되돌린다. 주소·MAC은 그대로다.
- **HA·비우기**는 다시 켜기와 같다. 죽은 노드의 포트는 `requested-chassis`가 새 노드로 바뀌며 넘어간다.
- **중앙이 정족수를 잃으면** 도는 VM의 흐름은 ovn-controller가 가진 마지막 것으로 계속 간다. 새 바인딩·바꾸기는 정족수가
  돌아올 때까지 미뤄지고 `sdn status`의 sync 오류와 중앙의 Raft 역할에 보인다.

### OvnSync: Raft에서 OVN으로

API는 Raft만 쓰고 OVN을 부르지 않는다. **리더의 `OvnSync`**가 Raft의 SDN 네트워크·서브넷·VM NIC를 NB의 행
(`Logical_Switch`·`Logical_Switch_Port`·`DHCP_Options`)으로 그려 지금 NB와 다른 것만 한 트랜잭션에 쓴다. Network·Vm·Sdn이
바뀌면 0.5초 모아 한 번, 리더가 된 직후, 그리고 60초마다(누가 NB를 고쳐도 되살린다) 돈다. NB는 다시 만들 수 있는
파생물이다 — 지워도 다음 차례에 되살아난다.

- **주인 표시.** Orbivisor가 만든 NB 행은 `external_ids:orbivisor:cluster=<클러스터 태그>`를 갖고, OvnSync는 그런 행만 바꾸고
  지운다. 관리자가 `ovn-nbctl`로 만든 행은 남는다.
- **펜싱.** 쓰기마다 NB_Global의 토큰 `"<Raft 임기>:<인덱스>"`를 `wait`으로 확인하고 올린다. 더 큰 임기의 토큰을 본 옛 리더는
  쓰지 않는다. 그래서 리더가 바뀌는 순간에도 낡은 상태가 새 리더의 쓰기를 덮지 못한다.
- **동기화 보기.** `sdn status`의 `sync`(임기·인덱스·시각·오류, `nb_cfg`와 chassis가 따라잡았는지 — "caught up"이면 모든
  chassis가 반영함), 웹 SDN 보기의 "마지막 동기화".

### 버전이 섞인 클러스터

모든 SDN 칸(`Sdn` 리소스, 네트워크의 `sdn`·`subnets`, NIC `ips`, 노드 `status.sdn`, Task `sdn-*`)은 기능 `sdn` 뒤에 있다.
**클러스터의 모든 노드를 이 버전으로 올리기 전에는** `/sdn` 쓰기, SDN 네트워크 만들기, NIC 주소가 409 `cluster_upgrading`이고,
노드는 `status.sdn`을 쓰지 않는다(옛 노드가 새 칸을 읽지 못하므로). 올린 뒤에도 `sdn init`을 하기 전에는 OvnSync와 에이전트가
쉬어서 SDN을 쓰지 않는 클러스터는 지금과 바이트까지 같다(꾸러미도 찾지 않는다).

### 보안

- **OVN 연결은 모두 클러스터 CA mTLS다.** NB·SB·Raft 포트는 `pssl`만 열고(평문 `ptcp` 없음) 노드 인증서
  (`/etc/opl/ovn/{ca.crt,node.crt,node.key}`, `{data_dir}/pki`의 사본, 0600 root, 디렉터리 0700)로 서로 확인한다. 이 클러스터의
  노드 인증서가 없으면 NB·SB에 붙을 수 없다(다른 CA의 인증서·인증서 없음·평문은 거절 — `ovn` CI 작업이 본다). 노드 인증서가
  다시 서명되면 에이전트가 사본을 바꾼다.
- OVS·OVN 서버는 상대 인증서의 CA만 보고 이름은 보지 않는다. Orbivisor의 OVSDB 클라이언트(OvnSync)는 서버 인증서의 이름도
  중앙 주소·노드 주소와 견준다.
- 로컬 `punix` 소켓(`/run/opl/ovn/{nb,sb}.sock`)은 root만 쓴다.
- Geneve(UDP 6081)는 VXLAN처럼 암호화하지 않는다. underlay는 믿을 수 있는 망이어야 한다.
- 계획 65에서 chassis는 SB의 다른 chassis 행도 쓸 수 있다(SB RBAC는 계획 67).

### 문제 풀기

```bash
# NB 보기(중앙 노드에서, root). 팔로어의 소켓에는 ovn-nbctl의 기본(--leader-only)이 붙지 못한다 — 리더 노드에서 하거나:
ovn-nbctl --db=unix:/run/opl/ovn/nb.sock --no-leader-only show
ovn-sbctl --db=unix:/run/opl/ovn/sb.sock --no-leader-only show          # chassis와 포트 바인딩
ovs-appctl -t /run/opl/ovn/nb.ctl cluster/status OVN_Northbound          # NB Raft(SB는 sb.ctl·OVN_Southbound)
ovn-appctl -t ovn-controller connection-status                          # chassis가 SB에 붙었나
ovs-vsctl list Open_vSwitch . | grep external_ids                       # system-id·ovn-remote·encap
ovs-vsctl --columns=name,external_ids list Interface             # VM tap의 iface-id(논리 포트)·ovn-installed
journalctl -u opl-ovn-nb -u opl-ovn-sb -u opl-ovn-northd; less /var/log/opl/ovn-nb.log
```

- `sdn status`의 CHASSIS가 `down`이면 ovn-controller가 SB에 붙지 않은 것이다(중앙에 닿는지, TCP 6642). `configuring`은 붙었으나
  SB에 chassis 행이 아직 없는 것이다.
- VM이 주소를 받지 못하면 `network ips`의 UP을 본다. `no`면 포트가 바인딩되지 않았다(chassis 상태, orbivisord 로그의 "port
  not bound" 경고). 게스트가 서브넷의 DHCP를 켰는지, 게스트 안 MTU가 1442 이하인지 본다.
- 배포판 단위를 되살리려면 `/etc/opl/ovn/distro-units`의 단위를 `systemctl unmask`·`enable`한다(Orbivisor 단위를 먼저 멈춘다).

### 계획 65가 아직 하지 않는 것

- **라우터·밖으로 나가기 없음.** SDN 네트워크는 고립된 L2다. 라우터, 외부(공급자) 네트워크, 게이트웨이 HA, SNAT, 플로팅 IP는
  **계획 66**이다. 그 전에 밖과 이으려면 VM에 단순 NIC를 함께 둔다.
- **SDN NIC에 보안 그룹 없음.** 보안 그룹을 OVN `Port_Group`·`ACL`로 옮기는 일은 **계획 66**이다(포트 보안만 켜짐).
- **DHCPv6·RA 없음**(**계획 66**). IPv6 주소는 cloud-init이 고정으로 넣는다.
- **부하 분산, QoS, 단순 → SDN 옮기기 도구**(VM NIC를 VXLAN·브리지 네트워크에서 SDN 네트워크로), 토폴로지 화면, 네트워크 폴더
  할당량, SB RBAC, OVN 상태 알림은 **계획 67**이다.
- BGP·EVPN, OVN 연동(IC), 하드웨어 오프로드·DPDK, SDN 끄기와 OVN 지우기, 중앙 자동 교체는 나중이다.

### 한계

- **죽은 중앙 노드는 뺄 수 없다.** 빼기 Task가 그 노드에서 돌아야 하므로 오프라인 중앙은 409다. 노드를 되살리거나, 손으로
  `ovs-appctl … cluster/kick`을 한 뒤의 정리는 아직 길이 없다(계획 66 이후).
- **여러 노드에서 실제로 보지 못한 것.** chassis 둘 사이의 Geneve, 라이브 마이그레이션의 두 chassis 바인딩, 중앙 셋의 실제 노드
  장애는 시험 장비가 노드 하나라 단위·가짜 OVSDB 시험으로만 보았다(`ovn` CI 작업은 노드 하나에 중앙·chassis와 127.0.0.2의
  두 번째 ovsdb 서버).
- "10초 안에 포트가 바인딩되지 않음" 경고는 orbivisord 로그에만 있고 API·웹에는 없다.
- 있던 NIC 채우기(`--fill`)는 CLI만이다. 웹·API에는 없다.
- 서브넷은 체계마다 하나, DHCP 릴레이·여러 서브넷·DNS 이름은 없다. 템플릿 배포 `--count` 2 이상에는 주소를 고를 수 없다.
- 원격 복제(계획 55)를 받는 클러스터에는 같은 이름의 SDN 네트워크가 있어야 하고, 주소가 겹치면 받기가 실패한다.
- 중앙 판은 첫 중앙의 OVN 판과 주·부 판이 같아야 한다(꾸러미를 올릴 때는 모든 중앙을 함께).
