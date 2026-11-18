// 패스키(WebAuthn) 브라우저 쪽(계획 62 설계 §10.1): 서버가 base64url 글로 준 옵션을 브라우저 API의 바이트로 바꾸고,
// 받은 자격 증명을 다시 base64url JSON으로 만든다. `PublicKeyCredential.toJSON()`이 없는 브라우저도 있어 직접 만든다.
import type { components } from "@/api/schema";

type Schemas = components["schemas"];

export function fromB64url(text: string): ArrayBuffer {
  const b64 = text.replace(/-/g, "+").replace(/_/g, "/");
  const bin = atob(b64 + "=".repeat((4 - (b64.length % 4)) % 4));
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) {
    bytes[i] = bin.charCodeAt(i);
  }
  return bytes.buffer;
}

export function toB64url(buf: ArrayBuffer): string {
  let bin = "";
  for (const b of new Uint8Array(buf)) {
    bin += String.fromCharCode(b);
  }
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** 이 브라우저가 패스키를 쓸 수 있다(보안 출처·API). IP 주소 접속은 서버가 `rp_id`를 주지 않는다. */
export function passkeysSupported(): boolean {
  return typeof window.PublicKeyCredential === "function" && !!navigator.credentials;
}

export function creationOptions(o: Schemas["CreationOptions"]): PublicKeyCredentialCreationOptions {
  return {
    challenge: fromB64url(o.challenge),
    rp: o.rp,
    user: { id: fromB64url(o.user.id), name: o.user.name, displayName: o.user.displayName },
    pubKeyCredParams: o.pubKeyCredParams.map((p) => ({
      alg: p.alg,
      type: p.type as PublicKeyCredentialType,
    })),
    timeout: o.timeout,
    excludeCredentials: o.excludeCredentials.map((c) => ({
      id: fromB64url(c.id),
      type: c.type as PublicKeyCredentialType,
    })),
    authenticatorSelection: {
      residentKey: o.authenticatorSelection.residentKey as ResidentKeyRequirement,
      userVerification: o.authenticatorSelection.userVerification as UserVerificationRequirement,
    },
    attestation: o.attestation as AttestationConveyancePreference,
  };
}

export function requestOptions(o: Schemas["RequestOptions"]): PublicKeyCredentialRequestOptions {
  return {
    challenge: fromB64url(o.challenge),
    rpId: o.rpId,
    timeout: o.timeout,
    userVerification: o.userVerification as UserVerificationRequirement,
    allowCredentials: o.allowCredentials.map((c) => ({
      id: fromB64url(c.id),
      type: c.type as PublicKeyCredentialType,
    })),
  };
}

/** 등록 응답(`navigator.credentials.create`) → 서버 `RegistrationCredential` */
export function registrationJson(cred: PublicKeyCredential): Schemas["RegistrationCredential"] {
  const response = cred.response as AuthenticatorAttestationResponse;
  return {
    id: toB64url(cred.rawId),
    response: {
      clientDataJSON: toB64url(response.clientDataJSON),
      attestationObject: toB64url(response.attestationObject),
    },
  };
}

/** 로그인 응답(`navigator.credentials.get`) → 서버 `AssertionCredential` */
export function assertionJson(cred: PublicKeyCredential): Schemas["AssertionCredential"] {
  const response = cred.response as AuthenticatorAssertionResponse;
  return {
    id: toB64url(cred.rawId),
    response: {
      clientDataJSON: toB64url(response.clientDataJSON),
      authenticatorData: toB64url(response.authenticatorData),
      signature: toB64url(response.signature),
    },
  };
}

/** 패스키로 로그인 둘째 단계를 한다(브라우저 창). 사용자가 취소하면 null */
export async function getAssertion(
  options: Schemas["RequestOptions"],
): Promise<Schemas["AssertionCredential"] | null> {
  try {
    const cred = (await navigator.credentials.get({
      publicKey: requestOptions(options),
    })) as PublicKeyCredential | null;
    return cred ? assertionJson(cred) : null;
  } catch (e) {
    if (e instanceof DOMException && e.name === "NotAllowedError") {
      return null;
    }
    throw e;
  }
}
