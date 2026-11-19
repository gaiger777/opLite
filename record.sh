#!/bin/bash
# OVSDB JSON-RPC 고정물 녹화(계획 65 스테이지 3). root로 돈다.
set -ex
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q ovn-central ovn-common openvswitch-common >/dev/null
systemctl disable --now ovn-central ovn-northd ovn-ovsdb-server-nb ovn-ovsdb-server-sb || true
D=$(mktemp -d); cd $D
ovsdb-tool create-cluster nb1.db /usr/share/ovn/ovn-nb.ovsschema tcp:127.0.0.1:6643
ovsdb-server nb1.db --remote=punix:$D/nb1.sock --unixctl=$D/nb1.ctl --pidfile=$D/nb1.pid --log-file=$D/nb1.log --detach --no-chdir
sleep 2
CID=$(ovsdb-tool db-cid nb1.db)
ovsdb-tool join-cluster nb2.db OVN_Northbound tcp:127.0.0.2:6643 tcp:127.0.0.1:6643
ovsdb-server nb2.db --remote=punix:$D/nb2.sock --unixctl=$D/nb2.ctl --pidfile=$D/nb2.pid --log-file=$D/nb2.log --detach --no-chdir
for i in $(seq 30); do ovs-appctl -t $D/nb2.ctl cluster/status OVN_Northbound | grep -q 'Status: cluster member' && break; sleep 1; done
ovs-appctl -t $D/nb1.ctl cluster/status OVN_Northbound
python3 $GITHUB_WORKSPACE/rec.py $D/nb1.sock $D/nb2.sock > $GITHUB_WORKSPACE/rpc.jsonl
cat $GITHUB_WORKSPACE/rpc.jsonl
