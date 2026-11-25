//! SHA-512 crypt(`$6$`, Ulrich Drepper의 "Unix crypt using SHA-256 and SHA-512" 명세). cloud-init 비밀번호를
//! 평문 대신 이 해시로 저장하고 게스트의 `chpasswd -e`가 그대로 넣는다(계획 57).

use ring::digest::{Context, SHA512};

/// 솔트에 쓰는 문자(crypt의 base64 알파벳)
pub const CRYPT_SALT_CHARS: &[u8; 64] =
    b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// 기본·최소·최대 반복 수와 솔트 최대 길이
const DEFAULT_ROUNDS: u32 = 5000;
const MIN_ROUNDS: u32 = 1000;
const MAX_ROUNDS: u32 = 999_999_999;
const MAX_SALT: usize = 16;

/// 해시 바이트를 base64로 적는 순서(명세의 표, 세 바이트씩)
const ORDER: [(usize, usize, usize); 21] = [
    (0, 21, 42),
    (22, 43, 1),
    (44, 2, 23),
    (3, 24, 45),
    (25, 46, 4),
    (47, 5, 26),
    (6, 27, 48),
    (28, 49, 7),
    (50, 8, 29),
    (9, 30, 51),
    (31, 52, 10),
    (53, 11, 32),
    (12, 33, 54),
    (34, 55, 13),
    (56, 14, 35),
    (15, 36, 57),
    (37, 58, 16),
    (59, 17, 38),
    (18, 39, 60),
    (40, 61, 19),
    (62, 20, 41),
];

/// `password`의 SHA-512 crypt 해시 `$6$[rounds=N$]솔트$해시`. `salt`는 `$6$` 없이 솔트(앞에 `rounds=N$`가 있으면
/// 그 반복 수, 1000~999999999로 맞춤)이고 첫 `$` 앞의 16자까지만 쓴다.
pub fn sha512_crypt(password: &[u8], salt: &str) -> String {
    let (rounds, salt) = match salt
        .strip_prefix("rounds=")
        .and_then(|s| s.split_once('$'))
        .and_then(|(n, rest)| Some((n.parse::<u64>().ok()?, rest)))
    {
        Some((n, rest)) => (
            Some(n.clamp(MIN_ROUNDS.into(), MAX_ROUNDS.into()) as u32),
            rest,
        ),
        None => (None, salt),
    };
    let salt = salt.split('$').next().unwrap_or_default();
    let salt = &salt.as_bytes()[..salt.len().min(MAX_SALT)];
    let hash = digest(password, salt, rounds.unwrap_or(DEFAULT_ROUNDS));

    let mut out = String::from("$6$");
    if let Some(n) = rounds {
        out.push_str(&format!("rounds={n}$"));
    }
    out.push_str(&String::from_utf8_lossy(salt));
    out.push('$');
    for (a, b, c) in ORDER {
        encode(&mut out, (hash[a], hash[b], hash[c]), 4);
    }
    encode(&mut out, (0, 0, hash[63]), 2);
    out
}

/// 명세 그대로의 64바이트 해시
fn digest(key: &[u8], salt: &[u8], rounds: u32) -> [u8; 64] {
    let sha = |parts: &[&[u8]]| {
        let mut c = Context::new(&SHA512);
        for p in parts {
            c.update(p);
        }
        c.finish()
    };
    // B = SHA512(키 + 솔트 + 키)
    let b = sha(&[key, salt, key]);
    let b = b.as_ref();
    // A = 키 + 솔트 + 키 길이만큼의 B + 키 길이의 비트마다 B 또는 키
    let mut a = Context::new(&SHA512);
    a.update(key);
    a.update(salt);
    let mut n = key.len();
    while n > 64 {
        a.update(b);
        n -= 64;
    }
    a.update(&b[..n]);
    let mut n = key.len();
    while n > 0 {
        a.update(if n & 1 == 1 { b } else { key });
        n >>= 1;
    }
    let mut prev: Vec<u8> = a.finish().as_ref().to_vec();
    // P = SHA512(키를 키 길이번)을 키 길이로, S = SHA512(솔트를 16 + A[0]번)을 솔트 길이로
    let stretch = |d: &[u8], len: usize| d.iter().cycle().take(len).copied().collect::<Vec<u8>>();
    let dp = sha(&vec![key; key.len()]);
    let p = stretch(dp.as_ref(), key.len());
    let ds = sha(&vec![salt; 16 + usize::from(prev[0])]);
    let s = stretch(ds.as_ref(), salt.len());
    for i in 0..rounds {
        let mut c = Context::new(&SHA512);
        c.update(if i & 1 == 1 { &p } else { &prev });
        if i % 3 != 0 {
            c.update(&s);
        }
        if i % 7 != 0 {
            c.update(&p);
        }
        c.update(if i & 1 == 1 { &prev } else { &p });
        prev = c.finish().as_ref().to_vec();
    }
    prev.try_into().expect("SHA-512 is 64 bytes")
}

