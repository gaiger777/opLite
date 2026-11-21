//! Orbivisor 소유 OVN 중앙 단위(계획 65 §7.3): NB·SB `ovsdb-server`(Raft 클러스터 DB)와 `ovn-northd`의 명령줄과
//! systemd 단위 파일. 배포판 `ovn-ctl` 대신 직접 쓰는 까닭은 듣는 주소를 노드 주소 하나로 묶고(`pssl:PORT:ADDR`) DB를
//! Orbivisor 자리(`/var/lib/opl/ovn`)에 두려는 것이다. 모두 순수 함수다(시험은 고정 기대 글과 견준다).
//!
//! OVN 접근 잠금(계획 67 §10.2)이 켜진 중앙의 SB 서버는 명령줄 `--remote` 대신 설정 파일(`sb.json`)로 뜬다: 6642는
//! `ovn-controller` 역할(chassis는 자기 행만 쓴다), 6645(특권)는 northd만, 로컬 `punix`는 root. northd는 잠금이 켜진
//! 중앙의 SB에 6645로 붙는다. 잠금이 꺼진 중앙의 글은 계획 65·66과 바이트까지 같다.

use std::net::IpAddr;

use opl_types::sdn::{NB_PORT, NB_RAFT_PORT, SB_ADMIN_PORT, SB_PORT, SB_RAFT_PORT};

use super::host::OvnPaths;

/// Orbivisor가 쓴 단위 파일의 첫 줄(이 표시가 없는 파일은 덮지 않는다)
pub const UNIT_MARK: &str = "# Managed by Orbivisor (orbivisord); changes are overwritten.";
/// `ovsdb-server`(openvswitch-common)
pub const OVSDB_SERVER: &str = "/usr/sbin/ovsdb-server";
/// `ovn-northd`(ovn-central)
pub const OVN_NORTHD: &str = "/usr/bin/ovn-northd";
/// northd 단위 이름
pub const NORTHD_UNIT: &str = "opl-ovn-northd.service";

/// OVN 중앙 DB 둘
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Db {
    Nb,
    Sb,
}

impl Db {
    pub const ALL: [Db; 2] = [Db::Nb, Db::Sb];

    /// 스키마 이름(`ovs-appctl cluster/status`의 인자)
    pub fn name(self) -> &'static str {
        match self {
            Db::Nb => "OVN_Northbound",
            Db::Sb => "OVN_Southbound",
        }
    }

    /// 파일·소켓 이름의 머리(`nb`·`sb`)
    pub fn short(self) -> &'static str {
        match self {
            Db::Nb => "nb",
            Db::Sb => "sb",
        }
    }

    /// 클라이언트가 붙는 포트(NB 6641, SB 6642)
    pub fn port(self) -> u16 {
        match self {
            Db::Nb => NB_PORT,
            Db::Sb => SB_PORT,
        }
    }

    /// 서버끼리의 Raft 포트(NB 6643, SB 6644)
    pub fn raft_port(self) -> u16 {
        match self {
            Db::Nb => NB_RAFT_PORT,
            Db::Sb => SB_RAFT_PORT,
        }
    }

    /// 꾸러미의 스키마 파일 이름
    pub fn schema_file(self) -> &'static str {
        match self {
            Db::Nb => "ovn-nb.ovsschema",
            Db::Sb => "ovn-sb.ovsschema",
        }
    }

    /// systemd 단위 이름(`opl-ovn-nb.service`)
    pub fn unit(self) -> String {
        format!("opl-ovn-{}.service", self.short())
    }

    fn description(self) -> &'static str {
        match self {
            Db::Nb => "Orbivisor OVN northbound database",
            Db::Sb => "Orbivisor OVN southbound database",
        }
    }
}

