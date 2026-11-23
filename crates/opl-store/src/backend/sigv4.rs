//! AWS 서명 버전 4(`AWS4-HMAC-SHA256`) — 헤더 방식만. 순수 함수이고 AWS 공개 시험 벡터로 확인한다.

use ring::{digest, hmac};

pub const ALGORITHM: &str = "AWS4-HMAC-SHA256";
/// 빈 본문의 SHA-256
pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// 서명할 요청
pub struct SignInput<'a> {
    pub method: &'a str,
    /// 이미 부호화한 경로(S3는 다시 부호화하지 않는다)
    pub canonical_uri: &'a str,
    /// 부호화하지 않은 질의 쌍
    pub query: &'a [(String, String)],
    /// 서명할 헤더(이름, 값) — `host`·`x-amz-date`를 반드시 담는다
    pub headers: &'a [(String, String)],
    /// 본문 SHA-256(16진)
    pub payload_hash: &'a str,
    /// `YYYYMMDDTHHMMSSZ`
    pub amz_date: &'a str,
    pub region: &'a str,
    pub service: &'a str,
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(digest::digest(&digest::SHA256, data))
}

/// RFC 3986 부호화(비예약 문자 `A-Za-z0-9-_.~`만 그대로, 나머지는 `%XX` 대문자). `encode_slash`가 거짓이면 `/`도 그대로.
pub fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 정규 질의 문자열(이름·값을 부호화하고 이름 차례, 같으면 값 차례)
pub fn canonical_query(query: &[(String, String)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (uri_encode(k, true), uri_encode(v, true)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn canonical_headers(headers: &[(String, String)]) -> (String, String) {
    let mut hs: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| {
            let v = v.split_whitespace().collect::<Vec<_>>().join(" ");
            (k.to_ascii_lowercase(), v)
        })
        .collect();
    hs.sort();
    let canon = hs
        .iter()
        .map(|(k, v)| format!("{k}:{v}\n"))
        .collect::<String>();
    let signed = hs
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    (canon, signed)
}

pub fn canonical_request(i: &SignInput<'_>) -> (String, String) {
    let (canon_headers, signed) = canonical_headers(i.headers);
    let req = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        i.method,
        i.canonical_uri,
        canonical_query(i.query),
        canon_headers,
        signed,
        i.payload_hash
    );
    (req, signed)
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), msg)
        .as_ref()
        .to_vec()
}

/// 서명(16진)과 서명한 헤더 목록
pub fn signature(i: &SignInput<'_>, secret: &str) -> (String, String) {
    let (req, signed) = canonical_request(i);
    let date = &i.amz_date[..8];
    let scope = format!("{date}/{}/{}/aws4_request", i.region, i.service);
    let to_sign = format!(
        "{ALGORITHM}\n{}\n{scope}\n{}",
        i.amz_date,
        sha256_hex(req.as_bytes())
    );
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, i.region.as_bytes());
    let k_service = hmac_sha256(&k_region, i.service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    (
        hex::encode(hmac_sha256(&k_signing, to_sign.as_bytes())),
        signed,
    )
}

/// `Authorization` 헤더 값
pub fn authorization(i: &SignInput<'_>, access_key_id: &str, secret: &str) -> String {
    let (sig, signed) = signature(i, secret);
    format!(
        "{ALGORITHM} Credential={access_key_id}/{}/{}/{}/aws4_request, SignedHeaders={signed}, Signature={sig}",
        &i.amz_date[..8],
        i.region,
        i.service
    )
}

