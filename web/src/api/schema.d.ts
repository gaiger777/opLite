// 생성된 파일이다. 고치지 말고 `pnpm gen:api`로 다시 만든다.

export interface paths {
    "/access-roles": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 내장 역할 셋과 사용자 정의 역할. 로그인한 누구나 본다(grant를 주는 위임자가 고른다). */
        get: operations["list_access_roles"];
        put?: never;
        /** 사용자 정의 역할을 만든다(`cluster.admin`). 이름 `admin`·`operator`·`viewer`·`none`은 400, 모르는 권한 이름은 400. */
        post: operations["create_access_role"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/access-roles/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_access_role"];
        /** 권한 목록을 바꾼다(`cluster.admin`). 이 역할을 건 모든 grant가 바로 바뀐다. 내장 역할은 400. */
        put: operations["update_access_role"];
        post?: never;
        /** 사용자 정의 역할을 지운다(`cluster.admin`). 이 역할을 건 grant가 있으면 409 `role_in_use`, 내장 역할은 400. */
        delete: operations["delete_access_role"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/alert-rules": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_alert_rules"];
        put?: never;
        /**
         * 경보 규칙을 만든다. 지표 임계값(`node_metric`·`vm_metric`, `for_secs` 동안 참이면 알림)·상태(`pool_usage`·
         *     `node_offline`·`vm_failed`)·이벤트(`task_failed`·`ha_recovery`·`backup_job_failed`) 조건과 알림 대상 이름(1~16개)을
         *     받는다. admin만 쓰고 모든 노드가 `notifications`를 알아야 한다.
         */
        post: operations["create_alert_rule"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/alert-rules/sdn-defaults": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * "SDN 경보 켜기"(계획 67 §11): 권하는 SDN 규칙 여덟을 한 번에 만든다 — `sdn-quorum-lost` critical 0초,
         *     `sdn-central-down`·`sdn-chassis-down` warning 60초, `sdn-gateway-unavailable` critical 30초, `sdn-gateway-failover` info,
         *     `sdn-sync-failed` warning 120초, `sdn-physnet-not-ready` warning 60초, `sdn-lb-member-down` warning 30초. 같은 이름의
         *     규칙이 있으면 건드리지 않는다. 만든 뒤는 보통 규칙이다. admin만 쓰고 모든 노드가 `notifications`·`sdn-services`를 알아야
         *     한다.
         */
        post: operations["create_sdn_default_rules"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/alert-rules/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_alert_rule"];
        /** 규칙을 통째로 바꾼다. 활성 경보는 새 설정으로 다시 판단한다(조건이 거짓이 되면 해제, 대상이 빠지면 알림 없이 뺀다). */
        put: operations["update_alert_rule"];
        post?: never;
        /** 규칙을 지운다. 그 규칙의 활성 경보는 알림 없이 사라진다. */
        delete: operations["delete_alert_rule"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/alerts": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 활성 경보(`pending` 포함): 리더 경보(Raft)와 모든 노드의 노드 경보를 합친다. `firing` 먼저, 심각도 높은 것 먼저,
         *     오래된 것 먼저다. 답하지 않은 노드는 `unreachable`(그 노드의 노드 경보는 보이지 않고 `node_offline` 경보가 대신
         *     보인다), 경보를 모르는 옛 노드는 `unsupported`다. 지운 규칙의 경보는 뺀다.
         */
        get: operations["list_alerts"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/alerts/history": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 모든 노드가 보낸 알림 기록을 새 것부터 읽는다(보낸 노드의 `alerts/history.log`). `limit`보다 많으면 잘라
         *     `truncated`다.
         */
        get: operations["list_alert_history"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/audit": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 모든 노드의 감사 로그를 새 것부터 읽는다. admin만 읽는다. 읽지 못한 노드는 `unreachable`에,
         *     감사 로그를 끈 노드는 `disabled`에, `limit`보다 많으면 잘라 `truncated`다.
         */
        get: operations["list_audit"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/2fa": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 자기 2단계 인증 상태. 강제 등록 중인 대기 세션도 읽는다. */
        get: operations["get_two_factor"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/2fa/recovery": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** 복구 코드 10개를 새로 만든다(남은 옛 코드는 모두 못 쓴다). 요소가 있어야 하고 본인 확인이 필요하다. */
        post: operations["regenerate_recovery"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/2fa/totp": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * TOTP 등록을 시작한다: 새 비밀과 `otpauth://` URI(QR)를 한 번 준다. 첫 코드로 확인(`/auth/2fa/totp/confirm`)해야
         *     쓰인다. 확인 전에 다시 부르면 새 비밀로 바뀐다. 이미 켜져 있으면 409 `totp_enabled`(먼저 끈다).
         */
        post: operations["enroll_totp"];
        /**
         * TOTP를 끈다(등록 중인 것도 지운다). 본인 확인(`code` 또는 `password`)이 필요하다. 남은 요소가 없으면 복구 코드도
         *     지우고, 정책이 요구하는 사용자의 마지막 요소면 409 `two_factor_required`.
         */
        delete: operations["disable_totp"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/2fa/totp/confirm": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 등록 중인 TOTP를 첫 코드로 확인해 켠다. 첫 요소면 복구 코드 10개를 만들어 한 번 준다. 강제 등록 중인 대기 세션은
         *     이것으로 로그인이 끝난다. 틀리면 403 `invalid_second_factor`(시도 제한).
         */
        post: operations["confirm_totp"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/2fa/webauthn/register": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 패스키 등록을 시작한다: `navigator.credentials.create({publicKey: options})`에 넘길 옵션과 끝낼 때 돌려보낼 토큰.
         *     RP ID는 정책의 `rp_id` 또는 요청 호스트이고, IP 주소로 접속했으면 400 `webauthn_unavailable`(브라우저가 막는다).
         */
        post: operations["start_passkey"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/2fa/webauthn/register/finish": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 패스키 등록을 끝낸다(201). 첫 요소면 복구 코드 10개를 만들어 한 번 준다. 강제 등록 중인 대기 세션은 이것으로
         *     로그인이 끝난다. 토큰이 틀렸거나 지났거나 응답이 맞지 않으면 400 `invalid_passkey`.
         */
        post: operations["finish_passkey"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/2fa/webauthn/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * 패스키를 지운다. 본인 확인(`code` 또는 `password`)이 필요하고, 정책이 요구하는 사용자의 마지막 요소면 409
         *     `two_factor_required`.
         */
        delete: operations["delete_passkey"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/login": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 세션 쿠키를 발급한다. 웹 UI가 쓴다. CLI는 로그인한 뒤 API 토큰을 만들어 쓴다.
         *     실패가 이어지면 잠시 막는다(429 `too_many_attempts`, `Retry-After`).
         *     LDAP 영역 로그인은 디렉터리로 확인하고 외부 사용자 `<이름>@<영역>`을 만들거나 역할을 고친다.
         *     모르는 영역과 OIDC 영역(IdP에서 로그인한다)은 400, 맞는 역할이 없으면 403 `no_role`, 디렉터리에
         *     닿지 못하면 503 `realm_unavailable`.
         *     사용자에게 2단계 인증 요소가 있거나 정책이 요구하면(계획 62) 쿠키는 5분짜리 대기 세션이고 본문의
         *     `second_factor`가 남은 단계를 알린다. 대기 세션으로는 둘째 단계·등록 경로만 쓴다(다른 경로는 401
         *     `second_factor_required`).
         */
        post: operations["login"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/login/second-factor": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 비밀번호 로그인의 둘째 단계. `totp`·`recovery`·`webauthn` 가운데 하나를 낸다. 맞으면 같은 세션 쿠키가 정식 세션이
         *     된다(200 사용자). 틀리면 401 `invalid_second_factor`이고 로그인과 같은 시도 제한을 받는다(429). 강제 등록 중인
         *     세션은 403 `enrollment_required`(등록 경로를 쓴다).
         */
        post: operations["second_factor"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/logout": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 세션을 끝내고(모든 노드에서) 브라우저의 세션 쿠키를 지운다. 클러스터가 세션을 지우지 못하면
         *     (리더 없음 등) 5XX이고 쿠키는 그대로다.
         */
        post: operations["logout"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/oidc/callback": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * IdP가 돌려보내는 곳. 로그인 상태 쿠키(서명·10분·state 같음)를 확인하고, 인가 응답의 `iss`(RFC 9207)를
         *     보고, 코드를 토큰으로 바꾸고(리디렉트 URI는 상태 쿠키의 것), ID 토큰(서명·iss·aud·exp·iat·nbf·nonce)을
         *     확인한 뒤 이름·그룹 클레임(없으면 userinfo)으로 외부 사용자 `<이름>@<영역>`을 만들거나 역할을 고쳐
         *     세션 쿠키를 놓고 `/`로 보낸다. 외부 사용자는 처음 만든 IdP 사용자(iss·sub)에 묶인다. 실패하면
         *     `/login?error=<sso_failed|no_role|realm_unavailable>`로 보낸다.
         */
        get: operations["oidc_callback"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/oidc/{realm}/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 영역의 IdP로 로그인을 시작한다. 로그인 상태(state·nonce·PKCE 검증자)를 서명한 쿠키
         *     `opl_oidc`(10분)에 넣고 IdP의 authorization endpoint로 보낸다. OIDC 영역이 아니면
         *     `/login?error=sso_failed`, IdP의 discovery 문서를 읽지 못하면 `/login?error=realm_unavailable`로 보낸다.
         */
        get: operations["oidc_start"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/realms": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 로그인할 수 있는 영역. 로그인 전에 읽는다. `local`이 먼저, 나머지는 이름 순이다. */
        get: operations["list_login_realms"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/whoami": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["whoami"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backup-jobs": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_backup_jobs"];
        put?: never;
        /** 작업을 만들고 다음 실행 시각(`status.next_run`)을 적는다. 이름이 겹치면 409 `name_taken`. */
        post: operations["create_backup_job"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backup-jobs/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_backup_job"];
        put?: never;
        post?: never;
        /** 작업만 지운다. 작업이 만든 백업은 남는다(백업의 `job`도 그대로). */
        delete: operations["delete_backup_job"];
        options?: never;
        head?: never;
        /**
         * 스펙의 주어진 값을 바꾼다. 아무것도 없으면 400. 일정을 바꾸거나 작업을 다시 켜면 다음 실행 시각을
         *     지금부터 다시 계산한다(꺼 둔 사이 지난 실행은 하지 않는다). 다만 켜 둔 작업의 지금 실행 요청이
         *     기다리고 있으면(다음 실행이 지났으면) 그대로 둔다. 진행 중인 실행은 그대로 끝까지 간다.
         *     `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다.
         */
        patch: operations["patch_backup_job"];
        trace?: never;
    };
    "/backup-jobs/{key}/run": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 다음 실행 시각을 지금으로 당긴다. 리더의 스케줄러가 다음 차례에 실행을 시작한다. 실행이 진행
         *     중이면 409 `job_running`, 꺼 둔 작업이면 409 `job_disabled`.
         */
        post: operations["run_backup_job"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backup-stores": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_stores"];
        put?: never;
        /**
         * 저장소를 만들고 초기화 Task를 돌려준다(201 — 저장소는 `initializing`). 쓰기 저장소는 `store.json`·주인 표식을 쓰고,
         *     읽기 전용은 있는 저장소를 키로 열어 본다. 암호화는 새 키(`generate` — 응답에 복구 키가 한 번 들어 있다), 복구 키,
         *     저장소 안 escrow 가운데 하나다. API 노드가 저장소에 닿으면 미리 확인한다: 키 지문이 다르면 400 `store_key_mismatch`,
         *     암호화 저장소인데 키가 없으면 400 `store_key_required`, 다른 클러스터가 주인인 곳에 쓰기 저장소면 409
         *     `store_owned_elsewhere`. 같은 풀·버킷/접두사의 저장소가 있으면 409 `store_target_in_use`, 형식이 틀리면 400
         *     `invalid_backup_store`, 옛 노드가 있으면 409 `cluster_upgrading`.
         */
        post: operations["create_store"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backup-stores/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_store"];
        put?: never;
        post?: never;
        /**
         * 저장소 리소스를 지운다(200). 저장소 안 데이터는 그대로다(버킷·디렉터리는 손으로). 저장소의 백업이 남아 있으면 409
         *     `store_in_use`, 저장소 Task나 저장소 백업을 쓰는 Task가 끝나지 않았으면 409 `store_busy`.
         */
        delete: operations["delete_store"];
        options?: never;
        head?: never;
        /**
         * 일정·검증·동시 요청 수·S3 자격 증명·CA를 바꾼다. 백엔드·암호화는 바꾸지 못한다(그 칸은 받지 않는다 — 400). 일정이
         *     바뀌면 다음 실행 시각을 다시 고른다.
         */
        patch: operations["patch_store"];
        trace?: never;
    };
    "/backup-stores/{key}/actions/check": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 연결 확인(200): API 노드가 그 저장소를 쓸 수 있으면 `store.json`을 읽어 키를 맞춰 보고, 노드마다 보고한 닿음을 함께
         *     돌려준다.
         */
        post: operations["check_store"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backup-stores/{key}/actions/gc": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * GC를 지금 돌린다(202 Task). 준비되지 않았으면 409 `store_not_ready`, 읽기 전용이면 409 `store_read_only`, 이미 돌면
         *     409 `store_busy`.
         */
        post: operations["gc_store"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backup-stores/{key}/actions/scan": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 읽기 전용 저장소의 스냅샷마다 백업 리소스를 만든다(202 Task — §12.6). 다시 하면 새것만 더한다. 읽기 전용이 아니면
         *     409 `store_read_only`의 반대 — 400.
         */
        post: operations["scan_store"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backup-stores/{key}/actions/take-over": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 읽기 전용으로 붙인 저장소를 넘겨받는다(200 — §12.6): 주인 표식(`owner.json`)의 하트비트가 24시간보다 오래됐거나
         *     `force`면 주인을 이 클러스터로 바꾸고 `read_only`를 끈다. 옛 주인은 다음 GC 때 바뀐 표식을 보고 그 저장소를 멈춘다.
         *     하트비트가 새로우면 409 `store_owned_elsewhere`, 읽기 전용이 아니면 400.
         */
        post: operations["take_over"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backup-stores/{key}/actions/verify": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 검증을 지금 돌린다(202 Task). `backups`가 비면 일정의 규칙대로 고른다. 읽기 전용 저장소도 검증한다(깨진 청크를
         *     옮기지 않고 결과만 적는다).
         */
        post: operations["verify_store"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backup-stores/{key}/key/escrow": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 주 키를 암호 문구로 감싸 저장소 안(`keys/<지문>.json`)에 쓴다(204 — 덮어씀). 암호 문구는 저장하지 않는다. 평문
         *     저장소면 400, 읽기 전용이면 409 `store_read_only`, API 노드가 그 저장소를 쓸 수 없으면 409 `store_not_on_node`,
         *     닿지 않으면 503 `store_unavailable`.
         */
        post: operations["escrow_key_handler"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backup-stores/{key}/key/export": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** 주 키를 복구 키로 돌려준다(200, 감사에 남음). 평문 저장소면 400. */
        post: operations["export_key"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backups": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 모든 백업. `vm`(id나 이름)을 주면 그 VM의 백업만이다. */
        get: operations["list_backups"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backups/receive": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 다른 클러스터의 백업 사본을 받기 시작한다(201): 매니페스트의 VM·스펙·디스크로 `receiving` 백업을
         *     만들고 백업 풀에 그 디렉터리를 만든다. 풀이 없거나 백업 풀이 아니거나 매니페스트가 올바르지 않으면
         *     400, 풀을 이 노드에서 쓸 수 없으면 409 `pool_not_on_node`·`pool_unavailable`, 같은 원본(CA 지문과
         *     원본 id)의 사본이 같은 풀에 있으면 409 `replica_exists`(`details.backup`이 그 id), 풀에 디스크 파일을
         *     받을 자리가 없으면 507 `insufficient_storage`, 옛 노드가 있으면 409 `cluster_upgrading`. 증분 백업은
         *     `parent`로 이 클러스터의 부모 사본을 함께 보낸다: 맞는 부모 사본이 없으면 409
         *     `replica_parent_missing`, 증분 받기(`incremental-replication`)를 모르는 노드가 있으면 409
         *     `cluster_upgrading`.
         */
        post: operations["receive_backup"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backups/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_backup"];
        put?: never;
        post?: never;
        /**
         * 백업 파일과 리소스를 지울 Task를 돌려준다(202). 실패한 백업도 지운다. 백업을 만들거나 되살리거나
         *     지우는 Task가 끝나지 않았으면 409 `backup_busy`. Task는 파일이 있는 노드(공유가 아닌 풀은 파일을
         *     쓴 노드, 공유 풀은 백업 풀을 쓸 수 있는 노드)에서 돈다. 풀이 없어졌으면 이 노드의 Task가 리소스만
         *     지운다. 파일을 지울 노드가 없으면 409(`node_offline`, `pool_unavailable` 등)이고, `force`면 리소스만
         *     바로 지우고 200이다(파일은 백업 풀에 남는다). 증분 백업(`failed`가 아닌 것)의 부모는 `force`여도 409
         *     `backup_in_use`다(자식부터 지운다).
         */
        delete: operations["delete_backup"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backups/{key}/actions/complete": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 받기를 끝낸다(200, 백업): 모든 파일이 있고 디스크 파일 크기가 매니페스트와 같으면 매니페스트를
         *     (`backup`은 이 클러스터의 백업 id로, 증분이면 버전 2에 `parent`는 부모 사본 id로) 쓰고 `ready`로
         *     바꾼다. 빠지거나 크기가 다른 파일이 있으면 409 `backup_incomplete`(`details.files`가 그 파일들), 받는
         *     중이 아니면 409 `backup_not_receiving`, 지우는 Task가 걸려 있으면 409 `backup_busy`, 증분 사본의 부모
         *     사본이 없어졌거나 `ready`가 아니거나 지우는 중이면 409 `replica_parent_missing`.
         */
        post: operations["complete_backup"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backups/{key}/actions/convert": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 옛 풀 백업을 저장소로 옮긴다(202 — `backup-convert` Task, 계획 68 §12.2). 원본은 남는다. 저장소 백업이면 400, 준비되지
         *     않았으면 409 `backup_not_ready`, 같은 저장소로 이미 옮겼으면 409 `backup_exists`, 저장소가 읽기 전용이면 409
         *     `store_read_only`, 파일이 있는 노드가 저장소를 쓸 수 없으면 409 `store_not_on_node`.
         */
        post: operations["convert_backup"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backups/{key}/actions/copy": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 저장소 백업을 다른 저장소로 복사한다(202 — `backup-copy` Task, 계획 68 §12.5). 대상에 없는 청크만 쓴다(키가 다르면
         *     다시 암호화). 풀 백업이면 400(convert), 같은 저장소면 400, 이미 복사했으면 409 `backup_exists`, 대상이 읽기 전용이면
         *     409 `store_read_only`, 두 저장소를 함께 쓰는 온라인 노드가 없으면 409 `no_node`.
         */
        post: operations["copy_backup"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backups/{key}/actions/replicate": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `ready` 백업을 원격 클러스터 `remote`의 백업 풀 `pool`에 사본으로 보낼 Task를 돌려준다(202). Task는
         *     백업 파일이 있는 노드(공유가 아닌 풀은 파일을 쓴 노드)에서 돌고, 원격의 받기 API로 파일을 보내며 원본
         *     `status.replicas`에 결과를 적는다. 원격 쪽 권한은 원격 토큰 주인의 역할이다(백업 쓰기, operator
         *     이상). 증분 백업은 체인의 앞 백업(원격에 사본이 없는 것)부터 차례로 보낸다(원격의 모든 노드가 증분
         *     받기를 알아야 한다). 원격이 없으면 404, 백업이 준비되지 않았으면 409 `backup_not_ready`, 지우는 중이거나
         *     같은 전체 백업에서 시작한 체인의 어느 백업이든(조상·자식·형제) 같은 원격으로 복제하는 Task가 끝나지
         *     않았으면 409 `backup_busy`, 파일을 읽을 노드가 없으면 409(`node_offline`, `pool_unavailable` 등), 옛
         *     노드가 있으면 409 `cluster_upgrading`.
         */
        post: operations["replicate_backup"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backups/{key}/actions/verify": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 저장소 백업을 지금 검증한다(202 — 저장소의 검증 Task, 계획 68 §10.5). 백업 풀의 백업은 400(저장소 백업만 검증한다),
         *     준비되지 않았으면 409 `backup_not_ready`, 저장소가 준비되지 않았으면 409 `store_not_ready`.
         */
        post: operations["verify_backup"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backups/{key}/chunks/missing": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** 받는 중인 저장소 사본에 보낼 청크 가운데 받는 쪽 저장소에 없는 것(200). 4096개까지 묻는다(400). */
        post: operations["missing_chunks"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backups/{key}/chunks/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * 저장소 사본의 청크 객체 하나를 받는다(204): 받는 쪽 키로 풀어 id·인증 태그를 맞춰 본 뒤 그대로 쓴다. 맞지 않으면 400.
         *     크기 상한은 4 MiB 청크와 머리·태그다.
         */
        put: operations["put_chunk"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backups/{key}/files/{file}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * 받는 중인 백업의 파일 하나를 받는다(200): `{file}.part`에 쓰고 디스크에 내려 쓴 뒤 이름을 바꾸고,
         *     받은 내용의 SHA-256과 크기를 돌려주며, 디스크 파일의 해시와 받은 시각(`received_at`)을 백업 상태에
         *     적는다. 같은 파일을 다시 보내면 덮는다. `file`은 매니페스트의 디스크 파일이나(UEFI 변수가 있으면)
         *     `vars.fd`, (TPM 상태가 있으면) `tpm2-00.permall`이어야 하고, `Content-Length`는 디스크 파일이면 매니페스트의
         *     크기, `vars.fd`면 64 MiB, `tpm2-00.permall`이면 4 MiB까지다(400). 본문 조각이 60초 넘게 오지 않으면 받은 것을 버리고 400(멈춘 업로드). `Content-Length`가 없으면
         *     411, 받는 중이 아니면 409 `backup_not_receiving`, 파일이 다른 노드에 있으면 409 `backup_on_other_node`,
         *     같은 파일을 받고 있으면 409 `backup_busy`.
         */
        put: operations["put_backup_file"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/backups/{key}/restore": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `ready` 백업을 새 VM `name`으로 되살릴 Task를 돌려준다(202). Task의 대상은 미리 정한 새 VM id다.
         *     백업의 VM 스펙이 쓰는 네트워크·보안 그룹이 지금 있어야 한다(400). 백업이 준비되지 않았으면 409
         *     `backup_not_ready`, 지우는 중이면 409 `backup_busy`, 이름이 겹치면 409 `name_taken`. 노드는 `node`나
         *     백업 풀과 대상 풀을 함께 쓸 수 있는 온라인 노드 가운데 이름이 가장 앞선 노드다(없으면 409 `no_node`).
         *     공유가 아닌 풀의 백업은 파일을 쓴 노드(`status.node`)에서만 되살린다: `node`가 다르면 409
         *     `backup_on_other_node`. 고른 노드가 오프라인이면 409 `node_offline`. 증분 백업은 고른 노드가 증분 백업을
         *     알아야 한다(버전 2 매니페스트, 409 `cluster_upgrading`). TPM 상태가 있거나 VM 스펙에 `tpm`·`secure_boot`가 있는
         *     백업은 모든 노드가 `vtpm`을, `guest_agent`가 있는 백업은 `guest-agent`를 알아야 한다(409 `cluster_upgrading`).
         *     전용 CPU·vNUMA·hugepages가 있는 백업은 모든 노드가 `numa`를 알아야 하고(409 `cluster_upgrading`), 전용 CPU·hugepages는
         *     새 폴더의 `vm.dedicated`가 든다(403, 계획 64).
         */
        post: operations["restore_backup"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/ceph": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Orbivisor가 관리하는 Ceph 클러스터(키는 넣지 않는다). 끝나지 않은 Ceph Task의 id를 함께 보인다. 없으면 404다. */
        get: operations["get_ceph"];
        put?: never;
        /**
         * Ceph를 세운다: 키·fsid를 만들고 `Ceph` 리소스(`bootstrapping`)와 노드 `node`의 `ceph-init` Task(202)를 만든다.
         *     그 노드가 Ceph 꾸러미를 설치하고 첫 mon·mgr을 세운다. `public_network`가 없으면 노드 주소가 든 인터페이스의
         *     망이다. 부트스트랩이 실패했으면 같은 노드로 다시 부르면 새 Task만 만들어 이어서 한다. 노드가 없거나 위트니스·망
         *     문법·망 안 주소가 없으면 400, 오프라인이면 409 `node_offline`, 옛 노드가 남았으면 409 `cluster_upgrading`,
         *     이미 있으면(다른 노드에서 세우는 중 포함) 409 `ceph_exists`, 부트스트랩 Task가 끝나지 않았으면 409 `ceph_busy`.
         */
        post: operations["init_ceph"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/ceph/clients": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 노드 `node`에 Ceph 클라이언트 꾸러미(`ceph-common`·`qemu-block-extra`)를 설치하는 `ceph-client-install`
         *     Task(202). 클라이언트 설정은 노드마다 이미 쓰여 있다. 노드가 없거나 위트니스면 400, 오프라인이면 409
         *     `node_offline`.
         */
        post: operations["install_client"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/ceph/mgrs": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 노드 `node`에 (대기) mgr을 더하는 `ceph-mgr-add` Task(202). 오류는 mon 더하기와 같다(상한은 없다 — 노드마다
         *     하나).
         */
        post: operations["add_mgr"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/ceph/mons": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 노드 `node`에 mon을 더하는 `ceph-mon-add` Task(202). mon은 5개까지(409 `ceph_limit`)이고 홀수(3·5)가 정족수를
         *     가장 잘 지킨다. 부트스트랩 전이면 409 `ceph_not_ready`, 다른 mon·mgr Task가 끝나지 않았으면 409 `ceph_busy`,
         *     그 노드에 이미 mon이 있으면 409 `ceph_exists`, 노드가 없거나 위트니스·망 안 주소가 없으면 400.
         */
        post: operations["add_mon"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/ceph/osds": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 노드 `node`의 디스크 `device`(`/dev/sdb`나 by-id 경로)에 OSD를 만드는 `ceph-osd-add` Task(202). 먼저 그 노드의
         *     디스크 목록으로 확인한다: 그런 디스크가 없으면 400, 쓰는 중이면 409 `disk_in_use`, 비지 않았는데 `wipe`가
         *     아니면 409 `disk_not_empty`. Task에는 by-id 경로가 있으면 그것을 적는다(재부팅으로 이름이 바뀌어도 같은 디스크).
         *     부트스트랩 전이면 409 `ceph_not_ready`.
         */
        post: operations["add_osd"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/ceph/pools": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Ceph 풀을 만들어 Orbivisor RBD 풀 `name`(사용자 `orbivisor`, 관리 conf)으로 등록하는 `ceph-pool-create` Task(202,
         *     온라인 mon 노드 가운데 id가 가장 작은 노드에서 돈다). `pool`이 없으면 Ceph 풀 이름은 `name`, `size`가 없으면
         *     min(3, OSD가 up인 호스트 수)다(호스트가 하나면 size 1을 적어야 한다). size는 1~3이고 호스트 수보다 클 수
         *     없다(400). 이름이 잘못됐으면 400, 이미 있는 풀 이름이면 409 `pool_exists`, 부트스트랩 전이거나 OSD가 없으면
         *     409 `ceph_not_ready`, Ceph 상태를 읽지 못하면 503 `ceph_unreachable`.
         */
        post: operations["create_ceph_pool"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/ceph/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Ceph 상태(health·mon·mgr·OSD·PG·사용량·풀). Raft가 아닌 mon 노드에서 읽는다(이 노드가 mon이면 직접, 아니면 온라인
         *     mon 노드에 노드 사이 RPC로). 잠깐(`[ceph] status_cache_secs`, 기본 5초) 둔 것을 돌려주고 `refresh`면 새로 읽는다.
         *     지금 읽지 못하면 마지막 보고서를 `stale`과 `error`로 돌려주고, 그것도 없으면 503 `ceph_unreachable`이다. mon
         *     노드가 이 RPC를 모르면 409 `cluster_upgrading`, 없으면 404.
         */
        get: operations["get_ceph_status"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/cluster": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 클러스터 이름, CA 지문, 멤버(이 노드가 아는 Raft 멤버십과 관찰 노드) */
        get: operations["get_cluster"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/cluster/api-endpoints": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `api` 역할 노드(기본: 위트니스가 아닌 노드)의 API 주소. 앞단은 이 노드들을 부르고
         *     `GET /health`로 준비되지 않은 노드를 뺀다.
         */
        get: operations["list_api_endpoints"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/cluster/ca": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 클러스터 CA 인증서(PEM). 인증 없이 읽는다. 받는 쪽은 반드시 지문을 확인해야 한다. */
        get: operations["get_ca"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/cluster/join": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** 새 노드의 합류 요청. 조인 토큰으로 인증한다(사용자 인증 없음). */
        post: operations["join"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/cluster/join-tokens": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["create_join_token"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/cpu/baseline": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 클러스터 CPU 기준(§9): 설정의 모드·자동 플래그·숨김 목록과 아키텍처마다 리더가 기록한 기준·셈에 든 노드·낮춘 노드·
         *     경고·`cluster` VM 수
         */
        get: operations["get_cpu_baseline"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/cpu/flags": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 아키텍처의 CPU 플래그(노드 QEMU가 아는 이름, 이름순)와 그것을 줄 수 있는 노드 수. VM 플래그·숨김 목록 입력의 자동
         *     완성에 쓴다.
         */
        get: operations["list_cpu_flags"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/cpu/models": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 아키텍처의 CPU 모델(§13): `host`·`max`·`cluster`(지금 기준)·`x86-64-v*`(x86_64)와 노드 카탈로그의 QEMU 모델(이름순,
         *     별칭 포함 — 별칭은 저장할 때 버전 모델로 고정된다). 모델마다 켤 수 있는 노드 수와, 루트 `cluster.read`가 있으면
         *     노드마다 켤 수 있는지·빠진 플래그다(숨김 목록·기준을 지금처럼 적용). VM을 만들거나 고칠 수 있는 사용자가 읽는다.
         */
        get: operations["list_cpu_models"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/floating-ips": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 볼 수 있는 유동 IP와 지금 모양(분산·중앙·라우팅 안 됨) */
        get: operations["list_floating_ips"];
        put?: never;
        /**
         * 유동 IP를 만든다(계획 66 §9.5): 폴더의 `network.config`와 공급자 네트워크의 `network.assign`, 붙이면 그 VM의
         *     `vm.config`. 주소는 API가 유동 IP 풀(없으면 풀)의 IPv4에서 고른다(준 주소는 서브넷 안의 빈 IPv4이고, 루트
         *     `cluster.admin`이 아니면 그 범위 안 — 아니면 400). 공급자 네트워크가
         *     아니면 400, 주소는 409 `ip_in_use`·`ip_exhausted`, 붙일 NIC가 라우팅되지 않으면 409 `fip_not_routable`, 이미 유동 IP가
         *     붙은 NIC는 409 `fip_in_use`, 폴더 할당량 `floating_ips`는 409 `quota_exceeded`.
         */
        post: operations["create_floating_ip"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/floating-ips/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_floating_ip"];
        put?: never;
        post?: never;
        /** 유동 IP를 지운다(붙어 있어도 — NAT가 사라진다). */
        delete: operations["delete_floating_ip"];
        options?: never;
        head?: never;
        /**
         * 유동 IP를 붙이거나 뗀다(유동 IP의 `network.config`, 붙이면 대상 VM의 `vm.config`). **떼기만**(`{"target": null}`)은
         *     지금 붙은 VM의 `vm.config`로도 된다(계획 67 §13 — 권한을 거둔 뒤 남은 유동 IP를 VM 주인이 떼는 길, 유동 IP를 읽을 수
         *     없어도). 붙일 NIC의 검사는 만들기와 같다. `If-Match`가 없으면 읽은 리비전에 쓰고 그새 바뀌었으면 다시 읽는다.
         */
        patch: operations["patch_floating_ip"];
        trace?: never;
    };
    "/folders": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_folders"];
        put?: never;
        /**
         * 폴더를 만든다. 부모(없으면 루트)의 `inventory.folder`가 있어야 한다. 부모가 없거나 다른 트리면 404, 깊이 16을 넘으면
         *     409 `folder_too_deep`, 형제 이름이 겹치면 409 `name_taken`.
         */
        post: operations["create_folder"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/folders/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_folder"];
        put?: never;
        post?: never;
        /** 빈 폴더를 지운다(그 폴더의 `inventory.folder`). 자식 폴더나 리소스가 있으면 409 `folder_not_empty`. */
        delete: operations["delete_folder"];
        options?: never;
        head?: never;
        /**
         * 폴더의 이름·부모·할당량을 바꾼다. 이름은 그 폴더의 `inventory.folder`, 부모는 그 폴더와 새 부모(없으면 루트)의
         *     `inventory.move`, 할당량은 부모(없으면 루트)의 `inventory.quota`(팀 관리자가 자기 상한을 올리지 못하게)가 있어야
         *     한다. 순환은 409 `folder_cycle`, 깊이 16을 넘으면 409 `folder_too_deep`, 형제 이름이 겹치면 409 `name_taken`.
         */
        patch: operations["patch_folder"];
        trace?: never;
    };
    "/folders/{id}/members": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 리소스를 이 폴더로 옮긴다(일괄, 항목마다 결과). 대상 폴더의 `inventory.move`와 항목마다 그 리소스(원본 쪽)의
         *     `inventory.move`가 있어야 한다. 폴더 id 대신 `root`면 그 트리의 루트로 옮긴다(루트의 `inventory.move`). 항목의
         *     실패(볼 수 없으면 404, 권한 403, 다른 트리 400)는 그 항목의 `error`에 담고 응답은 200이다.
         */
        post: operations["move_members"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/folders/{id}/usage": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 폴더의 하위 트리 사용량(vCPU·메모리·붙은 디스크·VM 수)과 그 폴더·조상의 할당량, 차원마다 묶는 폴더. 볼 수 없는
         *     조상(폴더 자체를 읽지 못함)은 `limits`·`binding`에서 뺀다.
         */
        get: operations["folder_usage"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/grants": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 볼 수 있는 grant(`cluster.admin`은 전부, 그 밖에는 자기·자기 그룹의 것과 `permission.modify`가 있는 대상의 것).
         *     `target`·`principal`로 좁힌다.
         */
        get: operations["list_grants"];
        put?: never;
        /**
         * grant를 준다. 루트 grant는 `cluster.admin`, 폴더·객체 grant는 대상의 `permission.modify`가 있어야 하고 역할의 권한이
         *     그 대상에서 자기 유효 권한 안이어야 한다(403). 주체·역할이 없으면 400, 대상이 없거나 볼 수 없으면 404.
         */
        post: operations["create_grant"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/grants/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_grant"];
        put?: never;
        post?: never;
        /**
         * grant를 뺀다. 루트 grant는 `cluster.admin`, 그 밖에는 대상의 `permission.modify`(없는 대상에 남은 grant는 루트
         *     관리자만). 볼 수 없으면 404.
         */
        delete: operations["delete_grant"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/groups": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_groups"];
        put?: never;
        /**
         * 로컬 그룹을 만든다(`cluster.admin`). `members`는 사용자 id, `external`은 영역의 디렉터리 그룹이다(그 영역 사용자가
         *     로그인 때 그 그룹에 있었으면 든다).
         */
        post: operations["create_group"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/groups/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_group"];
        put: operations["update_group"];
        post?: never;
        /** 그룹을 지운다(`cluster.admin`). 이 그룹에 건 grant가 있으면 409 `group_in_use`. */
        delete: operations["delete_group"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/ha-checks": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_ha_checks"];
        put?: never;
        /**
         * 노드가 5초마다 링크·핑·스토리지 풀 경로를 검사하고, 리더가 `grace_secs` 넘게 실패한 검사에
         *     `on_fail`대로 대응한다(경고·라이브 마이그레이션·다른 노드에서 다시 켜기·전원 끄기). `nodes`는 노드별
         *     대상이다. 모든 노드가 `ha-checks`를 알아야 한다.
         */
        post: operations["create_ha_check"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/ha-checks/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_ha_check"];
        /** 검사를 통째로 바꾼다. 진행 중인 실패는 새 설정으로 다시 판단한다. */
        put: operations["update_ha_check"];
        post?: never;
        delete: operations["delete_ha_check"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/ha-groups": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_ha_groups"];
        put?: never;
        /**
         * HA VM이 `ha.group`으로 쓰는 노드 묶음과 우선순위(클수록 먼저). `restricted`면 그룹 밖 노드에 놓지 않고,
         *     `nofailback`이면 우선 노드가 돌아와도 되돌아가지 않는다. 모든 노드가 `ha-placement`를 알아야 한다.
         */
        post: operations["create_ha_group"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/ha-groups/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_ha_group"];
        /** 그룹을 통째로 바꾼다. 이미 놓인 VM은 옮기지 않는다. */
        put: operations["update_ha_group"];
        post?: never;
        /** VM이 쓰고 있으면 409 `group_in_use` */
        delete: operations["delete_ha_group"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/health": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 이 노드의 헬스. 인증 없이 읽는다. `?live`는 언제나 200, 기본(`?ready`)은 실패한 확인이 있으면 503이다.
         *     로그인하지 않았으면 `status`·`failed`만 답한다.
         */
        get: operations["get_health"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/image-catalog": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 이미지 카탈로그: 내장 항목과 관리자 항목. `arch`를 주면 그 아키텍처만. 항목마다 그 URL에서 이미 받아 준비된 볼륨을
         *     `volumes`로 함께 준다. 관리자(설정 쓰기 권한)가 아니면 URL의 쿼리 문자열과 사용자 정보를 가린다.
         */
        get: operations["list_catalog"];
        put?: never;
        /**
         * 관리자 카탈로그 항목을 더한다(201). 같은 `(id, arch)`가 있으면 409 `already_exists`, 내장 항목의 id(아키텍처와
         *     상관없이)·잘못된 id·URL·해시 길이·200개 넘음은 400이다. 고정 해시는 소문자로 저장한다. 모든 노드가 `templates`를
         *     알아야 한다(409 `cluster_upgrading`).
         */
        post: operations["add_catalog_image"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/image-catalog/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** 관리자 카탈로그 항목을 지운다(204). 이미 받은 볼륨과 템플릿은 그대로다. 내장 항목은 지울 수 없고(400), 없으면 404다. */
        delete: operations["remove_catalog_image"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/image-catalog/{id}/download": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 카탈로그 이미지를 풀에 받는 Task(`volume-download`)를 돌려준다(202). 항목의 체크섬(고정 해시나 체크섬 파일)으로
         *     확인하고, `template`이 있으면 가져온 뒤 디스크를 늘리고 꺼진 템플릿 VM을 만든다(그 단계가 실패해도 받은 볼륨은
         *     남는다). 항목이 없으면 404, 볼륨이나 VM 이름이 쓰였으면 409 `name_taken`, 템플릿의 네트워크가 없거나 크기가
         *     이미지보다 작으면 400, 풀을 쓸 수 있는 노드 가운데 항목의 아키텍처인 노드가 없으면 400 `machine_arch_mismatch`다.
         *     볼륨 쓰기 권한(템플릿이면 VM 쓰기도)이 있어야 하고, 모든 노드가 `templates`를 알아야 한다(409 `cluster_upgrading`).
         */
        post: operations["download_catalog_image"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/load-balancers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 볼 수 있는 부하 분산기와 VIP·멤버의 지금 모양. `live=false`면 OVN을 읽지 않는다. */
        get: operations["list_load_balancers"];
        put?: never;
        /**
         * 부하 분산기를 만든다(계획 67 §7): 폴더의 `network.config`, VIP 네트워크의 `network.assign`, NIC 멤버의 VM마다
         *     `vm.config`, 주소 멤버의 네트워크와 바깥 VIP 네트워크의 `network.assign`. VIP·건강 검사 주소·바깥 VIP는 API가
         *     고른다(§6). 옛 노드가 남았으면 409 `cluster_upgrading`, SDN이 없으면 409 `sdn_not_ready`, 스펙은 400
         *     `invalid_load_balancer`, VIP 네트워크·바깥이 라우터와 맞지 않으면 400 `lb_network_not_routed`, 닿지 않는 멤버는 400
         *     `lb_member_unreachable`, 주소는 409 `ip_in_use`·`ip_exhausted`, 같은 VIP·포트는 409, 폴더 할당량 `load_balancers`(바깥
         *     VIP는 `floating_ips`도)는 409 `quota_exceeded`. 멤버 보안 그룹이 클라이언트·건강 검사를 막거나 바깥 VIP의 게이트웨이
         *     후보가 없으면 만들고 `orbivisor-warning`으로 알린다.
         */
        post: operations["create_load_balancer"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/load-balancers/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 부하 분산기 하나와 VIP·멤버의 지금 모양(멤버 건강은 SB `Service_Monitor`). `live=false`면 OVN을 읽지 않는다. */
        get: operations["get_load_balancer"];
        put?: never;
        post?: never;
        /** 부하 분산기를 지운다(언제든 — VIP·바깥 VIP·건강 검사 주소가 풀린다). */
        delete: operations["delete_load_balancer"];
        options?: never;
        head?: never;
        /**
         * 부하 분산기의 설명·바깥 VIP·리스너·출발지 고정을 바꾼다(`If-Match`로 리비전 확인, 없으면 읽은 리비전에 쓰고 그새
         *     바뀌었으면 다시 읽는다). 그 객체의 `network.config`(경로표), VIP 네트워크의 `network.assign`, 새 NIC 멤버의 VM마다
         *     `vm.config`, 새 주소 멤버·새 바깥 네트워크의 `network.assign`. 검사·주소·경고는 만들기와 같다. 건강 검사 주소는
         *     필요한 멤버 네트워크만 남긴다.
         */
        patch: operations["patch_load_balancer"];
        trace?: never;
    };
    "/metrics": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 이 노드의 Prometheus 지표(텍스트 형식 0.0.4, gauge). 이 노드와 이 노드에서 잰 VM의 마지막 5초 점, 이 노드에서
         *     준비된 풀, 클러스터 값(노드 수·VM 수·리더)이다. 노드마다 따로 긁는다. 루트 `cluster.read`와 루트 아래 모든 VM의
         *     읽기(전역 역할이나 propagate 루트 grant)가 필요하고(viewer 토큰이면 된다), 설정 `prometheus.enabled`가 꺼져 있으면 404 `not_found`다. 5초 캐시한다.
         */
        get: operations["get_prometheus"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/metrics/cluster": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 클러스터 전체의 기록 요약: 노드 열을 합친 클러스터 열(CPU %는 코어 수로 가중, 나머지는 합, AVG만), 노드 줄(20개
         *     넘으면 범위 평균 CPU 상위 20개), 범위 평균 CPU 상위 VM 10개, 풀의 지금 사용량(Raft). 답하지 않은 노드는
         *     `unreachable`, 기록을 모르는 옛 노드는 `unsupported`, 시계가 크게 어긋나 다른 층·칸으로 답한 노드는 `skewed`다.
         *     같은 조건은 이 노드에서 10초 동안 다시 묻지 않는다.
         */
        get: operations["get_cluster_history"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/metrics/nodes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 모든 노드(위트니스 제외)의 노드 지표. 답하지 않은 노드는 `unreachable`, 지표를 모르는 옛 노드는
         *     `unsupported`에 이름이 들어간다.
         */
        get: operations["list_node_metrics"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/network-migrations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 볼 수 있는 네트워크 옮기기(끝난 것도 기록으로 남는다) */
        get: operations["list_network_migrations"];
        put?: never;
        /**
         * 옮기기를 계획하고 만든다(계획 67 §9.2): `dry_run`이면 리소스 없이 계획만(200), 아니면 계획을 `status.vms`에 적은
         *     `planned` 리소스(201 — 시작은 `POST …/start`). 이름을 주지 않으면 `<원본>-to-<대상>`(쓰였으면 `-2` …)이다. 켜진 VM의
         *     주소는 게스트 에이전트가 알린 것을 쓴다. 부른 사용자가 읽을 수 없는 VM은 계획에 없고 수(`hidden`)만 있다. 원본의
         *     `network.config`·대상의 `network.assign`, 폴더(없으면 원본 네트워크의 폴더)의 `network.config`. 권한이 없는 VM은
         *     계획에서 막힌다. 옛 노드가 남았으면 409 `cluster_upgrading`, SDN이 없으면 409 `sdn_not_ready`, 스펙·네트워크는 400
         *     `invalid_migration`, 같은 원본·같은 다리 대상의 끝나지 않은 옮기기·다른 옮기기의 VM은 409 `migration_overlap`. 계획에
         *     경고가 있으면
         *     `orbivisor-warning`에 그 수.
         */
        post: operations["create_network_migration"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/network-migrations/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 네트워크 옮기기 하나(VM NIC마다 방식·주소·상태, 다리) */
        get: operations["get_network_migration"];
        put?: never;
        post?: never;
        /**
         * 옮기기 리소스를 지운다(끝난 기록도). 도는 중이면 409 `migration_in_progress`(먼저 `pause`). 놓인 다리는 함께 걷힌다
         *     (OvnSync·다리 노드가 리소스를 따른다). 옮긴 VM은 그대로다.
         */
        delete: operations["delete_network_migration"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/network-migrations/{key}/finish": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 옮기기를 끝낸다(Task, 202 — §9.7): 다리를 걷고 `router`가 있으면 대상 네트워크를 그 라우터 인터페이스로 붙인다(라우터와
         *     대상 네트워크의 `network.config` — 라우터 `PATCH`와 같다). 원본에 남은 NIC는 Task 경고다.
         */
        post: operations["finish_network_migration"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/network-migrations/{key}/pause": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** 도는 옮기기를 멈춘다(지금 옮기는 VM이 끝난 뒤 Task가 멈춘다). `running`이 아니면 409 `migration_not_running`. */
        post: operations["pause_network_migration"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/network-migrations/{key}/resume": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 멈췄거나 실패한 옮기기를 이어 간다(시작과 같은 Task — 옮긴 VM은 건너뛰고 `failed`는 다시 해 본다). 다리 노드를
         *     지워 멈췄으면 `bridge_node`로 다른 노드를 고른다.
         */
        post: operations["resume_network_migration"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/network-migrations/{key}/rollback": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * VM을 원본 네트워크로 되돌린다(Task, 202 — §9.6): 켜진 VM은 라이브(MTU가 줄면 다시 켬), 꺼진 VM은 스펙만. 원본의
         *     보안 그룹·cloud-init 항목을 되살리고 주소를 놓는다. 모두 되돌리면 다리를 걷고 `rolled-back`. 옮기기에 없는 VM은 400.
         */
        post: operations["rollback_network_migration"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/network-migrations/{key}/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 옮기기를 시작한다(`network-migrate` Task, 202): 다리를 놓고 VM NIC를 계획대로 옮긴다(VM마다 그때 다시 계획 — 달라진
         *     VM은 `failed`). 원본 `network.config`·대상 `network.assign`·남은 VM마다 `vm.config`. 끝난 옮기기는 409
         *     `migration_finished`, 이미 Task가 돌면 409 `migration_in_progress`.
         */
        post: operations["start_network_migration"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/network-topology": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 네트워크·라우터·게이트웨이·부하 분산기·유동 IP·VM을 한 그림으로(계획 67 §12.3). 읽을 수 있는 것만 담고 나머지는
         *     `hidden`에 센다. `folder`면 그 네트워크 폴더(하위 포함)의 것과 그 네트워크에 NIC가 있는 VM만.
         */
        get: operations["network_topology"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/networks": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_networks"];
        put?: never;
        /**
         * 노드에 미리 만든 Linux 브리지를 가리키거나 orbivisord가 만드는 VXLAN 네트워크, 또는 OVN 논리 스위치인 SDN
         *     네트워크(`sdn`, 서브넷 `subnets` — 계획 65). 이름 `user`와 브리지 `br-int`는 예약되어 있다. SDN 네트워크는 폴더의
         *     `network.config`로 만들고, SDN(OVN)이 세워지지 않았으면 409 `sdn_not_ready`, 서브넷 검증은 400 `invalid_subnet`.
         *     VNI가 다른 네트워크와 겹치면 409 `vni_in_use`(동시에 만드는 두 요청의 경쟁은 막지 않는다).
         * @description 공급자(외부) 네트워크(`sdn.provider {physnet, vlan}` — 계획 66 §8)는 노드의 물리 망에 붙으므로 루트 `cluster.admin`이고,
         *     같은 physnet·VLAN의 공급자 네트워크가 있으면 409 `physnet_conflict`다. 공급자 네트워크와 서브넷의 `floating_pools`·
         *     `ipv6_mode`는 모든 노드가 `sdn-routing`을 알아야 한다(409 `cluster_upgrading`, 검증보다 먼저). physnet이 준비된 노드가
         *     없어도 만들고 `orbivisor-warning`으로 알린다.
         */
        post: operations["create_network"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/networks/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_network"];
        put?: never;
        post?: never;
        /**
         * VM이 쓰거나, 라우터의 인터페이스·외부이거나, 유동 IP가 있으면 409 `network_in_use`. 공급자 네트워크는 루트
         *     `cluster.admin`이다.
         */
        delete: operations["delete_network"];
        options?: never;
        head?: never;
        /**
         * SDN 네트워크의 서브넷을 바꾼다(`If-Match`로 리비전 확인): 서브넷 더하기, `dns_servers`·`pools`·`dhcp` 바꾸기, 쓰는
         *     NIC가 없는 서브넷 빼기. 서브넷을 더하면 그 네트워크에서 주소가 하나도 없는 NIC(켜진 VM 포함)가 새 서브넷의 주소를
         *     받는다(리뷰 2 N-4 — 서브넷이 있는 네트워크의 주소 없는 NIC는 OVN 포트가 꺼진다. 켜진 게스트는 다음 DHCP 갱신에서 받는다).
         *     그만큼 빈 주소가 없으면 409 `ip_exhausted`(아무것도 바꾸지 않음). 이미 주소가 있는 NIC에 빠진 체계는 채우지 않는다
         *     (`orbivisorctl network ips NAME --fill`). 풀을 줄여도 이미 준 풀 밖 주소는 남는다. 단순 네트워크면 409
         *     `network_immutable`, 서브넷 검증·`gateway` 바꾸기는 400 `invalid_subnet`, 빼는 서브넷의 주소를 쓰는 VM이 있으면 409
         *     `subnet_in_use`(부른 사용자가 읽을 수 없는 VM은 수로만), 옛 노드가 남았으면 409 `cluster_upgrading`. 네트워크를 쓴 뒤
         *     채우지 못한 VM(그새 바뀜)은 응답 헤더 `orbivisor-warning`으로 알린다.
         */
        patch: operations["patch_network"];
        trace?: never;
    };
    "/networks/{key}/ips": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * SDN 네트워크의 주소: 서브넷마다 크기·사용·남음(모든 VM), NIC마다 VM·MAC·주소와 OVN 논리 포트의 `up`·바인딩
         *     노드(API 노드가 NB·SB 팔로어에서도 읽는다 — 못 읽으면 `live` null과 `live_error`). 네트워크 읽기 권한. NIC 목록은
         *     부른 사용자가 읽을 수 있는 VM의 것뿐이고 나머지는 `hidden` 수로만 알린다(리뷰 1 I-1 — 네트워크를 함께 쓰는 다른
         *     폴더의 VM 이름·주소를 드러내지 않게). `live=false`면 OVN을 읽지 않는다(목록의 사용률). 단순 네트워크면 409
         *     `network_immutable`.
         */
        get: operations["network_ips"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/networks/{key}/router": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * 네트워크를 그 네트워크가 인터페이스인 라우터에서 뗀다(리뷰 2 I-2): 그 네트워크의 `network.config`(경로표)면 라우터를
         *     읽을 수 없어도 된다 — 다른 폴더의 라우터가 권한을 거둔 뒤에도 내 네트워크의 기본 경로를 쥐고 있을 때 주인이 되찾는
         *     길이다. 라우터가 없으면 404, 이 길로 라우팅되는 유동 IP가 있으면 409 `fip_in_use`, 이 네트워크에 VIP가 있는 부하
         *     분산기가 있으면 409 `lb_in_use`(계획 67 §7.1).
         */
        delete: operations["detach_network_router"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_nodes"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_node"];
        /**
         * 노드의 주소, 유지보수 모드, 역할(`roles`), 호스트 자원 설정(`host_resources`)을 바꾼다. 노드 리소스는 orbivisord가
         *     시작할 때 스스로 만든다. 위트니스 여부(`witness`)는 합류할 때 정해지므로 요청의 값을 무시하고 그대로 둔다. 위트니스는
         *     API가 없으므로 `api` 역할을 줄 수 없다. `roles`를 적으려면 모든 노드가 그 필드를 알아야 한다(`node-roles`).
         * @description `sdn`(계획 66 §7: physnet → Linux 브리지, 게이트웨이 우선순위)도 칸이 없으면 저장된 값을 그대로 두고 `{}`면 지운다.
         *     바꾸려면 모든 노드가 `sdn-routing`을 알고(409 `cluster_upgrading`) SDN이 세워졌어야 하며(409 `sdn_not_ready`, 지우기는
         *     빼고) 위트니스·macOS 노드는 400, 검증은 400 `invalid_node_sdn`, 빠지는 physnet을 쓰는 VM이 이 노드에서 돌면 409
         *     `physnet_in_use`(`details.vms`)다.
         *
         *     `host_resources`(계획 64: 예약·전용 CPU, 실행 중 hugepages 예약)는 칸이 없으면 저장된 값을 그대로 두고(이 칸을
         *     모르는 클라이언트의 PUT이 지우지 않게) `{}`면 지운다. 바꾸려면 모든 노드가 `numa`를 알아야 하고(409
         *     `cluster_upgrading`), 노드가 알린 토폴로지에 맞아야 한다(온라인 CPU, 예약·전용이 겹치지 않음, 공유 CPU가 남음, 있는
         *     호스트 NUMA 노드 — 400 `invalid_host_resources`). 도는 VM이 고정된 pCPU가 새 전용 CPU 밖이면 409
         *     `host_cpus_in_use`(`details.vms`), Orbivisor VM이 쓰는 hugepages보다 적게 예약하면 409 `hugepages_in_use`이고
         *     `?force=true`면 쓰고 경고를 `orbivisor-warning`으로 준다.
         */
        put: operations["update_node"];
        post?: never;
        /**
         * 죽은 노드를 클러스터에서 뺀다. 살아 있는 노드는 그 노드에서 `orbivisorctl cluster leave`로 뺀다.
         *     그 노드에 배정된 VM은 그대로 남는다. VM들의 복제 대상(계획 55)에서는 그 노드를 뺀다. Ceph 데몬(mon·mgr·OSD)
         *     노드였으면 Orbivisor의 Ceph 기록에서도 빼고(Ceph는 바꾸지 않는다) 손으로 치울 명령을 경고로 돌려준다(200). OVN 중앙
         *     목록의 노드는 먼저 `sdn central remove`로 빼야 하고(409 `sdn_central_on_node`), 목록 밖의 OVN DB를 가졌던 노드는 지운
         *     뒤 살아 있는 중앙이 OVN Raft에서 내쫓는다(경고에 그 Task).
         */
        delete: operations["delete_node"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes/{key}/cpu": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 노드의 CPU 탐침 결과(QEMU 버전·가속기·호스트 CPU·읽은 시각·오류)와 카탈로그(모델마다 켤 수 있는지·빠진 플래그,
         *     플래그마다 이 가속기가 주는지)
         */
        get: operations["get_node_cpu"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes/{key}/cpu/refresh": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 노드의 QEMU에 CPU 모델·플래그를 다시 묻는 Task(`node-cpu-probe`, 그 노드에서 돈다)를 돌려준다(202). 탐침은 보통
         *     QEMU 바이너리가 바뀔 때 스스로 한다. 끝나지 않은 다시 읽기가 있으면 그것이다. 위트니스는 400, 오프라인이면 409
         *     `node_offline`, 옛 노드가 남았으면 409 `cluster_upgrading`.
         */
        post: operations["refresh_node_cpu"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes/{key}/disks": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 노드의 디스크(`lsblk`의 최상위 디스크)마다 OSD로 쓸 수 있는지: `available`, `wipeable`(지워야 함, 이유들),
         *     `in-use`(쓸 수 없음, 이유). 위트니스면 409 `node_unavailable`, 오프라인이면 409 `node_offline`, 이것을 모르는 옛
         *     노드면 409 `cluster_upgrading`, 읽지 못하면 503 `node_unreachable`.
         */
        get: operations["list_node_disks"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes/{key}/drain": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 노드를 유지보수로 바꾸고(이미면 그대로), 실행 중인 VM을 하나씩 다른 노드로 라이브 마이그레이션할
         *     Task(`node-drain`, 그 노드에서 돈다)를 돌려준다(202). Task가 실패해도 노드는 유지보수로 남는다.
         *     위트니스이거나 오프라인이면 409 `node_unavailable`, 그 노드의 비우기 Task가 끝나지 않았으면 409
         *     `node_busy`, 비우기를 모르는 노드가 남았으면 409 `cluster_upgrading`.
         */
        post: operations["drain_node"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes/{key}/fence": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 노드의 전원 끄기 펜싱 설정. 없으면 404. */
        get: operations["get_node_fence"];
        /**
         * 노드의 전원 끄기 펜싱 방법(1~4개, 차례로 해 본다)을 통째로 둔다. 비밀번호(`password`)를 빼면 같은 종류·주소·
         *     사용자의 저장된 값을 두고(주소나 사용자를 바꾸면 다시 줘야 한다 — 400), 빈 글이면 지운다. 위트니스는 400, 모든
         *     노드가 `fast-ha`를 알아야 한다(409 `cluster_upgrading`).
         */
        put: operations["put_node_fence"];
        post?: never;
        /** 노드의 전원 끄기 펜싱을 지운다(리더는 watchdog만 기다린다). 없으면 404. */
        delete: operations["delete_node_fence"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes/{key}/fence-test": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 노드의 펜싱 방법마다 전원을 읽는다(전원은 건드리지 않는다). 이 API 노드가 그 노드가 아니면 스스로 읽고, 그
         *     노드면 이웃(온라인·`fast-ha`인 다른 노드 가운데 id 순으로 다음)에게 노드 사이 RPC로 부탁한다. 이웃이 없으면
         *     409 `no_fence_checker`, 이웃이 답하지 않으면 502 `fence_check_failed`.
         */
        post: operations["test_node_fence"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes/{key}/metrics": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 노드 하나의 지표. 위트니스면 409 `node_unavailable`, 꺼진 것으로 기록됐거나 닿지 않으면 503
         *     `node_unreachable`, 지표를 모르는 옛 노드면 409 `cluster_upgrading`.
         */
        get: operations["get_node_metrics"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes/{key}/metrics/history": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 노드 하나의 기록(노드 열 9개). 위트니스면 409 `node_unavailable`, 기록을 모르는 옛 노드면 409
         *     `cluster_upgrading`, 꺼진 것으로 기록됐거나 닿지 않으면 503 `node_unreachable`이다.
         */
        get: operations["get_node_history"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes/{key}/topology": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 노드의 NUMA 토폴로지(소켓·NUMA 노드·L3·형제·거리·hugepages 풀·`isolcpus`)와 세 CPU 집합(예약·전용·공유), 호스트
         *     노드마다 사용(전용 CPU·hugepages), 이 노드에서 도는 VM이 잡은 pCPU·페이지, 노드에 직접 물은 지금 값(빈 메모리·빈
         *     hugepages·orbivisord 친화, 노드 사이 RPC `ReadTopology` — 답이 없으면 `live`가 없다), 경고
         */
        get: operations["get_node_topology"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes/{key}/unfreeze": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 얼린 노드(호스트 종료 정책 `freeze`로 HA VM을 끄고 꺼진 노드)의 표시를 지워, 리더가 평소대로 그 노드의
         *     HA VM을 다른 노드로 옮기게 한다. 돌아오지 않는 노드에 쓴다. 표시가 없으면 그대로 돌려준다. 온라인이면
         *     409 `node_online`(돌아온 노드는 등록하며 스스로 지운다).
         */
        post: operations["unfreeze_node"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes/{key}/updates/refresh": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 노드의 업데이트를 다시 확인할 Task(`node-update-check`, 그 노드에서 돈다)를 돌려준다(202). 결과는
         *     Node `status.updates`에 쓴다. 그 노드에 끝나지 않은 확인 Task가 있으면 새로 만들지 않고 그것을
         *     돌려준다. 업데이트를 확인하지 않는 노드면 400 `updates_unsupported`, 오프라인이면 409 `node_offline`,
         *     옛 노드가 남았으면 409 `cluster_upgrading`(옛 노드는 이 Task 종류를 풀지 못한다), 그 노드를
         *     업그레이드하는 중이면 409 `upgrade_in_progress`.
         */
        post: operations["refresh_updates"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/nodes/{key}/upgrade": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 노드의 패키지를 올릴 Task(`node-upgrade`, 그 노드에서 돈다)를 돌려준다(202). apt는 orbivisord 밖의 일시
         *     유닛에서 돌므로 VM은 계속 돌고 orbivisord만 잠깐 다시 시작한다. 업데이트를 확인하지 않는 노드면 400
         *     `updates_unsupported`, 오프라인이면 409 `node_offline`, 클러스터 어디든 끝나지 않은 업그레이드가
         *     있으면 409 `upgrade_in_progress`(한 번에 한 노드 — 나머지 노드가 과반수와 서비스를 지킨다), 옛 노드가
         *     남았으면 409 `cluster_upgrading`.
         */
        post: operations["upgrade_node"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/notification-targets": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_notification_targets"];
        put?: never;
        /** 알림 대상을 만든다(SMTP 또는 webhook `json`·`slack`·`teams`·`ntfy`·`gotify`). admin만 쓴다. 비밀은 응답에 넣지 않는다. */
        post: operations["create_notification_target"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/notification-targets/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_notification_target"];
        /**
         * 대상을 통째로 바꾼다(비밀 규칙은 [`NotificationTargetRequest`]). `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이
         *     바뀌었으면 다시 읽어 바꾼다.
         */
        put: operations["update_notification_target"];
        post?: never;
        /** 대상을 지운다. 규칙이 쓰고 있으면 409 `in_use`(규칙에서 먼저 뺀다). */
        delete: operations["delete_notification_target"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/notification-targets/{key}/test": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 대상으로 시험 알림을 이 노드에서 바로 한 번 보낸다(다시 하지 않음, 10초 제한, 꺼진 대상도 보냄). 결과는 이 노드의
         *     보낸 기록에 `test`로 남는다.
         */
        post: operations["test_notification_target"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/permissions/effective": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 사용자의 대상 유효 권한과 근거 grant. 다른 사용자는 그 대상의 `permission.modify`가 있어야 한다(아니면 403). 대상을
         *     볼 수 없으면 404.
         */
        get: operations["effective_permissions"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/permissions/mine": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 요청한 주체(토큰이면 범위까지)의 권한. 웹이 메뉴·단추를 숨길 때 쓴다. */
        get: operations["my_permissions"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/permissions/who": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 대상에 권한이 닿는 grant와 전역 역할 사용자. 대상의 `permission.modify`가 있어야 한다(403, 볼 수 없으면 404).
         *     전역 역할 사용자 목록은 사용자 목록을 읽는 루트 `cluster.admin`에게만 채운다(리뷰 1).
         */
        get: operations["who_permissions"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/placement": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 최적 배치 상태: 설정, 노드마다의 부하(리더가 리스로 받은 요약), 균형 조정기의 마지막 결과. 로그인한 누구나 읽는다.
         *     리더가 아닌 노드는 리더에 묻고, 묻지 못하면 503 `placement_unavailable`이다.
         */
        get: operations["get_placement"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/placement-rules": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_placement_rules"];
        put?: never;
        /**
         * VM끼리(`affinity`·`anti-affinity`와 `soft-` 형태) 또는 VM과 노드(`node-affinity`·`node-anti-affinity`와
         *     `soft-` 형태) 배치 규칙. VM은 이름(`vms`)이나 레이블(`vm_tag`), 노드는 이름(`nodes`)이나 노드 레이블
         *     (`node_tag`, 예: `zone=rack-a`)로 고른다. 켜진 hard 규칙끼리 모순이면 422 `rule_conflict`. 이미 놓인 VM은
         *     옮기지 않는다. 모든 노드가 `ha-placement`를 알아야 한다.
         */
        post: operations["create_placement_rule"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/placement-rules/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_placement_rule"];
        /** 규칙을 통째로 바꾼다(`enabled`로 켜고 끈다). */
        put: operations["update_placement_rule"];
        post?: never;
        delete: operations["delete_placement_rule"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/placement/recommendations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 균형 조정기의 마지막 회차가 추천한 옮기기(auto면 시작한 옮기기와 그 Task). 회차마다 새로 계산한다.
         *     리더가 아닌 노드는 리더에 묻고, 묻지 못하면 503 `placement_unavailable`이다.
         */
        get: operations["list_recommendations"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/placement/recommendations/{id}/apply": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 추천 `id`를 적용한다: 그 VM을 추천한 노드로 옮기는 마이그레이션 Task(202, 권한은 VM 마이그레이션과 같음).
         *     리더 노드의 API에서만 적용한다(리더가 아니면 409 `recommendation_stale` — 리더 메모리의 HA 검사 결과·균형 조정기의
         *     상태로 다시 보므로). 추천이 없으면(회차마다 새로 계산한다) 404 `recommendation_not_found`, VM이 고정
         *     (`placement.pinned`)이면 409 `vm_pinned`, 클러스터에 끝나지 않은 마이그레이션이 둘 이상이면 409 `placement_busy`다.
         *     추천은 회차 때의 상태로 계산했으므로 지금 상태로 균형 조정기의 조건을 다시 본다: 모드가 `off`이거나, 균형 조정기가 쉬는
         *     이유가 있거나(유지보수 노드·HA 되돌아가기 기다림 등), VM을 옮길 수 없거나(옮기는 중·Task 중·1시간 안에 옮김 등),
         *     대상이 조건에 맞지 않으면(메모리·규칙·HA 검사·살아 있음을 본 시간 등) 409 `recommendation_stale`(문구에 이유).
         *     적용은 한 번에 하나씩 다시 본다. 그 밖의 오류는 마이그레이션과 같다(VM이 이미 다른 노드면 409 `vm_moved`,
         *     `no_capacity`, `placement_rule` 등).
         */
        post: operations["apply_recommendation"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/realms": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_realms"];
        put?: never;
        /** 이름은 리소스 이름 규칙이고 `local`은 쓸 수 없다. 나쁜 값은 400 `invalid_request`. */
        post: operations["create_realm"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/realms/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_realm"];
        /**
         * 영역을 통째로 바꾼다(`bind_password` 규칙은 [`RealmRequest`]). `If-Match`가 없으면 읽은
         *     revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다.
         */
        put: operations["update_realm"];
        post?: never;
        delete: operations["delete_realm"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/realms/{key}/actions/test": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 영역을 시험한다. LDAP 영역은 사용자 이름과 비밀번호로 디렉터리를 확인하고, OIDC 영역은 IdP의
         *     discovery 문서와 서명 키를 받아 본다. 사용자도 세션도 만들지 않는다.
         */
        post: operations["test_realm"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/remotes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_remotes"];
        put?: never;
        /** 원격을 확인하고([`verify`]) 저장한 뒤 곧 상태를 한 번 모은다. 나쁜 값은 400 `invalid_request`. */
        post: operations["create_remote"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/remotes/probe": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 원격의 CA를 인증서를 검증하지 않고 받아 지문을 돌려준다(웹이 등록 전에 보여 주고 확인받는다).
         *     30초 안에 받지 못하면 502 `remote_unreachable`.
         */
        post: operations["probe_remote"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/remotes/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_remote"];
        /**
         * 원격을 통째로 바꾼다(`token` 규칙은 [`RemoteRequest`]). 바뀌면 다시 확인하고 상태를 곧 모은다.
         *     확인하는 사이에 상태만 바뀌었으면(수집기 따위) 다시 확인하지 않고 새 revision에 쓴다(`If-Match`도
         *     그대로 통과). spec이 바뀌었으면 `If-Match`가 있으면 412, 없으면 다시 읽어 바꾼다.
         */
        put: operations["update_remote"];
        post?: never;
        delete: operations["delete_remote"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/remotes/{key}/actions/refresh": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** 원격 상태를 곧 다시 모아 적고 돌려준다. */
        post: operations["refresh_remote"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/routers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 볼 수 있는 라우터(`network.config`가 아닌 읽기)와 게이트웨이. `live=false`면 NB를 읽지 않는다. */
        get: operations["list_routers"];
        put?: never;
        /**
         * 라우터를 만든다(계획 66 §9): 폴더의 `network.config`, 인터페이스 네트워크마다 `network.config`, 외부 네트워크의
         *     `network.assign`. 외부 주소는 API가 외부 네트워크의 체계마다 고른다(유동 IP 풀이 있으면 거기서만, 없으면 풀 — 준
         *     주소는 서브넷 안의 빈 주소이고 루트 `cluster.admin`이 아니면 그 범위 안). 옛 노드가
         *     남았으면 409 `cluster_upgrading`, SDN이 없으면 409 `sdn_not_ready`, 스펙·네트워크 검사는 400 `invalid_router`, 이미 다른
         *     라우터의 인터페이스인 네트워크는 409 `network_in_use`, 주소는 409 `ip_in_use`·`ip_exhausted`, 폴더 할당량
         *     `routers`는 409 `quota_exceeded`. 게이트웨이 후보가 없으면 만들고 `orbivisor-warning`으로 알린다.
         */
        post: operations["create_router"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/routers/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 라우터 하나와 게이트웨이 순위·지금 주인. `live=false`면 NB를 읽지 않는다. */
        get: operations["get_router"];
        put?: never;
        post?: never;
        /** 라우터를 지운다. 인터페이스가 남았으면 409 `router_in_use`(먼저 빼라). */
        delete: operations["delete_router"];
        options?: never;
        head?: never;
        /**
         * 라우터의 설명·외부·인터페이스·경로를 바꾼다(`If-Match`로 리비전 확인, 없으면 읽은 리비전에 쓰고 그새 바뀌었으면 다시
         *     읽는다). 라우터의 `network.config`(경로표), 새로 잇는 인터페이스 네트워크의 `network.config`, 새 외부 네트워크의
         *     `network.assign`. 유동 IP가 이
         *     라우터로 라우팅되는 인터페이스를 빼거나 외부를 바꾸면 409 `fip_in_use`(먼저 떼라), 부하 분산기의 VIP 네트워크
         *     인터페이스를 빼거나 바깥 VIP가 쓰는 외부를 바꾸면 409 `lb_in_use`(계획 67). 검사는 만들기와 같다.
         */
        patch: operations["patch_router"];
        trace?: never;
    };
    "/sdn": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * SDN(OVN)의 상태: 단계, 중앙과 각 중앙이 알린 Raft 역할·northd, 리더 OvnSync의 마지막 동기화, 위트니스가 아닌 노드마다의
         *     chassis 상태, 끝나지 않은 SDN Task. 설정 전이면 `phase`가 없다.
         */
        get: operations["get_sdn"];
        put?: never;
        /**
         * SDN(OVN)을 세운다: 노드 `node`에 OVN 꾸러미를 apt로 설치하고 NB·SB ovsdb Raft 클러스터와 northd를 Orbivisor 단위로
         *     세우고 chassis로 구성하는 `sdn-init` Task(202). `address`는 NB·SB가 들을 주소(없으면 노드 주소의 IP, 노드 인증서의
         *     이름 안의 노드 주소여야 하고 루프백·링크 로컬은 안 된다). 실패했으면 같은 노드로 다시 부르면 이어서 하고(주소를
         *     주지 않으면 지난번 주소로), 첫 세우기 노드가 꺼졌거나 지워졌고 아직 중앙이 없으면 다른 노드로 옮겨 다시 한다. 노드가 없거나 위트니스·macOS·유지보수·underlay 없음·인증서 밖 주소면 400,
         *     오프라인이면 409 `node_offline`, 옛 노드가 남았으면 409 `cluster_upgrading`, 이미 세웠거나 다른 노드에서 세우는 중이면
         *     409 `sdn_exists`, 다른 중앙 Task가 끝나지 않았으면 409 `sdn_busy`.
         */
        post: operations["init_sdn"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/sdn/centrals": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 노드 `node`를 OVN 중앙으로 더하는 `sdn-central-add` Task(202): NB·SB Raft 클러스터에 합류하고 northd를 띄우고
         *     chassis로 구성한다. 중앙은 5개까지(409 `sdn_limit`)이고 홀수(3·5)가 정족수를 가장 잘 지킨다(짝수가 되면 응답 헤더
         *     `orbivisor-warning`). SDN이 준비되지 않았으면 409 `sdn_not_ready`, 이미 중앙이면 409 `sdn_exists`, 다른 중앙 Task가
         *     끝나지 않았으면 409 `sdn_busy`, 노드 오류는 `POST /sdn`과 같다.
         */
        post: operations["add_central"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/sdn/centrals/{node}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * 노드 `node`를 OVN 중앙에서 빼는 Task(202). 노드가 온라인이면 그 노드의 `sdn-central-remove`: NB·SB Raft 클러스터를
         *     떠나고 단위를 멈추고 DB 파일은 이름만 바꾼다(지우지 않음 — chassis 구성은 그대로). 꺼졌거나 클러스터에서 지운
         *     노드면 살아 있는 다른 중앙의 `sdn-central-kick`: 그 노드의 Raft 주소를 NB·SB 클러스터에서 내쫓고 목록에서 뺀다(리뷰 2
         *     N-1 — 그 노드의 단위와 DB는 손대지 못한다). 중앙 목록에 없지만 이 SDN의 DB를 가진 노드(끝나지 않은 중앙 더하기가 남긴
         *     멤버)와 지운 노드 기록(`strays`)도 같이 받는다(마지막 중앙 검사·짝수 경고 없음). 마지막 중앙은 뺄 수 없다(409
         *     `sdn_last_central`). 노드가 중앙이 아니면 400(없으면 404), 내쫓을 살아 있는 중앙이 없으면 409 `node_offline`, SDN이
         *     준비되지 않았으면 409 `sdn_not_ready`, 다른 중앙 Task가 끝나지 않았으면 409 `sdn_busy`. 목록의 중앙을 빼서 짝수가 되면
         *     응답 헤더 `orbivisor-warning`.
         */
        delete: operations["remove_central"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/sdn/chassis": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 노드들(`nodes`, 또는 `all`이면 아직 준비된 chassis가 아닌 모든 온라인 Linux 노드)에 OVN chassis 꾸러미를 깔고 구성하는
         *     `sdn-chassis-install` Task(노드마다 하나, 함께 돈다, 202). 새로 합류한 노드에 저절로 깔지 않는다. `all`이 건너뛴
         *     오프라인 노드는 응답 헤더 `orbivisor-warning`으로 알린다. `nodes`와 `all`을 함께·둘 다 안 주면 400, SDN이 준비되지
         *     않았으면 409 `sdn_not_ready`, 노드 오류는 `POST /sdn`과 같다(유지보수 노드도 된다).
         */
        post: operations["install_chassis"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/sdn/lockdown": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * OVN 접근 잠금(계획 67 §10)을 켜거나 끄는 `sdn-lockdown` Task(202): `SdnSpec.lockdown`을 쓰고, 중앙마다의 SDN 에이전트가
         *     하나씩(다른 중앙의 NB·SB가 정족수를 가졌을 때) SB를 역할 포트(6642 `ovn-controller`)·northd 전용 포트(6645)로 다시
         *     띄운 뒤, 모든 중앙이 잠기면 chassis·중앙이 nftables 주소 허용 목록을 맞출 때까지 기다린다(끄기는 거꾸로). 같은 값을
         *     다시 보내면 남은 일을 마저 기다린다. `dry_run`이면 바꾸지 않고 허용 집합과 그 밖에서 지금 중앙 6641·6642에 붙어 있는
         *     상대(중앙이 알린 `ss` — NAT·다른 출발 주소로 붙은 노드를 미리 찾음)를 돌려준다(200). 켜기는 그런 상대가 있으면 409
         *     `lockdown_peers_outside`(`force`면 켠다 — 그 상대는 끊긴다). 옛 노드가 남았으면 409 `cluster_upgrading`, SDN이 준비되지
         *     않았으면 409 `sdn_not_ready`, 다른 중앙 Task가 끝나지 않았으면 409 `sdn_busy`.
         */
        post: operations["set_lockdown"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/security-groups": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_security_groups"];
        put?: never;
        /** 규칙은 적힌 순서대로 보고 처음 맞는 것을 따른다. */
        post: operations["create_security_group"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/security-groups/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_security_group"];
        put?: never;
        post?: never;
        /** VM의 NIC가 쓰고 있으면 409 `security_group_in_use` */
        delete: operations["delete_security_group"];
        options?: never;
        head?: never;
        /**
         * 설명이나 규칙을 바꾼다. 쓰는 VM의 노드가 곧바로 적용한다. `If-Match`가 없으면 읽은 revision에
         *     쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다.
         */
        patch: operations["patch_security_group"];
        trace?: never;
    };
    "/settings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 클러스터 설정. 로그인한 누구나 읽는다. 관리자(설정 쓰기 권한)가 아니면 이미지 카탈로그의 URL을 가린다. */
        get: operations["get_settings"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /**
         * 설정의 주어진 값을 바꾼다. 아무것도 없으면 400. 설정이 아직 없으면 만든다(`If-Match`는 0).
         *     `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다. 같은 값이면 쓰지
         *     않고 그대로 200이다. enterprise 채널은 모든 노드가 이 빌드 이상이고 구독이 active일 때만 고른다
         *     (이미 enterprise여도 다시 보내면 검사한다). 텔레메트리도 모든 노드가 이 빌드 이상일 때만 바꾸고,
         *     켤 때 id가 없으면 만든다. Prometheus 끝점은 모든 노드가 `metrics-history`를 알 때만 바꾸고(409
         *     `cluster_upgrading`), 끄면 설정에서 지운다.
         */
        patch: operations["patch_settings"];
        trace?: never;
    };
    "/snapshots": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 모든 VM의 스냅샷. watch와 함께 쓰는 목록이다. */
        get: operations["list_snapshots"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/storage-pools": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_pools"];
        put?: never;
        post: operations["create_pool"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/storage-pools/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_pool"];
        put?: never;
        post?: never;
        /** 볼륨이나 백업이 남아 있으면 409 `pool_in_use` */
        delete: operations["delete_pool"];
        options?: never;
        head?: never;
        /**
         * 디렉터리 풀의 `shared`(다른 백엔드는 400, 루트 `cluster.admin`)와 백업 풀 표시 `backups`(dir·NFS 풀만 켤 수
         *     있다)를 바꾼다. 둘 다 없으면 400이다. 로컬 → 공유는 언제나 된다. 공유 → 로컬은 풀의 볼륨을 쓰는 VM이 그
         *     볼륨을 만든 노드가 아닌 노드에 있거나 끝나지 않은 마이그레이션 Task가 있으면 409 `pool_in_use`다.
         *     백업이 남은 풀의 `backups`를 끄거나 `shared`를 바꾸면 409 `pool_in_use`다. 같은 값이면 쓰지 않고
         *     그대로 200이다.
         */
        patch: operations["patch_pool"];
        trace?: never;
    };
    "/storage-pools/{key}/metrics/history": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 풀 하나의 기록(`pool_used`·`pool_total`, MiB). 그 풀이 `ready`인 노드 하나(이 노드 먼저, 없으면 이름 순으로 켜져 있고
         *     기록을 아는 노드)에 묻는다. 노드 로컬 풀이면 그 노드의 값이다. 준비된 노드가 없으면 409 `pool_unavailable`, 모두
         *     옛 노드면 409 `cluster_upgrading`, 모두 꺼져 있거나 닿지 않으면 503 `node_unreachable`이다.
         */
        get: operations["get_pool_history"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/subscription": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 구독 상태. 로그인한 누구나 읽는다. */
        get: operations["get_subscription"];
        /**
         * 구독 키를 넣는다(있던 키는 바꾼다). 형식·서명이 맞지 않거나 만료된 키는 400
         *     `invalid_subscription_key`.
         */
        put: operations["put_subscription"];
        post?: never;
        /** 구독 키를 지운다. 없으면 그대로 200이다. */
        delete: operations["delete_subscription"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/tasks": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_tasks"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/tasks/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_task"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/telemetry/preview": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 지금 보낼 보고 그대로(꺼져 있어도). admin만 본다. */
        get: operations["preview_telemetry"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/telemetry/reset-id": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** 텔레메트리 id를 새로 만든다. 설정이 아직 없으면 만든다. */
        post: operations["reset_telemetry_id"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/tokens": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 자기 토큰. 관리자는 모든 토큰을 본다. */
        get: operations["list_tokens"];
        put?: never;
        /** 요청한 사용자의 토큰을 만든다. 토큰의 권한은 주인의 역할을 따른다. */
        post: operations["create_token"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/tokens/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete: operations["delete_token"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/topology/usage": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 노드마다 전용 CPU(전체·사용)와 크기마다 hugepages(전체·사용, MiB) — 노드 목록의 열과 VM 폼의 "들어갈 노드". 사용은
         *     스케줄러의 셈(도는 VM의 기록 + 아직 켜지 않은 VM·오는 마이그레이션을 맞춰 본 것)이다.
         */
        get: operations["topology_usage"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/users": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_users"];
        put?: never;
        post: operations["create_user"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/users/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * 관리자는 누구의 비밀번호와 역할이든 바꾼다. 다른 사용자는 자기 비밀번호만 바꾼다.
         *     자기 비밀번호를 바꿀 때는 현재 비밀번호가 필요하고(훔친 세션으로 계정을 빼앗지 못하게),
         *     바꾸면 모든 노드에서 그 사용자의 세션이 끝난다(API 토큰은 남는다). 마지막 관리자는 역할을 내려놓을 수 없다.
         *     현재 비밀번호 확인은 로그인과 같은 시도 제한을 받는다.
         */
        put: operations["update_user"];
        post?: never;
        /** 사용자와 그 사용자의 토큰을 지운다. 자기 자신은 지울 수 없다(마지막 관리자를 잃지 않게). */
        delete: operations["delete_user"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/users/{key}/two-factor": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * 관리자 초기화: 사용자의 2단계 인증 요소를 모두 지우고 세션을 끊는다(API 토큰은 남는다). 자기 자신은 409
         *     `cannot_reset_self`(자기 요소는 `/auth/2fa`에서 뺀다).
         */
        delete: operations["reset_two_factor"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/version": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 이 노드의 빌드 버전. 인증 없이 읽는다(로그인 화면과 orbivisorctl이 서버 버전을 본다). */
        get: operations["get_version"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vm-imports": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_vm_imports"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vm-imports/esxi": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * ESXi·vCenter 데이터스토어의 VM으로 `ready` 가져오기를 만든다(201). 이 노드가 `.vmx`와 디스크
         *     디스크립터를 읽어 원본 VM과 제안을 만들고, 자격 증명은 이 노드의 메모리에만 둔다(실행 Task도 이
         *     노드에서 돈다). 연결 오류는 browse와 같다(지문이 없으면 409 `untrusted_certificate`). 스냅샷이 있는
         *     VM, 다른 데이터스토어의 디스크, 통째로 받을 수 없는 디스크는 400. 잠금·일시 중단 파일이 있으면 경고만
         *     한다.
         */
        post: operations["create_esxi"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vm-imports/esxi/browse": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * ESXi·vCenter에 연결해 데이터스토어마다 맨 위 디렉터리의 VM(`.vmx`)을 요약한다. 지문이 없으면 409
         *     `untrusted_certificate`(`details.fingerprint`), 다르면 409 `fingerprint_mismatch`, 인증 실패 400
         *     `esxi_auth`, 연결할 수 없으면 502 `esxi_unreachable`. VM 디렉터리는 500개까지 본다.
         */
        post: operations["browse_esxi"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vm-imports/ova": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 요청 본문(OVA)을 이 노드에 받아 해석하고, 원본 VM과 제안을 담은 `ready` 가져오기를 돌려준다(201).
         *     OVA를 쓸 수 없으면 400 `invalid_ova`. 실패하거나 연결이 끊기면 받은 파일을 지운다.
         */
        post: operations["upload_ova"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vm-imports/ova/check": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** OVA를 받기 전에 받을 수 있는지 확인한다(204). 거절 이유는 올리기와 같다. 본문은 받지 않는다. */
        post: operations["check_ova"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vm-imports/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_vm_import"];
        put?: never;
        post?: never;
        /**
         * 원본 디렉터리와 리소스를 지울 Task를 돌려준다(202). Task는 원본 노드에서 돈다. 그것을 다루는 Task가
         *     끝나지 않았으면(`force`여도) 409 `import_busy`. 원본 노드가 오프라인이거나 클러스터에 없으면 409
         *     `node_offline`이고, `force`면 리소스만 바로 지우고 200이다.
         */
        delete: operations["delete_import"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vm-imports/{key}/run": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `ready` 가져오기를 `run`의 값으로 실행할 Task를 돌려준다(202). Task는 원본이 있는 노드에서 돌고, 대상은
         *     미리 정한 새 VM id다. 실행 Task가 끝나지 않았으면 409 `import_busy`, 이미 가져왔으면 `import_done`(끝난 실행
         *     Task가 VM을 만들었으면 가져오기를 `done`으로 끝내고), 원본을 쓸 수
         *     없으면 `import_failed`, 원본 노드가 오프라인이면 `node_offline`. 원본 NIC의 네트워크마다 있는 Orbivisor
         *     네트워크로 매핑해야 하고(400), VM 이름과 볼륨 이름 `{name}-disk{i}`가 비어 있어야 한다(409
         *     `name_taken`). 풀은 원본 노드에서 쓸 수 있어야 하고(409 `pool_not_on_node`·`pool_unavailable`), 결과
         *     VM 스펙은 VM 만들기의 검증을 통과해야 한다(400). 원본 MAC을 쓰면 다른 VM과 겹치지 않아야 한다(409
         *     `mac_in_use`). 원본이 이 노드에 있으면 가장 큰 디스크를 추출하거나 받을 여유도 본다(507).
         */
        post: operations["run_import"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_vms"];
        put?: never;
        /**
         * 빠진 MAC을 채우고, 켜진 채로 만들면 노드를 고른다. cloud-init 평문 비밀번호는 SHA-512 crypt로 바꿔 저장하고
         *     응답에서는 `********`로 가린다. `cloud_init.user_data`가 만든 user-data의 칸(`user`·`password`·`ssh_keys`)을
         *     쓰이지 않게 하면 응답 헤더 `orbivisor-warning`으로 알린다. `machine`을 빼면 지정 노드나 클러스터 아키텍처의
         *     기본값이다(노드 목록을 읽지 못하는 폴더 사용자의 웹 만들기 화면이 쓴다, 아키텍처가 섞였으면 400).
         */
        post: operations["create_vm"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_vm"];
        /**
         * spec 전체를 바꾼다. 실행 중인 VM의 `node`는 바꿀 수 없고 `ha`를 켜거나 `guest_watch`를 켜고 끌 수 없다(409
         *     `vm_running`). NIC의 `security_groups`는 곧바로 적용된다.
         * @description 모든 노드가 `hotplug`를 알면 실행 중인 VM의 하드웨어(CPU·메모리·상한·펌웨어·머신·디스크·NIC) 변경은 핫플러그
         *     Task(`vm-hotplug`)를 만들고 그 id를 응답 헤더 `orbivisor-task`로 준다. 상한 안의 vCPU(`cpu.cores`)·메모리, 끝에
         *     붙이는 virtio-blk 디스크·NIC, 실행 중 붙인 것을 끝에서 떼기는 바로 하고, 나머지는 `status.hardware.pending`(다음
         *     재시작 때 적용)에 남는다. 부팅 때 단 디스크·NIC 떼기, 가운데 떼기, 순서·버스·MAC·모델·네트워크 바꾸기, SATA 디스크
         *     붙이기는 409 `vm_running`, 핫플러그 없이 켠 VM이나 빈 루트 포트가 없을 때의 디스크·NIC 붙이기·떼기는 409
         *     `hotplug_unavailable`, 켜거나 끄는 중이거나 앞의 핫플러그 Task가 끝나지 않았으면 409 `vm_busy`, 메모리를 바로
         *     늘릴 미할당 메모리가 노드에 없으면 409 `no_capacity`다. 같은 PUT으로 끄는(`power: stopped`) VM의 하드웨어 변경은
         *     Task 없이 다음 시작부터 적용된다. 기능이 모자라거나 켜져 있어야 하지만 QEMU가 없는 VM(실패, 다시 켜기를
         *     기다림)은 실행 중에 디스크와 NIC 개수·순서·MAC을 바꿀 수 없고(보안 그룹을 건 NIC가 있으면 `security_groups`만)
         *     다른 변경은 다음 시작부터 적용된다. `If-Match`가 없으면 검증한 revision에 쓰고, 그
         *     사이 바뀌었으면 다시 읽어 검증한다.
         *
         *     CD-ROM 매체(`cdroms[i].iso`)는 실행 중에도 핫플러그 Task로 바꾼다(드라이브 더하기·빼기는 409 `vm_running`, CD
         *     부팅 순서와 `cloud_init`은 다음 재시작 때 적용). `cloud_init.password`가 `********`이면 저장된 비밀번호를 그대로
         *     둔다(받은 스펙을 고쳐 다시 보내는 길). 경고는 `POST /vms`처럼 `orbivisor-warning`이다.
         *
         *     `template`은 바꾸지 않는다(`POST`·`DELETE /vms/{key}/template`). 템플릿은 켤 수 없고(`power: running`은 409
         *     `vm_is_template`) 디스크 목록(볼륨·버스·개수)을 바꿀 수 없으며(409 `vm_is_template`) `ha`·`replication`·
         *     `snapshot_schedule`을 가질 수 없다(400). 연결 복제 디스크가 있는 VM의 복제 대상(`replication`)은 400이다.
         *
         *     `tpm`·`secure_boot`가 켜진 스펙은 모든 노드가 `vtpm`을, `guest_agent`가 있는 스펙은 `guest-agent`를 알아야 한다(409
         *     `cluster_upgrading`). 실행 중에 `tpm`이나 게스트 에이전트 채널(`guest_agent`)을 켜고 끄면 다음 재시작 때 적용하는
         *     보류(`status.hardware.pending`)가 되고, `guest_agent`의 `freeze_fs`·`shutdown`은 바로 적용된다. `secure_boot`는
         *     실행 중이면 409 `vm_running`, UEFI 변수가 있으면(`status.firmware_node`) 409 `firmware_reset_required`다(변수 초기화와
         *     함께 `POST /vms/{key}/firmware/reset`의 `secure_boot`로 바꾼다). 끝나지 않은 펌웨어 초기화 Task가 있으면 켜기는 409
         *     `vm_busy`다.
         *
         *     CPU 정책(`cpu.policy`·`thread_policy`·`emulator`)·vNUMA(`numa`)·`hugepages`(계획 64)는 모든 노드가 `numa`를 알아야
         *     하고(409 `cluster_upgrading`) 맞지 않으면 400 `invalid_numa`다. 전용 CPU·hugepages를 켜거나 그 칸을 바꾸려면 그 VM의
         *     `vm.dedicated`가 든다(403). 실행 중에 바꾸면 보류 `numa`(다음 전원 껐다 켜기 때 적용)이고, 전용 VM의 vCPU·메모리는
         *     핫플러그 상한 안에서 바로 바꾼다(켤 때 상한까지 pCPU를 잡아 두었다).
         */
        put: operations["update_vm"];
        post?: never;
        /**
         * 꺼진 VM만 지울 수 있다. 스냅샷이 남아 있으면 409 `vm_has_snapshots`, 끝나지 않은 스냅샷·복제
         *     Task가 있으면 409 `vm_busy`(복제는 원본 VM과 그 스냅샷을 읽는다). 템플릿은 먼저 VM으로 되돌린다(409
         *     `vm_is_template`).
         */
        delete: operations["delete_vm"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/actions/{action}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `start`와 `stop`은 `spec.power`를 바꾸고, 그 결과를 지켜볼 Task를 돌려준다(202).
         *     `stop?mode=force`와 `reboot`은 대상 노드의 Task 실행기가 수행한다. `hotplug`는 실행 중인 VM의 하드웨어를
         *     지금 스펙에 맞추는 Task(`vm-hotplug`, 실패한 변경 다시 해 보기)다: 모든 노드가 `hotplug`를 알아야 하고(409
         *     `cluster_upgrading`) 핫플러그 기록이 있는 실행 중 VM이어야 한다(409 `vm_not_running`). 끝나지 않은 스냅샷·되돌리기·
         *     복제·백업 Task가 있으면 409 `vm_busy`이고, 메모리를 바로 늘리면 VM이 도는 노드에 그만큼 미할당 메모리가 있어야 한다
         *     (409 `no_capacity`). 템플릿의 `start`·`reboot`은 409 `vm_is_template`이고, 템플릿 되돌리기 Task가 끝나지 않은 VM의
         *     `start`·`reboot`은 409 `vm_busy`다. 끝나지 않은 펌웨어 초기화 Task(`vm-firmware-reset`)가 있어도 `start`·`reboot`은 409
         *     `vm_busy`이고, 꺼진 TPM VM을 켤 노드가 지금 TPM 상태를 얻을 수 없으면(`firmware_node`가 오프라인이고 온라인 사본이
         *     없음) `start`는 409 `tpm_state_unavailable`이다. 꺼진 VM이 고정된 노드가 VM의 CPU 모델·플래그를 켤 수 없거나 전용
         *     CPU·hugepages·NUMA 모양이 들지 않으면 켤 수 있는 다른 노드를 고르고, 없으면 `start`는 409 `no_capacity`(까닭)다. 고른
         *     노드의 전용 CPU·hugepages를 그사이 다른 켜기가 잡았으면(클러스터 장부의 거절) 그 노드를 빼고 한 번 다시 고른다.
         *     `reboot`은 그 노드가 새 스펙의 CPU·NUMA를 켤 수 없으면 409 `no_capacity`다.
         */
        post: operations["vm_action"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/backups": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `pending` 백업을 만들고 그것을 채울 Task를 돌려준다(202). 풀은 백업 풀이어야 한다(400). 시작·정지
         *     중이면 409 `vm_busy`, 실행 중인데 스냅샷을 담지 못하는 디스크가 있으면 409 `vm_running`(스냅샷
         *     방식이 섞였으면 스냅샷처럼 409 `snapshot_unsupported`), Task를 돌릴 노드에서 백업 풀을 쓸 수 없으면
         *     409 `pool_not_on_node`·`pool_unavailable`, 돌릴 노드가 없으면 409 `no_node`, 이름이 겹치면 409
         *     `name_taken`. 그사이 풀이 지워졌거나 백업 표시가 꺼졌으면 만든 백업을 지우고 409 `pool_unavailable`.
         *     `incremental`은 증분 백업을 모르는 노드가 있으면 409 `cluster_upgrading`.
         */
        post: operations["create_backup"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/cdrom": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * CD-ROM 드라이브 하나의 매체를 넣거나 뺀다: 스펙의 `cdroms[drive].iso`만 바꿔 `PUT /vms/{key}`와 같은 길로 쓴다
         *     (같은 검사, 실행 중이면 `vm-hotplug` Task와 헤더 `orbivisor-task`). 드라이브가 없으면 400, 모든 노드가
         *     `install-media`를 알아야 한다(409 `cluster_upgrading`). 지우는 중인(끝나지 않은 지우기 Task가 있는) ISO는 409
         *     `volume_in_use`다. 드라이브 더하기·빼기는 `PUT`이다(꺼진 VM). 템플릿에 매체를 넣으면 409 `vm_is_template`이다.
         */
        post: operations["cdrom_media"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/clone": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * VM을 `name`으로 복제할 Task를 돌려준다(202). 원본은 `ready` 스냅샷이나 꺼진 VM의 지금 상태다
         *     (실행 중인데 스냅샷이 없으면 409 `vm_running`). 원본 스펙의 볼륨이 모두 있어야 하고, `pool`은 Task
         *     노드에서 쓸 수 있어야 한다. Task는 원본 VM을 대상으로 그 VM의 디스크 노드에서 돈다.
         * @description 템플릿에서 배포(계획 58 §6): 템플릿이면 디스크를 기반과 같은 풀에 연결 복제로 만든다(`linked`). `linked: true`는
         *     템플릿의 지금 상태에서만이고(400) 디스크마다 연결할 수 있어야 한다(400 `disk0: …` — 다른 풀, 연결 복제를 못 하는
         *     백엔드). `node`·`vcpus`·`memory_mib`·`nics`·`disks`·`cloud_init`·`start`는 원본 스펙을 바꿔 VM 만들기와 같은 검사를
         *     한다(400 — 디스크 줄이기, 없는 네트워크, 디스크가 닿지 않는 노드, 맞지 않는 아키텍처). 새 칸은 모든 노드가
         *     `templates`를 알아야 한다(409 `cluster_upgrading`). 응답 Task의 cloud-init 비밀번호는 가린다.
         */
        post: operations["clone_vm"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/console": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["console"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/cpu": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * VM의 CPU와 노드마다 켤 수 있는지(마이그레이션 대화상자·`oplctl vm cpu`). 도는 VM은 켤 때 기록한 CPU로 옮길 수
         *     있는지를, 꺼진 VM은 스펙을 그 노드에서 해석해 켤 수 있는지를 본다. `host`·`max` VM을 옮기면 대상에 없는 플래그가
         *     경고다.
         */
        get: operations["get_vm_cpu"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/firmware/reset": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 꺼진 VM의 UEFI 변수·TPM 상태를 초기화할 Task(`vm-firmware-reset`)를 만든다(202, 설계 §5). Task는 온라인인
         *     `firmware_node`에서 돌고(없거나 오프라인이면 VM의 디스크가 있는 노드), 그 노드의 파일을 지우고 `firmware_epoch`를
         *     올린다(다른 노드의 사본은 회차가 달라 켤 때 지워진다). `uefi_vars`·`tpm`이 모두 거짓이면 400, VM이 꺼져 있지 않으면
         *     409 `vm_running`, 켜거나 끄는 중이거나 마이그레이션·스냅샷·복제·백업 Task가 끝나지 않았으면 409 `vm_busy`, 모든
         *     노드가 `vtpm`을 알아야 한다(409 `cluster_upgrading`). TPM 상태가 있는 VM의 변수만 초기화는 `firmware_node`가
         *     오프라인이면 409 `node_unavailable`이다. `secure_boot`를 주면 Task가 스펙의 `secure_boot`도 바꾼다(UEFI 펌웨어가
         *     아니면 400). Task가 끝날 때까지 켜기는 409 `vm_busy`다.
         */
        post: operations["reset_firmware"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/guest": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * VM의 게스트 정보(OS·호스트 이름·NIC와 IP·파일 시스템). VM이 도는 노드의 메모리 캐시(60초마다 모음)를 읽고,
         *     `refresh`면 에이전트에 지금 묻는다(VM마다 5초에 한 번). `state`는 `answered`(정보가 있음), `no_agent`(채널이 없거나
         *     에이전트가 아직 답하지 않음), `not_running`, `unreachable`(VM이 도는 노드가 답하지 않음), `unsupported`(그 노드가
         *     게스트 에이전트를 모르는 옛 노드)다. 정보는 `answered`일 때만 있다.
         */
        get: operations["get_guest"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/guest/password": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 게스트 사용자 `user`의 비밀번호를 바꾼다(`guest-set-user-password`, 설계 §9). 게스트 안 관리자 권한과 같은 무게라
         *     VM 쓰기 권한이 필요하다. 비밀번호는 저장하지 않고 로그·감사(`vm guest password win user X`)·오류에 넣지 않는다.
         *     사용자 이름(1~64바이트, 제어 문자 없음)·비밀번호(1~256바이트)가 맞지 않으면 400, 모든 노드가 `guest-agent`를 알아야
         *     하고(409 `cluster_upgrading`), VM이 돌지 않으면 409 `vm_not_running`, 채널이 없거나 에이전트가 답하지 않으면 409
         *     `guest_agent_unavailable`, VM이 도는 노드가 답하지 않으면 503 `node_unreachable`이다.
         */
        post: operations["set_guest_password"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/live-restore/cancel": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 라이브 복원을 취소한다(202, 계획 68 §11.4): VM을 끄고 VM·대상 볼륨·오버레이를 지운다 — 게스트가 쓴 것을 버리므로
         *     `discard: true`가 없으면 400. 라이브 복원 중이 아니면 409 `vm_not_restoring`. 취소를 하는 Task(라이브 복원 Task)를
         *     돌려준다.
         */
        post: operations["cancel_live_restore"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/metrics": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * VM 하나의 지표. 모든 노드(위트니스 제외)에 물어 시각 순으로 합치므로 마이그레이션 앞뒤 기록이
         *     이어진다. 같은 `t`가 두 노드에서 오면 둘 다 둔다.
         */
        get: operations["get_vm_metrics"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/metrics/history": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * VM 하나의 기록(VM 열 8개). VM의 주인 노드(`status.node`, 없으면 `spec.node`, 둘 다 없으면 이 노드)에만 묻는다.
         *     마이그레이션 직후 앞 기록이 넘어오기 전이면 앞 칸이 `null`이고 `covered_from`이 알린다. 주인 노드가 기록을 모르면
         *     409 `cluster_upgrading`, 닿지 않으면 503 `node_unreachable`이다.
         */
        get: operations["get_vm_history"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/migrate": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 실행 중인 VM을 다른 노드로 라이브 마이그레이션할 Task를 돌려준다(202). Task는 지금 노드의 실행기가
         *     한다. 노드 로컬 풀의 디스크는 대상 노드의 같은 이름의 풀(`pool`을 주면 그 풀)에 복사한다. 그런 디스크가
         *     있으면 모든 노드가 디스크 복사를 알아야 하고(409 `cluster_upgrading`) VM에 스냅샷이 없어야 한다(409
         *     `migration_unsupported`). `pool`을 줬는데 그런 디스크가 없으면 400, 없는 풀이면 404다. HVF(macOS)
         *     노드는 원본도 대상도 될 수 없다(409 `migration_unsupported`). 핫플러그 배치로 도는 VM은 대상이 같은 장치로 켤 수
         *     있어야 하고(`hotplug` 기능, vCPU 핫플러그, virtio-mem — 아니면 409 `migration_unsupported`), 핫플러그 Task가 끝나지
         *     않았거나 스펙의 디스크·NIC가 도는 장치와 다르면 409 `vm_busy`다. 대상 노드가 위트니스이거나 오프라인이거나
         *     유지보수 중이거나 호스트 종료 중이면 409 `node_unavailable`, HA VM인데 대상이 HA 가능 노드가 아니면 409 `ha_unsupported`,
         *     스케줄러가 고를 수 없는 노드(풀, 대상 풀, 네트워크, 남은 메모리, VM이 켤 때 기록한 CPU 모델·플래그를 켤 수 없음)면
         *     409 `no_capacity`다. 대상이 VM이 켤 때 기록한 NUMA 모양(전용 CPU·hugepages)을 받을 수 없으면 409
         *     `migration_unsupported`(까닭)다. 라이브 마이그레이션을 막는 CPU 플래그(`+invtsc`)로 도는 VM은 409 `migration_unsupported`다. `host`·`max` VM을 원본에 있던 CPU 플래그가 없는 노드로 옮기면 막지 않고 감사 detail에
         *     `cpu-warning: target node "n3" lacks …`를 남긴다. 대상 노드가 hard
         *     배치 규칙을 어기면(자리 잡은 hard affinity 상대가 있는 VM 하나만 옮기기 포함) 409 `placement_rule`
         *     (`details.rules`)이고, 관리자는 `force`로 넘을 수 있다(감사 detail `force rules=<이름들>`). CD-ROM에 노드 로컬 풀의
         *     ISO가 들어 있으면 409 `cdrom_local`이다(매체를 빼거나 공유 풀의 ISO를 쓴다). 실행 중 cloud-init을 바꿨으면(호스트
         *     이름 없는 VM의 이름 바꾸기 포함) 다시 켤 때까지 409 `restart_required`다. 템플릿은 409 `vm_is_template`이다.
         */
        post: operations["migrate_vm"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/numa": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * VM의 CPU 배치와 NUMA(마이그레이션 대화상자·`oplctl vm numa`): 스펙, 기록한 모양과 배치, 보류, 노드마다 맞는지와 그
         *     노드에서 쓸 배치(호스트 노드·pCPU·페이지)
         */
        get: operations["get_vm_numa"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/replicate": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 실행 중인 VM의 노드 로컬 풀 디스크를 복제 대상 노드 하나로 지금 복제할 Task(`vm-replicate`)를 VM이 도는 노드에
         *     만든다(202). 결과는 `GET /vms/{key}`의 `status.replication`에 남는다. 대상이 둘 이상이면 `node`가 있어야 하고(400),
         *     `node`가 VM의 복제 대상이 아니면 400, 모든 노드가 복제를 알아야 하고(409 `cluster_upgrading`), VM이 실행 중이어야
         *     하며(409 `vm_not_running`, 옮기는 중이면 409 `vm_busy`), 같은 대상의 복제 Task가 끝나지 않았으면 409 `vm_busy`다.
         *     템플릿은 409 `vm_is_template`이다.
         */
        post: operations["replicate_vm"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/snapshots": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_vm_snapshots"];
        put?: never;
        /**
         * `pending` 스냅샷을 만들고(부모는 VM의 현재 위치, `vm_spec`은 지금 스펙) 그것을 채울 Task를
         *     돌려준다(202). 시작·정지 중이면 409 `vm_busy`, 스냅샷을 담지 못하는 디스크가 있거나 디스크의
         *     스냅샷 방식이 섞였거나 블록 디스크(LVM-thin, ZFS, RBD)의 VM에 메모리 스냅샷을 요청하면
         *     409 `snapshot_unsupported`. 실행 중에 바꾼 하드웨어나 다음 재시작 때 적용할 변경이 있는 VM과, `hotplug`를 모르는
         *     노드가 있는 동안 핫플러그 배치로 도는 VM도 메모리를 담지 않는다(`memory`를 주지 않으면 디스크만, `true`면 409
         *     `snapshot_unsupported`). 실행 중이 아니거나 디스크가 없는
         *     VM의 메모리 스냅샷은 400.
         */
        post: operations["create_snapshot"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/snapshots/{name}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_snapshot"];
        put?: never;
        post?: never;
        /** 스냅샷을 지울 Task를 돌려준다(202). 자식은 지운 스냅샷의 부모로 옮겨 붙는다. */
        delete: operations["delete_snapshot"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/snapshots/{name}/revert": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 디스크·UEFI 변수·VM 스펙을 스냅샷 것으로 되돌릴 Task를 돌려준다(202). 실행 중이면 강제로 끈다.
         *     스냅샷 스펙의 볼륨이 모두 있고 다 만들어졌으며 다른 VM이 쓰지 않아야 한다. ZFS 디스크는 VM의
         *     가장 최근 스냅샷으로만 되돌린다(아니면 409 `snapshot_unsupported`).
         */
        post: operations["revert_snapshot"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/vms/{key}/template": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * VM을 템플릿으로 바꾼다(계획 58 §3, Raft 쓰기 하나). 꺼져 있어야 하고(409 `vm_running`), 마이그레이션·스냅샷·
         *     복제·백업 Task가 없어야 하며(409 `vm_busy`), 스냅샷이 없어야 한다(409 `vm_has_snapshots` — 템플릿의 되돌리기가
         *     연결 복제의 기반을 바꾸면 안 된다). `ha`·`replication`·`snapshot_schedule`이 있거나 디스크가 연결 복제면 400이다.
         *     모든 노드가 `templates`를 알아야 한다(409 `cluster_upgrading`). 이미 템플릿이면 그대로 200이다. 저장소에는
         *     아무것도 하지 않는다(기반 스냅샷은 첫 연결 복제가 만든다).
         */
        post: operations["convert_template"];
        /**
         * 템플릿을 보통 VM으로 되돌릴 Task(`vm-untemplate`)를 VM의 노드에 만든다(202, 계획 58 §3). 템플릿이 아니면 400,
         *     연결 복제가 남았으면 409 `template_in_use`(쓰는 VM이나 붙지 않은 복제 볼륨의 이름), 앞 되돌리기 Task가 끝나지 않았으면
         *     409 `vm_busy`, 모든 노드가 `templates`를 알아야 한다(409 `cluster_upgrading`). 되돌리기 Task가 끝날 때까지 켜기·고치기·
         *     지우기도 409 `vm_busy`다. Task는 줄에서 기다린 뒤 다시 확인하고 디스크마다 기반 스냅샷을 푼 다음
         *     `template: false`를 쓴다.
         */
        delete: operations["untemplate_vm"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/volumes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_volumes"];
        put?: never;
        /** 볼륨 리소스를 만들고, 파일을 만들 Task를 돌려준다(202). Task가 실패하면 리소스도 지운다. */
        post: operations["create_volume"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/volumes/download": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * URL에서 이미지를 받아 볼륨을 채울 Task를 돌려준다(202). 풀을 쓸 수 있는 노드(`pool_node`)가 받는다.
         *     크기는 받아 가져온 뒤 실제 이미지 크기가 된다. 모든 노드가 `install-media`를 알아야 한다(새 Task).
         *     확인은 `sha256`·`sha512`·`checksum_url` 가운데 하나이고(둘 이상이면 400), `sha512`·`checksum_url`은 모든 노드가
         *     `templates`를 알아야 한다(409 `cluster_upgrading`).
         */
        post: operations["download_volume"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/volumes/import": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 요청 본문(이미지 파일)을 이 노드에 받아 두고, 그것을 변환해 볼륨을 채울 Task를 돌려준다(202).
         *     풀은 이 노드에서 쓸 수 있어야 한다. 받기 전에 `check_import`로 확인한다.
         */
        post: operations["import_volume"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/volumes/import/check": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** 업로드 전에 받을 수 있는지 확인한다(204). 거절 이유는 업로드와 같다. 본문은 받지 않는다. */
        post: operations["check_import_volume"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/volumes/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_volume"];
        put?: never;
        post?: never;
        /**
         * VM이 쓰지 않는 볼륨을 지울 Task를 돌려준다(202). 아직 만들고 있는 볼륨은 지울 수 없다.
         *     스냅샷의 VM 스펙이 가리키면 409 `volume_in_snapshot`, 템플릿의 디스크면 409 `volume_is_template`, 연결 복제의
         *     기반이면 409 `volume_has_clones`(복제를 먼저 지우거나 평탄화한다).
         */
        delete: operations["delete_volume"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/volumes/{key}/flatten": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 연결 복제 볼륨을 기반과 떼어 독립 볼륨으로 만들 Task(`volume-flatten`)를 돌려준다(202, 계획 58 §7). 볼륨을
         *     쓸 수 있는 노드에서 돈다(쓰는 VM이 있으면 그 VM의 디스크 노드). 모든 노드가 `templates`를 알아야 하고(409
         *     `cluster_upgrading`), 연결 복제가 아니면 400, 아직 만드는 중이면 409 `volume_not_ready`, 쓰는 VM이 꺼져 있지 않으면
         *     409 `vm_running`(실행 중 평탄화는 없다), 그 VM의 스냅샷·복제·백업·마이그레이션 Task가 끝나지 않았으면 409
         *     `vm_busy`다. 파일 풀(dir·NFS)의 디스크는 VM에 스냅샷이 있으면 409 `vm_has_snapshots`(qcow2 내부 스냅샷이 기반을
         *     잃는다). 기반이 쓰는 공간을 노드가 알린 풀 여유가 담지 못하면 409 `no_capacity`다.
         */
        post: operations["flatten_volume"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/volumes/{key}/resize": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * 볼륨 크기를 늘릴 Task를 돌려준다(202). 줄이지는 않는다(400). VM이 쓰면 그 VM의 스냅샷
         *     Task와 같은 노드(실행 중이면 그 VM의 노드, 꺼졌으면 다음에 돌 노드)의 실행기에서 차례로 돌도록
         *     거기서 한다. 쓰는 VM이 없거나 정할 노드가 없으면 볼륨의 노드에서 한다. 쓰는 VM이 켜지거나 꺼지는
         *     중이면(원하는 상태와 실제 상태가 다르다) 409 `vm_busy`다. 꺼진 VM으로 보고 저장소로 늘리는 동안
         *     시작한 QEMU가 늘리는 중인 볼륨을 열 수 있다. 템플릿의 디스크는 409 `volume_is_template`이다(배포 때 디스크 크기를
         *     준다).
         */
        post: operations["resize_volume"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/watch": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * 이벤트는 `opl_cluster::watch::WatchEvent`의 JSON이다. 이어 받을 수 없게 되면
         *     `{"type":"error","code":"expired"|"lagged"|"closed"}`를 보내고 닫는다. 클라이언트는 목록을
         *     다시 읽고 새로 구독한다.
         * @description 계획 62: 폴더를 따르는 종류(VM·볼륨·풀·네트워크·스냅샷·백업·가져오기·Task·폴더)와 grant는 이벤트마다 볼 수 있는
         *     것만 보낸다. 보이던 객체가 옮겨져 안 보이게 되면 그 객체의 `deleted` 이벤트(이름과 id만)를 보낸다. 구독자의 권한이
         *     바뀌거나(grant·역할·그룹·폴더 트리·사용자·토큰), 파생 종류(볼륨·스냅샷·백업·Task)를 구독하는데 VM·풀이 옮겨져
         *     보임이 바뀌면 `{"type":"error","code":"permissions"}`를 보내고 닫는다(목록을 다시 읽는다). 그 밖의 종류는 구독할 때
         *     루트에서 읽기 권한을 본다(403).
         */
        get: operations["watch"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        /** @description 역할(내장 또는 사용자 정의) */
        AccessRoleView: {
            /** @description 내장 역할(`admin`·`operator`·`viewer`)이면 참이고 바꿀 수 없다. */
            builtin: boolean;
            description: string;
            /**
             * Format: uuid
             * @description 사용자 정의 역할의 id. 내장 역할은 없다.
             */
            id?: string;
            name: string;
            /** @description 권한 이름 목록(`vm.power` 등) */
            privileges: string[];
            /**
             * Format: int64
             * @description 사용자 정의 역할의 revision(내장 역할은 0)
             */
            revision: number;
        };
        /** @enum {string} */
        Action: "accept" | "drop";
        /** @description 활성 경보 하나 */
        ActiveAlert: {
            /**
             * Format: int64
             * @description `firing`인데 조건이 거짓이 된 때(이만큼 [`RESOLVE_AFTER_MS`] 지나면 해제)
             */
            clear_since?: number;
            /**
             * Format: int64
             * @description 마지막으로 알린 때
             */
            last_sent?: number;
            /** Format: uuid */
            rule: string;
            /**
             * Format: int64
             * @description 조건이 참이 된 때
             */
            since: number;
            state: components["schemas"]["AlertState"];
            subject: components["schemas"]["AlertSubject"];
            /**
             * Format: double
             * @description 마지막으로 본 값(지표·풀 조건)
             */
            value?: number;
        };
        /**
         * @description 옮겨 갈 주소의 출처(앞이 먼저)
         * @enum {string}
         */
        AddressSource: "given" | "guest-agent" | "cloud-init" | "ipam";
        /**
         * @description 게스트 에이전트가 있는 VM을 끄는 방법
         * @enum {string}
         */
        AgentShutdown: "agent" | "acpi";
        /** @description 경보 조건 */
        AlertCondition: {
            /** @enum {string} */
            kind: "node_metric";
            metric: components["schemas"]["NodeAlertMetric"];
            op: components["schemas"]["AlertOp"];
            /** Format: double */
            value: number;
        } | {
            /** @enum {string} */
            kind: "vm_metric";
            metric: components["schemas"]["VmAlertMetric"];
            op: components["schemas"]["AlertOp"];
            /** Format: double */
            value: number;
        } | {
            /** @enum {string} */
            kind: "pool_usage";
            /** Format: double */
            percent: number;
        } | {
            /** @enum {string} */
            kind: "node_offline";
        } | {
            /** @enum {string} */
            kind: "vm_failed";
        } | {
            /** @enum {string} */
            kind: "task_failed";
            ops?: string[];
        } | {
            /** @enum {string} */
            kind: "ha_recovery";
        } | {
            /** @enum {string} */
            kind: "backup_job_failed";
        } | {
            /** @enum {string} */
            kind: "sdn_quorum_lost";
        } | {
            /** @enum {string} */
            kind: "sdn_central_down";
        } | {
            /** @enum {string} */
            kind: "sdn_chassis_down";
        } | {
            /** @enum {string} */
            kind: "sdn_gateway_unavailable";
        } | {
            /** @enum {string} */
            kind: "sdn_gateway_failover";
        } | {
            /** @enum {string} */
            kind: "sdn_sync_failed";
        } | {
            /** @enum {string} */
            kind: "sdn_physnet_not_ready";
        } | {
            /** @enum {string} */
            kind: "sdn_lb_member_down";
        } | {
            /** @enum {string} */
            kind: "backup_verify_failed";
        } | {
            /** @enum {string} */
            kind: "backup_store_unavailable";
        };
        /**
         * @description 비교
         * @enum {string}
         */
        AlertOp: "gt" | "lt";
        /** @description 경보 규칙 하나 */
        AlertRuleSpec: {
            condition: components["schemas"]["AlertCondition"];
            /**
             * @description 끄면 평가하지 않고, 활성 경보는 알림 없이 해제한다.
             * @default true
             */
            enabled: boolean;
            /**
             * Format: int32
             * @description 조건이 이만큼(초) 참이어야 알린다(0~86400). 이벤트 조건은 0이다.
             * @default 0
             */
            for_secs: number;
            /** @description 이 노드(이름)들만. 비면 모든 노드 */
            nodes?: string[];
            /**
             * Format: int32
             * @description 경보가 이어지는 동안 이만큼(초)마다 다시 알린다. 0이면 다시 보내지 않는다.
             * @default 14400
             */
            repeat_secs: number;
            /**
             * @description 해제도 알린다(이벤트 조건은 해제가 없다).
             * @default true
             */
            send_resolved: boolean;
            severity: components["schemas"]["Severity"];
            /** @description 알릴 `NotificationTarget` 이름(1~16개) */
            targets: string[];
            /** @description 이 VM(이름)들만(VM 조건만). 비면 모든 VM */
            vms?: string[];
        };
        /** @description 리더가 평가하는 규칙(`NodeOffline`·`BackupJobFailed`)의 활성 경보만 둔다. 노드 경보는 각 노드 파일에 있다. */
        AlertRuleStatus: {
            active?: components["schemas"]["ActiveAlert"][];
            /**
             * @description `BackupJobFailed`: 백업 작업(id)마다 이미 알린 마지막 실패 실행의 끝난 시각(ms). 리더가 바뀌어도 같은 실행을 다시
             *     알리지 않는다.
             */
            notified?: {
                [key: string]: number;
            };
        };
        /**
         * @description 경보의 단계
         * @enum {string}
         */
        AlertState: "pending" | "firing";
        /** @description 경보가 가리키는 것 */
        AlertSubject: {
            /** Format: uuid */
            id: string;
            kind: components["schemas"]["SubjectKind"];
            /** @description 알릴 때의 이름 */
            name: string;
            /** @description 그 일이 일어난 노드 이름(노드·풀 경보는 평가한 노드) */
            node?: string;
        };
        /** @description `GET /alerts`의 경보 하나(규칙 이름·심각도를 붙인 [`ActiveAlert`]) */
        AlertView: {
            /**
             * Format: int64
             * @description 해제를 기다리기 시작한 때(조건이 거짓이 됨)
             */
            clear_since?: number;
            /** @description 조건 종류(`node_metric`, `node_offline` …) */
            condition: string;
            /** Format: int64 */
            last_sent?: number;
            /** Format: uuid */
            rule: string;
            rule_name: string;
            severity: components["schemas"]["Severity"];
            /** Format: int64 */
            since: number;
            state: components["schemas"]["AlertState"];
            subject: components["schemas"]["AlertSubject"];
            /**
             * Format: double
             * @description 문턱(지표·풀 조건)
             */
            threshold?: number;
            /** Format: double */
            value?: number;
        };
        /** @description `GET /alerts`의 응답 */
        AlertsResponse: {
            /** @description `firing` 먼저, 심각도 높은 것 먼저, 오래된 것 먼저 */
            alerts: components["schemas"]["AlertView"][];
            /** @description 활성 경보가 상한을 넘은 노드 이름 */
            overflowing?: string[];
            /** @description 답하지 않은 노드 이름(그 노드의 노드 경보는 보이지 않는다) */
            unreachable: string[];
            /** @description 경보를 모르는 옛 노드 이름 */
            unsupported: string[];
        };
        /**
         * @description 할당 행의 종류(계획 66 §8)
         * @enum {string}
         */
        AllocationKind: "nic" | "router" | "floating-ip" | "lb-vip" | "lb-monitor";
        /** @description 앞단(로드 밸런서)이 부를 API 노드 하나 */
        ApiEndpoint: {
            /** @description API 주소(`host:port`) */
            address: string;
            /** @description Node 리소스 이름 */
            name: string;
            /** @description 장애 영역(노드 레이블 `zone`). 없으면 null이다. */
            zone?: string | null;
        };
        ApiEndpoints: {
            /** @description 이름 순 */
            items: components["schemas"]["ApiEndpoint"][];
        };
        /**
         * @description CPU 아키텍처. 노드가 등록할 때 기록하고, 머신 타입마다 하나로 정해진다.
         * @enum {string}
         */
        Arch: "x86_64" | "aarch64";
        AssertionCredential: {
            /** @description 자격 증명 id(base64url) */
            id: string;
            response: components["schemas"]["AssertionResponse"];
        };
        /** @description `AuthenticatorAssertionResponse`(base64url) */
        AssertionResponse: {
            authenticatorData: string;
            clientDataJSON: string;
            signature: string;
        };
        /** @description `AuthenticatorAttestationResponse`(base64url). 브라우저 `toJSON()`의 다른 칸은 무시한다. */
        AttestationResponse: {
            attestationObject: string;
            clientDataJSON: string;
        };
        /**
         * @description 요청을 인증한 방식
         * @enum {string}
         */
        AuditAuth: "session" | "token" | "none";
        /** @description 감사 로그 한 줄 */
        AuditEntry: {
            /** @description `"<METHOD> <라우트 템플릿>"`(예: `POST /vms/{key}/actions/start`) */
            action: string;
            auth: components["schemas"]["AuditAuth"];
            /** @description 핸들러가 남긴 짧은 문자열(비밀은 넣지 않는다) */
            detail?: string | null;
            /** Format: int64 */
            duration_ms: number;
            /**
             * Format: uuid
             * @description UUID v7
             */
            id: string;
            /** @description 클라이언트 IP */
            ip: string;
            method: string;
            /** @description 적은 노드의 이름 */
            node: string;
            /** @description 쿼리를 뺀 경로(`/api/v1` 아래) */
            path: string;
            role?: components["schemas"]["Role"] | null;
            /**
             * Format: int32
             * @description 응답 상태
             */
            status: number;
            target?: components["schemas"]["AuditTarget"] | null;
            /**
             * Format: int64
             * @description 유닉스 밀리초
             */
            time: number;
            /** @description 사용자 이름. 인증 전이나 실패면 없다. */
            user?: string | null;
            /** Format: uuid */
            user_id?: string | null;
        };
        /** @description `GET /audit`의 응답: 모든 노드의 항목을 합친 것 */
        AuditResponse: {
            /** @description 감사 로그를 끈 노드의 이름(정렬) */
            disabled: string[];
            /** @description 시간 내림차순 */
            entries: components["schemas"]["AuditEntry"][];
            /** @description 조건에 맞는 항목이 `limit`보다 많아 잘랐다. */
            truncated: boolean;
            /** @description 읽지 못한 노드의 이름(정렬) */
            unreachable: string[];
        };
        /** @description 요청의 대상(라우트 템플릿의 첫 경로 조각과 첫 매개변수 값) */
        AuditTarget: {
            /** @description 예: `web-1` */
            key: string;
            /** @description 예: `vms` */
            kind: string;
        };
        AuthenticatorSelection: {
            residentKey: string;
            userVerification: string;
        };
        /** @description 백업에 담은 디스크 하나. VM 스펙의 디스크 순서를 따른다. */
        BackupDisk: {
            /** @description 백업 디렉터리 안의 파일 이름(`disk{i}.qcow2`) */
            file: string;
            /** @description 디스크 이름(`disk{i}`) */
            name: string;
            /** @description 받은 사본이면 받은 파일의 SHA-256(16진수 소문자). 이 클러스터에서 만든 백업이면 뺀다. */
            sha256?: string;
            /**
             * Format: int64
             * @description 백업 파일 크기
             */
            stored_bytes: number;
            /**
             * Format: int64
             * @description 디스크 크기
             */
            virtual_bytes: number;
        };
        /**
         * Format: binary
         * @description 업로드 본문의 문서용 스키마: 바이너리 파일
         */
        BackupFile: string;
        /** @description 리소스만 지운 백업 지우기(`force`)의 응답 */
        BackupForgotten: {
            message: string;
        };
        /** @description 백업 작업에서 바꿀 값. 주어진 필드만 바꾼다. */
        BackupJobPatch: {
            copy_to?: components["schemas"]["CopyTarget"] | null;
            enabled?: boolean;
            /**
             * Format: int32
             * @description 전체 백업 사이에 둘 증분 백업 수. 0이면 늘 전체다.
             */
            full_every?: number;
            /** @description 보존 규칙 전체를 이것으로 바꾼다(적지 않은 단위는 0). */
            keep?: components["schemas"]["Keep"];
            pool?: string;
            replicate?: components["schemas"]["ReplicateTarget"] | null;
            schedule?: components["schemas"]["JobSchedule"];
            /** @description 백업 저장소(계획 68). 주면 그 뒤의 백업은 저장소로 간다(`pool`을 비운다). `pool`을 주면 저장소를 비운다. */
            store?: string;
            vms?: components["schemas"]["VmSelector"];
        };
        BackupJobSpec: {
            /** @description 작업의 백업이 `ready`가 되면 다른 저장소로 복사본을 보낸다(계획 68 §12.5 — 계층). 없으면 쓰지 않는다. */
            copy_to?: components["schemas"]["CopyTarget"];
            /**
             * @description 끄면 예약 실행을 하지 않는다.
             * @default true
             */
            enabled: boolean;
            /**
             * Format: int32
             * @description 0보다 크면 증분으로 백업하되(할 수 있으면) 체인의 증분이 이만큼 쌓이면 다음은 전체로 한다.
             *     0이면 늘 전체다(뺀다).
             *     저장소 작업(`store`)에서는 "n번 연속 비트맵으로 읽었으면 다음은 디스크 전체를 읽기"다(계획 68 §8.3 — 저장 공간은
             *     늘지 않는다).
             */
            full_every?: number;
            /** @description 보존 규칙. 모두 0이면 지우지 않는다. */
            keep?: components["schemas"]["Keep"];
            /**
             * @description 백업 풀 이름. 백업 저장소(`store`, 계획 68)로 백업하는 작업이면 비우고 쓰지 않는다(옛 JSON에는 늘 있으므로
             *     바이트 그대로다). `pool`과 `store` 중 정확히 하나다.
             */
            pool?: string;
            /** @description 만든 백업을 보낼 원격 클러스터의 백업 풀. 없으면 보내지 않는다. */
            replicate?: components["schemas"]["ReplicateTarget"];
            schedule: components["schemas"]["JobSchedule"];
            /** @description 백업 저장소 이름(계획 68). 없으면 쓰지 않는다. */
            store?: string;
            /** @description 백업할 VM */
            vms: components["schemas"]["VmSelector"];
        };
        BackupJobStatus: {
            /**
             * @description 마지막(또는 진행 중인) 실행에서 백업하지 못한 VM과 이유
             * @default []
             */
            errors: components["schemas"]["JobError"][];
            /** @default null */
            last_run: components["schemas"]["JobRunResult"] | null;
            /**
             * Format: int64
             * @description 다음 예약 실행 시각(유닉스 초)
             * @default null
             */
            next_run: number | null;
            /** @default null */
            run: components["schemas"]["JobRun"] | null;
        };
        /** @description 받은 사본의 원본: 보낸 클러스터와 그 클러스터의 백업 */
        BackupOrigin: {
            /**
             * Format: uuid
             * @description 원본 백업 id
             */
            backup: string;
            /** @description 보낸 클러스터 CA의 지문(`sha256:…`). 같은 원본의 사본을 알아보는 데 쓴다. */
            ca_fingerprint: string;
            /** @description 보낸 클러스터 이름 */
            cluster: string;
            /** @description 원본을 만든 백업 작업 이름. 손으로 만든 백업이면 없다. */
            job?: string;
        };
        /** @enum {string} */
        BackupPhase: "pending" | "ready" | "failed" | "receiving";
        /** @description 원격 클러스터의 백업 풀에 보낸 사본 하나. (remote, pool)마다 하나다. */
        BackupReplica: {
            /**
             * Format: int64
             * @description 상태가 바뀐 시각(유닉스 초)
             */
            at: number;
            /**
             * Format: uuid
             * @description 원격에 만든 사본의 id. 만들기 전이면 없다.
             */
            backup?: string;
            /** @description 실패한 이유 */
            error?: string;
            /** @description 원격의 백업 풀 이름 */
            pool: string;
            /** @description 원격 이름 */
            remote: string;
            state: components["schemas"]["ReplicaState"];
        };
        /** @description 저장소 백업의 원본(계획 68): 그 백업 id와 만든 방법 */
        BackupSource: {
            /**
             * Format: uuid
             * @description 원본 백업 id
             */
            backup: string;
            kind: components["schemas"]["BackupSourceKind"];
        };
        /** @enum {string} */
        BackupSourceKind: "convert" | "copy";
        BackupSpec: {
            /**
             * @description 할 수 있으면 증분으로 백업한다(바로 앞 백업을 부모로 바뀐 블록만). 할 수 없으면 전체로 하고 그
             *     이유를 `status.full_reason`에 적는다. false면 뺀다.
             */
            incremental?: boolean;
            /**
             * Format: uuid
             * @description 이 백업을 만든 백업 작업 id. 손으로 만든 백업이면 없다.
             */
            job?: string | null;
            /**
             * Format: int32
             * @description 증분 체인에 둘 증분 백업 수의 한도(백업 작업의 `full_every`). 부모의 체인에 이미 이만큼 증분이
             *     있으면 전체로 한다. 없으면 한도가 없다(손으로 만든 백업).
             */
            max_chain?: number;
            /** @description 다른 클러스터에서 받은 사본이면 그 원본. 이 클러스터에서 만든 백업이면 없다. */
            origin?: components["schemas"]["BackupOrigin"];
            /**
             * @description 백업 풀 이름. 저장소 백업(`store`, 계획 68)이면 비우고 쓰지 않는다(옛 JSON에는 늘 있으므로 바이트 그대로다).
             *     `pool`과 `store` 중 정확히 하나다([`BackupSpec::target`]).
             */
            pool?: string;
            /**
             * @description 옛 백업을 옮기거나(`convert`) 다른 저장소에서 복사해(`copy`) 만든 저장소 백업의 원본(계획 68 §12.2·§12.5). 없으면
             *     쓰지 않는다.
             */
            source?: components["schemas"]["BackupSource"];
            /** @description 백업 저장소 이름(계획 68). 없으면 쓰지 않는다. */
            store?: string;
            /**
             * Format: uuid
             * @description VM id. VM을 지워도 백업은 남는다.
             */
            vm: string;
            /** @description 만든 때의 VM 이름(표시용) */
            vm_name: string;
            /** @description 만든 때의 VM 스펙 */
            vm_spec: components["schemas"]["VmSpec"];
        };
        BackupStatus: {
            /**
             * Format: uuid
             * @description 저장소 백업(계획 68)에서 바뀌지 않은 청크를 가져온 앞 백업. 계획 40의 `parent`와 달리 복원은 이것을 따라가지
             *     않는다(인덱스가 늘 완전하다). 없으면 쓰지 않는다.
             */
            base?: string;
            /**
             * Format: int64
             * @description 만든 시각(유닉스 초)
             * @default null
             */
            created_at: number | null;
            /** @default [] */
            disks: components["schemas"]["BackupDisk"][];
            /**
             * @description 실패한 이유
             * @default null
             */
            error: string | null;
            /** @description 증분을 요청했지만 전체로 한 이유. 없으면 뺀다. */
            full_reason?: string;
            /**
             * Format: uuid
             * @description 파일을 쓴 노드. 공유가 아닌 풀의 백업은 이 노드에만 있으므로 지우기·복원도 이 노드에서 한다.
             *     공유 풀이면 없다.
             */
            node?: string;
            /**
             * Format: uuid
             * @description 증분 백업의 부모(같은 풀의 바로 앞 백업). 전체 백업이면 뺀다.
             */
            parent?: string;
            /** @default pending */
            phase: components["schemas"]["BackupPhase"];
            /**
             * Format: int64
             * @description 받는 중인 사본이 마지막으로 파일을 받은 시각(유닉스 초). 받은 적이 없으면 뺀다.
             */
            received_at?: number;
            /** @description 원격 클러스터에 보낸 사본. 없으면 뺀다. */
            replicas?: components["schemas"]["BackupReplica"][];
            /** @description 저장소 백업이 읽고 쓴 양(계획 68). 없으면 쓰지 않는다. */
            store_stats?: components["schemas"]["StoreBackupStats"];
            /** @description TPM 상태 파일(`tpm2-00.permall`)을 담았는지(계획 59). 거짓이면 뺀다. */
            tpm_state?: boolean;
            /**
             * @description UEFI 변수 파일(`vars.fd`)을 담았는지
             * @default false
             */
            uefi_vars: boolean;
            /** @description 마지막 검증 결과(계획 68). 검증한 적이 없으면 쓰지 않는다. */
            verify?: components["schemas"]["BackupVerify"];
        };
        /** @description 비밀(S3 비밀 키·주 키)을 뺀 저장소 스펙 */
        BackupStoreSpecView: {
            backend: components["schemas"]["StoreBackendView"];
            encryption?: components["schemas"]["StoreEncryptionView"];
            gc: components["schemas"]["JobSchedule"];
            /** Format: int32 */
            max_streams: number;
            read_only?: boolean;
            verify?: components["schemas"]["VerifyPolicy"];
        };
        /** @description 저장소의 실제 상태. 초기화·GC·검증 Task와 리더의 스케줄러가 쓴다. */
        BackupStoreStatus: {
            /**
             * Format: int64
             * @description 검증이 찾아 `bad/`로 옮긴 청크 수
             */
            bad_chunks?: number;
            /** @description 매니페스트를 쓰는 중인 백업(쓰기 잠금 — `sweeping`이 거짓일 때만 더한다, §10.3) */
            committing?: string[];
            error?: string;
            /** @description 진행 중인 GC(§10.2) */
            gc?: components["schemas"]["GcRun"];
            last_gc?: components["schemas"]["GcResult"];
            last_verify?: components["schemas"]["VerifyResult"];
            /**
             * Format: int64
             * @description 다음 예약 GC 시각(유닉스 초)
             */
            next_gc?: number;
            /**
             * Format: int64
             * @description 다음 예약 검증 시각(유닉스 초)
             */
            next_verify?: number;
            /**
             * Format: uuid
             * @description 공유가 아닌 백업 풀 안 저장소를 만든 노드(그 노드에만 있다 — §7.6). 공유 풀·S3면 없다.
             */
            node?: string;
            /** @description 저장소 `owner.json`의 주인 클러스터(§12.6) */
            owner?: components["schemas"]["StoreOwner"];
            /** @default initializing */
            phase: components["schemas"]["StorePhase"];
            /**
             * @description 검증이 깨진 청크를 `bad/`로 옮기는 중(리뷰 1 I5 — 쓸기 잠금처럼 `committing`이 비었을 때만 참이 되고 그동안 커밋
             *     구간에 새로 들지 않는다)
             */
            quarantining?: boolean;
            /**
             * Format: uuid
             * @description 저장소 `store.json`의 id. 이 클러스터가 만든 저장소는 리소스 id, 붙인 저장소는 남의 것이다.
             */
            store_id?: string;
            /**
             * Format: int64
             * @description 쓸기를 시작할 때마다 1씩 오른다. 백업이 시작과 커밋 때 견준다.
             */
            sweep_epoch?: number;
            /** @description GC가 청크를 지우는 중(쓰기 잠금 — `committing`이 비었을 때만 참이 된다) */
            sweeping?: boolean;
            /** @description 마지막 GC가 센 사용량 */
            usage?: components["schemas"]["StoreUsage"];
        };
        /** @description 백업 하나의 마지막 검증 결과(계획 68 §10.5) */
        BackupVerify: {
            /**
             * Format: int64
             * @description 검증한 시각(유닉스 초)
             */
            at: number;
            /**
             * Format: int64
             * @description 이 백업이 가리키는 깨진 청크 수
             */
            bad_chunks?: number;
            error?: string;
            state: components["schemas"]["VerifyState"];
        };
        /** @description 아키텍처 하나의 클러스터 기준(`ClusterStatus.cpu_baselines`, 리더가 쓴다). `cluster` 모델 VM이 켤 때 쓴다. */
        Baseline: {
            /** @description 모델 위에 켜고 끈 플래그(auto 플래그·고정 플래그·숨김 목록) */
            flags?: {
                [key: string]: boolean;
            };
            /** @description 기준을 낮춘 노드(auto: 빼면 더 좋은 모델이 됨, pinned: 이 기준을 켤 수 없음) */
            limited_by?: string[];
            mode: components["schemas"]["BaselineSource"];
            /** @description [`CpuModel`] 이름(버전 모델, `x86-64-v*`, `qemu64-v1`) */
            model: string;
            /** @description 셈에 든 노드 */
            nodes?: string[];
            previous?: components["schemas"]["PreviousBaseline"];
            /**
             * Format: int64
             * @description 이 모델·플래그가 된 시각(유닉스 초, 리더가 채운다)
             */
            since?: number;
            warnings?: string[];
        };
        /**
         * @description 클러스터 기준 자동 셈
         * @enum {string}
         */
        BaselineMode: "off" | "auto";
        /**
         * @description 기준이 어떻게 정해졌나
         * @enum {string}
         */
        BaselineSource: "auto" | "pinned";
        /** @description 본딩 포트 하나 */
        BondPort: {
            /**
             * Format: int32
             * @description 802.3ad의 aggregator id
             */
            aggregator?: number;
            name: string;
            /** Format: int32 */
            speed_mbps?: number;
            state: components["schemas"]["PortState"];
        };
        /**
         * @description 본딩 전체: 모든 포트가 `up`이면 `up`, 일부만이면 `degraded`(통과지만 경고), 하나도 없으면 `down`
         * @enum {string}
         */
        BondState: "up" | "degraded" | "down" | "unknown";
        /** @description 본딩과 그 포트의 상태 */
        BondStatus: {
            /** @description 본딩 모드(`active-backup`, `802.3ad` …) */
            mode: string;
            ports: components["schemas"]["BondPort"][];
            state: components["schemas"]["BondState"];
        };
        BridgeInfo: {
            name: string;
            /** @description VLAN-aware 브리지(`vlan_filtering 1`)인지. VLAN 태그를 쓰는 네트워크에 필요하다. */
            vlan_filtering: boolean;
        };
        /** @description 라이브 복원 취소 본문 */
        CancelLiveRestoreRequest: {
            /** @description 게스트가 라이브 복원 뒤 쓴 것을 버린다는 확인(참이어야 한다) */
            discard?: boolean;
        };
        /** @description `POST /image-catalog/{id}/download` */
        CatalogDownloadRequest: {
            /** @description 받을 항목의 아키텍처. 없으면 풀에서 받는 노드의 아키텍처다. */
            arch?: components["schemas"]["Arch"];
            /** @description 볼륨 이름. 없으면 id의 `.`을 `-`로 바꾸고 aarch64면 `-arm64`를 붙인다(`ubuntu-24-04`). */
            name?: string;
            /** @description 받은 디스크를 둘 풀 */
            pool: string;
            /** @description 받은 뒤 만들 템플릿 VM. 없으면 볼륨만 받는다. */
            template?: components["schemas"]["CatalogTemplate"];
        };
        /** @description 목록의 카탈로그 항목: 이미지와 출처, 이미 받은 볼륨 */
        CatalogEntry: components["schemas"]["CatalogImage"] & {
            source: components["schemas"]["CatalogSource"];
            /** @description 이 이미지 URL에서 받아 준비된 볼륨 이름(끝난 `volume-download` Task의 결과, 이름 순) */
            volumes: string[];
        };
        /** @description 카탈로그 이미지 하나 */
        CatalogImage: {
            arch: components["schemas"]["Arch"];
            checksum: components["schemas"]["Checksum"];
            /** @description cloud-init 기본 사용자 이름(예: `ubuntu`) */
            default_user?: string;
            firmware: components["schemas"]["Firmware"];
            /** @description 항목 id(소문자·숫자·`.`·`-`, 57자까지, 소문자·숫자로 시작하고 끝남). 아키텍처마다 같은 id를 쓴다. */
            id: string;
            /**
             * Format: int64
             * @description 이미지의 디스크 크기(가상 크기, 바이트). 모르면 0이다.
             */
            min_size_bytes?: number;
            /** @description 배포판(예: `ubuntu`) */
            os: string;
            /** @description `qcow2` 또는 `raw` */
            source_format: components["schemas"]["SourceFormat"];
            title: string;
            /** @description 이미지 URL(`http`·`https`, 사용자 정보 없음) */
            url: string;
            version: string;
        };
        /**
         * @description 카탈로그 항목의 출처
         * @enum {string}
         */
        CatalogSource: "builtin" | "custom";
        /**
         * @description 카탈로그 이미지를 받은 뒤 만들 템플릿 VM. 머신 타입은 항목의 아키텍처(`q35`·`virt`), 펌웨어는 항목의 것이고, 디스크는
         *     `virtio-blk` 첫 부팅 디스크, NIC 하나, cloud-init(호스트 이름·정적 IP 없음)이다.
         */
        CatalogTemplate: {
            /** @description cloud-init 기본 설정(사용자·비밀번호·SSH 키 등). 평문 비밀번호는 SHA-512 crypt로 바꿔 Task에 담는다. */
            cloud_init?: components["schemas"]["CloudInit"];
            /** @description 에뮬레이터 스레드 정책(계획 64) */
            cpu_emulator?: components["schemas"]["EmulatorPolicy"];
            /** @description CPU 플래그(`["-avx512vbmi"]`, 계획 63) */
            cpu_flags?: components["schemas"]["CpuFlag"][];
            /** @description CPU 모델(계획 63, 없으면 `host`). 별칭은 버전 모델로 고정된다. */
            cpu_model?: components["schemas"]["CpuModel"];
            /** @description CPU 정책(계획 64, 없으면 `shared`). `dedicated`는 권한 `vm.dedicated`가 든다. */
            cpu_policy?: components["schemas"]["CpuPolicy"];
            /** @description 전용 CPU 스레드 정책(계획 64) */
            cpu_thread_policy?: components["schemas"]["ThreadPolicy"];
            /** @description hugepages(계획 64). 권한 `vm.dedicated`가 든다. */
            hugepages?: components["schemas"]["PageSize"];
            /**
             * Format: int32
             * @default 2048
             */
            memory_mib: number;
            /** @description 템플릿 VM 이름 */
            name: string;
            /** @description NIC 하나의 네트워크 이름 */
            network: string;
            /** @description vNUMA(계획 64) */
            numa?: components["schemas"]["NumaSpec"];
            /**
             * Format: int64
             * @description 받은 디스크를 이 크기로 늘린다(바이트, 512의 배수, 이미지 크기 이상). 없으면 이미지 크기 그대로다.
             */
            size_bytes?: number;
            /**
             * Format: int32
             * @default 2
             */
            vcpus: number;
        };
        CdromMediaRequest: {
            /**
             * Format: int32
             * @description CD-ROM 드라이브 번호(`cdroms`의 순번)
             */
            drive?: number;
            /** @description 넣을 ISO 볼륨 이름. 없으면 매체를 뺀다. */
            iso?: string | null;
        };
        /** @description CD-ROM 드라이브. 매체는 늘 읽기 전용으로 열고, 여러 VM이 같은 ISO를 함께 넣을 수 있다. */
        CdromSpec: {
            /**
             * Format: int32
             * @description 부팅 순서. 없으면 디스크 뒤에 둔다(`effective_boot`).
             */
            boot_order?: number;
            /** @description 넣은 ISO 볼륨(`content: iso`)의 이름. 없으면 빈 드라이브다. */
            iso?: string;
        };
        /** @description 중앙 노드의 ovsdb Raft 역할과 northd */
        CentralHealth: {
            nb: components["schemas"]["RaftRole"];
            /** @description 이 노드의 northd가 SB 잠금을 가져 일하고 있다(나머지는 대기). */
            northd_active?: boolean;
            sb: components["schemas"]["RaftRole"];
        };
        CephHealth: {
            /** @default [] */
            checks: components["schemas"]["CephHealthCheck"][];
            /**
             * @description `HEALTH_OK`, `HEALTH_WARN`, `HEALTH_ERR`
             * @default
             */
            status: string;
        };
        /** @description health 검사 하나(예: `TOO_FEW_OSDS`) */
        CephHealthCheck: {
            /** @default  */
            code: string;
            /**
             * Format: int64
             * @default 0
             */
            count: number;
            /** @default false */
            muted: boolean;
            /**
             * @description `HEALTH_WARN`, `HEALTH_ERR`
             * @default
             */
            severity: string;
            /** @default  */
            summary: string;
        };
        /** @description `POST /ceph`: 노드 `node`에 첫 mon·mgr을 세운다. */
        CephInitRequest: {
            /** @description OSD 복제 트래픽 망(CIDR) */
            cluster_network?: string;
            /** @description 이름이나 id */
            node: string;
            /** @description mon과 클라이언트의 망(CIDR). 없으면 노드 주소가 든 인터페이스의 망 */
            public_network?: string;
        };
        /** @description mon 하나. 이름은 노드 이름이다. */
        CephMon: {
            /**
             * Format: ip
             * @description Ceph public 망 안의 주소
             */
            addr: string;
            name: string;
            /** Format: uuid */
            node: string;
        };
        /** @description `POST /ceph/mons`·`/ceph/mgrs`·`/ceph/clients` */
        CephNodeRequest: {
            /** @description 이름이나 id */
            node: string;
        };
        /** @description `POST /ceph/osds` */
        CephOsdRequest: {
            /** @description `/dev/sdb`나 `/dev/disk/by-id/…` */
            device: string;
            /** @description 이름이나 id */
            node: string;
            /** @description 디스크의 파티션·파일 시스템·LVM을 지우고 쓴다. */
            wipe?: boolean;
        };
        /** @enum {string} */
        CephPhase: "bootstrapping" | "ready";
        /** @description `POST /ceph/pools`: Ceph 풀을 만들어 Orbivisor RBD 풀 `name`으로 등록한다. */
        CephPoolRequest: {
            /** @description Orbivisor 스토리지 풀 이름 */
            name: string;
            /** @description Ceph 풀 이름. 없으면 `name` */
            pool?: string;
            /**
             * Format: int32
             * @description 복제 수(1~3). 없으면 min(3, OSD가 있는 호스트 수)
             */
            size?: number;
        };
        /** @description Ceph에서 읽은 클러스터 상태 */
        CephReport: {
            /**
             * @default {
             *       "checks": [],
             *       "status": ""
             *     }
             */
            health: components["schemas"]["CephHealth"];
            /**
             * @default {
             *       "available": false,
             *       "standbys": []
             *     }
             */
            mgr: components["schemas"]["MgrReport"];
            /** @default [] */
            mons: components["schemas"]["MonReport"][];
            /**
             * Format: int32
             * @description up OSD가 하나 이상 있는 호스트 수
             * @default 0
             */
            osd_hosts_up: number;
            /** @default [] */
            osds: components["schemas"]["OsdReport"][];
            /**
             * @default {
             *       "states": [],
             *       "total": 0
             *     }
             */
            pgs: components["schemas"]["PgReport"];
            /** @default [] */
            pools: components["schemas"]["PoolReport"][];
            /**
             * @description 정족수에 든 mon 이름
             * @default []
             */
            quorum: string[];
            /**
             * Format: int64
             * @description 읽은 시각(유닉스 초)
             * @default 0
             */
            read_at: number;
            /**
             * @default {
             *       "available_bytes": 0,
             *       "total_bytes": 0,
             *       "used_bytes": 0
             *     }
             */
            usage: components["schemas"]["Usage"];
            /**
             * Format: uuid
             * @description 읽은 노드
             * @default 00000000-0000-0000-0000-000000000000
             */
            via: string;
        };
        /** @description `GET /ceph/status`의 응답: 보고서와, 지금 읽지 못해 마지막으로 읽은 보고서를 돌려줄 때의 표시 */
        CephStatusView: components["schemas"]["CephReport"] & {
            /** @description 지금 읽지 못한 이유(`stale`일 때) */
            error?: string;
            /** @description 참이면 `report`는 `read_at`에 읽은 마지막 보고서다(지금은 읽지 못함). */
            stale?: boolean;
        };
        /** @description `GET /ceph`의 응답. 키는 담지 않는다. */
        CephView: {
            /** @example 10.1.0.0/24 */
            cluster_network?: string;
            /** Format: uuid */
            fsid: string;
            /** Format: uuid */
            init_node?: string;
            last_error?: string;
            /** @description 노드가 관리 Ceph에 붙는 클라이언트 설정(`/etc/opl/ceph/ceph.conf`) */
            managed_conf: string;
            mgrs: string[];
            mons: components["schemas"]["CephMon"][];
            osd_nodes: string[];
            phase: components["schemas"]["CephPhase"];
            /** @example 10.0.0.0/24 */
            public_network: string;
            release?: string;
            /** @description 끝나지 않은 Ceph Task */
            tasks: string[];
        };
        /**
         * @description 열 하나의 AVG·MAX·MIN. 모르는 칸(NaN)은 `null`이다. `Raw5s`는 세 열이 같고, `Digest`는 AVG만 있다(MAX·MIN은
         *     비어 있고 쓰지 않는다).
         */
        Cf: {
            avg: (number | null)[];
            max?: (number | null)[];
            min?: (number | null)[];
        };
        /**
         * @description 업데이트 채널. APT 소스 파일의 `Components`가 된다.
         * @enum {string}
         */
        Channel: "stable" | "testing" | "enterprise";
        /** @enum {string} */
        ChassisState: "missing" | "configuring" | "ready" | "down" | "unknown";
        /** @description 한 노드에서 검사 하나의 결과. 노드가 리스 갱신에 실어 리더에 보내고(판단용), 노드 상태에도 쓴다(화면용). */
        CheckReport: {
            /** @description 실패·degraded·unknown의 이유(200바이트까지) */
            error?: string;
            /** @description 검사 이름(`HaCheck` 이름이나 자동 검사 `cluster-link-<n>`) */
            name: string;
            /**
             * Format: int64
             * @description 노드 시계로 실패·degraded가 시작된 때(유닉스 초, 화면용. 리더는 자기 시계로 grace를 잰다)
             */
            since?: number;
            state: components["schemas"]["CheckState"];
        };
        /**
         * @description 검사 결과
         * @enum {string}
         */
        CheckState: "ok" | "degraded" | "failing" | "unknown";
        /** @description 받은 이미지를 확인하는 방법 */
        Checksum: {
            /** @enum {string} */
            kind: "sha256";
            value: string;
        } | {
            /** @enum {string} */
            kind: "sha512";
            value: string;
        } | {
            /** @enum {string} */
            kind: "sums";
            url: string;
        };
        /** @description 받을 청크를 묻는 본문 */
        ChunkIds: {
            /** @description 청크 id(16진 64자), 4096개까지 */
            ids: string[];
        };
        /** @description 배포(복제) 때 원본 스펙에서 바꾸는 값. 비어 있는 칸은 원본 그대로다. */
        CloneOverrides: {
            /** @description 주어진·비지 않은 칸만 원본의 cloud-init 설정을 바꾼다. 비밀번호는 crypt 해시다. */
            cloud_init?: components["schemas"]["CloudInit"];
            /** @description 새 VM의 에뮬레이터 스레드 정책(계획 64) */
            cpu_emulator?: components["schemas"]["EmulatorPolicy"];
            /** @description 새 VM의 CPU 플래그 전체(`["-avx512vbmi"]`, `[]`는 지운다, 계획 63) */
            cpu_flags?: components["schemas"]["CpuFlag"][];
            /** @description 새 VM의 CPU 모델(계획 63). API가 별칭을 버전 모델로 고정해 넣는다. */
            cpu_model?: components["schemas"]["CpuModel"];
            /** @description 새 VM의 CPU 정책(계획 64). `shared`면 스레드·에뮬레이터 정책도 지운다. */
            cpu_policy?: components["schemas"]["CpuPolicy"];
            /** @description 새 VM의 전용 CPU 스레드 정책(계획 64) */
            cpu_thread_policy?: components["schemas"]["ThreadPolicy"];
            /** @description 디스크 순번별 새 크기(바이트, 늘리기만). null은 그대로다. */
            disks?: (number | null)[];
            /**
             * Format: uuid
             * @description 새 VM을 둘 폴더(계획 62). 없으면 원본의 폴더다.
             */
            folder?: string;
            /** @description 새 VM의 hugepages(계획 64) */
            hugepages?: components["schemas"]["PageSize"];
            /** @description NIC 순번별 SDN 주소(계획 65, 주소 체계마다 하나까지). 빈 목록은 서브넷에서 새로 받는다. */
            ips?: string[][];
            /** Format: int32 */
            memory_mib?: number;
            /** @description NIC 순번별 새 네트워크 이름. null은 그대로다. */
            nics?: (string | null)[];
            /** @description 원본의 hugepages를 지운다(계획 64). */
            no_hugepages?: boolean;
            /** @description 원본의 vNUMA를 지운다(계획 64). */
            no_numa?: boolean;
            /**
             * Format: uuid
             * @description 새 VM을 켤 노드(`spec.node`). 디스크가 닿아야 한다.
             */
            node?: string;
            /** @description 새 VM의 vNUMA(계획 64) */
            numa?: components["schemas"]["NumaSpec"];
            /** @description 만든 뒤 켠다(`power: running`). */
            start?: boolean;
            /**
             * Format: int32
             * @description vCPU 수(소켓 하나에 코어로 둔다)
             */
            vcpus?: number;
        };
        CloneRequest: {
            /**
             * @description 주어진·비지 않은 칸만 원본의 cloud-init 설정을 바꾼다. 평문 비밀번호는 SHA-512 crypt로 바꿔 Task에 담고,
             *     `********`는 원본의 비밀번호를 그대로 둔다.
             */
            cloud_init?: components["schemas"]["CloudInit"];
            /** @description 새 VM의 에뮬레이터 스레드 정책(계획 64) */
            cpu_emulator?: components["schemas"]["EmulatorPolicy"];
            /** @description 새 VM의 CPU 플래그 전체(`["-avx512vbmi"]`, `[]`는 지운다, 계획 63) */
            cpu_flags?: components["schemas"]["CpuFlag"][];
            /** @description 새 VM의 CPU 모델(계획 63). 별칭은 버전 모델로 고정된다. */
            cpu_model?: components["schemas"]["CpuModel"];
            /** @description 새 VM의 CPU 정책(계획 64). `shared`면 스레드·에뮬레이터 정책도 지운다. */
            cpu_policy?: components["schemas"]["CpuPolicy"];
            /** @description 새 VM의 전용 CPU 스레드 정책(계획 64) */
            cpu_thread_policy?: components["schemas"]["ThreadPolicy"];
            /** @description 디스크 순번별 새 크기(바이트, 늘리기만, 512의 배수). null은 그대로다. */
            disks?: (number | null)[];
            /**
             * Format: uuid
             * @description 새 VM을 둘 폴더 id(계획 62). 없으면 원본의 폴더다. 대상 폴더의 `vm.create`가 있어야 한다.
             */
            folder?: string;
            /** @description 새 VM의 hugepages(계획 64) */
            hugepages?: components["schemas"]["PageSize"];
            /**
             * @description NIC 순번별 SDN 주소(계획 65, 주소 체계마다 하나까지 — 예 `[[], ["10.10.0.5"]]`). 빈 목록은 서브넷에서 새로
             *     받는다. 그 네트워크의 `network.assign`이 있어야 하고, 쓰이고 있으면 409 `ip_in_use`다.
             */
            ips?: string[][];
            /**
             * @description 연결 복제(계획 58, 템플릿에서만). 없으면 템플릿의 디스크 가운데 되는 것은 연결하고 안 되는 것은 전체 복사한다
             *     (Task 경고). `true`면 모두 연결하고(안 되면 400), `false`면 전체 복사다.
             */
            linked?: boolean;
            /** Format: int32 */
            memory_mib?: number;
            /** @description 새 VM의 이름 */
            name: string;
            /** @description NIC 순번별 새 네트워크 이름. null은 그대로다. */
            nics?: (string | null)[];
            /** @description 원본의 hugepages를 지운다(계획 64). */
            no_hugepages?: boolean;
            /** @description 원본의 vNUMA를 지운다(계획 64). */
            no_numa?: boolean;
            /** @description 새 VM을 켤 노드(이름이나 id, `spec.node`). 디스크가 닿아야 한다(공유 풀이거나 그 노드의 로컬 풀). */
            node?: string;
            /** @description 새 VM의 vNUMA(계획 64) */
            numa?: components["schemas"]["NumaSpec"];
            /** @description 새 디스크를 둘 풀. 없으면 원본 볼륨마다 그 볼륨의 풀이다. */
            pool?: string | null;
            /** @description 복제할 스냅샷(이름이나 id). 없으면 꺼진 VM의 지금 상태에서 복제한다. */
            snapshot?: string | null;
            /** @description 만든 뒤 켠다. 놓을 노드가 없으면 꺼진 채 만들고 Task 경고를 남긴다. */
            start?: boolean;
            /**
             * Format: int32
             * @description vCPU 수(소켓 하나에 코어로 둔다)
             */
            vcpus?: number;
        };
        /** @description VM의 cloud-init 설정. 비어 있는 칸은 cloud-init·배포판 기본을 따른다. */
        CloudInit: {
            dns?: components["schemas"]["CloudInitDns"];
            /** @description 호스트 이름(FQDN도 됨). 없으면 VM 이름. */
            hostname?: string;
            /** @description NIC마다의 주소 설정(순번 = `nics` 순번). 비면 cloud-init 기본(첫 NIC DHCP). */
            networks?: components["schemas"]["CloudInitNet"][];
            /** @description 기본 사용자의 비밀번호. 저장되는 것은 crypt 해시이고 응답에서는 [`PASSWORD_MASK`]로 가린다. */
            password?: string;
            /** @description 기본 사용자의 SSH 공개 키(한 줄씩) */
            ssh_keys?: string[];
            /** @description 배포판 기본 사용자의 새 이름. 없으면 배포판 기본 이름. */
            user?: string;
            /** @description 사용자 user-data. 있으면 만든 user-data 대신 그대로 쓴다(`user`·`password`·`ssh_keys`는 쓰이지 않음). */
            user_data?: string;
            /** @description vendor-data 파일. 만든 user-data와 함께 돈다(패키지·runcmd). */
            vendor_data?: string;
        };
        CloudInitDns: {
            search?: string[];
            servers?: string[];
        };
        /**
         * @description NIC 하나의 주소 설정. 빈 항목(`{}`)은 IPv4 DHCP다. `ipv4` 없이 `ipv6`만 있으면 IPv4를 두지 않는다(IPv6 전용 —
         *     DHCPv4도 쓰려면 `ipv4: {"mode": "dhcp"}`). `ipv6`이 없으면 IPv6 설정이 없다.
         */
        CloudInitNet: {
            ipv4?: components["schemas"]["IpConfig"];
            ipv6?: components["schemas"]["IpConfig"];
        };
        /**
         * @description 클러스터 모으기(`GET /metrics/cluster`)의 답. 열은 노드 열을 시각 칸끼리 합친 AVG다(CPU %는 코어 수로 가중한
         *     평균, 나머지는 합). 점 `i`의 시각은 `start + i × step_ms`다.
         */
        ClusterHistoryResponse: {
            /** @description 칸 이름 → 클러스터 열(AVG만) */
            columns: {
                [key: string]: components["schemas"]["Cf"];
            };
            /**
             * Format: int32
             * @description 열을 합친 노드 수(줄을 줄이기 전)
             */
            node_count: number;
            /** @description 답한 노드의 줄(이름 순). 20개 넘게 답했으면 범위 평균 CPU 상위 20개다. */
            nodes: components["schemas"]["ClusterNodeRow"][];
            /** @description 풀 사용량(Raft의 지금 값, 이름 순) */
            pools: components["schemas"]["ClusterPoolUsage"][];
            /** @description 다른 노드와 다른 층·칸으로 답한 노드 이름(시계가 크게 어긋난 노드). 열과 줄에 넣지 않는다. */
            skewed?: string[];
            /** Format: int64 */
            start: number;
            /** Format: int64 */
            step_ms: number;
            tier: components["schemas"]["Tier"];
            /** @description 범위 평균 CPU 상위 VM(내림차순, 10개까지) */
            top: components["schemas"]["ClusterTopVm"][];
            /** @description 답하지 않은 노드 이름 */
            unreachable: string[];
            /** @description 지표 기록을 모르는 옛 노드 이름 */
            unsupported: string[];
        };
        /** @description 클러스터 모으기의 노드 줄 하나 */
        ClusterNodeRow: {
            /** @description 칸 이름 → 범위 평균 */
            avg: {
                [key: string]: number | null;
            };
            /** @description CPU % 열(AVG, 클러스터 열과 같은 시각) */
            cpu: (number | null)[];
            /**
             * Format: int32
             * @description CPU 코어 수
             */
            cpus: number;
            name: string;
            /** Format: uuid */
            node: string;
        };
        /** @description 풀 하나의 지금 사용량. 공유 풀은 한 노드의 값, 노드 로컬 풀은 준비된 노드 값의 합이다. */
        ClusterPoolUsage: {
            name: string;
            /** Format: uuid */
            pool: string;
            shared: boolean;
            /** Format: int64 */
            total_bytes: number;
            /** Format: int64 */
            used_bytes: number;
        };
        /** @description 클러스터 모으기의 상위 VM 하나(범위 평균) */
        ClusterTopVm: {
            /**
             * Format: float
             * @description 범위 평균 CPU %
             */
            cpu?: number | null;
            /**
             * Format: float
             * @description 범위 평균 RSS(MiB)
             */
            mem_rss?: number | null;
            name: string;
            /** @description VM의 노드 이름(모르면 없음) */
            node?: string;
            /** Format: uuid */
            vm: string;
        };
        ClusterView: {
            /** @description 클러스터 CA 인증서의 지문. `orbivisorctl login --ca-fingerprint`에 쓴다. */
            ca_fingerprint: string;
            /** Format: uuid */
            leader?: string | null;
            members: components["schemas"]["MemberView"][];
            name: string;
            /**
             * Format: uuid
             * @description 이 응답을 보낸 노드의 id. 볼륨 업로드처럼 받은 노드에서 처리하는 요청에 쓴다.
             */
            node: string;
        };
        CodeRequest: {
            code: string;
            /** @description 이미 요소(패스키)가 있으면 본인 확인(그 밖의 코드·복구 코드 또는 로컬 비밀번호). 없으면 400 `proof_required`. */
            proof?: components["schemas"]["ProofRequest"];
        };
        /**
         * @description 리더 노드에서 `/bin/sh -c`로 돌리는 명령. 환경 변수 `ORBIVISOR_NODE_NAME`·`ORBIVISOR_NODE_ID`·
         *     `ORBIVISOR_NODE_ADDRESS`(노드 API 주소의 호스트)가 끌 노드를 알린다.
         */
        CommandFence: {
            /** @description 노드를 끈다. 0이 아닌 종료는 실패다. */
            off: string;
            /** @description 첫 줄에 `off`나 `on`을 찍는다(그 밖은 모름). */
            status: string;
        };
        /**
         * @description 백업 작업의 복사본을 보낼 저장소(계획 68 §12.5). 복사본의 보존은 `keep`(없으면 작업의 `keep`)으로 대상 저장소
         *     안에서 따로 고른다.
         */
        CopyTarget: {
            keep?: components["schemas"]["Keep"];
            /** @description 같은 클러스터의 다른 백업 저장소 이름 */
            store: string;
        };
        /** @description 아키텍처 하나의 클러스터 기준 */
        CpuBaselineInfo: {
            arch: components["schemas"]["Arch"];
            /** @description 리더가 기록한 기준(없으면 `cluster` VM은 `host`처럼 켠다) */
            baseline?: components["schemas"]["Baseline"];
            /**
             * Format: int32
             * @description `cluster` 모델 VM 수
             */
            cluster_vms: number;
            /** @description `Skylake-Server-v5 +pcid` 모양 */
            cpu?: string;
            /** @description 기준을 낮춘(고정이면 켤 수 없는) 노드 이름 */
            limited_by?: string[];
            /** @description 셈에 든 노드 이름 */
            nodes?: string[];
            /** @description 설정 `cpu.pinned`에 고정했다. */
            pinned: boolean;
        };
        /** @description 클러스터 CPU 기준 */
        CpuBaselines: {
            /** @description 노드가 있거나 기준이 있는 아키텍처마다 */
            arches: components["schemas"]["CpuBaselineInfo"][];
            auto_flags: boolean;
            /** @description 모든 VM에서 숨기는 플래그 */
            mask?: string[];
            mode: components["schemas"]["BaselineMode"];
        };
        /**
         * @description '+name' turns a CPU flag on, '-name' hides it
         * @example -avx512vbmi
         */
        CpuFlag: string;
        /** @description 플래그 하나 */
        CpuFlagEntry: {
            /** @description 켜면 QEMU가 라이브 마이그레이션을 막는다(`invtsc`). 그런 VM은 비우기·균형 조정·HA 검사가 옮기지 않는다. */
            blocks_migration?: boolean;
            /**
             * Format: int32
             * @description 이 이름을 아는 노드 수(끌 수 있다)
             */
            known: number;
            /** @description 클러스터 숨김 목록에 있다. */
            masked?: boolean;
            name: string;
            /** @description 루트 `cluster.admin`만 켤 수 있다(관리자가 아닌 사용자의 허용 목록 밖, 리뷰 2 #3). 끄기는 누구나 된다. */
            restricted?: boolean;
            /**
             * Format: int32
             * @description 이 플래그를 줄 수 있는 노드 수(`full max`)
             */
            supported: number;
        };
        /** @description 아키텍처 하나의 CPU 플래그 */
        CpuFlags: {
            arch: components["schemas"]["Arch"];
            flags: components["schemas"]["CpuFlagEntry"][];
            /**
             * Format: int32
             * @description 카탈로그를 알린 노드 수
             */
            nodes: number;
            /** @description 이 사용자가 `restricted` 플래그도 켤 수 있다(루트 `cluster.admin`). */
            turn_on_restricted: boolean;
        };
        /**
         * @description host, max, cluster, x86-64-v2, x86-64-v2-aes, x86-64-v3, x86-64-v4, or a versioned QEMU model (e.g. Icelake-Server-v7)
         * @example host
         */
        CpuModel: string;
        /** @description 모델 하나 */
        CpuModelEntry: {
            /** @description 별칭이면 가리키는 버전 모델(카탈로그마다 다르면 첫 것). VM에는 버전 모델로 저장된다. */
            alias_of?: string;
            /**
             * @description `cluster`: 지금 기준(`Skylake-Server-v5 +pcid`). 기준이 없으면(off, 업그레이드 중) 없다 — 그때 `cluster`는
             *     `host`처럼 켠다.
             */
            current?: string;
            deprecated?: boolean;
            kind: components["schemas"]["CpuModelKind"];
            name: string;
            /** @description 노드마다(루트 `cluster.read`가 있을 때만) */
            nodes?: components["schemas"]["CpuNodeFit"][];
            /**
             * Format: int32
             * @description 켤 수 있는 노드 수
             */
            usable: number;
            vendor?: string;
        };
        /**
         * @description 모델의 종류
         * @enum {string}
         */
        CpuModelKind: "basic" | "cluster" | "level" | "qemu";
        /** @description 아키텍처 하나의 CPU 모델 */
        CpuModels: {
            arch: components["schemas"]["Arch"];
            models: components["schemas"]["CpuModelEntry"][];
            /**
             * Format: int32
             * @description 이 아키텍처의 노드 수(위트니스 제외, `node`를 주면 1)
             */
            nodes: number;
        };
        /** @description 노드 하나가 CPU를 켤 수 있는지 */
        CpuNodeFit: {
            can_run: boolean;
            /** @description 빠진 플래그(끄지 않았는데 가속기가 못 주는 것, 켰는데 지원하지 않는 것) */
            missing?: string[];
            node: string;
            /** @description 켤 수 없는 까닭 */
            reason?: string;
            /** @description 막지 않는 경고(`host` VM을 옮기면 대상에 없는 플래그) */
            warnings?: string[];
        };
        /**
         * @description vCPU 정책(OpenStack `hw:cpu_policy`)
         * @enum {string}
         */
        CpuPolicy: "shared" | "dedicated";
        /**
         * @description Linux CPU list: numbers and ranges, e.g. 0-3,8,10-11
         * @example 0-3,8
         */
        CpuSet: string;
        /** @description 노드의 세 CPU 집합(§7) */
        CpuSets: {
            dedicated: components["schemas"]["CpuSet"];
            reserved: components["schemas"]["CpuSet"];
            shared: components["schemas"]["CpuSet"];
        };
        /** @description 설정 `cpu`(계획 63) */
        CpuSettings: {
            /**
             * @description auto 기준에 모든 노드가 주는 플래그를 더한다.
             * @default true
             */
            auto_flags: boolean;
            baseline?: components["schemas"]["BaselineMode"];
            /** @description 모든 VM(`host` 포함)에서 숨길 플래그(API가 `normalize_mask`로 맞춰 저장) */
            mask?: string[];
            /** @description 아키텍처마다 고정한 기준. 있는 아키텍처는 auto 대신 이것이다(`baseline`이 off여도). */
            pinned?: components["schemas"]["PinnedBaseline"][];
        };
        /** @description 설정 `cpu`에서 바꿀 값. 준 칸만 바꾼다. */
        CpuSettingsPatch: {
            /** @description auto 기준에 모든 노드가 주는 플래그를 더한다(기본 참). */
            auto_flags?: boolean;
            /** @description `off`(기본)이나 `auto`(리더가 모든 노드가 켤 수 있는 가장 좋은 모델을 셈함, x86_64만) */
            baseline?: components["schemas"]["BaselineMode"];
            /** @description 고정 기준을 켤 수 없는 노드가 있어도 저장한다(그 노드는 `cluster` VM을 받지 않는다). */
            force?: boolean;
            /** @description 모든 VM(`host` 포함)에서 숨길 플래그 전체(`[]`는 지운다). 다음 전원 껐다 켜기부터 적용된다. */
            mask?: string[];
            /** @description 아키텍처마다 고정한 기준 전체(`[]`는 지운다). 별칭은 버전 모델로 고정된다. */
            pinned?: components["schemas"]["PinnedBaseline"][];
        };
        /**
         * @description 해석된 CPU가 어디서 왔나
         * @enum {string}
         */
        CpuSource: "spec" | "cluster";
        CpuSpec: {
            /** Format: int32 */
            cores: number;
            /** @description 전용 VM의 에뮬레이터 스레드 정책(계획 64, 기본 `shared`). `dedicated`일 때만 준다. */
            emulator?: components["schemas"]["EmulatorPolicy"];
            /** @description 켜고 끌 CPU 플래그(계획 63, `"+aes"`·`"-avx512vbmi"`). 비면 쓰지 않아 옛 노드도 VM을 계속 푼다. */
            flags?: components["schemas"]["CpuFlag"][];
            /**
             * Format: int32
             * @description vCPU 핫플러그 상한(계획 54). 없으면 `sockets × cores`와 같다(핫플러그 꺼짐). 실행 중에는 `cores`를 이 안에서
             *     바꾼다(소켓마다 코어 `max_vcpus / sockets`개 자리).
             */
            max_vcpus?: number;
            model?: components["schemas"]["CpuModel"];
            /** @description vCPU 정책(계획 64): `shared`(기본, 쓰지 않음)·`dedicated`(vCPU마다 노드의 전용 pCPU 하나) */
            policy?: components["schemas"]["CpuPolicy"];
            /** Format: int32 */
            sockets?: number;
            /** @description 전용 VM의 SMT 형제 스레드 정책(계획 64, 기본 `prefer`). `dedicated`일 때만 준다. */
            thread_policy?: components["schemas"]["ThreadPolicy"];
        };
        CreateAccessRoleRequest: {
            description?: string;
            name: string;
            privileges: string[];
        };
        CreateBackupRequest: {
            /**
             * @description 할 수 있으면 증분으로(같은 풀의 바로 앞 백업을 부모로 바뀐 블록만) 백업한다. 할 수 없으면
             *     전체로 하고 그 이유를 `status.full_reason`에 적는다.
             */
            incremental?: boolean;
            /** @description 백업 이름(클러스터에서 고유). 없으면 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC)다. */
            name?: string | null;
            /** @description 백업 풀(`backups`를 켠 풀). `store`와 둘 중 하나다. */
            pool?: string;
            /** @description 백업 저장소(계획 68). `pool`과 둘 중 하나다. */
            store?: string;
        };
        /** @description `POST /floating-ips` */
        CreateFloatingIp: {
            labels?: {
                [key: string]: string;
            };
            /** @description 없으면 주소의 점을 `-`로 바꾼 것(`203-0-113-120`) */
            name?: string;
            spec: components["schemas"]["FloatingIpSpec"];
        };
        CreateFolderRequest: {
            kind: components["schemas"]["FolderKind"];
            /** @description 화면 이름(1~64자, `/` 없음). 형제 사이에서 대소문자 무시로 겹치면 409 `name_taken` */
            name: string;
            /**
             * Format: uuid
             * @description 부모 폴더 id. 없으면 그 트리의 루트 바로 아래다.
             */
            parent?: string;
            /** @description 하위 트리 할당량(VM 폴더만). 부모(없으면 루트)의 `inventory.quota`가 있어야 한다. */
            quota?: components["schemas"]["FolderQuota"];
        };
        CreateGrantRequest: {
            principal: components["schemas"]["PrincipalRef"];
            /**
             * @description 하위 폴더와 그 안의 객체로 내려가는지(기본 참)
             * @default true
             */
            propagate: boolean;
            role: components["schemas"]["RoleRef"];
            target: components["schemas"]["GrantTarget"];
        };
        /** @description `POST /load-balancers` */
        CreateLoadBalancer: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            /** @description `monitors`는 API가 채운다(주면 400). */
            spec: components["schemas"]["LoadBalancerSpec"];
        };
        /** @description `POST /network-migrations` */
        CreateNetworkMigration: {
            labels?: {
                [key: string]: string;
            };
            /** @description 비면 `<원본>-to-<대상>`(쓰였으면 `-2`, `-3` …) */
            name?: string;
            spec: components["schemas"]["NetworkMigrationSpec"];
        };
        CreateRequest_AlertRuleSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            /** @description 경보 규칙 하나 */
            spec: {
                condition: components["schemas"]["AlertCondition"];
                /**
                 * @description 끄면 평가하지 않고, 활성 경보는 알림 없이 해제한다.
                 * @default true
                 */
                enabled: boolean;
                /**
                 * Format: int32
                 * @description 조건이 이만큼(초) 참이어야 알린다(0~86400). 이벤트 조건은 0이다.
                 * @default 0
                 */
                for_secs: number;
                /** @description 이 노드(이름)들만. 비면 모든 노드 */
                nodes?: string[];
                /**
                 * Format: int32
                 * @description 경보가 이어지는 동안 이만큼(초)마다 다시 알린다. 0이면 다시 보내지 않는다.
                 * @default 14400
                 */
                repeat_secs: number;
                /**
                 * @description 해제도 알린다(이벤트 조건은 해제가 없다).
                 * @default true
                 */
                send_resolved: boolean;
                severity: components["schemas"]["Severity"];
                /** @description 알릴 `NotificationTarget` 이름(1~16개) */
                targets: string[];
                /** @description 이 VM(이름)들만(VM 조건만). 비면 모든 VM */
                vms?: string[];
            };
        };
        CreateRequest_BackupJobSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            spec: {
                /** @description 작업의 백업이 `ready`가 되면 다른 저장소로 복사본을 보낸다(계획 68 §12.5 — 계층). 없으면 쓰지 않는다. */
                copy_to?: components["schemas"]["CopyTarget"];
                /**
                 * @description 끄면 예약 실행을 하지 않는다.
                 * @default true
                 */
                enabled: boolean;
                /**
                 * Format: int32
                 * @description 0보다 크면 증분으로 백업하되(할 수 있으면) 체인의 증분이 이만큼 쌓이면 다음은 전체로 한다.
                 *     0이면 늘 전체다(뺀다).
                 *     저장소 작업(`store`)에서는 "n번 연속 비트맵으로 읽었으면 다음은 디스크 전체를 읽기"다(계획 68 §8.3 — 저장 공간은
                 *     늘지 않는다).
                 */
                full_every?: number;
                /** @description 보존 규칙. 모두 0이면 지우지 않는다. */
                keep?: components["schemas"]["Keep"];
                /**
                 * @description 백업 풀 이름. 백업 저장소(`store`, 계획 68)로 백업하는 작업이면 비우고 쓰지 않는다(옛 JSON에는 늘 있으므로
                 *     바이트 그대로다). `pool`과 `store` 중 정확히 하나다.
                 */
                pool?: string;
                /** @description 만든 백업을 보낼 원격 클러스터의 백업 풀. 없으면 보내지 않는다. */
                replicate?: components["schemas"]["ReplicateTarget"];
                schedule: components["schemas"]["JobSchedule"];
                /** @description 백업 저장소 이름(계획 68). 없으면 쓰지 않는다. */
                store?: string;
                /** @description 백업할 VM */
                vms: components["schemas"]["VmSelector"];
            };
        };
        CreateRequest_GroupSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            /** @description 로컬 그룹(`Kind::Group`). 외부 사용자는 그 영역의 `external_groups`에 `external`의 그룹이 있으면 든다. */
            spec: {
                description?: string;
                /** @description 연결한 디렉터리 그룹 */
                external?: components["schemas"]["ExternalGroup"][];
                /** @description 멤버 사용자 id(로컬·외부) */
                members?: string[];
            };
        };
        CreateRequest_HaCheckSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            /** @description 검사 하나 */
            spec: {
                /**
                 * @description 끄면 노드가 검사하지 않는다.
                 * @default true
                 */
                enabled: boolean;
                /**
                 * Format: int32
                 * @description 이만큼(초) 넘게 실패해야 대응한다. 10~600
                 * @default 10
                 */
                grace_secs: number;
                /** @description 노드 이름 → 그 노드의 대상(종류는 `target`과 같아야 한다) */
                nodes?: {
                    [key: string]: components["schemas"]["HaCheckTarget"];
                };
                /**
                 * @description 실패했을 때의 대응. 없으면 역할의 기본(`storage` → `restart_elsewhere`, `vm` → `migrate`,
                 *     `management` → `warn`)
                 */
                on_fail?: components["schemas"]["HaCheckAction"];
                /** @description 검사하는 네트워크·장치의 쓰임. 기본 대응과 영향받는 VM을 정한다. */
                role: components["schemas"]["HaCheckRole"];
                /** @description 모든 노드의 검사 대상(노드별 덮기가 없을 때) */
                target: components["schemas"]["HaCheckTarget"];
            };
        };
        CreateRequest_HaGroupSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            /** @description HA 그룹: HA VM을 복구할 노드와 그 우선순위. VM은 `ha.group`으로 그룹을 쓴다. */
            spec: {
                /** @description 그룹 노드. 우선순위가 큰 노드에 먼저 놓는다. */
                nodes: components["schemas"]["HaGroupNode"][];
                /** @description 켜면 우선순위가 더 높은 노드가 돌아와도 되돌아가지 않는다. */
                nofailback?: boolean;
                /** @description 켜면 그룹 밖 노드에는 놓지 않는다(놓을 곳이 없으면 켜지 않는다). */
                restricted?: boolean;
            };
        };
        CreateRequest_NetworkSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            /** @description `bridge`·`vxlan`·`sdn` 중 정확히 하나를 쓴다. */
            spec: {
                /** @description 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`) */
                bridge?: string;
                /**
                 * Format: uuid
                 * @description 든 네트워크 폴더(계획 62, `Folder` id). 없으면 네트워크 트리의 루트이고 쓰지 않아 옛 노드도 네트워크를 푼다.
                 */
                folder?: string;
                /**
                 * Format: int32
                 * @description VXLAN·SDN 네트워크의 MTU(1280~9000, 기본 1450·1442). `vxlan`·`sdn`과만 쓴다.
                 */
                mtu?: number;
                /** @description OVN 논리 스위치(Geneve)로 만드는 SDN 네트워크(계획 65). 없으면 쓰지 않아 옛 노드도 네트워크를 푼다. */
                sdn?: components["schemas"]["SdnNet"];
                /** @description SDN 네트워크의 서브넷(주소 체계마다 하나까지, 계획 65). `sdn`과만 쓰고 비면 쓰지 않는다. */
                subnets?: components["schemas"]["Subnet"][];
                /**
                 * Format: int32
                 * @description VLAN 태그(1~4094). `bridge`와만 쓰고, 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
                 */
                vlan?: number | null;
                /** @description orbivisord가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만드는 네트워크 */
                vxlan?: components["schemas"]["Vxlan"];
            };
        };
        CreateRequest_PlacementRuleSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * @description 배치 규칙: VM끼리(같은 노드·다른 노드) 또는 VM과 노드(이 노드들 중·이 노드들은 안 됨). hard 규칙은
             *     조건이고 soft 규칙은 벌점(`weight`)이다. 모든 VM에 적용한다.
             */
            spec: {
                /**
                 * @description 끄면 없는 것으로 본다.
                 * @default true
                 */
                enabled: boolean;
                /** @description hard 규칙만: HA 복구가 지킬 수 없으면 어기고 켠다. */
                ha_may_violate?: boolean;
                kind: components["schemas"]["RuleKind"];
                /**
                 * @description 이 레이블(`key=value`, 예: `zone=rack-a`)을 가진 노드들
                 * @example zone=rack-a
                 */
                node_tag?: string;
                /** @description VM과 노드 규칙의 노드 이름. `node_tag`와 둘 중 하나만. */
                nodes?: string[];
                /**
                 * @description 이 레이블(`key=value`)을 가진 VM들
                 * @example app=web
                 */
                vm_tag?: string;
                /** @description 규칙의 VM 이름(아직 없는 VM이어도 된다). `vm_tag`와 둘 중 하나만. */
                vms?: string[];
                /**
                 * Format: int32
                 * @description soft 규칙의 벌점 1~100. 없으면 50.
                 */
                weight?: number;
            };
        };
        CreateRequest_SecurityGroupSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            spec: {
                /** @description 200자 이하 */
                description?: string;
                /** @description 적힌 순서대로 보고 처음 맞는 규칙을 따른다(200개 이하). */
                rules?: components["schemas"]["Rule"][];
            };
        };
        CreateRequest_StoragePoolSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            spec: {
                backend: components["schemas"]["PoolBackend"];
                /** @description 백업 파일(`{루트}/backups/`)을 둘 수 있는 백업 풀인지. dir·NFS 풀만 된다. 켜도 VM 디스크는 그대로 둔다. */
                backups?: boolean;
                /**
                 * Format: uuid
                 * @description 든 스토리지 폴더(계획 62, `Folder` id). 없으면 스토리지 트리의 루트이고 쓰지 않아 옛 노드도 풀을 푼다.
                 */
                folder?: string;
                /** @description 이 풀을 쓸 수 있는 노드. 비어 있으면 모든 노드. */
                nodes?: string[];
                /** @description 모든 노드가 같은 데이터를 보는 공유 스토리지인지. NFS·RBD·공유 LVM 풀은 늘 공유다(`is_shared`). */
                shared?: boolean;
            };
        };
        CreateRequest_VmSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            /** @description VM의 원하는 상태. */
            spec: {
                /** @description CD-ROM 드라이브(계획 57, 넷까지, 순번 = 드라이브 번호). 비면 쓰지 않아 옛 노드도 VM을 푼다. */
                cdroms?: components["schemas"]["CdromSpec"][];
                /** @description cloud-init NoCloud 씨앗(계획 57). 없으면 쓰지 않아 옛 노드도 VM을 푼다. */
                cloud_init?: components["schemas"]["CloudInit"];
                cpu: components["schemas"]["CpuSpec"];
                disks?: components["schemas"]["DiskSpec"][];
                firmware?: components["schemas"]["Firmware"];
                /**
                 * Format: uuid
                 * @description 든 VM 폴더(계획 62, `Folder` id). 없으면 VM 트리의 루트이고 쓰지 않아 옛 노드도 VM을 푼다. 템플릿도 같은
                 *     트리에 산다.
                 */
                folder?: string;
                /**
                 * @description 게스트 에이전트(계획 59): 채널을 달고 게스트 정보·에이전트 종료·fs-freeze에 쓴다. 없으면 쓰지 않아 옛 노드도
                 *     VM을 푼다(`ha.guest_watch`가 있으면 채널은 그대로 단다).
                 */
                guest_agent?: components["schemas"]["GuestAgentSpec"];
                /**
                 * @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다.
                 *     JSON은 옛 bool(`false` = 없음, `true` = 기본 정책)이나 정책 객체다.
                 */
                ha?: components["schemas"]["HaSetting"];
                /** @description 게스트 메모리를 hugepages로(계획 64, `2M`·`1G`·`any`). 없으면 보통 페이지이고 쓰지 않는다. */
                hugepages?: components["schemas"]["PageSize"];
                machine: components["schemas"]["MachineType"];
                /**
                 * Format: int32
                 * @description 메모리 핫플러그 상한(MiB, 계획 54). 없으면 `memory_mib`와 같다(핫플러그 꺼짐). 없으면 쓰지 않아 옛 노드도
                 *     VM을 푼다.
                 */
                max_memory_mib?: number;
                /** Format: int32 */
                memory_mib: number;
                nics?: components["schemas"]["NicSpec"][];
                /**
                 * Format: uuid
                 * @description 배치된 노드의 id. None이면 시작할 때 스케줄러가 고른다.
                 */
                node?: string | null;
                /**
                 * @description 게스트 vNUMA(계획 64). 없으면 전용·hugepages VM은 `auto`, 그 밖은 vNUMA 없음(지금 그대로)이고 쓰지 않아 옛
                 *     노드도 VM을 푼다.
                 */
                numa?: components["schemas"]["NumaSpec"];
                /** @description 최적 배치 옵션(계획 53). 없으면 쓰지 않아 옛 노드도 VM을 푼다. */
                placement?: components["schemas"]["VmPlacement"];
                power?: components["schemas"]["PowerState"];
                /**
                 * @description 로컬 디스크 복제 대상(계획 55, 셋까지). 노드 로컬 풀의 디스크를 이 노드들에 주기적으로 복제한다. 비면 쓰지
                 *     않아 옛 노드도 VM을 푼다.
                 */
                replication?: components["schemas"]["ReplicationTarget"][];
                restart?: components["schemas"]["RestartPolicy"];
                /**
                 * @description Secure Boot(계획 59): Microsoft 키를 등록한 UEFI 변수로 켜는 secure-boot 펌웨어(x86_64는 SMM). UEFI VM만 쓸 수
                 *     있다. 거짓이면 쓰지 않아 옛 노드도 VM을 푼다.
                 */
                secure_boot?: boolean;
                snapshot_schedule?: components["schemas"]["SnapshotSchedule"] | null;
                /**
                 * @description 템플릿(계획 58): 켤 수 없고 디스크를 바꿀 수 없는 원본. 배포(복제)와 연결 복제의 기반이 된다. 거짓이면 쓰지
                 *     않아 옛 노드도 VM을 푼다.
                 */
                template?: boolean;
                /**
                 * @description TPM 2.0(계획 59): VM마다 swtpm 하나를 띄워 단다(q35 `tpm-crb`, virt `tpm-tis-device`). UEFI VM만 쓸 수 있다.
                 *     상태는 노드 로컬 파일이고 `firmware_node`를 따라다닌다. 거짓이면 쓰지 않아 옛 노드도 VM을 푼다.
                 */
                tpm?: boolean;
            };
        };
        CreateRequest_VolumeSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            spec: {
                /**
                 * Format: uuid
                 * @description 연결 복제(계획 58)면 기반(템플릿 디스크) 볼륨의 id. 기반은 늘 같은 풀에 있다. 없으면 쓰지 않아 옛 노드도
                 *     볼륨을 푼다.
                 */
                base?: string;
                /** @description 내용 종류(계획 57). 디스크(기본)면 쓰지 않아 옛 노드도 볼륨을 푼다. */
                content?: components["schemas"]["VolumeContent"];
                format: components["schemas"]["ImageFormat"];
                /** @description StoragePool 리소스 이름 */
                pool: string;
                /** Format: int64 */
                size_bytes: number;
            };
        };
        /** @description `POST /routers` */
        CreateRouter: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            spec: components["schemas"]["RouterSpec"];
        };
        CreateSnapshotRequest: {
            /** @description 최대 1024자 */
            description?: string;
            /** @description 메모리 상태도 담는다. 기본은 VM이 실행 중이고 디스크가 있으며 실행 중에 바꾼 하드웨어가 없으면 true다. */
            memory?: boolean | null;
            /** @description VM 안에서 고유한 이름 */
            name: string;
        };
        /** @description 저장소 만들기 본문 */
        CreateStoreRequest: {
            /** @description 없으면 평문 저장소 */
            encryption?: components["schemas"]["EncryptionRequest"];
            /**
             * @description 만들 때 주 키를 이 암호 문구로 감싸 저장소 안에 둔다(escrow — 클러스터를 잃어도 연다). 암호 문구는 저장하지
             *     않는다.
             */
            escrow_passphrase?: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            spec: components["schemas"]["StoreSpecRequest"];
        };
        CreateTokenRequest: {
            name: string;
            /**
             * @description 권한 범위(계획 62): 토큰의 권한은 주인 권한과 이 범위(`targets`마다 `privileges`를 건 propagate grant)의
             *     교집합이다. 없으면 주인과 같다.
             */
            scope?: components["schemas"]["TokenScope"];
        };
        CreateUserRequest: {
            name: string;
            password: string;
            role: components["schemas"]["Role"];
        };
        CreatedJoinToken: {
            /**
             * Format: int64
             * @description 만료 시각(유닉스 초)
             */
            expires_at: number;
            /** @description 새 노드에서 `orbivisorctl cluster join`에 넘길 URL. 다시 볼 수 없다. */
            url: string;
        };
        /** @description 만든 저장소 */
        CreatedStore: {
            /** @description 새 키를 만들었으면 그 복구 키(이때 한 번 — 나중에는 `key/export`). 잃으면 백업을 되살릴 길이 없다. */
            recovery_key?: string;
            store: components["schemas"]["Resource_BackupStoreSpecView_BackupStoreStatus"];
            /** @description 초기화 Task(OpenAPI가 별칭 `Task`를 남의 `Resource`로 잇지 않게 전체 이름으로 쓴다) */
            task: components["schemas"]["Resource_TaskSpec_TaskStatus"];
        };
        CreatedToken: {
            /** @description `Authorization: Bearer <secret>`로 쓴다. 다시 볼 수 없다. */
            secret: string;
            token: components["schemas"]["TokenView"];
        };
        /** @description `PublicKeyCredentialCreationOptions`(이진 값은 base64url) */
        CreationOptions: {
            attestation: string;
            authenticatorSelection: components["schemas"]["AuthenticatorSelection"];
            challenge: string;
            excludeCredentials: components["schemas"]["CredentialDescriptor"][];
            pubKeyCredParams: components["schemas"]["CredentialParameter"][];
            rp: components["schemas"]["RelyingParty"];
            /** Format: int64 */
            timeout: number;
            user: components["schemas"]["WebauthnUser"];
        };
        /** @description 패스키 자격 증명 설명자 */
        CredentialDescriptor: {
            /** @description 자격 증명 id(base64url) */
            id: string;
            type: string;
        };
        CredentialParameter: {
            /**
             * Format: int64
             * @description COSE 알고리즘(-7 ES256, -8 EdDSA, -257 RS256)
             */
            alg: number;
            type: string;
        };
        /** @enum {string} */
        Direction: "in" | "out";
        /** @enum {string} */
        DiskBus: "virtio-blk" | "sata";
        /**
         * @description 마이그레이션이 복사하는 디스크 하나: 볼륨 `volume`을 대상 노드의 풀 `pool`에 `format`으로 만든다.
         *     원본 풀·형식은 볼륨 기록에 그대로 있다(확정 전에는 바꾸지 않는다).
         */
        DiskMove: {
            /**
             * @description 대상 노드에 이미 이 볼륨의 복제본이 있다(계획 55): 새로 만들지 않고 복제 뒤 바뀐 것만 보낸다. false면
             *     빼고 써서 옛 노드도 읽는다.
             */
            existing?: boolean;
            format: components["schemas"]["ImageFormat"];
            pool: string;
            /** Format: uuid */
            volume: string;
        };
        DiskSpec: {
            /** Format: int32 */
            boot_order?: number | null;
            bus: components["schemas"]["DiskBus"];
            /** @description Volume 리소스 이름 */
            volume: string;
        };
        /** @description 디스크를 OSD로 쓸 수 있는지 */
        DiskState: {
            /** @enum {string} */
            state: "available";
        } | {
            reasons: string[];
            /** @enum {string} */
            state: "wipeable";
        } | {
            reason: string;
            /** @enum {string} */
            state: "in-use";
        };
        /** @description 유효 권한과 그 근거 */
        EffectivePermissions: {
            /** @description 이 대상에 닿는 그 사용자(와 그 그룹)의 grant */
            grants: components["schemas"]["Resource"][];
            name: string;
            object: string;
            privileges: string[];
            /** @description 전역 역할(루트에 건 암묵 grant) */
            role: components["schemas"]["Role"];
            /** Format: uuid */
            user: string;
        };
        /**
         * @description 전용 VM의 QEMU 에뮬레이터 스레드(메인 루프·I/O·일꾼) 정책(OpenStack `hw:emulator_threads_policy`)
         * @enum {string}
         */
        EmulatorPolicy: "shared" | "isolate";
        /** @description 만들기의 암호화: 셋 가운데 하나 — 새 키(`generate`), 복구 키 가져오기, 저장소 안 escrow를 암호 문구로 풀기 */
        EncryptionRequest: {
            /** @description 저장소 `keys/<지문>.json`을 풀 암호 문구 */
            escrow_passphrase?: string;
            generate?: boolean;
            /** @description 복구 키(`OPK1-…`) */
            recovery_key?: string;
        };
        /** @description 오류 응답 본문 */
        ErrorBody: {
            /** @description 기계가 읽는 오류 종류. 예: `not_found`, `revision_mismatch`, `cluster_no_quorum` */
            code: string;
            details?: unknown;
            message: string;
        };
        /** @description 감싸 둘 암호 문구 */
        EscrowRequest: {
            passphrase: string;
        };
        /** @description browse 응답 */
        EsxiBrowse: {
            datastores: components["schemas"]["EsxiDatastore"][];
        };
        /** @description ESXi·vCenter 연결(API 요청 본문). 비밀번호가 있으므로 저장하지 않고 로그에도 찍지 않는다. */
        EsxiConnection: {
            /**
             * @description vCenter면 데이터센터 이름
             * @default ha-datacenter
             */
            datacenter: string;
            /** @description 믿는 서버 인증서의 SHA-256 지문. 없으면 서버가 지문을 알려 주며 거절한다. */
            fingerprint?: string | null;
            host: string;
            password: string;
            /**
             * Format: int32
             * @default 443
             */
            port: number;
            user: string;
        };
        EsxiDatastore: {
            name: string;
            /** @description 맨 위 디렉터리마다 첫 `.vmx`(경로 순서) */
            vms: components["schemas"]["EsxiVm"][];
        };
        /** @description ESXi에서 가져오기 만들기 요청 */
        EsxiImportRequest: {
            /** @description 연결(`fingerprint`가 있어야 한다) */
            connection: components["schemas"]["EsxiConnection"];
            datastore: string;
            /** @description 가져오기 이름 */
            name: string;
            /** @description 데이터스토어 안의 `.vmx` 경로(`web01/web01.vmx`) */
            vmx: string;
        };
        EsxiSource: {
            /** @description 데이터센터(ESXi는 `ha-datacenter`) */
            datacenter: string;
            datastore: string;
            /** @description 믿기로 한 서버 인증서의 SHA-256 지문(대문자 콜론 구분 `AB:CD:…`) */
            fingerprint: string;
            host: string;
            /** Format: int32 */
            port: number;
            user: string;
            /** @description 데이터스토어 안의 `.vmx` 경로(예: `web01/web01.vmx`) */
            vmx: string;
        };
        /** @description 데이터스토어의 VM 요약. `.vmx`를 해석할 수 없으면 `error`가 있고 나머지는 빈 값이다. */
        EsxiVm: {
            /** Format: int32 */
            disks: number;
            error?: string | null;
            guest_os: string;
            /**
             * @description 디렉터리에 잠금·스왑 파일(`.lck`, `.lck-…`, `.vswp`)이 있거나(켜져 있을 수 있다) 일시 중단 파일
             *     (`.vmss`)이 있다
             */
            locked: boolean;
            /** Format: int64 */
            memory_mib: number;
            name: string;
            /** Format: int32 */
            vcpus: number;
            /** @description 데이터스토어 안의 `.vmx` 경로(`web01/web01.vmx`) */
            vmx: string;
        };
        /** @description 영역의 디렉터리 그룹(LDAP 그룹 DN 또는 OIDC `groups` 클레임 값) */
        ExternalGroup: {
            group: string;
            realm: string;
        };
        /** @description 이웃 노드의 전원 읽기 결과(설계 §8.1). `error`가 있으면 모든 방법이 실패한 것이다(`fence_unreachable`). */
        FenceCheck: {
            /**
             * Format: int64
             * @description 읽은 시각(유닉스 초)
             */
            at: number;
            /** @description 읽은 이웃 노드의 이름 */
            by: string;
            /** @description 모든 방법이 전원을 읽지 못한 이유(방법마다 `"<종류>: <이유>"`를 `; `로). 하나라도 읽었으면 None */
            error?: string;
        };
        /**
         * @description 방법의 종류
         * @enum {string}
         */
        FenceKind: "ipmi" | "redfish" | "command";
        /** @description 노드를 끄는 방법 하나 */
        FenceMethod: (components["schemas"]["IpmiFence"] & {
            /** @enum {string} */
            kind: "ipmi";
        }) | (components["schemas"]["RedfishFence"] & {
            /** @enum {string} */
            kind: "redfish";
        }) | (components["schemas"]["CommandFence"] & {
            /** @enum {string} */
            kind: "command";
        });
        /** @description 방법 하나로 전원을 읽은 결과(`fence-test`, 이웃 확인) */
        FenceMethodStatus: {
            /** @description 읽지 못한 이유 */
            error?: string;
            kind: components["schemas"]["FenceKind"];
            /** @description 읽지 못했으면 `unknown` */
            power: components["schemas"]["Power"];
        };
        FenceMethodView: (components["schemas"]["IpmiFenceView"] & {
            /** @enum {string} */
            kind: "ipmi";
        }) | (components["schemas"]["RedfishFenceView"] & {
            /** @enum {string} */
            kind: "redfish";
        }) | (components["schemas"]["CommandFence"] & {
            /** @enum {string} */
            kind: "command";
        });
        /** @description `fence-test`의 결과 */
        FenceTestResult: {
            /** @description 전원을 읽은 노드의 이름(시험하는 노드 자신이면 이웃) */
            checked_by: string;
            /** @description 방법마다의 결과(방법 순서) */
            methods: components["schemas"]["FenceMethodStatus"][];
        };
        /**
         * @description 리스를 잃은 노드가 HA VM을 끄는 방법
         * @enum {string}
         */
        Fencing: "watchdog" | "process";
        FinishRegistrationRequest: {
            credential: components["schemas"]["RegistrationCredential"];
            /** @description 패스키 이름(1~64자) */
            name: string;
            /** @description 이미 요소가 있으면 본인 확인(지금 요소의 코드·복구 코드 또는 로컬 비밀번호). 없으면 400 `proof_required`. */
            proof?: components["schemas"]["ProofRequest"];
            /** @description 등록 시작이 준 토큰 */
            token: string;
        };
        /** @description 유동 IP의 지금 모양(Raft에서 셈 — OvnSync가 쓰는 NAT 행과 같은 규칙, §9.5) */
        FipState: {
            /** @enum {string} */
            kind: "unattached";
        } | {
            /** @enum {string} */
            kind: "distributed";
            /** @description VM이 도는 노드(마이그레이션 중이면 둘, 쉼표로). 루트 `cluster.read`가 없으면 빈 글 */
            node: string;
        } | {
            /** @enum {string} */
            kind: "centralized";
            reason: string;
        } | {
            /** @enum {string} */
            kind: "unrouted";
            reason: string;
        };
        /** @description 유동 IP가 붙은 NIC. NIC는 MAC으로 가리킨다(순서가 바뀌어도 같은 NIC — OVN 포트 이름과 같다). */
        FipTarget: {
            mac: components["schemas"]["MacAddr"];
            /** Format: uuid */
            vm: string;
        };
        /** @enum {string} */
        Firmware: "bios" | "uefi";
        /** @description 노드의 펌웨어 능력(계획 59). 스케줄러가 TPM·Secure Boot VM을 놓을 노드를 거른다. */
        FirmwareCaps: {
            /** @description Secure Boot: `secure-boot`·`enrolled-keys` 펌웨어를 찾았다(firmware descriptor나 설정). */
            secure_boot?: boolean;
            /** @description TPM 2.0: `swtpm`·`swtpm_setup`이 있고 swtpm이 TPM 2.0을 하며 QEMU에 `emulator` TPM 백엔드가 있다. */
            tpm?: boolean;
        };
        /** @description 초기화할 것. 적어도 하나는 참이어야 한다. */
        FirmwareResetRequest: {
            /**
             * @description UEFI 변수 초기화와 함께 VM 스펙의 `secure_boot`를 이 값으로 바꾼다(`uefi_vars`가 있어야 한다). 변수가 있는
             *     VM의 Secure Boot는 이 길로만 바꾼다(`PUT`은 409 `firmware_reset_required`).
             */
            secure_boot?: boolean;
            /** @description TPM 상태를 버린다(다음 켜기가 새 TPM을 만든다). TPM에 봉인한 비밀(BitLocker·LUKS 키)을 잃는다. */
            tpm?: boolean;
            /** @description UEFI 변수(부팅 항목, Secure Boot 키)를 펌웨어 원본으로 되돌린다. */
            uefi_vars?: boolean;
        };
        /** @description `GET /floating-ips` */
        FloatingIpList: {
            items: components["schemas"]["FloatingIpView"][];
            /** Format: int64 */
            revision: number;
        };
        /** @description `PATCH /floating-ips/{key}` */
        FloatingIpPatch: {
            /** @description 빈 글이면 설명을 지운다. */
            description?: string;
            target?: components["schemas"]["FipTarget"] | null;
        };
        FloatingIpSpec: {
            /**
             * Format: ip
             * @description 유동 IP(IPv4). API가 그 네트워크의 유동 IP 풀(없으면 풀)에서 채운다.
             */
            address?: string;
            /** @description 200자 이하 */
            description?: string;
            /**
             * Format: uuid
             * @description 든 네트워크 폴더(`Folder` id). 없으면 네트워크 트리의 루트다.
             */
            folder?: string;
            /** @description 공급자 네트워크 이름 */
            network: string;
            /** @description 붙은 VM NIC. 없으면 떼어 둔 것이다. */
            target?: components["schemas"]["FipTarget"];
        };
        FloatingIpStatus: Record<string, never>;
        /** @description `GET /floating-ips/{key}` */
        FloatingIpView: components["schemas"]["Resource_FloatingIpSpec_FloatingIpStatus"] & {
            state: components["schemas"]["FipState"];
            /** @description 붙은 VM의 이름(부른 사용자가 읽을 수 있을 때만) */
            vm_name?: string;
        };
        /** @description 유동 IP 풀의 크기와 사용 */
        FloatingUsage: {
            /** Format: int64 */
            free: number;
            /** Format: int64 */
            size: number;
            /** Format: int64 */
            used: number;
        };
        /**
         * @description 폴더 트리의 종류
         * @enum {string}
         */
        FolderKind: "vm" | "network" | "storage";
        FolderPatch: {
            /** @description 새 이름(그 폴더의 `inventory.folder`) */
            name?: string;
            /**
             * Format: uuid
             * @description 새 부모 폴더 id, `null`은 트리의 루트(그 폴더와 새 부모 쪽의 `inventory.move`)
             */
            parent?: string | null;
            quota?: components["schemas"]["FolderQuota"] | null;
        };
        /** @description 폴더 할당량. 하위 트리 전체의 합에 건다. 칸마다 없으면 그 차원은 상한이 없다. */
        FolderQuota: {
            /**
             * Format: int64
             * @description 하위 트리 VM들의 백업이 차지한다고 보는 바이트(계획 68 §13, VM 폴더만): 저장소 백업은 처음 쓴 바이트, 풀 백업은
             *     파일 크기 합. 지운 VM의 백업은 백업한 때의 VM 폴더에 든다.
             */
            backup_bytes?: number;
            /**
             * Format: int64
             * @description 전용 VM의 vCPU 자리(핫플러그 상한 + 에뮬레이터 `isolate` 1, 계획 64)
             */
            dedicated_vcpus?: number;
            /**
             * Format: int64
             * @description 붙은 볼륨의 가상 크기 합(바이트)
             */
            disk_bytes?: number;
            /**
             * Format: int64
             * @description 유동 IP 수(계획 66, 네트워크 폴더만). 부하 분산기의 바깥 VIP도 하나씩 센다(계획 67).
             */
            floating_ips?: number;
            /**
             * Format: int64
             * @description 부하 분산기 수(계획 67, 네트워크 폴더만)
             */
            load_balancers?: number;
            /** Format: int64 */
            memory_mib?: number;
            /**
             * Format: int64
             * @description 네트워크 수(계획 67, 네트워크 폴더만)
             */
            networks?: number;
            /**
             * Format: int64
             * @description 라우터 수(계획 66, 네트워크 폴더만)
             */
            routers?: number;
            /** Format: int64 */
            vcpus?: number;
            /**
             * Format: int64
             * @description 템플릿 아닌 VM 수
             */
            vms?: number;
        };
        FolderSpec: {
            /** @description 이 폴더가 속한 트리 */
            kind: components["schemas"]["FolderKind"];
            /** @description 화면 이름. 형제 사이에서 대소문자 무시로 겹치지 않는다(상태 머신이 막는다). */
            name: string;
            /**
             * Format: uuid
             * @description 부모 폴더. 없으면 그 종류의 루트 바로 아래다. 부모는 같은 종류여야 한다.
             */
            parent?: string;
            /** @description 하위 트리 할당량. VM 폴더(VM 차원)와 네트워크 폴더(`routers`·`floating_ips`, 계획 66)에만 둔다. 없으면 상한이 없다. */
            quota?: components["schemas"]["FolderQuota"];
        };
        FolderStatus: Record<string, never>;
        /** @description 폴더 하위 트리의 사용량(할당 기준, 전원과 무관). 할당량과 같은 칸이다. */
        FolderUsage: {
            /**
             * Format: int64
             * @description 백업 바이트(계획 68). 0이면 쓰지 않는다.
             */
            backup_bytes?: number;
            /**
             * Format: int64
             * @description 전용 VM의 vCPU 자리(계획 64). 0이면 쓰지 않아 지금 응답 그대로다.
             */
            dedicated_vcpus?: number;
            /** Format: int64 */
            disk_bytes: number;
            /**
             * Format: int64
             * @description 유동 IP 수(계획 66, 부하 분산기의 바깥 VIP 포함). 0이면 쓰지 않는다.
             */
            floating_ips?: number;
            /**
             * Format: int64
             * @description 부하 분산기 수(계획 67). 0이면 쓰지 않는다.
             */
            load_balancers?: number;
            /** Format: int64 */
            memory_mib: number;
            /**
             * Format: int64
             * @description 네트워크 수(계획 67). 0이면 쓰지 않는다.
             */
            networks?: number;
            /**
             * Format: int64
             * @description 라우터 수(계획 66). 0이면 쓰지 않는다.
             */
            routers?: number;
            /** Format: int64 */
            vcpus: number;
            /** Format: int64 */
            vms: number;
        };
        /** @description 폴더의 사용량과 그 폴더를 묶는 할당량 */
        FolderUsageView: {
            /** @description 차원마다 남은 몫이 가장 적은(묶는) 폴더. 상한이 없는 차원은 없다. */
            binding: {
                [key: string]: string;
            };
            /** Format: uuid */
            folder: string;
            /** @description 이 폴더와 조상 가운데 할당량이 있는 폴더(가까운 것부터). 쓰기는 이 모두를 지켜야 한다. */
            limits: components["schemas"]["QuotaLimit"][];
            path: string;
            /** @description 하위 트리 사용량(할당 기준, 전원과 무관). 템플릿은 디스크만 센다. */
            usage: components["schemas"]["FolderUsage"];
        };
        /**
         * @description 라우터 게이트웨이의 지금 모양
         * @enum {string}
         */
        GatewayState: "active" | "no-candidate" | "unknown";
        /** @enum {string} */
        GcPhase: "mark" | "wait" | "sweep";
        /** @description 끝난 GC의 결과 */
        GcResult: {
            error?: string;
            /** Format: int64 */
            finished_at: number;
            /**
             * Format: int64
             * @description 표시되지 않았지만 GC 시작 뒤에 올라와 남긴 청크 수
             */
            kept_young: number;
            /** Format: int64 */
            removed_bytes: number;
            /** Format: int64 */
            removed_chunks: number;
            /** Format: int64 */
            started_at: number;
        };
        /** @description 진행 중인 GC(§10.2) */
        GcRun: {
            /**
             * Format: int64
             * @description GC 표식의 저장소 시각 T0(유닉스 초, 저장소 시계)
             */
            marker_at: number;
            phase: components["schemas"]["GcPhase"];
            /**
             * Format: int64
             * @description 시작한 시각(유닉스 초, 노드 시계)
             */
            started_at: number;
            /** Format: uuid */
            task: string;
        };
        /** @description 권한 하나(`Kind::Grant`): 주체에게 대상에서 역할을 준다. */
        GrantSpec: {
            principal: components["schemas"]["PrincipalRef"];
            /**
             * @description 하위 폴더와 그 안의 객체로 내려가는지. 거짓이면 대상 자체(폴더 이름 바꾸기·지우기·그 폴더의 권한 보기)에만
             *     닿는다. 기본은 참이고 참이면 쓰지 않는다.
             * @default true
             */
            propagate: boolean;
            role: components["schemas"]["RoleRef"];
            target: components["schemas"]["GrantTarget"];
        };
        GrantStatus: Record<string, never>;
        /** @description grant를 건 곳 */
        GrantTarget: {
            /** @enum {string} */
            type: "root";
        } | {
            /** Format: uuid */
            id: string;
            /** @enum {string} */
            type: "folder";
        } | {
            /** Format: uuid */
            id: string;
            kind: components["schemas"]["Kind"];
            /** @enum {string} */
            type: "object";
        };
        /** @description 이 그룹(보통 DN)에 든 사용자는 이 역할 */
        GroupRole: {
            group: string;
            role: components["schemas"]["Role"];
        };
        /** @description 로컬 그룹(`Kind::Group`). 외부 사용자는 그 영역의 `external_groups`에 `external`의 그룹이 있으면 든다. */
        GroupSpec: {
            description?: string;
            /** @description 연결한 디렉터리 그룹 */
            external?: components["schemas"]["ExternalGroup"][];
            /** @description 멤버 사용자 id(로컬·외부) */
            members?: string[];
        };
        GroupStatus: Record<string, never>;
        /** @description NIC의 IP 주소 하나 */
        GuestAddr: {
            /** Format: ip */
            ip: string;
            /** Format: int32 */
            prefix: number;
        };
        /** @description VM의 게스트 에이전트 설정(계획 59). JSON `{}`은 기본값 그대로 켠다는 뜻이고, 기본값인 칸은 쓰지 않는다. */
        GuestAgentSpec: {
            /**
             * @description 디스크 스냅샷·백업·복제 회차의 시점을 찍을 때 게스트 파일 시스템을 얼린다(`guest-fsfreeze-freeze`).
             * @default true
             */
            freeze_fs: boolean;
            /** @description VM을 끌 때 쓰는 방법 */
            shutdown?: components["schemas"]["AgentShutdown"];
        };
        /** @description 게스트의 파일 시스템 하나. 크기는 에이전트가 알려 줄 때만 있다. */
        GuestFs: {
            fstype: string;
            mountpoint: string;
            /** Format: int64 */
            total_bytes?: number;
            /** Format: int64 */
            used_bytes?: number;
        };
        /** @description 게스트 에이전트에서 모은 게스트 정보. 실패한 명령(옛 에이전트, freeze 중)의 칸은 비어 있다. */
        GuestInfo: {
            /** @description 에이전트 버전(`guest-info`) */
            agent_version?: string;
            /**
             * Format: int64
             * @description 모은 시각(유닉스 초)
             */
            collected_at: number;
            /** @description 파일 시스템(`guest-get-fsinfo`) */
            filesystems?: components["schemas"]["GuestFs"][];
            /** @description 호스트 이름(`guest-get-host-name`) */
            hostname?: string;
            /** @description NIC와 주소(`guest-network-get-interfaces`) */
            interfaces?: components["schemas"]["GuestNic"][];
            /** @description 게스트 OS(`guest-get-osinfo`) */
            os?: components["schemas"]["GuestOs"];
        };
        /** @description 게스트 정보 읽기의 응답 */
        GuestInfoResponse: {
            info?: components["schemas"]["GuestInfo"];
            state: components["schemas"]["GuestInfoState"];
        };
        /**
         * @description 게스트 정보 읽기의 결과(`GET /vms/{key}/guest`)
         * @enum {string}
         */
        GuestInfoState: "answered" | "no_agent" | "not_running" | "unreachable" | "unsupported";
        /** @description 게스트의 NIC 하나 */
        GuestNic: {
            addresses?: components["schemas"]["GuestAddr"][];
            /** @description 하드웨어 주소(게스트가 보낸 문자열 그대로) */
            mac?: string;
            name: string;
        };
        /** @description 게스트 NUMA 노드 하나 */
        GuestNode: {
            /** Format: int32 */
            memory_mib: number;
            /** @description 이 노드의 소켓(QEMU `socket-id`, 연속) */
            sockets: number[];
        };
        /** @description 게스트 OS(`guest-get-osinfo`의 `id`·`pretty-name`·`version`·`kernel-release`) */
        GuestOs: {
            /** @description 예: `ubuntu`, `mswindows` */
            id?: string;
            kernel?: string;
            /** @description 예: `Ubuntu 24.04.1 LTS` */
            name?: string;
            version?: string;
        };
        /** @description 게스트 사용자 비밀번호 설정 */
        GuestPasswordRequest: {
            /**
             * Format: password
             * @description 새 비밀번호(평문, 1~256바이트). 저장하지 않는다.
             */
            password: string;
            /** @description 게스트 사용자 이름(1~64바이트, 제어 문자 없음) */
            user: string;
        };
        /** @description 비밀번호를 설정한 사용자 */
        GuestPasswordResponse: {
            user: string;
        };
        /** @description 게스트 에이전트(QGA) 감시: 한 번 답한 에이전트가 `timeout_secs` 동안 답하지 않으면 VM을 강제로 다시 켠다. */
        GuestWatch: {
            /**
             * Format: int32
             * @default 60
             */
            timeout_secs: number;
        };
        /**
         * @description 검사가 grace 넘게 실패했을 때의 대응
         * @enum {string}
         */
        HaCheckAction: "warn" | "migrate" | "restart_elsewhere" | "fence";
        /**
         * @description 검사하는 것의 쓰임
         * @enum {string}
         */
        HaCheckRole: "management" | "storage" | "vm";
        /** @description 검사 하나 */
        HaCheckSpec: {
            /**
             * @description 끄면 노드가 검사하지 않는다.
             * @default true
             */
            enabled: boolean;
            /**
             * Format: int32
             * @description 이만큼(초) 넘게 실패해야 대응한다. 10~600
             * @default 10
             */
            grace_secs: number;
            /** @description 노드 이름 → 그 노드의 대상(종류는 `target`과 같아야 한다) */
            nodes?: {
                [key: string]: components["schemas"]["HaCheckTarget"];
            };
            /**
             * @description 실패했을 때의 대응. 없으면 역할의 기본(`storage` → `restart_elsewhere`, `vm` → `migrate`,
             *     `management` → `warn`)
             */
            on_fail?: components["schemas"]["HaCheckAction"];
            /** @description 검사하는 네트워크·장치의 쓰임. 기본 대응과 영향받는 VM을 정한다. */
            role: components["schemas"]["HaCheckRole"];
            /** @description 모든 노드의 검사 대상(노드별 덮기가 없을 때) */
            target: components["schemas"]["HaCheckTarget"];
        };
        HaCheckStatus: Record<string, never>;
        /** @description 검사 대상 */
        HaCheckTarget: {
            iface: string;
            /** @enum {string} */
            kind: "link";
        } | {
            /** @description 1~8개 */
            addresses: string[];
            /** @enum {string} */
            kind: "ping";
            /**
             * Format: int32
             * @description 1~주소 수
             * @default 1
             */
            need: number;
            /** @description 이 인터페이스로 보낸다(Linux만). */
            via?: string;
        } | {
            /** @enum {string} */
            kind: "pool";
            name: string;
        };
        /** @description 리더가 노드에 VM을 끄라고 한 요청(HA 검사 `restart_elsewhere`) */
        HaEvict: {
            /**
             * Format: int64
             * @description 요청한 때(유닉스 초)
             */
            at: number;
            /** @description 실패한 검사의 이름 */
            check: string;
            /**
             * Format: uuid
             * @description VM을 끌 노드
             */
            from: string;
            reason: string;
        };
        /** @description HA 그룹의 노드 하나 */
        HaGroupNode: {
            /** @description 노드 이름 */
            node: string;
            /**
             * Format: int32
             * @description 0~1000, 클수록 먼저
             */
            priority?: number;
        };
        /** @description HA 그룹: HA VM을 복구할 노드와 그 우선순위. VM은 `ha.group`으로 그룹을 쓴다. */
        HaGroupSpec: {
            /** @description 그룹 노드. 우선순위가 큰 노드에 먼저 놓는다. */
            nodes: components["schemas"]["HaGroupNode"][];
            /** @description 켜면 우선순위가 더 높은 노드가 돌아와도 되돌아가지 않는다. */
            nofailback?: boolean;
            /** @description 켜면 그룹 밖 노드에는 놓지 않는다(놓을 곳이 없으면 켜지 않는다). */
            restricted?: boolean;
        };
        HaGroupStatus: Record<string, never>;
        /** @description HA 용량 예약: HA 가능 노드의 메모리 가운데 복구 자리로 비워 두는 몫. */
        HaReserve: {
            /**
             * Format: int32
             * @description 메모리가 가장 큰 노드 N대(1~8)만큼
             */
            nodes: number;
        } | {
            /**
             * Format: int32
             * @description 전체 메모리의 P%(1~90)
             */
            percent: number;
        };
        /**
         * @description 예약을 지운다.
         * @enum {string}
         */
        HaReserveOff: "none";
        /** @description HA 용량 예약 바꾸기: `"none"`이나 예약 */
        HaReservePatch: components["schemas"]["HaReserveOff"] | components["schemas"]["HaReserve"];
        /** @description API 문서의 `ha` 모양: 옛 bool이나 정책 객체. 읽고 쓰기는 `VmSpec.ha`의 손으로 쓴 serde가 한다. */
        HaSetting: boolean | components["schemas"]["VmHa"];
        /**
         * @description HA가 VM을 어떻게 다루는지.
         * @enum {string}
         */
        HaState: "started" | "stopped" | "ignored";
        /**
         * @description 헬스 체크 결과. 200과 503이 같은 모양이다. `status`·`failed` 밖의 필드는 로그인(세션·토큰)한 요청에만
         *     있다.
         */
        Health: {
            /** @description `api` 역할 노드인지 */
            api?: boolean | null;
            /**
             * Format: int64
             * @description 이 노드 로그의 엔트리를 적용하지 못한 채 지난 시간(밀리초)
             */
            apply_lag_ms?: number | null;
            /** @description 실패한 확인. `?live`면 언제나 비어 있다. */
            failed: components["schemas"]["HealthCheck"][];
            /**
             * Format: uuid
             * @description 이 노드가 아는 리더(모르면 없다)
             */
            leader?: string | null;
            lease?: components["schemas"]["LeaseHealth"] | null;
            maintenance?: boolean | null;
            /**
             * Format: uuid
             * @description 이 노드의 id
             */
            node?: string | null;
            /** @description 리더를 알고, 이 노드가 리더면 과반수가 최근 답했다. */
            quorum?: boolean | null;
            runtimes?: components["schemas"]["RuntimeHealth"] | null;
            status: components["schemas"]["HealthStatus"];
        };
        /**
         * @description 실패할 수 있는 확인 하나
         * @enum {string}
         */
        HealthCheck: "quorum" | "apply_lag" | "lease" | "runtimes" | "maintenance" | "role";
        /** @enum {string} */
        HealthStatus: "ok" | "unavailable";
        /** @description 기록 조회의 답. 열은 웹 uPlot이 그대로 받는 배열이고, 점 `i`의 시각은 `start + i × step_ms`(칸 시작)다. */
        HistoryReply: {
            /** @description 칸 이름([`MetricField::as_str`]) → 열 */
            columns: {
                [key: string]: components["schemas"]["Cf"];
            };
            /**
             * Format: int64
             * @description 기록이 범위 시작보다 늦게 시작하면 실제로 아는 가장 이른 시각(재시작 직후·마이그레이션 직후)
             */
            covered_from?: number;
            /** @description 칸 이름 → p50·p95 */
            pct?: {
                [key: string]: components["schemas"]["Pct"];
            };
            /**
             * Format: int64
             * @description 첫 점의 시각(유닉스 ms)
             */
            start: number;
            /** @description 칸 이름 → 범위 요약 */
            stats?: {
                [key: string]: components["schemas"]["Stat"];
            };
            /**
             * Format: int64
             * @description 점 하나의 길이(층 간격 × 묶은 칸 수)
             */
            step_ms: number;
            tier: components["schemas"]["Tier"];
            /** @description `Digest`의 범위 평균 CPU 상위 VM(내림차순) */
            top?: components["schemas"]["TopVm"][];
        };
        /** @description 호스트 NUMA 노드 하나 */
        HostNode: {
            /**
             * @description 이 노드의 온라인 CPU
             * @default
             */
            cpus: components["schemas"]["CpuSet"];
            hugepages?: components["schemas"]["HugepagePool"][];
            /**
             * Format: int32
             * @default 0
             */
            id: number;
            /**
             * Format: int64
             * @description 노드 메모리 전체(hugepages 풀 포함)
             * @default 0
             */
            memory_mib: number;
        };
        /** @description 호스트 NUMA 노드 하나의 사용 */
        HostNodeUse: {
            cpus: components["schemas"]["CpuSet"];
            /** @description 이 호스트 노드의 전용 CPU */
            dedicated: components["schemas"]["CpuSet"];
            /** @description 그 가운데 잡힌 것 */
            dedicated_used: components["schemas"]["CpuSet"];
            hugepages: components["schemas"]["PagesUse"][];
            /** Format: int32 */
            id: number;
            /** Format: int64 */
            memory_mib: number;
        };
        /** @description 켠 노드에만 맞는 배치(`RunningHardware.host_placement`). 마이그레이션·HA는 대상에서 새로 고른다. */
        HostPlacement: {
            /** @description 에뮬레이터 스레드의 pCPU(`isolate`). 비면 노드의 공유 CPU(그때의 집합)다. */
            emulator?: components["schemas"]["CpuSet"];
            /** @description 게스트 노드마다 호스트 NUMA 노드 id. 비면 묶지 않는다. */
            host_nodes?: number[];
            /**
             * Format: uuid
             * @description 이 배치를 고른 노드. 지금 노드와 다르면 낡은 기록이다.
             */
            node: string;
            /** @description 스레드 정책 `isolate`가 비워 둔 형제 스레드(이 VM 몫으로 센다) */
            siblings?: components["schemas"]["CpuSet"];
            /** @description vCPU 자리(핫플러그 상한까지)마다 pCPU. 공유 VM은 비고 vCPU가 게스트 노드의 호스트 노드 공유 CPU에서 돈다. */
            vcpus?: number[];
        };
        /** @description 노드의 CPU 나누기와 hugepages 예약(`NodeSpec.host_resources`, 설계 §7). 공유 CPU는 `온라인 − 예약 − 전용`이다. */
        HostResources: {
            /** @description 전용 VM의 vCPU와 `isolate` 에뮬레이터만 쓰는 CPU */
            dedicated_cpus?: components["schemas"]["CpuSet"];
            /** @description 실행 중 hugepages 예약(opld가 `nr_hugepages`에 쓴다) */
            hugepages?: components["schemas"]["HugepageReserve"][];
            /** @description 호스트와 Orbivisor의 CPU. VM 스레드는 두지 않는다. */
            reserved_cpus?: components["schemas"]["CpuSet"];
        };
        /**
         * @description 핫플러그 배치(계획 54): 켤 때 둔 vCPU 자리·virtio-mem·빈 루트 포트와 지금 그 안에서 바뀐 것. 마이그레이션 대상은
         *     같은 배치로 켠다.
         */
        HotplugLayout: {
            /**
             * Format: int32
             * @description 켤 때 단 디스크 수. 이 뒤의 디스크는 실행 중 붙인 것이다.
             */
            boot_disks: number;
            /**
             * Format: int32
             * @description 켤 때 단 NIC 수. 이 뒤의 NIC는 실행 중 붙인 것이다.
             */
            boot_nics: number;
            /** @description 실행 중 붙인 디스크(순번 `boot_disks`부터)마다 그 루트 포트 */
            disk_ports?: number[];
            /**
             * Format: int32
             * @description 메모리 상한(MiB). 켤 때 `memory_mib`와 같으면 virtio-mem 장치가 없다.
             */
            max_memory_mib: number;
            /** @description 실행 중 붙인 NIC(순번 `boot_nics`부터)마다 그 루트 포트 */
            nic_ports?: number[];
            /**
             * Format: int32
             * @description virtio-mem에 꽂은 크기(`requested-size`, MiB)
             */
            plugged_mib: number;
            /**
             * Format: int32
             * @description 빈 루트 포트 수(`hp0`부터)
             */
            ports: number;
            /**
             * Format: int32
             * @description 지금 켜진 vCPU 수(켤 때 수 이상, 상한 이하)
             */
            vcpus: number;
        };
        /** @description 호스트 노드의 hugepages 풀 하나 */
        HugepagePool: {
            /** Format: int32 */
            size_kib: number;
            /**
             * Format: int32
             * @description 예약된 페이지 수(`nr_hugepages`)
             */
            total: number;
        };
        /** @description hugepages 예약 하나 */
        HugepageReserve: {
            /**
             * Format: int32
             * @description 페이지 수(전체가 이 수가 되게)
             */
            count: number;
            /**
             * Format: int32
             * @description 호스트 NUMA 노드. 없으면 커널이 노드에 나눈다.
             */
            host_node?: number;
            size: components["schemas"]["PageKind"];
        };
        /**
         * Format: binary
         * @description 업로드 본문의 문서용 스키마: 바이너리 파일
         */
        ImageFile: string;
        /** @enum {string} */
        ImageFormat: "qcow2" | "raw";
        /** @description 불균형(0.1% 단위): 노드 CPU·메모리 사용률의 표준편차와 그 평균 */
        ImbalanceView: {
            /** Format: int32 */
            cpu_pct: number;
            /** Format: int32 */
            mem_pct: number;
            /** Format: int32 */
            total_pct: number;
        };
        /** @enum {string} */
        ImportPhase: "ready" | "importing" | "done" | "failed";
        /** @description Orbivisor VM 설정 제안. 실행할 때 [`ImportRun`]에 없는 값의 기본값이다. */
        ImportProposal: {
            /** Format: int32 */
            cores: number;
            disk_bus: components["schemas"]["DiskBus"];
            firmware: components["schemas"]["Firmware"];
            /** Format: int32 */
            memory_mib: number;
            /** @description 원본 네트워크 → 이름이 같은 Orbivisor 네트워크(없으면 null, 실행 때 정해야 한다) */
            networks: {
                [key: string]: string | null;
            };
            nic_model: components["schemas"]["NicModel"];
            /** Format: int32 */
            sockets: number;
        };
        /** @description 실행 요청(`POST /vm-imports/{key}/run`). 비운 값은 제안 값을 쓴다. */
        ImportRun: {
            /** Format: int32 */
            cores?: number | null;
            /** @description 에뮬레이터 스레드 정책(계획 64) */
            cpu_emulator?: components["schemas"]["EmulatorPolicy"];
            /** @description CPU 플래그(`["-avx512vbmi"]`, 계획 63) */
            cpu_flags?: components["schemas"]["CpuFlag"][];
            /** @description CPU 모델(계획 63). 없으면 `host`다. API가 별칭을 버전 모델로 고정해 넣는다. */
            cpu_model?: components["schemas"]["CpuModel"];
            /** @description CPU 정책(계획 64). 없으면 `shared`다. */
            cpu_policy?: components["schemas"]["CpuPolicy"];
            /** @description 전용 CPU 스레드 정책(계획 64) */
            cpu_thread_policy?: components["schemas"]["ThreadPolicy"];
            disk_bus?: components["schemas"]["DiskBus"] | null;
            firmware?: components["schemas"]["Firmware"] | null;
            /**
             * Format: uuid
             * @description 만든 VM을 둘 폴더(계획 62). 없으면 VM 트리의 루트이고 쓰지 않아 옛 노드도 푼다.
             */
            folder?: string;
            /** @description hugepages(계획 64) */
            hugepages?: components["schemas"]["PageSize"];
            /** @description 원본 NIC 순번별 SDN 주소(계획 65, 주소 체계마다 하나까지). 빈 목록은 서브넷에서 받는다. */
            ips?: string[][];
            /**
             * @description 원본 MAC 주소를 그대로 쓴다. 끄면 새 MAC을 받는다.
             * @default true
             */
            keep_macs: boolean;
            /** Format: int32 */
            memory_mib?: number | null;
            /** @description 만들 VM 이름 */
            name: string;
            /** @description 원본 네트워크 → Orbivisor 네트워크. 원본 NIC의 네트워크마다 있어야 한다. */
            networks?: {
                [key: string]: string;
            };
            nic_model?: components["schemas"]["NicModel"] | null;
            /** @description vNUMA(계획 64) */
            numa?: components["schemas"]["NumaSpec"];
            /** @description 디스크 볼륨을 만들 풀 */
            pool: string;
            /** Format: int32 */
            sockets?: number | null;
        };
        /** @description 원본. JSON은 `{"ova": {...}}` 또는 `{"esxi": {...}}`다. */
        ImportSource: {
            /** @description 올린 OVA 파일 */
            ova: components["schemas"]["OvaSource"];
        } | {
            /** @description ESXi·vCenter 데이터스토어의 VM. 비밀번호는 여기에 없다(받은 노드의 메모리에만 둔다). */
            esxi: components["schemas"]["EsxiSource"];
        };
        /**
         * @description 인터페이스 종류(`ip -d`의 `info_kind`, 없으면 이더넷)
         * @enum {string}
         */
        InterfaceKind: "ethernet" | "bond" | "bridge" | "vlan" | "dummy" | "veth" | "other";
        /** @description NIC(또는 라우터 외부·유동 IP) 하나의 주소 */
        IpAllocation: {
            ips: string[];
            /** @description 없으면 `nic` */
            kind?: components["schemas"]["AllocationKind"];
            live?: components["schemas"]["PortLive"] | null;
            mac?: string;
            /** @description NIC 순번(0부터) */
            nic: number;
            /** @description 유동 IP가 붙은 VM의 이름(부른 사용자가 읽을 수 있을 때만) */
            target?: string;
            /** Format: uuid */
            vm: string;
            vm_name: string;
        };
        /** @description 주소를 얻는 방법 */
        IpConfig: {
            /** @enum {string} */
            mode: "dhcp";
        } | {
            /** @enum {string} */
            mode: "slaac";
        } | {
            /** @description `주소/접두 길이`(예: `10.0.0.5/24`) */
            address: string;
            gateway?: string;
            /** @enum {string} */
            mode: "static";
        };
        /** @description 주소 범위(양 끝 포함) */
        IpRange: {
            /** Format: ip */
            end: string;
            /** Format: ip */
            start: string;
        };
        /**
         * @description `ipmitool [-I lanplus] -H address -p port -U user -E chassis power off|status`. 비밀번호는 명령 인자가 아니라
         *     `IPMI_PASSWORD` 환경 변수로 넘긴다.
         */
        IpmiFence: {
            /** @description BMC 주소(호스트 이름이나 IP) */
            address: string;
            /**
             * @description IPMI 2.0(`-I lanplus`)
             * @default true
             */
            lanplus: boolean;
            /** @description BMC 비밀번호. API는 내보내지 않는다. */
            password?: string;
            /**
             * Format: int32
             * @default 623
             */
            port: number;
            user: string;
        };
        /** @description [`IpmiFence`]에서 `password`를 뺀 것 */
        IpmiFenceView: {
            address: string;
            /** @description 비밀번호가 저장되어 있는지 */
            has_password: boolean;
            lanplus: boolean;
            /** Format: int32 */
            port: number;
            user: string;
        };
        /**
         * @description IPv6 서브넷의 주소 알리기(계획 66 §10). 라우터 인터페이스인 네트워크에서는 라우터 포트가 RA로 알린다.
         * @enum {string}
         */
        Ipv6Mode: "slaac" | "dhcpv6-stateful" | "dhcpv6-stateless";
        /** @description 노드가 로그인할 iSCSI 타깃 */
        IscsiTarget: {
            /** @description `호스트[:포트]` 또는 `[IPv6][:포트]` */
            portal: string;
            /** @description 타깃 이름(`iqn.`·`eui.`·`naa.`로 시작) */
            target: string;
        };
        JobError: {
            error: string;
            /** @description VM 이름 */
            vm: string;
        };
        /** @description 진행 중인 실행 */
        JobRun: {
            /** @description 이번 실행에서 만든 백업 id */
            backups?: string[];
            /** @description 아직 백업을 시작하지 않은 VM id */
            queue?: string[];
            /**
             * Format: int64
             * @description 시작한 시각(유닉스 초)
             */
            started_at: number;
        };
        /** @description 끝난 실행의 결과 */
        JobRunResult: {
            /**
             * Format: int32
             * @description 실패한 백업과 백업하지 못한 VM 수
             */
            failed: number;
            /** Format: int64 */
            finished_at: number;
            /**
             * Format: int32
             * @description 성공한 백업 수
             */
            ok: number;
            /** Format: int64 */
            started_at: number;
        };
        /** @description 실행 일정: `days`의 `at`(UTC). `days`가 비면 매일. */
        JobSchedule: {
            /**
             * @description `HH:MM`(UTC)
             * @example 03:00
             */
            at: string;
            days?: components["schemas"]["Weekday"][];
        };
        JoinRequest: {
            /** @description 새 노드의 API 주소(`host:port`) */
            address: string;
            /** @description 새 노드의 노드 간 통신 주소(`host:port`, 첫 링크) */
            cluster_address: string;
            /**
             * @description 새 노드의 모든 노드 간 통신 링크(`host:port`, 앞이 우선, 첫 것이 `cluster_address`). 링크가 둘 이상일
             *     때만 보낸다(옛 노드는 이 필드를 모른다).
             */
            cluster_addresses?: string[];
            /** @description 새 노드가 만든 키의 CSR(PEM) */
            csr_pem: string;
            /** @description 새 노드의 이름 */
            name: string;
            /**
             * Format: uuid
             * @description 새 노드의 id(Raft 노드 id, Node 리소스 id)
             */
            node: string;
            /** @description 조인 URL의 토큰 */
            token: string;
            /** @description 새 노드의 orbivisord 빌드 버전. 옛 버전 노드는 보내지 않는다. */
            version?: string | null;
            /** @description 새 노드가 위트니스(투표만 하고 VM을 돌리지 않는 노드)인지 */
            witness?: boolean;
        };
        JoinResponse: {
            ca_pem: string;
            /** @description 새 노드의 인증서(PEM) */
            cert_pem: string;
            cluster: string;
            /**
             * @description 투표 멤버의 노드 간 통신 주소(이 노드가 투표 멤버면 이 노드, 아니면 리더나 다른 투표 멤버). 새 노드는
             *     여기로 승격을 요청한다.
             */
            via: string;
        };
        JoinTokenRequest: {
            /**
             * Format: int64
             * @description 유효 시간(초). 기본 1시간, 최대 7일
             */
            ttl_secs?: number | null;
        };
        /** @description 보존 규칙: 최신 `last`개, 그다음 날(UTC)·ISO 주·달마다 가장 새 것 하나씩 `daily`·`weekly`·`monthly`개. */
        Keep: {
            /**
             * Format: int32
             * @default 0
             */
            daily: number;
            /**
             * Format: int32
             * @default 0
             */
            last: number;
            /**
             * Format: int32
             * @default 0
             */
            monthly: number;
            /**
             * Format: int32
             * @default 0
             */
            weekly: number;
        };
        /**
         * @description 클러스터 저장소가 구분하는 리소스 종류.
         * @enum {string}
         */
        Kind: "vm" | "storage-pool" | "volume" | "node" | "task" | "user" | "api-token" | "cluster" | "join-token" | "network" | "snapshot" | "security-group" | "backup" | "backup-job" | "vm-import" | "settings" | "realm" | "remote" | "session" | "vip-holder" | "ha-group" | "placement-rule" | "node-fence" | "ha-check" | "ceph" | "alert-rule" | "notification-target" | "folder" | "grant" | "access-role" | "group" | "two-factor" | "cpu-catalog" | "sdn" | "router" | "floating-ip" | "load-balancer" | "network-migration" | "backup-store";
        /** @description 바깥 VIP */
        LbExternal: {
            /**
             * Format: ipv4
             * @description 바깥 VIP(IPv4). API가 그 네트워크의 유동 IP 풀(없으면 풀)에서 채운다.
             */
            ip?: string;
            /** @description 공급자 네트워크 이름(VIP 네트워크 라우터의 바깥) */
            network: string;
        };
        /** @description 건강 검사(OVN 서비스 모니터 — TCP SYN·UDP) */
        LbHealth: {
            /**
             * Format: int32
             * @description 이만큼 이어 실패하면 뺀다(1~10).
             * @default 3
             */
            fall: number;
            /**
             * Format: int32
             * @description 검사 간격(초, 1~300)
             * @default 5
             */
            interval_secs: number;
            /**
             * Format: int32
             * @description 이만큼 이어 성공하면 다시 넣는다(1~10).
             * @default 2
             */
            rise: number;
            /**
             * Format: int32
             * @description 답을 기다리는 시간(초, 1~간격)
             * @default 3
             */
            timeout_secs: number;
        };
        /** @description 리스너 하나: 프로토콜·포트와 멤버 */
        LbListener: {
            /** @description 건강 검사. 없으면 멤버를 늘 쓴다. */
            health?: components["schemas"]["LbHealth"];
            /** @description 멤버(0~64개). 없으면 연결을 거절한다(TCP RST·ICMP). */
            members?: components["schemas"]["LbMember"][];
            /**
             * Format: int32
             * @description VIP 포트(1~65535)
             */
            port: number;
            protocol: components["schemas"]["LbProtocol"];
        };
        /** @description 멤버 하나: VM NIC(`vm`·`mac` — 주소는 그 NIC의 IPv4) 또는 주소(`ip` — 건강 검사 없음) */
        LbMember: {
            /** Format: ipv4 */
            ip?: string;
            mac?: string;
            /**
             * Format: int32
             * @description 멤버 포트(1~65535)
             */
            port: number;
            /** Format: uuid */
            vm?: string;
        };
        /** @description 멤버 하나의 보기 */
        LbMemberView: {
            health: components["schemas"]["MemberHealth"];
            /**
             * Format: ipv4
             * @description 멤버 주소(NIC 멤버는 그 NIC의 IPv4, 찾지 못하면 없음)
             */
            ip?: string;
            /** @description 리스너 `tcp/80` */
            listener: string;
            mac?: string;
            /** Format: int32 */
            port: number;
            /** @description `missing`의 까닭 */
            reason?: string;
            /** Format: uuid */
            vm?: string;
            /** @description VM 이름(부른 사용자가 읽을 수 있을 때만) */
            vm_name?: string;
            /** @description 보안 그룹이 클라이언트·건강 검사를 막는다는 경고 */
            warning?: string;
        };
        /** @description 건강 검사 출발 주소 하나(그 네트워크 서브넷 안 — Raft `ip`로 다른 NIC·라우터·유동 IP와 겹치지 않음) */
        LbMonitor: {
            /** Format: ipv4 */
            ip: string;
            /** @description 멤버 네트워크 이름 */
            network: string;
        };
        /** @enum {string} */
        LbProtocol: "tcp" | "udp";
        /** @description VIP·바깥 VIP의 지금 모양(Raft에서 셈) */
        LbState: {
            /** @enum {string} */
            state: "ready";
        } | {
            reason: string;
            /** @enum {string} */
            state: "unrouted";
        };
        /** @description LDAP 디렉터리. `ldap://`는 StartTLS를 반드시 쓰고, `ldaps://`는 처음부터 TLS다. */
        LdapRealm: {
            /** @description 사용자를 찾을 기준 DN(subtree) */
            base_dn: string;
            /** @description 사용자를 찾을 때 바인드할 서비스 계정. 없으면 익명으로 찾는다. */
            bind_dn?: string;
            /** @description 서비스 계정 비밀번호. API는 내보내지 않는다. */
            bind_password?: string;
            /** @description 서버 인증서를 확인할 CA(PEM). 없으면 시스템 루트 */
            ca_pem?: string;
            /** @description 그룹 DN을 담은 사용자 항목의 속성 */
            group_attr?: string;
            /**
             * Format: int64
             * @description 연결과 작업마다의 시간 제한(초)
             */
            timeout_secs?: number;
            /** @description `ldap://host[:port]` 또는 `ldaps://host[:port]` */
            url: string;
            /** @description 사용자 찾기 필터. `{username}`이 이스케이프된 로그인 이름으로 바뀐다. */
            user_filter?: string;
        };
        /** @description [`LdapRealm`]에서 `bind_password`를 뺀 것 */
        LdapRealmView: {
            base_dn: string;
            bind_dn?: string;
            ca_pem?: string;
            group_attr: string;
            /** @description 서비스 계정 비밀번호가 저장되어 있는지 */
            has_bind_password: boolean;
            /** Format: int64 */
            timeout_secs: number;
            url: string;
            user_filter: string;
        };
        /**
         * @description 이 노드의 HA 리스
         * @enum {string}
         */
        LeaseHealth: "fresh" | "expired" | "none";
        /**
         * @description 구독 수준
         * @enum {string}
         */
        Level: "standard" | "premium";
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_AccessRoleView: {
            items: {
                /** @description 내장 역할(`admin`·`operator`·`viewer`)이면 참이고 바꿀 수 없다. */
                builtin: boolean;
                description: string;
                /**
                 * Format: uuid
                 * @description 사용자 정의 역할의 id. 내장 역할은 없다.
                 */
                id?: string;
                name: string;
                /** @description 권한 이름 목록(`vm.power` 등) */
                privileges: string[];
                /**
                 * Format: int64
                 * @description 사용자 정의 역할의 revision(내장 역할은 0)
                 */
                revision: number;
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_CatalogEntry: {
            items: (components["schemas"]["CatalogImage"] & {
                source: components["schemas"]["CatalogSource"];
                /** @description 이 이미지 URL에서 받아 준비된 볼륨 이름(끝난 `volume-download` Task의 결과, 이름 순) */
                volumes: string[];
            })[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_NotificationTargetView: {
            items: {
                enabled: boolean;
                /** Format: uuid */
                id: string;
                name: string;
                /** Format: int64 */
                revision: number;
                target: components["schemas"]["TargetConfigView"];
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_RealmView: {
            items: {
                backend: components["schemas"]["RealmBackendView"];
                display?: string;
                /** Format: uuid */
                id: string;
                name: string;
                /** Format: int64 */
                revision: number;
                roles: components["schemas"]["RoleMapping"];
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_RemoteView: {
            items: {
                ca_fingerprint: string;
                /** Format: uuid */
                id: string;
                name: string;
                /** Format: int64 */
                revision: number;
                status: components["schemas"]["RemoteStatus"];
                /**
                 * Format: uuid
                 * @description 저장한 API 토큰의 id
                 */
                token_id: string | null;
                url: string;
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_AlertRuleSpec_AlertRuleStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                /** @description 경보 규칙 하나 */
                spec: {
                    condition: components["schemas"]["AlertCondition"];
                    /**
                     * @description 끄면 평가하지 않고, 활성 경보는 알림 없이 해제한다.
                     * @default true
                     */
                    enabled: boolean;
                    /**
                     * Format: int32
                     * @description 조건이 이만큼(초) 참이어야 알린다(0~86400). 이벤트 조건은 0이다.
                     * @default 0
                     */
                    for_secs: number;
                    /** @description 이 노드(이름)들만. 비면 모든 노드 */
                    nodes?: string[];
                    /**
                     * Format: int32
                     * @description 경보가 이어지는 동안 이만큼(초)마다 다시 알린다. 0이면 다시 보내지 않는다.
                     * @default 14400
                     */
                    repeat_secs: number;
                    /**
                     * @description 해제도 알린다(이벤트 조건은 해제가 없다).
                     * @default true
                     */
                    send_resolved: boolean;
                    severity: components["schemas"]["Severity"];
                    /** @description 알릴 `NotificationTarget` 이름(1~16개) */
                    targets: string[];
                    /** @description 이 VM(이름)들만(VM 조건만). 비면 모든 VM */
                    vms?: string[];
                };
                /** @description 리더가 평가하는 규칙(`NodeOffline`·`BackupJobFailed`)의 활성 경보만 둔다. 노드 경보는 각 노드 파일에 있다. */
                status: {
                    active?: components["schemas"]["ActiveAlert"][];
                    /**
                     * @description `BackupJobFailed`: 백업 작업(id)마다 이미 알린 마지막 실패 실행의 끝난 시각(ms). 리더가 바뀌어도 같은 실행을 다시
                     *     알리지 않는다.
                     */
                    notified?: {
                        [key: string]: number;
                    };
                };
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_BackupJobSpec_BackupJobStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                spec: {
                    /** @description 작업의 백업이 `ready`가 되면 다른 저장소로 복사본을 보낸다(계획 68 §12.5 — 계층). 없으면 쓰지 않는다. */
                    copy_to?: components["schemas"]["CopyTarget"];
                    /**
                     * @description 끄면 예약 실행을 하지 않는다.
                     * @default true
                     */
                    enabled: boolean;
                    /**
                     * Format: int32
                     * @description 0보다 크면 증분으로 백업하되(할 수 있으면) 체인의 증분이 이만큼 쌓이면 다음은 전체로 한다.
                     *     0이면 늘 전체다(뺀다).
                     *     저장소 작업(`store`)에서는 "n번 연속 비트맵으로 읽었으면 다음은 디스크 전체를 읽기"다(계획 68 §8.3 — 저장 공간은
                     *     늘지 않는다).
                     */
                    full_every?: number;
                    /** @description 보존 규칙. 모두 0이면 지우지 않는다. */
                    keep?: components["schemas"]["Keep"];
                    /**
                     * @description 백업 풀 이름. 백업 저장소(`store`, 계획 68)로 백업하는 작업이면 비우고 쓰지 않는다(옛 JSON에는 늘 있으므로
                     *     바이트 그대로다). `pool`과 `store` 중 정확히 하나다.
                     */
                    pool?: string;
                    /** @description 만든 백업을 보낼 원격 클러스터의 백업 풀. 없으면 보내지 않는다. */
                    replicate?: components["schemas"]["ReplicateTarget"];
                    schedule: components["schemas"]["JobSchedule"];
                    /** @description 백업 저장소 이름(계획 68). 없으면 쓰지 않는다. */
                    store?: string;
                    /** @description 백업할 VM */
                    vms: components["schemas"]["VmSelector"];
                };
                status: {
                    /**
                     * @description 마지막(또는 진행 중인) 실행에서 백업하지 못한 VM과 이유
                     * @default []
                     */
                    errors: components["schemas"]["JobError"][];
                    /** @default null */
                    last_run: components["schemas"]["JobRunResult"] | null;
                    /**
                     * Format: int64
                     * @description 다음 예약 실행 시각(유닉스 초)
                     * @default null
                     */
                    next_run: number | null;
                    /** @default null */
                    run: components["schemas"]["JobRun"] | null;
                };
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_BackupSpec_BackupStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                spec: {
                    /**
                     * @description 할 수 있으면 증분으로 백업한다(바로 앞 백업을 부모로 바뀐 블록만). 할 수 없으면 전체로 하고 그
                     *     이유를 `status.full_reason`에 적는다. false면 뺀다.
                     */
                    incremental?: boolean;
                    /**
                     * Format: uuid
                     * @description 이 백업을 만든 백업 작업 id. 손으로 만든 백업이면 없다.
                     */
                    job?: string | null;
                    /**
                     * Format: int32
                     * @description 증분 체인에 둘 증분 백업 수의 한도(백업 작업의 `full_every`). 부모의 체인에 이미 이만큼 증분이
                     *     있으면 전체로 한다. 없으면 한도가 없다(손으로 만든 백업).
                     */
                    max_chain?: number;
                    /** @description 다른 클러스터에서 받은 사본이면 그 원본. 이 클러스터에서 만든 백업이면 없다. */
                    origin?: components["schemas"]["BackupOrigin"];
                    /**
                     * @description 백업 풀 이름. 저장소 백업(`store`, 계획 68)이면 비우고 쓰지 않는다(옛 JSON에는 늘 있으므로 바이트 그대로다).
                     *     `pool`과 `store` 중 정확히 하나다([`BackupSpec::target`]).
                     */
                    pool?: string;
                    /**
                     * @description 옛 백업을 옮기거나(`convert`) 다른 저장소에서 복사해(`copy`) 만든 저장소 백업의 원본(계획 68 §12.2·§12.5). 없으면
                     *     쓰지 않는다.
                     */
                    source?: components["schemas"]["BackupSource"];
                    /** @description 백업 저장소 이름(계획 68). 없으면 쓰지 않는다. */
                    store?: string;
                    /**
                     * Format: uuid
                     * @description VM id. VM을 지워도 백업은 남는다.
                     */
                    vm: string;
                    /** @description 만든 때의 VM 이름(표시용) */
                    vm_name: string;
                    /** @description 만든 때의 VM 스펙 */
                    vm_spec: components["schemas"]["VmSpec"];
                };
                status: {
                    /**
                     * Format: uuid
                     * @description 저장소 백업(계획 68)에서 바뀌지 않은 청크를 가져온 앞 백업. 계획 40의 `parent`와 달리 복원은 이것을 따라가지
                     *     않는다(인덱스가 늘 완전하다). 없으면 쓰지 않는다.
                     */
                    base?: string;
                    /**
                     * Format: int64
                     * @description 만든 시각(유닉스 초)
                     * @default null
                     */
                    created_at: number | null;
                    /** @default [] */
                    disks: components["schemas"]["BackupDisk"][];
                    /**
                     * @description 실패한 이유
                     * @default null
                     */
                    error: string | null;
                    /** @description 증분을 요청했지만 전체로 한 이유. 없으면 뺀다. */
                    full_reason?: string;
                    /**
                     * Format: uuid
                     * @description 파일을 쓴 노드. 공유가 아닌 풀의 백업은 이 노드에만 있으므로 지우기·복원도 이 노드에서 한다.
                     *     공유 풀이면 없다.
                     */
                    node?: string;
                    /**
                     * Format: uuid
                     * @description 증분 백업의 부모(같은 풀의 바로 앞 백업). 전체 백업이면 뺀다.
                     */
                    parent?: string;
                    /** @default pending */
                    phase: components["schemas"]["BackupPhase"];
                    /**
                     * Format: int64
                     * @description 받는 중인 사본이 마지막으로 파일을 받은 시각(유닉스 초). 받은 적이 없으면 뺀다.
                     */
                    received_at?: number;
                    /** @description 원격 클러스터에 보낸 사본. 없으면 뺀다. */
                    replicas?: components["schemas"]["BackupReplica"][];
                    /** @description 저장소 백업이 읽고 쓴 양(계획 68). 없으면 쓰지 않는다. */
                    store_stats?: components["schemas"]["StoreBackupStats"];
                    /** @description TPM 상태 파일(`tpm2-00.permall`)을 담았는지(계획 59). 거짓이면 뺀다. */
                    tpm_state?: boolean;
                    /**
                     * @description UEFI 변수 파일(`vars.fd`)을 담았는지
                     * @default false
                     */
                    uefi_vars: boolean;
                    /** @description 마지막 검증 결과(계획 68). 검증한 적이 없으면 쓰지 않는다. */
                    verify?: components["schemas"]["BackupVerify"];
                };
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_BackupStoreSpecView_BackupStoreStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                /** @description 비밀(S3 비밀 키·주 키)을 뺀 저장소 스펙 */
                spec: {
                    backend: components["schemas"]["StoreBackendView"];
                    encryption?: components["schemas"]["StoreEncryptionView"];
                    gc: components["schemas"]["JobSchedule"];
                    /** Format: int32 */
                    max_streams: number;
                    read_only?: boolean;
                    verify?: components["schemas"]["VerifyPolicy"];
                };
                /** @description 저장소의 실제 상태. 초기화·GC·검증 Task와 리더의 스케줄러가 쓴다. */
                status: {
                    /**
                     * Format: int64
                     * @description 검증이 찾아 `bad/`로 옮긴 청크 수
                     */
                    bad_chunks?: number;
                    /** @description 매니페스트를 쓰는 중인 백업(쓰기 잠금 — `sweeping`이 거짓일 때만 더한다, §10.3) */
                    committing?: string[];
                    error?: string;
                    /** @description 진행 중인 GC(§10.2) */
                    gc?: components["schemas"]["GcRun"];
                    last_gc?: components["schemas"]["GcResult"];
                    last_verify?: components["schemas"]["VerifyResult"];
                    /**
                     * Format: int64
                     * @description 다음 예약 GC 시각(유닉스 초)
                     */
                    next_gc?: number;
                    /**
                     * Format: int64
                     * @description 다음 예약 검증 시각(유닉스 초)
                     */
                    next_verify?: number;
                    /**
                     * Format: uuid
                     * @description 공유가 아닌 백업 풀 안 저장소를 만든 노드(그 노드에만 있다 — §7.6). 공유 풀·S3면 없다.
                     */
                    node?: string;
                    /** @description 저장소 `owner.json`의 주인 클러스터(§12.6) */
                    owner?: components["schemas"]["StoreOwner"];
                    /** @default initializing */
                    phase: components["schemas"]["StorePhase"];
                    /**
                     * @description 검증이 깨진 청크를 `bad/`로 옮기는 중(리뷰 1 I5 — 쓸기 잠금처럼 `committing`이 비었을 때만 참이 되고 그동안 커밋
                     *     구간에 새로 들지 않는다)
                     */
                    quarantining?: boolean;
                    /**
                     * Format: uuid
                     * @description 저장소 `store.json`의 id. 이 클러스터가 만든 저장소는 리소스 id, 붙인 저장소는 남의 것이다.
                     */
                    store_id?: string;
                    /**
                     * Format: int64
                     * @description 쓸기를 시작할 때마다 1씩 오른다. 백업이 시작과 커밋 때 견준다.
                     */
                    sweep_epoch?: number;
                    /** @description GC가 청크를 지우는 중(쓰기 잠금 — `committing`이 비었을 때만 참이 된다) */
                    sweeping?: boolean;
                    /** @description 마지막 GC가 센 사용량 */
                    usage?: components["schemas"]["StoreUsage"];
                };
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_FolderSpec_FolderStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                spec: {
                    /** @description 이 폴더가 속한 트리 */
                    kind: components["schemas"]["FolderKind"];
                    /** @description 화면 이름. 형제 사이에서 대소문자 무시로 겹치지 않는다(상태 머신이 막는다). */
                    name: string;
                    /**
                     * Format: uuid
                     * @description 부모 폴더. 없으면 그 종류의 루트 바로 아래다. 부모는 같은 종류여야 한다.
                     */
                    parent?: string;
                    /** @description 하위 트리 할당량. VM 폴더(VM 차원)와 네트워크 폴더(`routers`·`floating_ips`, 계획 66)에만 둔다. 없으면 상한이 없다. */
                    quota?: components["schemas"]["FolderQuota"];
                };
                status: Record<string, never>;
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_GrantSpec_GrantStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                /** @description 권한 하나(`Kind::Grant`): 주체에게 대상에서 역할을 준다. */
                spec: {
                    principal: components["schemas"]["PrincipalRef"];
                    /**
                     * @description 하위 폴더와 그 안의 객체로 내려가는지. 거짓이면 대상 자체(폴더 이름 바꾸기·지우기·그 폴더의 권한 보기)에만
                     *     닿는다. 기본은 참이고 참이면 쓰지 않는다.
                     * @default true
                     */
                    propagate: boolean;
                    role: components["schemas"]["RoleRef"];
                    target: components["schemas"]["GrantTarget"];
                };
                status: Record<string, never>;
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_GroupSpec_GroupStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                /** @description 로컬 그룹(`Kind::Group`). 외부 사용자는 그 영역의 `external_groups`에 `external`의 그룹이 있으면 든다. */
                spec: {
                    description?: string;
                    /** @description 연결한 디렉터리 그룹 */
                    external?: components["schemas"]["ExternalGroup"][];
                    /** @description 멤버 사용자 id(로컬·외부) */
                    members?: string[];
                };
                status: Record<string, never>;
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_HaCheckSpec_HaCheckStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                /** @description 검사 하나 */
                spec: {
                    /**
                     * @description 끄면 노드가 검사하지 않는다.
                     * @default true
                     */
                    enabled: boolean;
                    /**
                     * Format: int32
                     * @description 이만큼(초) 넘게 실패해야 대응한다. 10~600
                     * @default 10
                     */
                    grace_secs: number;
                    /** @description 노드 이름 → 그 노드의 대상(종류는 `target`과 같아야 한다) */
                    nodes?: {
                        [key: string]: components["schemas"]["HaCheckTarget"];
                    };
                    /**
                     * @description 실패했을 때의 대응. 없으면 역할의 기본(`storage` → `restart_elsewhere`, `vm` → `migrate`,
                     *     `management` → `warn`)
                     */
                    on_fail?: components["schemas"]["HaCheckAction"];
                    /** @description 검사하는 네트워크·장치의 쓰임. 기본 대응과 영향받는 VM을 정한다. */
                    role: components["schemas"]["HaCheckRole"];
                    /** @description 모든 노드의 검사 대상(노드별 덮기가 없을 때) */
                    target: components["schemas"]["HaCheckTarget"];
                };
                status: Record<string, never>;
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_HaGroupSpec_HaGroupStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                /** @description HA 그룹: HA VM을 복구할 노드와 그 우선순위. VM은 `ha.group`으로 그룹을 쓴다. */
                spec: {
                    /** @description 그룹 노드. 우선순위가 큰 노드에 먼저 놓는다. */
                    nodes: components["schemas"]["HaGroupNode"][];
                    /** @description 켜면 우선순위가 더 높은 노드가 돌아와도 되돌아가지 않는다. */
                    nofailback?: boolean;
                    /** @description 켜면 그룹 밖 노드에는 놓지 않는다(놓을 곳이 없으면 켜지 않는다). */
                    restricted?: boolean;
                };
                status: Record<string, never>;
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_NetworkSpec_NetworkStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                /** @description `bridge`·`vxlan`·`sdn` 중 정확히 하나를 쓴다. */
                spec: {
                    /** @description 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`) */
                    bridge?: string;
                    /**
                     * Format: uuid
                     * @description 든 네트워크 폴더(계획 62, `Folder` id). 없으면 네트워크 트리의 루트이고 쓰지 않아 옛 노드도 네트워크를 푼다.
                     */
                    folder?: string;
                    /**
                     * Format: int32
                     * @description VXLAN·SDN 네트워크의 MTU(1280~9000, 기본 1450·1442). `vxlan`·`sdn`과만 쓴다.
                     */
                    mtu?: number;
                    /** @description OVN 논리 스위치(Geneve)로 만드는 SDN 네트워크(계획 65). 없으면 쓰지 않아 옛 노드도 네트워크를 푼다. */
                    sdn?: components["schemas"]["SdnNet"];
                    /** @description SDN 네트워크의 서브넷(주소 체계마다 하나까지, 계획 65). `sdn`과만 쓰고 비면 쓰지 않는다. */
                    subnets?: components["schemas"]["Subnet"][];
                    /**
                     * Format: int32
                     * @description VLAN 태그(1~4094). `bridge`와만 쓰고, 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
                     */
                    vlan?: number | null;
                    /** @description orbivisord가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만드는 네트워크 */
                    vxlan?: components["schemas"]["Vxlan"];
                };
                status: Record<string, never>;
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_NodeSpec_NodeStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                spec: {
                    /** @description API 주소 (`host:port`) */
                    address: string;
                    /** @description 예약·전용 CPU와 hugepages 예약(계획 64 §7). 없으면 공유 = 모든 CPU — 지금과 같고 쓰지 않아 옛 노드도 푼다. */
                    host_resources?: components["schemas"]["HostResources"];
                    /** @description 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다. */
                    maintenance?: boolean;
                    /**
                     * @description 이 노드의 역할. 없으면 기본 역할(위트니스가 아니면 `api`)이다. 옛 노드는 이 필드를 모르므로 기본이면
                     *     쓰지 않는다.
                     */
                    roles?: components["schemas"]["NodeRole"][];
                    /** @description SDN physnet 연결과 게이트웨이 노드(계획 66 §7). 없으면 쓰지 않아 옛 노드도 푼다. */
                    sdn?: components["schemas"]["NodeSdnConfig"];
                    /** @description 위트니스 노드: Raft 투표만 하고 VM을 돌리지 않는다. 합류할 때 정해지고 바뀌지 않는다. */
                    witness?: boolean;
                };
                /** @description 노드가 시작할 때 스스로 기록한다. */
                status: {
                    /** @default null */
                    arch: components["schemas"]["Arch"] | null;
                    /**
                     * @description 이 노드가 쓸 수 있는 백업 저장소마다 닿는지(계획 68 §6 — 60초마다 `store.json` 읽기). 비면 쓰지 않는다(옛 노드·
                     *     저장소 없음). 옛 노드는 무시한다.
                     */
                    backup_stores?: components["schemas"]["NodeStoreState"][];
                    /**
                     * @description 이 노드에 있는 Linux 브리지. 스케줄러가 브리지 네트워크를 쓰는 VM을 배치할 때 본다.
                     * @default []
                     */
                    bridges: components["schemas"]["BridgeInfo"][];
                    /** @description 이 노드의 HA 검사 결과(화면용, 바뀔 때만 쓴다). 리더는 리스 갱신에 실린 결과로 판단한다. */
                    checks?: components["schemas"]["CheckReport"][];
                    /**
                     * @description 이 노드가 원하는 클러스터 링크(`host:port`, 앞이 우선). 노드가 등록할 때 설정의 `cluster_addresses`로
                     *     쓴다(옛 노드는 비어 있다). 리더가 Raft 멤버십의 링크를 이것에 맞춘다.
                     */
                    cluster_addresses?: string[];
                    /**
                     * @description 이 노드의 QEMU CPU 카탈로그(계획 63: 해시와 QEMU·가속기·호스트 CPU). 노드가 시작할 때와 QEMU가 바뀔 때 쓴다.
                     *     없으면(옛 노드·위트니스) `host`·`max`만 켤 수 있다. 옛 노드는 이 필드를 무시한다.
                     */
                    cpu?: components["schemas"]["NodeCpu"];
                    /**
                     * Format: int32
                     * @default 0
                     */
                    cpus: number;
                    /**
                     * @description 이 노드의 orbivisord가 아는 기능(예: `node-drain`). 노드가 등록할 때 쓴다(옛 노드는 비어 있다). 새 기능을
                     *     쓰는 API는 모든 노드가 그 기능을 적었는지 본다(버전이 섞여도 된다).
                     */
                    features?: string[];
                    /**
                     * @description 이웃 노드가 이 노드의 전원 끄기 펜싱 방법으로 전원을 읽어 본 마지막 결과. 이 노드가 쓴다(`NodeFence`가
                     *     없으면 None). 옛 노드는 이 필드를 무시한다.
                     */
                    fence_check?: components["schemas"]["FenceCheck"];
                    /**
                     * @description 이 노드에 VM 방화벽(nftables)이 있다. 보안 그룹을 건 NIC가 있는 VM은 이런 노드에서만 돈다
                     *     (위트니스는 늘 false).
                     */
                    firewall?: boolean;
                    /**
                     * @description 이 노드가 켤 수 있는 펌웨어 장치(계획 59: swtpm·Secure Boot 펌웨어). 노드가 시작할 때와 노드 검사가 쓴다.
                     *     없으면(옛 노드·위트니스) 모두 못 한다.
                     */
                    firmware_caps?: components["schemas"]["FirmwareCaps"];
                    /** @description 이 노드의 HA 펜싱. None이면 펜싱이 꺼져 HA VM을 돌리지 않는다(위트니스는 늘 None). */
                    ha?: components["schemas"]["NodeHa"];
                    /** @description 이 노드의 QEMU가 할 수 있는 핫플러그(계획 54). 노드가 시작할 때 쓴다. 없으면(옛 노드·위트니스) 모두 못 한다. */
                    hotplug?: components["schemas"]["NodeHotplug"];
                    /**
                     * @description 예: `qemu-hvf`, `qemu-kvm`, `qemu-tcg`
                     * @default
                     */
                    hypervisor: string;
                    /**
                     * @description 이 노드의 네트워크 인터페이스(루프백·tap·opld가 만든 장치 빼고 64개까지). 노드가 쓴다. HA 검사를 고를 때
                     *     보고, 본딩 포트 상태를 보인다. 옛 노드는 이 필드를 무시한다.
                     */
                    interfaces?: components["schemas"]["NodeInterface"][];
                    /**
                     * Format: int64
                     * @default 0
                     */
                    memory_mib: number;
                    /** @default false */
                    online: boolean;
                    /**
                     * @description 이 노드가 쓸 수 있는 스토리지 풀(풀 이름 → 상태). 노드가 주기적으로 기록한다.
                     * @default {}
                     */
                    pools: {
                        [key: string]: components["schemas"]["PoolState"];
                    };
                    /**
                     * @description 이 노드가 단순 NIC의 tap에 tc 속도 제한(`clsact`·police, 계획 67 §8.2)을 걸 수 있다. 검사 루프가 쓴다. 거짓이면
                     *     쓰지 않는다(옛 노드·macOS·위트니스). 옛 노드는 무시한다.
                     */
                    qos?: boolean;
                    /**
                     * @description 이 노드의 SDN(OVN) chassis·중앙 상태(계획 65). 노드의 SDN 에이전트가 바뀔 때만, 모든 노드가 `sdn`을 알릴 때만
                     *     쓴다. 없으면(SDN 설정 전·옛 노드·macOS·위트니스) chassis가 아니다. 옛 노드는 무시한다.
                     */
                    sdn?: components["schemas"]["NodeSdn"];
                    /**
                     * @description 호스트가 꺼지는 중이라 이 노드가 종료 정책대로 정리하며 남긴 표시. 노드가 다시 등록할 때 지운다. 리더는
                     *     `freeze` 표시가 있는 오프라인 노드의 HA VM을 옮기지 않는다(옛 노드는 이 필드를 무시한다).
                     */
                    shutdown?: components["schemas"]["NodeShutdown"];
                    /**
                     * @description 이 노드의 CPU·메모리 NUMA 토폴로지(계획 64: 소켓·코어·스레드·NUMA 노드·hugepages·거리·L3). 노드가 시작할
                     *     때와 검사 루프가 바뀔 때만 쓴다. 없으면(옛 노드·macOS·위트니스) 게스트 vNUMA만 된다. 옛 노드는 무시한다.
                     */
                    topology?: components["schemas"]["NodeTopology"];
                    /**
                     * Format: ip
                     * @description VXLAN 패킷을 주고받는 이 노드의 주소. 없으면 이 노드에 VXLAN 장치를 만들지 않는다(위트니스는 늘
                     *     None).
                     */
                    underlay?: string;
                    /**
                     * @description 이 노드의 APT 업데이트 확인 결과. 업데이트가 켜진 노드는 등록할 때 `Some`(아직 확인 전이면
                     *     빈 값)이고, 꺼진 노드(위트니스, macOS 등)와 옛 노드는 None이다.
                     */
                    updates?: components["schemas"]["NodeUpdates"];
                    /** @description 이 노드에서 도는 orbivisord의 빌드 버전. 노드가 등록할 때 쓴다(옛 버전 노드는 None). */
                    version?: string;
                };
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_PlacementRuleSpec_PlacementRuleStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                /**
                 * @description 배치 규칙: VM끼리(같은 노드·다른 노드) 또는 VM과 노드(이 노드들 중·이 노드들은 안 됨). hard 규칙은
                 *     조건이고 soft 규칙은 벌점(`weight`)이다. 모든 VM에 적용한다.
                 */
                spec: {
                    /**
                     * @description 끄면 없는 것으로 본다.
                     * @default true
                     */
                    enabled: boolean;
                    /** @description hard 규칙만: HA 복구가 지킬 수 없으면 어기고 켠다. */
                    ha_may_violate?: boolean;
                    kind: components["schemas"]["RuleKind"];
                    /**
                     * @description 이 레이블(`key=value`, 예: `zone=rack-a`)을 가진 노드들
                     * @example zone=rack-a
                     */
                    node_tag?: string;
                    /** @description VM과 노드 규칙의 노드 이름. `node_tag`와 둘 중 하나만. */
                    nodes?: string[];
                    /**
                     * @description 이 레이블(`key=value`)을 가진 VM들
                     * @example app=web
                     */
                    vm_tag?: string;
                    /** @description 규칙의 VM 이름(아직 없는 VM이어도 된다). `vm_tag`와 둘 중 하나만. */
                    vms?: string[];
                    /**
                     * Format: int32
                     * @description soft 규칙의 벌점 1~100. 없으면 50.
                     */
                    weight?: number;
                };
                status: Record<string, never>;
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_SecurityGroupSpec_SecurityGroupStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                spec: {
                    /** @description 200자 이하 */
                    description?: string;
                    /** @description 적힌 순서대로 보고 처음 맞는 규칙을 따른다(200개 이하). */
                    rules?: components["schemas"]["Rule"][];
                };
                status: {
                    /** @description 이 그룹을 건 VM NIC 수(계획 66 §11.4). API가 응답에서만 채운다(Raft에 쓰지 않는다). */
                    nics?: components["schemas"]["SecurityGroupNics"];
                };
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_SnapshotSpec_SnapshotStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                spec: {
                    description?: string;
                    /**
                     * @description 메모리 스냅샷이면 그때 도는 QEMU의 하드웨어(계획 54, 모든 노드가 `hotplug`일 때만). 메모리를 불러오려면 같은
                     *     장치로 켜야 하므로 되돌리기는 이 모양(핫플러그 배치 있음·없음, 상한)으로 켠다. 없으면 배치 없이 켠다.
                     */
                    hardware?: components["schemas"]["RunningHardware"];
                    /** @description 메모리 상태를 담았는지. 담았으면 VM의 첫 디스크에 있다. */
                    memory: boolean;
                    /** @description 사용자가 붙인 이름. VM 안에서 고유하다. */
                    name: string;
                    /**
                     * Format: uuid
                     * @description 부모 스냅샷 id. VM의 첫 스냅샷이면 없다.
                     */
                    parent?: string | null;
                    /** @description 예약으로 만들었는지. 보관 개수 정리는 이것만 지운다. */
                    scheduled?: boolean;
                    /**
                     * Format: uuid
                     * @description VM id
                     */
                    vm: string;
                    /** @description 만든 순간의 VM 스펙 */
                    vm_spec: components["schemas"]["VmSpec"];
                };
                status: {
                    /**
                     * Format: int64
                     * @description 만든 시각(유닉스 초)
                     * @default null
                     */
                    created_at: number | null;
                    /** @default pending */
                    phase: components["schemas"]["SnapshotPhase"];
                    /**
                     * Format: uuid
                     * @description TPM 상태 사본(`{스냅샷 id}.tpm`)을 둔 노드(계획 59). 되돌리기는 이 노드에서만 TPM 상태를 돌려놓는다. 찍을 때
                     *     TPM 상태가 없었으면(TPM이 없거나 한 번도 켜지 않은 VM) 없다.
                     */
                    tpm_node?: string;
                    /**
                     * Format: int64
                     * @description 메모리 상태 크기. 메모리 스냅샷만 있다.
                     * @default null
                     */
                    vmstate_bytes: number | null;
                };
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_StoragePoolSpec_StoragePoolStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                spec: {
                    backend: components["schemas"]["PoolBackend"];
                    /** @description 백업 파일(`{루트}/backups/`)을 둘 수 있는 백업 풀인지. dir·NFS 풀만 된다. 켜도 VM 디스크는 그대로 둔다. */
                    backups?: boolean;
                    /**
                     * Format: uuid
                     * @description 든 스토리지 폴더(계획 62, `Folder` id). 없으면 스토리지 트리의 루트이고 쓰지 않아 옛 노드도 풀을 푼다.
                     */
                    folder?: string;
                    /** @description 이 풀을 쓸 수 있는 노드. 비어 있으면 모든 노드. */
                    nodes?: string[];
                    /** @description 모든 노드가 같은 데이터를 보는 공유 스토리지인지. NFS·RBD·공유 LVM 풀은 늘 공유다(`is_shared`). */
                    shared?: boolean;
                };
                status: {
                    /**
                     * Format: int64
                     * @default null
                     */
                    available_bytes: number | null;
                    /**
                     * Format: int64
                     * @default null
                     */
                    total_bytes: number | null;
                };
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_TaskSpec_TaskStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                spec: {
                    /**
                     * Format: uuid
                     * @description 작업을 실행할 노드
                     */
                    node: string;
                    op: components["schemas"]["TaskOp"];
                    /**
                     * Format: uuid
                     * @description 대상 리소스(VM 또는 Volume)의 id. 스냅샷·복제·백업 Task는 (원본) VM, 백업 지우기는 백업,
                     *     복원과 VM 가져오기는 새 VM, 가져오기 지우기는 가져오기다.
                     */
                    target: string;
                };
                status: {
                    /** @default null */
                    error: string | null;
                    /** @description 실행 중인 Task의 진행률(지금은 로컬 디스크를 복사하는 마이그레이션만). Task가 끝나면 지운다. */
                    progress?: components["schemas"]["TaskProgress"];
                    /** @default pending */
                    state: components["schemas"]["TaskState"];
                };
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_VmImportSpec_VmImportStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                spec: {
                    /**
                     * Format: uuid
                     * @description 원본 파일이 있는 노드(올린 노드)
                     */
                    node: string;
                    source: components["schemas"]["ImportSource"];
                };
                status: {
                    /** @description 마지막 실행이 실패한 이유(phase는 `ready`로 돌아간다), 또는 `failed`인 이유 */
                    error?: string;
                    /** @default ready */
                    phase: components["schemas"]["ImportPhase"];
                    /** @description [`propose`]가 만든 기본값 */
                    proposal?: components["schemas"]["ImportProposal"];
                    /** @description 원본 VM에서 읽은 것(표시용) */
                    source_vm?: components["schemas"]["SourceVm"];
                    /**
                     * Format: uuid
                     * @description 만든 VM id(`done`일 때)
                     */
                    vm?: string;
                    /**
                     * @description 해석과 제안의 경고
                     * @default []
                     */
                    warnings: string[];
                };
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_VmSpec_VmStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                /** @description VM의 원하는 상태. */
                spec: {
                    /** @description CD-ROM 드라이브(계획 57, 넷까지, 순번 = 드라이브 번호). 비면 쓰지 않아 옛 노드도 VM을 푼다. */
                    cdroms?: components["schemas"]["CdromSpec"][];
                    /** @description cloud-init NoCloud 씨앗(계획 57). 없으면 쓰지 않아 옛 노드도 VM을 푼다. */
                    cloud_init?: components["schemas"]["CloudInit"];
                    cpu: components["schemas"]["CpuSpec"];
                    disks?: components["schemas"]["DiskSpec"][];
                    firmware?: components["schemas"]["Firmware"];
                    /**
                     * Format: uuid
                     * @description 든 VM 폴더(계획 62, `Folder` id). 없으면 VM 트리의 루트이고 쓰지 않아 옛 노드도 VM을 푼다. 템플릿도 같은
                     *     트리에 산다.
                     */
                    folder?: string;
                    /**
                     * @description 게스트 에이전트(계획 59): 채널을 달고 게스트 정보·에이전트 종료·fs-freeze에 쓴다. 없으면 쓰지 않아 옛 노드도
                     *     VM을 푼다(`ha.guest_watch`가 있으면 채널은 그대로 단다).
                     */
                    guest_agent?: components["schemas"]["GuestAgentSpec"];
                    /**
                     * @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다.
                     *     JSON은 옛 bool(`false` = 없음, `true` = 기본 정책)이나 정책 객체다.
                     */
                    ha?: components["schemas"]["HaSetting"];
                    /** @description 게스트 메모리를 hugepages로(계획 64, `2M`·`1G`·`any`). 없으면 보통 페이지이고 쓰지 않는다. */
                    hugepages?: components["schemas"]["PageSize"];
                    machine: components["schemas"]["MachineType"];
                    /**
                     * Format: int32
                     * @description 메모리 핫플러그 상한(MiB, 계획 54). 없으면 `memory_mib`와 같다(핫플러그 꺼짐). 없으면 쓰지 않아 옛 노드도
                     *     VM을 푼다.
                     */
                    max_memory_mib?: number;
                    /** Format: int32 */
                    memory_mib: number;
                    nics?: components["schemas"]["NicSpec"][];
                    /**
                     * Format: uuid
                     * @description 배치된 노드의 id. None이면 시작할 때 스케줄러가 고른다.
                     */
                    node?: string | null;
                    /**
                     * @description 게스트 vNUMA(계획 64). 없으면 전용·hugepages VM은 `auto`, 그 밖은 vNUMA 없음(지금 그대로)이고 쓰지 않아 옛
                     *     노드도 VM을 푼다.
                     */
                    numa?: components["schemas"]["NumaSpec"];
                    /** @description 최적 배치 옵션(계획 53). 없으면 쓰지 않아 옛 노드도 VM을 푼다. */
                    placement?: components["schemas"]["VmPlacement"];
                    power?: components["schemas"]["PowerState"];
                    /**
                     * @description 로컬 디스크 복제 대상(계획 55, 셋까지). 노드 로컬 풀의 디스크를 이 노드들에 주기적으로 복제한다. 비면 쓰지
                     *     않아 옛 노드도 VM을 푼다.
                     */
                    replication?: components["schemas"]["ReplicationTarget"][];
                    restart?: components["schemas"]["RestartPolicy"];
                    /**
                     * @description Secure Boot(계획 59): Microsoft 키를 등록한 UEFI 변수로 켜는 secure-boot 펌웨어(x86_64는 SMM). UEFI VM만 쓸 수
                     *     있다. 거짓이면 쓰지 않아 옛 노드도 VM을 푼다.
                     */
                    secure_boot?: boolean;
                    snapshot_schedule?: components["schemas"]["SnapshotSchedule"] | null;
                    /**
                     * @description 템플릿(계획 58): 켤 수 없고 디스크를 바꿀 수 없는 원본. 배포(복제)와 연결 복제의 기반이 된다. 거짓이면 쓰지
                     *     않아 옛 노드도 VM을 푼다.
                     */
                    template?: boolean;
                    /**
                     * @description TPM 2.0(계획 59): VM마다 swtpm 하나를 띄워 단다(q35 `tpm-crb`, virt `tpm-tis-device`). UEFI VM만 쓸 수 있다.
                     *     상태는 노드 로컬 파일이고 `firmware_node`를 따라다닌다. 거짓이면 쓰지 않아 옛 노드도 VM을 푼다.
                     */
                    tpm?: boolean;
                };
                /** @description VM의 실제 상태. reconciler만 기록한다. */
                status: {
                    /**
                     * @description `tpm_sha256`과 같은 펌웨어 사본(UEFI 변수 + TPM)을 가진 다른 노드(계획 59). 해시가 바뀌면 비운다. 비면 쓰지
                     *     않는다.
                     */
                    firmware_copies?: string[];
                    /**
                     * Format: int32
                     * @description 펌웨어 초기화 회차(계획 59). `vm-firmware-reset`이 1씩 올린다. 노드의 파일이 다른 회차의 것이면 낡은 것이다.
                     *     0이면 쓰지 않는다.
                     */
                    firmware_epoch?: number;
                    /**
                     * Format: uuid
                     * @description 지금 UEFI 변수 파일을 가진 노드. UEFI VM을 켠 노드나 마이그레이션의 대상 노드가 쓴다.
                     *     다른 노드에서 켤 때 이 노드에서 변수를 받아 온다. BIOS VM은 늘 없다.
                     */
                    firmware_node?: string;
                    /** @description HA 재시작·옮기기 기록. 기록이 없으면 빼고 쓴다. */
                    ha?: components["schemas"]["VmHaStatus"];
                    /**
                     * @description 도는 QEMU의 하드웨어(계획 54). VM을 켠 노드가 `running`을 쓸 때 함께 쓰고 핫플러그 Task가 고치며, QEMU가
                     *     없어지면 지운다. 옛 노드는 이 필드를 무시한다.
                     */
                    hardware?: components["schemas"]["RunningHardware"];
                    /** @default null */
                    last_error: string | null;
                    /**
                     * @description HA가 이 VM을 복제본으로 다시 켠 마지막 복구(계획 55). 리더의 HA 복구가 쓰고 다음 복구가 덮는다. 옛 노드는 이
                     *     필드를 무시한다.
                     */
                    last_recovery?: components["schemas"]["ReplicaRecovery"];
                    /**
                     * @description 진행 중인 라이브 복원(계획 68 §11). 라이브 복원 Task가 쓰고 갈아타기가 끝나면 지운다. 있는 동안 reconciler·HA가
                     *     이 VM을 켜거나 옮기지 않는다. 없으면 쓰지 않는다.
                     */
                    live_restore?: components["schemas"]["LiveRestore"];
                    /** @description 진행 중인 라이브 마이그레이션. 원본 노드의 마이그레이션 Task가 쓰고 지운다. */
                    migration?: components["schemas"]["MigrationStatus"];
                    /**
                     * Format: uuid
                     * @default null
                     */
                    node: string | null;
                    /** @default stopped */
                    phase: components["schemas"]["VmPhase"];
                    /**
                     * Format: int32
                     * @default null
                     */
                    pid: number | null;
                    /**
                     * @description 로컬 디스크 복제 대상마다의 상태(계획 55). 원본 노드의 복제 Task, 리더의 HA 복구·스케줄러, 마이그레이션
                     *     확정이 쓴다. 옛 노드는 이 필드를 무시한다.
                     */
                    replication?: components["schemas"]["ReplicaStatus"][];
                    /** @description 지금 배치가 어기는 배치 규칙의 이름. 리더의 HA 관리자가 쓴다. */
                    rule_violations?: string[];
                    /** @default null */
                    schedule_error: components["schemas"]["ScheduleError"] | null;
                    /**
                     * Format: uuid
                     * @description 현재 위치: 마지막으로 만들었거나 되돌린 스냅샷. 다음 스냅샷의 부모가 된다.
                     * @default null
                     */
                    snapshot: string | null;
                    /**
                     * Format: uuid
                     * @description 지금 TPM 상태를 가진 노드(리뷰 2). TPM VM을 켠 노드·마이그레이션 대상·TPM을 되돌리거나 복원한 노드가 쓰고, TPM을
                     *     초기화하면 지운다. UEFI 변수를 가진 노드(`firmware_node`)와 다를 수 있다(TPM을 끈 채 다른 노드에서 켬). 이 노드의
                     *     파일만 기록된 해시(`tpm_sha256`) 뒤에 바뀌었을 수 있어 해시가 달라도 지금 것으로 본다. 없으면 쓰지 않는다.
                     */
                    tpm_node?: string;
                    /**
                     * @description 지금 TPM 영구 상태(`tpm2-00.permall`)의 SHA-256(16진 소문자, 계획 59). 비밀이 아니다. VM을 돌리는 노드가
                     *     멈출 때와 실행 중 주기적으로 쓴다. 없으면(아직 TPM 상태가 없음, 초기화 뒤) 쓰지 않는다.
                     */
                    tpm_sha256?: string;
                };
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_Resource_VolumeSpec_VolumeStatus: {
            items: {
                /** Format: uuid */
                id: string;
                labels?: {
                    [key: string]: string;
                };
                name: string;
                /**
                 * Format: int64
                 * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
                 */
                revision: number;
                spec: {
                    /**
                     * Format: uuid
                     * @description 연결 복제(계획 58)면 기반(템플릿 디스크) 볼륨의 id. 기반은 늘 같은 풀에 있다. 없으면 쓰지 않아 옛 노드도
                     *     볼륨을 푼다.
                     */
                    base?: string;
                    /** @description 내용 종류(계획 57). 디스크(기본)면 쓰지 않아 옛 노드도 볼륨을 푼다. */
                    content?: components["schemas"]["VolumeContent"];
                    format: components["schemas"]["ImageFormat"];
                    /** @description StoragePool 리소스 이름 */
                    pool: string;
                    /** Format: int64 */
                    size_bytes: number;
                };
                status: {
                    /**
                     * Format: int64
                     * @description 실제로 디스크에 할당된 크기
                     * @default null
                     */
                    allocated_bytes: number | null;
                    /**
                     * Format: uuid
                     * @default null
                     */
                    attached_to: string | null;
                    /**
                     * Format: int64
                     * @description 늘리기 Task가 저장소를 늘린 뒤 읽은 실제 크기(계획 62 리뷰 1). 스펙 크기를 이 값까지 올리는 기록은 폴더
                     *     할당량에 막히지 않는다(이미 커진 디스크를 기록에서 숨기지 않게). Task가 기록한 뒤 지운다. 없으면 쓰지 않는다.
                     */
                    disk_bytes?: number;
                    /**
                     * Format: uuid
                     * @description 볼륨을 만든 Task를 실행하는 노드. 그 노드만 시작할 때 남은 고아 볼륨을 정리한다.
                     * @default null
                     */
                    node: string | null;
                    /** @default pending */
                    phase: components["schemas"]["VolumePhase"];
                };
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_TokenView: {
            items: {
                /** Format: uuid */
                id: string;
                name: string;
                /** Format: int64 */
                revision: number;
                /** @description 권한 범위(계획 62). 없으면 주인 권한 그대로다. */
                scope?: components["schemas"]["TokenScope"];
                /** Format: uuid */
                user: string;
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다. */
        List_UserView: {
            items: {
                /** Format: uuid */
                id: string;
                name: string;
                /** @description 외부 사용자의 영역. 로컬 사용자는 없다. */
                realm?: string;
                /** Format: int64 */
                revision: number;
                role: components["schemas"]["Role"];
                /** @description 2단계 인증 요소(확인된 TOTP 또는 패스키)가 있다(계획 62) */
                two_factor?: boolean;
            }[];
            /** Format: int64 */
            revision: number;
        };
        /** @description 호스트 NUMA 노드 하나의 지금 값 */
        LiveNode: {
            hugepages?: components["schemas"]["LivePool"][];
            /** Format: int32 */
            id: number;
            /**
             * Format: int64
             * @description 빈 메모리(`MemFree`, hugepages 풀 밖)
             */
            memory_free_mib: number;
        };
        /** @description hugepages 풀 하나의 지금 값 */
        LivePool: {
            /**
             * Format: int32
             * @description 아무도 쓰지 않는 페이지(`free_hugepages`)
             */
            free: number;
            /** Format: int32 */
            size_kib: number;
            /**
             * Format: int32
             * @description 예약 밖에서 커널이 더 준 페이지(`surplus_hugepages`)
             */
            surplus: number;
        };
        /** @description 라이브 복원 중인 VM의 상태(§11 — `VmStatus.live_restore`). 라이브 복원 Task가 쓰고, 갈아타기가 끝나면 지운다. */
        LiveRestore: {
            /**
             * Format: uuid
             * @description 되살리는 백업
             */
            backup: string;
            /**
             * @description 취소 요청(§11.4 `live-restore/cancel { discard: true }`): Task가 VM을 끄고 VM·대상 볼륨·오버레이를 지운다. 그동안
             *     reconciler는 VM을 켜지 않는다.
             */
            cancel?: boolean;
            /**
             * Format: int64
             * @description 대상 볼륨으로 옮긴 바이트(mirror 진행)
             */
            done_bytes?: number;
            /** @description 멈춘 까닭이나 실패한 이유 */
            error?: string;
            phase: components["schemas"]["LiveRestorePhase"];
            /**
             * Format: int64
             * @description 시작한 시각(유닉스 초)
             */
            started_at: number;
            /**
             * Format: uuid
             * @description 라이브 복원 Task
             */
            task: string;
            /** Format: int64 */
            total_bytes?: number;
        };
        /** @enum {string} */
        LiveRestorePhase: "starting" | "copying" | "pivoting" | "paused" | "stopped" | "failed";
        /** @description `GET /load-balancers` */
        LoadBalancerList: {
            items: components["schemas"]["LoadBalancerView"][];
            /** Format: int64 */
            revision: number;
        };
        /** @description `PATCH /load-balancers/{key}`: 준 칸만 바꾼다(리스너는 통째로). VIP 네트워크는 바꿀 수 없다(새로 만든다). */
        LoadBalancerPatch: {
            /**
             * Format: int32
             * @description 출발지 고정(초), `null`은 끄기
             */
            affinity_secs?: number | null;
            /** @description 빈 글이면 설명을 지운다. */
            description?: string;
            external?: components["schemas"]["LbExternal"] | null;
            /** @description 리스너 전체 */
            listeners?: components["schemas"]["LbListener"][];
        };
        LoadBalancerSpec: {
            /**
             * Format: int32
             * @description 같은 클라이언트를 이 시간(초, 1~65535) 동안 같은 멤버로 보낸다. 없으면 연결마다 해시다.
             */
            affinity_secs?: number;
            /** @description 200자 이하 */
            description?: string;
            /** @description 바깥 VIP(VIP 네트워크 라우터의 바깥 공급자 네트워크). 없으면 안쪽에서만 닿는다. */
            external?: components["schemas"]["LbExternal"];
            /**
             * Format: uuid
             * @description 든 네트워크 폴더(`Folder` id). 없으면 네트워크 트리의 루트다.
             */
            folder?: string;
            /** @description 리스너(1~16개, 같은 프로토콜·포트는 한 번) */
            listeners: components["schemas"]["LbListener"][];
            /**
             * @description 건강 검사 출발 주소(건강 검사가 있는 NIC 멤버의 네트워크마다 하나). API가 그 서브넷 풀에서 채운다(사용자가 주지
             *     않는다).
             */
            monitors?: components["schemas"]["LbMonitor"][];
            /** @description VIP 네트워크(IPv4 서브넷이 있고 라우터 인터페이스인 오버레이 SDN 네트워크) 이름 */
            network: string;
            /**
             * Format: ipv4
             * @description VIP(IPv4). API가 그 서브넷 풀에서 채운다(직접 주면 풀 안).
             */
            vip?: string;
        };
        /** @description 지금은 칸이 없다(멤버 건강·VIP 상태는 보기가 SB·NB에서 읽는다). */
        LoadBalancerStatus: Record<string, never>;
        /** @description `GET /load-balancers/{key}`: 부하 분산기와 VIP·바깥 VIP·멤버의 지금 모양 */
        LoadBalancerView: components["schemas"]["Resource_LoadBalancerSpec_LoadBalancerStatus"] & {
            external_state?: components["schemas"]["LbState"];
            /** @description SB를 읽지 못한 까닭(멤버 건강이 `unchecked`로 남는다) */
            live_error?: string;
            /** @description 리스너 차례의 멤버 */
            members: components["schemas"]["LbMemberView"][];
            vip_state: components["schemas"]["LbState"];
        };
        /** @description `POST /sdn/lockdown?dry_run=true`(§10.4 미리 확인): 허용 집합과 그 밖에서 붙어 있는 상대 */
        LockdownCheck: {
            /** @description 켜기가 막힌다(`outside`가 있고 `force`가 아님) */
            blocked: boolean;
            /** @description 중앙 집합(Raft·SB 특권 포트) */
            centrals: string[];
            enabled: boolean;
            /** @description 멤버 집합(NB 6641·SB 역할 6642를 받는 출발 주소) */
            members: string[];
            /** @description 지금 중앙 6641·6642에 붙은 상대 가운데 멤버 집합 밖인 것(중앙이 알린 `NodeSdn.peers`) */
            outside: components["schemas"]["LockdownPeer"][];
            /** @description 막지는 않지만 알릴 것(중앙이 둘이면 차례마다 SB가 정족수를 잃음 따위 — 리뷰 1 M4) */
            warnings?: string[];
        };
        /** @description 노드의 OVN 주소 허용 목록 상태(`NodeSdn.lockdown`) */
        LockdownNodeState: {
            /**
             * @description 허용 집합의 주소 수
             * @default 0
             */
            allowed: number;
            error?: string;
            /**
             * @description 표가 원하는 집합과 맞다.
             * @default false
             */
            firewall: boolean;
        };
        /** @description 노드 하나의 허용 목록 표 */
        LockdownNodeView: {
            /** @description 표의 허용 주소 수 */
            allowed: number;
            error?: string;
            /** @description 표가 원하는 집합과 맞다(알리지 않았으면 거짓). */
            firewall: boolean;
            name: string;
            /** Format: uuid */
            node: string;
        };
        /** @description 중앙에 붙어 있지만 허용 집합 밖인 상대(잠그면 끊긴다 — NAT·다른 출발 주소로 붙은 멤버 노드일 수 있다) */
        LockdownPeer: {
            /** @description 그 상대가 붙은 중앙 노드 이름 */
            central: string;
            /** Format: ip */
            peer: string;
            /** @description Raft(6643·6644) 상대이고 중앙 집합 밖이다(거짓이면 NB·SB 6641·6642 상대이고 멤버 집합 밖 — 리뷰 1 I5). */
            raft?: boolean;
        };
        /** @description `GET /sdn`의 잠금(§10.5): 켜졌는지, 바꾼 중앙, 허용 집합, 표를 거는 노드마다의 상태 */
        LockdownView: {
            /** @description SB를 원하는 모양으로 바꾼 중앙 */
            applied: string[];
            centrals: string[];
            /** @description 원하는 상태(`SdnSpec.lockdown`) */
            enabled: boolean;
            error?: string;
            members: string[];
            /** @description 허용 목록 표를 거는 노드(중앙과 구성된 chassis, 이름 순) */
            nodes: components["schemas"]["LockdownNodeView"][];
            /** @description 중앙을 바꾸는 중이다. */
            pending?: boolean;
        };
        LoginRequest: {
            password: string;
            /** @description 없거나 `local`이면 로컬 사용자 */
            realm?: string;
            /** @description 영역 로그인이면 영역 안의 이름이다(`alice`로 로그인하면 사용자 `alice@<영역>`). */
            username: string;
        };
        /** @description 로그인 응답: 사용자와, 둘째 단계가 남았으면 그 안내(쿠키는 대기 세션이다) */
        LoginResponse: components["schemas"]["UserView"] & {
            /** @description 둘째 단계가 남았다(`POST /auth/login/second-factor`, 강제 등록이면 `/auth/2fa/…`). 없으면 로그인이 끝났다. */
            second_factor?: components["schemas"]["SecondFactorPrompt"];
        };
        /** @description `52:54:00:12:34:56` 형식의 MAC 주소. */
        MacAddr: string;
        /** @enum {string} */
        MachineType: "q35" | "virt";
        /**
         * @description 멤버의 건강(SB `Service_Monitor`)
         * @enum {string}
         */
        MemberHealth: "online" | "offline" | "error" | "unchecked" | "missing";
        /** @description 옮길 리소스 하나 */
        MemberRef: {
            /** @description 이름이나 id */
            key: string;
            /** @description `vm`(템플릿 포함), `network`, `storage-pool` */
            kind: string;
        };
        /**
         * @description 노드가 클러스터에 무엇으로 참여하는지
         * @enum {string}
         */
        MemberRole: "voter" | "learner" | "observer";
        /** @description 클러스터 멤버 한 노드 */
        MemberView: {
            /** @description API 주소 */
            address?: string | null;
            /** @description 노드 간 통신 주소(우선 링크) */
            cluster_address: string;
            /** @description 노드 간 통신 링크 모두(앞이 우선, 첫 것이 `cluster_address`) */
            cluster_addresses: string[];
            fencing?: components["schemas"]["Fencing"] | null;
            /** Format: uuid */
            id: string;
            /** @description Node 리소스 이름. 아직 없으면(합류 중) 비어 있다. */
            name?: string | null;
            online: boolean;
            role: components["schemas"]["MemberRole"];
            /** @description 노드의 orbivisord 빌드 버전. 옛 버전 노드이거나 Node가 아직 없으면 null이다. */
            version?: string | null;
            /** @description 투표 멤버인지(`role`이 `voter`) */
            voter: boolean;
            /** @description 위트니스 노드인지(VM을 돌리지 않는 투표 멤버) */
            witness: boolean;
        };
        /**
         * @description 게스트 노드 메모리를 호스트 NUMA 노드에 묶는 방법
         * @enum {string}
         */
        MemoryPolicy: "strict" | "preferred";
        MgrReport: {
            active?: string;
            /** @default false */
            available: boolean;
            /** @default [] */
            standbys: string[];
        };
        MigrateRequest: {
            /**
             * @description hard 배치 규칙(restricted HA 그룹 포함)을 넘어 옮긴다. 노드 조건과 메모리는 그대로 본다. 배치 규칙을
             *     쓸 수 있는 사용자(관리자)만이다(아니면 403). 넘은 규칙은 감사 기록에 남는다.
             */
            force?: boolean;
            /** @description 옮겨 갈 노드(이름이나 id) */
            node: string;
            /** @description 노드 로컬 풀의 디스크를 복사해 둘 대상 노드의 풀. 없으면 디스크마다 같은 이름의 풀이다. */
            pool?: string | null;
        };
        /** @description 사용자가 주는 NIC 하나의 주소 */
        MigrationAddress: {
            /** @description 주소 체계마다 하나까지 */
            ip: string[];
            mac: string;
            /** Format: uuid */
            vm: string;
        };
        /** @description 놓은 다리(노드와 그 노드의 임시 physnet `mig<h8>`) */
        MigrationBridge: {
            /** Format: uuid */
            node: string;
            physnet: string;
        };
        /** @description `POST /network-migrations/{key}/finish` */
        MigrationFinishRequest: {
            /** @description 다리를 걷은 뒤 대상 네트워크를 이 라우터(이름이나 id)의 인터페이스로 붙인다. */
            router?: string;
        };
        /** @enum {string} */
        MigrationMode: "auto" | "offline" | "restart" | "live";
        /** @enum {string} */
        MigrationPhase: "planned" | "running" | "paused" | "moved" | "finished" | "rolling-back" | "rolled-back" | "failed";
        /** @description 계획 전체 */
        MigrationPlan: {
            bridge: components["schemas"]["PlannedBridge"];
            /** @description 원본에 NIC가 있지만 부른 사람이 읽을 수 없는 VM 수(리뷰 1 I3 — `nics`에 없고 옮기지 않는다) */
            hidden?: number;
            nics: components["schemas"]["PlannedNic"][];
        };
        /** @description `POST /network-migrations/{key}/rollback` */
        MigrationRollbackRequest: {
            /** @description 되돌릴 VM(이름이나 id). 비면 옮긴(또는 실패한) 모든 VM */
            vms?: string[];
        };
        /** @description `POST /network-migrations/{key}/start`·`resume` */
        MigrationStartRequest: {
            /**
             * @description 다리 노드(이름이나 id)를 바꾼다(§13 — 다리 노드를 클러스터에서 지웠거나 그 노드가 꺼져 멈춘 옮기기를 다른 노드로
             *     이어 간다). 놓인 다리가 온라인 노드에 있으면 바꿀 수 없다(409 `migration_bridge_laid`).
             */
            bridge_node?: string;
        };
        /** @description 진행 중인 라이브 마이그레이션: 대상 노드와 그 마이그레이션 Task */
        MigrationStatus: {
            /**
             * @description 대상 노드로 복사하는 노드 로컬 풀의 디스크. VM이 대상 노드로 확정되면 적용된 것이 된다.
             *     비면 빼고 쓰므로 공유 풀만의 마이그레이션은 기록이 옛 모양 그대로다.
             */
            disks?: components["schemas"]["DiskMove"][];
            /** Format: uuid */
            node: string;
            /** Format: uuid */
            task: string;
        };
        /**
         * @description `network-migrate` Task의 걸음(계획 67 §9.4·§9.6·§9.7)
         * @enum {string}
         */
        MigrationStep: "start" | "rollback" | "finish";
        /** @description 받는 쪽 저장소에 없는 청크 */
        MissingChunks: {
            missing: string[];
        };
        MonReport: {
            /**
             * @description v2 주소(`10.0.0.1:3300`)
             * @default
             */
            addr: string;
            /** @default false */
            in_quorum: boolean;
            /** @default  */
            name: string;
        };
        MoveMembersRequest: {
            items: components["schemas"]["MemberRef"][];
        };
        MoveMembersResponse: {
            items: components["schemas"]["MoveResult"][];
        };
        /**
         * @description 옮기는 이유
         * @enum {string}
         */
        MoveReason: "balance" | "rule";
        /** @description 항목 하나의 결과 */
        MoveResult: {
            /** @description 실패한 이유(`code`·`message`) */
            error?: components["schemas"]["ErrorBody"];
            key: string;
            kind: string;
            /** @description 옮겼거나 이미 그 폴더에 있었다. */
            ok: boolean;
        };
        /** @enum {string} */
        MoveState: "pending" | "moving" | "moved" | "failed" | "rolling-back" | "rolled-back";
        /** @description 내 권한(색인 그대로). 객체의 유효 권한 = 그 객체 폴더의 `contents`(없으면 `root_contents`) ∪ `objects`의 그 객체 */
        MyPermissions: {
            /** @description 루트에 모든 권한(전역 admin): 아래 표 없이 전부 참 */
            all: boolean;
            /** @description 폴더 안 객체의 권한(하위 폴더 포함) */
            contents: {
                [key: string]: string[];
            };
            /** @description 폴더 자체의 권한(이름 바꾸기·지우기·그 폴더의 grant) */
            folders: {
                [key: string]: string[];
            };
            /** @description 객체에 직접 건 grant로 더해지는 권한 */
            objects: components["schemas"]["ObjectPermissions"][];
            /** @description 루트(클러스터 객체)의 권한 */
            root: string[];
            /** @description 루트에 있는 객체(폴더 없음)의 권한 */
            root_contents: string[];
        };
        /** @description `GET /networks/{key}/ips` */
        NetworkIps: {
            /** @description 템플릿이 아닌 VM의 이 네트워크 NIC마다(VM 이름, NIC 순). 부른 사용자가 읽을 수 있는 VM의 것만 있다. */
            allocations: components["schemas"]["IpAllocation"][];
            /** @description 부른 사용자가 읽을 수 없는 VM(다른 폴더)의 NIC 수: 목록에서 빠졌지만 `subnets`의 사용 수에는 들었다. */
            hidden?: number;
            /** @description OVN을 읽지 못한 까닭(그때 `live`는 모두 null) */
            live_error?: string;
            network: string;
            subnets: components["schemas"]["SubnetUsage"][];
        };
        /** @description 만든 옮기기와 그 계획(VM NIC마다 막는 까닭·경고) */
        NetworkMigrationCreated: components["schemas"]["Resource_NetworkMigrationSpec_NetworkMigrationStatus"] & {
            plan: components["schemas"]["MigrationPlan"];
        };
        /** @description `GET /network-migrations` */
        NetworkMigrationList: {
            items: components["schemas"]["Resource_NetworkMigrationSpec_NetworkMigrationStatus"][];
            /** Format: int64 */
            revision: number;
        };
        NetworkMigrationSpec: {
            /** @description 사용자가 주는 NIC 주소(게스트 에이전트·cloud-init보다 앞선다) */
            addresses?: components["schemas"]["MigrationAddress"][];
            /** @description 게스트가 볼 MTU가 줄어도 `live`로 옮긴다(아니면 `restart`). */
            allow_mtu_shrink?: boolean;
            /** @description 옮기는 동안 L2 다리를 놓는지. 없으면 대상이 오버레이면 참이다. */
            bridge?: boolean;
            /**
             * Format: uuid
             * @description 다리 노드. 없으면 원본 브리지가 있는 chassis 가운데 옮길 VM이 가장 많은 노드다.
             */
            bridge_node?: string;
            /**
             * Format: uuid
             * @description 든 네트워크 폴더(`Folder` id — 원본 네트워크의 폴더). 없으면 네트워크 트리의 루트다.
             */
            folder?: string;
            /** @description VM마다 방식. 없으면 `auto`(전원·MTU·주소로 고름) */
            mode?: components["schemas"]["MigrationMode"];
            /**
             * Format: int32
             * @description 한 번에 옮기는 VM 수(1~8). 없으면 1이다.
             */
            parallel?: number;
            /** @description 원본 단순 네트워크(브리지·VLAN·VXLAN) 이름 */
            source: string;
            /** @description 대상 SDN 네트워크(서브넷이 있는 오버레이 또는 공급자) 이름 */
            target: string;
            /** @description 옮길 VM. 비면 계획할 때 원본에 NIC가 있는 모든 VM이다. */
            vms?: string[];
        };
        NetworkMigrationStatus: {
            /** @description 놓은 다리 */
            bridge?: components["schemas"]["MigrationBridge"];
            error?: string;
            /** @default planned */
            phase: components["schemas"]["MigrationPhase"];
            /**
             * Format: uuid
             * @description 도는(돈) `network-migrate` Task
             */
            task?: string;
            /** @description VM NIC마다의 계획과 상태(만들 때 계획을 적고 Task가 고친다) */
            vms?: components["schemas"]["VmMove"][];
        };
        /** @description `PATCH /networks/{key}`: SDN 네트워크의 서브넷 목록 전체(계획 65 §9) */
        NetworkPatch: {
            /**
             * @description 새 서브넷 목록. 이미 있는 서브넷(같은 `cidr`)은 `gateway`를 바꿀 수 없고 `dns_servers`·`pools`·`dhcp`·
             *     `description`을 바꾼다. 빠진 서브넷은 쓰는 NIC가 없어야 한다.
             */
            subnets: components["schemas"]["Subnet"][];
        };
        /** @description `bridge`·`vxlan`·`sdn` 중 정확히 하나를 쓴다. */
        NetworkSpec: {
            /** @description 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`) */
            bridge?: string;
            /**
             * Format: uuid
             * @description 든 네트워크 폴더(계획 62, `Folder` id). 없으면 네트워크 트리의 루트이고 쓰지 않아 옛 노드도 네트워크를 푼다.
             */
            folder?: string;
            /**
             * Format: int32
             * @description VXLAN·SDN 네트워크의 MTU(1280~9000, 기본 1450·1442). `vxlan`·`sdn`과만 쓴다.
             */
            mtu?: number;
            /** @description OVN 논리 스위치(Geneve)로 만드는 SDN 네트워크(계획 65). 없으면 쓰지 않아 옛 노드도 네트워크를 푼다. */
            sdn?: components["schemas"]["SdnNet"];
            /** @description SDN 네트워크의 서브넷(주소 체계마다 하나까지, 계획 65). `sdn`과만 쓰고 비면 쓰지 않는다. */
            subnets?: components["schemas"]["Subnet"][];
            /**
             * Format: int32
             * @description VLAN 태그(1~4094). `bridge`와만 쓰고, 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
             */
            vlan?: number | null;
            /** @description orbivisord가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만드는 네트워크 */
            vxlan?: components["schemas"]["Vxlan"];
        };
        NetworkStatus: Record<string, never>;
        /** @description `GET /network-topology` */
        NetworkTopology: {
            floating_ips: components["schemas"]["TopoFip"][];
            hidden: components["schemas"]["TopoHidden"];
            load_balancers: components["schemas"]["TopoLb"][];
            networks: components["schemas"]["TopoNetwork"][];
            routers: components["schemas"]["TopoRouter"][];
            vms: components["schemas"]["TopoVm"][];
        };
        /** @enum {string} */
        NicModel: "virtio" | "e1000e";
        /** @description 옮기기 전 NIC(되돌리기가 되살린다) */
        NicOrigin: {
            /** @description 지운 cloud-init NIC 항목 */
            cloud_init?: components["schemas"]["CloudInitNet"];
            network: string;
            security_groups?: string[];
        };
        /** @description NIC 하나의 QoS(`NicSpec.qos`). 보내기·받기는 VM 쪽에서 본 방향이다. */
        NicQos: {
            /**
             * Format: int32
             * @description 버스트(kbit, 1~10000000). 없으면 속도(kbit/s)의 80%(Neutron 기본과 같다).
             */
            burst_kbit?: number;
            /**
             * Format: int32
             * @description VM이 보내는 IP 패킷에 적는 DSCP(0~63). SDN NIC만 된다.
             */
            dscp?: number;
            /**
             * Format: int32
             * @description VM이 받는 속도 상한(Mbit/s, 1~100000)
             */
            in_mbps?: number;
            /**
             * Format: int32
             * @description VM이 보내는 속도 상한(Mbit/s, 1~100000)
             */
            out_mbps?: number;
        };
        NicSpec: {
            /**
             * @description SDN 네트워크 NIC의 주소(주소 체계마다 하나까지, 계획 65). 네트워크에 서브넷이 있으면 API가 IPAM으로 채운다(MAC
             *     처럼). 비면 쓰지 않아 옛 노드도 VM을 푼다.
             */
            ips?: string[];
            mac?: components["schemas"]["MacAddr"] | null;
            model: components["schemas"]["NicModel"];
            /** @description Network 리소스 이름 */
            network: string;
            /**
             * @description 보내기·받기 속도 제한과 DSCP(계획 67). SDN NIC는 OVN `QoS` 행, 단순 NIC는 tap의 tc(속도만). 없으면 쓰지 않아 옛
             *     노드도 VM을 푼다.
             */
            qos?: components["schemas"]["NicQos"];
            /** @description 이 NIC에 거는 보안 그룹 이름(8개 이하, 겹치지 않게). 비어 있지 않으면 방화벽이 켜진다. */
            security_groups?: string[];
        };
        /**
         * @description 노드 지표 규칙의 값
         * @enum {string}
         */
        NodeAlertMetric: "cpu" | "mem_percent" | "fs_percent" | "disk_read" | "disk_write" | "net_rx" | "net_tx";
        NodeCounts: {
            /** Format: int64 */
            total: number;
            /** Format: int64 */
            witness: number;
        };
        /** @description 노드가 알리는 CPU(약 250바이트). 카탈로그 본문은 해시 이름의 [`CpuCatalog`] 리소스다. */
        NodeCpu: {
            /**
             * @description `kvm`·`hvf`·`tcg`
             * @default
             */
            accel: string;
            /**
             * @description 카탈로그 리소스 이름(SHA-256 hex). 아직 읽지 못했으면 비어 있다.
             * @default
             */
            catalog: string;
            /**
             * @description 마지막 읽기가 실패한 까닭. 앞 카탈로그는 그대로 둔다.
             * @default null
             */
            error: string;
            /**
             * @description 호스트 CPU 이름(`model-id`)
             * @default null
             */
            model_id: string;
            /**
             * Format: int64
             * @description 읽은 시각(유닉스 초)
             * @default 0
             */
            probed_at: number;
            /**
             * @description QEMU 버전
             * @default
             */
            qemu: string;
            /**
             * @description 호스트 CPU 벤더(`GenuineIntel`·`AuthenticAMD`). TCG·aarch64는 없을 수 있다.
             * @default null
             */
            vendor: string;
        };
        /** @description 노드 QEMU가 아는 플래그 하나 */
        NodeCpuFlag: {
            name: string;
            /** @description 이 가속기가 줄 수 있다(`full max`). */
            supported: boolean;
        };
        /** @description 노드의 CPU: 탐침 결과와 카탈로그를 펼친 것 */
        NodeCpuInfo: {
            arch?: components["schemas"]["Arch"];
            /** @description 이 노드가 클러스터 CPU 기준을 켤 수 없는 까닭(`cluster` VM을 받지 않는다) */
            baseline_problem?: string;
            /** @description 탐침 결과(없으면 아직 읽지 않았거나 업그레이드 중 — `host`·`max`만) */
            cpu?: components["schemas"]["NodeCpu"];
            /** @description 카탈로그의 플래그(이름순) */
            flags: components["schemas"]["NodeCpuFlag"][];
            /** @description 카탈로그의 모델(이름순, 카탈로그가 없으면 빈 것) */
            models: components["schemas"]["NodeCpuModel"][];
            node: string;
        };
        /** @description 노드 카탈로그의 모델 하나 */
        NodeCpuModel: {
            alias_of?: string;
            deprecated?: boolean;
            migration_safe: boolean;
            /** @description 이 가속기가 못 주는 플래그(끄면 켤 수 있다) */
            missing?: string[];
            /** @description 플래그 아닌 까닭(`type`, aarch64 KVM·HVF의 `accel`) */
            missing_other?: string[];
            name: string;
            /** @description 플래그를 끄지 않고 이 노드에서 켤 수 있다. */
            usable: boolean;
            vendor?: string;
        };
        /** @description 노드의 디스크 하나(`lsblk`의 최상위 디스크)와 OSD로 쓸 수 있는지 */
        NodeDisk: components["schemas"]["DiskState"] & {
            /** @description `/dev/disk/by-id/…`(재부팅해도 같은 디스크) */
            by_id?: string;
            model?: string;
            /** @description 예: `/dev/sdb` */
            path: string;
            rotational: boolean;
            serial?: string;
            /** Format: int64 */
            size_bytes: number;
        };
        /** @description 노드를 끄는 방법들. 앞의 것부터 해 보고, 꺼진 것을 확인하지 못하면 다음 것을 한다. */
        NodeFenceSpec: {
            /** @description 1~4개 */
            methods: components["schemas"]["FenceMethod"][];
        };
        /** @description 응답의 펜싱 설정. 비밀번호 대신 있는지만 보인다. */
        NodeFenceView: {
            methods: components["schemas"]["FenceMethodView"][];
            /** @description 노드 이름 */
            node: string;
            /** Format: int64 */
            revision: number;
        };
        /** @description 노드가 등록할 때 쓰는 HA 펜싱 설정 */
        NodeHa: {
            /**
             * Format: int64
             * @description 리스가 이만큼 끊기면 이 노드가 스스로 HA VM을 끈다(초, `[ha]`의 실제 값). 계획 51 전 노드는 없다.
             */
            fence_after_secs?: number | null;
            fencing: components["schemas"]["Fencing"];
            /**
             * Format: int64
             * @description 마지막 리스 갱신 뒤 이만큼 지나면 이 노드의 HA VM이 꺼졌다고 보고 리더가 옮긴다(초).
             */
            recover_after_secs: number;
        };
        /** @description 노드의 핫플러그 능력. 디스크·NIC(루트 포트)는 `hotplug` 기능이 있는 모든 노드가 하므로 적지 않는다. */
        NodeHotplug: {
            /** @description vCPU 핫플러그(x86_64만) */
            cpu?: boolean;
            /** @description virtio-mem 메모리 핫플러그(QEMU에 `virtio-mem-pci`가 있음) */
            memory?: boolean;
            /**
             * @description virtio-mem 장치가 꽂을 때 메모리를 미리 잡는다(`virtio-mem-pci`의 `prealloc`, QEMU 7.1+, 계획 64 리뷰 2). 없으면
             *     hugepages VM은 메모리 핫플러그 없이 켠다(꽂은 hugepage가 모자라면 게스트가 SIGBUS로 죽는다). 거짓이면 쓰지
             *     않는다(옛 노드와 같은 JSON).
             */
            memory_prealloc?: boolean;
        };
        /** @description 노드의 네트워크 인터페이스 하나(`ip -d addr`와 sysfs) */
        NodeInterface: {
            /** @description IP 주소(CIDR, 예: `10.0.0.5/24`) */
            addresses?: string[];
            /** @description 본딩이면 그 포트 상태 */
            bond?: components["schemas"]["BondStatus"];
            /** @description 링크가 있는지(sysfs `carrier`). 읽지 못했으면 없다. */
            carrier?: boolean;
            kind: components["schemas"]["InterfaceKind"];
            mac?: string;
            /** @description 이 인터페이스를 묶은 본딩·브리지 */
            master?: string;
            name: string;
            /**
             * Format: int32
             * @description 링크 속도(Mbps). 모르면 없다.
             */
            speed_mbps?: number;
            /** @description operstate(`up`, `down`, `unknown`, `lowerlayerdown` …) */
            state: string;
        };
        /** @description 노드 하나의 부하. 백분율은 0.1% 단위다. */
        NodeLoadView: {
            /**
             * Format: int32
             * @description 부하 요약의 나이(초). 없으면 신선한 요약이 없다.
             */
            age_secs?: number;
            /**
             * Format: int32
             * @description 지난 1분·5분 노드 CPU 사용률(요약이 없으면 없음)
             */
            cpu1_pct?: number;
            /** Format: int32 */
            cpu5_pct?: number;
            /**
             * Format: int32
             * @description 점수가 보는 CPU·메모리 사용률(요약이 없으면 다른 노드 평균 CPU·할당 메모리)
             */
            cpu_use_pct: number;
            /**
             * Format: int64
             * @description 자리 잡은 VM에 할당한 메모리
             */
            mem_allocated_mib: number;
            /** Format: int64 */
            mem_total_mib: number;
            /** Format: int32 */
            mem_use_pct: number;
            /**
             * Format: int64
             * @description 실제 사용 메모리(요약)
             */
            mem_used_mib?: number;
            name: string;
            /** Format: uuid */
            node: string;
            /**
             * Format: int32
             * @description 자리 잡은 VM 수
             */
            vms: number;
        };
        /** @description `GET /metrics/nodes`의 응답 */
        NodeMetricsResponse: {
            /** @description 읽은 노드마다 하나(노드 이름 순) */
            nodes: components["schemas"]["NodeSeries"][];
            /** @description 답하지 않은 노드의 이름(정렬) */
            unreachable: string[];
            /** @description 지표를 모르는 옛 노드의 이름(정렬) */
            unsupported: string[];
        };
        /** @description 노드를 뺐지만 손으로 할 일이 남았을 때의 `DELETE /nodes/{key}` 응답 */
        NodeRemoved: {
            warnings: string[];
        };
        /**
         * @description 노드의 역할
         * @enum {string}
         */
        NodeRole: "api";
        /** @description 노드 점 하나 */
        NodeSample: {
            /**
             * Format: double
             * @description CPU 사용률 %(모든 코어를 합해 100)
             */
            cpu?: number | null;
            /**
             * Format: double
             * @description 실제 디스크의 읽기 바이트/초(macOS는 `null`)
             */
            disk_read_bps?: number | null;
            /** Format: double */
            disk_write_bps?: number | null;
            /** Format: int64 */
            fs_total_mib?: number | null;
            /**
             * Format: int64
             * @description 데이터 디렉터리가 있는 파일 시스템
             */
            fs_used_mib?: number | null;
            /** Format: int64 */
            mem_total_mib?: number | null;
            /** Format: int64 */
            mem_used_mib?: number | null;
            /**
             * Format: double
             * @description 실제 NIC의 받기 바이트/초
             */
            net_rx_bps?: number | null;
            /** Format: double */
            net_tx_bps?: number | null;
            /**
             * Format: int64
             * @description 측정 시각(그 노드 벽시계의 유닉스 밀리초)
             */
            t: number;
        };
        /** @description 노드의 SDN 상태(`NodeStatus.sdn`). 노드의 SDN 에이전트가 바뀔 때만, 모든 노드가 `sdn`을 알릴 때만 쓴다. */
        NodeSdn: {
            /** @description 이 노드가 중앙이면 NB·SB Raft 역할과 northd */
            central?: components["schemas"]["CentralHealth"];
            /** @default missing */
            chassis: components["schemas"]["ChassisState"];
            /**
             * Format: ip
             * @description Geneve 터널의 이 노드 주소(`ovn-encap-ip`)
             */
            encap_ip?: string;
            error?: string;
            /** @description `ovn-cms-options`에 `enable-chassis-as-gw`를 적었다(계획 66). 거짓이면 쓰지 않는다. */
            gateway?: boolean;
            /** @description 이 노드의 OVN 주소 허용 목록(nftables `inet opl_ovn_<태그>`, 계획 67 §10.3). 잠금이 꺼져 있으면 없다. */
            lockdown?: components["schemas"]["LockdownNodeState"];
            /** @description `ovn-controller --version`의 판 */
            ovn_version?: string;
            /** @description `ovs-vsctl --version`의 판 */
            ovs_version?: string;
            /**
             * @description 중앙이면 NB(6641)·SB(6642)에 지금 붙어 있는 상대 주소(`ss`, 이름 순 — 계획 67 §10.4 잠금 미리 확인). 비면 쓰지
             *     않는다.
             */
            peers?: string[];
            /** @description physnet 연결(계획 66, `NodeSpec.sdn.physnets`마다 이름 순). 비면 쓰지 않는다. */
            physnets?: components["schemas"]["PhysnetState"][];
            /**
             * @description 중앙이면 NB·SB Raft(6643·6644)에 지금 붙어 있는 상대 주소(다른 중앙의 출발 주소 — 잠금 미리 확인이 중앙 집합과
             *     견준다, 리뷰 1 I5). 비면 쓰지 않는다.
             */
            raft_peers?: string[];
        };
        /** @description 노드의 SDN 설정(`NodeSpec.sdn`, 계획 66 §7). 관리자가 노드 쓰기(`cluster.admin`)로 바꾼다. */
        NodeSdnConfig: {
            /**
             * Format: int32
             * @description 게이트웨이 노드 후보의 우선순위(1~1000, 클수록 먼저). 없으면 게이트웨이가 아니다.
             */
            gateway?: number;
            /** @description physnet 이름 → 이 노드의 Linux 브리지 이름(관리자가 만든 것, 8개까지). SDN 에이전트가 veth로 OVS 브리지를 잇는다. */
            physnets?: {
                [key: string]: string;
            };
        };
        /** @description 한 노드의 점들(시각 순) */
        NodeSeries: {
            /** Format: uuid */
            node: string;
            samples: components["schemas"]["NodeSample"][];
        };
        /** @description 호스트 종료 때 노드가 남긴 표시(설계 §8 노드 정상 종료 정책) */
        NodeShutdown: {
            action: components["schemas"]["ShutdownAction"];
            /**
             * Format: int64
             * @description 표시를 남긴 시각(유닉스 초)
             */
            at: number;
            /** @description 정책(`migrate`)이 노드를 유지보수로 바꿨다. 다시 등록할 때 유지보수를 끈다. */
            maintenance?: boolean;
        };
        NodeSpec: {
            /** @description API 주소 (`host:port`) */
            address: string;
            /** @description 예약·전용 CPU와 hugepages 예약(계획 64 §7). 없으면 공유 = 모든 CPU — 지금과 같고 쓰지 않아 옛 노드도 푼다. */
            host_resources?: components["schemas"]["HostResources"];
            /** @description 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다. */
            maintenance?: boolean;
            /**
             * @description 이 노드의 역할. 없으면 기본 역할(위트니스가 아니면 `api`)이다. 옛 노드는 이 필드를 모르므로 기본이면
             *     쓰지 않는다.
             */
            roles?: components["schemas"]["NodeRole"][];
            /** @description SDN physnet 연결과 게이트웨이 노드(계획 66 §7). 없으면 쓰지 않아 옛 노드도 푼다. */
            sdn?: components["schemas"]["NodeSdnConfig"];
            /** @description 위트니스 노드: Raft 투표만 하고 VM을 돌리지 않는다. 합류할 때 정해지고 바뀌지 않는다. */
            witness?: boolean;
        };
        /** @description 노드가 시작할 때 스스로 기록한다. */
        NodeStatus: {
            /** @default null */
            arch: components["schemas"]["Arch"] | null;
            /**
             * @description 이 노드가 쓸 수 있는 백업 저장소마다 닿는지(계획 68 §6 — 60초마다 `store.json` 읽기). 비면 쓰지 않는다(옛 노드·
             *     저장소 없음). 옛 노드는 무시한다.
             */
            backup_stores?: components["schemas"]["NodeStoreState"][];
            /**
             * @description 이 노드에 있는 Linux 브리지. 스케줄러가 브리지 네트워크를 쓰는 VM을 배치할 때 본다.
             * @default []
             */
            bridges: components["schemas"]["BridgeInfo"][];
            /** @description 이 노드의 HA 검사 결과(화면용, 바뀔 때만 쓴다). 리더는 리스 갱신에 실린 결과로 판단한다. */
            checks?: components["schemas"]["CheckReport"][];
            /**
             * @description 이 노드가 원하는 클러스터 링크(`host:port`, 앞이 우선). 노드가 등록할 때 설정의 `cluster_addresses`로
             *     쓴다(옛 노드는 비어 있다). 리더가 Raft 멤버십의 링크를 이것에 맞춘다.
             */
            cluster_addresses?: string[];
            /**
             * @description 이 노드의 QEMU CPU 카탈로그(계획 63: 해시와 QEMU·가속기·호스트 CPU). 노드가 시작할 때와 QEMU가 바뀔 때 쓴다.
             *     없으면(옛 노드·위트니스) `host`·`max`만 켤 수 있다. 옛 노드는 이 필드를 무시한다.
             */
            cpu?: components["schemas"]["NodeCpu"];
            /**
             * Format: int32
             * @default 0
             */
            cpus: number;
            /**
             * @description 이 노드의 orbivisord가 아는 기능(예: `node-drain`). 노드가 등록할 때 쓴다(옛 노드는 비어 있다). 새 기능을
             *     쓰는 API는 모든 노드가 그 기능을 적었는지 본다(버전이 섞여도 된다).
             */
            features?: string[];
            /**
             * @description 이웃 노드가 이 노드의 전원 끄기 펜싱 방법으로 전원을 읽어 본 마지막 결과. 이 노드가 쓴다(`NodeFence`가
             *     없으면 None). 옛 노드는 이 필드를 무시한다.
             */
            fence_check?: components["schemas"]["FenceCheck"];
            /**
             * @description 이 노드에 VM 방화벽(nftables)이 있다. 보안 그룹을 건 NIC가 있는 VM은 이런 노드에서만 돈다
             *     (위트니스는 늘 false).
             */
            firewall?: boolean;
            /**
             * @description 이 노드가 켤 수 있는 펌웨어 장치(계획 59: swtpm·Secure Boot 펌웨어). 노드가 시작할 때와 노드 검사가 쓴다.
             *     없으면(옛 노드·위트니스) 모두 못 한다.
             */
            firmware_caps?: components["schemas"]["FirmwareCaps"];
            /** @description 이 노드의 HA 펜싱. None이면 펜싱이 꺼져 HA VM을 돌리지 않는다(위트니스는 늘 None). */
            ha?: components["schemas"]["NodeHa"];
            /** @description 이 노드의 QEMU가 할 수 있는 핫플러그(계획 54). 노드가 시작할 때 쓴다. 없으면(옛 노드·위트니스) 모두 못 한다. */
            hotplug?: components["schemas"]["NodeHotplug"];
            /**
             * @description 예: `qemu-hvf`, `qemu-kvm`, `qemu-tcg`
             * @default
             */
            hypervisor: string;
            /**
             * @description 이 노드의 네트워크 인터페이스(루프백·tap·opld가 만든 장치 빼고 64개까지). 노드가 쓴다. HA 검사를 고를 때
             *     보고, 본딩 포트 상태를 보인다. 옛 노드는 이 필드를 무시한다.
             */
            interfaces?: components["schemas"]["NodeInterface"][];
            /**
             * Format: int64
             * @default 0
             */
            memory_mib: number;
            /** @default false */
            online: boolean;
            /**
             * @description 이 노드가 쓸 수 있는 스토리지 풀(풀 이름 → 상태). 노드가 주기적으로 기록한다.
             * @default {}
             */
            pools: {
                [key: string]: components["schemas"]["PoolState"];
            };
            /**
             * @description 이 노드가 단순 NIC의 tap에 tc 속도 제한(`clsact`·police, 계획 67 §8.2)을 걸 수 있다. 검사 루프가 쓴다. 거짓이면
             *     쓰지 않는다(옛 노드·macOS·위트니스). 옛 노드는 무시한다.
             */
            qos?: boolean;
            /**
             * @description 이 노드의 SDN(OVN) chassis·중앙 상태(계획 65). 노드의 SDN 에이전트가 바뀔 때만, 모든 노드가 `sdn`을 알릴 때만
             *     쓴다. 없으면(SDN 설정 전·옛 노드·macOS·위트니스) chassis가 아니다. 옛 노드는 무시한다.
             */
            sdn?: components["schemas"]["NodeSdn"];
            /**
             * @description 호스트가 꺼지는 중이라 이 노드가 종료 정책대로 정리하며 남긴 표시. 노드가 다시 등록할 때 지운다. 리더는
             *     `freeze` 표시가 있는 오프라인 노드의 HA VM을 옮기지 않는다(옛 노드는 이 필드를 무시한다).
             */
            shutdown?: components["schemas"]["NodeShutdown"];
            /**
             * @description 이 노드의 CPU·메모리 NUMA 토폴로지(계획 64: 소켓·코어·스레드·NUMA 노드·hugepages·거리·L3). 노드가 시작할
             *     때와 검사 루프가 바뀔 때만 쓴다. 없으면(옛 노드·macOS·위트니스) 게스트 vNUMA만 된다. 옛 노드는 무시한다.
             */
            topology?: components["schemas"]["NodeTopology"];
            /**
             * Format: ip
             * @description VXLAN 패킷을 주고받는 이 노드의 주소. 없으면 이 노드에 VXLAN 장치를 만들지 않는다(위트니스는 늘
             *     None).
             */
            underlay?: string;
            /**
             * @description 이 노드의 APT 업데이트 확인 결과. 업데이트가 켜진 노드는 등록할 때 `Some`(아직 확인 전이면
             *     빈 값)이고, 꺼진 노드(위트니스, macOS 등)와 옛 노드는 None이다.
             */
            updates?: components["schemas"]["NodeUpdates"];
            /** @description 이 노드에서 도는 orbivisord의 빌드 버전. 노드가 등록할 때 쓴다(옛 버전 노드는 None). */
            version?: string;
        };
        /**
         * @description 노드 하나가 본 저장소(§6 — `NodeStatus.backup_stores`). 이 노드가 그 저장소를 쓸 수 있을 때만 60초마다
         *     `store.json`을 읽어 적는다.
         */
        NodeStoreState: {
            /**
             * Format: int64
             * @description 확인한 시각(유닉스 초)
             */
            checked_at: number;
            error?: string;
            ok: boolean;
            /**
             * Format: uuid
             * @description 저장소 리소스 id
             */
            store: string;
        };
        /** @description 노드의 CPU·메모리 토폴로지(`NodeStatus.topology`, 설계 §6). 바뀔 때만 쓰므로 빈 자원은 넣지 않는다. */
        NodeTopology: {
            /**
             * @description 온라인 CPU
             * @default
             */
            cpus: components["schemas"]["CpuSet"];
            /**
             * @description cgroup v2 `cpuset`이 있고 VM 스코프를 쓴다(`AllowedCPUs`를 건다).
             * @default false
             */
            cpuset: boolean;
            /** @description `distances[i][j]`: `nodes[i]`와 `nodes[j]` 사이 거리(`/sys/.../distance`). 비면 모른다. */
            distances?: number[][];
            /** @description 탐침 실패(있으면 나머지 칸은 믿지 않는다) */
            error?: string;
            /** @description 커널 `isolcpus` */
            isolated?: components["schemas"]["CpuSet"];
            /** @description L3 캐시를 나누는 CPU 묶음 */
            l3?: components["schemas"]["CpuSet"][];
            /** @description 호스트 NUMA 노드(id 순) */
            nodes?: components["schemas"]["HostNode"][];
            /** @description 커널 `nohz_full` */
            nohz_full?: components["schemas"]["CpuSet"];
            /** @description 커널이 아는 hugepage 크기(KiB) */
            page_sizes_kib?: number[];
            /**
             * @description vCPU 스레드를 pCPU에 고정할 수 있다(Linux).
             * @default false
             */
            pinning: boolean;
            /**
             * Format: int64
             * @description 값이 바뀐 때(유닉스 초)
             * @default 0
             */
            probed_at: number;
            /** @description SMT 형제 묶음(스레드가 둘 이상인 코어만) */
            siblings?: components["schemas"]["CpuSet"][];
            /**
             * Format: int32
             * @default 0
             */
            sockets: number;
            /**
             * Format: int32
             * @description 코어마다 가장 많은 스레드 수(SMT가 없으면 1)
             * @default 0
             */
            threads_per_core: number;
            /** @description 호스트 준비가 덜 된 것(화면에 그대로) */
            warnings?: string[];
        };
        /** @description 노드의 토폴로지와 사용 */
        NodeTopologyInfo: {
            /** @description 호스트 NUMA 노드마다 사용(id 순) */
            host_nodes: components["schemas"]["HostNodeUse"][];
            host_resources?: components["schemas"]["HostResources"];
            /** @description 노드에 직접 물은 지금 값(답이 없으면 없음) */
            live?: components["schemas"]["TopologyLive"];
            node: string;
            sets?: components["schemas"]["CpuSets"];
            /** @description 탐침 결과(Linux 노드, 모든 노드가 `numa`를 알린 뒤). 없으면 아래 칸이 빈다. */
            topology?: components["schemas"]["NodeTopology"];
            /** @description 이 노드에서 도는 묶는 VM(이름순) */
            vms: components["schemas"]["VmPlacementUse"][];
            /** @description 호스트 준비가 덜 된 것(`topology.warnings`) */
            warnings: string[];
        };
        /** @description 노드의 APT 업데이트 확인 결과(`apt-get -s dist-upgrade`가 올릴 패키지) */
        NodeUpdates: {
            /**
             * Format: int64
             * @description 마지막으로 확인한 시각(유닉스 초). 아직 확인하지 않았으면 None
             * @default null
             */
            checked_at: number | null;
            /**
             * @description 확인이 실패한 이유(명령과 오류 출력 끝). 실패하면 `packages`는 비어 있다.
             * @default null
             */
            error: string | null;
            /**
             * @description 올릴 수 있는 패키지. 너무 많으면 앞의 것만 담고 `truncated`가 true다.
             * @default []
             */
            packages: components["schemas"]["PackageUpdate"][];
            truncated?: boolean;
        };
        /** @description 노드 목록 열용 요약 */
        NodeUsageSummary: {
            /** Format: int64 */
            dedicated_total: number;
            /** Format: int64 */
            dedicated_used: number;
            /** @description 풀이 있거나 쓰는 크기만 */
            hugepages: components["schemas"]["PagesMib"][];
            node: string;
            /** @description vCPU를 고정할 수 있다(토폴로지가 있는 Linux 노드) */
            pinning: boolean;
        };
        /** @description `GET /alerts/history`의 응답 */
        NotificationHistoryResponse: {
            /** @description 새 것부터 */
            records: components["schemas"]["NotificationRecord"][];
            /** @description `limit`보다 많아 잘랐다. */
            truncated: boolean;
            unreachable: string[];
            unsupported: string[];
        };
        /** @description 보낸 알림 하나(보낸 노드의 `alerts/history.log` 한 줄) */
        NotificationRecord: {
            /**
             * Format: int64
             * @description 보낸(또는 마지막으로 해 본) 때
             */
            at: number;
            /**
             * @description 실패 이유. 보낸 쪽이 비밀을 지운 문구다(대기열이 넘쳐 버렸으면 `dropped`로 시작). admin이 아닌 사용자에게는
             *     보이지 않는다.
             */
            error?: string;
            /** @description 보낸 노드 이름 */
            node: string;
            ok: boolean;
            /** @description 규칙 이름(시험 보내기는 없음) */
            rule?: string;
            state: components["schemas"]["NotificationState"];
            /** @description 대상 이름(예: VM 이름, 시험 보내기는 없음) */
            subject?: string;
            /** @description 알림 대상 이름. admin이 아닌 사용자에게는 보이지 않는다(빈 이름이면 빼고 보낸다). */
            target?: string;
        };
        /**
         * @description 알림의 종류
         * @enum {string}
         */
        NotificationState: "firing" | "resolved" | "event" | "test";
        /**
         * @description 알림 대상 만들기·바꾸기 본문. 바꾸기는 통째로 바꾸되 비밀은 다음처럼 지킨다(영역의 `bind_password`와 같은 규칙):
         *     - SMTP `password`를 빼면 `host`·`port`·`user`가 모두 그대로일 때만 저장된 값을 두고(하나라도 바뀌면 400), 빈 글이면
         *       지운다.
         *     - webhook `url`에 응답의 줄인 URL(`scheme://host[:port]/…`)을 그대로 주면 저장된 URL을 둔다.
         *     - webhook `token`을 빼면 URL의 출처(scheme·host·port)와 `ca_pem`이 그대로일 때만 저장된 값을 두고, 빈 글이면 지운다.
         *     - webhook `headers`의 값이 빈 글이면 그 이름의 저장된 값을 둔다(출처·`ca_pem`이 그대로일 때만). 뺀 이름은 지운다.
         */
        NotificationTargetRequest: {
            /**
             * @description 끄면 보내지 않는다(시험 보내기는 된다).
             * @default true
             */
            enabled: boolean;
            /** @description 만들 때 필요하다. 바꿀 때 주면 지금 이름과 같아야 한다. */
            name?: string;
            target: components["schemas"]["TargetConfig"];
        };
        /** @description 응답의 알림 대상. 비밀(비밀번호·토큰·헤더 값·URL의 경로와 질의) 대신 있는지만 보인다. */
        NotificationTargetView: {
            enabled: boolean;
            /** Format: uuid */
            id: string;
            name: string;
            /** Format: int64 */
            revision: number;
            target: components["schemas"]["TargetConfigView"];
        };
        /** @description 시험 보내기의 결과 */
        NotificationTestResult: {
            /** @description 실패 이유(비밀을 지운 문구) */
            error?: string;
            ok: boolean;
        };
        /** @description 노드 하나가 VM을 받거나 켤 수 있는지 */
        NumaNodeFit: {
            fits: boolean;
            node: string;
            plan?: components["schemas"]["NumaPlan"];
            reason?: string;
        };
        /** @description "auto", a guest NUMA node count (1-8), or one {cpus, memory_mib} per node */
        NumaNodes: "auto" | number | {
            /**
             * Format: int32
             * @description vCPU 자리 수(핫플러그 상한 기준). 소켓당 자리 수의 배수다.
             */
            cpus: number;
            /** Format: int32 */
            memory_mib: number;
        }[];
        /** @description 노드에서 쓸 배치(묶는 VM만) */
        NumaPlan: {
            /** @description 게스트 노드마다 호스트 NUMA 노드 */
            host_nodes: number[];
            page?: components["schemas"]["PageKind"];
            /** @description 잡을 전용 pCPU(vCPU·비워 둘 형제·`isolate` 에뮬레이터, 공유 VM은 빔) */
            pcpus: components["schemas"]["CpuSet"];
        };
        /** @description VM 스펙의 CPU 배치 칸 */
        NumaSettings: {
            emulator?: components["schemas"]["EmulatorPolicy"];
            hugepages?: components["schemas"]["PageSize"];
            numa?: components["schemas"]["NumaSpec"];
            policy: components["schemas"]["CpuPolicy"];
            thread_policy?: components["schemas"]["ThreadPolicy"];
        };
        /** @description VM의 vNUMA(`VmSpec.numa`) */
        NumaSpec: {
            /**
             * @description 게스트 노드 메모리를 호스트 NUMA 노드에 묶는다. 주면 공유 VM도 묶는다. 전용·hugepages VM은 주지 않으면
             *     `strict`다.
             */
            memory?: components["schemas"]["MemoryPolicy"];
            /** @description `"auto"`, 노드 수(1~8), 또는 노드마다 `{cpus, memory_mib}` 목록 */
            nodes?: components["schemas"]["NumaNodes"];
        };
        /** @description 객체 하나의 권한 */
        ObjectPermissions: {
            /** Format: uuid */
            id: string;
            kind: string;
            privileges: string[];
        };
        /** @description OpenID Connect 제공자(IdP). 브라우저가 IdP에서 로그인하고(인증 코드 흐름과 PKCE) 돌아온다. */
        OidcRealm: {
            /** @description IdP 인증서를 확인할 CA(PEM). 없으면 시스템 루트 */
            ca_pem?: string;
            client_id: string;
            /** @description 기밀 클라이언트의 비밀. 공개 클라이언트면 없다. API는 내보내지 않는다. */
            client_secret?: string;
            /** @description 그룹(문자열 배열)을 담은 클레임 */
            groups_claim?: string;
            /** @description `https://host[:port][/path]`. discovery 문서의 `issuer`와 글자 그대로 같아야 한다. */
            issuer: string;
            /** @description 요청할 scope. `openid`가 있어야 한다. */
            scopes?: string[];
            /**
             * Format: int64
             * @description IdP 요청마다의 시간 제한(초)
             */
            timeout_secs?: number;
            /** @description 사용자 이름을 담은 클레임 */
            username_claim?: string;
        };
        /** @description [`OidcRealm`]에서 `client_secret`을 뺀 것 */
        OidcRealmView: {
            ca_pem?: string;
            client_id: string;
            groups_claim: string;
            /** @description 클라이언트 비밀이 저장되어 있는지(없으면 공개 클라이언트) */
            has_client_secret: boolean;
            issuer: string;
            scopes: string[];
            /** Format: int64 */
            timeout_secs: number;
            username_claim: string;
        };
        OsdReport: {
            /** @description 장치 클래스(`hdd`, `ssd`, `nvme`) */
            class?: string;
            /** @description 노드의 장치(예: `/dev/sdb`) */
            device?: string;
            /** @default  */
            host: string;
            /**
             * Format: int32
             * @default 0
             */
            id: number;
            /** @default false */
            in: boolean;
            /**
             * Format: int64
             * @default 0
             */
            total_bytes: number;
            /** @default false */
            up: boolean;
            /**
             * Format: int64
             * @default 0
             */
            used_bytes: number;
            /**
             * Format: double
             * @description CRUSH 무게
             * @default 0
             */
            weight: number;
        };
        OvaSource: {
            /** @description 올린 파일 이름(표시용) */
            file_name: string;
            /** Format: int64 */
            size_bytes: number;
        };
        /** @description 올릴 수 있는 패키지 하나 */
        PackageUpdate: {
            /** @description 올릴 버전 */
            candidate: string;
            /** @description 설치된 버전. 새로 설치할 패키지(새 의존성)는 None이다. */
            current?: string | null;
            name: string;
        };
        /**
         * @description hugepage 크기 하나
         * @enum {string}
         */
        PageKind: "2M" | "1G";
        /**
         * @description VM이 원하는 hugepage 크기(`VmSpec.hugepages`)
         * @enum {string}
         */
        PageSize: "2M" | "1G" | "any";
        /** @description 크기 하나의 hugepages 합(MiB) */
        PagesMib: {
            size: components["schemas"]["PageKind"];
            /** Format: int64 */
            total_mib: number;
            /** Format: int64 */
            used_mib: number;
        };
        /** @description 크기 하나의 hugepages(페이지 수) */
        PagesUse: {
            size: components["schemas"]["PageKind"];
            /**
             * Format: int64
             * @description 풀의 페이지 수(노드가 알린 예약)
             */
            total: number;
            /**
             * Format: int64
             * @description Orbivisor VM이 잡은 페이지 수
             */
            used: number;
        };
        PasskeyAdded: {
            passkey: components["schemas"]["PasskeyView"];
            /** @description 첫 요소라 새로 만든 복구 코드(한 번만 보인다). 아니면 비어 있다. */
            recovery_codes: string[];
        };
        PasskeyView: {
            /** Format: int64 */
            created_at: number;
            id: string;
            name: string;
            rp_id: string;
        };
        /** @description 백분위(최근순위) */
        Pct: {
            /** @description 5초 원본이 아니라 층의 AVG 값으로 셈한 근사(짧은 봉우리를 낮게 본다) */
            approx: boolean;
            /** Format: float */
            p50?: number | null;
            /** Format: float */
            p95?: number | null;
        };
        /** @description 실행 중에 적용하지 못해 다음 재시작 때 적용할 변경 하나 */
        PendingChange: {
            /** @description 이유의 자세한 설명(예: 실패한 까닭) */
            detail?: string;
            /**
             * @description `vcpus`, `cpu`, `cpu.max_vcpus`, `memory_mib`, `max_memory_mib`, `firmware`, `machine`, `tpm`,
             *     `secure_boot`, `guest_agent`, `numa`
             */
            field: string;
            reason: components["schemas"]["PendingReason"];
            /** @description 도는 값 */
            running: string;
            /** @description 스펙의 값 */
            wanted: string;
        };
        /**
         * @description 실행 중에 적용하지 못한 이유
         * @enum {string}
         */
        PendingReason: "restart" | "unsupported" | "limit" | "failed";
        PgReport: {
            /**
             * @description 상태(`active+clean` 등)와 그 PG 수
             * @default []
             */
            states: [
                string,
                number
            ][];
            /**
             * Format: int64
             * @default 0
             */
            total: number;
        };
        /** @description 노드의 physnet 연결 상태(`NodeSdn.physnets`, 계획 66 §7) */
        PhysnetState: {
            /** @description 이 노드의 Linux 브리지 */
            bridge: string;
            /** @description 준비되지 않은 까닭(브리지 없음, 매핑 충돌, 명령 실패) */
            error?: string;
            name: string;
            /** @description veth·OVS 브리지·`ovn-bridge-mappings`가 다 있다. */
            ready?: boolean;
        };
        /** @description 아키텍처 하나의 고정 기준 */
        PinnedBaseline: {
            arch: components["schemas"]["Arch"];
            flags?: components["schemas"]["CpuFlag"][];
            /** @description `cluster`·`host`·`max`는 안 된다. */
            model: components["schemas"]["CpuModel"];
        };
        /**
         * @description 균형 조정기의 동작
         * @enum {string}
         */
        PlacementMode: "off" | "recommend" | "auto";
        /** @description 균형 조정기의 옮기기 하나 */
        PlacementMove: {
            /**
             * Format: int64
             * @description 비용: 옮길 메모리(MiB)
             */
            cost_mib: number;
            /** Format: uuid */
            from: string;
            from_name: string;
            /**
             * Format: int32
             * @description 줄어드는 불균형(0.1% 단위)
             */
            gain_pct: number;
            /**
             * Format: uuid
             * @description 추천 id(적용할 때 쓴다)
             */
            id: string;
            reason: components["schemas"]["MoveReason"];
            /** @description 바로잡는 hard 규칙(`rule`) */
            rules?: string[];
            /**
             * Format: uuid
             * @description auto가 만든 마이그레이션 Task
             */
            task?: string;
            /** Format: uuid */
            to: string;
            to_name: string;
            /** Format: uuid */
            vm: string;
            vm_name: string;
        };
        /** @description 최적 배치에서 바꿀 값 */
        PlacementPatch: {
            /** @description 균형 조정기: `off`, `recommend`(추천만, 기본), `auto`(스스로 옮김) */
            mode?: components["schemas"]["PlacementMode"];
            /** @description `balance`(고르게, 기본)나 `pack`(적은 노드에 모음) */
            policy?: components["schemas"]["PlacementPolicy"];
            /**
             * Format: int32
             * @description 불균형 문턱(백분율 1~50, 기본 15)
             */
            threshold?: number;
        };
        /**
         * @description 배치 점수 정책
         * @enum {string}
         */
        PlacementPolicy: "balance" | "pack";
        /**
         * @description 배치 규칙: VM끼리(같은 노드·다른 노드) 또는 VM과 노드(이 노드들 중·이 노드들은 안 됨). hard 규칙은
         *     조건이고 soft 규칙은 벌점(`weight`)이다. 모든 VM에 적용한다.
         */
        PlacementRuleSpec: {
            /**
             * @description 끄면 없는 것으로 본다.
             * @default true
             */
            enabled: boolean;
            /** @description hard 규칙만: HA 복구가 지킬 수 없으면 어기고 켠다. */
            ha_may_violate?: boolean;
            kind: components["schemas"]["RuleKind"];
            /**
             * @description 이 레이블(`key=value`, 예: `zone=rack-a`)을 가진 노드들
             * @example zone=rack-a
             */
            node_tag?: string;
            /** @description VM과 노드 규칙의 노드 이름. `node_tag`와 둘 중 하나만. */
            nodes?: string[];
            /**
             * @description 이 레이블(`key=value`)을 가진 VM들
             * @example app=web
             */
            vm_tag?: string;
            /** @description 규칙의 VM 이름(아직 없는 VM이어도 된다). `vm_tag`와 둘 중 하나만. */
            vms?: string[];
            /**
             * Format: int32
             * @description soft 규칙의 벌점 1~100. 없으면 50.
             */
            weight?: number;
        };
        PlacementRuleStatus: Record<string, never>;
        /** @description 최적 배치 설정. 모든 값이 기본이면 [`SettingsSpec::placement`]를 쓰지 않는다. */
        PlacementSettings: {
            /** @description 균형 조정기의 동작 */
            mode?: components["schemas"]["PlacementMode"];
            /** @description 점수 정책 */
            policy?: components["schemas"]["PlacementPolicy"];
            /**
             * Format: int32
             * @description 불균형 문턱(정수 백분율 1~50). 노드 CPU·메모리 사용률 표준편차의 평균이 이것을 넘으면 옮기기를 찾는다.
             * @default 15
             */
            threshold: number;
        };
        /**
         * @description 최적 배치 상태(`GET /placement`): 리더가 아는 노드 부하와 균형 조정기의 마지막 결과. 리더가 만들고 다른 노드는
         *     노드 사이 RPC(`ReadPlacement`)로 읽는다.
         */
        PlacementStatus: {
            /** @description 마지막 실행의 불균형 */
            imbalance?: components["schemas"]["ImbalanceView"];
            /**
             * Format: int64
             * @description 균형 조정기가 마지막으로 돈 시각(유닉스 초)
             */
            last_run?: number;
            /**
             * Format: uuid
             * @description 이 상태를 만든 리더
             */
            leader: string;
            /**
             * Format: int64
             * @description 다음에 돌 시각(유닉스 초)
             */
            next_run?: number;
            /** @description 노드마다의 부하(이름 순) */
            nodes: components["schemas"]["NodeLoadView"][];
            /** @description 균형 조정기가 쉬는 이유 */
            paused?: string;
            /** @description 추천한(auto면 한) 옮기기 */
            recommendations: components["schemas"]["PlacementMove"][];
            settings: components["schemas"]["PlacementSettings"];
        };
        /** @description 다리 계획 */
        PlannedBridge: {
            /** @description 다리를 놓을 수 없는 까닭(그러면 옮기기를 시작하지 못한다) */
            error?: string | null;
            /** @description 다리를 놓는다. */
            needed: boolean;
            /**
             * Format: uuid
             * @description 고른 다리 노드
             */
            node?: string | null;
            warnings: string[];
        };
        /** @enum {string} */
        PlannedMode: "offline" | "live" | "restart" | "blocked";
        /** @description NIC 하나의 계획 */
        PlannedNic: {
            address_source: components["schemas"]["AddressSource"];
            blockers: string[];
            ips: string[];
            mac: string;
            mode: components["schemas"]["PlannedMode"];
            /** @description VM 이름 */
            name: string;
            nic: number;
            /** Format: uuid */
            vm: string;
            warnings: string[];
        };
        PoolBackend: {
            /** @enum {string} */
            kind: "dir";
            path: string;
        } | {
            /** @description 서버의 내보내기 경로(절대 경로) */
            export: string;
            /** @enum {string} */
            kind: "nfs";
            /**
             * @description `mount -o`에 넘길 쉼표 목록(예: `vers=4.2,hard`). 늘 붙는 `nosuid,nodev,noexec,retry=0` 뒤에
             *     붙고, 마운트의 성격을 바꾸는 옵션(`bind`, `remount`, `exec` 등)은 받지 않는다. 없으면 커널
             *     기본값.
             */
            options?: string | null;
            /** @description 호스트 이름 또는 IP. IPv6는 `[...]` 없이 적는다. */
            server: string;
        } | {
            /** @enum {string} */
            kind: "lvm-thin";
            /** @description VG 안의 thin pool LV 이름 */
            thinpool: string;
            /** @description 볼륨 그룹 이름 */
            vg: string;
        } | {
            /** @description zvol을 둘 데이터셋 이름(예: `tank/opl`) */
            dataset: string;
            /** @enum {string} */
            kind: "zfs";
        } | {
            /** @description ceph.conf 절대 경로. 없으면 Ceph 기본(`/etc/ceph/ceph.conf`). */
            conf?: string | null;
            /** @enum {string} */
            kind: "rbd";
            /** @description 풀 안의 RBD 네임스페이스. 없으면 기본 네임스페이스. */
            namespace?: string | null;
            /** @description Ceph 풀 이름 */
            pool: string;
            /** @description Ceph 클라이언트 id(`client.` 없이). 없으면 `admin`. */
            user?: string | null;
        } | {
            iscsi?: components["schemas"]["IscsiTarget"] | null;
            /** @enum {string} */
            kind: "lvm-shared";
            /** @description 공유 볼륨 그룹 이름 */
            vg: string;
        };
        /** @description 풀에서 바꿀 수 있는 값: 디렉터리 풀의 `shared`, dir·NFS 풀의 `backups`. 적지 않은 값은 그대로 둔다. */
        PoolPatch: {
            backups?: boolean;
            shared?: boolean;
        };
        PoolReport: {
            /**
             * Format: int64
             * @default 0
             */
            id: number;
            /**
             * Format: int64
             * @default 0
             */
            max_available_bytes: number;
            /** @default  */
            name: string;
            /** @description 이 Ceph 풀을 쓰는 Orbivisor 스토리지 풀 */
            orbivisor_pool?: string;
            /**
             * Format: double
             * @description 0~1
             * @default 0
             */
            percent_used: number;
            /**
             * Format: int32
             * @description 복제 수
             */
            size?: number;
            /**
             * Format: int64
             * @default 0
             */
            stored_bytes: number;
        };
        /** @description 한 노드에서 본 스토리지 풀 상태 */
        PoolState: {
            /**
             * Format: int64
             * @default 0
             */
            available_bytes: number;
            /**
             * @description 준비되지 않은 이유(예: 마운트 실패 메시지)
             * @default null
             */
            error: string | null;
            /**
             * Format: uuid
             * @description 이 상태를 적을 때의 풀 id. 같은 이름으로 다시 만든 풀에 옛 상태를 쓰지 않게, 쓰는 쪽은 이 값이
             *     풀 id와 같을 때만 믿는다. 이 필드 전의 상태는 nil이다.
             * @default 00000000-0000-0000-0000-000000000000
             */
            pool_id: string;
            /**
             * @description 이 노드에서 풀에 볼륨을 만들고 VM 디스크로 쓸 수 있는지
             * @default false
             */
            ready: boolean;
            /**
             * Format: int64
             * @default 0
             */
            total_bytes: number;
        };
        /** @description OVN의 논리 포트 상태 */
        PortLive: {
            /** @description 바인딩한 노드(이름, 모르는 chassis면 그 이름). 바인딩 전이거나 SB를 읽지 못했으면 없다. */
            node?: string;
            /** @description 포트가 바인딩되어 흐름이 깔렸다(NB `up`). */
            up: boolean;
        };
        /**
         * @description 본딩 포트의 상태
         * @enum {string}
         */
        PortState: "up" | "down" | "lacp_mismatch" | "speed_mismatch" | "unknown";
        /**
         * @description 방법으로 읽은 노드의 전원
         * @enum {string}
         */
        Power: "on" | "off" | "unknown";
        /** @enum {string} */
        PowerState: "running" | "stopped";
        /** @description 바뀌기 전 기준 */
        PreviousBaseline: {
            flags?: {
                [key: string]: boolean;
            };
            model: string;
        };
        /** @description grant를 받는 주체 */
        PrincipalRef: {
            /** Format: uuid */
            id: string;
            /** @enum {string} */
            type: "user";
        } | {
            /** Format: uuid */
            id: string;
            /** @enum {string} */
            type: "group";
        };
        /** @description Prometheus 끝점에서 바꿀 값 */
        PrometheusPatch: {
            /** @description 켜면 각 노드가 `GET /api/v1/metrics`로 지표를 내보낸다(Node·Vm 읽기 권한 토큰). 끄면 404다. */
            enabled?: boolean;
        };
        /** @description Prometheus 끝점 설정 */
        PrometheusSettings: {
            /** @description 끄면 끝점이 404다. */
            enabled?: boolean;
        };
        /** @description 요소를 빼거나 복구 코드를 다시 만들 때의 본인 확인: TOTP 코드나 복구 코드(`code`), 또는 로컬 사용자의 비밀번호 */
        ProofRequest: {
            code?: string;
            password?: string;
        };
        /** @enum {string} */
        Protocol: "tcp" | "udp" | "icmp" | "icmpv6";
        /** @description 공급자 네트워크가 붙는 물리 망(계획 66 §8) */
        Provider: {
            /** @description 물리 망 이름(`[a-z][a-z0-9-]{0,30}`). 노드마다 `NodeSpec.sdn.physnets`가 이 이름을 그 노드의 Linux 브리지에 잇는다. */
            physnet: string;
            /**
             * Format: int32
             * @description VLAN 태그(1~4094). 없으면 flat(태그 없는 프레임)이다.
             */
            vlan?: number;
        };
        /** @description 할당량 있는 폴더 하나의 상한과 하위 트리 사용량 */
        QuotaLimit: {
            /** Format: uuid */
            folder: string;
            /** @description `/teamA/prod` */
            path: string;
            quota: components["schemas"]["FolderQuota"];
            usage: components["schemas"]["FolderUsage"];
        };
        /**
         * @description ovsdb 클러스터에서 이 서버의 역할(`cluster/status`)
         * @enum {string}
         */
        RaftRole: "leader" | "follower" | "candidate" | "joining" | "down" | "unknown";
        /** @description 영역이 사용자를 확인하는 방법 */
        RealmBackend: (components["schemas"]["LdapRealm"] & {
            /** @enum {string} */
            kind: "ldap";
        }) | (components["schemas"]["OidcRealm"] & {
            /** @enum {string} */
            kind: "oidc";
        });
        RealmBackendView: (components["schemas"]["LdapRealmView"] & {
            /** @enum {string} */
            kind: "ldap";
        }) | (components["schemas"]["OidcRealmView"] & {
            /** @enum {string} */
            kind: "oidc";
        });
        /**
         * @description 로그인 화면에 보일 영역의 종류
         * @enum {string}
         */
        RealmKind: "local" | "ldap" | "oidc";
        /**
         * @description 영역 만들기·바꾸기 본문. 바꾸기는 통째로 바꾸되, `backend.bind_password`를 빼면 저장된 값을
         *     그대로 두고(`url`·`bind_dn`·`ca_pem`이 모두 그대로일 때만 — 하나라도 바뀌면 400), 빈 글이면
         *     지운다. `bind_dn`에는 비밀번호가 있어야 한다(익명 검색은 둘 다 없을 때). `url`·`bind_dn`은 앞뒤
         *     공백을 떼고 `ca_pem`은 CRLF를 LF로 바꿔 앞뒤 공백을 뗀 뒤 비교·저장한다.
         *     OIDC 영역의 `backend.client_secret`도 같다: 빼면 `issuer`·`client_id`·`ca_pem`이 모두 그대로일 때만
         *     저장된 값을 두고(저장된 비밀이 없으면 공개 클라이언트로 남는다), 빈 글이면 지운다. `issuer`·`client_id`는
         *     앞뒤 공백을 떼고 `ca_pem`은 LDAP처럼 다듬는다.
         */
        RealmRequest: {
            backend: components["schemas"]["RealmBackend"];
            /** @description 로그인 화면에 보일 이름 */
            display?: string;
            /** @description 만들 때 필요하다. 바꿀 때 주면 지금 이름과 같아야 한다. */
            name?: string;
            roles?: components["schemas"]["RoleMapping"];
        };
        /** @description 로그인 화면에 보일 영역 */
        RealmSummary: {
            /** @description 없으면 null */
            display: string | null;
            kind: components["schemas"]["RealmKind"];
            name: string;
        };
        /**
         * @description 영역 시험 본문. LDAP 영역은 사용자 이름과 비밀번호가 필요하다. OIDC 영역은 본문 없이 부르고
         *     (있으면 무시한다) IdP의 discovery와 JWKS만 확인한다.
         */
        RealmTestRequest: {
            password?: string;
            username?: string;
        };
        /**
         * @description 영역 시험의 결과. 단계는 LDAP이면 `connect`, `service_bind`, `search`, `user_bind`, `groups`,
         *     OIDC면 `discovery`, `jwks` 순이고 실패한 단계에서 멈춘다.
         */
        RealmTestResult: {
            /** @description 찾은 사용자 항목. 찾기 전에 멈췄거나 OIDC 영역이면 null */
            dn: string | null;
            /** @description 모든 단계가 성공했을 때의 그룹 속성 값들(OIDC 영역은 비어 있다) */
            groups: string[];
            role: components["schemas"]["Role"] | null;
            steps: components["schemas"]["RealmTestStep"][];
        };
        RealmTestStep: {
            message: string;
            name: string;
            ok: boolean;
        };
        /** @description 응답의 영역. 서비스 계정 비밀번호·클라이언트 비밀 대신 있는지만 보인다. */
        RealmView: {
            backend: components["schemas"]["RealmBackendView"];
            display?: string;
            /** Format: uuid */
            id: string;
            name: string;
            /** Format: int64 */
            revision: number;
            roles: components["schemas"]["RoleMapping"];
        };
        ReceiveRequest: {
            /** @description 저장소 사본의 VM 칸(매니페스트 v3에는 없고 암호화된 `config` 블롭에 있다). 저장소면 있어야 한다. */
            config?: components["schemas"]["StoreCopyConfig"];
            /** @description 원본 백업의 매니페스트: 풀이면 `manifest.json`(버전 1·2), 저장소면 `index.json`(버전 3) */
            manifest: Record<string, never>;
            /** @description 받는 쪽 백업 이름. 없으면 `{VM 이름}-{만든 시각}`이고, 겹치면 뒤에 `-2`, `-3`…을 붙인다. */
            name?: string | null;
            origin: components["schemas"]["BackupOrigin"];
            /**
             * Format: uuid
             * @description 증분 백업이면 받는 쪽(이 클러스터)에 있는 부모 사본의 id. 전체 백업이면 뺀다.
             */
            parent?: string | null;
            /** @description 받을 백업 풀. 이 노드에서 쓸 수 있어야 한다. `store`와 둘 중 하나다. */
            pool?: string;
            /** @description 받을 백업 저장소(계획 68 §12.4 — 저장소 백업의 사본). 이 노드에서 쓸 수 있어야 한다. */
            store?: string;
        };
        /** @description 받은 파일 */
        ReceivedFile: {
            /** @description 받은 내용의 SHA-256(16진수 소문자) */
            sha256: string;
            /** Format: int64 */
            size: number;
        };
        /** @description 받기 시작한 백업 */
        ReceivingBackup: {
            /** Format: uuid */
            id: string;
            name: string;
        };
        /** @description 새 복구 코드(한 번만 보인다). 이미 남은 코드가 있어 새로 만들지 않았으면 비어 있다. */
        RecoveryCodes: {
            codes: string[];
        };
        /** @description 복구 키 */
        RecoveryKey: {
            recovery_key: string;
        };
        /** @description Redfish BMC. Basic 인증으로 `ComputerSystem.Reset`(`ForceOff`)을 부르고 `PowerState`로 확인한다. */
        RedfishFence: {
            /** @description BMC 인증서를 확인할 CA(PEM). 없으면 시스템 루트 */
            ca_pem?: string;
            /** @description BMC 인증서를 확인하지 않는다(자체 서명 인증서). */
            insecure?: boolean;
            /** @description BMC 비밀번호. API는 내보내지 않는다. */
            password?: string;
            /** @description `/redfish/v1/Systems/<id>`의 id. 없으면 시스템이 하나일 때 그것을 쓴다. */
            system_id?: string;
            /** @description `https://bmc[:port]`(`http://`도 된다) */
            url: string;
            user: string;
        };
        /** @description [`RedfishFence`]에서 `password`를 뺀 것 */
        RedfishFenceView: {
            ca_pem?: string;
            /** @description 비밀번호가 저장되어 있는지 */
            has_password: boolean;
            insecure: boolean;
            system_id?: string;
            url: string;
            user: string;
        };
        RegistrationCredential: {
            /** @description 자격 증명 id(base64url) */
            id: string;
            response: components["schemas"]["AttestationResponse"];
        };
        /** @description 패스키 등록 시작: 브라우저에 넘길 옵션과, 끝낼 때 돌려보낼 서명한 토큰(5분) */
        RegistrationStart: {
            options: components["schemas"]["CreationOptions"];
            token: string;
        };
        RelyingParty: {
            id: string;
            name: string;
        };
        RemoteNodeCounts: {
            /** Format: int64 */
            online: number;
            /** Format: int64 */
            total: number;
        };
        RemoteProbe: {
            /** @description 받은 CA 인증서의 지문. 원격 클러스터에서 보이는 지문과 같은지 사람이 확인해야 한다. */
            ca_fingerprint: string;
            /** @description 정규화한 URL */
            url: string;
        };
        RemoteProbeRequest: {
            /** @description `https://<호스트>[:포트]` */
            url: string;
        };
        /**
         * @description 원격 만들기·바꾸기 본문. 바꾸기는 통째로 바꾸되, `token`을 빼면 저장된 토큰을 그대로 둔다
         *     (`url`·`ca_fingerprint`가 모두 그대로일 때만 — 하나라도 바뀌면 400). 저장하기 전에 원격에 연결해
         *     CA 지문과 토큰을 확인한다(30초 제한). 확인한 CA 인증서는 저장해 두고 연결할 때 다시 받지 않으며,
         *     `url`이나 `ca_fingerprint`가 바뀔 때만 다시 받는다.
         */
        RemoteRequest: {
            /** @description 원격 클러스터 CA의 지문(`sha256:<16진수>` — 대문자·`:` 구분도 받는다) */
            ca_fingerprint: string;
            /** @description 만들 때 필요하다. 바꿀 때 주면 지금 이름과 같아야 한다. */
            name?: string;
            /** @description 원격 클러스터에서 만든 API 토큰. 만들 때 필요하다. */
            token?: string;
            /** @description `https://<호스트>[:포트]` */
            url: string;
        };
        /** @description 리더가 주기적으로 모은 원격 클러스터의 상태. 모으지 못하면 `reachable`이 false이고 `error`에 이유가 있다. */
        RemoteStatus: {
            /**
             * Format: int64
             * @description 상태가 마지막으로 바뀐 시각(유닉스 초). 같은 상태면 다시 쓰지 않으므로 마지막 확인 시각이 아니다.
             * @default null
             */
            checked_at: number | null;
            /** @default null */
            cluster_name: string | null;
            /**
             * Format: int64
             * @description 위트니스를 뺀 노드의 CPU 합
             * @default null
             */
            cpus: number | null;
            /**
             * @description 닿지 못한 이유
             * @default null
             */
            error: string | null;
            /**
             * Format: int64
             * @description 위트니스를 뺀 노드의 메모리 합(GiB)
             * @default null
             */
            memory_gib: number | null;
            /** @default null */
            nodes: components["schemas"]["RemoteNodeCounts"] | null;
            /** @default false */
            reachable: boolean;
            /**
             * @description 노드 버전 가운데 가장 낮은 것
             * @default null
             */
            version: string | null;
            /** @default null */
            vms: components["schemas"]["RemoteVmCounts"] | null;
        };
        /** @description 응답의 원격. 토큰 대신 토큰의 id만 보인다(원격에서 어느 토큰인지 알아보게). */
        RemoteView: {
            ca_fingerprint: string;
            /** Format: uuid */
            id: string;
            name: string;
            /** Format: int64 */
            revision: number;
            status: components["schemas"]["RemoteStatus"];
            /**
             * Format: uuid
             * @description 저장한 API 토큰의 id
             */
            token_id: string | null;
            url: string;
        };
        RemoteVmCounts: {
            /** Format: int64 */
            running: number;
            /** Format: int64 */
            total: number;
        };
        /** @description HA가 VM을 복제본으로 다시 켠 마지막 복구(VM `status.last_recovery`). 다음 복구가 덮고, 그 뒤 복제가 성공해도 남는다. */
        ReplicaRecovery: {
            /**
             * Format: int64
             * @description 복구한 때(유닉스 초)
             */
            at: number;
            /**
             * Format: uuid
             * @description 죽은 원본 노드
             */
            from: string;
            /**
             * Format: uuid
             * @description 복제본을 승격해 VM을 켠 노드
             */
            node: string;
            /**
             * Format: int64
             * @description 그 복제본의 시점(유닉스 초). 그 뒤 원본에 쓴 것은 잃었다.
             */
            synced_at: number;
        };
        /** @enum {string} */
        ReplicaState: "copying" | "ready" | "failed";
        /** @description VM `status.replication`의 대상 하나 */
        ReplicaStatus: {
            /** @description 마지막 회차가 실패한 이유. 성공하면 지운다. */
            error?: string;
            /**
             * Format: uuid
             * @description 마지막으로 끝난 회차의 id(그 회차 Task의 id)
             */
            generation?: string;
            /**
             * Format: int64
             * @description 마지막 회차가 보낸 바이트
             */
            last_bytes?: number;
            /** @description 마지막 회차가 전체 복사였는지 */
            last_full?: boolean;
            /**
             * Format: int64
             * @description 마지막 회차가 걸린 시간(초)
             */
            last_secs?: number;
            /**
             * Format: int64
             * @description 마지막으로 회차를 시작한 때(유닉스 초)
             */
            last_try?: number;
            /**
             * Format: uuid
             * @description 대상 노드의 id
             */
            node: string;
            /** @description HA 복구가 이 복제본으로 VM을 켜라고 한 표시. 대상 노드가 복제본을 승격한 뒤 지운다. */
            promote?: boolean;
            state: components["schemas"]["ReplicationState"];
            /**
             * Format: int64
             * @description 그 회차의 시점(유닉스 초)
             */
            synced_at?: number;
            /** @description 그 회차가 담은 로컬 디스크 */
            volumes?: components["schemas"]["ReplicaVolume"][];
        };
        /** @description 복제 회차가 담은 로컬 디스크 하나 */
        ReplicaVolume: {
            /**
             * Format: uuid
             * @description 볼륨 id(복제본도 같은 id다)
             */
            id: string;
            /**
             * Format: int64
             * @description 그 회차의 볼륨 크기(볼륨의 `spec.size_bytes` — 백엔드가 맞춘 이미지의 가상 크기가 아니다)
             */
            size_bytes: number;
        };
        ReplicateRequest: {
            /** @description 원격 클러스터의 백업 풀 이름 */
            pool: string;
            /** @description 사본을 보낼 원격 클러스터(이름이나 id) */
            remote: string;
        };
        /** @description 백업을 보낼 곳: 원격 `remote`의 백업 풀 `pool` */
        ReplicateTarget: {
            /** @description 원격의 백업 풀 이름 */
            pool: string;
            /** @description 원격 이름 */
            remote: string;
        };
        /**
         * @description 복제 대상 하나의 상태
         * @enum {string}
         */
        ReplicationState: "pending" | "ready" | "syncing" | "stale";
        /** @description VM의 복제 대상 하나 */
        ReplicationTarget: {
            /**
             * Format: uuid
             * @description 복제본을 둘 노드의 id
             */
            node: string;
            /** @description 그 노드에서 이 VM의 로컬 디스크를 모두 둘 풀. 없으면 디스크마다 같은 이름의 풀이다. */
            pool?: string;
            /**
             * @description 주기 `*\/N`(분). 앞 시도를 시작한 때에서 N분 뒤에 다시 한다(시계에 맞추지 않는다).
             * @default *\/15
             */
            schedule: string;
        };
        /** @description `PublicKeyCredentialRequestOptions`(이진 값은 base64url) */
        RequestOptions: {
            allowCredentials: components["schemas"]["CredentialDescriptor"][];
            challenge: string;
            rpId: string;
            /** Format: int64 */
            timeout: number;
            userVerification: string;
        };
        ResizeRequest: {
            /**
             * Format: int64
             * @description 새 크기(바이트). 지금보다 크고 512의 배수여야 한다.
             */
            size_bytes: number;
        };
        /**
         * @description 켤 때 실제로 쓴 CPU: QEMU `-cpu <model>[,enforce][,<flag>=on|off]…`. `RunningHardware.cpu_resolved`에 기록하고
         *     받는 쪽 마이그레이션은 이것을 그대로 쓴다.
         */
        ResolvedCpu: {
            /**
             * @description 켤 때 이 노드의 QEMU가 몰라 뺀 클러스터 숨김 이름(`host`·`max`, 리뷰 2). 그 QEMU는 그 기능을 주지 못해 숨긴 것과
             *     같지만, 그 이름을 아는 QEMU로 옮기거나(받는 쪽은 이 기록 그대로 켠다) 메모리 스냅샷을 되돌리면 드러난다 —
             *     마이그레이션 확인과 되돌리기가 이것으로 막는다.
             */
            dropped_mask?: string[];
            /** @description 가속기가 못 주는 기능이 있으면 켜지 않는다(명명 모델·수준). */
            enforce?: boolean;
            /** @description 켜고(참) 끈(거짓) 플래그 */
            flags?: {
                [key: string]: boolean;
            };
            /** @description QEMU `-cpu` 모델 이름(`host`, `max`, `qemu64-v1`, `Icelake-Server-v7` …) */
            model: string;
            source?: components["schemas"]["CpuSource"];
        };
        /**
         * @description 켤 때 정한 게스트의 NUMA 모양(`RunningHardware.numa`). 게스트가 보는 것과 마이그레이션 양쪽이 같아야 하는 것이라
         *     라이브 마이그레이션 동안 바꾸지 않는다.
         */
        ResolvedNuma: {
            /** @description 게스트 노드 사이 거리(`-numa dist`). 비면 QEMU 기본(10·20)이다. */
            distances?: number[][];
            /** @description 전용 VM의 에뮬레이터 정책(기본을 채운 것). 공유 VM은 없다. */
            emulator?: components["schemas"]["EmulatorPolicy"];
            /** @description 호스트 노드에 묶는 방법. 없으면 묶지 않는다(공유 vNUMA VM). */
            memory?: components["schemas"]["MemoryPolicy"];
            /** @description 게스트 노드(순번 = QEMU `nodeid`) */
            nodes: components["schemas"]["GuestNode"][];
            /** @description hugepage 크기. 없으면 보통 페이지다. */
            page?: components["schemas"]["PageKind"];
            policy?: components["schemas"]["CpuPolicy"];
            /** @description 전용 VM의 스레드 정책(기본을 채운 것). 공유 VM은 없다. */
            thread_policy?: components["schemas"]["ThreadPolicy"];
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: components["schemas"]["GrantSpec"];
            status: components["schemas"]["GrantStatus"];
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_AlertRuleSpec_AlertRuleStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            /** @description 경보 규칙 하나 */
            spec: {
                condition: components["schemas"]["AlertCondition"];
                /**
                 * @description 끄면 평가하지 않고, 활성 경보는 알림 없이 해제한다.
                 * @default true
                 */
                enabled: boolean;
                /**
                 * Format: int32
                 * @description 조건이 이만큼(초) 참이어야 알린다(0~86400). 이벤트 조건은 0이다.
                 * @default 0
                 */
                for_secs: number;
                /** @description 이 노드(이름)들만. 비면 모든 노드 */
                nodes?: string[];
                /**
                 * Format: int32
                 * @description 경보가 이어지는 동안 이만큼(초)마다 다시 알린다. 0이면 다시 보내지 않는다.
                 * @default 14400
                 */
                repeat_secs: number;
                /**
                 * @description 해제도 알린다(이벤트 조건은 해제가 없다).
                 * @default true
                 */
                send_resolved: boolean;
                severity: components["schemas"]["Severity"];
                /** @description 알릴 `NotificationTarget` 이름(1~16개) */
                targets: string[];
                /** @description 이 VM(이름)들만(VM 조건만). 비면 모든 VM */
                vms?: string[];
            };
            /** @description 리더가 평가하는 규칙(`NodeOffline`·`BackupJobFailed`)의 활성 경보만 둔다. 노드 경보는 각 노드 파일에 있다. */
            status: {
                active?: components["schemas"]["ActiveAlert"][];
                /**
                 * @description `BackupJobFailed`: 백업 작업(id)마다 이미 알린 마지막 실패 실행의 끝난 시각(ms). 리더가 바뀌어도 같은 실행을 다시
                 *     알리지 않는다.
                 */
                notified?: {
                    [key: string]: number;
                };
            };
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_BackupJobSpec_BackupJobStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                /** @description 작업의 백업이 `ready`가 되면 다른 저장소로 복사본을 보낸다(계획 68 §12.5 — 계층). 없으면 쓰지 않는다. */
                copy_to?: components["schemas"]["CopyTarget"];
                /**
                 * @description 끄면 예약 실행을 하지 않는다.
                 * @default true
                 */
                enabled: boolean;
                /**
                 * Format: int32
                 * @description 0보다 크면 증분으로 백업하되(할 수 있으면) 체인의 증분이 이만큼 쌓이면 다음은 전체로 한다.
                 *     0이면 늘 전체다(뺀다).
                 *     저장소 작업(`store`)에서는 "n번 연속 비트맵으로 읽었으면 다음은 디스크 전체를 읽기"다(계획 68 §8.3 — 저장 공간은
                 *     늘지 않는다).
                 */
                full_every?: number;
                /** @description 보존 규칙. 모두 0이면 지우지 않는다. */
                keep?: components["schemas"]["Keep"];
                /**
                 * @description 백업 풀 이름. 백업 저장소(`store`, 계획 68)로 백업하는 작업이면 비우고 쓰지 않는다(옛 JSON에는 늘 있으므로
                 *     바이트 그대로다). `pool`과 `store` 중 정확히 하나다.
                 */
                pool?: string;
                /** @description 만든 백업을 보낼 원격 클러스터의 백업 풀. 없으면 보내지 않는다. */
                replicate?: components["schemas"]["ReplicateTarget"];
                schedule: components["schemas"]["JobSchedule"];
                /** @description 백업 저장소 이름(계획 68). 없으면 쓰지 않는다. */
                store?: string;
                /** @description 백업할 VM */
                vms: components["schemas"]["VmSelector"];
            };
            status: {
                /**
                 * @description 마지막(또는 진행 중인) 실행에서 백업하지 못한 VM과 이유
                 * @default []
                 */
                errors: components["schemas"]["JobError"][];
                /** @default null */
                last_run: components["schemas"]["JobRunResult"] | null;
                /**
                 * Format: int64
                 * @description 다음 예약 실행 시각(유닉스 초)
                 * @default null
                 */
                next_run: number | null;
                /** @default null */
                run: components["schemas"]["JobRun"] | null;
            };
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_BackupSpec_BackupStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                /**
                 * @description 할 수 있으면 증분으로 백업한다(바로 앞 백업을 부모로 바뀐 블록만). 할 수 없으면 전체로 하고 그
                 *     이유를 `status.full_reason`에 적는다. false면 뺀다.
                 */
                incremental?: boolean;
                /**
                 * Format: uuid
                 * @description 이 백업을 만든 백업 작업 id. 손으로 만든 백업이면 없다.
                 */
                job?: string | null;
                /**
                 * Format: int32
                 * @description 증분 체인에 둘 증분 백업 수의 한도(백업 작업의 `full_every`). 부모의 체인에 이미 이만큼 증분이
                 *     있으면 전체로 한다. 없으면 한도가 없다(손으로 만든 백업).
                 */
                max_chain?: number;
                /** @description 다른 클러스터에서 받은 사본이면 그 원본. 이 클러스터에서 만든 백업이면 없다. */
                origin?: components["schemas"]["BackupOrigin"];
                /**
                 * @description 백업 풀 이름. 저장소 백업(`store`, 계획 68)이면 비우고 쓰지 않는다(옛 JSON에는 늘 있으므로 바이트 그대로다).
                 *     `pool`과 `store` 중 정확히 하나다([`BackupSpec::target`]).
                 */
                pool?: string;
                /**
                 * @description 옛 백업을 옮기거나(`convert`) 다른 저장소에서 복사해(`copy`) 만든 저장소 백업의 원본(계획 68 §12.2·§12.5). 없으면
                 *     쓰지 않는다.
                 */
                source?: components["schemas"]["BackupSource"];
                /** @description 백업 저장소 이름(계획 68). 없으면 쓰지 않는다. */
                store?: string;
                /**
                 * Format: uuid
                 * @description VM id. VM을 지워도 백업은 남는다.
                 */
                vm: string;
                /** @description 만든 때의 VM 이름(표시용) */
                vm_name: string;
                /** @description 만든 때의 VM 스펙 */
                vm_spec: components["schemas"]["VmSpec"];
            };
            status: {
                /**
                 * Format: uuid
                 * @description 저장소 백업(계획 68)에서 바뀌지 않은 청크를 가져온 앞 백업. 계획 40의 `parent`와 달리 복원은 이것을 따라가지
                 *     않는다(인덱스가 늘 완전하다). 없으면 쓰지 않는다.
                 */
                base?: string;
                /**
                 * Format: int64
                 * @description 만든 시각(유닉스 초)
                 * @default null
                 */
                created_at: number | null;
                /** @default [] */
                disks: components["schemas"]["BackupDisk"][];
                /**
                 * @description 실패한 이유
                 * @default null
                 */
                error: string | null;
                /** @description 증분을 요청했지만 전체로 한 이유. 없으면 뺀다. */
                full_reason?: string;
                /**
                 * Format: uuid
                 * @description 파일을 쓴 노드. 공유가 아닌 풀의 백업은 이 노드에만 있으므로 지우기·복원도 이 노드에서 한다.
                 *     공유 풀이면 없다.
                 */
                node?: string;
                /**
                 * Format: uuid
                 * @description 증분 백업의 부모(같은 풀의 바로 앞 백업). 전체 백업이면 뺀다.
                 */
                parent?: string;
                /** @default pending */
                phase: components["schemas"]["BackupPhase"];
                /**
                 * Format: int64
                 * @description 받는 중인 사본이 마지막으로 파일을 받은 시각(유닉스 초). 받은 적이 없으면 뺀다.
                 */
                received_at?: number;
                /** @description 원격 클러스터에 보낸 사본. 없으면 뺀다. */
                replicas?: components["schemas"]["BackupReplica"][];
                /** @description 저장소 백업이 읽고 쓴 양(계획 68). 없으면 쓰지 않는다. */
                store_stats?: components["schemas"]["StoreBackupStats"];
                /** @description TPM 상태 파일(`tpm2-00.permall`)을 담았는지(계획 59). 거짓이면 뺀다. */
                tpm_state?: boolean;
                /**
                 * @description UEFI 변수 파일(`vars.fd`)을 담았는지
                 * @default false
                 */
                uefi_vars: boolean;
                /** @description 마지막 검증 결과(계획 68). 검증한 적이 없으면 쓰지 않는다. */
                verify?: components["schemas"]["BackupVerify"];
            };
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_BackupStoreSpecView_BackupStoreStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            /** @description 비밀(S3 비밀 키·주 키)을 뺀 저장소 스펙 */
            spec: {
                backend: components["schemas"]["StoreBackendView"];
                encryption?: components["schemas"]["StoreEncryptionView"];
                gc: components["schemas"]["JobSchedule"];
                /** Format: int32 */
                max_streams: number;
                read_only?: boolean;
                verify?: components["schemas"]["VerifyPolicy"];
            };
            /** @description 저장소의 실제 상태. 초기화·GC·검증 Task와 리더의 스케줄러가 쓴다. */
            status: {
                /**
                 * Format: int64
                 * @description 검증이 찾아 `bad/`로 옮긴 청크 수
                 */
                bad_chunks?: number;
                /** @description 매니페스트를 쓰는 중인 백업(쓰기 잠금 — `sweeping`이 거짓일 때만 더한다, §10.3) */
                committing?: string[];
                error?: string;
                /** @description 진행 중인 GC(§10.2) */
                gc?: components["schemas"]["GcRun"];
                last_gc?: components["schemas"]["GcResult"];
                last_verify?: components["schemas"]["VerifyResult"];
                /**
                 * Format: int64
                 * @description 다음 예약 GC 시각(유닉스 초)
                 */
                next_gc?: number;
                /**
                 * Format: int64
                 * @description 다음 예약 검증 시각(유닉스 초)
                 */
                next_verify?: number;
                /**
                 * Format: uuid
                 * @description 공유가 아닌 백업 풀 안 저장소를 만든 노드(그 노드에만 있다 — §7.6). 공유 풀·S3면 없다.
                 */
                node?: string;
                /** @description 저장소 `owner.json`의 주인 클러스터(§12.6) */
                owner?: components["schemas"]["StoreOwner"];
                /** @default initializing */
                phase: components["schemas"]["StorePhase"];
                /**
                 * @description 검증이 깨진 청크를 `bad/`로 옮기는 중(리뷰 1 I5 — 쓸기 잠금처럼 `committing`이 비었을 때만 참이 되고 그동안 커밋
                 *     구간에 새로 들지 않는다)
                 */
                quarantining?: boolean;
                /**
                 * Format: uuid
                 * @description 저장소 `store.json`의 id. 이 클러스터가 만든 저장소는 리소스 id, 붙인 저장소는 남의 것이다.
                 */
                store_id?: string;
                /**
                 * Format: int64
                 * @description 쓸기를 시작할 때마다 1씩 오른다. 백업이 시작과 커밋 때 견준다.
                 */
                sweep_epoch?: number;
                /** @description GC가 청크를 지우는 중(쓰기 잠금 — `committing`이 비었을 때만 참이 된다) */
                sweeping?: boolean;
                /** @description 마지막 GC가 센 사용량 */
                usage?: components["schemas"]["StoreUsage"];
            };
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_FloatingIpSpec_FloatingIpStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                /**
                 * Format: ip
                 * @description 유동 IP(IPv4). API가 그 네트워크의 유동 IP 풀(없으면 풀)에서 채운다.
                 */
                address?: string;
                /** @description 200자 이하 */
                description?: string;
                /**
                 * Format: uuid
                 * @description 든 네트워크 폴더(`Folder` id). 없으면 네트워크 트리의 루트다.
                 */
                folder?: string;
                /** @description 공급자 네트워크 이름 */
                network: string;
                /** @description 붙은 VM NIC. 없으면 떼어 둔 것이다. */
                target?: components["schemas"]["FipTarget"];
            };
            status: Record<string, never>;
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_FolderSpec_FolderStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                /** @description 이 폴더가 속한 트리 */
                kind: components["schemas"]["FolderKind"];
                /** @description 화면 이름. 형제 사이에서 대소문자 무시로 겹치지 않는다(상태 머신이 막는다). */
                name: string;
                /**
                 * Format: uuid
                 * @description 부모 폴더. 없으면 그 종류의 루트 바로 아래다. 부모는 같은 종류여야 한다.
                 */
                parent?: string;
                /** @description 하위 트리 할당량. VM 폴더(VM 차원)와 네트워크 폴더(`routers`·`floating_ips`, 계획 66)에만 둔다. 없으면 상한이 없다. */
                quota?: components["schemas"]["FolderQuota"];
            };
            status: Record<string, never>;
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_GrantSpec_GrantStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            /** @description 권한 하나(`Kind::Grant`): 주체에게 대상에서 역할을 준다. */
            spec: {
                principal: components["schemas"]["PrincipalRef"];
                /**
                 * @description 하위 폴더와 그 안의 객체로 내려가는지. 거짓이면 대상 자체(폴더 이름 바꾸기·지우기·그 폴더의 권한 보기)에만
                 *     닿는다. 기본은 참이고 참이면 쓰지 않는다.
                 * @default true
                 */
                propagate: boolean;
                role: components["schemas"]["RoleRef"];
                target: components["schemas"]["GrantTarget"];
            };
            status: Record<string, never>;
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_GroupSpec_GroupStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            /** @description 로컬 그룹(`Kind::Group`). 외부 사용자는 그 영역의 `external_groups`에 `external`의 그룹이 있으면 든다. */
            spec: {
                description?: string;
                /** @description 연결한 디렉터리 그룹 */
                external?: components["schemas"]["ExternalGroup"][];
                /** @description 멤버 사용자 id(로컬·외부) */
                members?: string[];
            };
            status: Record<string, never>;
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_HaCheckSpec_HaCheckStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            /** @description 검사 하나 */
            spec: {
                /**
                 * @description 끄면 노드가 검사하지 않는다.
                 * @default true
                 */
                enabled: boolean;
                /**
                 * Format: int32
                 * @description 이만큼(초) 넘게 실패해야 대응한다. 10~600
                 * @default 10
                 */
                grace_secs: number;
                /** @description 노드 이름 → 그 노드의 대상(종류는 `target`과 같아야 한다) */
                nodes?: {
                    [key: string]: components["schemas"]["HaCheckTarget"];
                };
                /**
                 * @description 실패했을 때의 대응. 없으면 역할의 기본(`storage` → `restart_elsewhere`, `vm` → `migrate`,
                 *     `management` → `warn`)
                 */
                on_fail?: components["schemas"]["HaCheckAction"];
                /** @description 검사하는 네트워크·장치의 쓰임. 기본 대응과 영향받는 VM을 정한다. */
                role: components["schemas"]["HaCheckRole"];
                /** @description 모든 노드의 검사 대상(노드별 덮기가 없을 때) */
                target: components["schemas"]["HaCheckTarget"];
            };
            status: Record<string, never>;
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_HaGroupSpec_HaGroupStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            /** @description HA 그룹: HA VM을 복구할 노드와 그 우선순위. VM은 `ha.group`으로 그룹을 쓴다. */
            spec: {
                /** @description 그룹 노드. 우선순위가 큰 노드에 먼저 놓는다. */
                nodes: components["schemas"]["HaGroupNode"][];
                /** @description 켜면 우선순위가 더 높은 노드가 돌아와도 되돌아가지 않는다. */
                nofailback?: boolean;
                /** @description 켜면 그룹 밖 노드에는 놓지 않는다(놓을 곳이 없으면 켜지 않는다). */
                restricted?: boolean;
            };
            status: Record<string, never>;
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_LoadBalancerSpec_LoadBalancerStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                /**
                 * Format: int32
                 * @description 같은 클라이언트를 이 시간(초, 1~65535) 동안 같은 멤버로 보낸다. 없으면 연결마다 해시다.
                 */
                affinity_secs?: number;
                /** @description 200자 이하 */
                description?: string;
                /** @description 바깥 VIP(VIP 네트워크 라우터의 바깥 공급자 네트워크). 없으면 안쪽에서만 닿는다. */
                external?: components["schemas"]["LbExternal"];
                /**
                 * Format: uuid
                 * @description 든 네트워크 폴더(`Folder` id). 없으면 네트워크 트리의 루트다.
                 */
                folder?: string;
                /** @description 리스너(1~16개, 같은 프로토콜·포트는 한 번) */
                listeners: components["schemas"]["LbListener"][];
                /**
                 * @description 건강 검사 출발 주소(건강 검사가 있는 NIC 멤버의 네트워크마다 하나). API가 그 서브넷 풀에서 채운다(사용자가 주지
                 *     않는다).
                 */
                monitors?: components["schemas"]["LbMonitor"][];
                /** @description VIP 네트워크(IPv4 서브넷이 있고 라우터 인터페이스인 오버레이 SDN 네트워크) 이름 */
                network: string;
                /**
                 * Format: ipv4
                 * @description VIP(IPv4). API가 그 서브넷 풀에서 채운다(직접 주면 풀 안).
                 */
                vip?: string;
            };
            /** @description 지금은 칸이 없다(멤버 건강·VIP 상태는 보기가 SB·NB에서 읽는다). */
            status: Record<string, never>;
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_NetworkMigrationSpec_NetworkMigrationStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                /** @description 사용자가 주는 NIC 주소(게스트 에이전트·cloud-init보다 앞선다) */
                addresses?: components["schemas"]["MigrationAddress"][];
                /** @description 게스트가 볼 MTU가 줄어도 `live`로 옮긴다(아니면 `restart`). */
                allow_mtu_shrink?: boolean;
                /** @description 옮기는 동안 L2 다리를 놓는지. 없으면 대상이 오버레이면 참이다. */
                bridge?: boolean;
                /**
                 * Format: uuid
                 * @description 다리 노드. 없으면 원본 브리지가 있는 chassis 가운데 옮길 VM이 가장 많은 노드다.
                 */
                bridge_node?: string;
                /**
                 * Format: uuid
                 * @description 든 네트워크 폴더(`Folder` id — 원본 네트워크의 폴더). 없으면 네트워크 트리의 루트다.
                 */
                folder?: string;
                /** @description VM마다 방식. 없으면 `auto`(전원·MTU·주소로 고름) */
                mode?: components["schemas"]["MigrationMode"];
                /**
                 * Format: int32
                 * @description 한 번에 옮기는 VM 수(1~8). 없으면 1이다.
                 */
                parallel?: number;
                /** @description 원본 단순 네트워크(브리지·VLAN·VXLAN) 이름 */
                source: string;
                /** @description 대상 SDN 네트워크(서브넷이 있는 오버레이 또는 공급자) 이름 */
                target: string;
                /** @description 옮길 VM. 비면 계획할 때 원본에 NIC가 있는 모든 VM이다. */
                vms?: string[];
            };
            status: {
                /** @description 놓은 다리 */
                bridge?: components["schemas"]["MigrationBridge"];
                error?: string;
                /** @default planned */
                phase: components["schemas"]["MigrationPhase"];
                /**
                 * Format: uuid
                 * @description 도는(돈) `network-migrate` Task
                 */
                task?: string;
                /** @description VM NIC마다의 계획과 상태(만들 때 계획을 적고 Task가 고친다) */
                vms?: components["schemas"]["VmMove"][];
            };
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_NetworkSpec_NetworkStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            /** @description `bridge`·`vxlan`·`sdn` 중 정확히 하나를 쓴다. */
            spec: {
                /** @description 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`) */
                bridge?: string;
                /**
                 * Format: uuid
                 * @description 든 네트워크 폴더(계획 62, `Folder` id). 없으면 네트워크 트리의 루트이고 쓰지 않아 옛 노드도 네트워크를 푼다.
                 */
                folder?: string;
                /**
                 * Format: int32
                 * @description VXLAN·SDN 네트워크의 MTU(1280~9000, 기본 1450·1442). `vxlan`·`sdn`과만 쓴다.
                 */
                mtu?: number;
                /** @description OVN 논리 스위치(Geneve)로 만드는 SDN 네트워크(계획 65). 없으면 쓰지 않아 옛 노드도 네트워크를 푼다. */
                sdn?: components["schemas"]["SdnNet"];
                /** @description SDN 네트워크의 서브넷(주소 체계마다 하나까지, 계획 65). `sdn`과만 쓰고 비면 쓰지 않는다. */
                subnets?: components["schemas"]["Subnet"][];
                /**
                 * Format: int32
                 * @description VLAN 태그(1~4094). `bridge`와만 쓰고, 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
                 */
                vlan?: number | null;
                /** @description orbivisord가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만드는 네트워크 */
                vxlan?: components["schemas"]["Vxlan"];
            };
            status: Record<string, never>;
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_NodeSpec_NodeStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                /** @description API 주소 (`host:port`) */
                address: string;
                /** @description 예약·전용 CPU와 hugepages 예약(계획 64 §7). 없으면 공유 = 모든 CPU — 지금과 같고 쓰지 않아 옛 노드도 푼다. */
                host_resources?: components["schemas"]["HostResources"];
                /** @description 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다. */
                maintenance?: boolean;
                /**
                 * @description 이 노드의 역할. 없으면 기본 역할(위트니스가 아니면 `api`)이다. 옛 노드는 이 필드를 모르므로 기본이면
                 *     쓰지 않는다.
                 */
                roles?: components["schemas"]["NodeRole"][];
                /** @description SDN physnet 연결과 게이트웨이 노드(계획 66 §7). 없으면 쓰지 않아 옛 노드도 푼다. */
                sdn?: components["schemas"]["NodeSdnConfig"];
                /** @description 위트니스 노드: Raft 투표만 하고 VM을 돌리지 않는다. 합류할 때 정해지고 바뀌지 않는다. */
                witness?: boolean;
            };
            /** @description 노드가 시작할 때 스스로 기록한다. */
            status: {
                /** @default null */
                arch: components["schemas"]["Arch"] | null;
                /**
                 * @description 이 노드가 쓸 수 있는 백업 저장소마다 닿는지(계획 68 §6 — 60초마다 `store.json` 읽기). 비면 쓰지 않는다(옛 노드·
                 *     저장소 없음). 옛 노드는 무시한다.
                 */
                backup_stores?: components["schemas"]["NodeStoreState"][];
                /**
                 * @description 이 노드에 있는 Linux 브리지. 스케줄러가 브리지 네트워크를 쓰는 VM을 배치할 때 본다.
                 * @default []
                 */
                bridges: components["schemas"]["BridgeInfo"][];
                /** @description 이 노드의 HA 검사 결과(화면용, 바뀔 때만 쓴다). 리더는 리스 갱신에 실린 결과로 판단한다. */
                checks?: components["schemas"]["CheckReport"][];
                /**
                 * @description 이 노드가 원하는 클러스터 링크(`host:port`, 앞이 우선). 노드가 등록할 때 설정의 `cluster_addresses`로
                 *     쓴다(옛 노드는 비어 있다). 리더가 Raft 멤버십의 링크를 이것에 맞춘다.
                 */
                cluster_addresses?: string[];
                /**
                 * @description 이 노드의 QEMU CPU 카탈로그(계획 63: 해시와 QEMU·가속기·호스트 CPU). 노드가 시작할 때와 QEMU가 바뀔 때 쓴다.
                 *     없으면(옛 노드·위트니스) `host`·`max`만 켤 수 있다. 옛 노드는 이 필드를 무시한다.
                 */
                cpu?: components["schemas"]["NodeCpu"];
                /**
                 * Format: int32
                 * @default 0
                 */
                cpus: number;
                /**
                 * @description 이 노드의 orbivisord가 아는 기능(예: `node-drain`). 노드가 등록할 때 쓴다(옛 노드는 비어 있다). 새 기능을
                 *     쓰는 API는 모든 노드가 그 기능을 적었는지 본다(버전이 섞여도 된다).
                 */
                features?: string[];
                /**
                 * @description 이웃 노드가 이 노드의 전원 끄기 펜싱 방법으로 전원을 읽어 본 마지막 결과. 이 노드가 쓴다(`NodeFence`가
                 *     없으면 None). 옛 노드는 이 필드를 무시한다.
                 */
                fence_check?: components["schemas"]["FenceCheck"];
                /**
                 * @description 이 노드에 VM 방화벽(nftables)이 있다. 보안 그룹을 건 NIC가 있는 VM은 이런 노드에서만 돈다
                 *     (위트니스는 늘 false).
                 */
                firewall?: boolean;
                /**
                 * @description 이 노드가 켤 수 있는 펌웨어 장치(계획 59: swtpm·Secure Boot 펌웨어). 노드가 시작할 때와 노드 검사가 쓴다.
                 *     없으면(옛 노드·위트니스) 모두 못 한다.
                 */
                firmware_caps?: components["schemas"]["FirmwareCaps"];
                /** @description 이 노드의 HA 펜싱. None이면 펜싱이 꺼져 HA VM을 돌리지 않는다(위트니스는 늘 None). */
                ha?: components["schemas"]["NodeHa"];
                /** @description 이 노드의 QEMU가 할 수 있는 핫플러그(계획 54). 노드가 시작할 때 쓴다. 없으면(옛 노드·위트니스) 모두 못 한다. */
                hotplug?: components["schemas"]["NodeHotplug"];
                /**
                 * @description 예: `qemu-hvf`, `qemu-kvm`, `qemu-tcg`
                 * @default
                 */
                hypervisor: string;
                /**
                 * @description 이 노드의 네트워크 인터페이스(루프백·tap·opld가 만든 장치 빼고 64개까지). 노드가 쓴다. HA 검사를 고를 때
                 *     보고, 본딩 포트 상태를 보인다. 옛 노드는 이 필드를 무시한다.
                 */
                interfaces?: components["schemas"]["NodeInterface"][];
                /**
                 * Format: int64
                 * @default 0
                 */
                memory_mib: number;
                /** @default false */
                online: boolean;
                /**
                 * @description 이 노드가 쓸 수 있는 스토리지 풀(풀 이름 → 상태). 노드가 주기적으로 기록한다.
                 * @default {}
                 */
                pools: {
                    [key: string]: components["schemas"]["PoolState"];
                };
                /**
                 * @description 이 노드가 단순 NIC의 tap에 tc 속도 제한(`clsact`·police, 계획 67 §8.2)을 걸 수 있다. 검사 루프가 쓴다. 거짓이면
                 *     쓰지 않는다(옛 노드·macOS·위트니스). 옛 노드는 무시한다.
                 */
                qos?: boolean;
                /**
                 * @description 이 노드의 SDN(OVN) chassis·중앙 상태(계획 65). 노드의 SDN 에이전트가 바뀔 때만, 모든 노드가 `sdn`을 알릴 때만
                 *     쓴다. 없으면(SDN 설정 전·옛 노드·macOS·위트니스) chassis가 아니다. 옛 노드는 무시한다.
                 */
                sdn?: components["schemas"]["NodeSdn"];
                /**
                 * @description 호스트가 꺼지는 중이라 이 노드가 종료 정책대로 정리하며 남긴 표시. 노드가 다시 등록할 때 지운다. 리더는
                 *     `freeze` 표시가 있는 오프라인 노드의 HA VM을 옮기지 않는다(옛 노드는 이 필드를 무시한다).
                 */
                shutdown?: components["schemas"]["NodeShutdown"];
                /**
                 * @description 이 노드의 CPU·메모리 NUMA 토폴로지(계획 64: 소켓·코어·스레드·NUMA 노드·hugepages·거리·L3). 노드가 시작할
                 *     때와 검사 루프가 바뀔 때만 쓴다. 없으면(옛 노드·macOS·위트니스) 게스트 vNUMA만 된다. 옛 노드는 무시한다.
                 */
                topology?: components["schemas"]["NodeTopology"];
                /**
                 * Format: ip
                 * @description VXLAN 패킷을 주고받는 이 노드의 주소. 없으면 이 노드에 VXLAN 장치를 만들지 않는다(위트니스는 늘
                 *     None).
                 */
                underlay?: string;
                /**
                 * @description 이 노드의 APT 업데이트 확인 결과. 업데이트가 켜진 노드는 등록할 때 `Some`(아직 확인 전이면
                 *     빈 값)이고, 꺼진 노드(위트니스, macOS 등)와 옛 노드는 None이다.
                 */
                updates?: components["schemas"]["NodeUpdates"];
                /** @description 이 노드에서 도는 orbivisord의 빌드 버전. 노드가 등록할 때 쓴다(옛 버전 노드는 None). */
                version?: string;
            };
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_PlacementRuleSpec_PlacementRuleStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            /**
             * @description 배치 규칙: VM끼리(같은 노드·다른 노드) 또는 VM과 노드(이 노드들 중·이 노드들은 안 됨). hard 규칙은
             *     조건이고 soft 규칙은 벌점(`weight`)이다. 모든 VM에 적용한다.
             */
            spec: {
                /**
                 * @description 끄면 없는 것으로 본다.
                 * @default true
                 */
                enabled: boolean;
                /** @description hard 규칙만: HA 복구가 지킬 수 없으면 어기고 켠다. */
                ha_may_violate?: boolean;
                kind: components["schemas"]["RuleKind"];
                /**
                 * @description 이 레이블(`key=value`, 예: `zone=rack-a`)을 가진 노드들
                 * @example zone=rack-a
                 */
                node_tag?: string;
                /** @description VM과 노드 규칙의 노드 이름. `node_tag`와 둘 중 하나만. */
                nodes?: string[];
                /**
                 * @description 이 레이블(`key=value`)을 가진 VM들
                 * @example app=web
                 */
                vm_tag?: string;
                /** @description 규칙의 VM 이름(아직 없는 VM이어도 된다). `vm_tag`와 둘 중 하나만. */
                vms?: string[];
                /**
                 * Format: int32
                 * @description soft 규칙의 벌점 1~100. 없으면 50.
                 */
                weight?: number;
            };
            status: Record<string, never>;
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_RouterSpec_RouterStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                /** @description 200자 이하 */
                description?: string;
                /** @description 외부 게이트웨이(공급자 네트워크). 없으면 라우터는 안쪽 네트워크끼리만 잇는다. */
                external?: components["schemas"]["RouterExternal"];
                /**
                 * Format: uuid
                 * @description 든 네트워크 폴더(`Folder` id). 없으면 네트워크 트리의 루트다.
                 */
                folder?: string;
                /** @description 잇는 오버레이 SDN 네트워크(16개까지). 그 네트워크의 서브넷 게이트웨이가 라우터 포트 주소다. */
                interfaces?: components["schemas"]["RouterInterface"][];
                /** @description 정적 경로(64개까지). 외부가 있으면 기본 경로는 외부 서브넷 게이트웨이로 저절로 생긴다. */
                routes?: components["schemas"]["StaticRoute"][];
            };
            /** @description 지금은 칸이 없다(게이트웨이 주인·순위는 보기가 NB에서 읽는다). */
            status: Record<string, never>;
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_SecurityGroupSpec_SecurityGroupStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                /** @description 200자 이하 */
                description?: string;
                /** @description 적힌 순서대로 보고 처음 맞는 규칙을 따른다(200개 이하). */
                rules?: components["schemas"]["Rule"][];
            };
            status: {
                /** @description 이 그룹을 건 VM NIC 수(계획 66 §11.4). API가 응답에서만 채운다(Raft에 쓰지 않는다). */
                nics?: components["schemas"]["SecurityGroupNics"];
            };
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_SettingsSpec_SettingsStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                /**
                 * @description 앞단·VIP에서 API를 부르는 이름(DNS 이름·IP). 각 노드가 자기 API 인증서에 더한다(피어 인증서는 그대로).
                 *     비어 있으면 쓰지 않는다.
                 */
                api_names?: string[];
                /**
                 * @description 게스트 CPU 클러스터 기준과 숨김 목록(계획 63). 없으면 기준 없음·숨김 없음이고 쓰지 않아 옛 노드도 설정을 계속
                 *     푼다.
                 */
                cpu?: components["schemas"]["CpuSettings"];
                /** @description HA 복구를 위해 비워 두는 용량. 없으면 예약하지 않고(`none`), 쓰지 않아 옛 노드도 설정을 계속 푼다. */
                ha_reserve?: components["schemas"]["HaReserve"];
                /**
                 * Format: int32
                 * @description 리더가 HA 복구 때 대상 노드 하나에 한꺼번에 옮겨 켜는 VM 수(1~32). 없으면 4이고 쓰지 않는다.
                 */
                ha_start_concurrency?: number;
                /**
                 * @description 이미지 라이브러리의 관리자 항목(계획 58, 200개까지, 내장 id와 겹치지 않게). 비어 있으면 쓰지 않아 옛 노드도
                 *     설정을 계속 푼다.
                 */
                image_catalog?: components["schemas"]["CatalogImage"][];
                /**
                 * @description 최적 배치(계획 53): 점수 정책과 균형 조정기. 없으면 기본값(`balance`, `recommend`, 15%)이고 쓰지 않아 옛
                 *     노드도 설정을 계속 푼다.
                 */
                placement?: components["schemas"]["PlacementSettings"];
                /** @description Prometheus 끝점 `GET /api/v1/metrics`(계획 60). 없으면 꺼짐이고 쓰지 않아 옛 노드도 설정을 계속 푼다. */
                prometheus?: components["schemas"]["PrometheusSettings"];
                /**
                 * @description 호스트가 꺼지는 중(재부팅·전원 끄기)일 때 노드가 HA VM을 다루는 방법. 없으면 `conditional`이고, 쓰지 않아
                 *     옛 노드도 설정을 계속 푼다.
                 */
                shutdown_policy?: components["schemas"]["ShutdownPolicy"];
                /** @description 옵트인 텔레메트리. 기본값(꺼짐, 끝점 없음)이면 쓰지 않아 옛 노드도 설정을 계속 푼다. */
                telemetry?: components["schemas"]["TelemetrySettings"];
                /** @description 2단계 인증 정책(계획 62). 없으면 아무에게도 요구하지 않고 쓰지 않아 옛 노드도 설정을 계속 푼다. */
                two_factor?: components["schemas"]["TwoFactorPolicy"];
                updates?: components["schemas"]["UpdateSettings"];
                /**
                 * Format: int32
                 * @description 투표 멤버 상한: 3, 5, 7 가운데 하나. 없으면 5이고, 쓰지 않아 옛 노드도 설정을 계속 푼다. 위트니스도
                 *     센다. 노드가 이보다 적으면 모두 투표 멤버다.
                 */
                voters?: number;
                /**
                 * @description 쿠키 요청을 받는 다른 출처(`https://portal.example.com`). 따로 띄운 웹 포털이 API를 부를 때 쓴다.
                 *     비어 있으면 같은 출처만 받고, 쓰지 않아 옛 노드도 설정을 계속 푼다.
                 */
                web_origins?: string[];
            };
            status: {
                /** @description 텔레메트리 상태. 처음 켤 때 생기고, 없으면 쓰지 않는다. */
                telemetry?: components["schemas"]["TelemetryStatus"];
            };
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_SnapshotSpec_SnapshotStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                description?: string;
                /**
                 * @description 메모리 스냅샷이면 그때 도는 QEMU의 하드웨어(계획 54, 모든 노드가 `hotplug`일 때만). 메모리를 불러오려면 같은
                 *     장치로 켜야 하므로 되돌리기는 이 모양(핫플러그 배치 있음·없음, 상한)으로 켠다. 없으면 배치 없이 켠다.
                 */
                hardware?: components["schemas"]["RunningHardware"];
                /** @description 메모리 상태를 담았는지. 담았으면 VM의 첫 디스크에 있다. */
                memory: boolean;
                /** @description 사용자가 붙인 이름. VM 안에서 고유하다. */
                name: string;
                /**
                 * Format: uuid
                 * @description 부모 스냅샷 id. VM의 첫 스냅샷이면 없다.
                 */
                parent?: string | null;
                /** @description 예약으로 만들었는지. 보관 개수 정리는 이것만 지운다. */
                scheduled?: boolean;
                /**
                 * Format: uuid
                 * @description VM id
                 */
                vm: string;
                /** @description 만든 순간의 VM 스펙 */
                vm_spec: components["schemas"]["VmSpec"];
            };
            status: {
                /**
                 * Format: int64
                 * @description 만든 시각(유닉스 초)
                 * @default null
                 */
                created_at: number | null;
                /** @default pending */
                phase: components["schemas"]["SnapshotPhase"];
                /**
                 * Format: uuid
                 * @description TPM 상태 사본(`{스냅샷 id}.tpm`)을 둔 노드(계획 59). 되돌리기는 이 노드에서만 TPM 상태를 돌려놓는다. 찍을 때
                 *     TPM 상태가 없었으면(TPM이 없거나 한 번도 켜지 않은 VM) 없다.
                 */
                tpm_node?: string;
                /**
                 * Format: int64
                 * @description 메모리 상태 크기. 메모리 스냅샷만 있다.
                 * @default null
                 */
                vmstate_bytes: number | null;
            };
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_StoragePoolSpec_StoragePoolStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                backend: components["schemas"]["PoolBackend"];
                /** @description 백업 파일(`{루트}/backups/`)을 둘 수 있는 백업 풀인지. dir·NFS 풀만 된다. 켜도 VM 디스크는 그대로 둔다. */
                backups?: boolean;
                /**
                 * Format: uuid
                 * @description 든 스토리지 폴더(계획 62, `Folder` id). 없으면 스토리지 트리의 루트이고 쓰지 않아 옛 노드도 풀을 푼다.
                 */
                folder?: string;
                /** @description 이 풀을 쓸 수 있는 노드. 비어 있으면 모든 노드. */
                nodes?: string[];
                /** @description 모든 노드가 같은 데이터를 보는 공유 스토리지인지. NFS·RBD·공유 LVM 풀은 늘 공유다(`is_shared`). */
                shared?: boolean;
            };
            status: {
                /**
                 * Format: int64
                 * @default null
                 */
                available_bytes: number | null;
                /**
                 * Format: int64
                 * @default null
                 */
                total_bytes: number | null;
            };
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_TaskSpec_TaskStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                /**
                 * Format: uuid
                 * @description 작업을 실행할 노드
                 */
                node: string;
                op: components["schemas"]["TaskOp"];
                /**
                 * Format: uuid
                 * @description 대상 리소스(VM 또는 Volume)의 id. 스냅샷·복제·백업 Task는 (원본) VM, 백업 지우기는 백업,
                 *     복원과 VM 가져오기는 새 VM, 가져오기 지우기는 가져오기다.
                 */
                target: string;
            };
            status: {
                /** @default null */
                error: string | null;
                /** @description 실행 중인 Task의 진행률(지금은 로컬 디스크를 복사하는 마이그레이션만). Task가 끝나면 지운다. */
                progress?: components["schemas"]["TaskProgress"];
                /** @default pending */
                state: components["schemas"]["TaskState"];
            };
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_VmImportSpec_VmImportStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                /**
                 * Format: uuid
                 * @description 원본 파일이 있는 노드(올린 노드)
                 */
                node: string;
                source: components["schemas"]["ImportSource"];
            };
            status: {
                /** @description 마지막 실행이 실패한 이유(phase는 `ready`로 돌아간다), 또는 `failed`인 이유 */
                error?: string;
                /** @default ready */
                phase: components["schemas"]["ImportPhase"];
                /** @description [`propose`]가 만든 기본값 */
                proposal?: components["schemas"]["ImportProposal"];
                /** @description 원본 VM에서 읽은 것(표시용) */
                source_vm?: components["schemas"]["SourceVm"];
                /**
                 * Format: uuid
                 * @description 만든 VM id(`done`일 때)
                 */
                vm?: string;
                /**
                 * @description 해석과 제안의 경고
                 * @default []
                 */
                warnings: string[];
            };
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_VmSpec_VmStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            /** @description VM의 원하는 상태. */
            spec: {
                /** @description CD-ROM 드라이브(계획 57, 넷까지, 순번 = 드라이브 번호). 비면 쓰지 않아 옛 노드도 VM을 푼다. */
                cdroms?: components["schemas"]["CdromSpec"][];
                /** @description cloud-init NoCloud 씨앗(계획 57). 없으면 쓰지 않아 옛 노드도 VM을 푼다. */
                cloud_init?: components["schemas"]["CloudInit"];
                cpu: components["schemas"]["CpuSpec"];
                disks?: components["schemas"]["DiskSpec"][];
                firmware?: components["schemas"]["Firmware"];
                /**
                 * Format: uuid
                 * @description 든 VM 폴더(계획 62, `Folder` id). 없으면 VM 트리의 루트이고 쓰지 않아 옛 노드도 VM을 푼다. 템플릿도 같은
                 *     트리에 산다.
                 */
                folder?: string;
                /**
                 * @description 게스트 에이전트(계획 59): 채널을 달고 게스트 정보·에이전트 종료·fs-freeze에 쓴다. 없으면 쓰지 않아 옛 노드도
                 *     VM을 푼다(`ha.guest_watch`가 있으면 채널은 그대로 단다).
                 */
                guest_agent?: components["schemas"]["GuestAgentSpec"];
                /**
                 * @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다.
                 *     JSON은 옛 bool(`false` = 없음, `true` = 기본 정책)이나 정책 객체다.
                 */
                ha?: components["schemas"]["HaSetting"];
                /** @description 게스트 메모리를 hugepages로(계획 64, `2M`·`1G`·`any`). 없으면 보통 페이지이고 쓰지 않는다. */
                hugepages?: components["schemas"]["PageSize"];
                machine: components["schemas"]["MachineType"];
                /**
                 * Format: int32
                 * @description 메모리 핫플러그 상한(MiB, 계획 54). 없으면 `memory_mib`와 같다(핫플러그 꺼짐). 없으면 쓰지 않아 옛 노드도
                 *     VM을 푼다.
                 */
                max_memory_mib?: number;
                /** Format: int32 */
                memory_mib: number;
                nics?: components["schemas"]["NicSpec"][];
                /**
                 * Format: uuid
                 * @description 배치된 노드의 id. None이면 시작할 때 스케줄러가 고른다.
                 */
                node?: string | null;
                /**
                 * @description 게스트 vNUMA(계획 64). 없으면 전용·hugepages VM은 `auto`, 그 밖은 vNUMA 없음(지금 그대로)이고 쓰지 않아 옛
                 *     노드도 VM을 푼다.
                 */
                numa?: components["schemas"]["NumaSpec"];
                /** @description 최적 배치 옵션(계획 53). 없으면 쓰지 않아 옛 노드도 VM을 푼다. */
                placement?: components["schemas"]["VmPlacement"];
                power?: components["schemas"]["PowerState"];
                /**
                 * @description 로컬 디스크 복제 대상(계획 55, 셋까지). 노드 로컬 풀의 디스크를 이 노드들에 주기적으로 복제한다. 비면 쓰지
                 *     않아 옛 노드도 VM을 푼다.
                 */
                replication?: components["schemas"]["ReplicationTarget"][];
                restart?: components["schemas"]["RestartPolicy"];
                /**
                 * @description Secure Boot(계획 59): Microsoft 키를 등록한 UEFI 변수로 켜는 secure-boot 펌웨어(x86_64는 SMM). UEFI VM만 쓸 수
                 *     있다. 거짓이면 쓰지 않아 옛 노드도 VM을 푼다.
                 */
                secure_boot?: boolean;
                snapshot_schedule?: components["schemas"]["SnapshotSchedule"] | null;
                /**
                 * @description 템플릿(계획 58): 켤 수 없고 디스크를 바꿀 수 없는 원본. 배포(복제)와 연결 복제의 기반이 된다. 거짓이면 쓰지
                 *     않아 옛 노드도 VM을 푼다.
                 */
                template?: boolean;
                /**
                 * @description TPM 2.0(계획 59): VM마다 swtpm 하나를 띄워 단다(q35 `tpm-crb`, virt `tpm-tis-device`). UEFI VM만 쓸 수 있다.
                 *     상태는 노드 로컬 파일이고 `firmware_node`를 따라다닌다. 거짓이면 쓰지 않아 옛 노드도 VM을 푼다.
                 */
                tpm?: boolean;
            };
            /** @description VM의 실제 상태. reconciler만 기록한다. */
            status: {
                /**
                 * @description `tpm_sha256`과 같은 펌웨어 사본(UEFI 변수 + TPM)을 가진 다른 노드(계획 59). 해시가 바뀌면 비운다. 비면 쓰지
                 *     않는다.
                 */
                firmware_copies?: string[];
                /**
                 * Format: int32
                 * @description 펌웨어 초기화 회차(계획 59). `vm-firmware-reset`이 1씩 올린다. 노드의 파일이 다른 회차의 것이면 낡은 것이다.
                 *     0이면 쓰지 않는다.
                 */
                firmware_epoch?: number;
                /**
                 * Format: uuid
                 * @description 지금 UEFI 변수 파일을 가진 노드. UEFI VM을 켠 노드나 마이그레이션의 대상 노드가 쓴다.
                 *     다른 노드에서 켤 때 이 노드에서 변수를 받아 온다. BIOS VM은 늘 없다.
                 */
                firmware_node?: string;
                /** @description HA 재시작·옮기기 기록. 기록이 없으면 빼고 쓴다. */
                ha?: components["schemas"]["VmHaStatus"];
                /**
                 * @description 도는 QEMU의 하드웨어(계획 54). VM을 켠 노드가 `running`을 쓸 때 함께 쓰고 핫플러그 Task가 고치며, QEMU가
                 *     없어지면 지운다. 옛 노드는 이 필드를 무시한다.
                 */
                hardware?: components["schemas"]["RunningHardware"];
                /** @default null */
                last_error: string | null;
                /**
                 * @description HA가 이 VM을 복제본으로 다시 켠 마지막 복구(계획 55). 리더의 HA 복구가 쓰고 다음 복구가 덮는다. 옛 노드는 이
                 *     필드를 무시한다.
                 */
                last_recovery?: components["schemas"]["ReplicaRecovery"];
                /**
                 * @description 진행 중인 라이브 복원(계획 68 §11). 라이브 복원 Task가 쓰고 갈아타기가 끝나면 지운다. 있는 동안 reconciler·HA가
                 *     이 VM을 켜거나 옮기지 않는다. 없으면 쓰지 않는다.
                 */
                live_restore?: components["schemas"]["LiveRestore"];
                /** @description 진행 중인 라이브 마이그레이션. 원본 노드의 마이그레이션 Task가 쓰고 지운다. */
                migration?: components["schemas"]["MigrationStatus"];
                /**
                 * Format: uuid
                 * @default null
                 */
                node: string | null;
                /** @default stopped */
                phase: components["schemas"]["VmPhase"];
                /**
                 * Format: int32
                 * @default null
                 */
                pid: number | null;
                /**
                 * @description 로컬 디스크 복제 대상마다의 상태(계획 55). 원본 노드의 복제 Task, 리더의 HA 복구·스케줄러, 마이그레이션
                 *     확정이 쓴다. 옛 노드는 이 필드를 무시한다.
                 */
                replication?: components["schemas"]["ReplicaStatus"][];
                /** @description 지금 배치가 어기는 배치 규칙의 이름. 리더의 HA 관리자가 쓴다. */
                rule_violations?: string[];
                /** @default null */
                schedule_error: components["schemas"]["ScheduleError"] | null;
                /**
                 * Format: uuid
                 * @description 현재 위치: 마지막으로 만들었거나 되돌린 스냅샷. 다음 스냅샷의 부모가 된다.
                 * @default null
                 */
                snapshot: string | null;
                /**
                 * Format: uuid
                 * @description 지금 TPM 상태를 가진 노드(리뷰 2). TPM VM을 켠 노드·마이그레이션 대상·TPM을 되돌리거나 복원한 노드가 쓰고, TPM을
                 *     초기화하면 지운다. UEFI 변수를 가진 노드(`firmware_node`)와 다를 수 있다(TPM을 끈 채 다른 노드에서 켬). 이 노드의
                 *     파일만 기록된 해시(`tpm_sha256`) 뒤에 바뀌었을 수 있어 해시가 달라도 지금 것으로 본다. 없으면 쓰지 않는다.
                 */
                tpm_node?: string;
                /**
                 * @description 지금 TPM 영구 상태(`tpm2-00.permall`)의 SHA-256(16진 소문자, 계획 59). 비밀이 아니다. VM을 돌리는 노드가
                 *     멈출 때와 실행 중 주기적으로 쓴다. 없으면(아직 TPM 상태가 없음, 초기화 뒤) 쓰지 않는다.
                 */
                tpm_sha256?: string;
            };
        };
        /** @description 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다. */
        Resource_VolumeSpec_VolumeStatus: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: {
                /**
                 * Format: uuid
                 * @description 연결 복제(계획 58)면 기반(템플릿 디스크) 볼륨의 id. 기반은 늘 같은 풀에 있다. 없으면 쓰지 않아 옛 노드도
                 *     볼륨을 푼다.
                 */
                base?: string;
                /** @description 내용 종류(계획 57). 디스크(기본)면 쓰지 않아 옛 노드도 볼륨을 푼다. */
                content?: components["schemas"]["VolumeContent"];
                format: components["schemas"]["ImageFormat"];
                /** @description StoragePool 리소스 이름 */
                pool: string;
                /** Format: int64 */
                size_bytes: number;
            };
            status: {
                /**
                 * Format: int64
                 * @description 실제로 디스크에 할당된 크기
                 * @default null
                 */
                allocated_bytes: number | null;
                /**
                 * Format: uuid
                 * @default null
                 */
                attached_to: string | null;
                /**
                 * Format: int64
                 * @description 늘리기 Task가 저장소를 늘린 뒤 읽은 실제 크기(계획 62 리뷰 1). 스펙 크기를 이 값까지 올리는 기록은 폴더
                 *     할당량에 막히지 않는다(이미 커진 디스크를 기록에서 숨기지 않게). Task가 기록한 뒤 지운다. 없으면 쓰지 않는다.
                 */
                disk_bytes?: number;
                /**
                 * Format: uuid
                 * @description 볼륨을 만든 Task를 실행하는 노드. 그 노드만 시작할 때 남은 고아 볼륨을 정리한다.
                 * @default null
                 */
                node: string | null;
                /** @default pending */
                phase: components["schemas"]["VolumePhase"];
            };
        };
        /** @enum {string} */
        RestartPolicy: "never" | "on-failure";
        RestoreRequest: {
            /**
             * Format: uuid
             * @description 새 VM을 둘 폴더 id(계획 62). 없으면 백업한 VM의 폴더(아직 있으면), 아니면 루트다.
             */
            folder?: string | null;
            /** @description 라이브 복원(계획 68 §11): 디스크를 다 받기 전에 새 VM을 켜고 뒤에서 옮긴다. 새 폴더의 `vm.power`도 든다. */
            live?: boolean;
            /** @description 새 VM의 이름 */
            name: string;
            /** @description 복원을 실행할 노드(이름이나 id). 없으면 백업 풀과 대상 풀을 함께 쓸 수 있는 노드다. */
            node?: string | null;
            /** @description 새 디스크를 둘 풀 */
            pool: string;
        };
        /** @enum {string} */
        Role: "admin" | "operator" | "viewer" | "none";
        /** @description 전역 역할이 있는 사용자 */
        RoleHolder: {
            /** Format: uuid */
            id: string;
            name: string;
            role: components["schemas"]["Role"];
        };
        /** @description 그룹에서 역할 정하기 */
        RoleMapping: {
            /** @description 맞는 그룹이 없을 때의 역할. 없으면 로그인을 거절한다. */
            default?: components["schemas"]["Role"];
            groups?: components["schemas"]["GroupRole"][];
        };
        /** @description grant가 주는 역할 */
        RoleRef: {
            role: components["schemas"]["Role"];
            /** @enum {string} */
            type: "builtin";
        } | {
            /** Format: uuid */
            id: string;
            /** @enum {string} */
            type: "custom";
        };
        /** @description 라우터 인터페이스인 네트워크 하나(`RouterList.routed`) */
        RoutedNetwork: {
            /** @description 그 라우터의 외부(공급자) 네트워크. 외부가 없으면 없고, 부른 사용자가 그 네트워크를 읽을 수 없으면 빈 글이다. */
            external?: string;
            /** @description 오버레이 네트워크 이름 */
            network: string;
        };
        /** @description 라우터의 외부 게이트웨이 */
        RouterExternal: {
            /** @description 라우터의 외부 주소(체계마다 하나). API가 그 네트워크의 유동 IP 풀(없으면 풀)에서 채운다. */
            ips?: string[];
            /** @description 공급자 네트워크 이름 */
            network: string;
            /** @description 안쪽 IPv4 서브넷의 보낸 주소를 외부 주소로 바꾼다(SNAT). 없으면 참이다. */
            snat?: boolean;
        };
        /** @description 게이트웨이 노드 하나(Raft의 순위 — `HA_Chassis`) */
        RouterGateway: {
            /** @description NB의 분산 게이트웨이 포트를 지금 이 노드가 가졌다(`status:hosting-chassis`). NB를 읽지 않았으면 거짓 */
            active: boolean;
            /** @description 노드 이름(루트 `cluster.read`가 없으면 빈 글) */
            node: string;
            /**
             * Format: int32
             * @description 노드의 게이트웨이 우선순위(`NodeSpec.sdn.gateway`)
             */
            node_priority: number;
            /** @description 노드가 살아 있다(`NodeStatus.online` — 리뷰 2 웹 I2). 거짓이면 OVN이 다음 살아 있는 후보로 넘긴다. */
            online: boolean;
            /**
             * Format: int32
             * @description `HA_Chassis.priority`(32767부터 내림 — 가장 높은 살아 있는 chassis가 주인)
             */
            priority: number;
        };
        /** @description 라우터 인터페이스 하나(오버레이 SDN 네트워크) */
        RouterInterface: {
            /** @description 네트워크 이름 */
            network: string;
        };
        /** @description `GET /routers` */
        RouterList: {
            items: components["schemas"]["RouterView"][];
            /** Format: int64 */
            revision: number;
            /**
             * @description 라우터 인터페이스인 네트워크 가운데 부른 사용자가 읽을 수 있는 것(그 라우터를 읽을 수 없어도 — 리뷰 2 웹 I1).
             *     유동 IP를 붙일 수 있는 NIC와 라우터를 이을 수 있는 네트워크를 웹이 서버와 같게 고르게 한다. 라우터 이름은 없다.
             */
            routed?: components["schemas"]["RoutedNetwork"][];
        };
        /** @description `PATCH /routers/{key}`: 준 칸만 바꾼다(목록은 통째로). */
        RouterPatch: {
            /** @description 빈 글이면 설명을 지운다. */
            description?: string;
            external?: components["schemas"]["RouterExternal"] | null;
            /** @description 인터페이스 전체 */
            interfaces?: components["schemas"]["RouterInterface"][];
            /** @description 정적 경로 전체 */
            routes?: components["schemas"]["StaticRoute"][];
        };
        RouterSpec: {
            /** @description 200자 이하 */
            description?: string;
            /** @description 외부 게이트웨이(공급자 네트워크). 없으면 라우터는 안쪽 네트워크끼리만 잇는다. */
            external?: components["schemas"]["RouterExternal"];
            /**
             * Format: uuid
             * @description 든 네트워크 폴더(`Folder` id). 없으면 네트워크 트리의 루트다.
             */
            folder?: string;
            /** @description 잇는 오버레이 SDN 네트워크(16개까지). 그 네트워크의 서브넷 게이트웨이가 라우터 포트 주소다. */
            interfaces?: components["schemas"]["RouterInterface"][];
            /** @description 정적 경로(64개까지). 외부가 있으면 기본 경로는 외부 서브넷 게이트웨이로 저절로 생긴다. */
            routes?: components["schemas"]["StaticRoute"][];
        };
        /** @description 지금은 칸이 없다(게이트웨이 주인·순위는 보기가 NB에서 읽는다). */
        RouterStatus: Record<string, never>;
        /** @description `GET /routers/{key}`: 라우터와 게이트웨이(Raft의 순위와 NB의 지금 주인) */
        RouterView: components["schemas"]["Resource_RouterSpec_RouterStatus"] & {
            /** @description 외부가 있는데 게이트웨이 후보가 없다(그동안 외부 게이트웨이 포트는 꺼져 있고 SNAT·유동 IP가 없다). */
            gateway_error?: string;
            /** @description 게이트웨이 후보의 순위(외부가 있을 때 — 우선순위가 있고 유지보수가 아니며 외부 physnet이 준비된 노드, 앞 5개) */
            gateways?: components["schemas"]["RouterGateway"][];
            /**
             * @description 분산 게이트웨이 포트를 지금 가진 노드(NB `status:hosting-chassis`). 없거나 NB를 읽지 않았으면 없다. 루트
             *     `cluster.read`가 없으면 빈 글이다.
             */
            hosting?: string;
            /** @description NB를 읽지 못한 까닭 */
            live_error?: string;
            /**
             * @description 물리 라우터에 있어야 할 경로(NAT 없이 라우팅되는 안쪽 접두사 → 라우터 외부 주소): IPv6 서브넷은 늘(NAT66 없음),
             *     SNAT를 끈 라우터는 IPv4 서브넷도. 예 `fd00:20::/64 via 2001:db8::5`
             */
            upstream_routes?: string[];
        };
        Rule: {
            action: components["schemas"]["Action"];
            direction: components["schemas"]["Direction"];
            /**
             * @description 목적지 포트 `"22"` 또는 `"8000-8080"`. tcp·udp에서만.
             * @example 8000-8080
             */
            ports?: string;
            /** @description 없으면 모든 프로토콜 */
            protocol?: components["schemas"]["Protocol"];
            /**
             * @description `in`이면 보낸 쪽, `out`이면 받는 쪽 주소(CIDR). 없으면 어디든.
             * @example 10.0.0.0/8
             */
            remote?: string;
        };
        /**
         * @description 배치 규칙의 종류
         * @enum {string}
         */
        RuleKind: "affinity" | "anti-affinity" | "soft-affinity" | "soft-anti-affinity" | "node-affinity" | "node-anti-affinity" | "soft-node-affinity" | "soft-node-anti-affinity";
        /** @description 도는 QEMU의 하드웨어. CPU 토폴로지·메모리·펌웨어·머신은 켤 때 값이고, 핫플러그로 바뀐 것은 `layout`에 있다. */
        RunningHardware: {
            /**
             * @description 지금 QEMU의 CD-ROM 드라이브와 매체(계획 57, 순번 = QEMU의 `cd{i}-dev`). 켤 때 스펙 그대로이고 매체를 바꾸면
             *     핫플러그 Task가 고친다. 옛 노드는 이 필드를 무시한다.
             */
            cdroms?: components["schemas"]["CdromSpec"][];
            /** @description 켤 때 넣은 cloud-init 씨앗의 instance-id(계획 57). 없으면 씨앗 드라이브가 없다. 옛 노드는 이 필드를 무시한다. */
            cloud_init?: string;
            /** @description 켤 때의 토폴로지·모델·상한. 노드가 CPU 핫플러그를 못 하거나 핫플러그 배치 없이 켰으면 `max_vcpus`가 없다. */
            cpu: components["schemas"]["CpuSpec"];
            /**
             * @description 켤 때 실제로 쓴 CPU(계획 63: 해석된 모델·플래그). 없으면 계획 63 전 기록 = `cpu.model`대로 `host`·`max`로
             *     켰다. 받는 쪽 마이그레이션은 이것을 그대로 쓴다. 옛 노드는 이 필드를 무시한다.
             */
            cpu_resolved?: components["schemas"]["ResolvedCpu"];
            /** @description 지금 QEMU에 붙은 디스크(순번 = QEMU의 `disk{i}`). 실행 중 붙이고 떼기를 스펙과 견준다. */
            disks?: components["schemas"]["DiskSpec"][];
            firmware: components["schemas"]["Firmware"];
            /**
             * @description 켤 때 게스트 에이전트 채널을 달았다(계획 59). 거짓이면 쓰지 않는다. 계획 59 전 노드의 기록에는 없으므로
             *     `ha.guest_watch`만 있는 VM은 거짓이어도 채널이 있을 수 있다.
             */
            guest_agent?: boolean;
            /** @description 켠 노드의 호스트 NUMA 노드·pCPU(계획 64). 묶는 VM에만 있고 `node`가 지금 노드와 다르면 낡은 기록이다. */
            host_placement?: components["schemas"]["HostPlacement"];
            /** @description 핫플러그 배치. 없으면 핫플러그 없이 켠 VM이다(계획 54 전이나 업그레이드 중에 켬). */
            layout?: components["schemas"]["HotplugLayout"];
            machine: components["schemas"]["MachineType"];
            /**
             * Format: int32
             * @description 켤 때의 메모리(`-m`, MiB)
             */
            memory_mib: number;
            /** @description 지금 QEMU에 붙은 NIC(순번 = QEMU의 `nic{i}`). 보안 그룹은 견주지 않는다. */
            nics?: components["schemas"]["NicSpec"][];
            /**
             * @description 켤 때 정한 게스트 NUMA 모양·페이지·CPU 정책(계획 64). 없으면 계획 64 전 기록이거나 이 기능을 쓰지 않는 VM이다.
             *     받는 쪽 마이그레이션은 이것을 그대로 쓴다. 옛 노드는 이 필드를 무시한다.
             */
            numa?: components["schemas"]["ResolvedNuma"];
            /** @description 다음 재시작 때 적용할 변경. 핫플러그 Task가 쓴다. */
            pending?: components["schemas"]["PendingChange"][];
            /** @description 켤 때 Secure Boot 펌웨어로 켰다(계획 59). 거짓이면 쓰지 않는다. */
            secure_boot?: boolean;
            /** @description 켤 때 TPM을 달았다(계획 59). 거짓이면 쓰지 않아 옛 노드의 기록 그대로다. */
            tpm?: boolean;
            /**
             * @description 떼기를 요청했으나 게스트가 아직 내려 주지 않은 장치의 QEMU id(`disk{i}-dev`, `nic{i}`, `cpu{i}`). 디스크·NIC는
             *     `disks`·`nics`에 남아 있고 vCPU는 `layout.vcpus`에 들어 있으며, 다음 핫플러그 Task가 떼기를 마저 한다.
             */
            unplugging?: string[];
        };
        /**
         * @description 마지막 실행기 확인에서 `core`·`api`가 제때 답했는지(`work`만 늦으면 `ok`)
         * @enum {string}
         */
        RuntimeHealth: "ok" | "slow";
        /** @description S3 자격 증명 바꾸기(둘 다) */
        S3Credentials: {
            access_key_id: string;
            secret_access_key: string;
        };
        /** @description S3 호환 객체 저장소의 버킷과 접두사(§7.7) */
        S3Target: {
            access_key_id: string;
            bucket: string;
            /** @description 끝점 인증서를 확인할 사용자 CA(PEM). 없으면 시스템 루트. */
            ca_pem?: string;
            /** @description 끝점 URL(`https://host[:port]`). 시험만 `http`(`insecure_http`). */
            endpoint: string;
            /** @description `http` 끝점을 허락한다(시험용 — 자격 증명과 데이터가 평문으로 오간다). */
            insecure_http?: boolean;
            /** @description 경로 방식 주소(`{끝점}/{버킷}/…` — MinIO·Ceph RGW). 거짓이면 가상 호스트 방식(`{버킷}.{호스트}`). */
            path_style?: boolean;
            /** @description 버킷 안 접두사. 끝에 `/`를 붙여 쓴다. 없으면 버킷 맨 위다. */
            prefix?: string;
            /** @default us-east-1 */
            region: string;
            /** @description 비밀 키. Raft에만 있고 API 보기·watch·감사·`Debug`에서 뺀다. */
            secret_access_key: string;
        };
        /** @description 보기의 S3 대상: 비밀 키가 없다. */
        S3TargetView: {
            access_key_id: string;
            bucket: string;
            ca_pem?: string;
            endpoint: string;
            insecure_http?: boolean;
            path_style?: boolean;
            prefix?: string;
            region: string;
        };
        /**
         * @description 예약 스냅샷이 실패한 이유. `code`는 거절한 API 오류의 code(`vm_busy` 등)이고, Task가 실패했으면
         *     `task_failed`다. 옛 상태의 문자열은 `{ code: "unknown", message: <문자열> }`로 읽는다.
         */
        ScheduleError: {
            code: string;
            message: string;
        };
        /** @enum {string} */
        ScheduleEvery: "hourly" | "daily" | "weekly";
        /** @description 중앙 하나와 그 노드가 알린 Raft 역할 */
        SdnCentralView: {
            /** Format: ip */
            address: string;
            health?: components["schemas"]["CentralHealth"];
            /** @description 노드 이름(노드가 없어졌으면 id) */
            name: string;
            /** Format: uuid */
            node: string;
        };
        /** @description `POST /sdn/chassis`: `nodes`의 노드나(`all`이면) chassis가 아닌 모든 Linux 노드에 chassis를 설치한다. */
        SdnChassisRequest: {
            all?: boolean;
            /** @description 이름이나 id */
            nodes?: string[];
        };
        /** @description 노드 하나의 chassis 상태 */
        SdnChassisView: {
            name: string;
            /** Format: uuid */
            node: string;
            sdn?: components["schemas"]["NodeSdn"];
        };
        /** @description `POST /alert-rules/sdn-defaults` 본문 */
        SdnDefaultsRequest: {
            /** @description 알릴 `NotificationTarget` 이름(1~16개) */
            targets: string[];
        };
        /** @description `POST /alert-rules/sdn-defaults` 응답 */
        SdnDefaultsResponse: {
            /** @description 만든 규칙 */
            created: components["schemas"]["Resource_AlertRuleSpec_AlertRuleStatus"][];
            /** @description 같은 이름의 규칙이 이미 있어 그대로 둔 것 */
            existing: string[];
        };
        /** @description `POST /sdn`: 노드 `node`에 첫 중앙을 세운다. */
        SdnInitRequest: {
            /**
             * Format: ip
             * @description NB·SB가 들을 주소. 없으면 노드 주소의 IP
             */
            address?: string;
            /** @description 이름이나 id */
            node: string;
        };
        /** @description `POST /sdn/lockdown`(계획 67 §10.4) */
        SdnLockdownRequest: {
            /** @description 켜기(참)·끄기(거짓) */
            enabled: boolean;
            /** @description 허용 집합 밖에서 중앙에 붙어 있는 상대가 있어도 켠다(§10.4 — 그 상대는 끊긴다). */
            force?: boolean;
        };
        /** @description SDN(OVN) 네트워크. 칸이 없으면 오버레이(Geneve) 네트워크다. */
        SdnNet: {
            /** @description 공급자(외부) 네트워크(계획 66): 노드의 물리 망(physnet)에 붙는 OVN `localnet` 네트워크. 없으면 쓰지 않는다. */
            provider?: components["schemas"]["Provider"];
        };
        /** @description `POST /sdn/centrals` */
        SdnNodeRequest: {
            /** @description 이름이나 id */
            node: string;
        };
        /** @enum {string} */
        SdnPhase: "bootstrapping" | "ready";
        /** @description 중앙 목록 밖의 OVN Raft 멤버(일 수 있는 노드) */
        SdnStrayView: {
            /** @description 클러스터에서 이미 지운 노드다(살아 있는 중앙이 Raft에서 내쫓는다). */
            gone?: boolean;
            name: string;
            /** Format: uuid */
            node: string;
        };
        /** @description 리더 `OvnSync`가 NB에 쓴 마지막 기록 */
        SdnSync: {
            /**
             * Format: int64
             * @description 마지막으로 맞춘 시각(유닉스 초). 성공이 이어지면 60초 넘게 낡을 때만 바꾼다.
             * @default 0
             */
            at: number;
            /** @description 마지막 차례가 실패한 이유 */
            error?: string;
            /** @description 마지막 차례에서 실패한 묶음(`network tenant-a`, `port-group web,ssh` 따위, 계획 67). 비면 쓰지 않는다. */
            failed?: string[];
            /**
             * Format: int64
             * @description NB `NB_Global.hv_cfg`: 모든 chassis가 반영한 `nb_cfg`
             * @default 0
             */
            hv_cfg: number;
            /**
             * Format: int64
             * @description 읽은 Raft 로그 인덱스(펜싱 토큰의 뒤)
             * @default 0
             */
            index: number;
            /**
             * Format: int64
             * @description NB `NB_Global.nb_cfg`(쓸 때마다 1 올림)
             * @default 0
             */
            nb_cfg: number;
            /**
             * Format: int64
             * @description 쓴 리더의 Raft 임기(펜싱 토큰의 앞)
             * @default 0
             */
            term: number;
        };
        /** @description `GET /sdn` */
        SdnView: {
            centrals: components["schemas"]["SdnCentralView"][];
            /** @description 위트니스가 아닌 노드마다(이름 순)의 chassis 상태 */
            chassis: components["schemas"]["SdnChassisView"][];
            /** Format: uuid */
            init_node?: string;
            last_error?: string;
            /** @description OVN 접근 잠금(계획 67 §10.5). 켠 적이 없으면 없다. */
            lockdown?: components["schemas"]["LockdownView"];
            /** @description 없으면 SDN을 아직 설정하지 않았다(`sdn init` 전). */
            phase?: components["schemas"]["SdnPhase"];
            release?: string;
            /**
             * @description 중앙 목록에 없지만 OVN Raft에 남았을 수 있는 노드(이 SDN의 DB를 가진 노드와 클러스터에서 지운 노드 — 리뷰 2
             *     N-2·웹 I-3). `DELETE /sdn/centrals/{node}`로 뺀다.
             */
            strays?: components["schemas"]["SdnStrayView"][];
            sync?: components["schemas"]["SdnSync"];
            /** @description 끝나지 않은 SDN Task */
            tasks: string[];
        };
        /**
         * @description 둘째 단계 방법
         * @enum {string}
         */
        SecondFactorMethod: "totp" | "recovery" | "webauthn";
        /** @description 로그인 응답의 둘째 단계 안내. 쿠키는 대기 세션이고 [`PENDING_LOGIN_SECS`] 안에 끝내야 한다. */
        SecondFactorPrompt: {
            /** @description 정책이 요구하는데 요소가 없다: `/auth/2fa/totp`·`/auth/2fa/webauthn/register`로 등록하면 로그인이 끝난다. */
            enroll: boolean;
            /**
             * Format: int64
             * @description 대기 세션이 끝나는 시각(유닉스 초)
             */
            expires_at: number;
            /** @description 쓸 수 있는 방법. `enroll`이면 비어 있다. */
            methods: components["schemas"]["SecondFactorMethod"][];
            /** @description 패스키 로그인 옵션(`navigator.credentials.get({publicKey})`). 패스키가 없거나 이 접속(IP 주소)에서 쓸 수 없으면 없다. */
            webauthn?: components["schemas"]["RequestOptions"];
        };
        /** @description 둘째 단계: 셋 가운데 하나만 */
        SecondFactorRequest: {
            /** @description 복구 코드(`xxxx-xxxx-xxxx-xxxx`). 한 번 쓰면 없어진다. */
            recovery?: string;
            /** @description 인증 앱의 6자리 코드 */
            totp?: string;
            /** @description 패스키 응답(`navigator.credentials.get`) */
            webauthn?: components["schemas"]["AssertionCredential"];
        };
        /** @description 보안 그룹을 건 NIC 수: 단순 NIC(노드 nftables)와 SDN NIC(OVN ACL) */
        SecurityGroupNics: {
            sdn: number;
            simple: number;
        };
        /** @description 보안 그룹에서 바꿀 값. 주어진 필드만 바꾼다. */
        SecurityGroupPatch: {
            /** @description 빈 글이면 설명을 지운다. */
            description?: string;
            /** @description 규칙 전체를 이것으로 바꾼다. */
            rules?: components["schemas"]["Rule"][];
        };
        SecurityGroupSpec: {
            /** @description 200자 이하 */
            description?: string;
            /** @description 적힌 순서대로 보고 처음 맞는 규칙을 따른다(200개 이하). */
            rules?: components["schemas"]["Rule"][];
        };
        SecurityGroupStatus: {
            /** @description 이 그룹을 건 VM NIC 수(계획 66 §11.4). API가 응답에서만 채운다(Raft에 쓰지 않는다). */
            nics?: components["schemas"]["SecurityGroupNics"];
        };
        /** @description 요청을 받은 노드. */
        ServingNode: {
            /** Format: uuid */
            id: string;
            name: string;
        };
        /** @description 설정에서 바꿀 값. 주어진 필드만 바꾼다. */
        SettingsPatch: {
            /** @description 노드 API 인증서에 더할 이름(DNS 이름·IP) 전체. `[]`는 지워 노드 이름만 쓴다. */
            api_names?: string[];
            /**
             * @description 게스트 CPU(계획 63): 클러스터 기준 모드·고정 기준·자동 플래그·숨김 목록. 준 칸만 바꾼다. 실행 중 VM은 다음 전원
             *     껐다 켜기까지 그대로이고 응답의 `affected_running_vms`가 그 수다. 모든 노드가 `cpu-models`를 알아야 한다.
             */
            cpu?: components["schemas"]["CpuSettingsPatch"];
            /**
             * @description HA 복구를 위해 비워 둘 용량: `{"nodes": N}`(메모리가 가장 큰 노드 N대, 1~8), `{"percent": P}`(1~90),
             *     `"none"`(예약 없음). HA VM을 켤 때 넘으면 409 `ha_capacity`다.
             */
            ha_reserve?: components["schemas"]["HaReservePatch"];
            /**
             * Format: int32
             * @description HA 복구 때 대상 노드 하나에 한꺼번에 옮겨 켜는 VM 수(1~32, 기본 4)
             */
            ha_start_concurrency?: number;
            /** @description 최적 배치: 점수 정책, 균형 조정기의 동작, 불균형 문턱. 준 값만 바꾼다. */
            placement?: components["schemas"]["PlacementPatch"];
            /** @description Prometheus 끝점(`GET /api/v1/metrics`). 모든 노드가 `metrics-history`를 알아야 한다. */
            prometheus?: components["schemas"]["PrometheusPatch"];
            /**
             * @description 호스트가 꺼지는 중(재부팅·전원 끄기)일 때 노드가 HA VM을 다루는 방법. `conditional`(기본: 재부팅이면
             *     `freeze`, 끄기면 `failover`), `freeze`, `failover`, `migrate`. `systemctl stop`에는 쓰지 않는다.
             */
            shutdown_policy?: components["schemas"]["ShutdownPolicy"];
            telemetry?: components["schemas"]["TelemetryPatch"];
            /**
             * @description 2단계 인증 정책(계획 62) 전체: 전역 역할(`roles`)·영역(`realms`, 로컬 사용자는 `local`)에 요구하고, 패스키의
             *     RP ID(`rp_id`)를 정한다. `{}`는 정책을 지운다. OIDC 영역은 400(IdP가 MFA를 맡는다). 모든 노드가
             *     `two-factor`를 알아야 한다.
             */
            two_factor?: components["schemas"]["TwoFactorPolicy"];
            updates?: components["schemas"]["UpdateSettingsPatch"];
            /**
             * Format: int32
             * @description 투표 멤버 상한: 3, 5, 7 가운데 하나(위트니스도 센다). 리더가 learner를 올리거나 투표 멤버를 learner로 내려
             *     맞춘다.
             */
            voters?: number;
            /**
             * @description 쿠키 요청을 받는 다른 출처(`https://portal.example.com`) 전체. `[]`는 지워 같은 출처만 받는다.
             *     `https://`만 받는다(개발용 `http://localhost`·`127.0.0.1`·`[::1]`은 예외).
             */
            web_origins?: string[];
        };
        SettingsPatched: {
            /** Format: uuid */
            id: string;
            labels?: {
                [key: string]: string;
            };
            name: string;
            /**
             * Format: int64
             * @description 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
             */
            revision: number;
            spec: components["schemas"]["SettingsSpec"];
            status: components["schemas"]["SettingsStatus"];
        } & {
            /**
             * Format: int32
             * @description running VMs whose CPU changes at their next power cycle (only when cpu was given)
             */
            affected_running_vms?: number;
        };
        SettingsSpec: {
            /**
             * @description 앞단·VIP에서 API를 부르는 이름(DNS 이름·IP). 각 노드가 자기 API 인증서에 더한다(피어 인증서는 그대로).
             *     비어 있으면 쓰지 않는다.
             */
            api_names?: string[];
            /**
             * @description 게스트 CPU 클러스터 기준과 숨김 목록(계획 63). 없으면 기준 없음·숨김 없음이고 쓰지 않아 옛 노드도 설정을 계속
             *     푼다.
             */
            cpu?: components["schemas"]["CpuSettings"];
            /** @description HA 복구를 위해 비워 두는 용량. 없으면 예약하지 않고(`none`), 쓰지 않아 옛 노드도 설정을 계속 푼다. */
            ha_reserve?: components["schemas"]["HaReserve"];
            /**
             * Format: int32
             * @description 리더가 HA 복구 때 대상 노드 하나에 한꺼번에 옮겨 켜는 VM 수(1~32). 없으면 4이고 쓰지 않는다.
             */
            ha_start_concurrency?: number;
            /**
             * @description 이미지 라이브러리의 관리자 항목(계획 58, 200개까지, 내장 id와 겹치지 않게). 비어 있으면 쓰지 않아 옛 노드도
             *     설정을 계속 푼다.
             */
            image_catalog?: components["schemas"]["CatalogImage"][];
            /**
             * @description 최적 배치(계획 53): 점수 정책과 균형 조정기. 없으면 기본값(`balance`, `recommend`, 15%)이고 쓰지 않아 옛
             *     노드도 설정을 계속 푼다.
             */
            placement?: components["schemas"]["PlacementSettings"];
            /** @description Prometheus 끝점 `GET /api/v1/metrics`(계획 60). 없으면 꺼짐이고 쓰지 않아 옛 노드도 설정을 계속 푼다. */
            prometheus?: components["schemas"]["PrometheusSettings"];
            /**
             * @description 호스트가 꺼지는 중(재부팅·전원 끄기)일 때 노드가 HA VM을 다루는 방법. 없으면 `conditional`이고, 쓰지 않아
             *     옛 노드도 설정을 계속 푼다.
             */
            shutdown_policy?: components["schemas"]["ShutdownPolicy"];
            /** @description 옵트인 텔레메트리. 기본값(꺼짐, 끝점 없음)이면 쓰지 않아 옛 노드도 설정을 계속 푼다. */
            telemetry?: components["schemas"]["TelemetrySettings"];
            /** @description 2단계 인증 정책(계획 62). 없으면 아무에게도 요구하지 않고 쓰지 않아 옛 노드도 설정을 계속 푼다. */
            two_factor?: components["schemas"]["TwoFactorPolicy"];
            updates?: components["schemas"]["UpdateSettings"];
            /**
             * Format: int32
             * @description 투표 멤버 상한: 3, 5, 7 가운데 하나. 없으면 5이고, 쓰지 않아 옛 노드도 설정을 계속 푼다. 위트니스도
             *     센다. 노드가 이보다 적으면 모두 투표 멤버다.
             */
            voters?: number;
            /**
             * @description 쿠키 요청을 받는 다른 출처(`https://portal.example.com`). 따로 띄운 웹 포털이 API를 부를 때 쓴다.
             *     비어 있으면 같은 출처만 받고, 쓰지 않아 옛 노드도 설정을 계속 푼다.
             */
            web_origins?: string[];
        };
        SettingsStatus: {
            /** @description 텔레메트리 상태. 처음 켤 때 생기고, 없으면 쓰지 않는다. */
            telemetry?: components["schemas"]["TelemetryStatus"];
        };
        /**
         * @description 심각도(낮은 것부터)
         * @enum {string}
         */
        Severity: "info" | "warning" | "critical";
        /**
         * @description 호스트 종료 때 노드가 한 일
         * @enum {string}
         */
        ShutdownAction: "freeze" | "failover" | "migrate";
        /**
         * @description 노드 정상 종료 정책(설계 §8). `systemctl stop`·업그레이드 재시작이 아니라 호스트가 꺼지는 중일 때만 쓴다.
         * @enum {string}
         */
        ShutdownPolicy: "conditional" | "freeze" | "failover" | "migrate";
        /** @description 메일(일반 텍스트 본문) */
        SmtpTarget: {
            /** @description 보내는 주소 */
            from: string;
            host: string;
            /** @description AUTH 비밀번호. API는 내보내지 않는다. */
            password?: string;
            /** Format: int32 */
            port: number;
            tls?: components["schemas"]["SmtpTls"];
            /** @description 받는 주소(1~20개) */
            to: string[];
            /** @description AUTH 사용자. 없으면 인증하지 않는다. */
            user?: string;
        };
        /** @description [`SmtpTarget`]에서 `password`를 뺀 것 */
        SmtpTargetView: {
            from: string;
            has_password: boolean;
            host: string;
            /** Format: int32 */
            port: number;
            tls: components["schemas"]["SmtpTls"];
            to: string[];
            user?: string;
        };
        /**
         * @description SMTP 연결 보안
         * @enum {string}
         */
        SmtpTls: "starttls" | "tls" | "none";
        /** @enum {string} */
        SnapshotPhase: "pending" | "ready";
        /** @description 예약 스냅샷: 주기마다 스냅샷을 만들고, 예약으로 만든 것을 `keep`개만 남긴다. */
        SnapshotSchedule: {
            every: components["schemas"]["ScheduleEvery"];
            /**
             * Format: int32
             * @description 1부터 `MAX_SCHEDULE_KEEP`까지
             */
            keep: number;
            /** @description 실행 중이면 메모리도 저장한다. */
            memory?: boolean;
        };
        SnapshotSpec: {
            description?: string;
            /**
             * @description 메모리 스냅샷이면 그때 도는 QEMU의 하드웨어(계획 54, 모든 노드가 `hotplug`일 때만). 메모리를 불러오려면 같은
             *     장치로 켜야 하므로 되돌리기는 이 모양(핫플러그 배치 있음·없음, 상한)으로 켠다. 없으면 배치 없이 켠다.
             */
            hardware?: components["schemas"]["RunningHardware"];
            /** @description 메모리 상태를 담았는지. 담았으면 VM의 첫 디스크에 있다. */
            memory: boolean;
            /** @description 사용자가 붙인 이름. VM 안에서 고유하다. */
            name: string;
            /**
             * Format: uuid
             * @description 부모 스냅샷 id. VM의 첫 스냅샷이면 없다.
             */
            parent?: string | null;
            /** @description 예약으로 만들었는지. 보관 개수 정리는 이것만 지운다. */
            scheduled?: boolean;
            /**
             * Format: uuid
             * @description VM id
             */
            vm: string;
            /** @description 만든 순간의 VM 스펙 */
            vm_spec: components["schemas"]["VmSpec"];
        };
        SnapshotStatus: {
            /**
             * Format: int64
             * @description 만든 시각(유닉스 초)
             * @default null
             */
            created_at: number | null;
            /** @default pending */
            phase: components["schemas"]["SnapshotPhase"];
            /**
             * Format: uuid
             * @description TPM 상태 사본(`{스냅샷 id}.tpm`)을 둔 노드(계획 59). 되돌리기는 이 노드에서만 TPM 상태를 돌려놓는다. 찍을 때
             *     TPM 상태가 없었으면(TPM이 없거나 한 번도 켜지 않은 VM) 없다.
             */
            tpm_node?: string;
            /**
             * Format: int64
             * @description 메모리 상태 크기. 메모리 스냅샷만 있다.
             * @default null
             */
            vmstate_bytes: number | null;
        };
        SourceDisk: {
            /** Format: int64 */
            capacity_bytes: number;
            /** @description 원본 컨트롤러(`ide`, `sata`, `scsi`, `nvme`) */
            controller: string;
            /** @description 원본 안의 디스크 파일 이름 */
            file: string;
            /** @description `disk{i}` */
            name: string;
        };
        /**
         * @description 가져올 원본 이미지 형식
         * @enum {string}
         */
        SourceFormat: "vmdk" | "qcow2" | "raw";
        SourceNic: {
            /** @description 원본 어댑터 종류(`vmxnet3`, `e1000` 등) */
            adapter: string;
            mac?: components["schemas"]["MacAddr"] | null;
            /** @description 원본 네트워크 이름 */
            network: string;
        };
        /** @description 원본 VM에서 읽은 것 */
        SourceVm: {
            /** Format: int32 */
            cores_per_socket: number;
            /** @description 디스크(컨트롤러 순서, 그 안에서 주소 순서) */
            disks: components["schemas"]["SourceDisk"][];
            firmware: components["schemas"]["Firmware"];
            /** @description 게스트 OS(OVF `vmw:osType` 등) */
            guest_os: string;
            /** Format: int64 */
            memory_mib: number;
            name: string;
            nics: components["schemas"]["SourceNic"][];
            /** Format: int32 */
            vcpus: number;
        };
        /** @description 범위 전체의 요약. 아는 칸이 없으면 `null`이다. */
        Stat: {
            /**
             * Format: float
             * @description 아는 칸 수로 가중한 평균
             */
            avg?: number | null;
            /**
             * Format: float
             * @description MAX 열의 최대
             */
            max?: number | null;
            /**
             * Format: float
             * @description MIN 열의 최소
             */
            min?: number | null;
        };
        /** @description 정적 경로 */
        StaticRoute: {
            /** @example 10.20.0.0/16 */
            destination: string;
            /**
             * Format: ip
             * @description 다음 홉(붙은 서브넷 안의 주소, 같은 체계)
             */
            nexthop: string;
        };
        StoragePoolSpec: {
            backend: components["schemas"]["PoolBackend"];
            /** @description 백업 파일(`{루트}/backups/`)을 둘 수 있는 백업 풀인지. dir·NFS 풀만 된다. 켜도 VM 디스크는 그대로 둔다. */
            backups?: boolean;
            /**
             * Format: uuid
             * @description 든 스토리지 폴더(계획 62, `Folder` id). 없으면 스토리지 트리의 루트이고 쓰지 않아 옛 노드도 풀을 푼다.
             */
            folder?: string;
            /** @description 이 풀을 쓸 수 있는 노드. 비어 있으면 모든 노드. */
            nodes?: string[];
            /** @description 모든 노드가 같은 데이터를 보는 공유 스토리지인지. NFS·RBD·공유 LVM 풀은 늘 공유다(`is_shared`). */
            shared?: boolean;
        };
        StoragePoolStatus: {
            /**
             * Format: int64
             * @default null
             */
            available_bytes: number | null;
            /**
             * Format: int64
             * @default null
             */
            total_bytes: number | null;
        };
        /** @description 저장소가 사는 곳. JSON은 `{"pool": "bk"}` 또는 `{"s3": {...}}`이다. */
        StoreBackend: {
            /** @description 백업 풀(`backups`를 켠 dir·NFS 풀)의 이름. 경로는 `{풀 루트}/store/{저장소 id}/`. */
            pool: string;
        } | {
            s3: components["schemas"]["S3Target"];
        };
        /** @description 보기의 백엔드(`{"pool": "bk"}` 또는 `{"s3": {...}}`) */
        StoreBackendView: {
            pool: string;
        } | {
            s3: components["schemas"]["S3TargetView"];
        };
        /** @description 저장소 백업 하나가 읽고 쓴 양(계획 68 §8.4) */
        StoreBackupStats: {
            /**
             * Format: int64
             * @description 디스크 가상 크기 합
             * @default 0
             */
            logical_bytes: number;
            /**
             * Format: int64
             * @description 이 백업이 저장소에 처음 쓴 저장 바이트(압축·암호화 뒤). 할당량 `backup_bytes`가 센다.
             * @default 0
             */
            new_bytes: number;
            /**
             * Format: int64
             * @description 저장소에 처음 올린 청크 수
             * @default 0
             */
            new_chunks: number;
            /**
             * Format: int64
             * @description 이번에 읽은 바이트(증분이면 바뀐 청크만)
             * @default 0
             */
            read_bytes: number;
            /**
             * Format: int64
             * @description 이미 저장소에 있어 다시 쓰지 않은 청크 수
             * @default 0
             */
            reused_chunks: number;
            /**
             * Format: int64
             * @description 내용이 모두 0이라 저장하지 않은 청크 수
             * @default 0
             */
            zero_chunks: number;
        };
        /** @description 연결 확인 결과: 이 노드에서 곧바로 읽어 본 것과 노드마다 보고한 것 */
        StoreCheck: {
            /** @description API 노드가 그 저장소를 쓰지 못하면(다른 노드의 로컬 풀) 확인하지 않았다. */
            checked_here: boolean;
            error?: string;
            nodes: components["schemas"]["StoreNodeCheck"][];
            /** @description API 노드에서 `store.json`을 읽고 키가 맞았다. */
            ok: boolean;
        };
        /** @description 저장소 사본의 VM 칸(`config` 블롭과 같은 내용) */
        StoreCopyConfig: {
            tpm_state?: boolean;
            uefi_vars?: boolean;
            vm_name: string;
            vm_spec: components["schemas"]["VmSpec"];
        };
        /** @description 보기의 암호화: 키 지문만 */
        StoreEncryptionView: {
            fingerprint: string;
        };
        /** @description 노드 하나가 저장소에 닿는지(노드 보고 `NodeStatus.backup_stores`) */
        StoreNodeCheck: {
            /** Format: int64 */
            checked_at: number;
            error?: string;
            name: string;
            /** Format: uuid */
            node: string;
            ok: boolean;
        };
        /** @description 저장소의 주인 클러스터(`owner.json`) */
        StoreOwner: {
            /** @description 주인 클러스터 CA의 지문(`sha256:…`) */
            ca_fingerprint: string;
            cluster: string;
        };
        /** @description 저장소에서 바꿀 값. 주어진 것만 바꾼다. 백엔드·암호화·읽기 전용은 바꾸지 못한다(읽기 전용은 `take-over`). */
        StorePatch: {
            /** @description S3 저장소만. `null`이면 시스템 루트로 돌아간다. */
            ca_pem?: string | null;
            gc?: components["schemas"]["JobSchedule"];
            /** Format: int32 */
            max_streams?: number;
            /** @description S3 저장소만 */
            s3_credentials?: components["schemas"]["S3Credentials"];
            verify?: components["schemas"]["VerifyPolicy"] | null;
        };
        /** @enum {string} */
        StorePhase: "initializing" | "ready" | "failed";
        /** @description 만들기의 스펙(암호화는 [`CreateStoreRequest::encryption`]으로 따로 — 키를 본문에 받지 않는다) */
        StoreSpecRequest: {
            backend: components["schemas"]["StoreBackend"];
            /** @description GC 일정(UTC). 없으면 매일 05:00. */
            gc?: components["schemas"]["JobSchedule"];
            /**
             * Format: int32
             * @description S3 동시 요청 수(1~64, 기본 8)
             */
            max_streams?: number;
            /** @description 다른 클러스터가 주인인 저장소를 복원용으로 붙인다(백업·지우기·GC 막힘). */
            read_only?: boolean;
            /**
             * Format: uuid
             * @description 읽기 전용으로 붙이는 백업 풀 안 저장소의 id(`{풀 루트}/store/<id>/` — 다른 클러스터가 만든 디렉터리). S3는 쓰지
             *     않는다.
             */
            store_id?: string;
            /** @description 예약 검증. 없으면 예약 검증을 하지 않는다. */
            verify?: components["schemas"]["VerifyPolicy"];
        };
        /** @description 옮기기·복사의 대상 저장소 */
        StoreTargetRequest: {
            /** @description 백업 저장소 이름 */
            store: string;
        };
        /** @description 저장소 사용량(GC가 셈) */
        StoreUsage: {
            /**
             * Format: int64
             * @description 참조되는 청크 수
             * @default 0
             */
            chunks: number;
            /**
             * Format: int64
             * @description 스냅샷 디스크 가상 크기 합(dedup 배율 = logical / stored)
             * @default 0
             */
            logical_bytes: number;
            /**
             * Format: int64
             * @description 센 시각(유닉스 초)
             * @default 0
             */
            measured_at: number;
            /**
             * Format: int64
             * @description 스냅샷(완성된 백업) 수
             * @default 0
             */
            snapshots: number;
            /**
             * Format: int64
             * @description 청크가 차지한 바이트(압축·암호화 뒤)
             * @default 0
             */
            stored_bytes: number;
        };
        /** @description 저장소 동작 본문(검증만 쓴다) */
        StoreVerifyRequest: {
            /** @description 검증할 백업(이름이나 id). 비면 일정의 규칙대로 고른다(검증한 적 없거나 오래된 백업). */
            backups?: string[];
        };
        /**
         * @description 경보 대상의 종류
         * @enum {string}
         */
        SubjectKind: "node" | "vm" | "pool" | "task" | "backup_job" | "sdn" | "router" | "load_balancer" | "backup" | "backup_store";
        /** @description SDN 네트워크의 서브넷: 주소를 Orbivisor IPAM(`NicSpec.ips`)이 정하고 OVN 내장 DHCP가 준다. */
        Subnet: {
            /**
             * @description 범위(IPv4 `/8`~`/29`, IPv6 `/48`~`/124`). 접두사 밖 비트가 켜져 있으면 받지 않는다.
             * @example 10.10.0.0/24
             */
            cidr: string;
            description?: string;
            /** @description OVN DHCP로 주소를 준다. 없으면 IPv4는 참, IPv6는 거짓(IPv6 DHCP는 아직 없어 거짓만 받는다). */
            dhcp?: boolean;
            /** @description DHCP·cloud-init이 알려 줄 DNS 서버(4개까지) */
            dns_servers?: string[];
            /**
             * @description 라우터 외부 주소와 유동 IP를 고르는 범위(계획 66, 공급자 네트워크의 IPv4 서브넷만, 16개까지, `pools`·게이트웨이와
             *     겹치지 않게). 없으면 `pools`에서 고른다.
             */
            floating_pools?: components["schemas"]["IpRange"][];
            /**
             * Format: ip
             * @description 게이트웨이 주소. 없으면 첫 호스트 주소다. 라우터가 없어도 늘 예약해 VM에 주지 않는다.
             */
            gateway?: string;
            /** @description IPv6 주소 알리기(계획 66, IPv6 서브넷만). 없으면 cloud-init 고정 주소다. */
            ipv6_mode?: components["schemas"]["Ipv6Mode"];
            /** @description 자동 할당 범위(16개까지, 겹치지 않게). 없으면 CIDR 전체에서 예약을 뺀 것이다. */
            pools?: components["schemas"]["IpRange"][];
        };
        /** @description 서브넷 하나의 크기와 사용 */
        SubnetUsage: {
            cidr: string;
            /** @description 유동 IP 풀(계획 66 — 라우터 외부 주소와 유동 IP의 자리)의 크기·사용·남음. 풀이 없으면 없다. */
            floating?: components["schemas"]["FloatingUsage"];
            /**
             * Format: int64
             * @description 자동 할당 범위에서 남은 주소 수
             */
            free: number;
            /**
             * Format: int64
             * @description 자동 할당 범위의 주소 수(예약 빼고, 아주 크면 u64 끝에서 멈춤)
             */
            size: number;
            /**
             * Format: int64
             * @description 이 서브넷에서 NIC·라우터 외부·유동 IP가 쓰는 주소 수(풀 밖 주소 포함)
             */
            used: number;
        };
        /** @description 읽을 때 계산한 구독 상태. 키 문자열은 담지 않는다. */
        SubscriptionInfo: {
            customer?: string;
            /** @description 개발 키로 검증됨 */
            development?: boolean;
            /**
             * Format: int64
             * @description 만료 시각(유닉스 초)
             */
            expires_at?: number;
            id?: string;
            level?: components["schemas"]["Level"];
            /**
             * Format: int32
             * @description 노드 수 한도
             */
            nodes?: number;
            state: components["schemas"]["SubscriptionState"];
            /**
             * Format: int32
             * @description 위트니스가 아닌 노드 수
             */
            used_nodes: number;
        };
        /** @description 넣을 구독 키 */
        SubscriptionKey: {
            /** @description `opl1.<payload>.<signature>` */
            key: string;
        };
        /**
         * @description 구독 상태
         * @enum {string}
         */
        SubscriptionState: "none" | "active" | "expired" | "invalid" | "over_limit";
        /** @description 넘겨받기 본문 */
        TakeOverRequest: {
            /** @description 주인의 하트비트가 24시간 안이어도 넘겨받는다(옛 주인이 아직 살아 있으면 두 클러스터가 서로의 백업을 지울 수 있다). */
            force?: boolean;
        };
        /** @description 보내는 방법 */
        TargetConfig: (components["schemas"]["SmtpTarget"] & {
            /** @enum {string} */
            kind: "smtp";
        }) | (components["schemas"]["WebhookTarget"] & {
            /** @enum {string} */
            kind: "webhook";
        });
        TargetConfigView: (components["schemas"]["SmtpTargetView"] & {
            /** @enum {string} */
            kind: "smtp";
        }) | (components["schemas"]["WebhookTargetView"] & {
            /** @enum {string} */
            kind: "webhook";
        });
        TaskOp: {
            /** @enum {string} */
            kind: "vm-start";
        } | {
            /** @enum {string} */
            kind: "vm-stop";
        } | {
            /** @enum {string} */
            kind: "vm-force-stop";
        } | {
            /** @enum {string} */
            kind: "vm-reboot";
        } | {
            /** @enum {string} */
            kind: "volume-create";
        } | {
            /** @enum {string} */
            kind: "volume-import";
            source: string;
            source_format: components["schemas"]["SourceFormat"];
        } | {
            /** @description 체크섬 파일 URL(계획 58). 받기 전에 이 파일에서 이미지 파일 이름의 해시를 찾아 확인한다. */
            checksum_url?: string;
            /** @enum {string} */
            kind: "volume-download";
            /** @description 받은 파일의 SHA-256(소문자 16진 64자). 없으면 확인하지 않는다. */
            sha256?: string;
            /** @description 받은 파일의 SHA-512(소문자 16진 128자, 계획 58). `sha256`·`checksum_url`과 함께 쓰지 않는다. */
            sha512?: string;
            source_format: components["schemas"]["SourceFormat"];
            /** @description 가져오기가 끝나면 이 볼륨으로 만들 템플릿 VM(계획 58) */
            template?: components["schemas"]["TemplateDraft"];
            url: string;
        } | {
            /** @enum {string} */
            kind: "volume-delete";
        } | {
            /** @enum {string} */
            kind: "volume-flatten";
        } | {
            /** @enum {string} */
            kind: "snapshot-create";
            /** Format: uuid */
            snapshot: string;
        } | {
            /** @enum {string} */
            kind: "snapshot-revert";
            /** Format: uuid */
            snapshot: string;
        } | {
            /** @enum {string} */
            kind: "snapshot-delete";
            /** @description 예약 정리가 만든 삭제면 true. 이 삭제의 실패만 `schedule_error`에 남긴다. */
            scheduled?: boolean;
            /** Format: uuid */
            snapshot: string;
        } | {
            /** @enum {string} */
            kind: "volume-resize";
            /** Format: int64 */
            size_bytes: number;
        } | {
            /** @enum {string} */
            kind: "vm-clone";
            /**
             * @description 연결 복제(계획 58, 템플릿에서만): 없음 = 되는 디스크는 연결하고 안 되는 디스크는 전체 복사, 참 = 모두
             *     연결(안 되면 실패), 거짓 = 전체 복사. 없으면 쓰지 않는다.
             */
            linked?: boolean;
            name: string;
            /** @description 배포 때 바꾸는 값(계획 58). 없으면 쓰지 않는다. */
            overrides?: components["schemas"]["CloneOverrides"];
            pool?: string | null;
            /** Format: uuid */
            snapshot?: string | null;
        } | {
            /** @enum {string} */
            kind: "vm-migrate";
            /** Format: uuid */
            node: string;
            /**
             * @description 로컬 디스크를 둘 `node`의 풀. 없으면 디스크마다 같은 이름의 풀이다. 없으면 빼고 쓰므로
             *     옛 노드도 읽는다.
             */
            pool?: string;
        } | {
            /** Format: uuid */
            from: string;
            /** @enum {string} */
            kind: "vm-ha-recover";
        } | {
            /** Format: uuid */
            backup: string;
            /** @enum {string} */
            kind: "vm-backup";
        } | {
            /** @enum {string} */
            kind: "backup-delete";
        } | {
            /** Format: uuid */
            backup: string;
            /**
             * Format: uuid
             * @description 새 VM의 폴더(계획 62). API가 정한다: 요청의 폴더, 없으면 백업한 VM의 폴더(아직 있으면). 없으면 루트이고
             *     쓰지 않아 옛 노드도 푼다.
             */
            folder?: string;
            /** @enum {string} */
            kind: "backup-restore";
            name: string;
            pool: string;
        } | {
            /** @enum {string} */
            kind: "backup-replicate";
            pool: string;
            remote: string;
        } | {
            /** Format: uuid */
            import: string;
            /** @enum {string} */
            kind: "vm-import";
            run: components["schemas"]["ImportRun"];
        } | {
            /** @enum {string} */
            kind: "vm-import-delete";
        } | {
            /** @enum {string} */
            kind: "node-update-check";
        } | {
            /** @enum {string} */
            kind: "node-upgrade";
        } | {
            /** @enum {string} */
            kind: "node-cpu-probe";
        } | {
            /** @enum {string} */
            kind: "node-drain";
        } | {
            /** @enum {string} */
            kind: "node-power-off";
            method?: components["schemas"]["FenceKind"];
            reason: string;
        } | {
            action: components["schemas"]["HaCheckAction"];
            check: string;
            detail: string;
            /** @enum {string} */
            kind: "ha-check";
            vms?: string[];
        } | {
            bond: string;
            /** @enum {string} */
            kind: "bond-degraded";
            ports: string[];
        } | {
            /** @enum {string} */
            kind: "vm-hotplug";
        } | {
            /** @enum {string} */
            kind: "vm-replicate";
            /** Format: uuid */
            node: string;
        } | {
            /** @enum {string} */
            kind: "ceph-init";
        } | {
            /** @enum {string} */
            kind: "ceph-mon-add";
        } | {
            /** @enum {string} */
            kind: "ceph-mgr-add";
        } | {
            device: string;
            /** @enum {string} */
            kind: "ceph-osd-add";
            wipe?: boolean;
        } | {
            /** @enum {string} */
            kind: "ceph-pool-create";
            name: string;
            pool: string;
            /** Format: int32 */
            size: number;
        } | {
            /** @enum {string} */
            kind: "ceph-client-install";
        } | {
            /** @enum {string} */
            kind: "vm-untemplate";
        } | {
            /** @enum {string} */
            kind: "vm-firmware-reset";
            /**
             * @description 있으면 Task가 VM 스펙의 `secure_boot`도 이 값으로 바꾼다(UEFI 변수 초기화와 함께, 리뷰 1 M1). 스펙과 초기화를
             *     한 Task가 쓰므로 API가 실패해도 둘 중 하나만 적용되지 않는다.
             */
            secure_boot?: boolean | null;
            tpm?: boolean;
            uefi_vars?: boolean;
        } | {
            arch: components["schemas"]["Arch"];
            current?: string;
            /** @enum {string} */
            kind: "cpu-baseline";
            limited_by?: string[];
            mode?: components["schemas"]["BaselineSource"];
            previous?: string;
            /** Format: int32 */
            running_vms?: number;
        } | {
            /** Format: ip */
            address?: string;
            /** @enum {string} */
            kind: "sdn-init";
        } | {
            /** @enum {string} */
            kind: "sdn-central-add";
        } | {
            /** @enum {string} */
            kind: "sdn-central-remove";
        } | {
            /** Format: ip */
            address: string;
            /** @enum {string} */
            kind: "sdn-central-kick";
            /** Format: uuid */
            node: string;
        } | {
            /** @enum {string} */
            kind: "sdn-chassis-install";
        } | {
            enabled: boolean;
            /** @enum {string} */
            kind: "sdn-lockdown";
        } | {
            /** @enum {string} */
            kind: "network-migrate";
            router?: string;
            step: components["schemas"]["MigrationStep"];
            vms?: string[];
        } | {
            /** @enum {string} */
            kind: "backup-store-init";
        } | {
            /** @enum {string} */
            kind: "backup-store-gc";
        } | {
            backups?: string[];
            /** @enum {string} */
            kind: "backup-store-verify";
        } | {
            /** @enum {string} */
            kind: "backup-store-scan";
        } | {
            /** @enum {string} */
            kind: "backup-convert";
            store: string;
        } | {
            /** @enum {string} */
            kind: "backup-copy";
            store: string;
        } | {
            /** Format: uuid */
            backup: string;
            /**
             * Format: uuid
             * @description 새 VM의 폴더(지금 복원과 같다). 없으면 쓰지 않는다.
             */
            folder?: string;
            /** @enum {string} */
            kind: "backup-live-restore";
            name: string;
            pool: string;
        };
        /** @description Task의 진행률: 단계(`copying-disks`, `migrating-memory`)와 복사한 바이트 */
        TaskProgress: {
            /** Format: int64 */
            done_bytes: number;
            stage: string;
            /** Format: int64 */
            total_bytes: number;
        };
        TaskSpec: {
            /**
             * Format: uuid
             * @description 작업을 실행할 노드
             */
            node: string;
            op: components["schemas"]["TaskOp"];
            /**
             * Format: uuid
             * @description 대상 리소스(VM 또는 Volume)의 id. 스냅샷·복제·백업 Task는 (원본) VM, 백업 지우기는 백업,
             *     복원과 VM 가져오기는 새 VM, 가져오기 지우기는 가져오기다.
             */
            target: string;
        };
        /** @enum {string} */
        TaskState: "pending" | "running" | "succeeded" | "failed";
        TaskStatus: {
            /** @default null */
            error: string | null;
            /** @description 실행 중인 Task의 진행률(지금은 로컬 디스크를 복사하는 마이그레이션만). Task가 끝나면 지운다. */
            progress?: components["schemas"]["TaskProgress"];
            /** @default pending */
            state: components["schemas"]["TaskState"];
        };
        /** @description 텔레메트리에서 바꿀 값 */
        TelemetryPatch: {
            enabled?: boolean;
            /** @description 보낼 `https://` 주소. `""`는 지워 빌드 기본 끝점을 쓴다. */
            endpoint?: string;
        };
        /** @description 텔레메트리 보고. 맵은 종류 → 수다. */
        TelemetryReport: {
            /** @description 아키텍처별 노드 수(위트니스 제외, 모르면 `unknown`) */
            arch: {
                [key: string]: number;
            };
            /** Format: int64 */
            backup_jobs: number;
            /**
             * Format: int64
             * @description CPU 합(위트니스 제외)
             */
            cpus: number;
            /** @description 하이퍼바이저별 노드 수(위트니스 제외) */
            hypervisor: {
                [key: string]: number;
            };
            /**
             * Format: uuid
             * @description 설정 상태의 텔레메트리 id(아직 없으면 nil UUID)
             */
            id: string;
            /** @description 원본 종류별 VM 가져오기 수(`ova`, `esxi`) */
            imports: {
                [key: string]: number;
            };
            /**
             * Format: int64
             * @description 메모리 합(위트니스 제외, GiB 내림)
             */
            memory_gib: number;
            /** @description 종류별 네트워크 수: `vxlan`, `vlan`(브리지와 VLAN), `bridge` */
            networks: {
                [key: string]: number;
            };
            nodes: components["schemas"]["NodeCounts"];
            /** @description 백엔드 종류별 스토리지 풀 수 */
            pools: {
                [key: string]: number;
            };
            /**
             * Format: int32
             * @description 보고 형식 판(지금 1)
             */
            schema: number;
            /** Format: int64 */
            security_groups: number;
            /**
             * Format: int64
             * @description 보낸 시각(유닉스 초)
             */
            sent_at: number;
            /** @description 구독 수준(`standard`, `premium`). 서명이 맞으면 만료돼도 수준이고, 없으면 `none`. */
            subscription: string;
            /** @description 버전별 노드 수(위트니스 포함, 버전을 적지 않은 노드는 `unknown`) */
            versions: {
                [key: string]: number;
            };
            vms: components["schemas"]["VmCounts"];
        };
        /** @description 텔레메트리 설정. 켜면 리더가 하루 한 번 클러스터 규모(이름·주소 없음)를 끝점에 보낸다. */
        TelemetrySettings: {
            enabled?: boolean;
            /** @description 보낼 `https://` 주소. 없으면 빌드 값(`version::TELEMETRY_URL`) */
            endpoint?: string;
        };
        /** @description 텔레메트리 상태 */
        TelemetryStatus: {
            /**
             * Format: uuid
             * @description 보고에 붙는 무작위 id(UUID v4). 처음 켤 때 만들고, 꺼도 남는다.
             */
            id?: string;
            /** @description 마지막 보내기가 실패한 이유 */
            last_error?: string;
            /**
             * Format: int64
             * @description 마지막으로 보낸 시각(유닉스 초)
             */
            last_sent_at?: number;
        };
        /** @description 카탈로그 이미지를 받은 뒤 만들 템플릿 VM의 초안(`POST /image-catalog/{id}/download`의 `template`). */
        TemplateDraft: {
            /** @description 없으면 빈 설정(cloud-init 기본)이다. 비밀번호는 crypt 해시다. */
            cloud_init?: components["schemas"]["CloudInit"];
            /** @description 에뮬레이터 스레드 정책(계획 64) */
            cpu_emulator?: components["schemas"]["EmulatorPolicy"];
            /** @description CPU 플래그(`["-avx512vbmi"]`, 계획 63) */
            cpu_flags?: components["schemas"]["CpuFlag"][];
            /** @description CPU 모델(계획 63). 없으면 `host`다. API가 별칭을 버전 모델로 고정해 넣는다. */
            cpu_model?: components["schemas"]["CpuModel"];
            /** @description CPU 정책(계획 64). 없으면 `shared`다. */
            cpu_policy?: components["schemas"]["CpuPolicy"];
            /** @description 전용 CPU 스레드 정책(계획 64) */
            cpu_thread_policy?: components["schemas"]["ThreadPolicy"];
            /** @description 펌웨어. API가 카탈로그 항목의 것으로 채운다(없으면 UEFI). */
            firmware?: components["schemas"]["Firmware"];
            /** @description hugepages(계획 64) */
            hugepages?: components["schemas"]["PageSize"];
            /** @description 머신 타입. API가 카탈로그 항목의 아키텍처로 채운다(없으면 이 노드의 것). */
            machine?: components["schemas"]["MachineType"];
            /**
             * Format: int32
             * @default 2048
             */
            memory_mib: number;
            /** @description 템플릿 VM 이름 */
            name: string;
            /** @description NIC 하나의 네트워크 이름 */
            network: string;
            /** @description vNUMA(계획 64) */
            numa?: components["schemas"]["NumaSpec"];
            /**
             * Format: int64
             * @description 받은 디스크를 이 크기로 늘린다(바이트). 없으면 이미지 크기 그대로다.
             */
            size_bytes?: number;
            /**
             * Format: int32
             * @default 2
             */
            vcpus: number;
        };
        /**
         * @description 전용 VM의 SMT 형제 스레드 정책(OpenStack `hw:cpu_thread_policy`)
         * @enum {string}
         */
        ThreadPolicy: "prefer" | "isolate" | "require";
        /**
         * @description 기록 층. 칸 `b = floor(t / step)`(유닉스 기준)라서 모든 노드의 칸 경계가 같다.
         * @enum {string}
         */
        Tier: "raw5s" | "min1" | "min30" | "hour6";
        /** @description API 토큰의 권한 범위(설계 §6.6). `targets`마다 `privileges`를 건 propagate grant로 읽은 천장이다. */
        TokenScope: {
            /** @description 권한 이름 목록. 모르는 이름은 읽을 때 버린다. */
            privileges: string[];
            targets: components["schemas"]["GrantTarget"][];
        };
        TokenView: {
            /** Format: uuid */
            id: string;
            name: string;
            /** Format: int64 */
            revision: number;
            /** @description 권한 범위(계획 62). 없으면 주인 권한 그대로다. */
            scope?: components["schemas"]["TokenScope"];
            /** Format: uuid */
            user: string;
        };
        /** @description `Digest`의 상위 VM 하나(범위 평균) */
        TopVm: {
            /**
             * Format: float
             * @description 범위 평균 CPU %
             */
            cpu?: number | null;
            /**
             * Format: float
             * @description 범위 평균 RSS(MiB)
             */
            mem_rss?: number | null;
            /** Format: uuid */
            vm: string;
        };
        TopoExternal: {
            ips: string[];
            /**
             * Format: uuid
             * @description 공급자 네트워크 id
             */
            network: string;
        };
        TopoFip: {
            /** Format: ipv4 */
            address?: string;
            /** Format: uuid */
            id: string;
            /**
             * Format: uuid
             * @description 공급자 네트워크 id
             */
            network: string;
            /** @description `unattached`·`distributed`·`centralized`·`unrouted` */
            state: string;
            /**
             * Format: uuid
             * @description 붙은 VM(읽을 수 있을 때만)
             */
            vm?: string;
        };
        TopoGateway: {
            /** @description 게이트웨이 포트를 가진 노드 이름(루트 `cluster.read`가 없으면 빈 글) */
            node?: string;
            state: components["schemas"]["GatewayState"];
        };
        /** @description 읽을 수 없어 뺀 수 */
        TopoHidden: {
            floating_ips: number;
            load_balancers: number;
            networks: number;
            routers: number;
            vms: number;
        };
        TopoLb: {
            /** Format: ipv4 */
            external_ip?: string;
            /** Format: uuid */
            id: string;
            members: components["schemas"]["TopoLbMember"][];
            name: string;
            /**
             * Format: uuid
             * @description VIP 네트워크 id
             */
            network: string;
            /** Format: ipv4 */
            vip?: string;
        };
        TopoLbMember: {
            health: components["schemas"]["MemberHealth"];
            /** Format: ipv4 */
            ip?: string;
            /**
             * Format: uuid
             * @description 멤버 VM(읽을 수 있을 때만)
             */
            vm?: string;
        };
        /** @description 진행 중인 옮기기(원본 네트워크에) */
        TopoMigration: {
            /** Format: uuid */
            id: string;
            /** @description 옮긴 NIC 수 */
            moved: number;
            name: string;
            phase: components["schemas"]["MigrationPhase"];
            /**
             * Format: uuid
             * @description 대상 네트워크 id(읽을 수 없거나 없으면 없음)
             */
            target?: string;
            /** @description 옮길 NIC 수(막힌 것 빼고) */
            total: number;
        };
        TopoNetwork: {
            /** @description 서브넷 CIDR */
            cidrs: string[];
            /** Format: uuid */
            folder?: string;
            /** Format: uuid */
            id: string;
            kind: components["schemas"]["TopoNetworkKind"];
            migration?: components["schemas"]["TopoMigration"];
            /** Format: int32 */
            mtu?: number;
            name: string;
            physnet?: string;
            /**
             * Format: int32
             * @description VLAN 태그(VLAN·공급자)
             */
            vlan?: number;
        };
        /**
         * @description 네트워크의 모양
         * @enum {string}
         */
        TopoNetworkKind: "bridge" | "vlan" | "vxlan" | "sdn" | "provider";
        TopoNic: {
            ips: string[];
            /** Format: uuid */
            network: string;
        };
        TopoRouter: {
            external?: components["schemas"]["TopoExternal"];
            /** Format: uuid */
            folder?: string;
            gateway: components["schemas"]["TopoGateway"];
            /** Format: uuid */
            id: string;
            /** @description 인터페이스 네트워크 id(읽을 수 있는 것만) */
            interfaces: string[];
            name: string;
        };
        TopoVm: {
            /** Format: uuid */
            id: string;
            name: string;
            /** @description 그림에 있는 네트워크의 NIC */
            nics: components["schemas"]["TopoNic"][];
            /** @description 도는 노드 이름(루트 `cluster.read`가 없으면 없음) */
            node?: string;
            power: components["schemas"]["VmPhase"];
        };
        /** @description 노드에 직접 물은 지금 값(노드 사이 RPC `ReadTopology`, 설계 §5·§13). 바뀔 때마다 쓰지 않도록 Raft에는 두지 않는다. */
        TopologyLive: {
            /** @description orbivisord의 CPU 친화가 노드 설정(예약 CPU)과 맞다. */
            affinity_ok: boolean;
            /** @description 호스트 NUMA 노드마다(id 순) */
            nodes: components["schemas"]["LiveNode"][];
        };
        /** @description `GET /topology/usage` */
        TopologyUsage: {
            /** @description 위트니스가 아닌 노드(이름순) */
            nodes: components["schemas"]["NodeUsageSummary"][];
        };
        /** @description TOTP 등록: 인증 앱에 넣을 비밀과 `otpauth://` URI(QR). 다시 볼 수 없다. */
        TotpEnrollment: {
            secret: string;
            uri: string;
        };
        /** @description 2단계 인증 정책(`SettingsSpec.two_factor`) */
        TwoFactorPolicy: {
            /** @description 이 영역의 사용자에게 요구한다. 로컬 사용자는 `local`이다. OIDC 영역은 받지 않는다(IdP가 MFA를 맡는다). */
            realms?: string[];
            /** @description 이 전역 역할의 사용자에게 요구한다. */
            roles?: components["schemas"]["Role"][];
            /** @description 패스키의 RP ID(도메인). 없으면 요청 호스트다. */
            rp_id?: string;
        };
        /** @description 자기 2단계 인증 상태(비밀은 없다) */
        TwoFactorView: {
            /** @description 모든 노드가 2단계 인증을 안다(아니면 등록을 받지 않는다) */
            available: boolean;
            /** @description 이 세션은 강제 등록 중이다(등록을 마치면 로그인이 끝난다) */
            enrolling: boolean;
            passkeys: components["schemas"]["PasskeyView"][];
            /** @description 남은 복구 코드 수 */
            recovery_codes: number;
            /** @description 정책이 이 사용자에게 2단계 인증을 요구한다 */
            required: boolean;
            /** @description 이 접속에서 패스키에 쓸 RP ID. 없으면(IP 주소로 접속) 브라우저가 패스키를 막는다. */
            rp_id?: string;
            /** @description 확인된 TOTP가 있다 */
            totp: boolean;
            /** @description 확인을 기다리는 TOTP가 있다 */
            totp_pending: boolean;
        };
        UpdateAccessRoleRequest: {
            description?: string;
            privileges: string[];
        };
        UpdateRequest_AlertRuleSpec: {
            /**
             * @description 바꾼 뒤의 레이블 전체. 없거나 `null`이면 레이블이 없다(GET은 빈 레이블을 빼므로 읽어 다시 쓰는 클라이언트가
             *     `null`을 보낼 수 있다).
             */
            labels?: {
                [key: string]: string;
            };
            /** @description 경보 규칙 하나 */
            spec: {
                condition: components["schemas"]["AlertCondition"];
                /**
                 * @description 끄면 평가하지 않고, 활성 경보는 알림 없이 해제한다.
                 * @default true
                 */
                enabled: boolean;
                /**
                 * Format: int32
                 * @description 조건이 이만큼(초) 참이어야 알린다(0~86400). 이벤트 조건은 0이다.
                 * @default 0
                 */
                for_secs: number;
                /** @description 이 노드(이름)들만. 비면 모든 노드 */
                nodes?: string[];
                /**
                 * Format: int32
                 * @description 경보가 이어지는 동안 이만큼(초)마다 다시 알린다. 0이면 다시 보내지 않는다.
                 * @default 14400
                 */
                repeat_secs: number;
                /**
                 * @description 해제도 알린다(이벤트 조건은 해제가 없다).
                 * @default true
                 */
                send_resolved: boolean;
                severity: components["schemas"]["Severity"];
                /** @description 알릴 `NotificationTarget` 이름(1~16개) */
                targets: string[];
                /** @description 이 VM(이름)들만(VM 조건만). 비면 모든 VM */
                vms?: string[];
            };
        };
        UpdateRequest_GroupSpec: {
            /**
             * @description 바꾼 뒤의 레이블 전체. 없거나 `null`이면 레이블이 없다(GET은 빈 레이블을 빼므로 읽어 다시 쓰는 클라이언트가
             *     `null`을 보낼 수 있다).
             */
            labels?: {
                [key: string]: string;
            };
            /** @description 로컬 그룹(`Kind::Group`). 외부 사용자는 그 영역의 `external_groups`에 `external`의 그룹이 있으면 든다. */
            spec: {
                description?: string;
                /** @description 연결한 디렉터리 그룹 */
                external?: components["schemas"]["ExternalGroup"][];
                /** @description 멤버 사용자 id(로컬·외부) */
                members?: string[];
            };
        };
        UpdateRequest_HaCheckSpec: {
            /**
             * @description 바꾼 뒤의 레이블 전체. 없거나 `null`이면 레이블이 없다(GET은 빈 레이블을 빼므로 읽어 다시 쓰는 클라이언트가
             *     `null`을 보낼 수 있다).
             */
            labels?: {
                [key: string]: string;
            };
            /** @description 검사 하나 */
            spec: {
                /**
                 * @description 끄면 노드가 검사하지 않는다.
                 * @default true
                 */
                enabled: boolean;
                /**
                 * Format: int32
                 * @description 이만큼(초) 넘게 실패해야 대응한다. 10~600
                 * @default 10
                 */
                grace_secs: number;
                /** @description 노드 이름 → 그 노드의 대상(종류는 `target`과 같아야 한다) */
                nodes?: {
                    [key: string]: components["schemas"]["HaCheckTarget"];
                };
                /**
                 * @description 실패했을 때의 대응. 없으면 역할의 기본(`storage` → `restart_elsewhere`, `vm` → `migrate`,
                 *     `management` → `warn`)
                 */
                on_fail?: components["schemas"]["HaCheckAction"];
                /** @description 검사하는 네트워크·장치의 쓰임. 기본 대응과 영향받는 VM을 정한다. */
                role: components["schemas"]["HaCheckRole"];
                /** @description 모든 노드의 검사 대상(노드별 덮기가 없을 때) */
                target: components["schemas"]["HaCheckTarget"];
            };
        };
        UpdateRequest_HaGroupSpec: {
            /**
             * @description 바꾼 뒤의 레이블 전체. 없거나 `null`이면 레이블이 없다(GET은 빈 레이블을 빼므로 읽어 다시 쓰는 클라이언트가
             *     `null`을 보낼 수 있다).
             */
            labels?: {
                [key: string]: string;
            };
            /** @description HA 그룹: HA VM을 복구할 노드와 그 우선순위. VM은 `ha.group`으로 그룹을 쓴다. */
            spec: {
                /** @description 그룹 노드. 우선순위가 큰 노드에 먼저 놓는다. */
                nodes: components["schemas"]["HaGroupNode"][];
                /** @description 켜면 우선순위가 더 높은 노드가 돌아와도 되돌아가지 않는다. */
                nofailback?: boolean;
                /** @description 켜면 그룹 밖 노드에는 놓지 않는다(놓을 곳이 없으면 켜지 않는다). */
                restricted?: boolean;
            };
        };
        UpdateRequest_NodeSpec: {
            /**
             * @description 바꾼 뒤의 레이블 전체. 없거나 `null`이면 레이블이 없다(GET은 빈 레이블을 빼므로 읽어 다시 쓰는 클라이언트가
             *     `null`을 보낼 수 있다).
             */
            labels?: {
                [key: string]: string;
            };
            spec: {
                /** @description API 주소 (`host:port`) */
                address: string;
                /** @description 예약·전용 CPU와 hugepages 예약(계획 64 §7). 없으면 공유 = 모든 CPU — 지금과 같고 쓰지 않아 옛 노드도 푼다. */
                host_resources?: components["schemas"]["HostResources"];
                /** @description 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다. */
                maintenance?: boolean;
                /**
                 * @description 이 노드의 역할. 없으면 기본 역할(위트니스가 아니면 `api`)이다. 옛 노드는 이 필드를 모르므로 기본이면
                 *     쓰지 않는다.
                 */
                roles?: components["schemas"]["NodeRole"][];
                /** @description SDN physnet 연결과 게이트웨이 노드(계획 66 §7). 없으면 쓰지 않아 옛 노드도 푼다. */
                sdn?: components["schemas"]["NodeSdnConfig"];
                /** @description 위트니스 노드: Raft 투표만 하고 VM을 돌리지 않는다. 합류할 때 정해지고 바뀌지 않는다. */
                witness?: boolean;
            };
        };
        UpdateRequest_PlacementRuleSpec: {
            /**
             * @description 바꾼 뒤의 레이블 전체. 없거나 `null`이면 레이블이 없다(GET은 빈 레이블을 빼므로 읽어 다시 쓰는 클라이언트가
             *     `null`을 보낼 수 있다).
             */
            labels?: {
                [key: string]: string;
            };
            /**
             * @description 배치 규칙: VM끼리(같은 노드·다른 노드) 또는 VM과 노드(이 노드들 중·이 노드들은 안 됨). hard 규칙은
             *     조건이고 soft 규칙은 벌점(`weight`)이다. 모든 VM에 적용한다.
             */
            spec: {
                /**
                 * @description 끄면 없는 것으로 본다.
                 * @default true
                 */
                enabled: boolean;
                /** @description hard 규칙만: HA 복구가 지킬 수 없으면 어기고 켠다. */
                ha_may_violate?: boolean;
                kind: components["schemas"]["RuleKind"];
                /**
                 * @description 이 레이블(`key=value`, 예: `zone=rack-a`)을 가진 노드들
                 * @example zone=rack-a
                 */
                node_tag?: string;
                /** @description VM과 노드 규칙의 노드 이름. `node_tag`와 둘 중 하나만. */
                nodes?: string[];
                /**
                 * @description 이 레이블(`key=value`)을 가진 VM들
                 * @example app=web
                 */
                vm_tag?: string;
                /** @description 규칙의 VM 이름(아직 없는 VM이어도 된다). `vm_tag`와 둘 중 하나만. */
                vms?: string[];
                /**
                 * Format: int32
                 * @description soft 규칙의 벌점 1~100. 없으면 50.
                 */
                weight?: number;
            };
        };
        UpdateRequest_VmSpec: {
            /**
             * @description 바꾼 뒤의 레이블 전체. 없거나 `null`이면 레이블이 없다(GET은 빈 레이블을 빼므로 읽어 다시 쓰는 클라이언트가
             *     `null`을 보낼 수 있다).
             */
            labels?: {
                [key: string]: string;
            };
            /** @description VM의 원하는 상태. */
            spec: {
                /** @description CD-ROM 드라이브(계획 57, 넷까지, 순번 = 드라이브 번호). 비면 쓰지 않아 옛 노드도 VM을 푼다. */
                cdroms?: components["schemas"]["CdromSpec"][];
                /** @description cloud-init NoCloud 씨앗(계획 57). 없으면 쓰지 않아 옛 노드도 VM을 푼다. */
                cloud_init?: components["schemas"]["CloudInit"];
                cpu: components["schemas"]["CpuSpec"];
                disks?: components["schemas"]["DiskSpec"][];
                firmware?: components["schemas"]["Firmware"];
                /**
                 * Format: uuid
                 * @description 든 VM 폴더(계획 62, `Folder` id). 없으면 VM 트리의 루트이고 쓰지 않아 옛 노드도 VM을 푼다. 템플릿도 같은
                 *     트리에 산다.
                 */
                folder?: string;
                /**
                 * @description 게스트 에이전트(계획 59): 채널을 달고 게스트 정보·에이전트 종료·fs-freeze에 쓴다. 없으면 쓰지 않아 옛 노드도
                 *     VM을 푼다(`ha.guest_watch`가 있으면 채널은 그대로 단다).
                 */
                guest_agent?: components["schemas"]["GuestAgentSpec"];
                /**
                 * @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다.
                 *     JSON은 옛 bool(`false` = 없음, `true` = 기본 정책)이나 정책 객체다.
                 */
                ha?: components["schemas"]["HaSetting"];
                /** @description 게스트 메모리를 hugepages로(계획 64, `2M`·`1G`·`any`). 없으면 보통 페이지이고 쓰지 않는다. */
                hugepages?: components["schemas"]["PageSize"];
                machine: components["schemas"]["MachineType"];
                /**
                 * Format: int32
                 * @description 메모리 핫플러그 상한(MiB, 계획 54). 없으면 `memory_mib`와 같다(핫플러그 꺼짐). 없으면 쓰지 않아 옛 노드도
                 *     VM을 푼다.
                 */
                max_memory_mib?: number;
                /** Format: int32 */
                memory_mib: number;
                nics?: components["schemas"]["NicSpec"][];
                /**
                 * Format: uuid
                 * @description 배치된 노드의 id. None이면 시작할 때 스케줄러가 고른다.
                 */
                node?: string | null;
                /**
                 * @description 게스트 vNUMA(계획 64). 없으면 전용·hugepages VM은 `auto`, 그 밖은 vNUMA 없음(지금 그대로)이고 쓰지 않아 옛
                 *     노드도 VM을 푼다.
                 */
                numa?: components["schemas"]["NumaSpec"];
                /** @description 최적 배치 옵션(계획 53). 없으면 쓰지 않아 옛 노드도 VM을 푼다. */
                placement?: components["schemas"]["VmPlacement"];
                power?: components["schemas"]["PowerState"];
                /**
                 * @description 로컬 디스크 복제 대상(계획 55, 셋까지). 노드 로컬 풀의 디스크를 이 노드들에 주기적으로 복제한다. 비면 쓰지
                 *     않아 옛 노드도 VM을 푼다.
                 */
                replication?: components["schemas"]["ReplicationTarget"][];
                restart?: components["schemas"]["RestartPolicy"];
                /**
                 * @description Secure Boot(계획 59): Microsoft 키를 등록한 UEFI 변수로 켜는 secure-boot 펌웨어(x86_64는 SMM). UEFI VM만 쓸 수
                 *     있다. 거짓이면 쓰지 않아 옛 노드도 VM을 푼다.
                 */
                secure_boot?: boolean;
                snapshot_schedule?: components["schemas"]["SnapshotSchedule"] | null;
                /**
                 * @description 템플릿(계획 58): 켤 수 없고 디스크를 바꿀 수 없는 원본. 배포(복제)와 연결 복제의 기반이 된다. 거짓이면 쓰지
                 *     않아 옛 노드도 VM을 푼다.
                 */
                template?: boolean;
                /**
                 * @description TPM 2.0(계획 59): VM마다 swtpm 하나를 띄워 단다(q35 `tpm-crb`, virt `tpm-tis-device`). UEFI VM만 쓸 수 있다.
                 *     상태는 노드 로컬 파일이고 `firmware_node`를 따라다닌다. 거짓이면 쓰지 않아 옛 노드도 VM을 푼다.
                 */
                tpm?: boolean;
            };
        };
        /** @description 노드 업데이트 설정 */
        UpdateSettings: {
            /** @description 노드가 패키지를 받는 APT 저장소 채널 */
            channel?: components["schemas"]["Channel"];
        };
        UpdateSettingsPatch: {
            channel?: components["schemas"]["Channel"];
        };
        UpdateUserRequest: {
            /** @description 자기 비밀번호를 바꿀 때 필요하다. */
            current_password?: string | null;
            password?: string | null;
            role?: components["schemas"]["Role"] | null;
        };
        Usage: {
            /**
             * Format: int64
             * @default 0
             */
            available_bytes: number;
            /**
             * Format: int64
             * @default 0
             */
            total_bytes: number;
            /**
             * Format: int64
             * @default 0
             */
            used_bytes: number;
        };
        UserView: {
            /** Format: uuid */
            id: string;
            name: string;
            /** @description 외부 사용자의 영역. 로컬 사용자는 없다. */
            realm?: string;
            /** Format: int64 */
            revision: number;
            role: components["schemas"]["Role"];
            /** @description 2단계 인증 요소(확인된 TOTP 또는 패스키)가 있다(계획 62) */
            two_factor?: boolean;
        };
        /** @enum {string} */
        VerifyMode: "full" | "metadata";
        /** @description 예약 검증(§10.5) */
        VerifyPolicy: {
            mode?: components["schemas"]["VerifyMode"];
            /**
             * Format: int32
             * @description 이 날 수보다 오래전에 검증한 백업을 다시 검증한다(1~3650).
             * @default 30
             */
            reverify_days: number;
            schedule: components["schemas"]["JobSchedule"];
            /** @description 백업이 `ready`가 되면 곧바로 그 백업을 검증한다. */
            verify_new?: boolean;
        };
        /** @description 끝난 검증 실행의 결과 */
        VerifyResult: {
            /**
             * Format: int64
             * @description 깨진 청크 수
             */
            bad: number;
            /**
             * Format: int64
             * @description 확인한 청크 수
             */
            chunks: number;
            error?: string;
            /** Format: int64 */
            finished_at: number;
            /**
             * Format: int64
             * @description 검증한 백업 수
             */
            snapshots: number;
            /** Format: int64 */
            started_at: number;
        };
        /** @enum {string} */
        VerifyState: "ok" | "failed";
        VersionInfo: {
            /** @description 이 노드의 orbivisord 빌드 버전 */
            version: string;
        };
        /**
         * @description VM 지표 규칙의 값
         * @enum {string}
         */
        VmAlertMetric: "cpu" | "mem_rss" | "disk_read" | "disk_write" | "disk_read_iops" | "disk_write_iops" | "net_rx" | "net_tx";
        VmCounts: {
            /**
             * Format: int64
             * @description HA를 켠 VM
             */
            ha: number;
            /**
             * Format: int64
             * @description 지금 도는 VM(phase running)
             */
            running: number;
            /** Format: int64 */
            total: number;
        };
        /** @description VM의 CPU: 스펙, 지금 켜면 쓸 것, 도는 것, 보류, 노드마다 켤 수 있는지 */
        VmCpuInfo: {
            /**
             * @description 이 아키텍처의 노드마다(이름순): 도는 VM은 옮길 수 있는지(기록한 CPU), 꺼진 VM은 켤 수 있는지(스펙). 루트
             *     `cluster.read`가 없으면 비어 있다(노드 이름을 보이지 않는다).
             */
            nodes: components["schemas"]["CpuNodeFit"][];
            /** @description 다음 전원 껐다 켜기 때 바뀔 것(`status.hardware.pending`의 `cpu`) */
            pending?: components["schemas"]["PendingChange"];
            /** @description 지금 VM의 노드에서 켜면 쓸 CPU(노드가 없거나 해석할 수 없으면 없음) */
            resolved?: components["schemas"]["ResolvedCpu"];
            /** @description 켤 때 기록한 CPU(도는 VM). 계획 63 전에 켠 VM은 모델만 편 것이다. */
            running?: components["schemas"]["ResolvedCpu"];
            spec: components["schemas"]["CpuSpec"];
        };
        /** @description VM의 HA 정책. */
        VmHa: {
            /** @description HA 그룹 이름. 없으면 그룹 없이 아무 노드로 복구한다. */
            group?: string;
            /** @description 게스트 에이전트 감시. 없으면 쓰지 않는다. */
            guest_watch?: components["schemas"]["GuestWatch"];
            /**
             * Format: int32
             * @description 다른 노드로 옮기는 횟수. 넘으면 `error` 상태로 둔다.
             * @default 1
             */
            max_relocate: number;
            /**
             * Format: int32
             * @description 같은 노드에서 다시 켜는 횟수. 넘으면 다른 노드로 옮긴다.
             * @default 1
             */
            max_restart: number;
            /**
             * Format: int32
             * @description 복구 순서(0~100, 클수록 먼저). 기본(50)이면 쓰지 않아 계획 50 노드도 정책을 푼다.
             * @default 50
             */
            priority: number;
            /** @default started */
            state: components["schemas"]["HaState"];
        };
        /** @description HA 재시작·옮기기 기록 */
        VmHaStatus: {
            /** @description HA가 이 VM을 놓거나 옮기지 못한 이유. */
            error?: string;
            /**
             * @description HA 검사(`restart_elsewhere`)가 실패해 리더가 노드 `from`에 이 VM을 끄라고 한 요청. 그 노드가 QEMU를 강제로
             *     끈 뒤 지우고 `relocate_from`을 쓴다. 옛 노드는 이 필드를 무시한다.
             */
            evict?: components["schemas"]["HaEvict"];
            /**
             * Format: uuid
             * @description 재시작 횟수를 넘겨 이 노드에서 다른 노드로 옮겨 달라는 요청. reconciler가 쓴다.
             */
            relocate_from?: string;
            /**
             * Format: uuid
             * @description 마지막으로 옮겨 떠난 노드. 그 노드로 되돌아가면 옮기기 한 번으로 센다.
             */
            relocated_from?: string;
            /**
             * @description 창 안에서 다른 노드로 옮긴 때(유닉스 초).
             * @default []
             */
            relocations: number[];
        };
        /** @description 리소스만 지운 가져오기 지우기(`force`)의 응답 */
        VmImportForgotten: {
            message: string;
        };
        VmImportSpec: {
            /**
             * Format: uuid
             * @description 원본 파일이 있는 노드(올린 노드)
             */
            node: string;
            source: components["schemas"]["ImportSource"];
        };
        VmImportStatus: {
            /** @description 마지막 실행이 실패한 이유(phase는 `ready`로 돌아간다), 또는 `failed`인 이유 */
            error?: string;
            /** @default ready */
            phase: components["schemas"]["ImportPhase"];
            /** @description [`propose`]가 만든 기본값 */
            proposal?: components["schemas"]["ImportProposal"];
            /** @description 원본 VM에서 읽은 것(표시용) */
            source_vm?: components["schemas"]["SourceVm"];
            /**
             * Format: uuid
             * @description 만든 VM id(`done`일 때)
             */
            vm?: string;
            /**
             * @description 해석과 제안의 경고
             * @default []
             */
            warnings: string[];
        };
        /** @description `GET /vms/{key}/metrics`의 응답 */
        VmMetricsResponse: {
            /** @description 모든 노드에서 잰 점을 시각 순으로 합친 것(마이그레이션 앞뒤가 이어진다) */
            samples: components["schemas"]["VmSample"][];
            /** @description 답하지 않은 노드의 이름(정렬) */
            unreachable: string[];
            /** @description 지표를 모르는 옛 노드의 이름(정렬) */
            unsupported: string[];
            /** Format: uuid */
            vm: string;
        };
        /** @description VM NIC 하나의 옮기기 */
        VmMove: {
            address_source: components["schemas"]["AddressSource"];
            /**
             * Format: int64
             * @description 마지막으로 상태를 바꾼 시각(유닉스 초)
             */
            at?: number;
            error?: string;
            /** @description 되돌리기가 되살리는 원본 NIC */
            from: components["schemas"]["NicOrigin"];
            /** @description 옮겨 갈 주소(`ipam`이면 Task가 고른 주소) */
            ips?: string[];
            mac: string;
            /** @description 정한 방식 */
            mode: components["schemas"]["PlannedMode"];
            /** @description NIC 순번 */
            nic: number;
            state?: components["schemas"]["MoveState"];
            /** Format: uuid */
            vm: string;
        };
        /** @description VM의 NUMA: 스펙, 지금 모양, 기록, 보류, 노드마다 맞는지 */
        VmNumaInfo: {
            /**
             * @description 이 아키텍처의 노드마다(이름순): 도는 VM(하드웨어 기록이 있음)은 기록 모양으로 옮길 수 있는지, 꺼진 VM은 스펙으로
             *     켤 수 있는지. 루트 `cluster.read`가 없으면 비어 있다.
             */
            nodes: components["schemas"]["NumaNodeFit"][];
            /** @description 다음 전원 껐다 켜기 때 바뀔 것(`status.hardware.pending`의 `numa`) */
            pending?: components["schemas"]["PendingChange"];
            /** @description 켤 때 잡은 호스트 노드·pCPU. 루트 `cluster.read`가 없으면 없다(노드·pCPU 번호를 보이지 않는다). */
            placement?: components["schemas"]["HostPlacement"];
            /** @description 켤 때 기록한 게스트 모양(도는 VM, 계획 64 칸을 쓰지 않으면 없음) */
            running?: components["schemas"]["ResolvedNuma"];
            spec: components["schemas"]["NumaSettings"];
            /** @description 스펙의 짧은 이름(`2 nodes, dedicated, hugepages 1G`, 쓰지 않으면 `none`) */
            wanted: string;
        };
        /** @enum {string} */
        VmPhase: "stopped" | "starting" | "running" | "stopping" | "failed" | "migrating";
        /** @description VM의 최적 배치 옵션 */
        VmPlacement: {
            /** @description 균형 조정기가 옮기지 않는다(HA 복구·비우기·사용자 마이그레이션은 그대로 옮긴다). */
            pinned?: boolean;
        };
        /** @description 이 노드에서 도는 VM 하나가 잡은 것(기록) */
        VmPlacementUse: {
            /** @description 에뮬레이터 스레드의 전용 pCPU(`isolate`, 아니면 빔) */
            emulator: components["schemas"]["CpuSet"];
            /** @description 게스트 노드마다 호스트 NUMA 노드(묶지 않으면 빔) */
            host_nodes: number[];
            page?: components["schemas"]["PageKind"];
            /**
             * Format: int64
             * @description 잡은 페이지 수(hugepages VM)
             */
            pages: number;
            /** @description `isolate`가 비워 둔 형제 */
            siblings: components["schemas"]["CpuSet"];
            /** @description 자리마다 pCPU(공유 VM은 빔) */
            vcpus: number[];
            vm: string;
        };
        VmReplicateRequest: {
            /** @description 복제할 대상 노드(이름이나 id). VM의 복제 대상이 하나면 생략할 수 있다. */
            node?: string | null;
        };
        /** @description VM 점 하나 */
        VmSample: {
            /**
             * Format: double
             * @description vCPU 대비 사용률 %(vCPU가 모두 바쁘면 100). QEMU 프로세스 전체라 100을 조금 넘을 수 있다.
             */
            cpu?: number | null;
            /**
             * Format: double
             * @description 모든 디스크를 합한 값
             */
            disk_read_bps?: number | null;
            /** Format: double */
            disk_read_iops?: number | null;
            /** Format: double */
            disk_write_bps?: number | null;
            /** Format: double */
            disk_write_iops?: number | null;
            /**
             * Format: int64
             * @description QEMU 프로세스의 RSS
             */
            mem_rss_mib?: number | null;
            /**
             * Format: double
             * @description 게스트 기준 받기 바이트/초(tap이 없으면 `null`)
             */
            net_rx_bps?: number | null;
            /** Format: double */
            net_tx_bps?: number | null;
            /**
             * Format: int64
             * @description 측정 시각(그 노드 벽시계의 유닉스 밀리초)
             */
            t: number;
        };
        VmSelector: {
            all: boolean;
        } | {
            names: string[];
        } | {
            labels: {
                [key: string]: string;
            };
        };
        /** @description VM의 원하는 상태. */
        VmSpec: {
            /** @description CD-ROM 드라이브(계획 57, 넷까지, 순번 = 드라이브 번호). 비면 쓰지 않아 옛 노드도 VM을 푼다. */
            cdroms?: components["schemas"]["CdromSpec"][];
            /** @description cloud-init NoCloud 씨앗(계획 57). 없으면 쓰지 않아 옛 노드도 VM을 푼다. */
            cloud_init?: components["schemas"]["CloudInit"];
            cpu: components["schemas"]["CpuSpec"];
            disks?: components["schemas"]["DiskSpec"][];
            firmware?: components["schemas"]["Firmware"];
            /**
             * Format: uuid
             * @description 든 VM 폴더(계획 62, `Folder` id). 없으면 VM 트리의 루트이고 쓰지 않아 옛 노드도 VM을 푼다. 템플릿도 같은
             *     트리에 산다.
             */
            folder?: string;
            /**
             * @description 게스트 에이전트(계획 59): 채널을 달고 게스트 정보·에이전트 종료·fs-freeze에 쓴다. 없으면 쓰지 않아 옛 노드도
             *     VM을 푼다(`ha.guest_watch`가 있으면 채널은 그대로 단다).
             */
            guest_agent?: components["schemas"]["GuestAgentSpec"];
            /**
             * @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다.
             *     JSON은 옛 bool(`false` = 없음, `true` = 기본 정책)이나 정책 객체다.
             */
            ha?: components["schemas"]["HaSetting"];
            /** @description 게스트 메모리를 hugepages로(계획 64, `2M`·`1G`·`any`). 없으면 보통 페이지이고 쓰지 않는다. */
            hugepages?: components["schemas"]["PageSize"];
            machine: components["schemas"]["MachineType"];
            /**
             * Format: int32
             * @description 메모리 핫플러그 상한(MiB, 계획 54). 없으면 `memory_mib`와 같다(핫플러그 꺼짐). 없으면 쓰지 않아 옛 노드도
             *     VM을 푼다.
             */
            max_memory_mib?: number;
            /** Format: int32 */
            memory_mib: number;
            nics?: components["schemas"]["NicSpec"][];
            /**
             * Format: uuid
             * @description 배치된 노드의 id. None이면 시작할 때 스케줄러가 고른다.
             */
            node?: string | null;
            /**
             * @description 게스트 vNUMA(계획 64). 없으면 전용·hugepages VM은 `auto`, 그 밖은 vNUMA 없음(지금 그대로)이고 쓰지 않아 옛
             *     노드도 VM을 푼다.
             */
            numa?: components["schemas"]["NumaSpec"];
            /** @description 최적 배치 옵션(계획 53). 없으면 쓰지 않아 옛 노드도 VM을 푼다. */
            placement?: components["schemas"]["VmPlacement"];
            power?: components["schemas"]["PowerState"];
            /**
             * @description 로컬 디스크 복제 대상(계획 55, 셋까지). 노드 로컬 풀의 디스크를 이 노드들에 주기적으로 복제한다. 비면 쓰지
             *     않아 옛 노드도 VM을 푼다.
             */
            replication?: components["schemas"]["ReplicationTarget"][];
            restart?: components["schemas"]["RestartPolicy"];
            /**
             * @description Secure Boot(계획 59): Microsoft 키를 등록한 UEFI 변수로 켜는 secure-boot 펌웨어(x86_64는 SMM). UEFI VM만 쓸 수
             *     있다. 거짓이면 쓰지 않아 옛 노드도 VM을 푼다.
             */
            secure_boot?: boolean;
            snapshot_schedule?: components["schemas"]["SnapshotSchedule"] | null;
            /**
             * @description 템플릿(계획 58): 켤 수 없고 디스크를 바꿀 수 없는 원본. 배포(복제)와 연결 복제의 기반이 된다. 거짓이면 쓰지
             *     않아 옛 노드도 VM을 푼다.
             */
            template?: boolean;
            /**
             * @description TPM 2.0(계획 59): VM마다 swtpm 하나를 띄워 단다(q35 `tpm-crb`, virt `tpm-tis-device`). UEFI VM만 쓸 수 있다.
             *     상태는 노드 로컬 파일이고 `firmware_node`를 따라다닌다. 거짓이면 쓰지 않아 옛 노드도 VM을 푼다.
             */
            tpm?: boolean;
        };
        /** @description VM의 실제 상태. reconciler만 기록한다. */
        VmStatus: {
            /**
             * @description `tpm_sha256`과 같은 펌웨어 사본(UEFI 변수 + TPM)을 가진 다른 노드(계획 59). 해시가 바뀌면 비운다. 비면 쓰지
             *     않는다.
             */
            firmware_copies?: string[];
            /**
             * Format: int32
             * @description 펌웨어 초기화 회차(계획 59). `vm-firmware-reset`이 1씩 올린다. 노드의 파일이 다른 회차의 것이면 낡은 것이다.
             *     0이면 쓰지 않는다.
             */
            firmware_epoch?: number;
            /**
             * Format: uuid
             * @description 지금 UEFI 변수 파일을 가진 노드. UEFI VM을 켠 노드나 마이그레이션의 대상 노드가 쓴다.
             *     다른 노드에서 켤 때 이 노드에서 변수를 받아 온다. BIOS VM은 늘 없다.
             */
            firmware_node?: string;
            /** @description HA 재시작·옮기기 기록. 기록이 없으면 빼고 쓴다. */
            ha?: components["schemas"]["VmHaStatus"];
            /**
             * @description 도는 QEMU의 하드웨어(계획 54). VM을 켠 노드가 `running`을 쓸 때 함께 쓰고 핫플러그 Task가 고치며, QEMU가
             *     없어지면 지운다. 옛 노드는 이 필드를 무시한다.
             */
            hardware?: components["schemas"]["RunningHardware"];
            /** @default null */
            last_error: string | null;
            /**
             * @description HA가 이 VM을 복제본으로 다시 켠 마지막 복구(계획 55). 리더의 HA 복구가 쓰고 다음 복구가 덮는다. 옛 노드는 이
             *     필드를 무시한다.
             */
            last_recovery?: components["schemas"]["ReplicaRecovery"];
            /**
             * @description 진행 중인 라이브 복원(계획 68 §11). 라이브 복원 Task가 쓰고 갈아타기가 끝나면 지운다. 있는 동안 reconciler·HA가
             *     이 VM을 켜거나 옮기지 않는다. 없으면 쓰지 않는다.
             */
            live_restore?: components["schemas"]["LiveRestore"];
            /** @description 진행 중인 라이브 마이그레이션. 원본 노드의 마이그레이션 Task가 쓰고 지운다. */
            migration?: components["schemas"]["MigrationStatus"];
            /**
             * Format: uuid
             * @default null
             */
            node: string | null;
            /** @default stopped */
            phase: components["schemas"]["VmPhase"];
            /**
             * Format: int32
             * @default null
             */
            pid: number | null;
            /**
             * @description 로컬 디스크 복제 대상마다의 상태(계획 55). 원본 노드의 복제 Task, 리더의 HA 복구·스케줄러, 마이그레이션
             *     확정이 쓴다. 옛 노드는 이 필드를 무시한다.
             */
            replication?: components["schemas"]["ReplicaStatus"][];
            /** @description 지금 배치가 어기는 배치 규칙의 이름. 리더의 HA 관리자가 쓴다. */
            rule_violations?: string[];
            /** @default null */
            schedule_error: components["schemas"]["ScheduleError"] | null;
            /**
             * Format: uuid
             * @description 현재 위치: 마지막으로 만들었거나 되돌린 스냅샷. 다음 스냅샷의 부모가 된다.
             * @default null
             */
            snapshot: string | null;
            /**
             * Format: uuid
             * @description 지금 TPM 상태를 가진 노드(리뷰 2). TPM VM을 켠 노드·마이그레이션 대상·TPM을 되돌리거나 복원한 노드가 쓰고, TPM을
             *     초기화하면 지운다. UEFI 변수를 가진 노드(`firmware_node`)와 다를 수 있다(TPM을 끈 채 다른 노드에서 켬). 이 노드의
             *     파일만 기록된 해시(`tpm_sha256`) 뒤에 바뀌었을 수 있어 해시가 달라도 지금 것으로 본다. 없으면 쓰지 않는다.
             */
            tpm_node?: string;
            /**
             * @description 지금 TPM 영구 상태(`tpm2-00.permall`)의 SHA-256(16진 소문자, 계획 59). 비밀이 아니다. VM을 돌리는 노드가
             *     멈출 때와 실행 중 주기적으로 쓴다. 없으면(아직 TPM 상태가 없음, 초기화 뒤) 쓰지 않는다.
             */
            tpm_sha256?: string;
        };
        /**
         * @description 볼륨의 내용 종류
         * @enum {string}
         */
        VolumeContent: "disk" | "iso";
        /** @description `POST /volumes/download`: URL에서 이미지를 받아 볼륨을 만든다(계획 57). */
        VolumeDownloadRequest: {
            /**
             * @description 체크섬 파일(`SHA256SUMS`·`SHA512SUMS`·BSD `CHECKSUM` 모양)의 http·https URL(계획 58). 받기 전에 이 파일에서
             *     이미지 URL의 파일 이름 줄을 찾아 그 해시로 확인한다. `sha256`·`sha512`와 함께 쓰지 않는다.
             */
            checksum_url?: string;
            /** @description 내용 종류(기본 disk). iso는 dir·nfs 풀에 raw로 둔다. */
            content?: components["schemas"]["VolumeContent"];
            /** @description 만들 볼륨의 형식 (생략하면 풀 종류로: 블록 풀은 raw, 그 밖은 qcow2. ISO는 늘 raw) */
            format?: components["schemas"]["ImageFormat"];
            name: string;
            pool: string;
            /** @description 받은 파일의 SHA-256(16진 64자). 있으면 다르면 실패한다. */
            sha256?: string;
            /** @description 받은 파일의 SHA-512(16진 128자, 계획 58). `sha256`·`checksum_url`과 함께 쓰지 않는다. */
            sha512?: string;
            /** @description 받은 파일의 형식(디스크만, 기본 raw) */
            source_format?: components["schemas"]["SourceFormat"];
            /** @description 받을 http·https URL. 작업 노드가 닿을 수 있어야 한다. */
            url: string;
        };
        /**
         * @description 볼륨 파일이 준비됐는지. VM은 준비된 볼륨만 붙일 수 있다.
         * @enum {string}
         */
        VolumePhase: "pending" | "ready";
        VolumeSpec: {
            /**
             * Format: uuid
             * @description 연결 복제(계획 58)면 기반(템플릿 디스크) 볼륨의 id. 기반은 늘 같은 풀에 있다. 없으면 쓰지 않아 옛 노드도
             *     볼륨을 푼다.
             */
            base?: string;
            /** @description 내용 종류(계획 57). 디스크(기본)면 쓰지 않아 옛 노드도 볼륨을 푼다. */
            content?: components["schemas"]["VolumeContent"];
            format: components["schemas"]["ImageFormat"];
            /** @description StoragePool 리소스 이름 */
            pool: string;
            /** Format: int64 */
            size_bytes: number;
        };
        VolumeStatus: {
            /**
             * Format: int64
             * @description 실제로 디스크에 할당된 크기
             * @default null
             */
            allocated_bytes: number | null;
            /**
             * Format: uuid
             * @default null
             */
            attached_to: string | null;
            /**
             * Format: int64
             * @description 늘리기 Task가 저장소를 늘린 뒤 읽은 실제 크기(계획 62 리뷰 1). 스펙 크기를 이 값까지 올리는 기록은 폴더
             *     할당량에 막히지 않는다(이미 커진 디스크를 기록에서 숨기지 않게). Task가 기록한 뒤 지운다. 없으면 쓰지 않는다.
             */
            disk_bytes?: number;
            /**
             * Format: uuid
             * @description 볼륨을 만든 Task를 실행하는 노드. 그 노드만 시작할 때 남은 고아 볼륨을 정리한다.
             * @default null
             */
            node: string | null;
            /** @default pending */
            phase: components["schemas"]["VolumePhase"];
        };
        Vxlan: {
            /**
             * Format: int32
             * @description VXLAN 네트워크 식별자(1~16777215). 클러스터 안에서 겹치지 않는다.
             */
            vni: number;
        };
        WebauthnUser: {
            displayName: string;
            /** @description 사용자 id 바이트(base64url) */
            id: string;
            name: string;
        };
        /**
         * @description webhook 본문 형식
         * @enum {string}
         */
        WebhookFormat: "json" | "slack" | "teams" | "ntfy" | "gotify";
        /** @description HTTP(S) POST */
        WebhookTarget: {
            /** @description 있으면 이 CA(PEM)만 믿는다. */
            ca_pem?: string;
            format?: components["schemas"]["WebhookFormat"];
            /** @description 더 붙일 헤더. 값은 비밀로 본다(API는 이름만 보인다). */
            headers?: {
                [key: string]: string;
            };
            /** @description ntfy·Gotify 토큰. API는 내보내지 않는다. */
            token?: string;
            /** @description `http(s)://…`. 경로·질의는 비밀로 본다(Slack·Teams URL). API는 [`redact_url`]로 줄여 보인다. */
            url: string;
        };
        /** @description [`WebhookTarget`]에서 비밀을 뺀 것 */
        WebhookTargetView: {
            ca_pem?: string;
            format: components["schemas"]["WebhookFormat"];
            has_token: boolean;
            /** @description 헤더 이름(값은 비밀) */
            header_names: string[];
            /** @description [`redact_url`]로 줄인 URL(`scheme://host[:port]/…`). 바꾸기 본문에 이 값을 그대로 주면 저장된 URL을 지킨다. */
            url: string;
        };
        /** @enum {string} */
        Weekday: "mon" | "tue" | "wed" | "thu" | "fri" | "sat" | "sun";
        /**
         * @description `GET /auth/whoami`의 응답: 주체에 요청을 받은 노드를 더한다(계획 62 리뷰 2 — 노드 목록을 읽지 못하는 폴더 사용자의
         *     화면이 "이 노드"를 안다). 옛 응답과 같은 칸에 `node`만 더해진다(노드 기록을 읽지 못하면 빠진다).
         */
        WhoAmI: {
            name: string;
            node?: components["schemas"]["ServingNode"];
            role: components["schemas"]["Role"];
            /** Format: uuid */
            user: string;
        };
        /** @description 대상에 권한이 닿는 것: grant와 전역 역할 사용자 */
        WhoPermissions: {
            /** @description 이 대상에 직접 건 것과 조상에 propagate로 건 것 */
            grants: components["schemas"]["Resource"][];
            object: string;
            /** @description 전역 역할(`none`이 아닌)이 있는 사용자 — 루트에 건 암묵 grant다. 루트 `cluster.admin`이 아니면 비어 있다. */
            users: components["schemas"]["RoleHolder"][];
        };
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
    list_access_roles: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_AccessRoleView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_access_role: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateAccessRoleRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AccessRoleView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_access_role: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name (admin, operator and viewer are the built-in roles) */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AccessRoleView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    update_access_role: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateAccessRoleRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AccessRoleView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_access_role: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_alert_rules: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_AlertRuleSpec_AlertRuleStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_alert_rule: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRequest_AlertRuleSpec"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_AlertRuleSpec_AlertRuleStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_sdn_default_rules: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SdnDefaultsRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SdnDefaultsResponse"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_alert_rule: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_AlertRuleSpec_AlertRuleStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    update_alert_rule: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateRequest_AlertRuleSpec"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_AlertRuleSpec_AlertRuleStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_alert_rule: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_alerts: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AlertsResponse"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_alert_history: {
        parameters: {
            query?: {
                /** @description 이 시각부터(포함): 유닉스 밀리초나 RFC 3339. 기본은 `until`의 24시간 전 */
                since?: string;
                /** @description 이 시각까지(포함): 유닉스 밀리초나 RFC 3339. 기본은 지금 */
                until?: string;
                /** @description 최대 개수(1~5000, 기본 200) */
                limit?: number;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NotificationHistoryResponse"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_audit: {
        parameters: {
            query?: {
                /** @description 이 시각부터(포함): 유닉스 밀리초나 RFC 3339. 기본은 `until`의 24시간 전 */
                since?: string;
                /** @description 이 시각까지(포함): 유닉스 밀리초나 RFC 3339. 기본은 지금 */
                until?: string;
                /** @description 사용자 이름(정확히 같은 것) */
                user?: string;
                /** @description 노드 이름(정확히 같은 것) */
                node?: string;
                /** @description 동작(`POST /vms/{key}`)이나 경로에 들어 있는 문자열 */
                action?: string;
                /** @description 최대 개수(1~5000, 기본 200) */
                limit?: number;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AuditResponse"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_two_factor: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TwoFactorView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    regenerate_recovery: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ProofRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecoveryCodes"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    enroll_totp: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TotpEnrollment"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    disable_totp: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ProofRequest"];
            };
        };
        responses: {
            /** @description Disabled */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    confirm_totp: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CodeRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecoveryCodes"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    start_passkey: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RegistrationStart"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    finish_passkey: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["FinishRegistrationRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PasskeyAdded"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_passkey: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description passkey credential id (base64url) */
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ProofRequest"];
            };
        };
        responses: {
            /** @description Removed */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    login: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["LoginRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["LoginResponse"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    second_factor: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SecondFactorRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["UserView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    logout: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Logged out; the session cookie is cleared */
            204: {
                headers: {
                    "set-cookie"?: string;
                    [name: string]: unknown;
                };
                content?: never;
            };
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    oidc_callback: {
        parameters: {
            query?: {
                /** @description 인가 코드 */
                code?: string;
                /** @description 로그인 상태 쿠키의 state와 같아야 한다. */
                state?: string;
                /** @description IdP가 로그인을 거절한 이유(RFC 6749 §4.1.2.1) */
                error?: string;
                error_description?: string;
                /** @description 인가 응답을 낸 IdP(RFC 9207). 있으면 영역의 issuer와 같아야 하고, IdP가 지원한다고 하면 있어야 한다. */
                iss?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description To / with the session cookie, or to /login?error=<code> */
            302: {
                headers: {
                    location?: string;
                    "set-cookie"?: string;
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    oidc_start: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description OIDC realm name */
                realm: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description To the identity provider with the login state cookie, or to /login?error=<code> */
            302: {
                headers: {
                    location?: string;
                    "set-cookie"?: string;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description The request has no valid Host header */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_login_realms: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RealmSummary"][];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    whoami: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["WhoAmI"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_backup_jobs: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_BackupJobSpec_BackupJobStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_backup_job: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRequest_BackupJobSpec"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_BackupJobSpec_BackupJobStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_backup_job: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_BackupJobSpec_BackupJobStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_backup_job: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    patch_backup_job: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["BackupJobPatch"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_BackupJobSpec_BackupJobStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    run_backup_job: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_BackupJobSpec_BackupJobStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_stores: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_BackupStoreSpecView_BackupStoreStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_store: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateStoreRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CreatedStore"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_store: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_BackupStoreSpecView_BackupStoreStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_store: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_BackupStoreSpecView_BackupStoreStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    patch_store: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["StorePatch"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_BackupStoreSpecView_BackupStoreStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    check_store: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["StoreCheck"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    gc_store: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    scan_store: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    take_over: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["TakeOverRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_BackupStoreSpecView_BackupStoreStatus"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    verify_store: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["StoreVerifyRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    escrow_key_handler: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["EscrowRequest"];
            };
        };
        responses: {
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    export_key: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecoveryKey"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_backups: {
        parameters: {
            query?: {
                /** @description 이 VM(id나 이름)의 백업만. 이름이면 지금 그 이름의 VM 백업과 그 이름으로 남은(지운 VM의) 백업이다. */
                vm?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_BackupSpec_BackupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    receive_backup: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ReceiveRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ReceivingBackup"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_backup: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_BackupSpec_BackupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_backup: {
        parameters: {
            query?: {
                /** @description 파일을 지울 노드가 없어도 리소스만 지운다. 파일은 백업 풀에 남는다. */
                force?: boolean;
            };
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description force: only the resource was deleted */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BackupForgotten"];
                };
            };
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    complete_backup: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_BackupSpec_BackupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    convert_backup: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["StoreTargetRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    copy_backup: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["StoreTargetRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    replicate_backup: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ReplicateRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    verify_backup: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    missing_chunks: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ChunkIds"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MissingChunks"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    put_chunk: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
                /** @description chunk id (64 hex digits) */
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/octet-stream": components["schemas"]["BackupFile"];
            };
        };
        responses: {
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    put_backup_file: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
                /** @description disk file of the manifest, vars.fd or tpm2-00.permall */
                file: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/octet-stream": components["schemas"]["BackupFile"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ReceivedFile"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    restore_backup: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RestoreRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_ceph: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CephView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    init_ceph: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CephInitRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    install_client: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CephNodeRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    add_mgr: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CephNodeRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    add_mon: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CephNodeRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    add_osd: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CephOsdRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_ceph_pool: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CephPoolRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_ceph_status: {
        parameters: {
            query?: {
                /** @description 잠깐 둔 상태를 쓰지 않고 새로 읽는다. */
                refresh?: boolean;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CephStatusView"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_cluster: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ClusterView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_api_endpoints: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiEndpoints"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_ca: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description CA certificate */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/x-pem-file": string;
                };
            };
            "5XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    join: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["JoinRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["JoinResponse"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_join_token: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["JoinTokenRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CreatedJoinToken"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_cpu_baseline: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CpuBaselines"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_cpu_flags: {
        parameters: {
            query?: {
                /** @description 아키텍처. 없으면 노드들의 아키텍처(섞였으면 400)다. */
                arch?: components["schemas"]["Arch"];
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CpuFlags"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_cpu_models: {
        parameters: {
            query?: {
                /** @description 아키텍처(`x86_64`·`aarch64`). 없으면 노드들의 아키텍처(섞였으면 400)다. */
                arch?: components["schemas"]["Arch"];
                /** @description 이 노드(id나 이름)에서 켤 수 있는지만 본다. */
                node?: string;
                /** @description 켤 수 있는 노드가 있는 모델만 */
                usable?: boolean;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CpuModels"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_floating_ips: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FloatingIpList"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_floating_ip: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateFloatingIp"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_FloatingIpSpec_FloatingIpStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_floating_ip: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FloatingIpView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_floating_ip: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    patch_floating_ip: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["FloatingIpPatch"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_FloatingIpSpec_FloatingIpStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_folders: {
        parameters: {
            query?: {
                /** @description 이 트리의 폴더만(`vm`, `network`, `storage`) */
                kind?: "vm" | "network" | "storage";
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_FolderSpec_FolderStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_folder: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateFolderRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_FolderSpec_FolderStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_folder: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description folder id */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_FolderSpec_FolderStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_folder: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description folder id */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    patch_folder: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description folder id */
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["FolderPatch"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_FolderSpec_FolderStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    move_members: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description folder id, or root */
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["MoveMembersRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MoveMembersResponse"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    folder_usage: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description folder id */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FolderUsageView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_grants: {
        parameters: {
            query?: {
                /** @description 대상: `root`, `folder/<id>`, `vm/<id>`, `network/<id>`, `storage-pool/<id>`, `router/<id>`, `floating-ip/<id>` */
                target?: string;
                /** @description 주체: `user/<id>`, `group/<id>` */
                principal?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_GrantSpec_GrantStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_grant: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateGrantRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_GrantSpec_GrantStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_grant: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description grant id */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_GrantSpec_GrantStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_grant: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description grant id */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_groups: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_GroupSpec_GroupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_group: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRequest_GroupSpec"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_GroupSpec_GroupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_group: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_GroupSpec_GroupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    update_group: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateRequest_GroupSpec"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_GroupSpec_GroupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_group: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_ha_checks: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_HaCheckSpec_HaCheckStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_ha_check: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRequest_HaCheckSpec"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_HaCheckSpec_HaCheckStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_ha_check: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_HaCheckSpec_HaCheckStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    update_ha_check: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateRequest_HaCheckSpec"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_HaCheckSpec_HaCheckStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_ha_check: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_ha_groups: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_HaGroupSpec_HaGroupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_ha_group: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRequest_HaGroupSpec"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_HaGroupSpec_HaGroupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_ha_group: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_HaGroupSpec_HaGroupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    update_ha_group: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateRequest_HaGroupSpec"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_HaGroupSpec_HaGroupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_ha_group: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_health: {
        parameters: {
            query?: {
                /** @description 있으면(값은 보지 않는다) 프로세스가 답하는지만 본다(언제나 200). */
                live?: string;
                /** @description 있으면(값은 보지 않는다) 준비됐는지 본다. 기본이다. */
                ready?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Health"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Health"];
                };
            };
        };
    };
    list_catalog: {
        parameters: {
            query?: {
                /** @description 이 아키텍처의 항목만 */
                arch?: "x86_64" | "aarch64";
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_CatalogEntry"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    add_catalog_image: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CatalogImage"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CatalogImage"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    remove_catalog_image: {
        parameters: {
            query?: {
                /** @description 이 아키텍처의 항목만 지운다. 없으면 그 id의 모든 항목이다. */
                arch?: "x86_64" | "aarch64";
            };
            header?: never;
            path: {
                /** @description catalog image id */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Removed */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    download_catalog_image: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description catalog image id */
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CatalogDownloadRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_load_balancers: {
        parameters: {
            query?: {
                /** @description 거짓이면 OVN SB를 읽지 않는다(건강 검사 멤버가 `unchecked`). 기본 참 */
                live?: boolean;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["LoadBalancerList"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_load_balancer: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateLoadBalancer"];
            };
        };
        responses: {
            201: {
                headers: {
                    /** @description security groups block clients or health checks, or no gateway for the external VIP (only then) */
                    "orbivisor-warning"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_LoadBalancerSpec_LoadBalancerStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_load_balancer: {
        parameters: {
            query?: {
                /** @description 거짓이면 OVN SB를 읽지 않는다(건강 검사 멤버가 `unchecked`). 기본 참 */
                live?: boolean;
            };
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["LoadBalancerView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_load_balancer: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    patch_load_balancer: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["LoadBalancerPatch"];
            };
        };
        responses: {
            200: {
                headers: {
                    /** @description security groups block clients or health checks, or no gateway for the external VIP (only then) */
                    "orbivisor-warning"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_LoadBalancerSpec_LoadBalancerStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_prometheus: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Prometheus text format 0.0.4 */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "text/plain; version=0.0.4": string;
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_cluster_history: {
        parameters: {
            query?: {
                /** @description 지금에서 끝나는 범위: `hour`, `day`, `week`, `month`(30일), `year`(365일). `from`·`until`과 함께 쓰지 않는다. */
                range?: "hour" | "day" | "week" | "month" | "year";
                /** @description 범위 시작(유닉스 밀리초) */
                from?: number;
                /** @description 범위 끝(유닉스 밀리초, 없으면 지금). `from`과 함께만 쓴다. 범위는 366일까지다. */
                until?: number;
                /** @description 돌려줄 점 수의 상한(1~1000). 기본은 300이고 클러스터 모으기는 120이다. */
                points?: number;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ClusterHistoryResponse"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_node_metrics: {
        parameters: {
            query?: {
                /** @description 이 `t`(그 노드 시계의 유닉스 밀리초)보다 뒤의 점만. 없으면 남은 것 모두(1시간) */
                since?: number;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NodeMetricsResponse"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_network_migrations: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NetworkMigrationList"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_network_migration: {
        parameters: {
            query?: {
                /** @description 참이면 리소스를 만들지 않고 계획만 돌려준다(200). */
                dry_run?: boolean;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateNetworkMigration"];
            };
        };
        responses: {
            /** @description The plan only (dry_run) */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MigrationPlan"];
                };
            };
            201: {
                headers: {
                    /** @description the plan has warnings (only then) */
                    "orbivisor-warning"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NetworkMigrationCreated"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_network_migration: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_NetworkMigrationSpec_NetworkMigrationStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_network_migration: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    finish_network_migration: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["MigrationFinishRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    pause_network_migration: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_NetworkMigrationSpec_NetworkMigrationStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    resume_network_migration: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["MigrationStartRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    rollback_network_migration: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["MigrationRollbackRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    start_network_migration: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["MigrationStartRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    network_topology: {
        parameters: {
            query?: {
                /** @description 이 네트워크 폴더(하위 포함)의 네트워크·라우터·부하 분산기·유동 IP만 */
                folder?: string;
                /** @description 참이면 NB 게이트웨이 주인과 SB 건강 검사를 읽는다(기본 거짓 — 화면은 watch로 다시 읽고 살아 있는 상태는 가끔). */
                live?: boolean;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NetworkTopology"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_networks: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_NetworkSpec_NetworkStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_network: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRequest_NetworkSpec"];
            };
        };
        responses: {
            201: {
                headers: {
                    /** @description no node has the provider network's physnet ready (only then) */
                    "orbivisor-warning"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_NetworkSpec_NetworkStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_network: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_NetworkSpec_NetworkStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_network: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    patch_network: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["NetworkPatch"];
            };
        };
        responses: {
            200: {
                headers: {
                    /** @description interfaces that could not get an address (only then) */
                    "orbivisor-warning"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_NetworkSpec_NetworkStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    network_ips: {
        parameters: {
            query?: {
                /**
                 * @description 거짓이면 OVN 포트 상태(`live`)를 읽지 않는다(목록의 사용률처럼 주소만 볼 때 — 그때 `live`는 모두 null이고
                 *     `live_error`도 없다). 기본 참
                 */
                live?: boolean;
            };
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NetworkIps"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    detach_network_router: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description network id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Detached */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_nodes: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_NodeSpec_NodeStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_node: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_NodeSpec_NodeStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    update_node: {
        parameters: {
            query?: {
                /** @description `host_resources`의 409 `host_cpus_in_use`·`hugepages_in_use`를 넘기고 쓴다(응답 헤더 `orbivisor-warning`). */
                force?: boolean;
            };
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateRequest_NodeSpec"];
            };
        };
        responses: {
            200: {
                headers: {
                    /** @description what `force` let through (only when there is something) */
                    "orbivisor-warning"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_NodeSpec_NodeStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_node: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Removed; something is left to do by hand */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NodeRemoved"];
                };
            };
            /** @description Removed */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_node_cpu: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NodeCpuInfo"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    refresh_node_cpu: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_node_disks: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NodeDisk"][];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    drain_node: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_node_fence: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NodeFenceView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    put_node_fence: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["NodeFenceSpec"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NodeFenceView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_node_fence: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    test_node_fence: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FenceTestResult"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_node_metrics: {
        parameters: {
            query?: {
                /** @description 이 `t`(그 노드 시계의 유닉스 밀리초)보다 뒤의 점만. 없으면 남은 것 모두(1시간) */
                since?: number;
            };
            header?: never;
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NodeSeries"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_node_history: {
        parameters: {
            query?: {
                /** @description 지금에서 끝나는 범위: `hour`, `day`, `week`, `month`(30일), `year`(365일). `from`·`until`과 함께 쓰지 않는다. */
                range?: "hour" | "day" | "week" | "month" | "year";
                /** @description 범위 시작(유닉스 밀리초) */
                from?: number;
                /** @description 범위 끝(유닉스 밀리초, 없으면 지금). `from`과 함께만 쓴다. 범위는 366일까지다. */
                until?: number;
                /** @description 돌려줄 점 수의 상한(1~1000). 기본은 300이고 클러스터 모으기는 120이다. */
                points?: number;
            };
            header?: never;
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["HistoryReply"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_node_topology: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NodeTopologyInfo"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    unfreeze_node: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_NodeSpec_NodeStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    refresh_updates: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    upgrade_node: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description node id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_notification_targets: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_NotificationTargetView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_notification_target: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["NotificationTargetRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NotificationTargetView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_notification_target: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NotificationTargetView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    update_notification_target: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["NotificationTargetRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NotificationTargetView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_notification_target: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    test_notification_target: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NotificationTestResult"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    effective_permissions: {
        parameters: {
            query: {
                /** @description 사용자(이름이나 id). 없으면 요청한 주체다. */
                user?: string;
                /** @description 대상: `root`, `folder/<id>`, `vm/<key>`, `network/<key>`, `storage-pool/<key>`(key는 이름이나 id) */
                object: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["EffectivePermissions"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    my_permissions: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MyPermissions"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    who_permissions: {
        parameters: {
            query: {
                /** @description 대상: `root`, `folder/<id>`, `vm/<key>`, `network/<key>`, `storage-pool/<key>` */
                object: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["WhoPermissions"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_placement: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PlacementStatus"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_placement_rules: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_PlacementRuleSpec_PlacementRuleStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_placement_rule: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRequest_PlacementRuleSpec"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_PlacementRuleSpec_PlacementRuleStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_placement_rule: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_PlacementRuleSpec_PlacementRuleStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    update_placement_rule: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateRequest_PlacementRuleSpec"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_PlacementRuleSpec_PlacementRuleStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_placement_rule: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_recommendations: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PlacementMove"][];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    apply_recommendation: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description recommendation id */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_realms: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_RealmView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_realm: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RealmRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RealmView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_realm: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RealmView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    update_realm: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RealmRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RealmView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_realm: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    test_realm: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: {
            content: {
                "application/json": components["schemas"]["RealmTestRequest"] | null;
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RealmTestResult"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_remotes: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_RemoteView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_remote: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RemoteRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RemoteView"];
                };
            };
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    probe_remote: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RemoteProbeRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RemoteProbe"];
                };
            };
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_remote: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RemoteView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    update_remote: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RemoteRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RemoteView"];
                };
            };
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_remote: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    refresh_remote: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RemoteView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_routers: {
        parameters: {
            query?: {
                /** @description 거짓이면 OVN NB를 읽지 않는다(`hosting`·`active`가 비고 `live_error`도 없다). 기본 참 */
                live?: boolean;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RouterList"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_router: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRouter"];
            };
        };
        responses: {
            201: {
                headers: {
                    /** @description no gateway node for the external network (only then) */
                    "orbivisor-warning"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_RouterSpec_RouterStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_router: {
        parameters: {
            query?: {
                /** @description 거짓이면 OVN NB를 읽지 않는다(`hosting`·`active`가 비고 `live_error`도 없다). 기본 참 */
                live?: boolean;
            };
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RouterView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_router: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    patch_router: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RouterPatch"];
            };
        };
        responses: {
            200: {
                headers: {
                    /** @description no gateway node for the external network (only then) */
                    "orbivisor-warning"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_RouterSpec_RouterStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_sdn: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SdnView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    init_sdn: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SdnInitRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    add_central: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SdnNodeRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    /** @description the central count becomes even (only then) */
                    "orbivisor-warning"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    remove_central: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description node id or name (a deleted node: the name or id `strays` shows) */
                node: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    /** @description the central count becomes even (only then) */
                    "orbivisor-warning"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    install_chassis: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SdnChassisRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    /** @description offline nodes `all` skipped (only then) */
                    "orbivisor-warning"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"][];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    set_lockdown: {
        parameters: {
            query?: {
                /** @description 참이면 바꾸지 않고 허용 집합과 그 밖의 상대만 돌려준다(200). */
                dry_run?: boolean;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SdnLockdownRequest"];
            };
        };
        responses: {
            /** @description The check only (dry_run) */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["LockdownCheck"];
                };
            };
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_security_groups: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_SecurityGroupSpec_SecurityGroupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_security_group: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRequest_SecurityGroupSpec"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_SecurityGroupSpec_SecurityGroupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_security_group: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_SecurityGroupSpec_SecurityGroupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_security_group: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    patch_security_group: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SecurityGroupPatch"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_SecurityGroupSpec_SecurityGroupStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_settings: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_SettingsSpec_SettingsStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    patch_settings: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SettingsPatch"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SettingsPatched"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_snapshots: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_SnapshotSpec_SnapshotStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_pools: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_StoragePoolSpec_StoragePoolStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_pool: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRequest_StoragePoolSpec"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_StoragePoolSpec_StoragePoolStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_pool: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_StoragePoolSpec_StoragePoolStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_pool: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    patch_pool: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PoolPatch"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_StoragePoolSpec_StoragePoolStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_pool_history: {
        parameters: {
            query?: {
                /** @description 지금에서 끝나는 범위: `hour`, `day`, `week`, `month`(30일), `year`(365일). `from`·`until`과 함께 쓰지 않는다. */
                range?: "hour" | "day" | "week" | "month" | "year";
                /** @description 범위 시작(유닉스 밀리초) */
                from?: number;
                /** @description 범위 끝(유닉스 밀리초, 없으면 지금). `from`과 함께만 쓴다. 범위는 366일까지다. */
                until?: number;
                /** @description 돌려줄 점 수의 상한(1~1000). 기본은 300이고 클러스터 모으기는 120이다. */
                points?: number;
            };
            header?: never;
            path: {
                /** @description pool id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["HistoryReply"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_subscription: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SubscriptionInfo"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    put_subscription: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SubscriptionKey"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SubscriptionInfo"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_subscription: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SubscriptionInfo"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_tasks: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_task: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    preview_telemetry: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TelemetryReport"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    reset_telemetry_id: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_SettingsSpec_SettingsStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_tokens: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_TokenView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_token: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateTokenRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CreatedToken"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_token: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    topology_usage: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TopologyUsage"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_users: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_UserView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_user: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateUserRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["UserView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    update_user: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateUserRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["UserView"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_user: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    reset_two_factor: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Reset */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_version: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["VersionInfo"];
                };
            };
        };
    };
    list_vm_imports: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_VmImportSpec_VmImportStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_esxi: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["EsxiImportRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_VmImportSpec_VmImportStatus"];
                };
            };
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    browse_esxi: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["EsxiConnection"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["EsxiBrowse"];
                };
            };
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    upload_ova: {
        parameters: {
            query: {
                /** @description 가져오기 이름 */
                name: string;
                /** @description 올린 파일 이름(표시용). 없으면 `{name}.ova` */
                file_name?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/octet-stream": components["schemas"]["ImageFile"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_VmImportSpec_VmImportStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    check_ova: {
        parameters: {
            query: {
                /** @description 가져오기 이름 */
                name: string;
                /** @description 올릴 파일 크기(바이트) */
                size: number;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_vm_import: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_VmImportSpec_VmImportStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_import: {
        parameters: {
            query?: {
                /** @description 원본 노드가 오프라인이거나 클러스터에 없어도 리소스만 지운다. 원본 파일은 그 노드에 남는다. */
                force?: boolean;
            };
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description force: only the resource was deleted */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["VmImportForgotten"];
                };
            };
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    run_import: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ImportRun"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_vms: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_VmSpec_VmStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_vm: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRequest_VmSpec"];
            };
        };
        responses: {
            201: {
                headers: {
                    /** @description a warning about the stored spec (only when there is one) */
                    "orbivisor-warning"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_VmSpec_VmStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_vm: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_VmSpec_VmStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    update_vm: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateRequest_VmSpec"];
            };
        };
        responses: {
            200: {
                headers: {
                    /** @description id of the vm-hotplug task that applies a hardware change to the running VM (only when one was created) */
                    "orbivisor-task"?: string;
                    /** @description a warning about the stored spec (only when there is one) */
                    "orbivisor-warning"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_VmSpec_VmStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_vm: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    vm_action: {
        parameters: {
            query?: {
                /** @description `stop`에만 쓴다. `force`면 ACPI 응답을 기다리지 않는다. */
                mode?: "acpi" | "force";
            };
            header?: never;
            path: {
                /** @description id or name */
                key: string;
                /** @description start, stop, reboot or hotplug */
                action: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_backup: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description VM id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateBackupRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    cdrom_media: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CdromMediaRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    /** @description id of the vm-hotplug task that changes the medium of the running VM (only when one was created) */
                    "orbivisor-task"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_VmSpec_VmStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    clone_vm: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description VM id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CloneRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    console: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description WebSocket carrying the VNC (RFB) stream */
            101: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_vm_cpu: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["VmCpuInfo"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    reset_firmware: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["FirmwareResetRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_guest: {
        parameters: {
            query?: {
                /** @description 캐시(60초마다 모음) 대신 에이전트에 지금 묻는다. VM마다 5초에 한 번이고 그 사이에는 캐시를 돌려준다. */
                refresh?: boolean;
            };
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuestInfoResponse"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    set_guest_password: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["GuestPasswordRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuestPasswordResponse"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    cancel_live_restore: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description VM id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CancelLiveRestoreRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_vm_metrics: {
        parameters: {
            query?: {
                /** @description 이 `t`(그 노드 시계의 유닉스 밀리초)보다 뒤의 점만. 없으면 남은 것 모두(1시간) */
                since?: number;
            };
            header?: never;
            path: {
                /** @description VM id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["VmMetricsResponse"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_vm_history: {
        parameters: {
            query?: {
                /** @description 지금에서 끝나는 범위: `hour`, `day`, `week`, `month`(30일), `year`(365일). `from`·`until`과 함께 쓰지 않는다. */
                range?: "hour" | "day" | "week" | "month" | "year";
                /** @description 범위 시작(유닉스 밀리초) */
                from?: number;
                /** @description 범위 끝(유닉스 밀리초, 없으면 지금). `from`과 함께만 쓴다. 범위는 366일까지다. */
                until?: number;
                /** @description 돌려줄 점 수의 상한(1~1000). 기본은 300이고 클러스터 모으기는 120이다. */
                points?: number;
            };
            header?: never;
            path: {
                /** @description VM id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["HistoryReply"];
                };
            };
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    migrate_vm: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["MigrateRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_vm_numa: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["VmNumaInfo"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    replicate_vm: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["VmReplicateRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_vm_snapshots: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description VM id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_SnapshotSpec_SnapshotStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_snapshot: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description VM id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateSnapshotRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_snapshot: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description VM id or name */
                key: string;
                /** @description snapshot name or id */
                name: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_SnapshotSpec_SnapshotStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_snapshot: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description VM id or name */
                key: string;
                /** @description snapshot name or id */
                name: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    revert_snapshot: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description VM id or name */
                key: string;
                /** @description snapshot name or id */
                name: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    convert_template: {
        parameters: {
            query?: never;
            header?: {
                "If-Match"?: number | null;
            };
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_VmSpec_VmStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    untemplate_vm: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    list_volumes: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["List_Resource_VolumeSpec_VolumeStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    create_volume: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRequest_VolumeSpec"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    download_volume: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["VolumeDownloadRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    import_volume: {
        parameters: {
            query: {
                name: string;
                pool: string;
                /** @description 만들 볼륨의 형식 (생략하면 풀 종류로: lvm-thin·zfs는 raw, 그 밖은 qcow2) */
                format?: "qcow2" | "raw";
                source_format: "vmdk" | "qcow2" | "raw";
                /** @description 내용 종류(기본 disk). iso는 dir·nfs 풀에 raw로 둔다. */
                content?: "disk" | "iso";
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/octet-stream": components["schemas"]["ImageFile"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    check_import_volume: {
        parameters: {
            query: {
                name: string;
                pool: string;
                /** @description 만들 볼륨의 형식 (생략하면 풀 종류로: lvm-thin·zfs는 raw, 그 밖은 qcow2) */
                format?: "qcow2" | "raw";
                source_format: "vmdk" | "qcow2" | "raw";
                /** @description 내용 종류(기본 disk) */
                content?: "disk" | "iso";
                /** @description 올릴 파일 크기(바이트) */
                size: number;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    get_volume: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_VolumeSpec_VolumeStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    delete_volume: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    flatten_volume: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    resize_volume: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ResizeRequest"];
            };
        };
        responses: {
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Resource_TaskSpec_TaskStatus"];
                };
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    watch: {
        parameters: {
            query: {
                /** @description 쉼표로 구분한 종류 이름. 예: `vm,task` */
                kinds: string;
                /** @description 목록 응답의 `revision`. 그 뒤의 변경부터 받는다. */
                since: number;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description WebSocket stream of change events */
            101: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            "4XX": {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
}