/// OVS 원격 주소의 호스트 칸(IPv6는 `[…]`)
fn host(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

/// `ssl:ADDR:PORT`
pub fn ssl_addr(ip: IpAddr, port: u16) -> String {
    format!("ssl:{}:{port}", host(ip))
}

/// `pssl:PORT:ADDR`(이 주소에서만 듣는다)
pub fn listen_addr(ip: IpAddr, port: u16) -> String {
    format!("pssl:{port}:{}", host(ip))
}

/// 중앙 모두의 `ssl:C1:PORT,ssl:C2:PORT,…`(주어진 차례)
pub fn remotes(centrals: &[IpAddr], port: u16) -> String {
    centrals
        .iter()
        .map(|ip| ssl_addr(*ip, port))
        .collect::<Vec<_>>()
        .join(",")
}

/// 노드 TLS 사본의 `--private-key`·`--certificate`·`--ca-cert`
fn tls_args(paths: &OvnPaths) -> Vec<String> {
    vec![
        format!("--private-key={}", paths.tls_key().display()),
        format!("--certificate={}", paths.tls_cert().display()),
        format!("--ca-cert={}", paths.tls_ca().display()),
    ]
}

/// 로그: 파일만 info, 콘솔(저널)은 심각한 것만(배포판과 같다)
fn log_args(file: &str, paths: &OvnPaths) -> Vec<String> {
    vec![
        "-vconsole:emer".into(),
        "-vsyslog:err".into(),
        "-vfile:info".into(),
        format!("--log-file={}", paths.log.join(file).display()),
    ]
}

/// DB `db`의 `ovsdb-server` 인자(프로그램 뺌): 노드 주소의 `pssl`과 root만 쓰는 로컬 `punix`, 노드 TLS 사본, 제어
/// 소켓·pid 파일은 `run` 아래. Raft 주소는 DB 파일 안에 있다.
pub fn ovsdb_server_args(db: Db, address: IpAddr, paths: &OvnPaths) -> Vec<String> {
    let mut args = vec![
        paths.db_file(db).display().to_string(),
        format!("--remote={}", listen_addr(address, db.port())),
        format!("--remote=punix:{}", paths.db_socket(db).display()),
    ];
    args.extend(tls_args(paths));
    args.push(format!("--unixctl={}", paths.db_ctl(db).display()));
    args.push(format!(
        "--pidfile={}",
        paths.run.join(format!("{}.pid", db.short())).display()
    ));
    args.extend(log_args(&format!("ovn-{}.log", db.short()), paths));
    args.push("--no-chdir".into());
    args
}

/// 잠금이 켜진 SB의 설정 파일(`ovsdb-server --config-file`, OVS 3.3): 노드 주소의 6642는 `ovn-controller` 역할, 6645는
/// 역할 없음(northd), 로컬 `punix`, DB 파일. 키는 이름 순이다(같은 입력이면 같은 글).
pub fn sb_config(address: IpAddr, paths: &OvnPaths) -> String {
    let q = |s: &str| serde_json::Value::from(s).to_string();
    format!(
        "{{\n  \"databases\": {{\n    {db}: {{}}\n  }},\n  \"remotes\": {{\n    {role}: {{\n      \"role\": \
         \"ovn-controller\"\n    }},\n    {admin}: {{}},\n    {local}: {{}}\n  }}\n}}\n",
        db = q(&paths.db_file(Db::Sb).display().to_string()),
        role = q(&listen_addr(address, SB_PORT)),
        admin = q(&listen_addr(address, SB_ADMIN_PORT)),
        local = q(&format!("punix:{}", paths.db_socket(Db::Sb).display())),
    )
}

/// 잠금이 켜진 SB `ovsdb-server`의 인자(프로그램 뺌): DB와 듣는 주소는 설정 파일에 있고(명령줄 `--remote`·DB 파일과 함께
/// 쓸 수 없다), TLS·제어 소켓·pid·로그는 [`ovsdb_server_args`]와 같다.
pub fn sb_locked_args(paths: &OvnPaths) -> Vec<String> {
    let mut args = vec![format!("--config-file={}", paths.sb_config().display())];
    args.extend(tls_args(paths));
    args.push(format!("--unixctl={}", paths.db_ctl(Db::Sb).display()));
    args.push(format!(
        "--pidfile={}",
        paths.run.join(format!("{}.pid", Db::Sb.short())).display()
    ));
    args.extend(log_args(&format!("ovn-{}.log", Db::Sb.short()), paths));
    args.push("--no-chdir".into());
    args
}

/// `ovn-northd` 인자: 중앙 모두의 NB·SB(northd는 SB 잠금으로 하나만 일한다). SB는 잠금이 켜진 중앙(`locked`)이면
/// 특권 포트 6645, 아니면 6642다(잠금을 하나씩 바꾸는 동안 둘이 섞인다 — 계획 67 §10.4).
pub fn northd_args(centrals: &[IpAddr], locked: &[IpAddr], paths: &OvnPaths) -> Vec<String> {
    let sb = centrals
        .iter()
        .map(|ip| {
            let port = if locked.contains(ip) {
                SB_ADMIN_PORT
            } else {
                SB_PORT
            };
            ssl_addr(*ip, port)
        })
        .collect::<Vec<_>>()
        .join(",");
    let mut args = vec![
        format!("--ovnnb-db={}", remotes(centrals, Db::Nb.port())),
        format!("--ovnsb-db={sb}"),
    ];
    args.extend(tls_args(paths));
    args.push(format!("--unixctl={}", paths.northd_ctl().display()));
    args.push(format!(
        "--pidfile={}",
        paths.run.join("northd.pid").display()
    ));
    args.extend(log_args("ovn-northd.log", paths));
    args.push("--no-chdir".into());
    args
}

/// systemd `ExecStart` 한 낱말(빈칸·따옴표·역빗금이 있으면 따옴표로 감싼다)
fn exec_word(word: &str) -> String {
    if word.is_empty() || word.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\') {
        format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        word.to_string()
    }
}

