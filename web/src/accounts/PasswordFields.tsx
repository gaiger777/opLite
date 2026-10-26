import { useTranslation } from "react-i18next";

import { Field } from "@/components/Field";
import { Input } from "@/components/ui/input";

/** 새 비밀번호와 그 확인. 오류는 `passwordErrors`의 결과다. */
export function PasswordFields({
  id,
  password,
  confirm,
  errors,
  onPassword,
  onConfirm,
}: {
  id: string;
  password: string;
  confirm: string;
  errors: Record<string, string>;
  onPassword(value: string): void;
  onConfirm(value: string): void;
}) {
  const { t } = useTranslation();
  return (
    <>
      <Field
        id={`${id}-password`}
        label={t("accounts.newPassword")}
        error={errors.password}
        hint={t("accounts.passwordHint")}
      >
        {(a11y) => (
          <Input
            id={`${id}-password`}
            type="password"
            autoComplete="new-password"
            value={password}
            onChange={(e) => onPassword(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field id={`${id}-confirm`} label={t("accounts.confirmPassword")} error={errors.confirm}>
        {(a11y) => (
          <Input
            id={`${id}-confirm`}
            type="password"
            autoComplete="new-password"
            value={confirm}
            onChange={(e) => onConfirm(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </>
  );
}
