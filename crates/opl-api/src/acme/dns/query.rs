//! 손으로 짠 최소 DNS(RFC 1035): 질의 부호화, 응답 풀기(이름 압축, A·AAAA·NS·CNAME·SOA·TXT), UDP(잘리면 TCP) 주고받기,
//! 영역 찾기(SOA), 권한 서버(NS)에 직접 TXT 묻기. RFC 2136 UPDATE([`super::rfc2136`])가 부호화를 함께 쓴다.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const TYPE_A: u16 = 1;
pub const TYPE_NS: u16 = 2;
pub const TYPE_CNAME: u16 = 5;
pub const TYPE_SOA: u16 = 6;
pub const TYPE_TXT: u16 = 16;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_ANY: u16 = 255;
pub const CLASS_IN: u16 = 1;
pub const CLASS_NONE: u16 = 254;
pub const CLASS_ANY: u16 = 255;

/// 질의 하나의 시간 한도
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RData {
    A(IpAddr),
    Name(String),
    Soa { mname: String },
    Txt(Vec<String>),
    Other(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub rtype: u16,
    pub class: u16,
    pub ttl: u32,
    pub data: RData,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Message {
    pub id: u16,
    pub flags: u16,
    pub answers: Vec<Record>,
    pub authority: Vec<Record>,
    pub additional: Vec<Record>,
}

impl Message {
    pub fn rcode(&self) -> u16 {
        self.flags & 0x000f
    }
    pub fn truncated(&self) -> bool {
        self.flags & 0x0200 != 0
    }
}

/// 이름을 낮춰 끝 점을 뗀다.
pub fn normalize(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// 압축 없는 이름(레이블 63, 전체 255)
pub fn write_name(buf: &mut Vec<u8>, name: &str) -> Result<(), String> {
    let name = name.trim_end_matches('.');
    if name.len() > 253 {
        return Err(format!("DNS name too long: {name}"));
    }
    if !name.is_empty() {
        for label in name.split('.') {
            if label.is_empty() || label.len() > 63 {
                return Err(format!("bad DNS name: {name}"));
            }
            buf.push(label.len() as u8);
            buf.extend_from_slice(label.as_bytes());
        }
    }
    buf.push(0);
    Ok(())
}

/// 머리(id, 깃발, 섹션 수 넷)
pub fn header(buf: &mut Vec<u8>, id: u16, flags: u16, counts: [u16; 4]) {
    buf.extend_from_slice(&id.to_be_bytes());
    buf.extend_from_slice(&flags.to_be_bytes());
    for c in counts {
        buf.extend_from_slice(&c.to_be_bytes());
    }
}

/// 재귀를 바라는(`rd`) 질의 하나
pub fn encode_query(id: u16, name: &str, qtype: u16, recursion: bool) -> Result<Vec<u8>, String> {
    let mut buf = Vec::with_capacity(64);
    header(
        &mut buf,
        id,
        if recursion { 0x0100 } else { 0 },
        [1, 0, 0, 0],
    );
    write_name(&mut buf, name)?;
    buf.extend_from_slice(&qtype.to_be_bytes());
    buf.extend_from_slice(&CLASS_IN.to_be_bytes());
    Ok(buf)
}

fn read_u16(b: &[u8], at: usize) -> Result<u16, String> {
    b.get(at..at + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
        .ok_or_else(|| "short DNS message".to_string())
}

fn read_u32(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| "short DNS message".to_string())
}

/// 압축된 이름을 읽는다. (이름, 다음 위치)
pub fn read_name(b: &[u8], mut at: usize) -> Result<(String, usize), String> {
    let mut labels: Vec<String> = Vec::new();
    let mut next = None;
    let mut jumps = 0;
    loop {
        let len = *b.get(at).ok_or("short DNS name")? as usize;
        if len & 0xc0 == 0xc0 {
            let ptr = (read_u16(b, at)? & 0x3fff) as usize;
            next.get_or_insert(at + 2);
            jumps += 1;
            if jumps > 32 {
                return Err("DNS name pointer loop".into());
            }
            at = ptr;
            continue;
        }
        if len == 0 {
            let end = next.unwrap_or(at + 1);
            return Ok((labels.join("."), end));
        }
        let label = b.get(at + 1..at + 1 + len).ok_or("short DNS label")?;
        labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
        at += 1 + len;
    }
}

fn read_record(b: &[u8], at: usize) -> Result<(Record, usize), String> {
    let (name, at) = read_name(b, at)?;
    let rtype = read_u16(b, at)?;
    let class = read_u16(b, at + 2)?;
    let ttl = read_u32(b, at + 4)?;
    let len = read_u16(b, at + 8)? as usize;
    let start = at + 10;
    let raw = b.get(start..start + len).ok_or("short DNS record")?;
    let data = match rtype {
        TYPE_A if len == 4 => RData::A(IpAddr::from(<[u8; 4]>::try_from(raw).expect("len"))),
        TYPE_AAAA if len == 16 => RData::A(IpAddr::from(<[u8; 16]>::try_from(raw).expect("len"))),
        TYPE_NS | TYPE_CNAME => RData::Name(read_name(b, start)?.0),
        TYPE_SOA => RData::Soa {
            mname: read_name(b, start)?.0,
        },
        TYPE_TXT => {
            let mut parts = Vec::new();
            let mut i = 0;
            while i < raw.len() {
                let l = raw[i] as usize;
                let s = raw.get(i + 1..i + 1 + l).ok_or("short TXT")?;
                parts.push(String::from_utf8_lossy(s).into_owned());
                i += 1 + l;
            }
            RData::Txt(parts)
        }
        _ => RData::Other(raw.to_vec()),
    };
    Ok((
        Record {
            name,
            rtype,
            class,
            ttl,
            data,
        },
        start + len,
    ))
}

/// 응답을 푼다(질문 섹션은 건너뛴다).
pub fn decode(b: &[u8]) -> Result<Message, String> {
    let id = read_u16(b, 0)?;
    let flags = read_u16(b, 2)?;
    let counts = [
        read_u16(b, 4)?,
        read_u16(b, 6)?,
        read_u16(b, 8)?,
        read_u16(b, 10)?,
    ];
    let mut at = 12;
    for _ in 0..counts[0] {
        at = read_name(b, at)?.1 + 4;
    }
    let mut sections: [Vec<Record>; 3] = Default::default();
    for (i, section) in sections.iter_mut().enumerate() {
        for _ in 0..counts[i + 1] {
            let (r, next) = read_record(b, at)?;
            section.push(r);
            at = next;
        }
    }
    let [answers, authority, additional] = sections;
    Ok(Message {
        id,
        flags,
        answers,
        authority,
        additional,
    })
}

fn random_id() -> u16 {
    let mut b = [0u8; 2];
    getrandom::fill(&mut b).expect("system random source");
    u16::from_be_bytes(b)
}

/// TCP로 주고받는다(2바이트 길이 앞붙임).
pub async fn exchange_tcp(server: SocketAddr, msg: &[u8]) -> Result<Vec<u8>, String> {
    let run = async {
        let mut s = tokio::net::TcpStream::connect(server)
            .await
            .map_err(|e| format!("{server}: {e}"))?;
        let mut out = (msg.len() as u16).to_be_bytes().to_vec();
        out.extend_from_slice(msg);
        s.write_all(&out)
            .await
            .map_err(|e| format!("{server}: {e}"))?;
        let mut len = [0u8; 2];
        s.read_exact(&mut len)
            .await
            .map_err(|e| format!("{server}: {e}"))?;
        let mut reply = vec![0u8; u16::from_be_bytes(len) as usize];
        s.read_exact(&mut reply)
            .await
            .map_err(|e| format!("{server}: {e}"))?;
        Ok(reply)
    };
    tokio::time::timeout(QUERY_TIMEOUT, run)
        .await
        .map_err(|_| format!("{server}: DNS query timed out"))?
}

/// UDP로 묻고, 잘렸으면 TCP로 다시 묻는다.
pub async fn exchange(server: SocketAddr, msg: &[u8]) -> Result<Message, String> {
    let bind: SocketAddr = if server.is_ipv4() {
        "0.0.0.0:0".parse().expect("addr")
    } else {
        "[::]:0".parse().expect("addr")
    };
    let id = read_u16(msg, 0)?;
    let udp = async {
        let sock = tokio::net::UdpSocket::bind(bind)
            .await
            .map_err(|e| e.to_string())?;
        sock.send_to(msg, server)
            .await
            .map_err(|e| format!("{server}: {e}"))?;
        let mut buf = vec![0u8; 4096];
        loop {
            let (n, from) = sock
                .recv_from(&mut buf)
                .await
                .map_err(|e| format!("{server}: {e}"))?;
            if from == server && n >= 2 && read_u16(&buf, 0)? == id {
                return Ok::<_, String>(buf[..n].to_vec());
            }
        }
    };
    let reply = tokio::time::timeout(QUERY_TIMEOUT, udp)
        .await
        .map_err(|_| format!("{server}: DNS query timed out"))??;
    let message = decode(&reply)?;
    if message.truncated() {
        return decode(&exchange_tcp(server, msg).await?);
    }
    Ok(message)
}

/// 질의 하나
pub async fn query(
    server: SocketAddr,
    name: &str,
    qtype: u16,
    recursion: bool,
) -> Result<Message, String> {
    exchange(server, &encode_query(random_id(), name, qtype, recursion)?).await
}

/// 시스템 재귀 서버(`/etc/resolv.conf`의 첫 `nameserver`, 없으면 127.0.0.1)
pub fn system_resolver() -> SocketAddr {
    std::fs::read_to_string("/etc/resolv.conf")
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                let ip = l.trim().strip_prefix("nameserver")?.trim();
                ip.split('%').next()?.parse::<IpAddr>().ok()
            })
        })
        .map(|ip| SocketAddr::new(ip, 53))
        .unwrap_or_else(|| "127.0.0.1:53".parse().expect("addr"))
}