fn render(
    description: &str,
    after: &str,
    program: &str,
    args: &[String],
    paths: &OvnPaths,
) -> String {
    let exec = std::iter::once(program.to_string())
        .chain(args.iter().map(|a| exec_word(a)))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{UNIT_MARK}\n\
         [Unit]\n\
         Description={description}\n\
         After={after}\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStartPre=/bin/mkdir -p {run} {log}\n\
         ExecStart={exec}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         LimitNOFILE=65535\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        run = exec_word(&paths.run.display().to_string()),
        log = exec_word(&paths.log.display().to_string()),
    )
}

/// DB `db`의 단위 파일. `locked`면(SB만) 설정 파일로 뜬다([`sb_locked_args`]).
pub fn db_unit(db: Db, address: IpAddr, locked: bool, paths: &OvnPaths) -> String {
    let args = if locked && db == Db::Sb {
        sb_locked_args(paths)
    } else {
        ovsdb_server_args(db, address, paths)
    };
    render(
        db.description(),
        "network-online.target",
        OVSDB_SERVER,
        &args,
        paths,
    )
}

/// northd 단위 파일(`locked`: SB가 잠금으로 뜬 중앙 — [`northd_args`])
pub fn northd_unit(centrals: &[IpAddr], locked: &[IpAddr], paths: &OvnPaths) -> String {
    render(
        "Orbivisor OVN northd",
        "network-online.target opl-ovn-nb.service opl-ovn-sb.service",
        OVN_NORTHD,
        &northd_args(centrals, locked, paths),
        paths,
    )
}

