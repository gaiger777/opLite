// 2단계 인증 등록(계획 62 설계 §10·§13 웹): 인증 앱(TOTP, QR)과 패스키, 한 번만 보이는 복구 코드.
// 설정 › 2단계 인증과 로그인의 강제 등록 화면이 함께 쓴다.
import { useQuery } from "@tanstack/react-query";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";

import { creationOptions, passkeysSupported, registrationJson } from "@/accounts/webauthn";
import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import { CopyButton } from "@/components/CopyButton";
import { Field } from "@/components/Field";
import { QrCode } from "@/components/QrCode";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { errorMessage } from "@/i18n/errors";

/** 새 복구 코드(한 번만 보인다). 복사하고 "저장했습니다"를 눌러야 넘어간다. */
export function RecoveryCodes({ codes, onDone }: { codes: string[]; onDone(): void }) {
  const { t } = useTranslation();
  const titleId = useId();
  return (
    <section aria-labelledby={titleId} className="flex flex-col gap-3">
      <h2 id={titleId} className="font-medium">
        {t("twoFactor.recoveryTitle")}
      </h2>
      <p className="text-sm text-muted-foreground">{t("twoFactor.recoveryOnce")}</p>
      <ul
        aria-label={t("twoFactor.recoveryTitle")}
        className="grid grid-cols-2 gap-1 rounded-md bg-muted p-3 font-mono text-sm"
      >
        {codes.map((code) => (
          <li key={code}>{code}</li>
        ))}
      </ul>
      <div className="flex items-center gap-2">
        <CopyButton text={codes.join("\n")} label={t("twoFactor.copyCodes")} />
        <Button onClick={onDone}>{t("twoFactor.savedCodes")}</Button>
      </div>
    </section>
  );
}

type Enrollment = components["schemas"]["TotpEnrollment"];

/** 본인 확인(지금 요소의 코드·복구 코드 또는 로컬 비밀번호) */
export type Proof = { code: string } | { password: string };

/**
 * 이미 요소가 있으면 요소를 더하기 전에 본인 확인을 묻는다(서버 400 `proof_required`, 설계 §18 리뷰 1). 부르는 쪽이
 * 확인 창을 열고 받은 확인으로 `send`를 부른다(던지면 그 창에 오류를 보이고, 끝나면 창을 닫는다). 인증 앱은 실패를
 * 받아 창을 닫는다. 없으면(첫 요소·강제 등록) 바로 보낸다.
 */
export type Prove = (title: string, send: (proof: Proof) => Promise<void>) => void;

/**
 * 인증 앱 등록: 시작하면 QR과 비밀(직접 입력용)을 보이고, 앱의 코드를 확인하면 끝난다. 첫 요소면 복구 코드를 준다
 * (`onDone(codes)`, 아니면 빈 목록).
 */