/// `Authorization` 헤더를 푼다: (액세스 키, 날짜, 지역, 서비스, 서명한 헤더들, 서명)
pub fn parse_authorization(h: &str) -> Option<ParsedAuth> {
    let rest = h.strip_prefix(ALGORITHM)?.trim_start();
    let mut cred = None;
    let mut signed = None;
    let mut sig = None;
    for part in rest.split(',') {
        let (k, v) = part.trim().split_once('=')?;
        match k {
            "Credential" => cred = Some(v),
            "SignedHeaders" => signed = Some(v),
            "Signature" => sig = Some(v),
            _ => {}
        }
    }
    let mut c = cred?.split('/');
    let access_key_id = c.next()?.to_string();
    let date = c.next()?.to_string();
    let region = c.next()?.to_string();
    let service = c.next()?.to_string();
    (c.next()? == "aws4_request").then_some(())?;
    Some(ParsedAuth {
        access_key_id,
        date,
        region,
        service,
        signed_headers: signed?.split(';').map(str::to_string).collect(),
        signature: sig?.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAuth {
    pub access_key_id: String,
    pub date: String,
    pub region: String,
    pub service: String,
    pub signed_headers: Vec<String>,
    pub signature: String,
}

/// 유닉스 초 → `YYYYMMDDTHHMMSSZ`
pub fn amz_date(unix: u64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(unix as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        t.month() as u8,
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // AWS SigV4 시험 묶음(aws-sig-v4-test-suite)의 `get-vanilla`
    #[test]
    fn suite_get_vanilla() {
        let headers = h(&[
            ("Host", "example.amazonaws.com"),
            ("X-Amz-Date", "20150830T123600Z"),
        ]);
        let i = SignInput {
            method: "GET",
            canonical_uri: "/",
            query: &[],
            headers: &headers,
            payload_hash: EMPTY_SHA256,
            amz_date: "20150830T123600Z",
            region: "us-east-1",
            service: "service",
        };
        let (sig, signed) = signature(&i, "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY");
        assert_eq!(signed, "host;x-amz-date");
        assert_eq!(
            sig,
            "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    // `get-vanilla-query-order-key-case`: 질의 이름 차례
    #[test]
    fn suite_query_order() {
        let headers = h(&[
            ("Host", "example.amazonaws.com"),
            ("X-Amz-Date", "20150830T123600Z"),
        ]);
        let q = h(&[("Param2", "value2"), ("Param1", "value1")]);
        let i = SignInput {
            method: "GET",
            canonical_uri: "/",
            query: &q,
            headers: &headers,
            payload_hash: EMPTY_SHA256,
            amz_date: "20150830T123600Z",
            region: "us-east-1",
            service: "service",
        };
        let (sig, _) = signature(&i, "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY");
        assert_eq!(
            sig,
            "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500"
        );
    }

    const S3_SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    // S3 API 참고서 "Signature Calculations for the Authorization Header" 예: GET Object(Range)
    #[test]
    fn s3_get_object() {
        let headers = h(&[
            ("Host", "examplebucket.s3.amazonaws.com"),
            ("Range", "bytes=0-9"),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", "20130524T000000Z"),
        ]);
        let i = SignInput {
            method: "GET",
            canonical_uri: "/test.txt",
            query: &[],
            headers: &headers,
            payload_hash: EMPTY_SHA256,
            amz_date: "20130524T000000Z",
            region: "us-east-1",
            service: "s3",
        };
        assert_eq!(
            authorization(&i, "AKIAIOSFODNN7EXAMPLE", S3_SECRET),
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    // 같은 참고서의 PUT Object(`test$file.text`, 경로 부호화)
    #[test]
    fn s3_put_object() {
        let body = b"Welcome to Amazon S3.";
        let hash = sha256_hex(body);
        assert_eq!(
            hash,
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
        );
        let headers = h(&[
            ("Date", "Fri, 24 May 2013 00:00:00 GMT"),
            ("Host", "examplebucket.s3.amazonaws.com"),
            ("x-amz-content-sha256", &hash),
            ("x-amz-date", "20130524T000000Z"),
            ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
        ]);
        let uri = uri_encode("/test$file.text", false);
        assert_eq!(uri, "/test%24file.text");
        let i = SignInput {
            method: "PUT",
            canonical_uri: &uri,
            query: &[],
            headers: &headers,
            payload_hash: &hash,
            amz_date: "20130524T000000Z",
            region: "us-east-1",
            service: "s3",
        };
        let (sig, _) = signature(&i, S3_SECRET);
        assert_eq!(
            sig,
            "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
        );
    }

    // 같은 참고서의 GET Bucket(List Objects) — 질의 `max-keys=2&prefix=J`
    #[test]
    fn s3_list_objects() {
        let headers = h(&[
            ("Host", "examplebucket.s3.amazonaws.com"),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", "20130524T000000Z"),
        ]);
        let q = h(&[("prefix", "J"), ("max-keys", "2")]);
        let i = SignInput {
            method: "GET",
            canonical_uri: "/",
            query: &q,
            headers: &headers,
            payload_hash: EMPTY_SHA256,
            amz_date: "20130524T000000Z",
            region: "us-east-1",
            service: "s3",
        };
        let (sig, _) = signature(&i, S3_SECRET);
        assert_eq!(
            sig,
            "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
        );
    }

    #[test]
    fn encoding_and_parse() {
        assert_eq!(uri_encode("a b/c~d", true), "a%20b%2Fc~d");
        assert_eq!(uri_encode("ab:cd/é", false), "ab%3Acd/%C3%A9");
        assert_eq!(
            canonical_query(&h(&[("b", "2"), ("a", "x y"), ("a", "1")])),
            "a=1&a=x%20y&b=2"
        );
        assert_eq!(amz_date(1_369_353_600), "20130524T000000Z");
        let p = parse_authorization(
            "AWS4-HMAC-SHA256 Credential=AK/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-date, Signature=ab",
        )
        .unwrap();
        assert_eq!(p.access_key_id, "AK");
        assert_eq!(p.signed_headers, vec!["host", "x-amz-date"]);
        assert_eq!(p.signature, "ab");
        assert!(parse_authorization("Bearer x").is_none());
    }
}
