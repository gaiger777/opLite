# opLite 네트워크·SDN 설계 (하위 프로젝트 6: 계획 25·26)

작성일: 2026-11-03
상위 문서: `2026-10-20-opl-node-core-design.md`(하위 프로젝트 분해 §1.1, 1차 네트워크 §7, 스케줄러 §3)

## 1. 목표와 범위

처음 분해에서 하위 프로젝트 6은 네트워크와 SDN(브리지 관리, VXLAN, 방화벽: pve-network·NSX)이다. 지금 VM NIC는 관리자가 노드마다 미리 만든 Linux 브리지(선택적으로 VLAN 태그)에만 붙는다. 노드 사이에 L2를 잇는 길은 물리 스위치의 VLAN뿐이고, VM 트래픽을 거르는 길이 없다.

| 계획 | 내용 |
|---|---|
| 25 | VXLAN 네트워크: opld가 모든 노드에 브리지와 VXLAN 장치를 만들고 관리한다(노드 underlay 주소, 피어 목록, MTU), 스케줄러·API·oplctl·웹 |
| 26 | VM 방화벽: 보안 그룹(`SecurityGroup`) 리소스, NIC마다 보안 그룹, 노드가 클러스터 상태로 nftables(bridge family) 규칙을 만들어 통째로 바꾼다, 상태 추적·MAC 위조 막기 |

넣지 않는 것: 물리 NIC·본딩·업링크 브리지 관리(설치 ISO, 하위 프로젝트 9에서), EVPN·BGP 제어 평면, IPAM·DHCP·SNAT(서브넷), VXLAN 암호화, 노드(호스트) 방화벽과 클러스터 전체 규칙, IP 위조 막기(IPAM이 필요), 원격으로 다른 보안 그룹 가리키기, 규칙 로그, 실행 중 VM의 NIC 추가·제거(지금처럼 다음 시작에 반영).

### 견준 길

- **오버레이: VXLAN 유니캐스트 헤드엔드 복제(채택) vs 멀티캐스트 vs EVPN.** 멀티캐스트는 underlay 스위치 설정이 필요하고, EVPN은 FRR·BGP를 들여와야 한다. 클러스터가 이미 모든 노드 주소를 알고 있으므로 각 VXLAN 장치에 다른 노드 주소를 고정 FDB(전부 0 MAC)로 넣어 BUM 트래픽을 복제하고, 유니캐스트는 커널이 배운다(Proxmox SDN의 VXLAN 영역과 같은 방식). 3~32노드에서 충분하다.
- **장치 다루기: iproute2 명령(채택) vs netlink 크레이트.** tap을 이미 `ip`·`bridge` 명령으로 다룬다. 같은 방식을 쓰면 시험과 운영에서 같은 길을 탄다.
- **방화벽 엔진: nftables bridge family(채택) vs iptables + VM마다 방화벽 브리지(Proxmox의 옛 `fwbr`).** nftables는 브리지 안에서 바로 거르고(커널 5.3 이상의 `nf_conntrack_bridge`로 상태 추적), 규칙 전체를 `nft -f`로 원자적으로 바꾼다. VM마다 브리지를 더 두면 장치 수와 실패 경우가 늘어난다.
- **규칙 범위: 클러스터의 모든 방화벽 NIC를 모든 노드에(채택) vs 그 노드에 배치된 VM만.** tap 이름이 VM id에서 나와 클러스터 안에서 겹치지 않으므로 규칙을 tap 이름으로 걸면 어느 노드에 있든 같은 규칙 집합이 된다. 배치·마이그레이션·HA 복구 중에 "이 VM이 지금 이 노드에 있나"를 따질 필요가 없다. 규칙 수는 판정 맵(verdict map)으로 tap 이름에서 VM 체인으로 바로 뛰므로 패킷당 비용은 늘지 않는다.
- **규칙 모양: 순서 있는 허용·차단 규칙과 기본 정책(채택) vs AWS식 허용만.** 허용만으로는 나가는 방향을 막을 수 없다. 들어오기 기본 차단, 나가기 기본 허용에 위에서부터 처음 맞는 규칙을 따르는 Proxmox 방식을 쓴다.

## 2. VXLAN 네트워크 (계획 25)

### 2.1 리소스

`NetworkSpec`에 VXLAN을 더한다. 저장된 옛 네트워크(JSON)는 그대로 읽힌다.

