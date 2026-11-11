#!/bin/sh
# CI `ldap` 작업의 시험 디렉터리: Ubuntu의 slapd(OpenLDAP)를 debconf로 설정하고, 시험 CA가 서명한
# 서버 인증서로 LDAPS(636)와 StartTLS(389)를 켜고, memberOf 오버레이 아래 사용자 둘과 그룹 하나를
# 넣는다. opl-api의 `ldap-it` 시험(tests/ldap_it.rs)이 이 디렉터리를 쓴다.
# 값은 시험과 같은 OPL_TEST_LDAP_* 환경 변수에서 읽는다(없으면 아래 기본값).
# 경고: 이 기계의 slapd 설정과 디렉터리(데이터베이스)를 지우고 새로 만든다. 그래서 CI(CI=true)가
# 아니면 OPL_LDAP_SETUP_WIPE=1을 주어야 돈다.
# 쓰는 법(root): sudo -E sh .github/scripts/ldap-setup.sh  (CI 밖: sudo -E OPL_LDAP_SETUP_WIPE=1 sh …)
# 다시 돌려도 된다: 늘 debconf 값으로 slapd를 다시 설정해 새 데이터베이스에서 시작한다.
set -eu

if [ "${CI:-}" != true ] && [ "${OPL_LDAP_SETUP_WIPE:-}" != 1 ]; then
	cat >&2 <<'MSG'
ldap-setup: this wipes the slapd configuration and directory on this machine and makes a new test
directory. Run it only on a throwaway machine: set OPL_LDAP_SETUP_WIPE=1 (CI=true also allows it).
MSG
	exit 1
fi

BASE_DN=${OPL_TEST_LDAP_BASE_DN:-dc=example,dc=com}
BIND_DN=${OPL_TEST_LDAP_BIND_DN:-cn=admin,$BASE_DN}
BIND_PASSWORD=${OPL_TEST_LDAP_BIND_PASSWORD:-opl-admin-pw}
CA_OUT=${OPL_TEST_LDAP_CA:-/tmp/opl-test-ldap-ca.pem}
USER_NAME=${OPL_TEST_LDAP_USER:-alice}
USER_PASSWORD=${OPL_TEST_LDAP_USER_PASSWORD:-alice-pw}
OTHER_NAME=${OPL_TEST_LDAP_OTHER_USER:-bob}
OTHER_PASSWORD=${OPL_TEST_LDAP_OTHER_PASSWORD:-bob-pw}
GROUP_DN=${OPL_TEST_LDAP_GROUP:-cn=admins,ou=groups,$BASE_DN}

# slapd가 읽을 TLS 파일. Ubuntu의 slapd AppArmor 프로필이 /etc/ldap 아래를 읽게 한다.
TLS_CA=/etc/ldap/opl-test-ca.pem
TLS_CERT=/etc/ldap/opl-test-cert.pem
TLS_KEY=/etc/ldap/opl-test-key.pem

die() {
	echo "ldap-setup: $*" >&2
	exit 1
}

[ "$(id -u)" -eq 0 ] || die "run as root"

# dc=example,dc=com → example.com (debconf가 이 도메인으로 기준 DN을 만든다)
DOMAIN=$(printf '%s\n' "$BASE_DN" | sed -e 's/^dc=//' -e 's/,dc=/./g')
[ "$(printf '%s\n' "$DOMAIN" | sed -e 's/^/dc=/' -e 's/\./,dc=/g')" = "$BASE_DN" ] ||
	die "OPL_TEST_LDAP_BASE_DN must be dc=…,dc=… (got $BASE_DN)"
