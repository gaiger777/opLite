//! WebAuthn 최소 검증기(계획 62 설계 §10.1·§14): 시험 안 소프트웨어 인증기로 ES256·EdDSA·RS256 등록·로그인, 틀린
//! origin·challenge·rpId·type·플래그와 서명 횟수 되감기 거절.

mod common;

use common::authenticator::{Alg, Authenticator, UP, UV, b64};
use orbivisor_api::webauthn::{
    EDDSA, ES256, Expected, RS256, WebauthnError, key_algorithm, verify_assertion,
    verify_registration,
};

const RP: &str = "opl.example.com";
const ORIGIN: &str = "https://opl.example.com:8006";

fn origins() -> Vec<String> {
    vec![ORIGIN.to_string()]
}

fn expected<'a>(challenge: &'a [u8], origins: &'a [String]) -> Expected<'a> {
    Expected {
        challenge,
        rp_id: RP,
        origins,
        user_verification: false,
    }
}

/// 등록하고 (인증기, 저장할 COSE 키, 서명 횟수)
fn registered(alg: Alg) -> (Authenticator, Vec<u8>, u32) {
    let auth = Authenticator::new(alg);
    let challenge = [7u8; 32];
    let o = origins();
    let (client, att) = auth.register_raw(RP, &b64(challenge), ORIGIN);
    let reg = verify_registration(&expected(&challenge, &o), &client, &att).unwrap();
    assert_eq!(reg.credential_id, auth.credential_id);
    assert_eq!(reg.cose_key, auth.cose_key());
    assert_eq!(reg.sign_count, 1);
    assert!(reg.user_verified);
    (auth, reg.cose_key, reg.sign_count)
}

#[test]
fn registers_and_signs_in_with_every_algorithm() {
    for (alg, cose) in [
        (Alg::Es256, ES256),
        (Alg::EdDsa, EDDSA),
        (Alg::Rs256, RS256),
    ] {
        let (mut auth, key, count) = registered(alg);
        assert_eq!(key_algorithm(&key), Some(cose), "{alg:?}");
        let challenge = [9u8; 32];
        let o = origins();
        let (client, data, sig) = auth.assert_raw(RP, &b64(challenge), ORIGIN);
        let a = verify_assertion(&expected(&challenge, &o), &key, count, &client, &data, &sig)
            .unwrap_or_else(|e| panic!("{alg:?}: {e}"));
        assert_eq!(a.sign_count, 2);
        assert!(a.user_verified);
        // 다른 키의 서명
        let other = Authenticator::new(alg);
        let other_key = other.cose_key();
        if other_key != key {
            assert_eq!(
                verify_assertion(
                    &expected(&challenge, &o),
                    &other_key,
                    0,
                    &client,
                    &data,
                    &sig
                ),
                Err(WebauthnError::Signature),
                "{alg:?}"
            );
        }
        // 서명을 한 바이트 바꾼다.
        let mut bad = sig.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert_eq!(
            verify_assertion(&expected(&challenge, &o), &key, count, &client, &data, &bad),
            Err(WebauthnError::Signature),
            "{alg:?}"
        );
    }
}

#[test]
fn rejects_wrong_challenge_origin_type_and_rp_id_on_registration() {
    let auth = Authenticator::new(Alg::Es256);
    let challenge = [1u8; 32];
    let o = origins();
    let e = expected(&challenge, &o);
    let (client, att) = auth.register_raw(RP, &b64([2u8; 32]), ORIGIN);
    assert_eq!(
        verify_registration(&e, &client, &att),
        Err(WebauthnError::Challenge)
    );
    let (client, att) = auth.register_raw(RP, &b64(challenge), "https://evil.example");
    assert_eq!(
        verify_registration(&e, &client, &att),
        Err(WebauthnError::Origin("https://evil.example".into()))
    );
    let (client, att) = auth.register_raw("evil.example", &b64(challenge), ORIGIN);
    assert_eq!(
        verify_registration(&e, &client, &att),
        Err(WebauthnError::RpId)
    );
    // 로그인 응답의 clientData로 등록하지 않는다.
    let (_, att) = auth.register_raw(RP, &b64(challenge), ORIGIN);
    let client = Authenticator::client_data("webauthn.get", &b64(challenge), ORIGIN);
    assert_eq!(
        verify_registration(&e, &client, &att),
        Err(WebauthnError::Type)
    );
    // 깨진 입력
    assert_eq!(
        verify_registration(&e, b"{", &att),
        Err(WebauthnError::ClientData)
    );
    let (client, _) = auth.register_raw(RP, &b64(challenge), ORIGIN);
    assert_eq!(
        verify_registration(&e, &client, &[0xff, 0x00]),
        Err(WebauthnError::Attestation)
    );
}