```yaml
# 지금처럼: 관리자가 만든 브리지
spec: { bridge: vmbr0, vlan: 10 }
# 새로: opld가 만드는 VXLAN 네트워크
spec: { vxlan: { vni: 1000 }, mtu: 1450 }
```

- `bridge`와 `vxlan` 중 정확히 하나가 있어야 한다. `vlan`은 `bridge`와만, `mtu`는 `vxlan`과만 쓴다.
- `vni`는 1~16777215. 클러스터 안에서 겹치면 409 `vni_in_use`.
- `mtu`는 1280~9000, 기본 1450(underlay 1500에서 VXLAN 머리 50바이트를 뺀 값). underlay는 `mtu + 50`을 실어야 한다.
- 관리 장치 이름: 브리지 `oplv<vni>`, VXLAN 장치 `oplx<vni>`(최대 12자). `bridge` 네트워크는 `^opl[vx][0-9]+$` 모양 이름을 쓸 수 없다(400). tap 이름(`op` + 16진수)과는 겹치지 않는다.
- 네트워크는 지금처럼 만들고 지우기만 한다(바꾸기 없음). VM이 쓰는 네트워크는 지울 수 없다(409 `network_in_use`, 그대로).
- `NetworkSpec::bridge_name()`이 VM NIC가 붙을 브리지 이름을 준다(`bridge` 또는 `oplv<vni>`). 스케줄러, resolve, 웹이 이것을 쓴다.

구현(계획 25): `opl_types::network`의 `NetworkSpec { bridge: Option<String>, vlan, vxlan: Option<Vxlan { vni }>, mtu }`(`deny_unknown_fields`), 비어 있는 필드는 `vlan`까지 모두 빼고 써서 옛 JSON이 같은 JSON으로 다시 쓰인다(OpenAPI는 `nullable = false`, 선택 필드). 검증 오류는 `NetworkError::{KindRequired, VlanWithVxlan, MtuWithBridge, BadVni, BadMtu, ReservedBridge}`(400). 함수 `vxlan_bridge`·`vxlan_device`·`is_managed_ifname`(정규식 크레이트 없이), 상수 `DEFAULT_VXLAN_MTU = 1450`·`VXLAN_PORT = 4789`, `NetworkSpec::{bridge_name, mtu, vni}`이다. `bridge_name()`은 검증을 통과하지 않은 스펙이면 빈 문자열이고, `mtu()`는 VXLAN이면 늘 값이 있다(없으면 기본값). `POST /networks`의 409 `vni_in_use`("VNI {vni} is used by network {name:?}")는 저장된 목록을 보고 판단하므로 같은 VNI를 동시에 만드는 두 요청은 막지 못한다. 그때 노드의 에이전트는 이름이 앞선 네트워크만 쓴다(§2.3): 두 네트워크의 VM이 같은 `oplv<vni>`에 붙어 L2를 나누고, 브리지·VXLAN 장치·tap의 MTU는 이름이 앞선 네트워크의 것이다. IPv6 underlay는 바깥 머리가 70바이트(IPv6 40 + UDP 8 + VXLAN 8 + 이더넷 14)라 underlay가 `mtu + 70`(기본 1450이면 1520)을 실어야 한다.

### 2.2 노드 underlay

- 노드는 VXLAN 패킷을 주고받을 주소를 `NodeStatus.underlay: Option<IpAddr>`로 기록한다(없으면 필드를 뺀다).
- 값: 설정 `[network] underlay = "10.0.0.11"`. 없으면 노드 `address`의 호스트 부분이 IP면 그것, 이름이면 시작할 때 한 번 풀어 나온 첫 주소. 어느 것도 안 되면 None이고 경고를 남긴다. 위트니스는 기록하지 않는다.
- underlay가 없는 노드에는 VXLAN 장치를 만들지 않는다. 그래서 그 노드 브리지 목록에 `oplv<vni>`가 없고 스케줄러가 그 네트워크를 쓰는 VM을 배치하지 않는다.
- 모든 노드의 underlay는 같은 주소 체계(IPv4끼리 또는 IPv6끼리)여야 한다. 다른 체계의 피어는 FDB에 넣지 않고 경고한다.

