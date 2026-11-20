# 여러 노드 SDN 시험(계획 66 스테이지 6, CI `sdn-multinode`)의 Orbivisor 노드 하나: systemd가 PID 1인 privileged 컨테이너.
# OVN·OVS는 넣지 않는다 — orbivisord(`sdn init`·`sdn central add`·`sdn chassis install`)가 apt로 깐다. orbivisord 바이너리와
# 설정·데이터 디렉터리는 시험이 바인드 마운트한다(`crates/opld/tests/sdn_multinode.rs`).
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      systemd systemd-sysv dbus iproute2 nftables kmod procps iputils-ping tcpdump busybox python3 \
      qemu-system-x86 qemu-utils ovmf openssl ca-certificates \
 && rm -f /usr/sbin/policy-rc.d \
 # 컨테이너에 필요 없거나 호스트 장치를 건드리는 단위
 && systemctl mask systemd-udevd.service systemd-udevd-control.socket systemd-udevd-kernel.socket \
      getty.target console-getty.service systemd-logind.service systemd-resolved.service \
      systemd-networkd.service systemd-timesyncd.service

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
