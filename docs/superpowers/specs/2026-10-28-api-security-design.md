# opLite API·보안 보완 설계 (계획 13)

작성일: 2026-10-28
상위 문서: `2026-10-20-opl-node-core-design.md`, `2026-10-27-robustness-design.md`

## 1. 목표와 범위

계획 5·8·9에서 넘긴 API·보안 사항을 마친다.

| 항목 | 내용 |
|---|---|
| 시도 제한 | 로그인과 현재 비밀번호 확인의 실패 횟수를 노드마다 세어 막는다 |
| 세션 세대 | 비밀번호가 바뀌면 모든 노드의 옛 세션을 끊는다 |
| 업로드 사전 확인 | 본문을 보내기 전에 거절 이유를 알려 주는 API, 웹의 처리 대기 제한 시간 |
| 노드 아키텍처 | `NodeStatus.arch`, 머신 기본값(웹·oplctl), 스케줄러와 지정 노드 검증 |

넣지 않는 것: 클러스터 전체에서 세는 시도 제한(실패마다 Raft에 쓰면 공격이 곧 쓰기 부하다), 프록시 헤더(`X-Forwarded-For`) 신뢰, API 토큰을 비밀번호 변경과 함께 끊기, 감사 로그. 계획 14(웹 정리)와 공유 스토리지는 그 뒤다.

## 2. 시도 제한

`crates/opl-api/src/limit.rs`의 `AttemptLimiter`. `AppState`가 노드마다 하나 가진다(세션도 노드별이다). 노드가 N개면 공격자가 시도할 수 있는 횟수도 N배지만, 이 플랫폼의 규모(노드 3–5개)에서는 받아들인다.

- **키 두 종류.**
  - (소문자로 바꾼 사용자 이름, IP): 연속 5번 실패하면 30초 막는다.
  - IP만: 연속 20번 실패하면 30초 막는다. 한 IP에서 사용자 이름을 바꿔 가며 시도하는 것과 argon2 계산으로 CPU를 쓰게 하는 것을 막는다.
- **늘어나는 잠금.** 막힌 뒤 풀린 다음에도 실패가 이어지면 실패할 때마다 막는 시간을 두 배로 늘린다. 최대 15분이다.
- **성공하면** 그 (사용자 이름, IP) 기록과 그 IP 기록을 지운다.
- **기록 정리.** 마지막 시도 뒤 15분이 지난 기록은 지운다(기록할 때 함께). 기록 수는 1만 개를 넘지 않는다. 넘치면 마지막 시도가 가장 오래된 것부터 버린다.
- **판단 순서.** 막힌 상태면 해시를 계산하지 않고 바로 **429** `too_many_attempts`를 돌려준다. `Retry-After` 헤더에 남은 초를 넣고, 메시지에 "try again in N seconds"를 넣는다. 막히지 않았으면 평소처럼 확인하고 성공·실패를 기록한다.
- **적용 대상.**
  - `POST /auth/login`. 없는 사용자도 실패로 센다.
  - 자기 비밀번호를 바꿀 때의 현재 비밀번호 확인(`PATCH /users/{id}`의 `current_password`). 키는 (요청한 사용자 이름, IP)이고 로그인과 같은 기록을 쓴다.
- **값**은 상수다. 설정에 노출하지 않는다.
- **클라이언트 IP**는 TLS 리스너의 연결 주소(`into_make_service_with_connect_info::<SocketAddr>()`)다. 프록시 뒤에서는 모든 요청이 프록시 IP로 보인다(문서에 적는다). IP를 얻을 수 없는 경우(테스트 라우터를 `oneshot`으로 부를 때)는 미지정 주소(`0.0.0.0`) 하나로 센다.
- 시계는 `tokio::time::Instant`를 써서 테스트가 시간을 멈추고 당길 수 있게 한다.

## 3. 세션 세대

- `UserSpec.session_epoch: u64`, `#[serde(default)]`(옛 데이터는 0).
- 비밀번호가 바뀌면(본인이든 관리자든) 같은 쓰기에서 1을 올린다.
- 세션은 만들 때의 세대를 함께 저장한다.
- 쿠키로 인증할 때 Raft에서 다시 읽은 사용자의 세대와 비교한다(지금도 요청마다 사용자를 다시 읽는다). 다르면 그 세션을 지우고 401 `unauthorized`를 돌려준다. 그래서 다른 노드의 옛 세션도 다음 요청에서 끊긴다.
- 요청을 처리한 노드의 `sessions.remove_user`는 그대로 둔다.
- API 토큰은 끊지 않는다. 토큰은 사용자가 따로 지운다(문서에 적는다).

## 4. 업로드 사전 확인

- `import_volume` 앞부분의 확인을 `check_import(state, principal, params, size)`로 뽑는다. 순서:
  1. 권한(Volume 쓰기)
  2. 이름 형식과 예약어 `import`
  3. 풀이 있는지
  4. 풀이 이 노드에 있는지(`pool_not_on_node`)
  5. 이름 중복(`name_taken`, 이름으로 조회)
  6. 공간: 크기 + 1 GiB를 남길 수 있는지(`insufficient_storage`)