구현(계획 25): opld 설정 `Config.network: NetworkSection { underlay: Option<IpAddr> }`, `0.0.0.0`·`::`은 설정 오류다. `Config::underlay_source()`(순수: 설정 → `address`가 IP 리터럴 → 이름)와 `Config::underlay()`(이름이면 `tokio::net::lookup_host((host, 0))`의 결과에서 `pick_underlay`가 loopback·link-local·미지정 주소를 빼고 IPv4를 먼저, 없으면 IPv6를 고른다. 남는 것이 없거나 실패하면 None과 경고; 시험은 `underlay_with`로 이름 풀기를 바꿔 끼운다)가 정하고, `register_node`가 시작할 때마다 `NodeStatus.underlay`에 쓴다(위트니스는 쓰지 않는다). 이름을 풀지 못해 None이면 기록된 값을 지킨다(`underlay.or(status.underlay)`). OpenAPI는 `format: ip`인 선택 필드다.

### 2.3 오버레이 에이전트

위트니스가 아니고 호스트 네트워크(`HostNet`)가 있는 노드(Linux)의 opld에서 돈다.

- 바라는 상태: 클러스터의 VXLAN 네트워크마다
  - 브리지 `oplv<vni>`: `mtu`, VLAN 거르기 없음, up
  - VXLAN 장치 `oplx<vni>`: `id <vni> local <내 underlay> dstport 4789`, `mtu`, 주인은 `oplv<vni>`, up. 학습은 켠다(유니캐스트 MAC → VTEP를 배운다).
  - FDB: 위트니스가 아니고 underlay가 있는 다른 노드마다 `00:00:00:00:00:00 dst <피어 underlay>` 하나. 남는 전부 0 항목은 지운다.
- 소유 표시: 두 장치의 `alias`를 `opl:<클러스터 태그>`로 둔다. 클러스터 태그는 클러스터 CA 인증서 지문의 앞 12자리(16진수)다. 정리는 이름이 `^opl[vx][0-9]+$`이고 alias가 내 태그인 장치 가운데 바라는 상태에 없는 것만 지운다. 같은 호스트의 다른 클러스터(시험)와 관리자가 만든 장치는 건드리지 않는다.
- 이미 있는 장치가 바라는 값(vni, local, dstport, 주인)과 다르면 VXLAN 장치를 지우고 다시 만든다. MTU만 다르면 바꾼다.
- 언제: opld 시작, Network·Node 리소스가 바뀔 때(watch, 1초 모아 한 번), 그리고 60초마다. 장치를 만들거나 지웠으면 바로 브리지 목록을 다시 기록한다(지금은 1분마다라 새 네트워크를 1분 동안 못 쓴다).
- 실패하면 경고를 남기고 다음 차례에 다시 한다. 한 네트워크 실패가 다른 네트워크를 막지 않는다.
- `HostNet`에 `sync_overlays(&self, tag, desired: &[Overlay])`를 더한다. `Overlay { vni, mtu, local: IpAddr, peers: Vec<IpAddr> }`. Mock은 받은 값을 기록한다.

