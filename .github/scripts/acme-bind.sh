#!/bin/sh
# ACME RFC 2136 시험용 BIND(계획 73): 영역 bind.test(모든 이름 → 127.0.0.1)를 TSIG 키 acme-key(hmac-sha256)로 바꿀 수 있게
# 127.0.0.1:PORT에 띄운다. 쓰는 법: acme-bind.sh NAMED DIR PORT — NAMED는 named 실행 파일(우분투는 AppArmor를 비껴가게
# 복사한 것), 끝나면 ORBIVISOR_TEST_BIND·ORBIVISOR_TEST_BIND_KEY를 KEY=VALUE 줄로 찍는다(GITHUB_ENV에 붙인다).
set -eu
named=$1
dir=$2
port=$3
mkdir -p "$dir"
secret=$(head -c 32 /dev/urandom | base64 | tr -d '\n')
cat > "$dir/named.conf" <<CONF
key "acme-key" { algorithm hmac-sha256; secret "$secret"; };
options {
  directory "$dir";
  pid-file "$dir/named.pid";
  session-keyfile "$dir/session.key";
  managed-keys-directory "$dir";
  listen-on port $port { 127.0.0.1; };
  listen-on-v6 { none; };
  recursion no;
  allow-query { any; };
  dnssec-validation no;
};
zone "bind.test" { type master; file "$dir/bind.test.zone"; allow-update { key "acme-key"; }; };
CONF
cat > "$dir/bind.test.zone" <<ZONE
\$TTL 60
@   IN SOA ns.bind.test. hostmaster.bind.test. 1 60 60 600 60
@   IN NS  ns.bind.test.
ns  IN A   127.0.0.1
@   IN A   127.0.0.1
*   IN A   127.0.0.1
ZONE
"$named" -c "$dir/named.conf" -L "$dir/named.log" >/dev/null 2>&1 &
i=0
until dig +short -p "$port" @127.0.0.1 ns.bind.test A | grep -q 127.0.0.1; do
  i=$((i + 1))
  if [ "$i" -gt 50 ]; then
    cat "$dir/named.log" >&2 || true
    echo "named did not start" >&2
    exit 1
  fi
  sleep 0.2
done
echo "ORBIVISOR_TEST_BIND=127.0.0.1:$port"
echo "ORBIVISOR_TEST_BIND_KEY=$secret"
