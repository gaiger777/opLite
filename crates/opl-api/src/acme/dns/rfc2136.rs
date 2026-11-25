//! DNS UPDATE(RFC 2136) + TSIG(RFC 8945, HMAC-SHA256/512 — ring). TCP로 보낸다. 영역이 없으면 SOA 질의로 찾는다.

use std::net::SocketAddr;

use opl_types::acme::TsigAlgorithm;

use super::query::{self, CLASS_ANY, CLASS_IN, CLASS_NONE, TYPE_SOA, TYPE_TXT};

const TYPE_TSIG: u16 = 250;
const OPCODE_UPDATE: u16 = 5 << 11;
/// TSIG 시각 허용 오차(초)
const FUDGE: u16 = 300;
/// 넣는 TXT의 TTL
const TTL: u32 = 60;

fn algorithm_name(a: TsigAlgorithm) -> &'static str {
    match a {
        TsigAlgorithm::HmacSha256 => "hmac-sha256",
        TsigAlgorithm::HmacSha512 => "hmac-sha512",
    }
}

fn txt_rdata(value: &str) -> Vec<u8> {
    // ACME 값은 43자라 문자열 하나에 들어간다.
    let mut out = Vec::with_capacity(value.len() + 1);
    for chunk in value.as_bytes().chunks(255) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out
}

fn rr(
    buf: &mut Vec<u8>,
    name: &str,
    rtype: u16,
    class: u16,
    ttl: u32,
    rdata: &[u8],
) -> Result<(), String> {
    query::write_name(buf, name)?;
    buf.extend_from_slice(&rtype.to_be_bytes());
    buf.extend_from_slice(&class.to_be_bytes());
    buf.extend_from_slice(&ttl.to_be_bytes());
    buf.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    buf.extend_from_slice(rdata);
    Ok(())
}

/// TSIG를 단 UPDATE 메시지. `add`면 TXT를 넣고, 아니면 그 값의 TXT 하나만 지운다.
#[allow(clippy::too_many_arguments)]
pub fn update_message(
    id: u16,
    zone: &str,
    fqdn: &str,
    value: &str,
    add: bool,
    key_name: &str,
    algorithm: TsigAlgorithm,
    secret: &[u8],
    time_signed: u64,
) -> Result<Vec<u8>, String> {
    let mut msg = Vec::with_capacity(256);
    query::header(&mut msg, id, OPCODE_UPDATE, [1, 0, 1, 0]);
    query::write_name(&mut msg, zone)?;
    msg.extend_from_slice(&TYPE_SOA.to_be_bytes());
    msg.extend_from_slice(&CLASS_IN.to_be_bytes());
    let rdata = txt_rdata(value);
    if add {
        rr(&mut msg, fqdn, TYPE_TXT, CLASS_IN, TTL, &rdata)?;
    } else {
        rr(&mut msg, fqdn, TYPE_TXT, CLASS_NONE, 0, &rdata)?;
    }
    // TSIG 변수(RFC 8945 §4.3.3): 열쇠 이름·CLASS ANY·TTL 0·알고리즘·시각·오차·오류·기타 길이
    let key_name = query::normalize(key_name);
    let alg = algorithm_name(algorithm);
    let mut vars = Vec::new();
    query::write_name(&mut vars, &key_name)?;
    vars.extend_from_slice(&CLASS_ANY.to_be_bytes());
    vars.extend_from_slice(&0u32.to_be_bytes());
    query::write_name(&mut vars, alg)?;
    let time = &time_signed.to_be_bytes()[2..];
    vars.extend_from_slice(time);
    vars.extend_from_slice(&FUDGE.to_be_bytes());
    vars.extend_from_slice(&0u16.to_be_bytes());
    vars.extend_from_slice(&0u16.to_be_bytes());
    let hmac_alg = match algorithm {
        TsigAlgorithm::HmacSha256 => ring::hmac::HMAC_SHA256,
        TsigAlgorithm::HmacSha512 => ring::hmac::HMAC_SHA512,
    };
    let key = ring::hmac::Key::new(hmac_alg, secret);
    let mut ctx = ring::hmac::Context::with_key(&key);
    ctx.update(&msg);
    ctx.update(&vars);
    let mac = ctx.sign();
    let mac = mac.as_ref();
    // TSIG RR
    let mut tsig = Vec::new();
    query::write_name(&mut tsig, alg)?;
    tsig.extend_from_slice(time);
    tsig.extend_from_slice(&FUDGE.to_be_bytes());
    tsig.extend_from_slice(&(mac.len() as u16).to_be_bytes());
    tsig.extend_from_slice(mac);
    tsig.extend_from_slice(&id.to_be_bytes());
    tsig.extend_from_slice(&0u16.to_be_bytes());
    tsig.extend_from_slice(&0u16.to_be_bytes());
    rr(&mut msg, &key_name, TYPE_TSIG, CLASS_ANY, 0, &tsig)?;
    // ARCOUNT = 1
    msg[10..12].copy_from_slice(&1u16.to_be_bytes());
    Ok(msg)
}