#[test]
fn user_presence_is_required_and_verification_when_asked() {
    let mut auth = Authenticator::new(Alg::EdDsa);
    let challenge = [3u8; 32];
    let o = origins();
    auth.flags = UP;
    let (client, att) = auth.register_raw(RP, &b64(challenge), ORIGIN);
    let reg = verify_registration(&expected(&challenge, &o), &client, &att).unwrap();
    assert!(!reg.user_verified);
    let strict = Expected {
        user_verification: true,
        ..expected(&challenge, &o)
    };
    assert_eq!(
        verify_registration(&strict, &client, &att),
        Err(WebauthnError::UserVerified)
    );
    auth.flags = UV;
    let (client, data, sig) = auth.assert_raw(RP, &b64(challenge), ORIGIN);
    assert_eq!(
        verify_assertion(
            &expected(&challenge, &o),
            &reg.cose_key,
            reg.sign_count,
            &client,
            &data,
            &sig
        ),
        Err(WebauthnError::UserPresent)
    );
}

#[test]
fn sign_count_must_grow_unless_the_authenticator_does_not_count() {
    let (mut auth, key, _) = registered(Alg::Es256);
    let challenge = [4u8; 32];
    let o = origins();
    let e = expected(&challenge, &o);
    // 저장한 횟수가 5인데 인증기가 3을 낸다(되감기: 복제된 인증기).
    auth.counter = 2;
    let (client, data, sig) = auth.assert_raw(RP, &b64(challenge), ORIGIN);
    assert_eq!(
        verify_assertion(&e, &key, 5, &client, &data, &sig),
        Err(WebauthnError::Counter)
    );
    // 같은 횟수도 거절
    assert_eq!(
        verify_assertion(&e, &key, 3, &client, &data, &sig),
        Err(WebauthnError::Counter)
    );
    assert_eq!(
        verify_assertion(&e, &key, 2, &client, &data, &sig)
            .unwrap()
            .sign_count,
        3
    );
    // 횟수를 세지 않는 인증기(늘 0)는 저장한 값도 0이면 받는다.
    auth.counter = 0;
    let (client, data, sig) = auth.assert_raw(RP, &b64(challenge), ORIGIN);
    assert_eq!(
        verify_assertion(&e, &key, 0, &client, &data, &sig)
            .unwrap()
            .sign_count,
        0
    );
    assert_eq!(
        verify_assertion(&e, &key, 7, &client, &data, &sig),
        Err(WebauthnError::Counter)
    );
}

#[test]
fn assertion_checks_challenge_origin_rp_id_and_type() {
    let (mut auth, key, count) = registered(Alg::Rs256);
    let challenge = [5u8; 32];
    let o = origins();
    let e = expected(&challenge, &o);
    let (client, data, sig) = auth.assert_raw(RP, &b64([6u8; 32]), ORIGIN);
    assert_eq!(
        verify_assertion(&e, &key, count, &client, &data, &sig),
        Err(WebauthnError::Challenge)
    );
    let (client, data, sig) = auth.assert_raw(RP, &b64(challenge), "http://opl.example.com:8006");
    assert!(matches!(
        verify_assertion(&e, &key, count, &client, &data, &sig),
        Err(WebauthnError::Origin(_))
    ));
    let (client, data, sig) = auth.assert_raw("other.example", &b64(challenge), ORIGIN);
    assert_eq!(
        verify_assertion(&e, &key, count, &client, &data, &sig),
        Err(WebauthnError::RpId)
    );
    let (_, data, sig) = auth.assert_raw(RP, &b64(challenge), ORIGIN);
    let client = Authenticator::client_data("webauthn.create", &b64(challenge), ORIGIN);
    assert_eq!(
        verify_assertion(&e, &key, count, &client, &data, &sig),
        Err(WebauthnError::Type)
    );
    assert_eq!(
        verify_assertion(&e, &key, count, &client, &data[..20], &sig),
        Err(WebauthnError::Type)
    );
    // 저장한 키가 깨졌다.
    let (client, data, sig) = auth.assert_raw(RP, &b64(challenge), ORIGIN);
    assert_eq!(
        verify_assertion(&e, &[0xa0, 0x00], count, &client, &data, &sig),
        Err(WebauthnError::Key)
    );
    assert_eq!(key_algorithm(&[0xa0]), None);
}
