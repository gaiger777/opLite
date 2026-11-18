# 네트워크

VXLAN 네트워크와 VM 방화벽(보안 그룹). 처음 보는 사람은 [README](../../README.md)의 빠른 시작부터.

- [VXLAN 네트워크](#vxlan-네트워크)
- [VM 방화벽 (보안 그룹)](#vm-방화벽-보안-그룹)

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
