/** 서버의 `MIN_PASSWORD_LEN`(글자 수) */
export const MIN_PASSWORD_LEN = 8;

/** 새 비밀번호와 확인 입력의 오류(번역 키). 서버는 글자 수만 본다. */
export function passwordErrors(password: string, confirm: string): Record<string, string> {
  const errors: Record<string, string> = {};
  if ([...password].length < MIN_PASSWORD_LEN) {
    errors.password = "accounts.errors.password";
  } else if (password !== confirm) {
    errors.confirm = "accounts.errors.mismatch";
  }
  return errors;
}