- **`POST /volumes/import/check?name&pool&format&source_format&size`**: `check_import`만 부르고 문제가 없으면 204. 본문은 받지 않는다. OpenAPI에 등록한다.
- 실제 업로드도 `check_import`를 부른다(크기는 `Content-Length`, 없으면 411). 확인과 업로드 사이에 다른 요청이 이름을 가져갈 수 있으므로 Raft에 만들 때의 `name_taken`도 그대로 둔다.
- **웹**(`web/src/storage/upload.ts`): XHR 전에 확인 API를 부르고, 거절이면 업로드하지 않고 오류 코드 문구로 실패 토스트를 보인다.
- **처리 대기 제한 시간(웹).** 모든 바이트를 보낸 뒤 2분 안에 응답이 없으면 "처리 중" 토스트를 `uploads.noResponse`("서버 응답이 없습니다. 볼륨 목록에서 결과를 확인하세요.")로 바꾸고 닫을 수 있게 한다. XHR은 끊지 않는다(끊으면 서버의 `UploadGuard`가 받은 볼륨을 지운다). 나중에 응답이 오면 평소처럼 결과 토스트를 띄운다.
- **oplctl**(`volume import`): 파일 크기로 확인 API를 먼저 부르고, 거절이면 파일을 보내지 않는다.

## 5. 노드 아키텍처

- **타입.** `Arch { X86_64, Aarch64 }`(serde `x86_64`/`aarch64`)를 opl-types로 옮긴다. `native()`(컴파일된 아키텍처)와 `machine()`도 함께 옮기고, opl-qemu는 opl-types 것을 쓴다. `MachineType::arch()`(`q35` → `x86_64`, `virt` → `aarch64`)를 둔다. `NodeStatus.arch: Option<Arch>`, `#[serde(default)]`(옛 노드는 `None`).
- **opld.** `register_node`가 이 노드의 `HostProfile.arch`로 채운다.
- **스케줄러.** 후보 조건에 "노드 arch == VM 머신의 arch"를 더한다. arch가 `None`인 노드는 후보가 아니다. 맞는 노드가 없으면 지금처럼 409 `no_node`다.
- **지정 노드 검증.** VM을 만들거나 고칠 때(같은 검증 함수) 지정 노드의 arch가 있고 머신의 arch와 다르면 400 `machine_arch_mismatch`. arch를 모르는 노드는 통과시킨다(옛 노드가 섞인 업그레이드 중에 막지 않는다).
- **oplctl.** `--machine`이 없으면 노드 목록을 읽어 정한다. 지정 노드에 arch가 있으면 그것, 아니면 arch를 아는 노드가 모두 같은 arch일 때 그것. 그 밖에는 오류로 `--machine`을 달라고 한다. 클라이언트의 컴파일 아키텍처로 정하던 코드는 지운다.
- **웹.** `defaultMachine(nodes, selectedNode)`: 지정 노드의 arch → arch를 아는 노드가 모두 같으면 그 arch → `q35`. hvf로 추측하던 코드는 지운다. 노드를 바꾸면 사용자가 머신을 직접 고르지 않았을 때만 기본값을 다시 정한다. 노드 목록과 상세에 "아키텍처"를 보인다.

## 6. 클라이언트 문구

- 웹 `errors.too_many_attempts`: ko "시도가 너무 많습니다. 잠시 뒤에 다시 하세요." / en "Too many attempts. Try again later."
- 웹 `errors.machine_arch_mismatch`: ko "선택한 노드의 아키텍처와 머신 종류가 맞지 않습니다." / en "The machine type doesn't match the selected node's architecture."
- 웹 `uploads.noResponse`: ko "서버 응답이 없습니다. 볼륨 목록에서 결과를 확인하세요." / en "No response from the server. Check the volume list for the result."
- oplctl은 서버 메시지를 그대로 보인다.

## 7. 테스트

- `AttemptLimiter` 단위 테스트(멈춘 시계): 5번째 실패 뒤 막힘, 30초 뒤 풀림, 이어진 실패로 두 배, 최대 15분, 성공하면 초기화, IP 키 20번, 정리와 상한.
- API: 로그인 6번째 시도가 429와 `Retry-After`; 막힌 동안 맞는 비밀번호도 429; 현재 비밀번호 확인도 막힘.
- 세션 세대: 노드 둘(같은 클러스터의 `AppState` 둘)에서 한쪽이 비밀번호를 바꾸면 다른 쪽의 옛 쿠키가 401.
- 업로드 확인: 각 거절 코드, 통과 204, 업로드의 이름 중복이 볼륨을 만들기 전에 409.
- 아키텍처: 스케줄러가 arch가 다르거나 `None`인 노드를 빼는지, 지정 노드가 맞지 않으면 400, 모르면 통과, opld 등록이 arch를 채우는지.
- oplctl과 웹: 기본값 함수, 웹 업로드의 확인 거절(XHR을 보내지 않음)과 2분 제한 시간(가짜 타이머).

## 8. 알려진 한계

- 시도 제한은 노드마다 따로 센다. opld가 다시 시작하면 기록이 사라진다.
- 프록시 뒤에서는 모든 사용자가 한 IP로 보여, IP 키가 정상 사용자를 함께 막을 수 있다.
- API 토큰은 비밀번호를 바꿔도 살아 있다.
- arch를 모르는 노드(옛 버전)는 자동 배치 후보가 아니다. 모든 노드가 새 버전으로 다시 등록하면 풀린다.