구현(계획 25, 드라이버): `opl_driver::{Overlay, OverlayChanges { devices_changed }}`, `HostNet::sync_overlays(tag, desired) -> Result<OverlayChanges, DriverError>`. `devices_changed`는 장치를 만들거나 지우거나 다시 만들었을 때만 참이다(FDB·MTU만 고치면 거짓). 여러 네트워크의 실패는 모아 `DriverError::Overlay("VNI …: …; …")` 하나로 돌려주고, 실패하면 이번 호출에서 `ip link add`가 성공한 장치만 지운다(`rollback_links`). 이름이 관리 모양이고 alias도 포트도 없고 내려가 있는 장치는 만들다 멈춘 내 장치로 보고 지우고 다시 만든다(alias와 up을 한 명령으로 주므로 만들다 멈춘 장치는 늘 내려가 있다; alias가 다르거나 포트가 있거나 alias 없이 올라가 있으면 남의 것). 정리할 내 브리지에 자기 `oplx<vni>` 말고 포트(VM tap 따위)가 남아 있으면 그 브리지와 VXLAN 장치를 남기고 "bridge oplvN still has ports: …"를 오류에 더한다. `IpRoute`는 `IpRoute { netns: Option<String>, tag: Option<String> }`(`IpRoute::default()`, 시험용 `IpRoute::in_netns(name)`이 모든 `ip`·`bridge` 명령 앞에 `-n NAME`; `with_cluster_tag(tag)`면 `bridges()`와 `create_tap`이 관리 이름 브리지 가운데 alias가 `opl:<tag>`인 것만 쓰고, 태그가 없으면 관리 이름 브리지를 모두 뺀다 — 남의 것이나 만들다 멈춘 `oplv…`에 tap이 붙지 않게)이다. opld는 `Host::from_config(config, tag)`로 태그를 넘긴다. **Ubuntu 24.04의 iproute2 6.1(fan 패치)은 VXLAN 장치의 `ip -j -d link show` JSON을 깨뜨리고**(`"info_data":{"id":7fan-map ,…}`), **호스트 이름공간에서는 `ip -d link show` 목록(전체나 `type vxlan`)이 세그폴트한다**(장치 하나를 `dev`로 지정하면 된다). 그래서 목록 명령에는 `-d`를 쓰지 않는다: 이름·`master`·`ifalias`·`mtu`·up은 `ip -j link show`, 브리지 구별은 `ip -j link show type bridge`, 브리지가 아닌 관리 이름 장치마다 `ip -d -o link show dev NAME` 글의 `vxlan` 단락에서 `id`·`local`·`dstport`를 읽는다(`parse_vxlan_details`, `vxlan` 단락이 없으면 VXLAN 장치가 아니다). 세부를 읽지 못한 장치의 네트워크만 그 차례에 오류로 남기고 다시 만들지 않는다. 도구가 stderr 없이 실패하면 `ToolFailed`에 종료 상태("exit status N", "killed by signal N")를 넣는다. FDB는 `bridge -j fdb show dev oplx<vni>`의 전부 0 MAC 항목의 `dst`를 `bridge fdb append`·`del`로 맞춘다. 시험: 명령 목록·파싱 단위 시험, `net-it` `opl-driver/tests/overlay.rs`(이름공간 셋을 veth로 잇고 "VM" 사이 ping, 다시 부르면 바뀐 것 없음, 피어 빼기, 비우기, alias가 다른 장치는 남음).

구현(계획 25, 에이전트): `crates/opld/src/overlay.rs`. `cluster_tag(ca_pem)`은 지문 `sha256:` 뒤 16진수 앞 12자, `desired_overlays(networks, nodes, me) -> Option<Vec<Overlay>>`는 순수 함수다(내가 없거나 위트니스거나 underlay가 없으면 None이고 에이전트는 sync를 건너뛴다 — 잠깐 underlay를 잃었다고 VM이 쓰는 장치를 지우지 않게, 경고는 한 번이고 내 Node가 아직 없으면(첫 부팅) debug만; 피어는 나·위트니스·underlay 없는 노드·내 주소와 같은 주소를 빼고 정렬·중복 제거, VNI 순, 같은 VNI가 둘이면 이름이 앞선 것). `OverlayAgent::spawn`은 위트니스가 아니고 `host.net`이 있을 때만 띄운다(member의 `Services.overlay`, 종료 때 멈추고 장치는 남긴다. `cluster leave`(`close(left = true)`)면 `sync_overlays(tag, &[])`로 이 노드의 장치를 지운다). 바라는 목록이 비어도 `sync_overlays`를 불러 내 태그 장치를 정리한다. `devices_changed`이거나 **`sync_overlays`가 실패해도**(다른 네트워크 장치는 바뀌었을 수 있다) 곧바로 `bootstrap::refresh_bridges`를 부른다(sync 오류를 먼저 경고로 남긴다). 종료 신호는 차례 사이에서만 보고 하던 sync는 끝까지 한다. Network·Node watch는 목록을 읽기 전에 구독하고, 구독이 끊기면(뒤처짐·스냅샷) 변경 때처럼 1초 기다리고 맞춘 뒤 다시 구독한다. 시험: `desired_overlays` 표, 기록하는 가짜 `HostNet`으로 만들기·노드 더하기·지우기 뒤 다시 맞춤과 브리지 기록, 위트니스·`host.net` 없음, underlay가 없으면 건너뜀, 종료가 하던 sync를 기다림.

### 2.4 tap과 QEMU