export function TotpSetup({ onDone, prove }: { onDone(codes: string[]): void; prove?: Prove }) {
  const { t } = useTranslation();
  const [enrollment, setEnrollment] = useState<Enrollment | null>(null);
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function start() {
    setBusy(true);
    setError(null);
    try {
      setEnrollment(await call(api.POST("/auth/2fa/totp")));
    } catch (e) {
      setError(errorMessage(t, e));
    } finally {
      setBusy(false);
    }
  }

  async function confirm(e: React.FormEvent) {
    e.preventDefault();
    const send = async (proof?: Proof) => {
      const result = await call(
        api.POST("/auth/2fa/totp/confirm", { body: { code: code.replace(/\s/g, ""), proof } }),
      );
      onDone(result.codes);
    };
    if (prove) {
      setError(null);
      // 실패하면 확인 창을 닫고 앱 코드를 비워 오류를 여기 보인다. 창에서 다시 누르면 지난 앱 코드를 또 보내게
      // 된다(그 사이 코드가 바뀌었거나 틀린 코드였다, 계획 62 리뷰 2).
      prove(t("twoFactor.setUpApp"), async (proof) => {
        try {
          await send(proof);
        } catch (err) {
          setCode("");
          setError(errorMessage(t, err));
        }
      });
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await send();
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  if (!enrollment) {
    return (
      <div className="flex flex-col gap-2">
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}
        <div>
          <Button disabled={busy} onClick={() => void start()}>
            {t("twoFactor.setUpApp")}
          </Button>
        </div>
      </div>
    );
  }
  return (
    <form onSubmit={(e) => void confirm(e)} className="flex flex-col gap-3">
      <p className="text-sm">{t("twoFactor.scan")}</p>
      <QrCode text={enrollment.uri} label={t("twoFactor.qrLabel")} />
      <div className="flex items-center gap-2 text-sm">
        <span className="text-muted-foreground">{t("twoFactor.secret")}</span>
        <code data-testid="totp-secret" className="font-mono break-all">
          {enrollment.secret}
        </code>
        <CopyButton text={enrollment.secret} label={t("twoFactor.copySecret")} />
      </div>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <Field id="totp-confirm" label={t("twoFactor.code")} help={t("help.twoFactor.code")}>
        {(a11y) => (
          <Input
            id="totp-confirm"
            inputMode="numeric"
            autoComplete="one-time-code"
            className="w-40"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <div>
        <Button type="submit" disabled={busy || code.trim().length < 6}>
          {t("twoFactor.confirm")}
        </Button>
      </div>
    </form>
  );
}

/** 패스키 등록: 이름을 받고 브라우저 창을 연다. 첫 요소면 복구 코드를 준다. */
export function PasskeySetup({ onDone, prove }: { onDone(codes: string[]): void; prove?: Prove }) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const send = async (proof?: Proof) => {
    const start = await call(api.POST("/auth/2fa/webauthn/register"));
    let cred: PublicKeyCredential | null;
    try {
      cred = (await navigator.credentials.create({
        publicKey: creationOptions(start.options),
      })) as PublicKeyCredential | null;
    } catch (err) {
      throw err instanceof DOMException && err.name === "NotAllowedError"
        ? new Error(t("twoFactor.passkeyCancelled"))
        : err;
    }
    if (!cred) {
      return;
    }
    const result = await call(
      api.POST("/auth/2fa/webauthn/register/finish", {
        body: { token: start.token, name: name.trim(), credential: registrationJson(cred), proof },
      }),
    );
    onDone(result.recovery_codes);
  };

  async function add(e: React.FormEvent) {
    e.preventDefault();
    setError(null);
    // 이미 요소가 있으면 본인 확인 창에서 등록 전체를 한다(창의 단추가 브라우저 패스키 창을 여는 사용자 동작이다).
    if (prove) {
      prove(t("twoFactor.addPasskey"), send);
      return;
    }
    setBusy(true);
    try {
      await send();
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={(e) => void add(e)} className="flex flex-col gap-3">
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <Field
        id="passkey-name"
        label={t("twoFactor.passkeyName")}
        help={t("help.twoFactor.passkeyName")}
      >
        {(a11y) => (
          <Input
            id="passkey-name"
            autoComplete="off"
            className="w-64"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <div>
        <Button type="submit" variant="outline" disabled={busy || name.trim().length === 0}>
          {t("twoFactor.addPasskey")}
        </Button>
      </div>
    </form>
  );
}

/**
 * 로그인의 강제 등록 화면: 정책이 2단계 인증을 요구하는데 요소가 없다. 인증 앱이나(이 접속에서 되면) 패스키를
 * 등록하면 같은 세션이 정식 세션이 된다. 복구 코드를 보인 뒤 `onDone`이 로그인을 끝낸다.
 */
export function EnrollStep({ onDone, onCancel }: { onDone(): void; onCancel(): void }) {
  const { t } = useTranslation();
  const [codes, setCodes] = useState<string[] | null>(null);
  // 패스키는 도메인 접속(`rp_id`)이고 브라우저가 쓸 수 있을 때만
  const status = useQuery({
    queryKey: ["2fa"],
    queryFn: () => call(api.GET("/auth/2fa")),
    retry: false,
  });
  const passkeys = !!status.data?.rp_id && passkeysSupported();
  const finished = (got: string[]) => (got.length > 0 ? setCodes(got) : onDone());
  if (codes) {
    return <RecoveryCodes codes={codes} onDone={onDone} />;
  }
  return (
    <div className="flex flex-col gap-4">
      <h2 className="font-medium">{t("twoFactor.enrollTitle")}</h2>
      <p className="text-sm text-muted-foreground">{t("twoFactor.enrollRequired")}</p>
      <TotpSetup onDone={finished} />
      {passkeys && (
        <>
          <p className="text-sm text-muted-foreground">{t("twoFactor.orPasskey")}</p>
          <PasskeySetup onDone={finished} />
        </>
      )}
      <div>
        <Button variant="link" className="px-0" onClick={onCancel}>
          {t("twoFactor.backToLogin")}
        </Button>
      </div>
    </div>
  );
}
