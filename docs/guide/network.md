# 네트워크

VXLAN 네트워크, VM 방화벽(보안 그룹), 물리 네트워크(포트·본딩·LLDP), SDN(OVN) 네트워크. 처음 보는 사람은 [README](../../README.md)의 빠른 시작부터.

- [VXLAN 네트워크](#vxlan-네트워크)
- [VM 방화벽 (보안 그룹)](#vm-방화벽-보안-그룹)
- [물리 네트워크: 포트·본딩·LLDP](#물리-네트워크-포트본딩lldp)
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
# /etc/orbivisor/orbivisord.toml (노드마다, 선택)
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

웹은 네트워크 › 방화벽(보안 그룹)에서 그룹을 만들고 지우며, 그룹 화면의 규칙 표에서 방향·동작·프로토콜·포트·원격 CIDR을
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
- **NIC에 걸기.** 브리지·VXLAN 네트워크 NIC와 SDN 네트워크 NIC(계획 66 — OVN ACL, [SDN NIC의 보안
  그룹](#sdn-nic의-보안-그룹))에 건다(`user` 네트워크는 400). 없는 그룹은 400이다(VM 복제도
  원본 스펙의 그룹이 모두 있어야 한다). 노드를 정해 둔 VM에 그룹을 걸면 그 노드에 방화벽이 있어야 한다(409
  `firewall_unsupported` — 단순 NIC만 본다. SDN NIC만 그룹이 있는 VM은 nftables가 없는 chassis에도 간다). 실행 중인 VM의 그룹을 바꾸거나(`vm nic-sg`, 웹 VM 편집, `PUT /api/v1/vms/{name}`) 그룹
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

위 설명(nftables, 노드마다의 테이블, `status.firewall`)은 **단순 NIC**의 것이다. 같은 그룹을 SDN NIC에 걸면 뜻(순서, 처음
맞는 규칙, 기본 정책, 늘 허용되는 것)은 같고 OVN ACL이 집행한다 — 다른 점 셋은 [SDN NIC의 보안 그룹](#sdn-nic의-보안-그룹)에
있다. 그룹 목록의 "인터페이스" 칸은 그룹을 쓰는 NIC를 "단순 n · SDN n"으로 센다.

## 물리 네트워크: 포트·본딩·LLDP

Orbivisor는 호스트의 NIC·본딩·VLAN을 설정하지 않는다(그대로 Debian `ifupdown`·NetworkManager 몫). 노드는 있는 것을 **읽어서** 알리고,
API가 "물리 포트 → 본딩 → VLAN → 브리지 → 네트워크 → VM" 사슬로 엮어 홈 대시보드·지도·링크 경보가 함께 쓴다(계획 75,
[운영자 대시보드](web-ui.md#운영자-대시보드), [링크 경보](cluster.md#링크-경보)).

- **노드가 알리는 것.** 30초마다 `ip -j -d addr show`와 sysfs로 인터페이스(이름·종류·operstate·carrier·속도·MAC·주소·`master`), 본딩
  상태(모드·구성 포트마다 up/down/LACP 어긋남/속도 어긋남/사라짐), **MTU**, 관리자 UP(`ip link set … down`이면 거짓), VLAN·macvlan의
  **아래 장치**(`ip`의 `link`)와 **VLAN id**를 노드 상태(`status.interfaces`)에 쓴다. 역할이 업링크·underlay·service인 포트는 이름으로
  **기억한다**(`status.known_ports`: 종류·역할·스스로 가진 주소·든 본딩·위 브리지·사라짐·본 때 — 지난 기억에서 이어 가므로 다시 시작해도 남음).
  링크가 끊길 때 NetworkManager가 주소를 떼어도 그 포트를 그 역할 그대로 보고, netdev가 사라지면(PCIe 장애·드라이버 unbind·핫플러그 제거
  — 커널이 본딩·브리지에서도 뺀다) `missing`으로 남긴다: 본딩 구성 포트였으면 본딩이 저하, 본딩 밖 브리지 업링크였으면 그 브리지가 그만큼
  업링크를 잃는다. 다시 나타나 감시 밖 역할이 되거나 본딩에 새 이름의 구성 포트가 들어오면 잊는다. **NIC를 교체·뺐으면 잊기**:
  `orbivisorctl node forget-ports <노드> <포트>…`([링크 경보](cluster.md#링크-경보)). 본딩 상태(사라진 구성 포트 포함)가 바뀌면 감사 항목과
  기록 Task `bond-degraded`도 남는다([HA 상태 검사](ha.md#ha-상태-검사)).
- **LLDP(스위치 쪽 이웃).** 노드에 **lldpd**가 있으면 같은 30초 회차에 `lldpcli -f json show neighbors`(3초 상한)를 따로 띄워(검사
  회차를 막지 않음) 끝난 뒤의 회차가 포트마다 건너편 스위치 이름·섀시 id·스위치 포트·포트 설명·관리 주소를 붙인다. 없으면 칸이 비고 화면이 "lldpd를 설치하면 스위치 포트가 보입니다."를 보인다.
  Orbivisor가 직접 LLDP를 받지는 않는다.

  ```bash
  apt install lldpd                  # Debian·Ubuntu (RHEL 계열은 EPEL의 lldpd: dnf install lldpd)
  systemctl enable --now lldpd
  lldpcli show neighbors             # 스위치가 보이는지 먼저 확인
  ```

  `lldpcli`가 없으면 노드가 5분마다 다시 찾으므로 orbivisord를 다시 시작하지 않아도 된다(5분 안에 칸이 찬다).

  **스위치 쪽에서도 LLDP를 켠다**(제조사마다 `lldp run`·`feature lldp`·`lldp enable` 등 — 많은 스위치가 기본으로 켜져 있지만 포트마다
  송신을 끈 곳이 있다). 이웃이 30초 회차 안에 보이지 않으면 lldpd의 첫 교환(기본 30초 간격)을 기다린다. lldpd가 멈추면 다음 회차에 이웃이 빈다.
- **역할.** 포트마다 `uplink`(브리지·본딩 아래와 본딩), `underlay`(VXLAN·SDN 터널 주소를 가진 것과 그 아래 — 주소가 떨어졌으면
  그 주소를 기억한 포트. 아무 인터페이스도 그 주소를 갖지 않으면 링크가 살아 있어도 underlay 레일은 `down`), `service`(그 밖에 전역 주소를 가진 물리 포트와 그 아래 — 스토리지·복제·관리·클러스터 링크망; 노드의 클러스터
  링크 주소·API 주소면 `uses`에 `cluster`·`management`), `unused`(묶이지 않고 주소도 없는 이더넷), `other`(업링크가 아닌 브리지, 묶이지 않은
  가상 장치)다. 주소를 잃은 기억 포트와 그 아래는 기억한 역할 그대로다. 링크 경보의 기본(`role: uplink`)과 지도의 포트 줄·영향은
  `uplink`·`underlay`·`service`를 본다.
- **`GET /api/v1/topology/physical`**(루트 `cluster.read`, 온라인 노드가 모두 기능 `operator-dashboard`를 알아야 함 — 아니면 **409**
  `cluster_upgrading`, 꺼진 옛 노드는 막지 않음):
  `{"nodes":[{"node","online","legacy","ports":[{name,kind,state,carrier,speed_mbps,mtu,mac,master,parent,vlan_id,lldp,bond,role,uses,addresses,health}],"bridges":[{name,
  uplinks,vlan_filtering,networks,vm_nics,health}],"underlay","physnets","impact":[{port,severity,bond,missing,bridges,underlay,uses,networks,vms,vm_ids}],
  "unreachable":[{network,vms,vm_ids}]}],"networks":[{id,name,kind,carrier,vlan,nodes:[{node,state}]}]}`(노드는 이름 순, veth는 포트에서 뺌).
  `health`는 그 포트에서 아래로 본 상태(본딩은 본딩 상태, VLAN은 아래 장치, 브리지는 업링크들 — 관리자가 내린 브리지는 `down`, 주소를 잃은
  기억 포트는 `down`)다. 사라진 기억 포트는 목록 끝에 `state: "missing"`·`health: "down"`으로 오고, 본딩의 `members`에도 `missing`으로 든다.
  `addresses`는 포트가 가진 전역 주소(주소를 잃었거나 사라진 포트는 마지막으로 가졌던 것)다. `impact`는
  포트·본딩 장애마다 하나다 — 포트 carrier 없음·주소 잃음·사라짐(`missing: true`)·본딩 끊김·네트워크가 실리는데 관리자가 내린 브리지면 그 위 브리지·네트워크·이 노드의 VM이 `down`, 본딩 저하·
  구성 포트 하나만 끊김이면 `degraded`(이중화 잃음), `vm_ids`는 50개까지. 오프라인 노드의 `impact`는 마지막으로 본 값이다(화면은 그렇게 적고
  배지에 세지 않는다). `unreachable`은 이 노드에서 도는 VM이 NIC를 붙였는데 이 노드에 실을 수단이 없는 네트워크다. `networks`는
  네트워크마다 노드별 레일 상태다: 사슬이 끝까지 up이면 `up`, 본딩 저하면 `degraded`, 끊기면 `down`, 그 노드에 그 네트워크를 실을 수단이
  없으면 `absent`, 노드가 오프라인이거나 underlay 주소의 인터페이스를 찾지 못했고 전에 본 것도 없으면 `unknown`. 사슬은 bridge·VLAN 네트워크 → 노드 브리지, SDN 공급자 → 노드 physnet 브리지, VXLAN·SDN 오버레이 → underlay 주소를 가진
  인터페이스, 그다음 브리지 업링크(`master`) → VLAN이면 아래 장치 → 본딩이면 구성 포트다. 상태 머신 리비전이 같으면 이전 결과를 준다.
- **옛 판 노드**(`legacy: true` — 업그레이드하지 않은 채 꺼진 노드, 온라인 옛 노드가 있으면 끝점이 409다)는 MTU·LLDP가 없고 VLAN 아래
  장치를 이름(`eno1.100` → `eno1`)으로 짐작한다.
- **한계.** 포트·본딩 설정 바꾸기, SNMP, 노드 사이 실제 경로 추적, 인터페이스별 트래픽 기록은 없다(트래픽은 마지막 표본뿐 —
  [지표](cluster.md#측정)).

## SDN (OVN)

SDN 네트워크는 [OVN](https://www.ovn.org/)의 논리 스위치다(계획 65). 서브넷을 두면 **Orbivisor가 VM NIC의 IP 주소를 정하고
(IPAM) OVN 내장 DHCP가 그 주소를 준다.** OVN은 orbivisord가 apt로 설치하고 세우며, Orbivisor는 OpenStack Neutron
ML2/OVN이 하는 자리(OVN에 원하는 상태를 쓰는 관리 시스템)를 맡는다. 지금의 브리지·VLAN·VXLAN 네트워크와 보안 그룹은
"단순 네트워크"로 그대로 있고, 한 VM에 단순 NIC와 SDN NIC를 섞을 수 있다. 관리자가 `sdn init`을 하기 전에는 아무것도
설치하지 않고 OVN에 붙지도 않는다.

계획 66부터 SDN 네트워크는 밖과 이어진다: 노드의 물리 망에 붙는 **외부(공급자) 네트워크**, SDN 네트워크를 잇는 **분산
라우터**와 정적 경로, 라우터의 바깥 포트를 노드 사이에서 넘기는 **게이트웨이 HA**, **SNAT**와 **유동 IP**(플로팅 IP),
**IPv6 SLAAC·DHCPv6·RA**, 그리고 SDN NIC의 **보안 그룹**(OVN ACL). 자세한 비교는 [Neutron·Proxmox SDN·NSX와
견주기](#neutronproxmox-sdnnsx와-견주기).

계획 67은 **서비스와 운영**이다: OVN 내장 **부하 분산**(VIP·건강 검사·바깥 VIP), NIC마다 **QoS**(속도 제한·DSCP — 단순 NIC도 tc로
속도 제한), 네트워크·라우터·VM·LB를 한 그림으로 보는 **2.5D 토폴로지**, 단순 네트워크의 VM NIC를 SDN으로 옮기는 **옮기기 도구**
(미리 보기·라이브·되돌리기), 지운 노드를 끊는 **OVN 접근 잠금**(SB RBAC·주소 허용 목록), **SDN 경보** 여덟, 네트워크 폴더 할당량
`networks`·`load_balancers`. 기능 하나 **`sdn-services`**가 이것을 가린다. 견주기는 [부하 분산·QoS·옮기기: Octavia·Proxmox·NSX와
견주기](#부하-분산qos옮기기-octaviaproxmoxnsx와-견주기).

| | Proxmox VE SDN | OpenStack Neutron ML2/OVN | Orbivisor 계획 65~67 |
|---|---|---|---|
| 데이터 평면 | Linux 브리지 + VLAN·QinQ·VXLAN·EVPN(FRR) | OVS + OVN(Geneve) | OVS + OVN(Geneve) |
| 제어 | `/etc/pve/sdn` 설정을 노드마다 ifupdown2로 적용 | API가 NB에 쓰고 주기적으로 고침 | Raft의 원하는 상태를 리더 `OvnSync`가 NB에 맞춤 |
| OVN 설치 | 해당 없음 | 배포 도구(kolla·charm 따위)가 따로 | orbivisord가 Task로(중앙 1·3·5, 노드마다 chassis) |
| 서브넷·IPAM | VNet › 서브넷, pve·NetBox·phpIPAM | 서브넷, Neutron DB | 네트워크 안 서브넷, Raft(NIC 스펙의 `ips`) |
| DHCP | dnsmasq(simple 영역) | OVN 내장 | OVN 내장 DHCPv4·DHCPv6, RA |
| 라우터·NAT·플로팅 IP | EVPN 출구·SNAT | 분산 라우터, 게이트웨이 HA | 분산 라우터, 게이트웨이 HA, SNAT, 유동 IP(계획 66) |
| 부하 분산 | 없음 | Octavia(Amphora VM 또는 OVN 공급자) | OVN 내장 `Load_Balancer`, 건강 검사, 바깥 VIP(계획 67) |
| QoS | NIC `rate`(tc) | QoS 정책(OVN `QoS` 행) | NIC `qos` — SDN은 OVN `QoS` 행, 단순 NIC는 tc(계획 67) |
| 테넌트 | 없음(권한 경로) | 프로젝트 | [네트워크 폴더](folders.md) |

### 개념

- **중앙(central).** OVN의 northbound DB(NB, 원하는 논리 망), southbound DB(SB, chassis와 바인딩), `ovn-northd`(NB를 SB의
  흐름으로 바꿈)가 도는 노드다. NB·SB는 ovsdb의 Raft 클러스터이고, northd는 SB 잠금으로 한 번에 하나만 일한다. 중앙은
  관리자가 고른 **고정 집합**(1·3·5개)이고 Orbivisor의 투표 멤버를 저절로 따라가지 않는다(Ceph mon과 같은 방식).
- **chassis.** VM을 돌리는 노드의 `ovn-controller`와 Open vSwitch다. SB에서 흐름을 받아 `br-int`에 깔고, 다른 chassis와
  Geneve(UDP 6081)로 잇는다. chassis 이름은 **노드 UUID**(노드 인증서 CN과 같음), Geneve 주소는 노드의 underlay 주소
  (VXLAN과 같은 값, `node list`의 UNDERLAY)다. 위트니스와 macOS 노드는 chassis가 될 수 없다.
- **논리 스위치.** SDN 네트워크 하나가 NB의 `Logical_Switch` `opl-<네트워크 id>` 하나다. 네트워크끼리는 L2가 섞이지 않고,
  [라우터](#라우터와-경로)에 함께 붙이기 전에는 L3로도 닿지 않는다(고립 망). 그래서 폴더(테넌트)가 다른 네트워크는 같은
  CIDR을 써도 된다(같은 라우터에는 못 붙는다).
- **서브넷.** 네트워크마다 IPv4 하나·IPv6 하나까지. CIDR(IPv4 `/8`~`/29`, IPv6 `/48`~`/124`), 게이트웨이(없으면 첫 호스트
  주소 — 늘 예약해 두고 라우터에 붙이면 라우터 포트의 주소가 된다), DHCP(IPv4 기본 켬 — 공급자 네트워크는 기본 끔),
  IPv6 모드(`ipv6-mode`, [IPv6](#ipv6-slaacdhcpv6ra)), DNS 서버 4개까지, 할당 범위(pool) 16개까지, 유동 IP 범위
  (`floating`, 공급자 네트워크의 IPv4만), 설명.
- **IPAM.** 주소의 출처는 Raft의 VM 스펙(`nics[].ips`)이다. API가 MAC처럼 새 NIC에 서브넷의 가장 낮은 빈 주소를 주고,
  직접 고른 주소(`ip=`)면 그것을 검사한다. 한 네트워크 안에서 같은 주소를 두 NIC가 갖지 못하게 Raft 상태 머신이 막는다
  (MAC과 같은 겹침 검사 — 두 API 노드가 동시에 골라도 하나만 되고 다른 쪽은 다음 빈 주소로 다시 고른다).
- **DHCP.** OVN 내장이라 DHCP 서버 VM·에이전트가 없고 노드마다 ovn-controller가 답한다. 논리 포트의 주소만 답한다(Orbivisor가
  정한 주소만 나감). 임대 86400초, MTU, DNS를 준다. **기본 경로(`router`)는 라우터에 붙은 네트워크와 공급자 네트워크에서만
  준다** — 라우터가 없는 네트워크는 밖으로 나갈 길이 없고, 단순 NIC와 함께 쓰는 VM의 기본 경로를 빼앗지 않게.
- **포트 보안.** NIC마다 논리 포트 `opl-<VM id>-<MAC 12자리>`가 생기고 `port_security`가 늘 켜져 있다. NIC는 자기 MAC과
  받은 주소로만 보낼 수 있다(MAC·IP 위조와 ARP 위조를 OVN이 막는다). 꺼진 VM의 포트도 NB에 남아 주소를 잡아 둔다.
  서브넷이 있는 네트워크에서 주소가 하나도 없는 NIC(빈 주소가 없어 템플릿 되돌리기가 주소 없이 둠, 복원 실패)의
  포트는 **꺼 둔다**(`enabled=false` — 아무것도 오가지 않음). MAC만인 포트 보안은 어느 IP든 보내게 두기 때문이다. 주소를
  받으면(VM 편집 — 켜진 VM도 된다, `network ips --fill`) 다음 동기화에 켜진다. 웹의 VM 상세·IP 표는 이 NIC를 "꺼짐: 주소
  없음"으로 보인다. 서브넷이 없는 SDN 네트워크는 L2만이라 MAC만으로 켠다.

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
| TCP 6645 | 중앙끼리 | SB 특권 포트 — northd만([OVN 접근 잠금](#ovn-접근-잠금)을 켰을 때) |

잠금을 켜면(새 클러스터는 기본으로 켜짐) 6641·6642는 클러스터 멤버 노드의 주소에서만, 6643~6645는 중앙 주소에서만, Geneve는
멤버 노드에서만 받는다.

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
orbivisorctl sdn central remove n3        # 마지막 중앙은 뺄 수 없다(409 sdn_last_central). 꺼진 노드는 다른 중앙이 내쫓는다
```

웹은 **네트워크 › SDN(가상 네트워크)** 보기의 "SDN 설정 시작" 마법사다(① 중앙 노드 — 투표 멤버 셋을 미리 고름 ② 설치 진행과
나머지 중앙의 합류 ③ chassis — 노드마다·"모든 노드에 설치"). 자세한 것은 [웹 화면 둘러보기](web-ui.md#sdn-화면). API는
`GET/POST /api/v1/sdn`(`{"node": "n1", "address": "10.1.0.11"}`), `POST /api/v1/sdn/centrals`(`{"node": "n2"}`),
`DELETE /api/v1/sdn/centrals/{node}`, `POST /api/v1/sdn/chassis`(`{"nodes": [...]}` 또는 `{"all": true}`, 노드마다 Task 목록)이다.
모두 루트 `cluster.admin`(읽기는 `cluster.read`)이다.

- **무엇을 하나.** 중앙 Task는 빠진 꾸러미를 깔고, 노드 TLS를 `/etc/orbivisor/ovn/`에 베끼고, 배포판의 중앙 단위(`ovn-central`·
  `ovn-northd`·`ovn-ovsdb-server-nb`·`-sb`)를 끄고 가린 뒤(끈 목록은 `/etc/orbivisor/ovn/distro-units`) Orbivisor 단위
  `orbivisor-ovn-nb`·`orbivisor-ovn-sb`·`orbivisor-ovn-northd`를 띄운다. DB는 `/var/lib/orbivisor/ovn/{nb,sb}.db`, 로그는 `/var/log/orbivisor/ovn-*.log`다.
  NB·SB는 노드 클러스터 주소 하나에만 듣는다(`--address`로 노드의 다른 주소 — 노드 주소나 클러스터 링크의 주소 — 를 고를
  수 있다. 루프백·링크 로컬·`0.0.0.0`은 다른 중앙과 chassis가 닿지 못해 400이다. 노드 주소 자체가 127.0.0.1인 한 기계
  클러스터만 루프백을 쓴다). chassis 구성은
  로컬 OVS의 `external_ids`(`system-id`=노드 UUID, `ovn-remote`=모든 중앙의 `ssl:…:6642`, `ovn-encap-type=geneve`,
  `ovn-encap-ip`=underlay)와 `ovs-vsctl set-ssl`이다.
- **다시 하기.** Task는 단계마다 지금 상태를 먼저 보므로 실패한 것을 같은 노드로 다시 부르면 이어서 한다(DB를 다시 만들지
  않는다). 끝나지 않은 `sdn init`을 주소 없이 다시 하면 지난번 `--address`로 한다. 다른 주소로 다시 하면 아직 아무도 쓰지
  않은 DB를 `*.left-<시각>`으로 치우고 새 주소로 만든다. 준비된 SDN에서는 DB가 없어도 새 OVN 클러스터를 만들지 않고
  실패한다(다른 중앙과 갈라진 NB·SB가 서지 않게 — 파일을 되살리거나, 중앙이 더 있으면 빼고 다시 더한다). 중앙 Task는
  클러스터에서 한 번에 하나다(바쁘면 409 `sdn_busy`). apt 때문에 시간 제한이 없다.
- **중앙 더하기가 실패하면** 합류한 Raft를 다시 떠나고(`cluster/leave`) 단위를 멈추고 DB는 이름만 바꾼다 — 목록에 없는
  멤버가 남아 정족수를 깎지 않게. 아직 합류 중이거나(OVS는 합류 중 떠나기를 거절한다) 서버가 한 번도 돌지 않은(클러스터
  id를 아직 모르는) DB는 Raft 구성에 없으므로 떠나기 없이 멈추고 치운다. 떠나지 못하면(멤버인데 서버가 돌지 않음 등) 그대로
  둔다: 노드 상태가 중앙 칸을 알리고(`sdn status`·웹의 "남은 중앙 DB"), `sdn central add`로 다시 하거나 `sdn central remove`로
  떠나게 한다.
- **아무것도 몰래 지우지 않는다.** 다른 OVN·OpenStack이 쓰는 노드(`ovn-remote`가 이미 있고 이 클러스터 표시가 아님),
  Raft가 모르는 클러스터 id의 DB, 논리 스위치가 든 배포판 NB DB가 있으면 거절한다(400, 까닭과 함께). 중앙에서 빠진 노드의
  DB는 `*.left-<시각>`으로 이름만 바꾼다.
- **새 노드에는 저절로 깔지 않는다.** 나중에 합류한 노드는 `sdn chassis install`로 고른다(꾸러미 설치는 늘 명시적).
- **노드 SDN 에이전트.** 노드마다 orbivisord가 시작할 때, 중앙 목록·underlay가 바뀔 때, 60초마다(`[sdn] refresh_secs`)
  `ovn-remote`·`encap-ip`·TLS 사본을 맞추고, 중앙이면 멈춘 단위를 다시 켜고 OVN 꾸러미를 올린 뒤의 DB 스키마 변환을 한다.
  재부팅 뒤 장치가 없는 이 클러스터의 OVS 포트를 지운다(남의 포트는 둔다). 노드 상태 `status.sdn`(chassis 상태
  `missing`·`configuring`·`ready`·`down`, OVN·OVS 판, encap 주소, 중앙이면 NB·SB Raft 역할과 northd 활성)을 알린다.
  `cluster leave`면 chassis 구성의 이 클러스터 칸만 지우고 ovn-controller를 멈춘다(꾸러미·DB는 남김). 중앙이거나 남은 중앙
  DB를 알린 노드의 `cluster leave`는 409 `sdn_central_on_node`다 — 먼저 `sdn central remove`.
- **꺼진 중앙 빼기.** `sdn central remove`는 노드가 온라인이면 그 노드가 떠나고(`cluster/leave`), 꺼졌으면 살아 있는 다른
  중앙이 그 노드의 Raft 주소(`ssl:IP:6643`·`:6644`)를 `cluster/kick`으로 내쫓고 목록에서 뺀다(Task `sdn-central-kick`).
  꺼진 노드의 단위와 DB는 손대지 못하므로 그 노드를 되살리면 "남은 중앙 DB"로 보이고 다시 `sdn central remove`로 치운다.
  내쫓기도 Raft 정족수(중앙 셋이면 둘)가 있어야 한다.
- **노드 지우기.** 목록의 중앙인 노드는 먼저 `sdn central remove`(409 `sdn_central_on_node` — 꺼졌으면 위처럼 내쫓는다).
  끝나지 않은 중앙 더하기의 DB를 알린 채 꺼진 노드는 막지 않는다: 지운 뒤 `SdnStatus.departed`에 적고 살아 있는 중앙이
  내쫓는 Task를 만든다(응답 경고에 그 Task, 다른 중앙 Task가 돌면 나중에 `sdn central remove 이름`). chassis만인 노드의 SB
  `Chassis` 행은 **SB Raft 리더인 중앙의 에이전트**가 로컬 소켓으로 지운다(계획 67 — 모든 노드가 `sdn-services`를 알기 전에는
  리더 OvnSync가). 그 노드가 옮기기 다리 노드였으면 그 옮기기는 멈춘다([옮기기](#단순-네트워크를-sdn으로-옮기기)).

```toml
# /etc/orbivisor/orbivisord.toml (노드마다, 선택 — 기본값)
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
orbivisorctl network subnet add tenant-a fd00:10::/64                # 체계마다 하나(주소 없는 NIC는 이때 받는다)
orbivisorctl network subnet set tenant-a 10.10.0.0/24,dns=1.1.1.1+8.8.8.8,pool=10.10.0.100-10.10.0.199
orbivisorctl network subnet remove tenant-b fd00:20::/64             # 그 주소를 쓰는 NIC가 없어야 한다
```

`--subnet`은 `CIDR[,gateway=IP][,dhcp=on|off][,dns=IP+IP][,pool=A-B+C-D][,floating=A-B+C-D]
[,ipv6-mode=slaac|dhcpv6-stateful|dhcpv6-stateless][,description=TEXT]`이다(`floating`은 [공급자 네트워크](#외부공급자-네트워크와-노드-physnet),
`ipv6-mode`는 [IPv6](#ipv6-slaacdhcpv6ra)). 웹은 네트워크 만들기의
종류 "SDN(OVN)"과 서브넷 표, 목록에서 이름을 누르면 여는 서랍(서브넷 편집과 IP 표)이다. API는 `POST /api/v1/networks`의
`{"spec": {"sdn": {}, "mtu": 1442, "subnets": [{"cidr": "10.10.0.0/24", "dns_servers": ["10.0.0.53"]}]}}`,
`PATCH /api/v1/networks/{name}`(`{"subnets": [...]}` — 목록 전체, `If-Match`), `GET /api/v1/networks/{name}/ips`(`?live=false`면
OVN 포트 상태를 읽지 않음)다.

- **MTU.** 기본 1442(underlay 1500에서 Geneve 머리 58바이트를 뺀 값, Neutron과 같음), 1280~9000. IPv6 underlay면 1422 이하로
  둔다. tap MTU와 virtio `host_mtu`, DHCP의 MTU 옵션이 이 값이다.
- **먼저 SDN을 세운다.** SDN이 준비되지 않았으면 409 `sdn_not_ready`. 서브넷 오류는 400 `invalid_subnet`(`subnet 2: …`처럼
  번호), 단순 네트워크 바꾸기는 409 `network_immutable`. 브리지 이름 `br-int`는 브리지 네트워크에 쓸 수 없다.
- **서브넷 바꾸기.** DHCP·DNS·할당 범위·설명은 바꿀 수 있고 CIDR·게이트웨이는 바꿀 수 없다(빼고 다시 더한다). 할당 범위를
  줄여도 이미 준 범위 밖 주소는 남는다. 서브넷을 빼려면 그 주소를 쓰는 NIC가 없어야 한다(409 `subnet_in_use`와 VM 이름들).
- **서브넷을 더하면 주소가 하나도 없는 NIC가 그 서브넷의 주소를 받는다**(켜진 VM 포함). 주소 없는 NIC의 포트는 꺼지므로
  (포트 보안) L2만이던 네트워크에 첫 서브넷을 더하는 순간 그 NIC들이 끊기지 않게 같은 `PATCH`가 채운다. 켜진 게스트는 다음
  DHCP 갱신(또는 재부팅)에서 새 주소를 받는다 — 그동안 정적 주소를 쓰던 게스트는 그 주소가 포트 보안에 막히므로 서브넷을
  더하기 전에 맞춘다. 모두 줄 빈 주소가 없으면 409 `ip_exhausted`(주소 없는 NIC 수와 서브넷마다 남은 수)이고 아무것도 바뀌지
  않는다. 이미 주소가 있는 NIC의 빠진 체계(IPv4만 있는데 IPv6 서브넷을 더함)는 채우지 않는다 — `orbivisorctl network ips
  tenant-a --fill`(켜진 VM은 주소 없는 NIC만 채우고 나머지는 알린다). 새로 붙는 NIC는 곧바로 받는다. 웹 서랍의 서브넷 편집도
  저장 전에 몇 개의 NIC가 주소를 받을지 알린다.
- **`network ips`의 UP·NODE**는 API 노드가 OVN에서 읽은 논리 포트의 `up`과 바인딩한 chassis다. OVN을 읽지 못하면 `-`이고
  까닭이 경고로 나온다(주소 표는 Raft에서 오므로 그대로 나온다). 목록에는 부른 사용자가 읽을 수 있는 VM의 NIC만 나온다 —
  네트워크를 함께 쓰는 다른 폴더의 VM은 `USED`에만 들고 수로만 알린다(API `hidden`). 409 `ip_in_use`와 409
  `subnet_in_use`도 읽을 수 있는 VM만 이름으로 대고 나머지는 수로 댄다.
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

- **`ip=`**는 체계마다 하나까지(`A+B`, IPv4 대응 IPv6 `::ffff:a.b.c.d`는 IPv4로 바꿔 저장)이고 서브넷 안이어야 한다. 할당 범위 밖이어도 서브넷 안이면 받는다(Neutron의 고정 IP).
  예약 주소(망·브로드캐스트·게이트웨이)나 서브넷 밖은 400, 다른 NIC가 쓰는 주소는 409 `ip_in_use`, 빈 주소가 없으면 409
  `ip_exhausted`, SDN이 아닌 네트워크에 주면 400 `ips_without_sdn`.
- **NIC 일생.** 새 NIC(새 VM, 붙인 NIC, 네트워크를 바꾼 NIC)는 서브넷마다 주소를 받는다. 있던 NIC를 주소 없이 보내면(주소를
  모르는 클라이언트의 `PUT`) 지금 주소를 둔다 — 그래서 `PUT`으로 주소를 비울 수는 없다. NIC를 빼거나 VM을 지우면 주소가
  풀린다. 템플릿으로 바꾸면 주소를 지우고, 템플릿에서 되돌리면 새로 받는다. 복제·템플릿 배포·가져오기는 새 주소(`--ip`로
  고름). 복제·배포가 NIC를 SDN 네트워크로 옮기면(배포의 네트워크 바꾸기) 그 NIC의 보안 그룹과 cloud-init 주소 항목을
  빼고 Task 경고에 적는다. 복제·가져오기의 `--ip`는 권한(새 VM의 폴더·네트워크)을 본 뒤에 본다. 백업 복원은 원래 주소가 비어 있으면 그대로, 아니면 새로 받고 Task 경고에 적는다. 스냅샷 되돌리기는 주소도
  되돌리고, 그 사이 다른 VM이 그 주소를 잡았으면 409 `ip_in_use`(몰래 바꾸지 않는다). 라이브 마이그레이션·HA는 그대로.
- **켜진 VM의 주소는 바꿀 수 없다**(409 `vm_running`). 포트 보안이 새 주소만 받아 게스트의 옛 주소가 곧바로 막히기 때문이다.
  켜진 VM에 새로 붙이는 NIC는 주소를 받는다(핫플러그). 주소가 하나도 없던 NIC에 주기는 켜진 채로 된다 — 그 포트는 꺼져
  있어 잃을 것이 없고 게스트는 DHCP 갱신에서 받는다.
- **템플릿 되돌리기**가 서브넷에 빈 주소가 없어 주소 없이 끝나면 Task 경고에 적는다(그 NIC의 포트는 꺼져 있다).
- **게스트 쪽.** IPv4 서브넷의 DHCP가 켜져 있으면 게스트가 DHCP로 받는다(cloud-init이 없는 이미지도). cloud-init이 있는 VM은
  network-config에 SDN NIC를 IPv4 `dhcp`(DHCP를 끈 서브넷이면 `static` 주소/접두사·DNS), IPv6 `static`(`ipv6-mode`가 있고
  라우터에 붙었으면 `ipv6_slaac`·`ipv6_dhcpv6-stateless`·`ipv6_dhcpv6-stateful`)으로 적는다. 고정 주소의 게이트웨이는 라우터에
  붙은 네트워크와 공급자 네트워크에서만 적는다(서브넷 게이트웨이). SDN NIC 자리의 `cloud_init.networks[i]`는 빈 `{}`만 받는다(400 `cloud_init_sdn_nic` — 주소는
  IPAM이 정한다). NIC 주소가 바뀌면 instance-id가 바뀌어 게스트가 다시 적용하고, 서브넷의 DNS·DHCP만 바꾸면 그대로다(DHCP
  NIC는 임대 갱신 때 받는다).
- **보안 그룹.** SDN NIC에도 건다(`--nic tenant-a,sg=web`, 계획 66 — [SDN NIC의 보안 그룹](#sdn-nic의-보안-그룹)). 그룹을
  걸지 않은 SDN NIC는 포트 보안이 위조만 막고 들어오는 것을 거르지 않으니, 같은 SDN 네트워크의 VM끼리, 라우터를 지나 다른
  네트워크와 유동 IP로 밖에서도 닿는다.

### 켜기·마이그레이션·HA

- **스케줄러.** SDN NIC가 있는 VM은 chassis가 `ready`인 노드에만 간다(마이그레이션 대상·HA 복구·비우기도 같다). 둘 곳이
  없으면 "… is an SDN (OVN) chassis for VMs on SDN networks"가 든 409다. 공급자 네트워크 NIC가 있는 VM은 그 physnet이
  `ready`인 노드에만 간다("… has the physical networks (physnets) of its provider networks").
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

### 외부(공급자) 네트워크와 노드 physnet

**외부(공급자) 네트워크**는 노드의 물리 망(flat 또는 VLAN)에 붙는 SDN 네트워크다(OVN `localnet` 포트, 계획 66). VM을 곧바로
붙일 수도 있고, 라우터의 바깥쪽과 유동 IP의 주소 풀로도 쓴다. 물리 망은 **physnet**이라는 이름으로 부르고, 노드마다 그
physnet이 어느 Linux 브리지인지 정한다.

```bash
# 노드마다: physnet ext = 관리자가 만들어 둔 Linux 브리지 vmbr1, 게이트웨이 후보(우선순위 200·100)
orbivisorctl node sdn n1 --physnet ext=vmbr1
orbivisorctl node sdn n2 --physnet ext=vmbr1 --gateway 200
orbivisorctl node sdn n3 --physnet ext=vmbr1 --gateway          # 우선순위 100
orbivisorctl node sdn n2                                         # 아무 칸 없이 부르면 보이기
# sdn: physnets ext=vmbr1 (ready); gateway priority 200
orbivisorctl node sdn n1 --no-physnet ext                        # 빼기(그 physnet을 쓰는 VM이 n1에서 돌면 409)
orbivisorctl node sdn n3 --no-gateway

# 공급자 네트워크(admin): flat이면 PHYSNET, VLAN이면 PHYSNET:VLAN
orbivisorctl network create ext --sdn --provider ext \
  --subnet 203.0.113.0/24,gateway=203.0.113.1,pool=203.0.113.10-203.0.113.99,floating=203.0.113.100-203.0.113.199 \
  --subnet 2001:db8:0:1::/64,gateway=2001:db8:0:1::1
orbivisorctl network create dmz --sdn --provider ext:100 --subnet 198.51.100.0/24,dhcp=on
orbivisorctl network list
# NAME  TYPE      BRIDGE  VLAN/VNI  MTU   SUBNETS
# ext   provider  br-int  ext       1500  203.0.113.0/24,2001:db8:0:1::/64
# dmz   provider  br-int  ext:100   1500  198.51.100.0/24
```

웹은 **클러스터·가용성 › 노드 › 노드 상세의 "SDN 물리 네트워크" 카드**(physnet 표 — 이름 → 브리지, 준비·적용 중·빼는 중·오류 — 와
게이트웨이 줄, admin의 "고치기" 대화상자)와 **네트워크 만들기의 종류 "외부(공급자)"**(admin만, SDN 설정 전에는 막힘 — physnet은
노드가 알린 이름을 제안하는 입력, VLAN, MTU 기본 1500, 서브넷 표에 "유동 IP 범위"와 DHCP 기본 끔)다. 네트워크 목록은 공급자
행에 `physnet[:vlan]`과 physnet이 준비된 chassis를 보인다. 자세한 것은 [웹 화면 둘러보기](web-ui.md#sdn-화면).

- **잇는 모양.** 노드의 SDN 에이전트가 관리자의 Linux 브리지에 **veth 짝 하나**를 매달고(`oplp<h8>b`), 다른 끝(`oplp<h8>o`)을
  Orbivisor가 만든 OVS 브리지 `oplp<h8>x`에 넣은 뒤 OVS의 `external_ids:ovn-bridge-mappings`에 `<physnet>:oplp<h8>x`를 더한다
  (`h8`은 클러스터 태그와 physnet 이름의 해시). ovn-controller가 `br-int`와 그 브리지 사이에 patch 포트를 만든다. **관리자의
  브리지와 물리 NIC는 고치지 않는다** — 주소·본딩·VLAN 설정은 그대로이고 브리지 포트가 veth 하나 늘 뿐이다(Neutron 관례처럼
  NIC를 OVS로 옮기지 않으므로 잘못돼도 노드가 끊기지 않는다. 대가는 veth 한 단).
- **VLAN.** 브리지가 VLAN 인식(`vlan_filtering 1`)이면 veth를 매달 때 `vid 2-4094`를 트렁크로 연다(flat 공급자는 브리지의 기본
  pvid). 아니면 태그 프레임이 그대로 지난다. 매단 뒤에 관리자가 `vlan_filtering`을 켜면 다음에 다시 매달 때까지(physnet을
  뺐다 넣거나 veth가 사라졌을 때) 트렁크가 열리지 않는다.
- **남의 설정은 둔다.** `ovn-bridge-mappings`의 남의 항목(OpenStack 따위)은 차례 그대로 두고 내 항목만 더하고 뺀다. 같은
  physnet이 남의 브리지에 매핑돼 있으면 덮지 않고 그 physnet을 `ready: false`("… mapped to … by someone else")로 알린다.
- **노드 상태.** `node get`의 `sdn:` 줄과 노드 상세 "SDN 물리 네트워크" 카드가 physnet마다 준비·오류를 보인다(브리지가 없음,
  매핑 충돌, 명령 실패, "this node is not an SDN (OVN) chassis"). 설정이 바뀌면 1초 안에, 그리고 60초마다 맞춘다. 브리지가
  잠시 없거나 명령이 실패해도 이미 만든 것은 지우지 않는다(빼기는 설정에서 빠질 때만).
- **재부팅.** veth는 재부팅하면 사라지고(OVS 브리지는 남음) orbivisord가 시작할 때 다시 만든다 — 부팅 뒤 orbivisord가 뜰
  때까지 그 노드의 공급자 트래픽은 끊겨 있다.
- **설정 규칙.** `PUT /api/v1/nodes/{name}`의 `spec.sdn`(`{"physnets": {"ext": "vmbr1"}, "gateway": 200}` — 칸이 없으면 그대로,
  `{}`는 지우기). 루트 `cluster.admin`. physnet 이름은 `[a-z][a-z0-9-]{0,30}`, 노드마다 8개, 브리지는 `br-int`·Orbivisor 장치가
  아닌 이름, 게이트웨이 우선순위 1~1000(400 `invalid_node_sdn`). 위트니스·macOS 노드는 400, SDN 전은 409 `sdn_not_ready`.
  빠지거나 **브리지가 바뀌는** physnet을 쓰는 공급자 네트워크 NIC의 VM이 그 노드에서 돌거나 그리로 마이그레이션 중이면 409
  `physnet_in_use`(`details.vms`) — 브리지를 바꾸면 에이전트가 veth를 새 브리지로 옮겨 도는 VM이 다른 L2로 가므로 먼저 멈추거나
  옮긴다. **브리지만 바꾸는** physnet은 그 노드가 그 physnet으로 나가는 라우터의 게이트웨이 후보이거나(`details.routers`) 그
  노드의 VM에 그 physnet의 유동 IP가 붙었을 때도(`details.floating_ips`) 409다 — physnet이 준비된 채라 OVN이 아무것도 옮기지
  않고(BFD도 Geneve로 살아 있다) SNAT·유동 IP가 말없이 다른 L2로 가기 때문이다. 그럴 때는 physnet을 먼저 빼고(`--no-physnet` —
  게이트웨이는 다음 후보로, 유동 IP는 중앙으로 넘어간다) 새 브리지로 다시 넣는다. `cluster leave`는 매핑·veth·OVS 브리지도 뺀다.
- **공급자 네트워크 만들기·바꾸기·지우기는 루트 `cluster.admin`**(물리 망은 노드 자원 — 브리지 네트워크와 같다). 폴더는
  `network.assign`을 나눠 주는 자리일 뿐이다(Neutron의 공유 외부 네트워크). 같은 `(physnet, VLAN)`은 클러스터에 하나(409
  `physnet_conflict`). physnet이 준비된 노드가 없어도 만들 수 있고 경고(`orbivisor-warning`)가 나온다.
- **서브넷의 뜻.** `gateway`는 **물리 라우터의 주소**, `pool`은 VM NIC 주소, **`floating`은 라우터 바깥 주소와 유동 IP**다.
  라우터·유동 IP는 `floating`이 있으면 거기서만, 없으면 `pool`에서 고른다. 주소를 직접 줄 때(`fip create --ip`, 라우터
  `external ips`)도 그 범위 안이어야 한다 — 루트 `cluster.admin`만 범위 밖(서브넷 안)의 주소를 고른다(400 "only a cluster
  administrator …"). 물리 망의 다른 장비(DNS·NTP 따위) 주소를 테넌트가 유동 IP로 가로채지 못하게 하려는 것이다(Neutron도 고른
  유동 IP 주소는 관리자만). NIC의 자동 할당은 `floating`을 건너뛴다. 셋은 한 주소 공간이라 한 주소는 NIC·라우터·유동 IP 가운데
  하나만 갖는다(409 `ip_in_use` "… by a router or floating IP").
- **DHCP는 기본 끔**(`dhcp=on`으로 켬). 물리 망에 DHCP 서버가 있으면 그것도 VM에 답하고, VM이 다른 주소를 받으면 포트 보안이
  막는다 — 그 VLAN에 다른 DHCP가 없을 때만 켠다. 켜면 기본 경로(`router` = 서브넷 게이트웨이)도 준다. MTU 기본은 **1500**
  (1280~9000, 물리 망에 맞춘다).
- **VM을 곧바로 붙이기.** `--nic ext`처럼 붙이면 VM이 물리 망에 그대로 선다(주소는 `pool`에서, `network.assign` 권한). `ip=`로
  고르는 주소도 **`pool` 안**(유동 IP 범위 밖)이어야 한다 — 루트 `cluster.admin`만 그 밖(서브넷 안)을 고른다(400 "only a cluster
  administrator …"). 공급자 네트워크에서는 IPAM 주소가 포트 보안의 주소라, 아니면 유동 IP를 만들라고 `network.assign`을 받은
  테넌트가 물리 망의 다른 장비(DNS·게이트웨이) 주소를 NIC 주소로 잡아 가로챌 수 있다(Neutron도 고정 IP 고르기는 관리자만).
  이미 가진 주소와 오버레이 SDN 네트워크(계획 65)는 그대로다. VM은 physnet이 준비된 노드에서만 돈다. 라이브 마이그레이션 동안(두 chassis 바인딩) OVN이 터널을 쓰므로 그 몇 초 동안 1442바이트보다
  큰 패킷은 버려진다.
- **IPv6와 물리 라우터의 RA.** 공급자 IPv6 서브넷의 NIC는 cloud-init에 고정 주소(`static6`, 물리 라우터 게이트웨이)와 함께
  **`accept-ra: false`**를 받는다. 물리 라우터가 A 플래그 RA를 보내면 게스트가 SLAAC 주소를 하나 더 만들고, 그 주소로 나가는
  패킷은 포트 보안(IPAM 주소만)이 막아 IPv6가 가끔 끊기기 때문이다(오버레이는 Orbivisor 라우터만 RA를 보내 이런 일이 없다).
  cloud-init을 쓰지 않는 게스트는 그 NIC의 SLAAC를 끄거나, 물리 라우터가 SLAAC를 광고하는 망이면 서브넷에 `ipv6-mode=slaac`를
  줘 IPAM 주소를 게스트가 만드는 EUI-64와 같게 한다.
- **지우기.** 라우터의 바깥이거나 유동 IP가 있으면 409 `network_in_use`(읽을 수 있는 것은 이름, 나머지는 "N router(s) you
  cannot see"처럼 수만). 라우터·유동 IP가 쓰는 서브넷 빼기는 409 `subnet_in_use`. 지운 네트워크 이름을 라우터·유동 IP가 아직
  가리키면(지우기와 라우터 만들기가 겹침) 그 이름으로 새 네트워크를 만들 수 없다(409 `network_in_use` — 남의 라우터가 새
  네트워크의 기본 경로가 되지 않게). 그 가리킴을 먼저 지운다.

### 라우터와 경로

라우터는 오버레이 SDN 네트워크들을 잇고, 바깥(공급자 네트워크)이 있으면 그리로 나간다. OVN 분산 라우터(`Logical_Router`)라서
**같은 라우터의 네트워크 사이 트래픽(동서)은 보낸 VM의 노드에서 라우팅돼** 받는 VM의 노드로 곧장 간다 — 게이트웨이 노드를
지나지 않는다(여러 노드 시험에서 게이트웨이 노드의 Geneve 캡처 0개).

```bash
orbivisorctl network create t1 --sdn --subnet 10.10.0.0/24 --folder /teamA
orbivisorctl network create t2 --sdn --subnet 10.20.0.0/24 --subnet fd00:20::/64,ipv6-mode=slaac --folder /teamA
orbivisorctl router create r1 --external ext --interface t1 --interface t2 --folder /teamA
# router r1 created (external 203.0.113.100, 2001:db8:0:1::2)
orbivisorctl router route add r1 192.168.50.0/24 10.10.0.254      # 정적 경로(다음 홉은 붙은 서브넷 안)
orbivisorctl router route remove r1 192.168.50.0/24
orbivisorctl router interface remove r1 t2
orbivisorctl router detach t2                                     # 내 네트워크를 남의 라우터에서 떼기(네트워크의 network.config)
orbivisorctl router external set r1 ext,snat=off                  # 같은 네트워크면 바깥 주소는 그대로
orbivisorctl router external set r1 ext,ip=203.0.113.150          # 바깥 주소를 직접
orbivisorctl router external clear r1
orbivisorctl router list
# NAME  EXTERNAL            SNAT  INTERFACES  GATEWAY
# r1    ext 203.0.113.100   on    t1,t2       n2
orbivisorctl router get r1
# router r1
# interfaces: t1, t2
# external: ext 203.0.113.100, 2001:db8:0:1::2 (snat on)
#
# NODE  PRIORITY  NODE-PRIORITY  ACTIVE
# n2    32767     200            yes
# n3    32766     100            -
#
# routes the physical router needs (no NAT):
#   fd00:20::/64 via 2001:db8:0:1::2
orbivisorctl router delete r1                                     # 인터페이스가 없어야 한다(409 router_in_use)
```

웹은 **네트워크 › 라우터·유동 IP › 라우터**(`/routers`) 보기다. "라우터 만들기"(이을 SDN 네트워크와 바깥 네트워크)와, 이름을 누르면
여는 서랍: 인터페이스 더하기·빼기(붙일 수 있는 네트워크만 보임), 바깥 네트워크와 SNAT 바꾸기, 정적 경로 표, 게이트웨이 표
(노드·노드 우선순위·순위·주인), "게이트웨이 노드 없음" 경고 띠, 물리 라우터에 둘 IPv6 경로.

- **인터페이스.** 서브넷이 있는 오버레이 SDN 네트워크(공급자 아님)만 붙는다. 그 네트워크 서브넷의 **게이트웨이 주소가 라우터
  포트의 주소**가 되므로 한 네트워크는 라우터 하나에만 붙는다(409 `network_in_use` — 읽을 수 없는 라우터면 "a router you cannot
  see"). 네트워크 주인(그 네트워크의 `network.config`)은 라우터를 읽을 수 없어도 `orbivisorctl router detach NET`(`DELETE
  /api/v1/networks/{name}/router`)으로 뗀다 — 다른 폴더의 라우터에 이어 준 뒤 권한을 거뒀을 때 되찾는 길이다(그 길로 라우팅되는
  유동 IP가 있으면 409 `fip_in_use`). 한 라우터의 인터페이스·바깥 서브넷끼리
  CIDR이 겹치면 400이다(폴더가 다른 네트워크는 같은 CIDR을 써도 되지만 같은 라우터에는 못 붙는다). 인터페이스 16개까지.
  인터페이스인 네트워크에 다른 인터페이스와 겹치는 서브넷을 더하면 400 `invalid_subnet`.
- **바깥(external).** 공급자 네트워크 하나. 바깥 주소는 체계마다 하나이고 주지 않으면 서버가 `floating`(없으면 `pool`)에서
  고른다. 바깥이 있으면 **기본 경로**(`0.0.0.0/0`·`::/0`)가 그 서브넷 게이트웨이로 저절로 생기고 사용자 경로로 기본 경로를 줄
  수 없다(400).
- **정적 경로.** 64개까지, 목적지와 다음 홉은 같은 체계, 다음 홉은 붙은 서브넷 안, 같은 목적지 두 번은 400(ECMP 없음). 가장
  긴 접두사가 이긴다.
- **기본 경로를 게스트에.** 라우터에 붙은 네트워크의 DHCP가 `router` 옵션(= 서브넷 게이트웨이)을 준다. 이미 켜진 게스트는 임대
  갱신 때(최대 12시간 — 임대 86400초의 절반) 받는다 — 곧바로 원하면 게스트에서 DHCP를 다시 받는다. DHCP를 끈 서브넷의 고정
  주소 게스트는 cloud-init에 게이트웨이가 적히지만 instance-id가 NIC 주소로만 정해지므로 **이미 부팅한 게스트는 다시 적용하지
  않는다** — 게스트에서 기본 경로를 직접 더하거나 새로 만든 VM부터 받는다. VM이 라우터 있는 네트워크에 SDN NIC를 둘 이상
  가지면 기본 경로가 둘이 된다(Neutron과 같은 한계).
- **MTU.** 라우터 포트는 네트워크 MTU를 `gateway_mtu`로 갖는다. 바깥에서 들어오는 큰 패킷은 [MTU](#mtu-바깥에서-들어오는-큰-패킷)를 본다.
- **권한·테넌트.** 라우터는 네트워크 폴더에 산다(아래 [폴더 권한과 할당량](#폴더-권한과-할당량)). 라우터 하나에 바깥은
  하나이고 라우터끼리 잇기는 없다.
- **API.** `GET|POST /api/v1/routers`, `GET|PATCH|DELETE /api/v1/routers/{name}`. 만들기는 `{"name": "r1", "spec":
  {"external": {"network": "ext", "snat": true}, "interfaces": [{"network": "t1"}], "routes": [{"destination": "192.168.50.0/24",
  "nexthop": "10.10.0.254"}], "folder": "<id>"}}`, `PATCH`는 보낸 칸만(`description`·`external`(null은 지우기)·`interfaces`·
  `routes` — 목록은 통째로, `If-Match`). 경로나 바깥을 바꾸는 `PATCH`는 **모든 인터페이스 네트워크의 `network.config`**가
  있어야 한다(경로·바깥은 그 네트워크들 모두의 길이다). 바깥 주소는 `external`을 보낸 `PATCH`만 채운다(설명만 바꾸는 `PATCH`가
  바깥 네트워크에 새로 생긴 체계의 주소를 받지 않는다). 읽기는 보기(`gateways` — 후보마다 `online`(노드가 살아 있음) —
  `gateway_error`·`hosting`·`live_error`·`upstream_routes`)이고
  `?live=false`면 OVN을 읽지 않는다. 지금 주인(`hosting`·`active`)은 OVN NB의 게이트웨이 포트 칸 셋만 읽는다(라우터 하나면 그
  포트 하나). 루트 `cluster.read`가 없는 사용자(폴더 테넌트)에게는 게이트웨이 노드 이름(`gateways[].node`·`hosting`)이 빈 글이다
  — 순위·우선순위·주인 표시는 보인다. `If-Match` 없이 지워도 읽은 리비전으로 지워, 그사이 인터페이스가 더해졌으면 지우지 않는다.

### 게이트웨이 HA

라우터의 바깥 포트(OVN 분산 게이트웨이 포트)는 **한 번에 한 게이트웨이 노드**에 있다. SNAT, 중앙 유동 IP, 물리 망에 대한 ARP
답이 그 노드에서 나간다. 그 노드가 죽으면 OVN이 BFD로 알아채고 다음 노드로 넘긴다(`HA_Chassis_Group`).

- **후보.** `node sdn --gateway`를 준 노드 가운데 **유지보수가 아니고 바깥 네트워크의 physnet이 준비된** 노드. 노드가 살아
  있는지는 OVN이 BFD로 고르므로 후보 조건이 아니다.
- **순위.** 관리자 우선순위가 높은 순, 같은 우선순위 안에서는 라우터·노드의 해시(HRW) 순으로 앞 **5개**를 라우터마다 고른다
  (`HA_Chassis.priority` 32767, 32766, …). 같은 우선순위의 게이트웨이 노드에 라우터들이 고르게 나뉘고, 노드를 더하거나 빼도
  그 노드가 이기거나 지던 라우터만 움직인다. 관리자 우선순위는 "이 노드를 먼저"의 손잡이다(바깥 회선이 좋은 노드 200, 나머지
  100 따위). `router get`·웹 라우터 서랍의 게이트웨이 표가 순위와 지금 주인(`ACTIVE`, OVN의 `hosting-chassis`)을 보인다. 웹
  라우터 목록의 **"1순위 게이트웨이"** 칸은 OVN을 읽지 않고 순위가 가장 높은 후보(또는 "게이트웨이 노드 없음")와 "지금 활성
  노드는 라우터를 열어 봅니다" 줄을 보인다. 그 노드가 꺼져 있으면(`gateways[].online` 거짓) "꺼짐 — 대기 노드가 맡음" 칩이
  붙는다(OVN이 다음 살아 있는 후보로 넘겼다). 서랍의 표도 노드마다 같은 칩을 달고, OVN을 읽지 못했으면 활성 칸이 "?"(모름)다.
- **후보가 없으면** 바깥 포트를 꺼 둔다(물리 망에 라우터가 보이지 않음, SNAT·유동 IP 없음). 동서 라우팅은 그대로 된다.
  `router get`이 "warning: no gateway node has physnet ext ready; the external gateway is off", 웹이 "게이트웨이 노드 없음" 띠를
  보이고 유동 IP는 `unrouted`다.
- **넘김 시간**(여러 노드 시험 — 한 러너의 컨테이너 셋, OVN 24.03, BFD 기본값):

| 일 | 주인이 바뀜 | SNAT ping이 다시 닿음 |
|---|---|---|
| 게이트웨이 노드의 `systemctl stop ovn-controller`(점잖은 멈춤) | 0.11~0.16초 | 0.15~0.2초 |
| `kill -9` ovs-vswitchd·ovn-controller(BFD가 끊김) | 2.8~3.1초 | 4.1~4.4초 |
| 노드 통째로 죽음(`docker kill` — Orbivisor 투표 멤버·OVN 중앙·게이트웨이 주인이 함께) | 3.1초 | 3.1초 |
| 되돌아감(높은 노드가 돌아옴) | 1.1~1.9초 | 2.4~3.1초 |

  실제 하드웨어 장애(전원·링크)는 BFD 감지 시간이 비슷하리라 보지만 재지 않았다. 점잖은 멈춤은 ovn-controller가 바인딩을 놓아
  BFD를 기다리지 않는다. CI(`sdn-multinode`)는 바쁜 러너에서 거짓 실패가 없게 넘김 상한을 60초로 넉넉히 두고 잰 값을 로그
  (`PROBE`)에 남긴다.
- **맺은 SNAT 연결은 끊긴다.** conntrack은 노드마다라 새 게이트웨이로 옮겨지지 않는다 — 넘김 뒤 새로 맺는다(Neutron L3 HA·NSX
  액티브·스탠바이와 같다). 넘긴 노드가 GARP로 물리 망에 알린다.
- **되돌아감은 늘 일어난다.** 우선순위가 높은 노드가 돌아오면 OVN이 곧바로 다시 그쪽으로 넘긴다(두 번째 짧은 끊김). OVN에
  되돌아감 막기가 없으므로 Orbivisor도 두지 않는다. 고친 노드를 돌려놓을 때는 **유지보수로 둔 채 켜서 확인한 뒤** 원하는
  시각에 유지보수를 푼다(그때 넘어간다).
- **유지보수·비우기.** 유지보수를 켜면(`node drain` 포함) 그 노드가 후보에서 빠져 그 노드가 주인인 라우터가 다음 후보로 넘어간다
  — 계획된 넘김. 노드를 지우면 다음 동기화에서 그 노드의 `HA_Chassis`가 빠진다.

### SNAT와 유동 IP

**SNAT.** 바깥이 있는 라우터는 기본으로 인터페이스 네트워크의 **IPv4 서브넷마다** 보낸 주소를 라우터 바깥 IPv4 주소로 바꾼다
(OVN `snat`, 게이트웨이 노드에서 — 여러 노드 시험에서 SNAT 패킷은 주인 게이트웨이의 물리 포트로만 나갔다). `snat=off`면
라우팅만 하므로 물리 라우터가 안쪽 CIDR을 라우터 바깥 주소로 보내는 경로를 가져야 한다(`router get`의 "routes the physical
router needs"가 그 줄을 보인다). IPv6에는 NAT가 없다(아래).

**유동 IP**(플로팅 IP)는 공급자 네트워크의 IPv4 주소 하나를 VM NIC 하나에 1:1로 잇는다(OVN `dnat_and_snat`). 밖에서 그 주소로
들어오고, 그 NIC에서 나가는 패킷의 보낸 주소도 그 주소가 된다(라우터의 SNAT보다 앞선다). 리소스라서 VM보다 오래 살고(VM을
바꿔 붙여도 같은 주소) 권한·할당량이 주소에 붙는다.

```bash
orbivisorctl fip create --network ext                           # floating 범위에서. 이름은 주소의 점을 -로
# floating IP 203-0-113-101 created (203.0.113.101)
orbivisorctl fip create --network ext --ip 203.0.113.120 --name web --attach web1:0 --folder /teamA
orbivisorctl fip attach 203-0-113-101 db1:52:54:00:ab:cd:ef      # NIC는 순번(0부터)이나 MAC
orbivisorctl fip list
# NAME           ADDRESS        NETWORK  VM                        STATE
# web            203.0.113.120  ext      web1/52:54:00:12:34:56    distributed n1
# 203-0-113-101  203.0.113.101  ext      db1/52:54:00:ab:cd:ef     centralized
orbivisorctl fip get 203-0-113-101
# …
# centralized: the VM is not running
orbivisorctl fip detach web
orbivisorctl fip delete web                                     # 붙어 있어도 지운다(떼고 지움)
```

웹은 **네트워크 › 라우터·유동 IP › 유동 IP**(`/floating-ips`) 보기다. 목록(주소·붙은 VM 링크·상태 칩), "유동 IP 만들기"(바깥 네트워크,
주소·이름은 선택, 곧바로 붙일 곳), 행마다 "붙이기"(라우팅되는 오버레이 SDN 인터페이스만 고를 수 있고, 못 고르는 것에는 까닭 —
템플릿, 라우터에 없음, IPv4 없음, 다른 유동 IP가 붙음, 권한 없음. 라우팅은 서버가 `GET /routers`의 `routed`로 알려 볼 수 없는
라우터 뒤의 인터페이스도 고를 수 있다)·"떼기"·삭제. VM 상세 › 하드웨어의 NIC 표에 "유동 IP" 칸(링크)이 있다
— 붙이기·떼기는 유동 IP 보기에서 한다.

- **붙일 수 있는 NIC.** NIC의 네트워크가 **유동 IP의 네트워크를 바깥으로 가진 라우터의 인터페이스**이고 NIC에 IPv4 주소가 있어야
  한다(아니면 409 `fip_not_routable`과 까닭). NIC 하나에 유동 IP 하나(409 `fip_in_use`), 템플릿에는 못 붙인다. 붙인 유동 IP가
  있는 네트워크를 라우터에서 빼거나 라우터 바깥을 바꾸면 409 `fip_in_use`(먼저 뗀다).
- **상태**(`fip list`의 STATE, 웹의 칩): `unattached`(붙지 않음), `distributed n1`(분산 — 그 노드에서 바로 나감), `centralized`(중앙
  — 게이트웨이 노드가 맡음, 까닭과 함께), `unrouted`(라우팅 안 됨 — 게이트웨이 후보가 없거나 경로가 끊김, 까닭과 함께). 붙은 VM을
  읽을 수 없는 사용자에게는 까닭에 VM 이름 대신 "the target VM"이, 루트 `cluster.read`가 없으면 노드 이름이 빈다.
- **분산과 중앙은 저절로 고른다.** VM이 도는 노드(마이그레이션 중이면 두 노드 모두)가 **바깥 네트워크의 physnet을 준비됨으로
  알리면 분산**이다: NAT를 VM의 노드가 하고 물리 망에 유동 IP의 MAC으로 NIC MAC을 알리므로, 트래픽이 게이트웨이 노드를 지나지
  않는다(게이트웨이 대역폭이 병목이 되지 않고 게이트웨이 넘김에 끊기지 않는다). 아니면(꺼진 VM, physnet이 없는 노드) **중앙**:
  게이트웨이 노드가 NAT를 한다. 그래서
  - 모든 계산 노드에 바깥 망이 닿으면(노드마다 `--physnet`) 유동 IP는 대개 분산이다 — 권하는 모양.
  - 바깥 망이 게이트웨이 노드 몇에만 닿는 클러스터도 유동 IP가 된다(중앙) — Neutron의 전역 스위치
    `enable_distributed_floating_ip`처럼 바깥 망이 없는 계산 노드에서 끊기는 일이 없다.
  - 바뀌는 때는 VM이 노드를 옮길 때와 노드의 physnet이 바뀔 때뿐이다. 여러 노드 시험: 분산 유동 IP 패킷은 VM 노드의 물리
    포트로만, 그 노드의 physnet을 빼면 게이트웨이 노드로(이웃 표의 MAC이 라우터 MAC으로), 다시 넣으면 분산. VM을 c1 → c3로
    라이브 마이그레이션하면 끝난 뒤 **약 8초**(GARP가 물리 브리지의 MAC 표를 고칠 때까지) 지나 c3의 물리 포트로 옮겨 갔다.
- **VM을 지우면** 그 VM을 가리키는 유동 IP가 떨어져(`unattached`) 남는다. 복제·템플릿 배포·복원·가져오기는 새 MAC이라 유동
  IP를 물려받지 않는다. 원격 복제의 받는 클러스터로도 가지 않는다.
- **API.** `GET|POST /api/v1/floating-ips`(`{"name"?: …, "spec": {"network": "ext", "address"?: …, "target"?: {"vm": "<id>",
  "mac": "52:54:…"}, "folder"?: …}}`), `GET|PATCH|DELETE /api/v1/floating-ips/{name}`(`PATCH {"target": {…} | null,
  "description": …}`). 읽기 보기에 `state`와 `vm_name`. 목록은 붙은 VM만 읽는다(유동 IP가 없으면 다른 것을 읽지 않음). 웹 VM
  상세는 유동 IP가 있을 때만 30초마다 다시 읽는다.
- 포트 포워딩(주소 하나의 포트를 여러 VM에)·IPv6 유동 IP는 없다.

### IPv6: SLAAC·DHCPv6·RA

IPv6 서브넷에 `ipv6-mode`를 주면 게스트가 라우터 광고(RA)와 DHCPv6로 주소를 받는다(계획 66). 없으면 계획 65 그대로 cloud-init
고정 주소다.

| `ipv6-mode` | NIC 주소(IPAM) | OVN | cloud-init |
|---|---|---|---|
| `slaac` | **EUI-64**(MAC에서) | RA `slaac` | `ipv6_slaac` |
| `dhcpv6-stateless` | EUI-64 | RA `dhcpv6_stateless` + DHCPv6(DNS만) | `ipv6_dhcpv6-stateless` |
| `dhcpv6-stateful` | 할당 범위에서 | RA `dhcpv6_stateful` + DHCPv6(주소·DNS) | `ipv6_dhcpv6-stateful` |

```bash
orbivisorctl network subnet add t1 fd00:10::/64,ipv6-mode=slaac,dns=fd00::53
orbivisorctl vm create web3 --memory 1024 --disk root --nic t1      # fd00:10::5054:ff:fe…(EUI-64)를 받는다
```

- **RA는 라우터 포트가 보낸다.** 그래서 RA(와 RA를 따르는 cloud-init 항목)는 **라우터에 붙은 네트워크에서만** 나간다. 라우터가
  없으면 DHCPv6 행만 생기고(상태 있음은 게스트가 스스로 물으면 받는다) cloud-init은 고정 주소를 쓴다 — `ipv6-mode`를 줄 수는
  있다. RA는 주기적으로도 나가고 MTU(1280 이상)·첫 IPv6 DNS 서버(OVN은 RDNSS 하나만)를 싣는다. 기본 경로는 라우터 포트의 링크
  로컬 주소다(시험: `default via fe80::… mtu 1442`).
- **SLAAC 계열은 `/64`만, 주소는 EUI-64.** 게스트가 MAC에서 만드는 주소와 같아야 포트 보안을 지나기 때문이다. `ip=`로 다른 주소를
  주면 400. 게스트의 **임시 주소(RFC 4941)와 안정 불투명 주소(RFC 7217)는 포트 보안이 막으므로** 게스트는 EUI-64 주소 만들기를
  써야 한다(Neutron과 같다). Ubuntu 24.04 클라우드 이미지는 cloud-init `ipv6_slaac`로 EUI-64 주소를 쓴다(GitHub `ovn` 작업의 실제
  게스트가 그 주소로 ping6에 답했다). 상태 있는 DHCPv6는 busybox `udhcpc6`로 확인했다.
- **바꾸기.** 그 서브넷의 IPv6 주소를 쓰는 NIC가 있으면 `ipv6-mode` 바꾸기는 409 `subnet_in_use`다(EUI-64와 할당 범위 주소가
  서로 달라 몰래 주소를 바꾸지 않는다). 새 `slaac` 서브넷을 더하면 주소 없는 NIC가 EUI-64 주소를 받는다.
- **NAT66은 없다.** 안쪽 IPv6 접두사는 라우팅만 되므로 물리 라우터가 그 접두사를 라우터 바깥 IPv6 주소로 보내는 정적 경로를
  가져야 한다. 바깥 네트워크에 IPv6 서브넷이 있으면 라우터가 바깥 IPv6 주소를 받고 `::/0`이 바깥 게이트웨이로 간다. `router get`·
  웹 라우터 서랍이 물리 라우터에 둘 줄(`fd00:20::/64 via 2001:db8:0:1::2`)을 보인다. 접두사 위임·BGP 광고는 없다.

### SDN NIC의 보안 그룹

[보안 그룹](#vm-방화벽-보안-그룹)은 SDN NIC에도 건다(계획 66). **같은 그룹 리소스, 같은 뜻**이다: NIC의 그룹 순서 → 그룹 안
순서로 **처음 맞는 규칙**, 맞는 것이 없으면 들어오기 막기·나가기 보내기, `out accept`는 받는 쪽 `in` 검사로 넘김, 늘 허용
(응답·DHCP 응답), 없는 그룹이면 찾은 그룹의 규칙 뒤에서 모두 막기. 그래서 이미 쓰던 그룹(drop 규칙·순서 포함)을 SDN NIC에
그대로 건다. 집행은 노드의 nftables가 아니라 OVN ACL이다.

```bash
orbivisorctl vm create web1 --memory 1024 --disk root --nic t1,sg=web+ssh --start
orbivisorctl vm nic-sg web1 0 web          # 실행 중에도 그룹만 바꾼다(단순 NIC와 같은 규칙)
```

**단순 NIC와 다른 것**(집행하는 곳이 주는 차이뿐):

| | 단순 NIC(nftables, 노드마다) | SDN NIC(OVN ACL) |
|---|---|---|
| 보낸 IP 위조 | 막지 않는다(보낸 MAC만) | **막는다**(포트 보안이 MAC·IP 모두) |
| 규칙에서 허용을 빼면 | 맺은 연결은 conntrack 항목이 끝날 때까지 간다 | **맺은 연결도 끊긴다**(OVN은 요청 방향을 늘 ACL에 다시 비추고, 막히면 그 연결의 응답도 막음) |
| IPv6 이웃 찾기·RA(NS·NA·RS·RA·MLD) | 들어오기는 늘 허용, 나가기는 규칙을 따름 | **양 방향 늘 허용** |
| IP가 아니고 ARP도 아닌 프레임(LLDP, 다른 이더타입) | 들어오기는 막음(기본 막기가 ARP 말고 모두) | **들어오기도 지나감**(OVN 기본 막기는 `ip`만) |
| 노드 요구 | `status.firewall`(nftables), 409 `firewall_unsupported` | chassis면 된다 |
| 적용 | 노드 에이전트(0.5초 모아, 60초마다) | 리더 OvnSync(0.5초 모아, 60초마다) |

- **처음 맞는 규칙을 OVN에서.** OVN ACL은 한 층 안에서 가장 높은 우선순위의 맞는 ACL 하나가 판정하므로, NIC의 **그룹 목록**
  (순서 있음)마다 `Port_Group` 하나를 두고 그 목록의 펼친 규칙을 우선순위 내림차순(32000, 31999, …)으로 그린다. 같은 목록의
  NIC는 같은 Port_Group에 든다(`[web, ssh]`와 `[ssh, web]`은 다른 목록). 그룹을 걸지 않은 SDN NIC는 거르지 않는다.
- **열린 순간이 없다.** 새 목록의 Port_Group과 ACL을 먼저 쓰고, 포트를 넣거나 목록을 바꾸는 트랜잭션은 그 Port_Group이 있는지
  확인한 뒤에만 포트를 넣는다 — Port_Group 쓰기가 실패하면 포트가 생기지 않는다(ACL 없는 포트가 없다). 켜진 VM의 그룹 바꾸기는
  한 트랜잭션이다. GitHub `ovn` 작업에서 목록을 네 번 바꾸는 동안 막혀야 할 연속 탐침은 **232번 중 0번** 붙었고, 같은 그룹을
  건 nftables 이름공간과 OVN 이름공간의 탐침 표가 같았다.
- **라우터를 지나 들어오는 것.** 유동 IP로 들어오는 패킷은 DNAT 뒤 VM 포트의 `in` 규칙이 본다. 라우터·공급자 포트 자체에는
  ACL을 걸 수 없다(OVN).
- **남의 ACL.** 관리자가 `ovn-nbctl`로 만든 Port_Group·ACL(주인 표시 없음)은 두고, 클러스터 전체 칸인 `NB_Global
  options:default_acl_drop`도 건드리지 않는다.
- **업그레이드 먼저.** 모든 노드가 기능 `sdn-routing`을 알려야 SDN NIC에 그룹을 건다(아니면 409 `cluster_upgrading`). 섞인
  클러스터의 복제는 계획 65처럼 SDN NIC의 그룹을 뺀다.

### 부하 분산

부하 분산기(LB, 계획 67)는 VIP 하나로 오는 TCP·UDP 연결을 VM NIC(또는 주소) 여럿에 L4로 나눈다. OVN 내장 `Load_Balancer`라
서비스 VM·에이전트가 없고(Octavia의 OVN 공급자와 같은 자리), 안쪽 트래픽은 **클라이언트 chassis에서 분산**으로 DNAT된다(게이트웨이를
지나지 않음). 건강 검사가 죽은 멤버를 빼고, 라우터 바깥의 유동 IP 범위에서 받은 **바깥 VIP**로 물리 망에도 연다.

```bash
orbivisorctl lb create web-lb --network t1 --listener tcp/80 --listener tcp/443 --folder /tenant-a   # VIP는 t1 풀에서
orbivisorctl lb member add web-lb tcp/80 web1             # VM NIC 멤버(NIC 0, 리스너 포트)
orbivisorctl lb member add web-lb tcp/80 web2:1@8080      # NIC 1의 8080으로(VM 멤버의 포트는 @ — MAC에 :가 있다)
orbivisorctl lb member add web-lb tcp/80 10.20.0.30:80    # 주소 멤버(라우터 인터페이스 서브넷 안)
orbivisorctl lb health set web-lb tcp/80 --interval 5 --timeout 3 --rise 2 --fall 3   # 주지 않은 값은 기본 5초·3초·2·3
orbivisorctl lb external set web-lb ext                   # 바깥 VIP(ext의 유동 IP 범위에서, ip=A로 고름)
orbivisorctl lb get web-lb
# load balancer web-lb
# vip: 10.10.0.50 on t1
# external: 203.0.113.130 on ext
#
# LISTENER  MEMBER     ADDRESS     PORT  HEALTH   WARNING
# tcp/80    web1       10.10.0.11  80    online   -
# tcp/80    web2:1     10.20.0.12  8080  offline  -
# tcp/80    10.20.0.30 10.20.0.30  80    unchecked -
orbivisorctl lb list                                      # NAME NETWORK VIP EXTERNAL LISTENERS MEMBERS
```

- **VIP 네트워크.** 서브넷(IPv4)이 있는 오버레이 SDN 네트워크이고 **어떤 라우터의 인터페이스**여야 한다(아니면 400
  `lb_network_not_routed`). LB는 그 라우터와 라우터의 모든 인터페이스 스위치에 붙는다 — 라우터가 VIP의 ARP에 답해 **같은 서브넷
  클라이언트도 VIP에 닿고**(GitHub `ovn` 작업: 같은 서브넷·다른 서브넷 클라이언트 모두 두 멤버가 16·14로 나눔, 바깥을 뗀 라우터도
  같음), 다른 인터페이스 네트워크의 클라이언트는 자기 스위치에서 DNAT한다. VIP는 직접 주면 그 서브넷의 NIC 범위(풀) 안이어야 하고,
  없으면 풀에서 받는다. VIP 네트워크는 바꿀 수 없다(새로 만든다). LB가 VIP를 둔 네트워크를 라우터에서 떼거나, 바깥 VIP가 쓰는
  라우터 바깥을 바꾸면 409 `lb_in_use`.
- **리스너와 멤버.** 리스너는 `tcp|udp/포트` 1~16개, 리스너마다 멤버 64개까지. NIC 멤버는 그 NIC의 네트워크가 VIP 네트워크이거나
  **같은 라우터의 다른 인터페이스**이고 IPv4가 있어야 한다(아니면 400 `lb_member_unreachable`과 까닭). 붙은 뒤 깨지면(NIC를 빼거나
  VM을 지움) 멤버는 스펙에 남고 흐름에서만 빠져 `missing`(까닭)으로 보인다 — 몰래 스펙을 바꾸지 않는다(Octavia의 `ERROR`와 같다).
  멤버가 하나도 남지 않은 리스너는 연결을 바로 거절한다(RST·ICMP — 조용히 버리지 않음).
- **나누기와 출발지 고정.** OVN 기본(`dp_hash`, 5튜플 해시)이다. `--affinity SECS`(1~65535)면 같은 클라이언트 주소를 그 시간 동안
  같은 멤버로 보낸다(시험: 60초면 10번 모두 같은 멤버).
- **건강 검사.** 리스너마다 켠다(NIC 멤버만 — 주소 멤버는 `unchecked`). OVN 서비스 모니터가 멤버의 chassis에서 검사하고, `fall`번
  실패하면 그 멤버를 **새 연결**에서 뺀다(맺은 연결은 conntrack에 남아 그 멤버로 가다 끊긴다), `rise`번 되면 다시 넣는다. 기본값이면
  죽은 멤버를 약 15초 안에 뺀다. 잰 값: 간격 2초·`fall` 2로 노드 하나에서 3.6~3.7초, 여러 chassis(멤버 c1·c3, 클라이언트 c2)에서
  4.4초 뒤 남은 멤버만 답했고, 되살리면 2초에 `online`, 둘 다 죽으면 곧바로 `refused`. **UDP 검사는 약하다** — "ICMP 포트 닿지 않음이
  오지 않으면 online"이라 듣지 않는 서비스도 살아 보일 수 있다. 검사는 **IPv4만**이다(VIP도 IPv4만).
- **검사 출발 주소와 보안 그룹.** 검사는 LB와 멤버 네트워크마다 IPAM에서 잡은 주소(`monitors` — `network ips`의 `lb-monitor` 행)에서
  온다. 멤버 NIC에 보안 그룹이 있으면 그 주소에서 멤버 포트로 오는 것과 클라이언트를 받아야 한다(Octavia와 같다). API는 만들기·바꾸기
  때 멤버 NIC의 그룹이 검사·클라이언트를 막는지 셈해 경고 헤더와 `lb get`의 WARNING을 단다(막지는 않음 — 일부러 닫아 둘 수 있다).
  클라이언트 NIC의 `out` 규칙은 DNAT **앞**(목적지 = VIP), 멤버 NIC의 `in` 규칙은 DNAT **뒤**(목적지 = 멤버, 출발지 = 클라이언트)를 본다.
  멤버 NIC를 같은 라우터의 다른 인터페이스 네트워크로 옮기면(`PUT /vms`, 꺼진 VM) 그 LB의 검사 출발 주소를 다시 고른다 — 새 네트워크에
  주소를 잡고 쓰지 않게 된 네트워크의 것은 놓는다(전에는 옮긴 멤버가 검사 없이 늘 건강하게 보였다).
- **바깥 VIP.** VIP 네트워크 라우터의 바깥이 그 공급자 네트워크여야 하고, 주소는 유동 IP 범위에서 온다(범위 밖은 루트
  `cluster.admin`). 물리 망 → 게이트웨이 chassis에서 DNAT → 분산 라우팅으로 멤버. **SNAT가 없어 멤버는 바깥 클라이언트 주소를 그대로
  본다.** 게이트웨이가 넘어가면 새 게이트웨이가 바깥 VIP도 GARP로 알린다 — 여러 노드 시험에서 게이트웨이 주인이 0.25초에 바뀌고 물리 쪽
  ARP 캐시를 그대로 둔 채 1.3초에 다시 닿았다. 게이트웨이 후보가 없으면 `external_state: unrouted`(까닭)이고 경보
  `sdn_gateway_unavailable`이 뜬다. 바깥 VIP는 폴더 할당량 `floating_ips`를 하나 쓴다.
- **다른 기능과.** 같은 멤버 NIC에 유동 IP를 붙여도 된다. 멤버 VM이 라이브 마이그레이션하면 검사 chassis가 따라간다. LB는 VM 복제·
  템플릿·원격 복제를 따라가지 않는다(멤버는 MAC으로 가리키고 복제본은 새 MAC). L7(HTTP 규칙·TLS 종료)이 필요하면 LB 뒤에 HAProxy VM을
  둔다.
- **API.** `GET|POST /api/v1/load-balancers`, `GET|PATCH|DELETE /api/v1/load-balancers/{key}`(`PATCH`는 보낸 칸만, `listeners`는 통째로,
  `If-Match`). 보기 `LoadBalancerView`는 리소스에 `vip_state`·`external_state`(`ready | unrouted {reason}`)와 멤버마다 `address`·`health`
  (`online|offline|error|unchecked|missing`)·`warning`, `live_error`를 더한다(`?live=false`면 OVN을 읽지 않음). 감사 `lb create web-lb …`.
  웹 목록은 `?live=false`(OVN을 읽지 않음)를 watch 사건마다 다시 읽고, 멤버 건강만 `live`로 30초마다 따로 읽어 얹는다. 한 멤버의 경고가
  여럿이면(`; `로 이음) 하나씩 번역해 보인다. 부른 사람이 읽을 수 없는 VM의 멤버는 보기에서 VM id·MAC·주소 없이 건강만 나오고, 웹은
  "볼 수 없는 VM"으로 보인다. 웹 고치기 대화상자는 바깥 VIP 확인란이나 주소를 바꿨을 때만 `external`을 보낸다 — 라우터의 외부
  네트워크를 볼 수 없어도 있는 바깥 VIP가 그대로 남는다. 라우터를 읽을 수 없으면 멤버 후보 NIC를 라우터에 붙은 모든 오버레이
  네트워크에서 내놓고, 닿지 않는 것은 서버(400 `lb_member_unreachable`)가 가린다.

### QoS

NIC마다 보내기·받기 속도 상한과 DSCP 표시를 둔다(계획 67). **SDN NIC는 OVN `QoS` 행**, **단순 NIC(브리지·VLAN·VXLAN)는 tap의 tc**가
집행하고 둘 다 **폴리싱**(넘으면 버림)이라 같은 칸이 같은 뜻이다 — [옮기기](#단순-네트워크를-sdn으로-옮기기) 앞뒤에서도 그대로다.

```bash
orbivisorctl vm create web1 --memory 1024 --disk root --nic t1,qos-out=100,qos-in=200,dscp=46 --start
orbivisorctl vm nic-qos web1 0 out=50                     # 실행 중에도 곧바로(준 값만 바꿈)
orbivisorctl vm nic-qos web1 0 burst=4000                 # 버스트(kbit) — 없으면 속도의 80%
orbivisorctl vm nic-qos web1 0 --clear                    # 모두 뺌
```

| 칸 | 뜻 | SDN NIC | 단순 NIC |
|---|---|---|---|
| `out_mbps`(`out=`) | VM이 보내는 속도 상한, 1~100000 Mbit/s | `from-lport` `QoS` 행(우선순위 2000) | tap ingress의 matchall `police`(우선순위 49152) |
| `in_mbps`(`in=`) | VM이 받는 속도 상한 | `to-lport` `QoS` 행 | tap egress의 `police` |
| `burst_kbit`(`burst=`) | 버스트, 1~10000000 kbit, 기본 속도의 80% | 행의 `bandwidth.burst` | `police burst`(바이트로 적음) |
| `dscp` | VM이 보내는 IP 패킷의 DSCP, 0~63 | 행의 `action.dscp` | **없음** — 400 `qos_dscp_needs_sdn` |

- **정확도(잰 값, GitHub).** SDN NIC: `out_mbps=50` → 50.0, `in_mbps=30` → 30.1, 실행 중 20으로 → 20.1 Mbit/s, 여러 chassis
  (Geneve 너머 c1 → c3) 50 → 50.3, DSCP 46이 받는 쪽에 `tos 0xb8`로 보였다. 단순 NIC(tc): 40 → 38.8, 25 → 24.1, 바꾼 15 → 14.5.
- **버스트.** 폴리싱은 처음에 버스트만큼 더 보낸다 — 기본 버스트(속도의 0.8초분)가 짧은 측정을 부풀린다(5초 iperf3에서 50 Mbit/s 제한이
  57.7). 잴 때는 앞 몇 초를 빼고(`iperf3 -O 3`) 길게 잰다. TCP는 버림을 보고 줄여 대개 제한 근처로 모이고, **UDP는 넘는 만큼 잃는다**.
- **실행 중 바꾸기**는 장치 변경이 아니다(SDN은 OvnSync 한 차례, 단순 NIC는 핫플러그 Task의 `SetNicQos` 한 단계 — 바뀐 훅만 다시 넣고 같은
  값이면 건드리지 않음). 단순 NIC의 tc가 실패하면 스펙은 그대로 두고 보류(`failed`)로 남아 `vm hotplug`가 다시 한다.
- **노드.** 단순 NIC QoS는 tc `clsact`를 쓸 수 있는 노드(`NodeStatus.qos` — 시작할 때 시험 장치로 확인, macOS는 거짓)에만 놓이고
  (스케줄러, 아니면 409 `qos_unsupported`), 그 노드의 tap을 만들 때(켜기·마이그레이션 받기·핫플러그) 건다. 우선순위 49152만 우리 것으로
  보고 남의 tc 필터는 건드리지 않는다. QoS 없는 VM의 tap 명령은 바이트 그대로다. 웹 VM 폼의 QoS 칸은 고른 노드(없으면 모든 노드)가
  `qos`를 알리지 않으면 단순 NIC 속도 제한 옆에 "이 노드(모든 노드)에서 걸 수 없음"을 보인다. 오류 코드(`qos_unsupported` 따위)는 번역된다.
- **따라가는 것.** 라이브 마이그레이션(SDN은 포트를 따라, 단순은 받는 노드가 tap에 다시 건다), 템플릿·복제·가져오기·원격 복제(NIC 칸).
  SDN NIC의 행은 유동 IP·LB 트래픽도 함께 제한한다(같은 포트를 지나므로).
- `user` 네트워크 NIC의 `qos`는 400 `invalid_qos`(tap이 없어 아무것도 하지 않게 되므로). 권한은 VM의 `vm.config`(보안 그룹과 같다).
  감사는 VM 줄에 `nic-qos 0 out=100 in=200 dscp=46`(뺐으면 `nic-qos 0 clear`).

### 토폴로지

웹 **네트워크 › 토폴로지 그림**(`/networks/topology`)는 네트워크·라우터·게이트웨이·유동 IP·VM·LB를 **2.5D 그림**(아이소메트릭
SVG — WebGL이 아님) 하나로 보인다. 바닥에 오버레이 네트워크 판과 그 위의 VM 블록·LB 기둥, 가운데 높이에 라우터 블록과 게이트웨이
칩(활성 노드, 후보 없으면 경고 색), 뒤에 공급자 네트워크 판과 유동 IP·바깥 VIP 꼬리표, 옆에 회색 단순 네트워크 판(옮기는 중이면 대상
판으로 화살표와 진행 수)이다. 누르면 그 서랍(라우터·LB는 그 보기로 가서 서랍), 가리키면 이어진 길(VM → 네트워크 → 라우터 → 공급자,
LB → 멤버)이 밝아진다. 자세한 다루기는 [웹 화면 둘러보기](web-ui.md#sdn-화면).

```bash
orbivisorctl network topology --folder /tenant-a     # 같은 자료를 글 나무로(읽을 수 있는 것만)
```

API는 `GET /api/v1/network-topology?folder=&live=`(`system.read`)다. 읽을 수 없는 것은 빼고 `hidden`에 세며, 루트 `cluster.read`가
없으면 노드 이름을 비운다. 읽을 수 없는 VM인 LB 멤버는 주소도 비우고, 읽을 수 없는 폴더로 거르면 404다(남의 폴더의 감춘 수를 캐 보지
못하게). `live=true`면 OVN의 게이트웨이 주인과 LB 멤버 건강을 한 번씩 읽는다(기본 `false`). 화면은 그림을 `live=false`로 읽어 watch
사건(1초 모음)마다 다시 읽고, 살아 있는 상태는 `live=true`를 30초마다 **따로** 읽어 그림에 얹는다. 그래서 바쁜 클러스터에서도 사건마다
NB·SB를 읽지 않는다. 가리키기·초점·끌기는 원소를 다시 그리지 않는다(VM 2000대에서도 가리킨 원소 하나만).

**공급자 네트워크에만 NIC가 있는 VM**은 그 공급자 판(유동 IP·바깥 VIP 꼬리표 뒤)에 블록으로 놓이고, 목록 보기에서는 공급자 아래 유동 IP
다음에 나온다. **NIC가 모두 볼 수 없는 네트워크에 있는 VM**은 그림·목록에 그리지 않고 큰 그림 띠·이름 문턱에도 세지 않으며,
"볼 수 없는 네트워크에만 있는 VM N대는 그리지 않습니다" 한 줄로 수만 알린다.

### 단순 네트워크를 SDN으로 옮기기

옮기기 도구(계획 67)는 단순 네트워크(브리지·VLAN·VXLAN) **S**의 VM NIC를 서브넷이 있는 SDN 네트워크 **T**로 옮긴다. **주소를 그대로
가져가고**, VM마다 꺼진 채·다시 켜며·**라이브**(tap만 옮김)로 하나씩 옮기며, 그동안 두 쪽 L2를 OVN `l2gateway` 다리로 잇고, 언제든
되돌린다. 옮기는 것은 NIC 스펙(`network`, 주소, cloud-init NIC 항목)뿐이고 MAC·모델·보안 그룹·QoS는 그대로다(보안 그룹은 같은 뜻의 OVN
ACL이 된다). 원본 네트워크는 지우지 않는다.

```bash
orbivisorctl network create t-lan --sdn --subnet 10.66.0.0/24,gateway=10.66.0.1   # 보통 원본 L2와 같은 CIDR, 게이트웨이 = 물리 라우터
orbivisorctl network migrate lan --to t-lan --dry-run     # 계획만(리소스를 만들지 않음)
# VM    NIC  MODE     ADDRESS     SOURCE       BLOCKER/WARNING
# web1  0    live     10.66.0.21  guest-agent  -
# db1   0    offline  10.66.0.11  cloud-init   the cloud-init instance-id changes: …
# app1  0    blocked  -           -            the VM runs on n4, which is not an OVN chassis
# bridge: on node n1
orbivisorctl network migrate lan --to t-lan --ip app2=10.66.0.40    # 만들고 시작(이름 lan-to-t-lan, 쓰였으면 -2 …), 막힌 VM은 건너뜀
orbivisorctl network migration get lan-to-t-lan            # VM NIC마다 MODE·ADDRESS·STATE·ERROR
orbivisorctl network migration pause lan-to-t-lan          # 지금 VM이 끝난 뒤 멈춤 — resume으로 이어 감
orbivisorctl network migration rollback lan-to-t-lan web1  # 이 VM만 원본으로(없으면 모두)
orbivisorctl network migration finish lan-to-t-lan --router r1   # 다리를 걷고 T를 r1 인터페이스로
```

- **미리 보기.** `--dry-run`(API `POST /network-migrations?dry_run=true`)이 VM NIC마다 방식·옮겨 갈 주소와 출처·막는 까닭·경고, 다리 노드를
  보인다. 만들 때 이 계획을 적고, Task가 VM마다 **그때 다시** 계획해 달라졌으면(전원·주소가 바뀜) 그 VM만 `failed`(까닭)로 둔다.
  막는 까닭: 템플릿 VM, T 서브넷 밖·예약 주소, 다른 VM과 같은 주소(단순 네트워크에는 IPAM이 없어 겹칠 수 있다), 이미 쓰는 주소, 같은
  VM의 다른 NIC가 이미 T에(같은 옮기기가 옮긴 NIC는 빼고 — 원본에 NIC가 둘인 VM은 둘 다 옮긴다), 다른 옮기기 중, 켜진 VM이 chassis가
  아닌 노드에 있음, 권한 없음. 경고: 주소가 바뀜(`ipam`), MTU가 줄어듦, 게스트 에이전트 없음, 원본에 보안 그룹 없음, DHCP 다툼, IPAM이
  원본 L2의 다른 장비 주소를 모름(다리), cloud-init instance-id 바뀜. **읽을 수 없는 VM**(`system.read` 없음)은 계획에 이름·MAC·주소 없이
  수(`hidden`)만 나오고 옮기지 않으며 리소스의 VM 기록에도 적히지 않는다. 스펙의 `vms`·`addresses`에 그런 VM을 주면 없는 VM과
  같다(400). `network-migrate` Task의 오류·경고는 VM id 없이 수만 적는다(`2 NIC(s) failed to move`) — 까닭은 옮기기 기록의 VM마다
  `error`에 있고, 읽을 수 없는 VM의 것은 보기에서 빠진다.
- **웹 마법사.** 네트워크 목록의 단순 네트워크 줄 "SDN으로 옮기기"(진행 중이면 진행 칩)가 연다. 순서는 ① 대상(또는 같은 CIDR의 새 SDN
  네트워크)과 방식·다리 노드, ② 계획 표, ③ 진행, ④ 끝내기다. 마법사가 지키는 것:
  - **미리 보기는 늘 원본의 모든 VM이다.** VM을 빼면 그 줄이 흐려지고 다시 넣을 수 있다. `vms`는 만들 때만 보낸다.
  - **화면의 계획대로만 시작한다.** 방식이나 주소를 고치면 시작 단추가 막히고 "다시 계획"을 하라고 알린다.
  - **만들 때 서버가 다시 계획한다.** 그새 VM이 켜져 `auto`가 꺼진 채 → 라이브·다시 켬이 되는 따위로 달라졌으면, 시작하기 전에 다른 점을
    보이고 묻는다. 꺼졌다 켜지거나 라이브로 옮겨질 VM이 맨 앞이다. 시작하지 않으면 옮기기는 `planned`로 남아 진행 단계에서 시작하거나
    지운다. 시작이 실패해도 그렇다.
  - **이름은 서버가 고른다.** `<원본>-to-<대상>`이고, 끝난 기록이 그 이름을 쓰면 `-2`, `-3`…이다(API `name`을 비우면 — CLI `--name`이
    없을 때도). 그래서 같은 원본을 다른 방식으로 다시 옮길 때 옛 기록을 먼저 지우지 않아도 된다. 이름을 주면 그 이름이다(쓰였으면 409).
  - **연 옮기기는 끝내거나 모두 되돌린 뒤에도 기록으로 보인다.** "기록 지우기"와 "닫기"가 있다.
  - **시작 단추 위에 무엇이 일어나는지 센다**: 꺼졌다 켜질 VM, 라이브로 옮겨질 VM(약 1초 끊김), 꺼진 채 바뀔 VM(뺀 VM·막힌 NIC 빼고).
    다시 켜질 VM이 있으면 경고색이다.
  - **공급자 대상에는 `bridge: false`를 보낸다**(다리를 놓지 않으니 같은 공급자 네트워크로 여럿을 함께 옮길 수 있다).
  - **다리 노드가 꺼지거나 지워지면** 진행 단계에 "새 다리 노드"가 나와 다른 노드로 이어 한다(꺼진 노드는 후보에서 뺌).
  - **이어 가기의 새 다리 노드 후보는 원본 브리지가 있는 chassis뿐이다.**
  - **볼 수 없는 VM은 수로만 나온다.** 원본에 그런 VM이 있으면 "볼 수 없는 VM N대 — 옮기지 않음"이다.
- **방식**(`--mode`, 옮기기 하나에 하나 — 기본 `auto`):

| 방식 | 언제(`auto`) | 하는 일 | 끊김 |
|---|---|---|---|
| `offline` | VM이 꺼짐 | 스펙만 바꾼다(다음 켜기에 스케줄러가 chassis를 고름) | 없음 |
| `live` | 켜짐, VM 노드가 chassis, 게스트가 볼 MTU가 줄지 않음(또는 `--allow-mtu-shrink`), 주소를 앎 | tap을 Linux 브리지에서 떼어 `br-int`에 붙이고 `ovn-installed`를 30초까지 기다린 뒤 QMP `set_link` 끔·켬(게스트가 링크를 다시 보고 DHCP를 다시 묻게). QEMU 장치·PID는 그대로, 게스트에 NIC가 빠졌다 꽂히지 않는다. 오지 않으면 tap을 되돌리고 그 VM은 `failed` | 보통 1초 안 |
| `restart` | 켜짐, `live`가 안 됨 | ACPI 끄기 → 스펙 바꾸기와 켜기를 한 번에(켜지 못하거나 15분 안에 켜지지 않으면 끄고 원래 네트워크로 되돌려 다시 켬) | 끄기 + 켜기 |
| `blocked` | 막는 까닭이 있음 | 건너뜀 | — |

  `--mode live`인데 안 되는 VM은 `blocked`다(몰래 다시 켜지 않는다). VM마다 다른 방식이 필요하면 그 VM을 빼고(`--vm`) 끝낸 뒤 새 옮기기로
  한다. 원본 MTU는 VXLAN이면 그 MTU, 브리지·VLAN은 1500으로 본다(오버레이 T는 1442라 그대로면 `restart`). `--parallel N`(1~8, 기본 1).
- **주소.** 출처 차례는 `given`(`--ip VM[:NIC]=A[+B]`) → `guest-agent`(게스트 에이전트가 이 MAC에 보고한, T 서브넷 안의 주소) →
  `cloud-init`(그 NIC의 고정 주소) → `ipam`(T 풀의 새 주소 — 주소가 바뀜). 주소를 모르고(`ipam`) 게스트가 고정 주소면 옮긴 뒤 포트 보안이
  그 주소를 막으므로 `--ip`로 주거나 게스트 에이전트를 켠 뒤 한다. cloud-init 고정 주소 항목은 지우고(SDN NIC의 주소는 IPAM이 정함)
  되돌리기가 되살린다. 되돌리면 단순 네트워크가 받지 않는 `qos.dscp`는 빠진다(속도 제한은 그대로).
- **주소 잡아 두기.** 끝나지 않은 옮기기가 계획한 주소(원본에 남은 VM이 아직 쓴다)와 원본에 NIC가 있는 VM의 cloud-init 고정 주소(T 서브넷
  안)는 T에서 **쓴 주소**다: 새 VM·다른 옮기기의 `ipam` 주소·LB VIP·건강 검사 주소·유동 IP·라우터 바깥이 받지 않고, 준 주소면 409
  `ip_in_use`다(다리가 두 L2를 잇는 동안 ARP가 겹치지 않게). **계획에 없이 원본에 남는 켜진 VM**(읽을 수 없거나 뺀 VM)이 게스트
  에이전트로 알린 T 서브넷 안의 주소도 만들 때 서버가 물어 VM id 없이 잡아 둔다(보기에는 나오지 않는다). 옮기는 VM은 그 주소로 간다.
  끝내거나 모두 되돌리면 풀린다. IPAM은 원본 L2의
  다른 장비(물리 라우터·서버, 게스트 에이전트 없는 VM)의 주소를 모르므로, 다리를 놓는 동안 T의 풀에서 그 주소를 빼 둔다(계획 경고).
- **cloud-init instance-id가 바뀐다.** 씨앗 키가 cloud-init 주소 항목과 NIC 주소를 함께 세므로 옮긴 VM의 instance-id가 달라지고, 그
  게스트는 **다음 부팅에 인스턴스마다 하는 일(SSH 호스트 키 새로 만들기 따위)을 다시 한다.** 계획과 웹 마법사가 cloud-init이 있는 VM에
  이 경고를 단다. 피할 수 없다면 옮긴 뒤 SSH 호스트 키가 바뀐다고 알린다.
- **다리**(`l2gateway`). 대상이 오버레이면 첫 VM 앞에서 T 스위치에 `l2gateway` 포트 `opl-l2gw-<옮기기 id>`를 두고, **다리 노드**(원본
  브리지가 있는 chassis 가운데 옮길 VM이 가장 많은 노드, 또는 `--bridge-node`)가 계획 66 physnet 길(veth `oplp<h8>{o,b}`·OVS 브리지·
  `ovn-bridge-mappings`의 `mig<h8>`)로 원본 브리지에 잇는다. 옮긴 VM과 남은 VM이 같은 서브넷으로 서로 닿는다(잰 값: 노드 하나 ping 1.0초
  안, 여러 노드 VXLAN 원본 0.04초 — 다리 노드의 veth를 지남). 다리 동안 T의 DHCP는 `router` = 서브넷 게이트웨이(원본 망의 물리
  라우터)를 주고, T를 라우터 인터페이스로 붙이는 것은 409 `network_migrating`이다(라우터 포트가 같은 게이트웨이 주소를 가지므로).
  `--no-bridge`면 놓지 않는다. 대상이 원본과 같은 L2의 공급자 네트워크면 다리가 필요 없다(놓으면 고리) — 공급자 대상의 옮기기는 만들 때
  `bridge: false`로 적혀 아래의 "대상 하나에 하나"에 걸리지 않는다. **한 대상 네트워크에 다리를 놓을 수
  있는 끝나지 않은 옮기기는 하나다**(Raft `migration-target` — 둘째는 409 `migration_overlap`; 한 스위치에 다리 둘이면 물리 L2와 고리가
  생기거나 두 VLAN이 하나로 이어진다). 다른 원본을 같은 T로 함께 옮기려면 `--no-bridge`로 하거나 앞의 옮기기를 끝낸 뒤 한다. 같은
  원본·같은 VM의 겹침도 409 `migration_overlap`이다.
- **DHCP 다툼.** 원본 망에 DHCP 서버가 있으면 그 응답도 다리를 건너 옮긴 VM에 온다. 보통 같은 chassis의 OVN DHCP가 먼저 답하지만, 다른
  주소를 받으면 포트 보안이 막는다 — 원본 DHCP 서버에 같은 주소로 예약하거나 옮기는 동안 끈다.
- **되돌리기.** `rollback [VM…]`이 옮긴(또는 실패한) NIC를 거꾸로 옮긴다 — 켜진 VM은 `live`(tap만 옮긴 VM이거나 MTU가 줄지 않으면,
  아니면 `restart`), 꺼진 VM은 `offline`. 원본 네트워크·보안 그룹·cloud-init 항목을 되살리고 IPAM 주소를 놓는다. 마지막 VM이 돌아가면 다리를
  걷고 `rolled-back`. `finished` 뒤에는 되돌리기가 없다(그 뒤는 VM 편집이나 새 옮기기).
- **끝내기.** `finish [--router R]`이 다리를 걷고(노드가 physnet을 걷기까지 120초 기다림), `--router`면 같은 Task가 T를 그 라우터
  인터페이스로 붙인다(물리 라우터에서 OVN 라우터로 기본 경로 넘김 — DHCP 게스트는 다음 갱신, 고정 주소 게스트는 게이트웨이가 같은 주소라
  바로). 원본에 NIC가 남은 VM이 있으면 경고만. 끝난 옮기기는 기록으로 남는다(`network migration list`, 지우기는 언제든 — 도는 Task가
  있으면 409 `migration_in_progress`).
- **다리 노드 바꾸기.** 다리 노드를 클러스터에서 지우면(`node remove`) 그 옮기기는 `paused`가 되고 다리 기록이 지워진다 — `network
  migration resume M --bridge-node n2`로 새 다리 노드에서 이어 간다. 다리 노드가 **오프라인**이면(지우지 않았어도) 같은 `resume
  --bridge-node`로 바꾼다 — 다리 기록을 지우고 새 노드에 놓으며, 그 노드가 돌아오면 에이전트가 기록에 없는 다리를 걷는다(끝내기도 오프라인
  다리 노드를 기다리지 않고 경고만). **온라인 노드에 다리가 놓인 동안 바꾸기는 409 `migration_bridge_laid`**다: 모두 되돌린 뒤 `network
  migrate … --bridge-node n2`로 다시 한다.
- **견디기.** Task는 VM마다 상태를 리소스에 적고 VM 스펙·전원·도는 NIC를 보고 다음 한 걸음을 하므로 orbivisord가 다시 떠도 이어 간다(시간 제한
  없음). 다리는 노드 하나를 지나는 길이다(넘김 없음 — 다리 노드가 죽으면 다리가 끊기고 경보 `sdn_physnet_not_ready`가 `mig…`로 뜬다).
  라이브 옮기기가 옮기기 전에 만든 핫플러그 Task를 기다렸는데 tap이 그대로면 새 Task로 한 번 더 하고, 그래도 안 되면 스펙을 원본으로
  되돌린다(스펙만 T에 남아 다음 켜기가 말없이 옮기지 않게).
- **잰 값(GitHub `ovn`, 노드 하나).** `start` 4.1초(다리 준비 + 꺼진 VM 스펙 + QEMU VM 라이브 tap 옮기기 — QEMU PID 그대로), QEMU VM
  되돌리기 1.1초, 끝내기 5.1초 뒤 `l2gateway` 행·매핑·veth·OVS 브리지가 모두 사라짐.
- **권한.** 원본 네트워크의 `network.config` + 대상의 `network.assign` + 옮길 VM마다 `vm.config`(막힌 VM은 "권한 없음"). 멈춤·지우기는
  네트워크 권한만, 끝내며 붙일 라우터는 그 라우터와 대상의 `network.config`. 리소스는 원본 네트워크의 폴더에 산다. 실행 중 VM의 NIC
  네트워크를 일반 `PUT`으로 바꾸면 409 `nic_network_live`("use network migrate"). 감사 `network migrate lan → t-lan (3 VMs, live 2, restart 1)`.
  옮기기를 읽는 사람(`GET`·목록·watch)에게는 읽을 수 없는 VM의 기록(MAC·주소·까닭)과 준 주소가 빠진다.

### OVN 접근 잠금

계획 65의 OVN은 "클러스터 CA mTLS"라 이 클러스터의 노드 인증서만 있으면 NB·SB·Raft에 붙어 무엇이든 쓸 수 있었다. 잠금(계획 67)은 셋을
막는다: **망가진 chassis가 남의 SB 행을 쓰는 것**(SB RBAC), **클러스터에서 지운 노드가 살아 있는 인증서로 다시 붙는 것**(주소 허용 목록),
NB·Raft 포트가 모든 곳에 열린 것.

```bash
orbivisorctl sdn lockdown on --dry-run      # 바꾸지 않고 허용 집합과 집합 밖에서 붙은 상대를 보임
# lockdown on: members 10.1.0.11, 10.1.0.12, 10.1.0.13, 10.1.0.14
# centrals: 10.1.0.11, 10.1.0.12, 10.1.0.13
# no connected peer is outside the allowed sets
orbivisorctl sdn lockdown on                # 중앙을 하나씩 바꾼 뒤 허용 목록을 건다
orbivisorctl sdn lockdown on --force        # 집합 밖 상대가 있어도
orbivisorctl sdn status | grep lockdown     # lockdown: on (3/3 centrals, firewall on 5/5 nodes)
orbivisorctl sdn lockdown off
```

- **SB 역할 포트와 특권 포트.** SB `ovsdb-server`가 설정 파일 `/etc/orbivisor/ovn/sb.json`(0600)로 뜬다: **6642**는 `role=ovn-controller`
  (chassis는 인증서 CN = 노드 UUID = chassis 이름인 **자기 `Chassis`·`Encap`·`Chassis_Private` 행만** 쓴다 — 남의 행·`SB_Global` 쓰기는
  `RBAC rules … prohibit …`으로 거절, GitHub에서 확인), **6645**는 northd만 쓰는 특권 포트(중앙 주소만 받음), `punix`는 중앙의 root.
  OvnSync·API의 SB 읽기(바인딩·서비스 모니터)는 6642로 한다. 지운 노드의 `Chassis` 청소는 이제 SB 리더인 중앙의 에이전트가 로컬
  소켓으로 한다. NB에는 RBAC가 없다(OVN).
- **주소 허용 목록**(nftables 표 `inet opl_ovn_<태그>`, 입력 훅, 우리 포트만 거름):

| 노드 | 포트 | 받는 출발 주소 |
|---|---|---|
| 중앙 | TCP 6641(NB)·6642(SB 역할) | **멤버 집합**: Raft 노드 가운데 위트니스·macOS가 아닌 노드의 클러스터 주소(IP일 때)와 underlay 주소 |
| 중앙 | TCP 6643·6644(Raft)·6645(SB 특권) | **중앙 집합**: 중앙 주소와 그 노드들의 underlay |
| chassis | UDP 6081(Geneve) | 멤버 집합과 알린 Geneve 주소 |

  그 밖의 출발 주소는 버린다(맺은 연결도). 루프백은 `127.0.0.1`·`::1`만 받는다. SSH·Orbivisor API·gRPC는 건드리지 않고, 집합을 읽지 못하면
  마지막 표를 둔다(비우지 않음). 노드를 지우면 1초 안에 모든 에이전트가 그 주소를 빼고 — **잰 값: `cluster remove-node` 뒤 1.1초에 지운 노드의
  인증서가 남은 중앙의 6641·6642에 붙지 못했다** — 그 노드의 `Chassis`도 지워져 다른 chassis의 터널이 사라진다. 인증서는 여전히 CA로 맞지만
  쓸 길이 없다.
- **켜고 끄기.** `sdn lockdown on|off`(`POST /api/v1/sdn/lockdown {enabled, force?}`, 루트 `cluster.admin`)가 `sdn-lockdown` Task를 만든다.
  중앙마다 에이전트가 **자기 차례**(다른 중앙이 모두 온라인이고 리더·팔로어)에 설정을 쓰고 SB를 다시 띄워 따라잡은 뒤 적용을 알린다 —
  한 번에 하나라 OVN 정족수가 남고, northd는 잠긴 중앙엔 6645·아직 아닌 중앙엔 6642로 붙는다. 모든 중앙이 끝난 **뒤에** 허용 목록을 건다.
  끄기는 거꾸로(표를 먼저 걷고 중앙을 하나씩). 잰 값: 노드 하나 켜기 3.1초·끄기 5.1초, 중앙 셋 켜기 10.5초(그동안 VM ping 61/61)·끄기
  6.2초(40/40) — **잃은 패킷 0**. 중앙 Task(더하기·빼기)와 동시에 돌지 않는다(409 `sdn_busy`).
  허용 목록도 **중앙은 하나씩** 건다(목록에서 앞선 중앙이 표를 걸었다고 알린 뒤). **잠금 켜기 Task가 도는 동안 처음 거는** 중앙은 걸기
  전의 NB·SB `cluster/status`를 적어 두고, 건 뒤 선거 시간 두 배에 1초를 더한 만큼 기다려 견준다 — 여전히 멤버이고 리더를 알며, 리더면
  **걸기 전에 듣던** 다른 중앙마다·팔로어면 리더에게서 **건 뒤에** 소식을 받았어야 한다(다섯 번까지). 걸기 전부터 꺼졌던 중앙이나 중앙이
  아닌 Raft 서버(내쫓지 못한 것)는 보지 않는다. 끊겼으면 **표를 곧바로 걷고** 오류(`the address allow-list cut the OVN Raft connections …`)를
  알리며, `sdn-lockdown` Task는 그 중앙과 까닭으로 실패하고 **잠금을 다시 끈다**(`SdnSpec.lockdown = false` — 에이전트가 SB를 하나씩
  되돌리고 걸었던 표를 걷는다, 걷기가 실패하면 차례마다 다시). Task가 끝나거나 orbivisord를 다시 띄워도 그 표를 다시 넣지 않는다. orbivisord를 다시
  띄운 뒤의 다시 걸기나 이미 받아들인 표는 Raft를 보지 않으므로, 중앙 하나가 꺼져 있다고 맞게 걸린 표를 걷지 않는다. **중앙이 둘이면** 차례마다
  SB를 다시 띄우는 동안 남은 하나로는 과반이 아니어서 SB가 몇 초 멈춘다(데이터 길은 그대로) — 미리 확인이 경고하고, 셋째 중앙을 더하면
  없다.
- **미리 확인과 `--force`.** `--dry-run`(`?dry_run=true`)은 허용 집합과, 지금 중앙의 6641·6642에 붙은 상대 주소(중앙 에이전트가 `ss`로 읽어
  알림) 가운데 집합 밖인 것을 보인다 — NAT·다른 출발 주소로 붙은 노드를 미리 찾는다. **Raft 6643·6644에 붙은 상대도 중앙 집합과 견준다**
  (여러 망에 걸친 중앙이 중앙 주소가 아닌 출발 주소로 Raft를 보내면 표가 그것을 끊어 NB·SB가 정족수를 잃는다 — 그런 상대는 `(Raft)`로
  나온다). 루프백은 표와 같게 `127.0.0.1`·`::1`만 뺀다(`127.0.0.2` 따위는 밖). 집합 밖 상대가 **하나라도** 있으면 켜기를 막는다(409
  `lockdown_peers_outside`, `--force`로만 — 주소만으로는 NAT 뒤 멤버인지 지운 노드인지 가릴 수 없다). 막지 않는 경고(`warnings` — 중앙
  둘)도 함께 나온다.
- **새 클러스터는 켜진 채로.** `sdn init`은 모든 노드가 `sdn-services`를 알면 잠금을 켠 채로 세운다(첫 중앙부터 잠근 모양). 계획 65·66으로
  세운 클러스터는 관리자가 켤 때까지 그대로다(업그레이드가 OVN 단위를 바꾸지 않는다). 잠금이 꺼져 있으면 OVN 단위는 계획 66과 바이트까지
  같다. 중앙 더하기는 잠금 상태를 따르고, 더할 노드는 합류 전에 중앙 집합에 든다.
- **보기.** `GET /api/v1/sdn`의 `lockdown { enabled, applied, pending, error, allowed, nodes[{node, firewall, error}] }`(켠 적이 있을 때만),
  `sdn status`의 잠금 줄, 웹 SDN 보기의 잠금 카드. 켜졌는데 표가 맞지 않는 노드는 경보 `sdn_sync_failed`의 까닭으로 보인다. 웹 잠금 카드의
  "먼저 확인"은 집합 밖 상대마다 그 중앙을 보이고, Raft 포트(6643·6644)에서 본 상대에는 "Raft" 꼬리표를 단다. 서버 경고(예: 중앙이 둘이면
  차례마다 정족수를 잃음)도 보인다. 켜기 Task가 중간에 실패하면(켜짐인데 아직 바꾸는 중이거나 오류, 도는 Task 없음) 칩이 "중간에 멈춤"이
  되고 "마저 켜기"가 나온다. 끄기가 중간에 멈추면 같은 칩에 "마저 끄기"(`POST {enabled: false}`)가 나온다. 서버의 잠금 경고·오류
  문장은 번역해 보인다. 같은 `POST {enabled: true}`가 남은 일을 마저 하고, 새 미리 확인에서 집합 밖 상대가 있으면 "그래도 켬"을 골라야
  한다.
- **남는 것.** OVN 24.03 RBAC는 `Port_Binding`·`MAC_Binding`·`FDB` 쓰기에 id를 보지 않는다 — 망가진 chassis가 남의 포트를 자기에게
  바인딩할 수 있다(OVN이 막지 않음, `network ips`의 NODE가 VM 노드와 다르게 보인다). NB는 여전히 모든 멤버 노드가 쓴다. 인증서 폐기·CA
  바꾸기, Geneve 암호화는 없다.

### SDN 경보

[알림](cluster.md#알림)의 경보 규칙에 SDN 조건 여덟이 있다(계획 67). 모두 **리더가 평가**하고(`for_secs`·`repeat_secs`·`severity`·대상은 다른
규칙과 같다), 입력은 Raft의 노드·SDN 상태와 리더 OvnSync가 10초마다 읽는 관찰(게이트웨이 주인, LB 멤버 건강)이다. 관찰은 맞추기 루프와
따로 도는 Task가 이어 쓰는 연결과 2초 시간 제한으로 읽으므로, 꺼진 중앙이 NB 쓰기(새 포트·라이브 마이그레이션 relink)를 늦추지 않는다.
읽을 때마다 그 중앙이 아직 클러스터에 붙어 있는지(`_Server.Database.connected`) 보고, 떨어졌으면 연결을 버리고 다른 중앙에 붙는다 —
갈라진 중앙의 얼어붙은 사본을 새 관찰로 적지 않는다(붙을 중앙이 없으면 관찰이 낡아 경보 엔진이 모르는 것으로 본다).
SDN을 세우기 전에는 모두 거짓이다.

| 조건 | 대상 | 참일 때 |
|---|---|---|
| `sdn_quorum_lost` | SDN | NB 또는 SB에 리더를 알리는 중앙이 없거나, 리더·팔로어인 중앙이 과반 미만(오프라인 노드는 꺼짐으로 셈) |
| `sdn_central_down` | 노드 | 중앙의 NB·SB 가운데 하나가 꺼짐·후보이거나 노드가 오프라인 |
| `sdn_chassis_down` | 노드 | 온라인 chassis 노드의 ovn-controller가 꺼짐·구성 중 |
| `sdn_gateway_unavailable` | 라우터 | 바깥이 있는데 게이트웨이 후보가 없음, 또는 모든 후보가 오프라인 |
| `sdn_gateway_failover` | 라우터 | (이벤트) 게이트웨이 주인 노드가 바뀜 — 문구에 옛·새 노드. 주인이 없던 관찰을 지나 바뀌어도 잡고(같은 임기에서 마지막으로 안 주인과 견줌), 같은 라우터의 같은 넘김(A→B)은 규칙의 `repeat_secs` 안에 한 번만 보낸다(오가는 BFD) |
| `sdn_sync_failed` | SDN | OvnSync 오류, 또는 300초 넘게 동기화 없음, 또는 잠금 표가 맞지 않는 노드 — 문구에 실패한 묶음 |
| `sdn_physnet_not_ready` | 노드 | 설정한 physnet(옮기기 다리 `mig…` 포함)이 준비 안 됨 — 문구에 까닭 |
| `sdn_lb_member_down` | LB | 건강 검사 멤버 가운데 `offline`·`error`가 있음(LB마다 하나 — 문구에 멤버 목록) |

```bash
orbivisorctl alert rule add-sdn-defaults --target ops-mail       # 권하는 여덟 규칙을 한 번에(같은 이름은 그대로 둠)
orbivisorctl alert rule create gw --condition sdn_gateway_unavailable --for 30s --severity critical --target phone
```

- **권하는 묶음**(`add-sdn-defaults`, `POST /api/v1/alert-rules/sdn-defaults {targets}` → `{created, existing}`, 웹 SDN 보기의 "SDN 경보
  켜기"): `sdn-quorum-lost` critical 0초, `sdn-central-down`·`sdn-chassis-down` warning 60초, `sdn-gateway-unavailable` critical 30초,
  `sdn-gateway-failover` info(이벤트), `sdn-sync-failed` warning 120초, `sdn-physnet-not-ready` warning 60초, `sdn-lb-member-down` warning
  30초, 반복 4시간, 해제 알림. 만든 뒤는 보통 규칙이고 다시 불러도 안전하다(같은 이름이면 건드리지 않음 — 웹·CLI가 같은 서버 묶음을 부른다).
- **노드 거르기**(`--node`)는 대상이 노드인 조건(`central_down`·`chassis_down`·`physnet_not_ready`)에만 쓴다(나머지는 400).
- **리더가 바뀌면** 관찰 기반 조건(게이트웨이 넘김·LB 멤버)은 새 리더의 첫 관찰까지 평가하지 않고(거짓으로 풀지 않음), 넘김은 같은 임기의
  관찰끼리만 견준다. 대상이 사라진 firing 경보(라우터·노드를 지움, 중앙이 아니게 됨)는 해제로 알린다. 오프라인 노드의 `chassis_down`·
  `physnet_not_ready`는 모르는 것으로 둔다(`node-offline`이 알림).
- **잰 값(여러 노드 시험, 지속 시간 5초로 줄임).** LB 멤버 듣기 멈춤 → `sdn_lb_member_down` 11.5초, 게이트웨이 후보를 모두 빼면
  `sdn_gateway_unavailable` 9.5초(되돌리면 해제 62.6초 — 해제 대기 60초), 게이트웨이 노드의 ovn-controller 멈춤 → 넘김 확인 때
  `sdn_gateway_failover`("moved from c2 to c3")와 `sdn_chassis_down`이 이미 와 있음, 중앙 컨테이너 죽이기 → `sdn_central_down` 17.0초.
  `sdn_quorum_lost`는 순수 시험 표로만 보았다(컨테이너 셋에서 중앙 둘을 죽이면 Orbivisor Raft도 정족수를 잃어 평가할 리더가 없다).

### 폴더 권한과 할당량

라우터·유동 IP·부하 분산기·옮기기는 **네트워크 폴더**에 산다(네트워크 폴더가 테넌트, [폴더와 권한](folders.md)).

| 일 | 필요한 것 |
|---|---|
| 공급자 네트워크 만들기·바꾸기·지우기, 노드 `sdn`(physnet·게이트웨이) | 루트 `cluster.admin` |
| 라우터 만들기·바꾸기·지우기 | 라우터(만들기는 넣을 폴더)의 `network.config` |
| 라우터 인터페이스 더하기 | + 그 네트워크의 `network.config` — 그 네트워크의 모든 VM 기본 경로(DHCP `router`)가 라우터로 가므로 NIC 붙이기(`network.assign`)보다 크다 |
| 라우터 경로·바깥 바꾸기 | + 모든 인터페이스 네트워크의 `network.config` |
| 내 네트워크를 라우터에서 떼기 | 그 네트워크의 `network.config`(라우터 권한 없이 — `router detach`) |
| 라우터 바깥 정하기 | + 바깥 네트워크의 `network.assign` |
| 유동 IP 만들기·지우기 | 유동 IP 폴더의 `network.config` + 바깥 네트워크의 `network.assign`(주소를 직접 주면 유동 IP 범위 안 — 밖은 루트 `cluster.admin`) |
| 유동 IP 붙이기·떼기 | 유동 IP의 `network.config` + 대상 VM의 `vm.config`. 떼기는 유동 IP의 `network.config` **또는** 지금 붙은 VM의 `vm.config`(계획 67 — 유동 IP를 읽을 수 없어도 VM 주인이 뗄 수 있다) |
| 부하 분산기 만들기·바꾸기·지우기 | LB(만들기는 폴더)의 `network.config` + VIP 네트워크의 `network.assign`(지우기는 LB의 `network.config`만) |
| LB 멤버 | NIC 멤버는 + 그 VM의 `vm.config`(남의 VM으로 트래픽을 보내지 않게), 주소 멤버는 + 그 서브넷 네트워크의 `network.assign` |
| LB 바깥 VIP | + 공급자 네트워크의 `network.assign`(유동 IP 범위 밖 주소는 루트 `cluster.admin`) |
| NIC `qos` | VM의 `vm.config` |
| 옮기기 만들기·시작·끝내기·되돌리기 | 원본 네트워크의 `network.config` + 대상의 `network.assign` + 옮길 VM마다 `vm.config`(멈춤·지우기는 네트워크 권한만) |
| OVN 잠금 켜고 끄기, SDN 경보 규칙 | 루트 `cluster.admin` |
| 보기 | `system.read`(라우터·유동 IP·네트워크·LB·옮기기·토폴로지). 다른 폴더의 VM은 이름을 감추고, 루트 `cluster.read`가 없으면 노드 이름도 감춘다. 오류 문구도 읽을 수 없는 라우터·유동 IP는 이름 없이 센다. `GET /routers`의 `routed`는 읽을 수 있는 네트워크 가운데 라우터 인터페이스인 것과 그 바깥(라우터 이름 없이)이다 — 웹이 읽을 수 없는 라우터 뒤의 NIC에도 유동 IP를 붙이게 한다 |
| 보안 그룹 | 지금 그대로(만들고 바꾸기는 admin) |

- 라우터·유동 IP·부하 분산기·옮기기에도 객체 grant를 준다(`/api/v1/grants`의 대상 `router/<id>`·`floating-ip/<id>`·`load-balancer/<id>`·
  `network-migration/<id>`, 웹의 행마다 권한 서랍).
- **할당량.** 네트워크 폴더에 `routers`·`floating_ips`와 계획 67의 `networks`·`load_balancers`(하위 트리의 수)를 둔다 — 공인 주소가
  테넌트에게 가장 귀한 것이라서다. **LB의 바깥 VIP는 `floating_ips`도 하나 쓴다**(공인 주소는 어떤 리소스가 잡든 같은 것 — LB에 바깥
  VIP를 붙이는 바꾸기도 센다). 옮기기는 세지 않는다. `ips` 차원은 두지 않는다: 오버레이 주소는 테넌트 자기 망의 것이고 공유 공급자 망의
  주소는 `floating_ips`와 관리자가 준 `network.assign`으로 이미 묶인다.
  VM 폴더에 주면 400("quota routers cannot be set on a vm folder"), 넘으면 409 `quota_exceeded`(Raft 상태 머신이 커밋 순서대로
  센다). `orbivisorctl folder quota set network:/tenant --routers 2 --floating-ips 8 --networks 10 --load-balancers 4`(`folder usage
  network:/tenant`), API `PATCH /api/v1/folders/{id}`의 `quota: {"routers": 2, "floating_ips": 8, "networks": 10, "load_balancers": 4}`, 웹
  네트워크 폴더 상세의 할당량 탭. `GET /api/v1/folders/{id}/usage`가 넷을 센다(0이면 칸이 없음). 계획 67 차원은 기능 `sdn-services` 뒤다.
- **옮기기.** `orbivisorctl folder members move network:/tenant router/r1 fip/web lb/web-lb`, 웹 네트워크 폴더의 내용 탭(라우터·유동 IP·LB도
  종류 칸과 함께 나오고 끌어 놓기·"폴더로 옮기기"), API `POST /api/v1/folders/{id}/members {"items": [{"kind": "router", "key":
  "r1"}, {"kind": "floating-ip", "key": "web"}]}`. 객체 grant도 CLI가 받는다(`orbivisorctl grant add --user bob --role operator --on router/r1`).

### MTU: 바깥에서 들어오는 큰 패킷

오버레이 네트워크의 MTU는 1442, 공급자 네트워크는 1500이다. 안 → 밖은 게스트가 1442를 알고 있어 문제가 없다. **밖 → 안의
1500바이트 DF(조각내지 말 것) 패킷은 ICMP "Frag Needed" 없이 조용히 버려진다** — OVN 24.03의 `gateway_mtu`는 게이트웨이
포트로 **나가는** 패킷만 재기 때문이다(GitHub `ovn` 작업의 탐침: 바깥 이름공간에서 유동 IP로 보낸 DF 1500 ping이 답도 ICMP도
없이 사라짐). 그래서 경로 MTU 찾기가 그 방향에서 되지 않는다.

- **TCP는 대개 괜찮다.** 안쪽 VM이 연결을 맺을 때 MSS 1402를 알리므로 밖의 상대가 그보다 큰 조각을 보내지 않는다.
- **막히는 것.** 큰 UDP(DNS over UDP의 큰 답, 일부 VPN·스트리밍), DF를 켠 큰 ICMP, MSS를 무시하는 장비 뒤의 TCP.
- **고치는 길**(하나를 고른다):
  1. **물리 망(underlay) MTU를 오버레이 + 58 이상으로**(예: 1558 — 점보 프레임이면 underlay 9000, 오버레이 네트워크 `--mtu 8942`)
     올려, 바깥 1500 패킷이 안쪽에 그대로 들어가게 한다. 가장 깨끗하다.
  2. **공급자 네트워크 MTU를 오버레이에 맞춰 줄인다**(`network create ext … --mtu 1442`, 물리 라우터의 그 인터페이스 MTU도 함께).
  3. 물리 라우터·방화벽에서 TCP MSS를 1402로 고정(MSS clamping)하고 UDP 큰 패킷은 받아들인다.

### Neutron·Proxmox SDN·NSX와 견주기

| | OpenStack Neutron ML2/OVN | Proxmox VE 8 SDN | VMware NSX 4 | Orbivisor 계획 66 |
|---|---|---|---|---|
| 라우터 | OVN 분산 라우터(에이전트 없음) | 라우터 객체 없음. simple 영역은 노드 브리지 게이트웨이, EVPN은 VRF·출구 노드(FRR) | Tier-1/Tier-0: 호스트의 분산 라우터 + Edge의 서비스 라우터 | OVN 분산 라우터 + 분산 게이트웨이 포트 |
| 외부 연결 | `localnet` + `ovn-bridge-mappings`(NIC를 OVS 브리지로) | 노드 브리지(관리자 구성) | Edge 업링크, VLAN 세그먼트 | `localnet` + 노드 physnet(관리자 Linux 브리지에 veth, NIC는 그대로) |
| 게이트웨이 HA | `HA_Chassis_Group` 우선순위 + BFD, "가장 덜 쓴 chassis" 스케줄러 | EVPN 출구 노드 여럿(BGP 경로) | Edge 클러스터 액티브·스탠바이(BFD) | `HA_Chassis_Group` 라우터마다, 관리자 우선순위 + HRW로 흩음(상태 없음) |
| SNAT | 게이트웨이 chassis | 영역의 SNAT 선택(노드 iptables) | SR의 NAT | 게이트웨이 노드(OVN `snat`) |
| 유동 IP | `dnat_and_snat`, 전역 스위치로 분산(외부 망 없는 계산 노드면 끊김) | 없음 | NAT 규칙(Edge, 중앙) | 리소스, VM 노드에 physnet이 있으면 **저절로** 분산, 아니면 중앙 |
| IPv6 | RA·DHCPv6, NAT 없음 | DHCP는 IPv4 위주 | SLAAC·DHCPv6 서버 | RA·DHCPv6(SLAAC·상태 있음·없음), EUI-64 IPAM, NAT 없음 |
| 방화벽 뜻 | 허용만·순서 없음(그룹의 합집합) | pve-firewall(SDN과 따로, 순서 있는 규칙) | 분산 방화벽: 범주별 순서대로 처음 맞는 규칙 | 보안 그룹 그대로(순서대로 처음 맞는 규칙, drop 규칙) — 단순 NIC와 같은 리소스·뜻 |
| 테넌트·할당량 | 프로젝트, 라우터·유동 IP 할당량 | 없음 | 프로젝트·VPC | 네트워크 폴더, 할당량 `routers`·`floating_ips` |
| 설치·운영 | 배포 도구가 따로 | 내장(SDN은 FRR 따로) | NSX Manager·Edge VM | orbivisord가 OVN 설치, physnet·게이트웨이는 노드 설정 한 줄 |

### 부하 분산·QoS·옮기기: Octavia·Proxmox·NSX와 견주기

| | OpenStack Octavia(Amphora / OVN 공급자) | Proxmox VE 8 | VMware NSX 4(NSX ALB·NSX LB) | Orbivisor 계획 67 |
|---|---|---|---|---|
| LB 모양 | Amphora: 테넌트마다 HAProxy 서비스 VM(액티브·스탠바이), L7·TLS. OVN 공급자: NB `Load_Balancer`, L4만 | 없음(사용자가 HAProxy VM) | Edge의 서비스 라우터(L4·L7), NSX ALB는 서비스 엔진 VM | OVN 내장 `Load_Balancer`(OVN 공급자와 같은 자리), L4 TCP·UDP |
| 데이터 길 | Amphora는 VM을 지남. OVN 공급자는 클라이언트 chassis에서 분산 DNAT | — | Edge 노드에 모임 | 클라이언트 chassis에서 분산 DNAT, 바깥 VIP만 게이트웨이 chassis |
| 건강 검사 | Amphora: HTTP·TCP 따위. OVN 공급자: TCP·UDP(서비스 모니터, 네트워크마다 `ovn-lb-hm` 포트) | — | 능동·수동 모니터, L7 | TCP·UDP 서비스 모니터, LB·네트워크마다 출발 주소, 멤버 SG 경고 |
| 넘김 | Amphora 짝 VRRP, OVN 공급자는 모니터 + 게이트웨이 HA | — | Edge 액티브·스탠바이 | 죽은 멤버 빼기 3.6~4.4초(간격 2초·fall 2 측정), 바깥 VIP는 게이트웨이 HA(GARP로 1.3초) |
| 바깥 주소 | 유동 IP를 VIP 포트에 붙임 | — | VIP를 Tier-0/1에 광고 | LB의 `external` 칸(유동 IP 범위·`floating_ips` 할당량), SNAT 없음 |
| 운영 부담 | Amphora 이미지·업그레이드·자원 셈 | — | Edge·서비스 엔진 VM | NB 행뿐(VM·에이전트 없음) |
| QoS | Neutron QoS 정책(대역폭·DSCP·최소 대역폭) | NIC `rate`(tc, 속도만) | 세그먼트 프로필(셰이핑·DSCP) | NIC 칸: 속도·버스트·DSCP(SDN), 단순 NIC도 tc 속도 — 같은 폴리싱 뜻 |
| 네트워크 옮기기 | 없음(사용자가 포트를 바꿈) | 실행 중 브리지 바꾸기(NIC마다 손으로) | Migration Coordinator(단계별·되돌리기, VLAN → 세그먼트 다리) | 미리 보기 계획, 주소 가져가기, VM마다 offline·live·restart, `l2gateway` 다리, 되돌리기, 끝내며 라우터 붙이기 |
| 제어 평면 접근 | SB RBAC(`ovn-controller` 역할) | — | 관리자·전송 노드 인증서 | SB RBAC + northd 특권 포트 + Raft 멤버십을 따르는 주소 허용 목록(지운 노드 1.1초에 끊김) |

**고르지 않은 것.** L7(HTTP 규칙·TLS 종료·쿠키 고정)·SCTP·IPv6 VIP는 넣지 않았다 — 서비스 VM이 필요하거나(L7) OVN 서비스 모니터가 IPv4만이라서다.
L7이 필요하면 LB 뒤에 HAProxy VM을 둔다. 최소 대역폭 보장은 스케줄러의 대역폭 셈이 필요해 나중이다.

### OvnSync: Raft에서 OVN으로

API는 Raft만 쓰고 OVN을 부르지 않는다. **리더의 `OvnSync`**가 Raft의 SDN 네트워크·서브넷·VM NIC·라우터·유동 IP·보안
그룹·부하 분산기·옮기기 다리를 NB의 행(`Logical_Switch`·`Logical_Switch_Port`·`DHCP_Options`, 계획 66부터 `Logical_Router`·라우터
포트·정적 경로·`NAT`·`HA_Chassis_Group`·`Port_Group`·`ACL`, 계획 67부터 `Load_Balancer`·`Load_Balancer_Health_Check`·`QoS`·`l2gateway`
포트)으로 그려 지금 NB와 다른 것만 쓴다. 트랜잭션은 그룹 목록 Port_Group마다 → LB마다 → 네트워크마다(QoS 행·다리·LB 붙이기 포함) →
라우터마다 → 지울 것 차례이고, 하나가 실패해도 다른 것은 쓰인다(`sync`의 오류가 그 이름을 댄다 — 실패한 묶음은 경보
`sdn_sync_failed`의 문구에도). Network·Vm·Sdn·Router·FloatingIp·SecurityGroup·LoadBalancer·NetworkMigration이 바뀌거나 노드의 게이트웨이 우선순위·유지보수·준비된 physnet이 바뀌면 0.5초
모아 한 번, 리더가 된 직후, 그리고 60초마다(누가 NB를 고쳐도 되살린다) 돈다. NB는 다시 만들 수 있는
파생물이다 — 지워도 다음 차례에 되살아난다. 리더 OvnSync는 차례 뒤와 10초마다 게이트웨이 주인·LB 멤버 건강을 읽어 경보 엔진에 넘긴다
(Raft에 쓰지 않음). LB·QoS·다리를 쓰지 않으면 NB 행은 계획 66과 같다.

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

계획 66의 칸(공급자 네트워크, 서브넷 `floating`·`ipv6-mode`, 노드 `spec.sdn`과 `status.sdn.physnets`·`gateway`, 라우터, 유동
IP, 네트워크 폴더 할당량 `routers`·`floating_ips`, SDN NIC의 보안 그룹)은 기능 **`sdn-routing`** 뒤에 있다 — 모든 노드가 알리기
전에는 409 `cluster_upgrading`이다. 라우터·공급자 네트워크·`ipv6-mode`·SDN NIC 그룹을 쓰지 않으면 OvnSync가 계획 65와 같은
NB 행을 쓰고(업그레이드가 NB를 건드리지 않는다) 단순 NIC의 nftables 규칙 글도 바이트 그대로다.

계획 67의 칸(부하 분산기, 옮기기, NIC `qos`, OVN 잠금, SDN 경보 조건, 네트워크 폴더 할당량 `networks`·`load_balancers`)은 기능
**`sdn-services`** 뒤에 있다 — 모든 노드가 알리기 전에는 409 `cluster_upgrading`이고, 노드는 `status.qos`·잠금 상태를 쓰지 않는다.
LB·QoS·옮기기를 쓰지 않으면 NB 행, QoS 없는 VM의 tap 명령, nftables 글, cloud-init 씨앗이 바이트 그대로이고, 잠금을 켜지 않은 클러스터의
OVN 단위도 그대로다.

### 보안

- **OVN 연결은 모두 클러스터 CA mTLS다.** NB·SB·Raft 포트는 `pssl`만 열고(평문 `ptcp` 없음) 노드 인증서
  (`/etc/orbivisor/ovn/{ca.crt,node.crt,node.key}`, `{data_dir}/pki`의 사본, 0600 root, 디렉터리 0700)로 서로 확인한다. 이 클러스터의
  노드 인증서가 없으면 NB·SB에 붙을 수 없다(다른 CA의 인증서·인증서 없음·평문은 거절 — `ovn` CI 작업이 본다). 노드 인증서가
  다시 서명되면 에이전트가 사본을 바꾼다.
- OVS·OVN 서버는 상대 인증서의 CA만 보고 이름은 보지 않는다. Orbivisor의 OVSDB 클라이언트(OvnSync)는 서버 인증서의 이름도
  중앙 주소·노드 주소와 견준다.
- 로컬 `punix` 소켓(`/run/orbivisor/ovn/{nb,sb}.sock`)은 root만 쓴다.
- Geneve(UDP 6081)는 VXLAN처럼 암호화하지 않는다. underlay는 믿을 수 있는 망이어야 한다.
- **잠금을 켜면**(새 클러스터의 기본) chassis는 SB에서 자기 행만 쓰고, northd만 특권 포트를 쓰며, 지운 노드는 인증서가 살아 있어도 붙지
  못한다([OVN 접근 잠금](#ovn-접근-잠금)). 계획 65·66으로 세운 클러스터는 `sdn lockdown on`을 할 때까지 chassis가 SB의 다른 chassis 행도
  쓸 수 있다.

### 문제 풀기

```bash
# NB 보기(중앙 노드에서, root). 팔로어의 소켓에는 ovn-nbctl의 기본(--leader-only)이 붙지 못한다 — 리더 노드에서 하거나:
ovn-nbctl --db=unix:/run/orbivisor/ovn/nb.sock --no-leader-only show
ovn-sbctl --db=unix:/run/orbivisor/ovn/sb.sock --no-leader-only show          # chassis와 포트 바인딩
ovs-appctl -t /run/orbivisor/ovn/nb.ctl cluster/status OVN_Northbound          # NB Raft(SB는 sb.ctl·OVN_Southbound)
ovn-appctl -t ovn-controller connection-status                          # chassis가 SB에 붙었나
ovs-vsctl list Open_vSwitch . | grep external_ids                       # system-id·ovn-remote·encap
ovs-vsctl --columns=name,external_ids list Interface             # VM tap의 iface-id(논리 포트)·ovn-installed
journalctl -u orbivisor-ovn-nb -u orbivisor-ovn-sb -u orbivisor-ovn-northd; less /var/log/orbivisor/ovn-nb.log
# 라우터·NAT·게이트웨이(계획 66). 이름은 opl-r-<라우터 id>, 바깥 포트 opl-gw-<라우터 id>, HA 그룹 opl-ha-<라우터 id>
ovn-nbctl --db=unix:/run/orbivisor/ovn/nb.sock --no-leader-only lr-list
ovn-nbctl --db=unix:/run/orbivisor/ovn/nb.sock --no-leader-only lr-nat-list opl-r-<id>
ovn-nbctl --db=unix:/run/orbivisor/ovn/nb.sock --no-leader-only ha-chassis-group-list
ovn-nbctl --db=unix:/run/orbivisor/ovn/nb.sock --no-leader-only --bare get Logical_Router_Port opl-gw-<id> status:hosting-chassis
ovn-nbctl --db=unix:/run/orbivisor/ovn/nb.sock --no-leader-only acl-list opl_<태그>_<h16>    # 그룹 목록 Port_Group의 ACL
ovs-vsctl get Open_vSwitch . external_ids:ovn-bridge-mappings                # 노드의 physnet 매핑
ip -br link show master vmbr1                                                # 관리자 브리지에 매단 veth oplp…b
# 부하 분산·QoS(계획 67). LB 행은 opl-lb-<LB id>-<tcp|udp>
ovn-nbctl --db=unix:/run/orbivisor/ovn/nb.sock --no-leader-only lb-list
ovn-sbctl --db=unix:/run/orbivisor/ovn/sb.sock --no-leader-only list Service_Monitor    # 멤버 건강(online·offline)
ovn-nbctl --db=unix:/run/orbivisor/ovn/nb.sock --no-leader-only qos-list opl-<네트워크 id>
tc filter show dev <tap> ingress; tc filter show dev <tap> egress            # 단순 NIC의 police(pref 49152)
# 잠금(계획 67)
ovs-appctl -t /run/orbivisor/ovn/sb.ctl ovsdb-server/list-remotes                  # 잠겼으면 pssl:6642·pssl:6645·punix
nft list table inet opl_ovn_<태그>                                            # 허용 집합 members·centrals·geneve
# 옮기기 다리(계획 67). 포트 opl-l2gw-<옮기기 id>, 다리 노드의 매핑 mig<h8>
ovn-nbctl --db=unix:/run/orbivisor/ovn/nb.sock --no-leader-only lsp-list opl-<대상 네트워크 id> | grep l2gw
```

- `sdn status`의 CHASSIS가 `down`이면 ovn-controller가 SB에 붙지 않은 것이다(중앙에 닿는지, TCP 6642). `configuring`은 붙었으나
  SB에 chassis 행이 아직 없는 것이다.
- VM이 주소를 받지 못하면 `network ips`의 UP을 본다. `no`면 포트가 바인딩되지 않았다(chassis 상태, orbivisord 로그의 "port
  not bound" 경고). 게스트가 서브넷의 DHCP를 켰는지, 게스트 안 MTU가 1442 이하인지 본다.
- `sdn status`의 동기화가 멈춰 있고 리더 orbivisord 로그에 "fence token from a newer Raft term" 오류가 차례마다 나오면 NB에
  지금 Raft보다 큰 임기의 토큰이 남은 것이다(Orbivisor 클러스터를 새로 세우고 OVN DB는 둔 경우). NB 리더에서
  `ovn-nbctl remove NB_Global . external_ids orbivisor:<태그>:fence`로 지운다.
- 배포판 단위를 되살리려면 `/etc/orbivisor/ovn/distro-units`의 단위를 `systemctl unmask`·`enable`한다(Orbivisor 단위를 먼저 멈춘다).
- **physnet이 준비되지 않음.** `node sdn NODE`의 오류를 본다: 브리지 이름이 틀렸거나 없음(`ip link show vmbr1`), 같은 physnet이
  남의 OVS 브리지에 매핑됨(`ovn-bridge-mappings`), 노드가 chassis가 아님(`sdn chassis install`).
- **라우터가 밖에 닿지 않음.** `router get`의 경고("no gateway node has physnet … ready" — 게이트웨이 후보가 없음)와 ACTIVE 노드를
  본다. 물리 라우터에 돌아오는 경로가 있는지(SNAT를 끈 IPv4, 모든 IPv6), 게스트가 기본 경로를 받았는지(DHCP 갱신 전이면 다시
  받기) 본다.
- **유동 IP가 `unrouted`.** 까닭(`fip get`)을 본다: NIC의 네트워크가 그 바깥의 라우터에 없음, NIC에 IPv4 없음, 게이트웨이 후보 없음.
- **보안 그룹을 바꾸자 SDN VM의 맺은 연결이 끊김.** 허용을 뺀 규칙에 걸린 연결은 OVN에서 끊기는 것이 뜻이다([SDN NIC의 보안
  그룹](#sdn-nic의-보안-그룹)).
- **밖에서 들어오는 큰 UDP·DF 패킷이 사라짐.** [MTU](#mtu-바깥에서-들어오는-큰-패킷).
- **LB 멤버가 `offline`인데 서비스는 산다.** 멤버 NIC의 보안 그룹이 검사 주소(`network ips`의 `lb-monitor` 행)에서 오는 것을 막는지
  본다(`lb get`의 WARNING). UDP 검사는 ICMP 포트 닿지 않음으로만 죽음을 안다.
- **VIP에 닿지 않음.** `lb get`의 `vip_state`·`external_state` 까닭(라우터 인터페이스가 아님, 게이트웨이 후보 없음)과 `lb-list`의 행을 본다.
  멤버가 하나도 없거나 모두 죽었으면 연결이 곧바로 거절된다.
- **QoS 측정이 제한보다 크다.** 처음 몇 초는 버스트다 — 앞을 빼고 길게 잰다. 단순 NIC 노드를 다시 띄운 뒤 tc가 어긋나 보이면 `vm hotplug`가
  맞춘다.
- **옮긴 VM이 주소를 잃음.** 원본 DHCP 서버가 다른 주소를 줬는지(다리를 건너 옴), 게스트가 고정 주소인데 계획이 `ipam`이었는지 본다.
  되돌리기(`network migration rollback`)는 언제든 된다.
- **잠금 켜기가 409 `lockdown_peers_outside`.** `--dry-run`의 밖 상대가 NAT 뒤 멤버면 그 출발 주소를 노드 underlay로 알리게 고치거나
  `--force`, 지운 노드면 그대로 켠다. 켠 뒤 노드가 붙지 못하면 `sdn status`의 잠금 줄과 그 노드의 `firewall`·오류를 본다. 밖 상대가
  `(Raft)`면 그 중앙이 Raft를 중앙 주소로 보내게(라우팅·출발 주소) 고친다 — 억지로 켜면 그 중앙이 표를 건 뒤 Raft가 끊긴 것을 보고 걷는다.
- **잠금 켜기 Task가 `could not keep its address allow-list … turned the OVN lockdown off again`으로 실패.** 그 중앙이 표를 건 뒤
  NB·SB Raft가 끊겨 표를 걷었고, Task가 잠금을 다시 껐다(에이전트가 되돌린다 — `sdn status`의 잠금 줄). `--dry-run`의 `(Raft)` 상대를
  보고 고친 뒤 `sdn lockdown on`을 다시 한다.
- **옮기기 만들기가 409 `migration_overlap`.** 같은 원본·같은 VM, 또는 다리를 놓는 같은 대상의 끝나지 않은 옮기기가 있다 — 끝내거나 되돌린 뒤,
  또는 `--no-bridge`로.

### 계획 67이 아직 하지 않는 것

- 부하 분산의 L7(HTTP 규칙·TLS 종료·쿠키 고정), SCTP, IPv6 VIP·건강 검사, 라우터 없는 네트워크·공급자 네트워크에 곧바로 둔 VIP, LB끼리
  VIP 나누기, `Load_Balancer_Group`.
- QoS 정책 리소스·네트워크 기본 QoS, 최소 대역폭 보장, 유동 IP QoS, 단순 NIC의 DSCP.
- SDN → 단순 옮기기(되돌리기만), VXLAN 장치를 통째로 OVN으로 바꾸기, 실행 중 단순 ↔ 단순 NIC 네트워크 바꾸기.
- OVN 잠금 다음: `Port_Binding` 주인 견주기 경보, 클러스터 CA 바꾸기·인증서 폐기, Geneve IPsec, NB 읽기 전용 원격.
- 토폴로지에서 끌어 놓아 잇기(라우터 인터페이스 만들기 따위), 노드·랙 배치(계획 70의 2.5D 클러스터 보기).
- 라우터끼리 잇기, 공급자 네트워크를 인터페이스로, 바깥 게이트웨이 여럿·ECMP, 정책 경로, 포트 포워딩.
- 보안 그룹을 폴더에(테넌트가 자기 그룹을 만듦), 다른 그룹을 원격으로 가리키는 규칙, ACL 로그·카운터, 관리자 정책 층.
- 서브넷 `default_route: false`(SDN NIC 여럿인 VM), DNS 이름, 접두사 위임, BGP·EVPN 광고, OVN 연동(IC), physnet에 물리 NIC를
  직접, 하드웨어 오프로드·DPDK, SDN 끄기와 OVN 지우기, 중앙 자동 교체.

### 한계

- **꺼진 중앙을 내쫓아도 그 노드는 모른다.** 내쫓은 노드의 단위·DB는 그대로라, 되살리면 옛 OVN 서버가 다시 켜진다(클러스터는
  받지 않는다). "남은 중앙 DB"로 보이므로 `sdn central remove`로 치운다.
- **여러 노드 시험은 컨테이너다.** chassis 셋 사이의 Geneve, 실제 OVN Raft 셋, 두 chassis 라이브 마이그레이션, 게이트웨이 넘김,
  노드 통째 죽음과 죽은 중앙의 실제 `cluster/kick`, 계획 67의 여러 chassis 보안 그룹(c1 → c3 ping이 그룹을 건 뒤 2.1초에 막힘, tcp 22는
  됨)·LB 넘김·바깥 VIP의 게이트웨이 넘김·Geneve 너머 QoS·VXLAN 원본 옮기기와 다리 노드 바꾸기·잠금과 지운 노드·경보는 GitHub
  `sdn-multinode` 작업(한 러너의 systemd 컨테이너 셋, 커널 하나)이 본다. 실제 하드웨어 장애의 넘김 시간, 실제 물리 VLAN 스위치와 그
  DSCP 처리, 높은 속도의 QoS 정확도(veth), 게스트 OS의 응답(게스트 없는 VM)은 보지 않았다.
- **연 라우터 서랍의 지금 게이트웨이, LB 목록의 멤버 건강, 토폴로지의 살아 있는 상태는 30초마다 읽는다**(OVN의 상태라 사건이 없다).
  라우터·유동 IP·LB·옮기기 목록은 계획 67부터 watch다.
- `vm get`은 NIC의 유동 IP를 따로 보이지 않는다(웹 하드웨어 표·`fip list`). 그래서 **유동 IP를 읽을 수 없는 VM 주인은 웹에서 그 유동 IP를
  찾을 길이 없다**(떼기 권한은 있음 — 키를 알면 CLI·API로).
- **보안 그룹 쓰기가 실패하면.** 한 그룹 목록의 Port_Group 쓰기가 계속 실패하면(같은 이름의 남의 Port_Group 따위) 그 목록을 쓰는
  네트워크의 포트 쓰기가 모두 실패한다(그 네트워크의 새 VM은 포트가 없어 켜지지 않음 — 닫힌 쪽으로 실패). 이미 있는 목록의 규칙을
  더 좁히는 쓰기가 실패하면 그 목록의 포트는 옛(더 넓은) 규칙에 남는다. 둘 다 `sdn status`의 동기화 오류로 보인다.
- **게이트웨이 표에 chassis 상태가 없다**(`sdn status`가 보인다). 넘김·chassis 꺼짐은 [SDN 경보](#sdn-경보)로 안다.
- "10초 안에 포트가 바인딩되지 않음" 경고는 orbivisord 로그에만 있고 API·웹에는 없다.
- 주소가 있는 NIC의 빠진 체계 채우기(`--fill`)는 CLI만이다. 주소 없는 NIC는 서브넷을 더할 때 서버가 채운다.
- 서브넷은 체계마다 하나, DHCP 릴레이·여러 서브넷·DNS 이름은 없다. 템플릿 배포 `--count` 2 이상에는 주소를 고를 수 없다.
- 원격 복제(계획 55)를 받는 클러스터에는 같은 이름의 SDN 네트워크가 있어야 하고, 주소가 겹치면 받기가 실패한다.
- 중앙 판은 첫 중앙의 OVN 판과 주·부 판이 같아야 한다(꾸러미를 올릴 때는 모든 중앙을 함께).
- **부하 분산.** VIP·건강 검사는 IPv4만, UDP 검사는 약하다. 죽은 멤버는 새 연결에서만 빠진다(맺은 연결은 그 멤버로 가다 끊김). VIP 네트워크는
  라우터 인터페이스여야 한다. LB는 복제·템플릿·원격 복제를 따라가지 않는다.
- **QoS는 폴리싱이다.** 넘는 패킷을 버리므로 UDP는 넘는 만큼 잃고 처음 몇 초는 버스트만큼 더 간다. 단순 NIC의 tc는 orbivisord를 다시 띄워도
  다시 맞추지 않는다(커널이 tap과 함께 들고 있음 — 어긋나면 `vm hotplug`).
- **옮기기.** cloud-init이 있는 VM은 instance-id가 바뀌어 다음 부팅에 인스턴스마다 하는 일(SSH 호스트 키 따위)을 다시 한다. 방식은 옮기기
  하나에 하나다. 켜진 VM이 chassis가 아닌 노드에 있으면 막힌다. 다리는 노드 하나를 지나고 넘김이 없으며, 살아 있는 다리 노드는 바꿀 수 없다
  (모두 되돌린 뒤 `--bridge-node`로 다시). 브리지·VLAN 원본의 MTU는 1500으로 본다.
- **잠금.** `Port_Binding`·`MAC_Binding`·`FDB` 가로채기는 OVN이 막지 않는다. NB는 여전히 모든 멤버 노드가 쓴다. 미리 확인은 집합 밖 상대가
  하나라도 있으면 막는다(`--force`). 계획 65·66으로 세운 클러스터는 켤 때까지 잠기지 않는다.
- **경보.** `sdn_quorum_lost`는 순수 시험으로만 보았다. 넘김 이벤트는 리더가 바뀐 직후의 첫 관찰과 견주지 않는다.
- **토폴로지.** 네트워크 200·VM 2000이면 SVG 원소가 약 11,400개다(배치는 13 ms) — 그 크기에서는 폴더로 거르거나 VM을 감추라는 띠가 뜬다.
