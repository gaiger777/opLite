import { useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { Vm } from "@/api/types";
import { haPolicy } from "@/vms/ha";

export type GuestInfoResponse = components["schemas"]["GuestInfoResponse"];
export type GuestInfo = components["schemas"]["GuestInfo"];

/** 새로 묻기(`?refresh=true`)의 간격. 서버의 노드 캐시도 VM마다 5초에 한 번만 다시 묻는다. */
export const GUEST_REFRESH_MS = 5000;
/** 노드가 게스트 정보를 모으는 주기(60초)에 맞춰 다시 읽는다. */
const GUEST_POLL_MS = 60_000;
/** 서버의 `validate_guest_password`: 사용자 이름은 1~64바이트, 비밀번호는 1~256바이트 */
export const MAX_GUEST_USER_BYTES = 64;
export const MAX_GUEST_PASSWORD_BYTES = 256;

/**
 * 서버의 `VmSpec::agent_channel`: 게스트 에이전트 채널이 붙는다(`guest_agent`나 HA 게스트 감시). 도는 QEMU에 이미 붙어
 * 있어도(`status.hardware.guest_agent`, 끄기는 다음 시작 때) 채널이 있다.
 */
export function agentChannel(vm: Vm): boolean {
  return (
    vm.spec.guest_agent != null ||
    haPolicy(vm.spec)?.guest_watch != null ||
    vm.status.hardware?.guest_agent === true
  );
}

/** 게스트 정보 쿼리 키. 노드나 상태가 바뀌면(켜짐·꺼짐·옮김) 다시 묻는다. */
export function guestKey(vm: Vm) {
  return ["guest", vm.id, vm.status.node ?? null, vm.status.phase] as const;
}

/** `GET /vms/{key}/guest`. `refresh`면 노드가 에이전트에 새로 묻는다. */
export function fetchGuest(vm: Vm, refresh = false): Promise<GuestInfoResponse> {
  return call(
    api.GET("/vms/{key}/guest", {
      params: { path: { key: vm.id }, ...(refresh && { query: { refresh: true } }) },
    }),
  );
}

/** VM의 게스트 정보. `enabled`가 거짓이면 묻지 않는다. */
export function useGuest(vm: Vm, enabled = true) {
  return useQuery({
    queryKey: guestKey(vm),
    queryFn: () => fetchGuest(vm),
    enabled,
    refetchInterval: GUEST_POLL_MS,
  });
}

/** 루프백이 아닌 주소(`10.0.0.5/24`) */
export function guestAddresses(info: GuestInfo): string[] {
  return (info.interfaces ?? []).flatMap((nic) =>
    (nic.addresses ?? [])
      .filter((a) => !a.ip.startsWith("127.") && a.ip !== "::1")
      .map((a) => `${a.ip}/${a.prefix}`),
  );
}

/** OS 이름과 버전(`Ubuntu 24.04`). 이름이 없으면 id, 둘 다 없으면 null */
export function guestOs(info: GuestInfo): string | null {
  const os = info.os;
  const name = os?.name ?? os?.id;
  if (!name) {
    return null;
  }
  // 이름에 버전이 이미 들어 있는 OS(Windows의 "Microsoft Windows 11")가 많다.
  return os?.version && !name.includes(os.version) ? `${name} ${os.version}` : name;
}

const encoder = new TextEncoder();

/**
 * 게스트 비밀번호 입력의 오류(필드 → 번역 키). 서버의 `validate_guest_password`와 같은 규칙에 두 번 입력한 값이 같은지를
 * 더한다.
 */
export function guestPasswordErrors(
  user: string,
  password: string,
  again: string,
): Record<string, string> {
  const errors: Record<string, string> = {};
  const userBytes = encoder.encode(user).length;
  // 제어 문자(Rust의 `char::is_control`, 유니코드 Cc)를 받지 않는다.
  if (userBytes === 0 || userBytes > MAX_GUEST_USER_BYTES || /\p{Cc}/u.test(user)) {
    errors.user = "vms.guest.errors.user";
  }
  const passwordBytes = encoder.encode(password).length;
  if (passwordBytes === 0 || passwordBytes > MAX_GUEST_PASSWORD_BYTES) {
    errors.password = "vms.guest.errors.password";
  } else if (password !== again) {
    errors.again = "vms.guest.errors.again";
  }
  return errors;
}