/// 단위 파일이 Orbivisor 것인지(첫 줄이 표시)
pub fn is_managed(text: &str) -> bool {
    text.lines().next() == Some(UNIT_MARK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn addresses() {
        assert_eq!(ssl_addr(ip("10.0.0.1"), 6642), "ssl:10.0.0.1:6642");
        assert_eq!(ssl_addr(ip("fd00::1"), 6642), "ssl:[fd00::1]:6642");
        assert_eq!(listen_addr(ip("10.0.0.1"), 6641), "pssl:6641:10.0.0.1");
        assert_eq!(listen_addr(ip("fd00::1"), 6641), "pssl:6641:[fd00::1]");
        assert_eq!(
            remotes(&[ip("10.0.0.1"), ip("10.0.0.2")], 6642),
            "ssl:10.0.0.1:6642,ssl:10.0.0.2:6642"
        );
        assert_eq!(Db::Nb.unit(), "opl-ovn-nb.service");
        assert_eq!((Db::Sb.port(), Db::Sb.raft_port()), (6642, 6644));
        assert_eq!(exec_word("a b"), "\"a b\"");
        assert_eq!(exec_word("a\"b"), "\"a\\\"b\"");
    }

    /// 단위 파일 내용은 고정이다(배포판 단위와 섞이지 않는 경로, 노드 주소에만 듣기, 노드 TLS 사본).
    #[test]
    fn unit_files() {
        let paths = OvnPaths::default();
        let nb = db_unit(Db::Nb, ip("10.0.0.1"), false, &paths);
        assert_eq!(
            nb,
            "# Managed by Orbivisor (orbivisord); changes are overwritten.\n\
             [Unit]\n\
             Description=Orbivisor OVN northbound database\n\
             After=network-online.target\n\
             Wants=network-online.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStartPre=/bin/mkdir -p /run/opl/ovn /var/log/opl\n\
             ExecStart=/usr/sbin/ovsdb-server /var/lib/opl/ovn/nb.db --remote=pssl:6641:10.0.0.1 \
             --remote=punix:/run/opl/ovn/nb.sock --private-key=/etc/opl/ovn/node.key \
             --certificate=/etc/opl/ovn/node.crt --ca-cert=/etc/opl/ovn/ca.crt \
             --unixctl=/run/opl/ovn/nb.ctl --pidfile=/run/opl/ovn/nb.pid -vconsole:emer -vsyslog:err \
             -vfile:info --log-file=/var/log/opl/ovn-nb.log --no-chdir\n\
             Restart=on-failure\n\
             RestartSec=2\n\
             LimitNOFILE=65535\n\
             \n\
             [Install]\n\
             WantedBy=multi-user.target\n"
        );
        assert!(is_managed(&nb));
        let sb = db_unit(Db::Sb, ip("fd00::1"), false, &paths);
        assert!(sb.contains(
            "ExecStart=/usr/sbin/ovsdb-server /var/lib/opl/ovn/sb.db --remote=pssl:6642:[fd00::1] \
             --remote=punix:/run/opl/ovn/sb.sock"
        ));
        assert!(sb.contains("--log-file=/var/log/opl/ovn-sb.log"));
        let northd = northd_unit(&[ip("10.0.0.1"), ip("10.0.0.2")], &[], &paths);
        assert!(
            northd.contains("After=network-online.target opl-ovn-nb.service opl-ovn-sb.service\n")
        );
        assert!(northd.contains(
            "ExecStart=/usr/bin/ovn-northd --ovnnb-db=ssl:10.0.0.1:6641,ssl:10.0.0.2:6641 \
             --ovnsb-db=ssl:10.0.0.1:6642,ssl:10.0.0.2:6642 --private-key=/etc/opl/ovn/node.key \
             --certificate=/etc/opl/ovn/node.crt --ca-cert=/etc/opl/ovn/ca.crt \
             --unixctl=/run/opl/ovn/northd.ctl --pidfile=/run/opl/ovn/northd.pid"
        ));
        assert!(!is_managed("[Unit]\nDescription=x\n"));
        // 잠금은 NB 단위를 바꾸지 않는다.
        assert_eq!(db_unit(Db::Nb, ip("10.0.0.1"), true, &paths), nb);
    }

    /// 잠금(계획 67 §10.2): SB는 설정 파일로, northd는 잠금이 켜진 중앙의 SB에 6645로 붙는다.
    #[test]
    fn locked_units() {
        let paths = OvnPaths::default();
        assert_eq!(
            sb_config(ip("10.0.0.1"), &paths),
            "{\n  \"databases\": {\n    \"/var/lib/opl/ovn/sb.db\": {}\n  },\n  \"remotes\": {\n    \
             \"pssl:6642:10.0.0.1\": {\n      \"role\": \"ovn-controller\"\n    },\n    \
             \"pssl:6645:10.0.0.1\": {},\n    \"punix:/run/opl/ovn/sb.sock\": {}\n  }\n}\n"
        );
        let v: serde_json::Value = serde_json::from_str(&sb_config(ip("fd00::1"), &paths)).unwrap();
        assert_eq!(
            v["remotes"]["pssl:6642:[fd00::1]"]["role"],
            "ovn-controller"
        );
        let sb = db_unit(Db::Sb, ip("10.0.0.1"), true, &paths);
        assert!(
            sb.contains(
                "ExecStart=/usr/sbin/ovsdb-server --config-file=/etc/opl/ovn/sb.json \
                 --private-key=/etc/opl/ovn/node.key --certificate=/etc/opl/ovn/node.crt \
                 --ca-cert=/etc/opl/ovn/ca.crt --unixctl=/run/opl/ovn/sb.ctl --pidfile=/run/opl/ovn/sb.pid \
                 -vconsole:emer -vsyslog:err -vfile:info --log-file=/var/log/opl/ovn-sb.log --no-chdir\n"
            ),
            "{sb}"
        );
        assert!(!sb.contains("--remote"), "{sb}");
        let northd = northd_unit(
            &[ip("10.0.0.1"), ip("10.0.0.2"), ip("10.0.0.3")],
            &[ip("10.0.0.2")],
            &paths,
        );
        assert!(northd.contains(
            "--ovnnb-db=ssl:10.0.0.1:6641,ssl:10.0.0.2:6641,ssl:10.0.0.3:6641 \
             --ovnsb-db=ssl:10.0.0.1:6642,ssl:10.0.0.2:6645,ssl:10.0.0.3:6642 "
        ));
    }
}