# debconf는 관리자를 cn=admin,<기준 DN>(rootDN)으로 만든다.
[ "$BIND_DN" = "cn=admin,$BASE_DN" ] || die "OPL_TEST_LDAP_BIND_DN must be cn=admin,$BASE_DN"
GROUP_CN=${GROUP_DN%%,*}
GROUP_CN=${GROUP_CN#cn=}
[ "$GROUP_DN" = "cn=$GROUP_CN,ou=groups,$BASE_DN" ] ||
	die "OPL_TEST_LDAP_GROUP must be cn=<name>,ou=groups,$BASE_DN"

export DEBIAN_FRONTEND=noninteractive

# slapd의 설정 스크립트는 쓴 비밀번호 답을 지우므로, 설치와 다시 설정 앞에서 매번 넣는다.
preseed() {
	debconf-set-selections <<SEL
slapd slapd/no_configuration boolean false
slapd slapd/domain string $DOMAIN
slapd shared/organization string Orbivisor test
slapd slapd/password1 password $BIND_PASSWORD
slapd slapd/password2 password $BIND_PASSWORD
slapd slapd/purge_database boolean true
slapd slapd/move_old_database boolean true
slapd slapd/dump_database select when needed
SEL
}

echo "== slapd: domain $DOMAIN"
preseed
apt-get update -q
apt-get install -y -q slapd ldap-utils openssl

# cn=config를 고치는 ldapmodify·찾기(로컬 소켓, root의 EXTERNAL)
config_modify() {
	ldapmodify -Q -Y EXTERNAL -H ldapi:///
}
config_search() {
	ldapsearch -Q -LLL -o ldif-wrap=no -Y EXTERNAL -H ldapi:/// -b cn=config "$@"
}

# slapd가 소켓으로 답할 때까지(최대 30초)
wait_slapd() {
	i=0
	until config_search -s base dn >/dev/null 2>&1; do
		i=$((i + 1))
		[ "$i" -lt 30 ] || die "slapd did not answer on ldapi:///"
		sleep 1
	done
}

# 미리 설치된 slapd이거나 다시 돌릴 때도 같은 상태가 되게 늘 다시 설정한다(옛 데이터베이스는
# debconf의 move_old_database로 /var/backups에 옮겨진다).
echo "== slapd: reconfigure with a new database"
preseed
dpkg-reconfigure -f noninteractive slapd
wait_slapd
slapd -VV 2>&1 | head -n 1 || true

DB_DN=$(config_search "(olcSuffix=$BASE_DN)" dn | sed -n 's/^dn: //p')
[ -n "$DB_DN" ] || die "no database for $BASE_DN"
echo "== database $DB_DN"

echo "== TLS: test CA and a server certificate for 127.0.0.1 and localhost"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cat >"$WORK/ca.cnf" <<CNF
[req]
distinguished_name = dn
prompt = no
x509_extensions = v3_ca
[dn]
CN = Orbivisor test LDAP CA
[v3_ca]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
subjectKeyIdentifier = hash
CNF
cat >"$WORK/server.cnf" <<CNF
[req]
distinguished_name = dn
prompt = no
[dn]
CN = localhost
CNF
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -sha256 -config "$WORK/ca.cnf" \
	-keyout "$WORK/ca.key" -out "$WORK/ca.pem" 2>/dev/null
openssl req -new -newkey rsa:2048 -nodes -sha256 -config "$WORK/server.cnf" \
	-keyout "$WORK/server.key" -out "$WORK/server.csr" 2>/dev/null
cat >"$WORK/server.ext" <<EXT
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:localhost,IP:127.0.0.1
EXT
openssl x509 -req -in "$WORK/server.csr" -CA "$WORK/ca.pem" -CAkey "$WORK/ca.key" \
	-CAcreateserial -days 2 -sha256 -extfile "$WORK/server.ext" -out "$WORK/server.pem" 2>/dev/null
openssl verify -CAfile "$WORK/ca.pem" "$WORK/server.pem"

install -m 0644 "$WORK/ca.pem" "$TLS_CA"
install -m 0644 "$WORK/server.pem" "$TLS_CERT"
install -m 0640 -g openldap "$WORK/server.key" "$TLS_KEY"
install -m 0644 "$WORK/ca.pem" "$CA_OUT"

config_modify <<LDIF
dn: cn=config
changetype: modify
replace: olcTLSCACertificateFile
olcTLSCACertificateFile: $TLS_CA
-
replace: olcTLSCertificateFile
olcTLSCertificateFile: $TLS_CERT
-
replace: olcTLSCertificateKeyFile
olcTLSCertificateKeyFile: $TLS_KEY
LDIF

echo "== memberOf overlay"
MODULE_DN=$(config_search "(objectClass=olcModuleList)" dn | sed -n 's/^dn: //p' | head -n 1)
if [ -z "$MODULE_DN" ]; then
	config_modify <<LDIF
dn: cn=module,cn=config
changetype: add
objectClass: olcModuleList
cn: module
olcModulePath: /usr/lib/ldap
olcModuleLoad: memberof
LDIF
elif ! config_search "(objectClass=olcModuleList)" olcModuleLoad | grep -qi memberof; then
	config_modify <<LDIF
dn: $MODULE_DN
changetype: modify
add: olcModuleLoad
olcModuleLoad: memberof
LDIF
fi
if ! config_search "(&(objectClass=olcMemberOf))" dn | grep -q '^dn:'; then
	config_modify <<LDIF
dn: olcOverlay=memberof,$DB_DN
changetype: add
objectClass: olcOverlayConfig
objectClass: olcMemberOf
olcOverlay: memberof
olcMemberOfDangling: ignore
olcMemberOfGroupOC: groupOfNames
olcMemberOfMemberAD: member
olcMemberOfMemberOfAD: memberOf
LDIF
fi

echo "== LDAPS and StartTLS listeners"
if grep -q '^[#[:space:]]*SLAPD_SERVICES=' /etc/default/slapd; then
	sed -i 's|^[#[:space:]]*SLAPD_SERVICES=.*|SLAPD_SERVICES="ldap:/// ldaps:/// ldapi:///"|' /etc/default/slapd
else
	echo 'SLAPD_SERVICES="ldap:/// ldaps:/// ldapi:///"' >>/etc/default/slapd
fi
grep '^SLAPD_SERVICES=' /etc/default/slapd
systemctl restart slapd
wait_slapd

echo "== people and a group"
USER_HASH=$(slappasswd -s "$USER_PASSWORD")
OTHER_HASH=$(slappasswd -s "$OTHER_PASSWORD")
ldapadd -x -H ldapi:/// -D "$BIND_DN" -w "$BIND_PASSWORD" <<LDIF
dn: ou=people,$BASE_DN
objectClass: organizationalUnit
ou: people

dn: ou=groups,$BASE_DN
objectClass: organizationalUnit
ou: groups

dn: uid=$USER_NAME,ou=people,$BASE_DN
objectClass: person
objectClass: organizationalPerson
objectClass: inetOrgPerson
uid: $USER_NAME
cn: $USER_NAME
sn: $USER_NAME
userPassword: $USER_HASH

dn: uid=$OTHER_NAME,ou=people,$BASE_DN
objectClass: person
objectClass: organizationalPerson
objectClass: inetOrgPerson
uid: $OTHER_NAME
cn: $OTHER_NAME
sn: $OTHER_NAME
userPassword: $OTHER_HASH

dn: $GROUP_DN
objectClass: groupOfNames
cn: $GROUP_CN
member: uid=$USER_NAME,ou=people,$BASE_DN
LDIF

echo "== check: LDAPS search sees memberOf, StartTLS bind as the user"
LDAPTLS_CACERT=$CA_OUT
LDAPTLS_REQCERT=demand
export LDAPTLS_CACERT LDAPTLS_REQCERT
ldapsearch -x -LLL -o ldif-wrap=no -H ldaps://127.0.0.1:636 -D "$BIND_DN" -w "$BIND_PASSWORD" \
	-b "$BASE_DN" "(uid=$USER_NAME)" memberOf >"$WORK/member"
cat "$WORK/member"
grep -qi "^memberOf: $GROUP_DN\$" "$WORK/member" || die "uid=$USER_NAME has no memberOf $GROUP_DN"
ldapwhoami -x -ZZ -H ldap://127.0.0.1:389 -D "uid=$USER_NAME,ou=people,$BASE_DN" -w "$USER_PASSWORD"
ldapwhoami -x -ZZ -H ldap://127.0.0.1:389 -D "uid=$OTHER_NAME,ou=people,$BASE_DN" -w "$OTHER_PASSWORD"
echo "ldap-setup: ready (CA $CA_OUT)"