fn rcode_name(rcode: u16) -> &'static str {
    match rcode {
        1 => "FORMERR",
        2 => "SERVFAIL",
        3 => "NXDOMAIN",
        4 => "NOTIMP",
        5 => "REFUSED",
        9 => "NOTAUTH (check the TSIG key and its update policy)",
        10 => "NOTZONE",
        _ => "error",
    }
}

/// RFC 2136 공급자
pub struct Rfc2136 {
    pub server: SocketAddr,
    pub key_name: String,
    pub algorithm: TsigAlgorithm,
    pub secret: Vec<u8>,
    pub zone: Option<String>,
}

impl Rfc2136 {
    async fn zone_of(&self, fqdn: &str) -> Result<String, String> {
        match &self.zone {
            Some(z) => Ok(query::normalize(z)),
            // 그 서버가 영역의 권한 서버이므로 그 서버에 SOA를 묻는다.
            None => query::find_zone(self.server, fqdn).await,
        }
    }

    pub async fn send(&self, fqdn: &str, value: &str, add: bool) -> Result<(), String> {
        let zone = self.zone_of(fqdn).await?;
        let mut id = [0u8; 2];
        getrandom::fill(&mut id).expect("system random source");
        let msg = update_message(
            u16::from_be_bytes(id),
            &zone,
            fqdn,
            value,
            add,
            &self.key_name,
            self.algorithm,
            &self.secret,
            crate::backups::now(),
        )?;
        let reply = query::decode(&query::exchange_tcp(self.server, &msg).await?)?;
        match reply.rcode() {
            0 => Ok(()),
            r => Err(format!(
                "DNS UPDATE of {fqdn} at {}: {}",
                self.server,
                rcode_name(r)
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_message_layout_and_mac() {
        let secret = b"0123456789abcdef";
        let msg = update_message(
            0x0102,
            "example.com",
            "_acme-challenge.www.example.com",
            "value",
            true,
            "Acme-Key.",
            TsigAlgorithm::HmacSha256,
            secret,
            0x0000_6000_0000,
        )
        .unwrap();
        // 머리: id, UPDATE, ZO 1, PR 0, UP 1, AD 1
        assert_eq!(&msg[..12], &[1, 2, 0x28, 0, 0, 1, 0, 0, 0, 1, 0, 1]);
        // TSIG 앞까지 + 변수로 MAC를 다시 셈해 같은지 본다(ARCOUNT 0인 원래 메시지).
        let tsig_at =
            msg.len() - ("acme-key".len() + 2 + 10 + "hmac-sha256".len() + 2 + 6 + 2 + 2 + 32 + 6);
        let mut original = msg[..tsig_at].to_vec();
        original[10..12].copy_from_slice(&[0, 0]);
        let mut vars = Vec::new();
        query::write_name(&mut vars, "acme-key").unwrap();
        vars.extend_from_slice(&[0, 255, 0, 0, 0, 0]);
        query::write_name(&mut vars, "hmac-sha256").unwrap();
        vars.extend_from_slice(&[0, 0, 0x60, 0, 0, 0, 1, 44, 0, 0, 0, 0]);
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret);
        let want = ring::hmac::sign(&key, &[original, vars].concat());
        let mac_at = msg.len() - 6 - 32;
        assert_eq!(&msg[mac_at..mac_at + 32], want.as_ref());
        // 지우기는 CLASS NONE·TTL 0
        let del = update_message(
            1,
            "example.com",
            "x.example.com",
            "v",
            false,
            "k",
            TsigAlgorithm::HmacSha512,
            secret,
            1,
        )
        .unwrap();
        let up = 12 + 13 + 4 + 15;
        assert_eq!(&del[up..up + 8], &[0, 16, 0, 254, 0, 0, 0, 0]);
    }
}