/// `name`이 든 영역(SOA의 주인 이름 — 답 또는 권한 섹션)
pub async fn find_zone(resolver: SocketAddr, name: &str) -> Result<String, String> {
    let m = query(resolver, name, TYPE_SOA, true).await?;
    m.answers
        .iter()
        .chain(&m.authority)
        .find(|r| r.rtype == TYPE_SOA)
        .map(|r| r.name.clone())
        .ok_or_else(|| format!("cannot find the DNS zone of {name}"))
}

/// `name`의 CNAME 대상(위임 — `_acme-challenge.x CNAME x.acme.example.net`). 없으면 그대로.
pub async fn follow_cname(resolver: SocketAddr, name: &str) -> String {
    let mut name = normalize(name);
    for _ in 0..8 {
        let Ok(m) = query(resolver, &name, TYPE_CNAME, true).await else {
            break;
        };
        match m
            .answers
            .iter()
            .find(|r| r.rtype == TYPE_CNAME && r.name == name)
        {
            Some(Record {
                data: RData::Name(target),
                ..
            }) => name = normalize(target),
            _ => break,
        }
    }
    name
}

/// 영역의 권한 서버 주소(NS → A/AAAA, 53번)
pub async fn authoritative_servers(
    resolver: SocketAddr,
    zone: &str,
) -> Result<Vec<SocketAddr>, String> {
    let m = query(resolver, zone, TYPE_NS, true).await?;
    let mut out = Vec::new();
    for ns in m.answers.iter().filter(|r| r.rtype == TYPE_NS) {
        let RData::Name(host) = &ns.data else {
            continue;
        };
        if let Ok(ip) = host.parse::<IpAddr>() {
            out.push(SocketAddr::new(ip, 53));
            continue;
        }
        let glue: Vec<IpAddr> = m
            .additional
            .iter()
            .filter(|r| r.name == *host)
            .filter_map(|r| match r.data {
                RData::A(ip) => Some(ip),
                _ => None,
            })
            .collect();
        if !glue.is_empty() {
            out.extend(glue.into_iter().map(|ip| SocketAddr::new(ip, 53)));
            continue;
        }
        if let Ok(a) = query(resolver, host, TYPE_A, true).await {
            out.extend(a.answers.iter().filter_map(|r| match r.data {
                RData::A(ip) => Some(SocketAddr::new(ip, 53)),
                _ => None,
            }));
        }
    }
    if out.is_empty() {
        return Err(format!("cannot find the name servers of {zone}"));
    }
    Ok(out)
}

