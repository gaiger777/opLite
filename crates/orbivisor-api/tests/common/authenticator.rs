//! 시험용 소프트웨어 WebAuthn 인증기(계획 62 설계 §14). ring 키로 `attestationObject`(형식 `none`)·
//! `authenticatorData`·`clientDataJSON`·서명을 만든다. 알고리즘은 ES256(시험마다 새 키)·EdDSA(새 키)·RS256(저장소
//! 견본 `tests/fixtures/oidc-rsa.pk8` — ring은 RSA 키를 만들지 못한다). 시험이 RP ID·origin·challenge·서명 횟수·
//! 플래그를 바꿔 틀린 응답도 만든다.
#![allow(dead_code)]

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ciborium::Value;
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, Ed25519KeyPair, KeyPair, RsaKeyPair};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

const RSA_PK8: &[u8] = include_bytes!("../fixtures/oidc-rsa.pk8");

pub const UP: u8 = 0x01;
pub const UV: u8 = 0x04;
const AT: u8 = 0x40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alg {
    Es256,
    EdDsa,
    Rs256,
}

enum Key {
    Ec(EcdsaKeyPair),
    Ed(Ed25519KeyPair),
    Rsa(RsaKeyPair),
}

pub struct Authenticator {
    key: Key,
    pub credential_id: Vec<u8>,
    /// 다음 서명에 쓸 횟수. 0이면 횟수를 세지 않는 인증기다(0을 그대로 낸다).
    pub counter: u32,
    /// 낼 플래그(기본 UP|UV)
    pub flags: u8,
}

pub fn b64(bytes: impl AsRef<[u8]>) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn cbor(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(value, &mut out).unwrap();
    out
}

fn int(i: i64) -> Value {
    Value::Integer(i.into())
}

impl Authenticator {
    pub fn new(alg: Alg) -> Self {
        let rng = SystemRandom::new();
        let key = match alg {
            Alg::Es256 => {
                let pk8 =
                    EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
                        .unwrap();
                Key::Ec(
                    EcdsaKeyPair::from_pkcs8(
                        &signature::ECDSA_P256_SHA256_ASN1_SIGNING,
                        pk8.as_ref(),
                        &rng,
                    )
                    .unwrap(),
                )
            }
            Alg::EdDsa => {
                let pk8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
                Key::Ed(Ed25519KeyPair::from_pkcs8(pk8.as_ref()).unwrap())
            }
            Alg::Rs256 => Key::Rsa(RsaKeyPair::from_pkcs8(RSA_PK8).unwrap()),
        };
        let mut credential_id = vec![0u8; 16];
        getrandom::fill(&mut credential_id).unwrap();
        Self {
            key,
            credential_id,
            counter: 1,
            flags: UP | UV,
        }
    }

    /// COSE_Key(CBOR)
    pub fn cose_key(&self) -> Vec<u8> {
        let map = match &self.key {
            Key::Ec(k) => {
                let p = k.public_key().as_ref();
                vec![
                    (int(1), int(2)),
                    (int(3), int(-7)),
                    (int(-1), int(1)),
                    (int(-2), Value::Bytes(p[1..33].to_vec())),
                    (int(-3), Value::Bytes(p[33..].to_vec())),
                ]
            }
            Key::Ed(k) => vec![
                (int(1), int(1)),
                (int(3), int(-8)),
                (int(-1), int(6)),
                (int(-2), Value::Bytes(k.public_key().as_ref().to_vec())),
            ],
            Key::Rsa(k) => {
                let public: signature::RsaPublicKeyComponents<Vec<u8>> = k.public().into();
                vec![
                    (int(1), int(3)),
                    (int(3), int(-257)),
                    (int(-1), Value::Bytes(public.n)),
                    (int(-2), Value::Bytes(public.e)),
                ]
            }
        };
        cbor(&Value::Map(map))
    }

    pub fn id(&self) -> String {
        b64(&self.credential_id)
    }

    fn sign(&self, message: &[u8]) -> Vec<u8> {
        let rng = SystemRandom::new();
        match &self.key {
            Key::Ec(k) => k.sign(&rng, message).unwrap().as_ref().to_vec(),
            Key::Ed(k) => k.sign(message).as_ref().to_vec(),
            Key::Rsa(k) => {
                let mut sig = vec![0; k.public().modulus_len()];
                k.sign(&signature::RSA_PKCS1_SHA256, &rng, message, &mut sig)
                    .unwrap();
                sig
            }
        }
    }

    fn auth_data(&self, rp_id: &str, flags: u8, attested: bool) -> Vec<u8> {
        let mut out = Sha256::digest(rp_id.as_bytes()).to_vec();
        out.push(flags | if attested { AT } else { 0 });
        out.extend_from_slice(&self.counter.to_be_bytes());
        if attested {
            out.extend_from_slice(&[0u8; 16]); // aaguid
            out.extend_from_slice(&(self.credential_id.len() as u16).to_be_bytes());
            out.extend_from_slice(&self.credential_id);
            out.extend_from_slice(&self.cose_key());
        }
        out
    }

    pub fn client_data(kind: &str, challenge: &str, origin: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "type": kind,
            "challenge": challenge,
            "origin": origin,
            "crossOrigin": false,
        }))
        .unwrap()
    }

    /// (`clientDataJSON`, `attestationObject`) 바이트
    pub fn register_raw(&self, rp_id: &str, challenge: &str, origin: &str) -> (Vec<u8>, Vec<u8>) {
        let client = Self::client_data("webauthn.create", challenge, origin);
        let att = Value::Map(vec![
            (Value::Text("fmt".into()), Value::Text("none".into())),
            (Value::Text("attStmt".into()), Value::Map(vec![])),
            (
                Value::Text("authData".into()),
                Value::Bytes(self.auth_data(rp_id, self.flags, true)),
            ),
        ]);
        (client, cbor(&att))
    }

    /// 등록 응답 JSON(`PublicKeyCredential.toJSON()` 모양)
    pub fn register(&self, rp_id: &str, challenge: &str, origin: &str) -> Json {
        let (client, att) = self.register_raw(rp_id, challenge, origin);
        json!({
            "id": self.id(),
            "rawId": self.id(),
            "type": "public-key",
            "response": {
                "clientDataJSON": b64(client),
                "attestationObject": b64(att),
                "transports": ["internal"],
            },
            "clientExtensionResults": {},
        })
    }

    /// (`clientDataJSON`, `authenticatorData`, 서명) 바이트. 먼저 횟수를 올린다(0이면 그대로).
    pub fn assert_raw(
        &mut self,
        rp_id: &str,
        challenge: &str,
        origin: &str,
    ) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        if self.counter != 0 {
            self.counter += 1;
        }
        let client = Self::client_data("webauthn.get", challenge, origin);
        let auth = self.auth_data(rp_id, self.flags, false);
        let mut message = auth.clone();
        message.extend_from_slice(&Sha256::digest(&client));
        let sig = self.sign(&message);
        (client, auth, sig)
    }

    /// 로그인 응답 JSON
    pub fn assert(&mut self, rp_id: &str, challenge: &str, origin: &str) -> Json {
        let (client, auth, sig) = self.assert_raw(rp_id, challenge, origin);
        json!({
            "id": self.id(),
            "rawId": self.id(),
            "type": "public-key",
            "response": {
                "clientDataJSON": b64(client),
                "authenticatorData": b64(auth),
                "signature": b64(sig),
            },
        })
    }
}