- tap을 만들 때 MTU를 붙을 브리지의 MTU로 맞춘다(브리지 목록 JSON의 `mtu`). 모든 브리지에 같다.
- `NicBackend::Tap`에 `mtu: Option<u16>`을 더한다. VXLAN 네트워크면 resolve가 네트워크 `mtu`를 넣고, QEMU는 virtio NIC에 `host_mtu=<mtu>`를 붙여 게스트가 MTU를 알게 한다(e1000 등은 게스트 설정에 맡긴다). `bridge` 네트워크는 None으로 지금과 같다.

구현(계획 25): `create_tap`이 `ip tuntap add` 뒤, 브리지에 붙이기(`master`) 전에 브리지 목록 JSON(`ip -j -d link show type bridge`, 브리지만 나오므로 fan 패치와 상관없다)의 `mtu`로 `ip link set dev TAP mtu M`을 한다(값이 없으면 생략). 그래서 `Tap` 구조체에는 MTU가 없다. `NicBackend::Tap.mtu`는 None이면 빼고 쓴다(옛 정의는 None). `-incoming`(마이그레이션 받기)도 같은 명령줄 코드라 `host_mtu`가 붙는다.

### 2.5 API·oplctl·웹

- API: `POST /networks`가 새 모양을 받는다. 검증 오류는 400, `vni_in_use`는 409.
- oplctl: `network create NAME --vxlan VNI [--mtu N]`(지금의 `--bridge`·`--vlan`과 함께 하나만). `network list`는 `TYPE`(bridge·vxlan), `BRIDGE`, `VLAN/VNI`, `MTU`. `node list`/`show`에 underlay.
- 웹: 네트워크 만들기에서 종류(브리지·VXLAN)를 고르고, 목록에 종류·VNI·MTU를 보인다. 노드 상세에 underlay.

구현(계획 25): oplctl `network create NAME (--bridge B [--vlan V] | --vxlan VNI [--mtu M])`(clap 그룹, 주지 않은 값은 보내지 않는다), `network list` 열 `NAME TYPE BRIDGE VLAN/VNI MTU`(VXLAN의 BRIDGE는 `oplv<vni>`, MTU는 없으면 1450, 브리지 네트워크는 `-`), `node list`에 `UNDERLAY`(없으면 `-`). `node show` 명령은 없다 — `node get`이 JSON을 보이므로 `status.underlay`가 그대로 나온다. 웹은 만들기 대화상자의 종류 선택(브리지면 브리지·VLAN, VXLAN이면 VNI·MTU, 관리 이름 모양의 브리지 이름은 받지 않음), 목록의 종류·브리지(관리 이름)·VLAN/VNI·MTU, 그리고 노드 상세 대신 **노드 목록**의 "언더레이 주소" 열이다. E2E `crates/opld/tests/vxlan.rs`(`qemu-it,net-it`, Linux root): opld 하나에 무작위 VNI 네트워크를 만들면 `oplv`·`oplx`가 alias `opl:<태그>`로 생기고 노드 브리지 목록에 오르며, 그 네트워크 VM의 tap이 MTU 1450으로 붙고 QEMU 명령줄에 `host_mtu=1450`이 있다. 같은 호스트에 띄운 다른 클러스터의 opld는 그 장치를 지우지 않고, 네트워크를 지우면 두 장치와 브리지 목록 항목이 곧바로 사라진다. CI는 이미 `opld/net-it`·`opl-driver/net-it`를 돌리므로 바꾸지 않았다.

## 3. VM 방화벽 (계획 26)

### 3.1 보안 그룹

새 리소스 `SecurityGroup`(`/security-groups`, 만들기·읽기·목록·지우기, `PATCH`로 규칙 바꾸기, `If-Match`).

```yaml
spec:
  description: web servers        # 선택
  rules:
    - { direction: in,  action: accept, protocol: tcp, ports: "80", remote: 0.0.0.0/0 }
    - { direction: in,  action: accept, protocol: tcp, ports: "22", remote: 10.0.0.0/8 }
    - { direction: out, action: drop,   protocol: tcp, ports: "25" }
```