/// `server`가 `fqdn`의 TXT로 `value`를 답하나(재귀 없이)
pub async fn txt_present(server: SocketAddr, fqdn: &str, value: &str) -> Result<bool, String> {
    let m = query(server, fqdn, TYPE_TXT, false).await?;
    Ok(m.answers.iter().any(|r| match &r.data {
        RData::Txt(parts) => parts.concat() == value,
        _ => false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 손으로 만든 응답: 질문 하나, 답 TXT(압축 이름), 권한 SOA
    fn reply() -> Vec<u8> {
        let mut b = Vec::new();
        header(&mut b, 0x1234, 0x8180, [1, 1, 1, 0]);
        write_name(&mut b, "_acme-challenge.www.example.com").unwrap();
        b.extend_from_slice(&TYPE_TXT.to_be_bytes());
        b.extend_from_slice(&CLASS_IN.to_be_bytes());
        // 답: 이름은 질문(12)을 가리킨다.
        b.extend_from_slice(&[0xc0, 12]);
        b.extend_from_slice(&TYPE_TXT.to_be_bytes());
        b.extend_from_slice(&CLASS_IN.to_be_bytes());
        b.extend_from_slice(&60u32.to_be_bytes());
        let txt = [&[3u8][..], b"abc", &[2], b"de"].concat();
        b.extend_from_slice(&(txt.len() as u16).to_be_bytes());
        b.extend_from_slice(&txt);
        // 권한: SOA, 주인 "example.com"은 질문 안 위치를 가리킨다(12 + 1 + 15 + 1 + 3 + 1 = "example" 앞).
        let zone_at = 12 + 1 + "_acme-challenge".len() + 1 + "www".len();
        b.extend_from_slice(&[0xc0, zone_at as u8]);
        b.extend_from_slice(&TYPE_SOA.to_be_bytes());
        b.extend_from_slice(&CLASS_IN.to_be_bytes());
        b.extend_from_slice(&300u32.to_be_bytes());
        let mut soa = Vec::new();
        write_name(&mut soa, "ns1.example.com").unwrap();
        write_name(&mut soa, "hostmaster.example.com").unwrap();
        soa.extend_from_slice(&[0; 20]);
        b.extend_from_slice(&(soa.len() as u16).to_be_bytes());
        b.extend_from_slice(&soa);
        b
    }

    #[test]
    fn decodes_compressed_answers() {
        let m = decode(&reply()).unwrap();
        assert_eq!(m.id, 0x1234);
        assert_eq!(m.rcode(), 0);
        assert_eq!(m.answers.len(), 1);
        assert_eq!(m.answers[0].name, "_acme-challenge.www.example.com");
        assert_eq!(
            m.answers[0].data,
            RData::Txt(vec!["abc".into(), "de".into()])
        );
        assert_eq!(m.authority[0].name, "example.com");
        assert_eq!(
            m.authority[0].data,
            RData::Soa {
                mname: "ns1.example.com".into()
            }
        );
    }

    #[test]
    fn encodes_a_query() {
        let q = encode_query(7, "a.b.", TYPE_TXT, true).unwrap();
        assert_eq!(
            q,
            [
                0, 7, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, b'a', 1, b'b', 0, 0, 16, 0, 1
            ]
        );
        assert!(write_name(&mut Vec::new(), "a..b").is_err());
        assert!(write_name(&mut Vec::new(), &"x".repeat(64)).is_err());
        // 고리 포인터
        let mut loopy = Vec::new();
        header(&mut loopy, 1, 0x8000, [0, 1, 0, 0]);
        loopy.extend_from_slice(&[0xc0, 12]);
        assert!(decode(&loopy).is_err());
    }

    #[tokio::test]
    async fn udp_exchange_with_a_fake_server() {
        let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let (_, from) = sock.recv_from(&mut buf).await.unwrap();
            let mut r = reply();
            r[0] = buf[0];
            r[1] = buf[1];
            sock.send_to(&r, from).await.unwrap();
        });
        assert!(
            txt_present(addr, "_acme-challenge.www.example.com", "abcde")
                .await
                .unwrap()
        );
    }
}
