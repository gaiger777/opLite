import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { passwordErrors } from "@/accounts/password";
import { PasswordFields } from "@/accounts/PasswordFields";
import { ApiError, api, call } from "@/api/client";
import { useAuth, usePrincipal } from "@/auth/AuthProvider";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Input } from "@/components/ui/input";

/**
 * 내 비밀번호를 바꾼다. 서버는 현재 비밀번호를 확인하고(훔친 세션으로 계정을 빼앗지 못하게), 바꾸면 내
 * 세션을 모두 끝낸다. 그래서 바꾼 뒤에는 로그아웃하고 새 비밀번호로 다시 로그인하게 한다.
 */
export function ChangePasswordDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const { logout } = useAuth();
  const [current, setCurrent] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found = passwordErrors(password, confirm);
    if (current === "") {
      found.current = "accounts.errors.current";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    try {
      await call(
        api.PUT("/users/{key}", {
          params: { path: { key: me.user } },
          body: { current_password: current, password },
        }),
      );
    } catch (err) {
      if (err instanceof ApiError && err.code === "invalid_credentials") {
        setErrors({ current: "accounts.errors.wrongCurrent" });
        return false;
      }
      throw err;
    }
    await logout();
    // 로그아웃이 알림을 모두 닫으므로 그 뒤에 알린다.
    toast.success(t("accounts.passwordChanged"));
  }

  return (
    <FormDialog
      title={t("accounts.changePassword")}
      description={t("accounts.changePasswordDescription")}
      submitLabel={t("users.save")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="current-password" label={t("accounts.currentPassword")} error={errors.current}>
        {(a11y) => (
          <Input
            id="current-password"
            type="password"
            autoComplete="current-password"
            value={current}
            onChange={(e) => setCurrent(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <PasswordFields
        id="mine"
        password={password}
        confirm={confirm}
        errors={errors}
        onPassword={setPassword}
        onConfirm={setConfirm}
      />
    </FormDialog>
  );
}