- `direction`: `in`(VM으로 들어옴) 또는 `out`(VM에서 나감). `action`: `accept` 또는 `drop`.
- `protocol`(선택): `tcp`, `udp`, `icmp`, `icmpv6`. 없으면 모든 프로토콜.
- `ports`(선택): 목적지 포트 `"22"` 또는 `"8000-8080"`(1~65535, 시작 ≤ 끝). `tcp`·`udp`에서만.
- `remote`(선택): CIDR. `in`이면 보낸 쪽, `out`이면 받는 쪽 주소. 없으면 어디든. IPv4 CIDR은 IPv4 패킷에만, IPv6은 IPv6에만 맞는다. `icmp`는 IPv4, `icmpv6`는 IPv6에만 쓸 수 있다.
- 규칙은 그룹당 200개까지. 이름 규칙은 다른 리소스와 같다. VM이 쓰는 그룹은 지울 수 없다(409 `security_group_in_use`).

### 3.2 NIC에 걸기

- `NicSpec.security_groups: Vec<String>`(기본 빈 목록, 비면 직렬화에서 뺀다). 비어 있지 않으면 그 NIC의 방화벽이 켜진다. 규칙 없는 그룹 하나만 걸면 "들어오기 모두 차단, 나가기 모두 허용"이다.
- NIC당 8개까지, 겹치면 안 되고, 모두 있어야 한다(400). `user` 네트워크 NIC에는 걸 수 없다(400).
- 실행 중에도 바꿀 수 있고 곧바로 적용된다(디스크·노드처럼 막지 않는다). 그룹 규칙을 바꿔도 곧바로 적용된다.

### 3.3 규칙의 뜻

방화벽이 켜진 NIC마다 두 체인이 생긴다. 규칙은 NIC의 그룹 순서대로, 그룹 안에서는 적힌 순서대로 보고 처음 맞는 것을 따른다.

- 나가기(VM → 밖): ① 보낸 MAC이 NIC의 MAC이 아니면 차단 ② ARP 허용 ③ 이미 맺은 연결(established·related) 허용, 잘못된 패킷(invalid) 차단 ④ `out` 규칙 ⑤ 기본 허용.
- 들어오기(밖 → VM): ① ARP, IPv6 이웃 찾기(neighbor solicit·advert, router advert), DHCP 응답(UDP 67→68, 547→546) 허용 ② 이미 맺은 연결 허용, 잘못된 패킷 차단 ③ `in` 규칙 ④ 기본 차단.
- 같은 브리지의 방화벽 VM 둘 사이 패킷은 보낸 VM의 나가기와 받는 VM의 들어오기를 모두 통과해야 한다. 나가기 체인의 허용은 `return`(다음 검사로), 차단만 끝이다.
- 노드 자신과의 트래픽(브리지에 노드 주소가 있을 때)도 같은 체인을 탄다(bridge `input`·`output` 훅).

### 3.4 노드의 방화벽 에이전트

- 테이블: `table bridge opl_<클러스터 태그>` 하나(§2.3의 태그. 같은 호스트의 다른 클러스터 시험과 겹치지 않는다). 판정 맵 둘(`tap 이름 → jump out_<nic>`, `tap 이름 → jump in_<nic>`), 훅 체인 셋(`forward`, `input`, `output`, 기본 허용), NIC마다 체인 둘.
- 클러스터의 모든 VM에서 방화벽 NIC를 모아 규칙 글을 만든다(순수 함수, 같은 입력이면 같은 글). 적용은 `nft -f -`에 "테이블 만들기(없으면) → 지우기 → 새로 만들기"를 한 번에 넣어 원자적으로 바꾼다. 방화벽 NIC가 하나도 없으면 테이블을 지운다.
- 규칙 글에는 사용자가 쓴 글자를 넣지 않는다(이름·설명 대신 tap 이름과 순번만). 그래서 nft 문법에 끼어들 수 없다.
- 언제: opld 시작, VM·SecurityGroup이 바뀔 때(watch, 0.5초 모아 한 번), 60초마다(누가 지웠어도 되살린다). 바뀐 글이 없으면 60초 차례만 다시 넣는다.
- 시작 직전 확인: 워커는 방화벽 NIC가 있는 VM을 켜거나(마이그레이션 받기 포함) 다시 켜기 전에 에이전트의 `sync()`를 불러 지금 클러스터 상태로 규칙을 넣는다. 실패하면 켜지 않는다(fail closed). 에이전트가 늦게 깨어나도 방화벽 없이 도는 순간이 없다.
- 노드 능력: 시작할 때 `nft`를 찾으면 `NodeStatus.firewall = true`. 스케줄러는 방화벽 NIC가 있는 VM을 이런 노드에만 둔다. 워커도 이 노드에 방화벽이 없으면 켜지 않는다.
- 위트니스와 macOS 노드에는 에이전트가 없다.