/// 세 바이트(큰 것부터)를 `n`자 base64로(작은 6비트부터)
fn encode(out: &mut String, (b2, b1, b0): (u8, u8, u8), n: usize) {
    let mut w = (u32::from(b2) << 16) | (u32::from(b1) << 8) | u32::from(b0);
    for _ in 0..n {
        out.push(char::from(CRYPT_SALT_CHARS[(w & 0x3f) as usize]));
        w >>= 6;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_init::is_crypt_hash;

    #[test]
    fn matches_the_specification_test_vectors() {
        for (salt, key, want) in [
            (
                "saltstring",
                "Hello world!",
                "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1",
            ),
            (
                "rounds=10000$saltstringsaltstring",
                "Hello world!",
                "$6$rounds=10000$saltstringsaltst$OW1/O6BYHV6BcXZu8QVeXbDWra3Oeqh0sbHbbMCVNSnCM/UrjmM0Dp8vOuZeHBy/YTBmSK6H9qs/y3RnOaw5v.",
            ),
            (
                "rounds=5000$toolongsaltstring",
                "This is just a test",
                "$6$rounds=5000$toolongsaltstrin$lQ8jolhgVRVhY4b5pZKaysCLi0QBxGoNeKQzQ3glMhwllF7oGDZxUhx1yxdYcz/e1JSbq3y6JMxxl8audkUEm0",
            ),
            (
                "rounds=1400$anotherlongsaltstring",
                "a very much longer text to encrypt.  This one even stretches over morethan one line.",
                "$6$rounds=1400$anotherlongsalts$POfYwTEok97VWcjxIiSOjiykti.o/pQs.wPvMxQ6Fm7I6IoYN3CmLs66x9t0oSwbtEW7o7UmJEiDwGqd8p4ur1",
            ),
            // 1000보다 적은 반복 수는 1000으로 올린다.
            (
                "rounds=10$roundstoolow",
                "the minimum number is still observed",
                "$6$rounds=1000$roundstoolow$kUMsbe306n21p9R.FRkW3IGn.S9NPN0x50YhH1xhLsPuWGsUSklZt58jaTfF4ZEQpyUNGc0dqbpBYYBaHHrsX.",
            ),
        ] {
            let got = sha512_crypt(key.as_bytes(), salt);
            assert_eq!(got, want, "{salt}");
            assert!(is_crypt_hash(&got));
        }
    }

    #[test]
    fn salt_stops_at_a_dollar_sign() {
        assert_eq!(
            sha512_crypt(b"Hello world!", "saltstring$ignored"),
            sha512_crypt(b"Hello world!", "saltstring")
        );
    }

    /// 다른 구현(`openssl passwd -6`)과 무작위 비밀번호·솔트 다섯 개를 견준다. openssl이 없으면 건너뛴다.
    #[test]
    fn matches_openssl_on_random_inputs() {
        let rng = ring::rand::SystemRandom::new();
        for i in 0..5 {
            let mut bytes = [0u8; 40];
            ring::rand::SecureRandom::fill(&rng, &mut bytes).unwrap();
            let pick = |b: &[u8]| -> String {
                b.iter()
                    .map(|&x| char::from(CRYPT_SALT_CHARS[usize::from(x) % 64]))
                    .collect()
            };
            let salt = pick(&bytes[..16]);
            let password = pick(&bytes[16..16 + 4 + i * 4]);
            let Ok(out) = std::process::Command::new("openssl")
                .args(["passwd", "-6", "-salt", &salt, &password])
                .output()
            else {
                eprintln!("openssl not found; skipping");
                return;
            };
            if !out.status.success() {
                eprintln!("openssl passwd -6 failed; skipping");
                return;
            }
            let want = String::from_utf8(out.stdout).unwrap();
            assert_eq!(sha512_crypt(password.as_bytes(), &salt), want.trim());
        }
    }
}