### 3.5 API·oplctl·웹

- API: `/security-groups` 다섯 경로, VM 만들기·바꾸기에서 그룹 검사.
- oplctl: `security-group create NAME [--description D] [--rule RULE]...`, `list`, `show`, `delete`, `rule add NAME RULE [--at N]`, `rule rm NAME N`. RULE은 `in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]` 한 줄이다(예: `"in accept tcp 22 10.0.0.0/8"`). `vm create`·`vm set`의 NIC 지정에 `sg=a+b`.
- 웹: 보안 그룹 목록·만들기·규칙 편집 페이지, VM NIC 편집에서 그룹 고르기.

## 4. 오류와 경계

- VXLAN 장치 만들기 실패, `nft` 실패는 에이전트가 경고로 남기고 다시 한다. 방화벽 시작 직전 확인의 실패만 VM 시작 실패(지금의 시작 실패 처리와 같다)다.
- underlay가 바뀌면(설정 바꾸고 재시작) 에이전트가 VXLAN 장치를 다시 만들고, 다른 노드는 Node watch로 FDB를 고친다.
- br_netfilter가 켜진 호스트에서도 bridge family 규칙은 그대로 적용된다. 노드의 iptables 규칙이 VM 트래픽을 따로 거를 수 있다는 점은 README에 적는다.
- VXLAN은 암호화·인증이 없다. underlay는 믿을 수 있는 전용 망이어야 하고 노드 사이 UDP 4789가 열려야 한다(README).

## 5. 시험

| 층 | 무엇 | 어디 |
|---|---|---|
| 단위 | 네트워크·보안 그룹 검증, 관리 장치 이름, iproute2 명령·JSON 파싱, nft 규칙 글(고정된 기대 글), 규칙 문자열 파싱(oplctl) | macOS, CI |
| 에이전트 | Mock `HostNet`으로 바라는 상태(피어, 위트니스 빼기, underlay 없는 노드), 브리지 다시 기록, 시작 직전 방화벽 확인의 실패가 시작을 막음 | macOS, CI |
| API | VXLAN 만들기·`vni_in_use`, 보안 그룹 CRUD·`PATCH`·`security_group_in_use`, NIC 그룹 검사, 스케줄러의 `firewall` 능력 | macOS, CI |
| 통합 `net-it` | 두 네트워크 이름공간을 veth로 잇고 그 안에서 드라이버(`ip -n`)로 VXLAN 네트워크를 만들어 양쪽 "VM"(veth 끝을 둔 이름공간) 사이 ping, 피어 빼기·정리, alias가 다른 장치는 남김 | Linux root(CI) |
| 통합 `net-it` | 이름공간 안의 브리지에 tap 이름의 veth로 "VM" 둘을 붙이고 규칙을 넣어 허용 포트는 통하고 나머지는 막힘, 나가기 차단, MAC 위조 차단, 규칙 바꾸면 곧바로 반영 | Linux root(CI) |
| E2E | opld 하나로 VXLAN 네트워크를 만들고 VM NIC가 `oplv<vni>`에 붙음, 방화벽 NIC VM이 켜질 때 테이블에 그 tap이 있음 | Linux root(CI, `qemu-it,net-it`) |

드라이버의 `IpRoute`와 nft 실행기는 이름공간을 받을 수 있게 한다(`ip -n NS`, `ip netns exec NS nft`). 시험만 쓴다. CI의 linux-kvm 작업에 `nftables` 꾸러미를 더한다.

## 6. 계획 나누기

- 계획 25(VXLAN): 타입·검증 → 노드 underlay → `HostNet::sync_overlays`(iproute2, 이름공간 시험) → 오버레이 에이전트·브리지 다시 기록 → tap MTU·`host_mtu` → API·oplctl·웹 → E2E·문서.
- 계획 26(방화벽): `SecurityGroup` 타입·API → NIC 그룹 검사 → nft 규칙 글 → nft 실행기(이름공간 시험) → 에이전트·워커 시작 직전 확인·노드 능력·스케줄러 → oplctl·웹 → E2E·문서.
