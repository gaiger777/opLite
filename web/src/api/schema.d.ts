// 생성된 파일이다. 고치지 말고 `pnpm gen:api`로 다시 만든다.

export interface paths {
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
         */
        post: operations["login"];
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
        /** 세션을 끝내고 브라우저의 세션 쿠키를 지운다. */
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
         *     받을 자리가 없으면 507 `insufficient_storage`, 옛 노드가 있으면 409 `cluster_upgrading`.
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
         *     바로 지우고 200이다(파일은 백업 풀에 남는다).
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
         *     (`backup`은 이 클러스터의 백업 id로) 쓰고 `ready`로 바꾼다. 빠지거나 크기가 다른 파일이 있으면 409
         *     `backup_incomplete`(`details.files`가 그 파일들), 받는 중이 아니면 409 `backup_not_receiving`, 지우는
         *     Task가 걸려 있으면 409 `backup_busy`.
         */
        post: operations["complete_backup"];
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
         *     `vars.fd`여야 하고, `Content-Length`는 디스크 파일이면 매니페스트의 크기, `vars.fd`면 64 MiB까지다
         *     (400). 본문 조각이 60초 넘게 오지 않으면 받은 것을 버리고 400(멈춘 업로드). `Content-Length`가 없으면
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
         *     `backup_on_other_node`. 고른 노드가 오프라인이면 409 `node_offline`.
         */
        post: operations["restore_backup"];
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
        /** 클러스터 이름, CA 지문, 멤버(이 노드가 아는 Raft 멤버십) */
        get: operations["get_cluster"];
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
         * 노드에 미리 만든 Linux 브리지를 가리키거나 opld가 만드는 VXLAN 네트워크. 이름 `user`는 예약되어 있다.
         *     VNI가 다른 네트워크와 겹치면 409 `vni_in_use`(동시에 만드는 두 요청의 경쟁은 막지 않는다).
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
        /** VM이 쓰고 있으면 409 */
        delete: operations["delete_network"];
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
         * 노드의 주소와 유지보수 모드를 바꾼다. 노드 리소스는 opld가 시작할 때 스스로 만든다.
         *     역할(`witness`)은 합류할 때 정해지므로 요청의 값을 무시하고 그대로 둔다.
         */
        put: operations["update_node"];
        post?: never;
        /**
         * 죽은 노드를 클러스터에서 뺀다. 살아 있는 노드는 그 노드에서 `oplctl cluster leave`로 뺀다.
         *     그 노드에 배정된 VM은 그대로 남는다.
         */
        delete: operations["delete_node"];
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
         * 노드의 패키지를 올릴 Task(`node-upgrade`, 그 노드에서 돈다)를 돌려준다(202). apt는 opld 밖의 일시
         *     유닛에서 돌므로 VM은 계속 돌고 opld만 잠깐 다시 시작한다. 업데이트를 확인하지 않는 노드면 400
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
        /** 클러스터 설정. 로그인한 누구나 읽는다. */
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
         *     켤 때 id가 없으면 만든다.
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
         * 디렉터리 풀의 `shared`(다른 백엔드는 400)와 백업 풀 표시 `backups`(dir·NFS 풀만 켤 수 있다)를
         *     바꾼다. 둘 다 없으면 400이다. 로컬 → 공유는 언제나 된다. 공유 → 로컬은 풀의 볼륨을 쓰는 VM이 그
         *     볼륨을 만든 노드가 아닌 노드에 있거나 끝나지 않은 마이그레이션 Task가 있으면 409 `pool_in_use`다.
         *     백업이 남은 풀의 `backups`를 끄거나 `shared`를 바꾸면 409 `pool_in_use`다. 같은 값이면 쓰지 않고
         *     그대로 200이다.
         */
        patch: operations["patch_pool"];
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
    "/version": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** 이 노드의 빌드 버전. 인증 없이 읽는다(로그인 화면과 oplctl이 서버 버전을 본다). */
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
         *     없으면 `import_failed`, 원본 노드가 오프라인이면 `node_offline`. 원본 NIC의 네트워크마다 있는 opLite
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
        /** 빠진 MAC을 채우고, 켜진 채로 만들면 노드를 고른다. */
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
         * spec 전체를 바꾼다. 실행 중인 VM의 `node`와 `disks`는 바꿀 수 없고 `ha`를 켤 수 없다. 보안 그룹을
         *     건 NIC가 있으면 실행 중에는 NIC의 `security_groups`만 바꿀 수 있다. NIC의
         *     `security_groups`는 곧바로, 다른 변경은 다음 시작부터 적용된다. `If-Match`가 없으면 검증한 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 검증한다.
         */
        put: operations["update_vm"];
        post?: never;
        /**
         * 꺼진 VM만 지울 수 있다. 스냅샷이 남아 있으면 409 `vm_has_snapshots`, 끝나지 않은 스냅샷·복제
         *     Task가 있으면 409 `vm_busy`(복제는 원본 VM과 그 스냅샷을 읽는다).
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
         *     `stop?mode=force`와 `reboot`은 대상 노드의 Task 실행기가 수행한다.
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
         */
        post: operations["create_backup"];
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
         *     한다. 디스크가 모두 공유 풀에 있어야 하고, HVF(macOS) 노드는 원본도 대상도 될 수 없다(409
         *     `migration_unsupported`). 대상 노드가 위트니스이거나 오프라인이거나 유지보수 중이면 409 `node_unavailable`,
         *     HA VM인데 대상이 HA 가능 노드가 아니면 409 `ha_unsupported`,
         *     스케줄러가 고를 수 없는 노드(풀, 네트워크, 남은 메모리)면 409 `no_capacity`다.
         */
        post: operations["migrate_vm"];
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
         *     409 `snapshot_unsupported`. 실행 중이 아니거나 디스크가 없는 VM의 메모리 스냅샷은 400.
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
         *     스냅샷의 VM 스펙이 가리키면 409 `volume_in_snapshot`.
         */
        delete: operations["delete_volume"];
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
         *     시작한 QEMU가 늘리는 중인 볼륨을 열 수 있다.
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
        /** @enum {string} */
        Action: "accept" | "drop";
        /**
         * @description CPU 아키텍처. 노드가 등록할 때 기록하고, 머신 타입마다 하나로 정해진다.
         * @enum {string}
         */
        Arch: "x86_64" | "aarch64";
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
            enabled?: boolean;
            /** @description 보존 규칙 전체를 이것으로 바꾼다(적지 않은 단위는 0). */
            keep?: components["schemas"]["Keep"];
            pool?: string;
            schedule?: components["schemas"]["JobSchedule"];
            vms?: components["schemas"]["VmSelector"];
        };
        BackupJobSpec: {
            /**
             * @description 끄면 예약 실행을 하지 않는다.
             * @default true
             */
            enabled: boolean;
            /** @description 보존 규칙. 모두 0이면 지우지 않는다. */
            keep?: components["schemas"]["Keep"];
            /** @description 백업 풀 이름 */
            pool: string;
            /** @description 만든 백업을 보낼 원격 클러스터의 백업 풀. 없으면 보내지 않는다. */
            replicate?: components["schemas"]["ReplicateTarget"];
            schedule: components["schemas"]["JobSchedule"];
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
        /** @description 백업 디렉터리의 `manifest.json`. opLite 없이도 백업을 알아볼 수 있게 VM 스펙과 디스크 목록을 담는다. */
        BackupManifest: {
            /**
             * Format: uuid
             * @description 백업 id
             */
            backup: string;
            /**
             * Format: int64
             * @description 만든 시각(유닉스 초)
             */
            created_at: number;
            disks: components["schemas"]["BackupDisk"][];
            uefi_vars: boolean;
            /**
             * Format: int32
             * @description 형식 버전([`MANIFEST_VERSION`])
             */
            version: number;
            /** Format: uuid */
            vm: string;
            vm_name: string;
            vm_spec: components["schemas"]["VmSpec"];
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
        BackupSpec: {
            /**
             * Format: uuid
             * @description 이 백업을 만든 백업 작업 id. 손으로 만든 백업이면 없다.
             */
            job?: string | null;
            /** @description 다른 클러스터에서 받은 사본이면 그 원본. 이 클러스터에서 만든 백업이면 없다. */
            origin?: components["schemas"]["BackupOrigin"];
            /** @description 백업 풀 이름 */
            pool: string;
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
            /**
             * Format: uuid
             * @description 파일을 쓴 노드. 공유가 아닌 풀의 백업은 이 노드에만 있으므로 지우기·복원도 이 노드에서 한다.
             *     공유 풀이면 없다.
             */
            node?: string;
            /** @default pending */
            phase: components["schemas"]["BackupPhase"];
            /**
             * Format: int64
             * @description 받는 중인 사본이 마지막으로 파일을 받은 시각(유닉스 초). 받은 적이 없으면 뺀다.
             */
            received_at?: number;
            /** @description 원격 클러스터에 보낸 사본. 없으면 뺀다. */
            replicas?: components["schemas"]["BackupReplica"][];
            /**
             * @description UEFI 변수 파일(`vars.fd`)을 담았는지
             * @default false
             */
            uefi_vars: boolean;
        };
        BridgeInfo: {
            name: string;
            /** @description VLAN-aware 브리지(`vlan_filtering 1`)인지. VLAN 태그를 쓰는 네트워크에 필요하다. */
            vlan_filtering: boolean;
        };
        /**
         * @description 업데이트 채널. APT 소스 파일의 `Components`가 된다.
         * @enum {string}
         */
        Channel: "stable" | "testing" | "enterprise";
        CloneRequest: {
            /** @description 새 VM의 이름 */
            name: string;
            /** @description 새 디스크를 둘 풀. 없으면 원본 볼륨마다 그 볼륨의 풀이다. */
            pool?: string | null;
            /** @description 복제할 스냅샷(이름이나 id). 없으면 꺼진 VM의 지금 상태에서 복제한다. */
            snapshot?: string | null;
        };
        ClusterView: {
            /** @description 클러스터 CA 인증서의 지문. `oplctl login --ca-fingerprint`에 쓴다. */
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
        /** @enum {string} */
        CpuModel: "host" | "max";
        CpuSpec: {
            /** Format: int32 */
            cores: number;
            model?: components["schemas"]["CpuModel"];
            /** Format: int32 */
            sockets?: number;
        };
        CreateBackupRequest: {
            /** @description 백업 이름(클러스터에서 고유). 없으면 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC)다. */
            name?: string | null;
            /** @description 백업 풀(`backups`를 켠 풀) */
            pool: string;
        };
        CreateRequest_BackupJobSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            spec: {
                /**
                 * @description 끄면 예약 실행을 하지 않는다.
                 * @default true
                 */
                enabled: boolean;
                /** @description 보존 규칙. 모두 0이면 지우지 않는다. */
                keep?: components["schemas"]["Keep"];
                /** @description 백업 풀 이름 */
                pool: string;
                /** @description 만든 백업을 보낼 원격 클러스터의 백업 풀. 없으면 보내지 않는다. */
                replicate?: components["schemas"]["ReplicateTarget"];
                schedule: components["schemas"]["JobSchedule"];
                /** @description 백업할 VM */
                vms: components["schemas"]["VmSelector"];
            };
        };
        CreateRequest_NetworkSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            /** @description `bridge`와 `vxlan` 중 정확히 하나를 쓴다. */
            spec: {
                /** @description 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`) */
                bridge?: string;
                /**
                 * Format: int32
                 * @description VXLAN 네트워크의 MTU(1280~9000, 기본 1450). `vxlan`과만 쓴다.
                 */
                mtu?: number;
                /**
                 * Format: int32
                 * @description VLAN 태그(1~4094). `bridge`와만 쓰고, 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
                 */
                vlan?: number | null;
                /** @description opld가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만드는 네트워크 */
                vxlan?: components["schemas"]["Vxlan"];
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
                cpu: components["schemas"]["CpuSpec"];
                disks?: components["schemas"]["DiskSpec"][];
                firmware?: components["schemas"]["Firmware"];
                /** @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다. */
                ha?: boolean;
                machine: components["schemas"]["MachineType"];
                /** Format: int32 */
                memory_mib: number;
                nics?: components["schemas"]["NicSpec"][];
                /**
                 * Format: uuid
                 * @description 배치된 노드의 id. None이면 시작할 때 스케줄러가 고른다.
                 */
                node?: string | null;
                power?: components["schemas"]["PowerState"];
                restart?: components["schemas"]["RestartPolicy"];
                snapshot_schedule?: components["schemas"]["SnapshotSchedule"] | null;
            };
        };
        CreateRequest_VolumeSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            spec: {
                format: components["schemas"]["ImageFormat"];
                /** @description StoragePool 리소스 이름 */
                pool: string;
                /** Format: int64 */
                size_bytes: number;
            };
        };
        CreateSnapshotRequest: {
            /** @description 최대 1024자 */
            description?: string;
            /** @description 메모리 상태도 담는다. 기본은 VM이 실행 중이고 디스크가 있으면 true다. */
            memory?: boolean | null;
            /** @description VM 안에서 고유한 이름 */
            name: string;
        };
        CreateTokenRequest: {
            name: string;
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
            /** @description 새 노드에서 `oplctl cluster join`에 넘길 URL. 다시 볼 수 없다. */
            url: string;
        };
        CreatedToken: {
            /** @description `Authorization: Bearer <secret>`로 쓴다. 다시 볼 수 없다. */
            secret: string;
            token: components["schemas"]["TokenView"];
        };
        /** @enum {string} */
        Direction: "in" | "out";
        /** @enum {string} */
        DiskBus: "virtio-blk" | "sata";
        DiskSpec: {
            /** Format: int32 */
            boot_order?: number | null;
            bus: components["schemas"]["DiskBus"];
            /** @description Volume 리소스 이름 */
            volume: string;
        };
        /** @description 오류 응답 본문 */
        ErrorBody: {
            /** @description 기계가 읽는 오류 종류. 예: `not_found`, `revision_mismatch`, `cluster_no_quorum` */
            code: string;
            details?: unknown;
            message: string;
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
        /**
         * @description 리스를 잃은 노드가 HA VM을 끄는 방법
         * @enum {string}
         */
        Fencing: "watchdog" | "process";
        /** @enum {string} */
        Firmware: "bios" | "uefi";
        /** @description 이 그룹(보통 DN)에 든 사용자는 이 역할 */
        GroupRole: {
            group: string;
            role: components["schemas"]["Role"];
        };
        /**
         * Format: binary
         * @description 업로드 본문의 문서용 스키마: 바이너리 파일
         */
        ImageFile: string;
        /** @enum {string} */
        ImageFormat: "qcow2" | "raw";
        /** @enum {string} */
        ImportPhase: "ready" | "importing" | "done" | "failed";
        /** @description opLite VM 설정 제안. 실행할 때 [`ImportRun`]에 없는 값의 기본값이다. */
        ImportProposal: {
            /** Format: int32 */
            cores: number;
            disk_bus: components["schemas"]["DiskBus"];
            firmware: components["schemas"]["Firmware"];
            /** Format: int32 */
            memory_mib: number;
            /** @description 원본 네트워크 → 이름이 같은 opLite 네트워크(없으면 null, 실행 때 정해야 한다) */
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
            disk_bus?: components["schemas"]["DiskBus"] | null;
            firmware?: components["schemas"]["Firmware"] | null;
            /**
             * @description 원본 MAC 주소를 그대로 쓴다. 끄면 새 MAC을 받는다.
             * @default true
             */
            keep_macs: boolean;
            /** Format: int32 */
            memory_mib?: number | null;
            /** @description 만들 VM 이름 */
            name: string;
            /** @description 원본 네트워크 → opLite 네트워크. 원본 NIC의 네트워크마다 있어야 한다. */
            networks?: {
                [key: string]: string;
            };
            nic_model?: components["schemas"]["NicModel"] | null;
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
            /** @description 새 노드의 노드 간 통신 주소(`host:port`) */
            cluster_address: string;
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
            /** @description 새 노드의 opld 빌드 버전. 옛 버전 노드는 보내지 않는다. */
            version?: string | null;
            /** @description 새 노드가 위트니스(투표만 하고 VM을 돌리지 않는 노드)인지 */
            witness?: boolean;
        };
        JoinResponse: {
            ca_pem: string;
            /** @description 새 노드의 인증서(PEM) */
            cert_pem: string;
            cluster: string;
            /** @description 이 노드의 노드 간 통신 주소. 새 노드는 여기로 승격을 요청한다. */
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
         * @description 구독 수준
         * @enum {string}
         */
        Level: "standard" | "premium";
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
                    /**
                     * @description 끄면 예약 실행을 하지 않는다.
                     * @default true
                     */
                    enabled: boolean;
                    /** @description 보존 규칙. 모두 0이면 지우지 않는다. */
                    keep?: components["schemas"]["Keep"];
                    /** @description 백업 풀 이름 */
                    pool: string;
                    /** @description 만든 백업을 보낼 원격 클러스터의 백업 풀. 없으면 보내지 않는다. */
                    replicate?: components["schemas"]["ReplicateTarget"];
                    schedule: components["schemas"]["JobSchedule"];
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
                     * Format: uuid
                     * @description 이 백업을 만든 백업 작업 id. 손으로 만든 백업이면 없다.
                     */
                    job?: string | null;
                    /** @description 다른 클러스터에서 받은 사본이면 그 원본. 이 클러스터에서 만든 백업이면 없다. */
                    origin?: components["schemas"]["BackupOrigin"];
                    /** @description 백업 풀 이름 */
                    pool: string;
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
                    /**
                     * Format: uuid
                     * @description 파일을 쓴 노드. 공유가 아닌 풀의 백업은 이 노드에만 있으므로 지우기·복원도 이 노드에서 한다.
                     *     공유 풀이면 없다.
                     */
                    node?: string;
                    /** @default pending */
                    phase: components["schemas"]["BackupPhase"];
                    /**
                     * Format: int64
                     * @description 받는 중인 사본이 마지막으로 파일을 받은 시각(유닉스 초). 받은 적이 없으면 뺀다.
                     */
                    received_at?: number;
                    /** @description 원격 클러스터에 보낸 사본. 없으면 뺀다. */
                    replicas?: components["schemas"]["BackupReplica"][];
                    /**
                     * @description UEFI 변수 파일(`vars.fd`)을 담았는지
                     * @default false
                     */
                    uefi_vars: boolean;
                };
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
                /** @description `bridge`와 `vxlan` 중 정확히 하나를 쓴다. */
                spec: {
                    /** @description 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`) */
                    bridge?: string;
                    /**
                     * Format: int32
                     * @description VXLAN 네트워크의 MTU(1280~9000, 기본 1450). `vxlan`과만 쓴다.
                     */
                    mtu?: number;
                    /**
                     * Format: int32
                     * @description VLAN 태그(1~4094). `bridge`와만 쓰고, 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
                     */
                    vlan?: number | null;
                    /** @description opld가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만드는 네트워크 */
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
                    /** @description 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다. */
                    maintenance?: boolean;
                    /** @description 위트니스 노드: Raft 투표만 하고 VM을 돌리지 않는다. 합류할 때 정해지고 바뀌지 않는다. */
                    witness?: boolean;
                };
                /** @description 노드가 시작할 때 스스로 기록한다. */
                status: {
                    /** @default null */
                    arch: components["schemas"]["Arch"] | null;
                    /**
                     * @description 이 노드에 있는 Linux 브리지. 스케줄러가 브리지 네트워크를 쓰는 VM을 배치할 때 본다.
                     * @default []
                     */
                    bridges: components["schemas"]["BridgeInfo"][];
                    /**
                     * Format: int32
                     * @default 0
                     */
                    cpus: number;
                    /**
                     * @description 이 노드에 VM 방화벽(nftables)이 있다. 보안 그룹을 건 NIC가 있는 VM은 이런 노드에서만 돈다
                     *     (위트니스는 늘 false).
                     */
                    firewall?: boolean;
                    /** @description 이 노드의 HA 펜싱. None이면 펜싱이 꺼져 HA VM을 돌리지 않는다(위트니스는 늘 None). */
                    ha?: components["schemas"]["NodeHa"];
                    /**
                     * @description 예: `qemu-hvf`, `qemu-kvm`, `qemu-tcg`
                     * @default
                     */
                    hypervisor: string;
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
                    /** @description 이 노드에서 도는 opld의 빌드 버전. 노드가 등록할 때 쓴다(옛 버전 노드는 None). */
                    version?: string;
                };
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
                status: Record<string, never>;
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
                    cpu: components["schemas"]["CpuSpec"];
                    disks?: components["schemas"]["DiskSpec"][];
                    firmware?: components["schemas"]["Firmware"];
                    /** @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다. */
                    ha?: boolean;
                    machine: components["schemas"]["MachineType"];
                    /** Format: int32 */
                    memory_mib: number;
                    nics?: components["schemas"]["NicSpec"][];
                    /**
                     * Format: uuid
                     * @description 배치된 노드의 id. None이면 시작할 때 스케줄러가 고른다.
                     */
                    node?: string | null;
                    power?: components["schemas"]["PowerState"];
                    restart?: components["schemas"]["RestartPolicy"];
                    snapshot_schedule?: components["schemas"]["SnapshotSchedule"] | null;
                };
                /** @description VM의 실제 상태. reconciler만 기록한다. */
                status: {
                    /**
                     * Format: uuid
                     * @description 지금 UEFI 변수 파일을 가진 노드. UEFI VM을 켠 노드나 마이그레이션의 대상 노드가 쓴다.
                     *     다른 노드에서 켤 때 이 노드에서 변수를 받아 온다. BIOS VM은 늘 없다.
                     */
                    firmware_node?: string;
                    /** @default null */
                    last_error: string | null;
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
                    /** @default null */
                    schedule_error: components["schemas"]["ScheduleError"] | null;
                    /**
                     * Format: uuid
                     * @description 현재 위치: 마지막으로 만들었거나 되돌린 스냅샷. 다음 스냅샷의 부모가 된다.
                     * @default null
                     */
                    snapshot: string | null;
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
            }[];
            /** Format: int64 */
            revision: number;
        };
        LoginRequest: {
            password: string;
            /** @description 없거나 `local`이면 로컬 사용자 */
            realm?: string;
            /** @description 영역 로그인이면 영역 안의 이름이다(`alice`로 로그인하면 사용자 `alice@<영역>`). */
            username: string;
        };
        /** @description `52:54:00:12:34:56` 형식의 MAC 주소. */
        MacAddr: string;
        /** @enum {string} */
        MachineType: "q35" | "virt";
        /** @description 클러스터 멤버 한 노드 */
        MemberView: {
            /** @description API 주소 */
            address?: string | null;
            /** @description 노드 간 통신 주소 */
            cluster_address: string;
            fencing?: components["schemas"]["Fencing"] | null;
            /** Format: uuid */
            id: string;
            /** @description Node 리소스 이름. 아직 없으면(합류 중) 비어 있다. */
            name?: string | null;
            online: boolean;
            /** @description 노드의 opld 빌드 버전. 옛 버전 노드이거나 Node가 아직 없으면 null이다. */
            version?: string | null;
            /** @description 투표 멤버인지(아니면 learner) */
            voter: boolean;
            /** @description 위트니스 노드인지(VM을 돌리지 않는 투표 멤버) */
            witness: boolean;
        };
        MigrateRequest: {
            /** @description 옮겨 갈 노드(이름이나 id) */
            node: string;
        };
        /** @description 진행 중인 라이브 마이그레이션: 대상 노드와 그 마이그레이션 Task */
        MigrationStatus: {
            /** Format: uuid */
            node: string;
            /** Format: uuid */
            task: string;
        };
        /** @description `bridge`와 `vxlan` 중 정확히 하나를 쓴다. */
        NetworkSpec: {
            /** @description 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`) */
            bridge?: string;
            /**
             * Format: int32
             * @description VXLAN 네트워크의 MTU(1280~9000, 기본 1450). `vxlan`과만 쓴다.
             */
            mtu?: number;
            /**
             * Format: int32
             * @description VLAN 태그(1~4094). `bridge`와만 쓰고, 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
             */
            vlan?: number | null;
            /** @description opld가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만드는 네트워크 */
            vxlan?: components["schemas"]["Vxlan"];
        };
        NetworkStatus: Record<string, never>;
        /** @enum {string} */
        NicModel: "virtio" | "e1000e";
        NicSpec: {
            mac?: components["schemas"]["MacAddr"] | null;
            model: components["schemas"]["NicModel"];
            /** @description Network 리소스 이름 */
            network: string;
            /** @description 이 NIC에 거는 보안 그룹 이름(8개 이하, 겹치지 않게). 비어 있지 않으면 방화벽이 켜진다. */
            security_groups?: string[];
        };
        NodeCounts: {
            /** Format: int64 */
            total: number;
            /** Format: int64 */
            witness: number;
        };
        /** @description 노드가 등록할 때 쓰는 HA 펜싱 설정 */
        NodeHa: {
            fencing: components["schemas"]["Fencing"];
            /**
             * Format: int64
             * @description 마지막 리스 갱신 뒤 이만큼 지나면 이 노드의 HA VM이 꺼졌다고 보고 리더가 옮긴다(초).
             */
            recover_after_secs: number;
        };
        NodeSpec: {
            /** @description API 주소 (`host:port`) */
            address: string;
            /** @description 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다. */
            maintenance?: boolean;
            /** @description 위트니스 노드: Raft 투표만 하고 VM을 돌리지 않는다. 합류할 때 정해지고 바뀌지 않는다. */
            witness?: boolean;
        };
        /** @description 노드가 시작할 때 스스로 기록한다. */
        NodeStatus: {
            /** @default null */
            arch: components["schemas"]["Arch"] | null;
            /**
             * @description 이 노드에 있는 Linux 브리지. 스케줄러가 브리지 네트워크를 쓰는 VM을 배치할 때 본다.
             * @default []
             */
            bridges: components["schemas"]["BridgeInfo"][];
            /**
             * Format: int32
             * @default 0
             */
            cpus: number;
            /**
             * @description 이 노드에 VM 방화벽(nftables)이 있다. 보안 그룹을 건 NIC가 있는 VM은 이런 노드에서만 돈다
             *     (위트니스는 늘 false).
             */
            firewall?: boolean;
            /** @description 이 노드의 HA 펜싱. None이면 펜싱이 꺼져 HA VM을 돌리지 않는다(위트니스는 늘 None). */
            ha?: components["schemas"]["NodeHa"];
            /**
             * @description 예: `qemu-hvf`, `qemu-kvm`, `qemu-tcg`
             * @default
             */
            hypervisor: string;
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
            /** @description 이 노드에서 도는 opld의 빌드 버전. 노드가 등록할 때 쓴다(옛 버전 노드는 None). */
            version?: string;
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
        /** @enum {string} */
        PowerState: "running" | "stopped";
        /** @description 인증된 요청의 주체. 요청마다 저장소의 User에서 다시 읽으므로 역할 변경과 삭제가 곧바로 반영된다. */
        Principal: {
            name: string;
            role: components["schemas"]["Role"];
            /** Format: uuid */
            user: string;
        };
        /** @enum {string} */
        Protocol: "tcp" | "udp" | "icmp" | "icmpv6";
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
            /** @description 원본 백업의 매니페스트(`manifest.json`) */
            manifest: components["schemas"]["BackupManifest"];
            /** @description 받는 쪽 백업 이름. 없으면 `{VM 이름}-{만든 시각}`이고, 겹치면 뒤에 `-2`, `-3`…을 붙인다. */
            name?: string | null;
            origin: components["schemas"]["BackupOrigin"];
            /** @description 받을 백업 풀. 이 노드에서 쓸 수 있어야 한다. */
            pool: string;
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
        /** @enum {string} */
        ReplicaState: "copying" | "ready" | "failed";
        /** @description 백업을 보낼 곳: 원격 `remote`의 백업 풀 `pool` */
        ReplicateTarget: {
            /** @description 원격의 백업 풀 이름 */
            pool: string;
            /** @description 원격 이름 */
            remote: string;
        };
        ResizeRequest: {
            /**
             * Format: int64
             * @description 새 크기(바이트). 지금보다 크고 512의 배수여야 한다.
             */
            size_bytes: number;
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
                /**
                 * @description 끄면 예약 실행을 하지 않는다.
                 * @default true
                 */
                enabled: boolean;
                /** @description 보존 규칙. 모두 0이면 지우지 않는다. */
                keep?: components["schemas"]["Keep"];
                /** @description 백업 풀 이름 */
                pool: string;
                /** @description 만든 백업을 보낼 원격 클러스터의 백업 풀. 없으면 보내지 않는다. */
                replicate?: components["schemas"]["ReplicateTarget"];
                schedule: components["schemas"]["JobSchedule"];
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
                 * Format: uuid
                 * @description 이 백업을 만든 백업 작업 id. 손으로 만든 백업이면 없다.
                 */
                job?: string | null;
                /** @description 다른 클러스터에서 받은 사본이면 그 원본. 이 클러스터에서 만든 백업이면 없다. */
                origin?: components["schemas"]["BackupOrigin"];
                /** @description 백업 풀 이름 */
                pool: string;
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
                /**
                 * Format: uuid
                 * @description 파일을 쓴 노드. 공유가 아닌 풀의 백업은 이 노드에만 있으므로 지우기·복원도 이 노드에서 한다.
                 *     공유 풀이면 없다.
                 */
                node?: string;
                /** @default pending */
                phase: components["schemas"]["BackupPhase"];
                /**
                 * Format: int64
                 * @description 받는 중인 사본이 마지막으로 파일을 받은 시각(유닉스 초). 받은 적이 없으면 뺀다.
                 */
                received_at?: number;
                /** @description 원격 클러스터에 보낸 사본. 없으면 뺀다. */
                replicas?: components["schemas"]["BackupReplica"][];
                /**
                 * @description UEFI 변수 파일(`vars.fd`)을 담았는지
                 * @default false
                 */
                uefi_vars: boolean;
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
            /** @description `bridge`와 `vxlan` 중 정확히 하나를 쓴다. */
            spec: {
                /** @description 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`) */
                bridge?: string;
                /**
                 * Format: int32
                 * @description VXLAN 네트워크의 MTU(1280~9000, 기본 1450). `vxlan`과만 쓴다.
                 */
                mtu?: number;
                /**
                 * Format: int32
                 * @description VLAN 태그(1~4094). `bridge`와만 쓰고, 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
                 */
                vlan?: number | null;
                /** @description opld가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만드는 네트워크 */
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
                /** @description 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다. */
                maintenance?: boolean;
                /** @description 위트니스 노드: Raft 투표만 하고 VM을 돌리지 않는다. 합류할 때 정해지고 바뀌지 않는다. */
                witness?: boolean;
            };
            /** @description 노드가 시작할 때 스스로 기록한다. */
            status: {
                /** @default null */
                arch: components["schemas"]["Arch"] | null;
                /**
                 * @description 이 노드에 있는 Linux 브리지. 스케줄러가 브리지 네트워크를 쓰는 VM을 배치할 때 본다.
                 * @default []
                 */
                bridges: components["schemas"]["BridgeInfo"][];
                /**
                 * Format: int32
                 * @default 0
                 */
                cpus: number;
                /**
                 * @description 이 노드에 VM 방화벽(nftables)이 있다. 보안 그룹을 건 NIC가 있는 VM은 이런 노드에서만 돈다
                 *     (위트니스는 늘 false).
                 */
                firewall?: boolean;
                /** @description 이 노드의 HA 펜싱. None이면 펜싱이 꺼져 HA VM을 돌리지 않는다(위트니스는 늘 None). */
                ha?: components["schemas"]["NodeHa"];
                /**
                 * @description 예: `qemu-hvf`, `qemu-kvm`, `qemu-tcg`
                 * @default
                 */
                hypervisor: string;
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
                /** @description 이 노드에서 도는 opld의 빌드 버전. 노드가 등록할 때 쓴다(옛 버전 노드는 None). */
                version?: string;
            };
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
            status: Record<string, never>;
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
                /** @description 옵트인 텔레메트리. 기본값(꺼짐, 끝점 없음)이면 쓰지 않아 옛 노드도 설정을 계속 푼다. */
                telemetry?: components["schemas"]["TelemetrySettings"];
                updates?: components["schemas"]["UpdateSettings"];
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
                cpu: components["schemas"]["CpuSpec"];
                disks?: components["schemas"]["DiskSpec"][];
                firmware?: components["schemas"]["Firmware"];
                /** @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다. */
                ha?: boolean;
                machine: components["schemas"]["MachineType"];
                /** Format: int32 */
                memory_mib: number;
                nics?: components["schemas"]["NicSpec"][];
                /**
                 * Format: uuid
                 * @description 배치된 노드의 id. None이면 시작할 때 스케줄러가 고른다.
                 */
                node?: string | null;
                power?: components["schemas"]["PowerState"];
                restart?: components["schemas"]["RestartPolicy"];
                snapshot_schedule?: components["schemas"]["SnapshotSchedule"] | null;
            };
            /** @description VM의 실제 상태. reconciler만 기록한다. */
            status: {
                /**
                 * Format: uuid
                 * @description 지금 UEFI 변수 파일을 가진 노드. UEFI VM을 켠 노드나 마이그레이션의 대상 노드가 쓴다.
                 *     다른 노드에서 켤 때 이 노드에서 변수를 받아 온다. BIOS VM은 늘 없다.
                 */
                firmware_node?: string;
                /** @default null */
                last_error: string | null;
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
                /** @default null */
                schedule_error: components["schemas"]["ScheduleError"] | null;
                /**
                 * Format: uuid
                 * @description 현재 위치: 마지막으로 만들었거나 되돌린 스냅샷. 다음 스냅샷의 부모가 된다.
                 * @default null
                 */
                snapshot: string | null;
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
            /** @description 새 VM의 이름 */
            name: string;
            /** @description 복원을 실행할 노드(이름이나 id). 없으면 백업 풀과 대상 풀을 함께 쓸 수 있는 노드다. */
            node?: string | null;
            /** @description 새 디스크를 둘 풀 */
            pool: string;
        };
        /** @enum {string} */
        Role: "admin" | "operator" | "viewer";
        /** @description 그룹에서 역할 정하기 */
        RoleMapping: {
            /** @description 맞는 그룹이 없을 때의 역할. 없으면 로그인을 거절한다. */
            default?: components["schemas"]["Role"];
            groups?: components["schemas"]["GroupRole"][];
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
         * @description 예약 스냅샷이 실패한 이유. `code`는 거절한 API 오류의 code(`vm_busy` 등)이고, Task가 실패했으면
         *     `task_failed`다. 옛 상태의 문자열은 `{ code: "unknown", message: <문자열> }`로 읽는다.
         */
        ScheduleError: {
            code: string;
            message: string;
        };
        /** @enum {string} */
        ScheduleEvery: "hourly" | "daily" | "weekly";
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
        SecurityGroupStatus: Record<string, never>;
        /** @description 설정에서 바꿀 값. 주어진 필드만 바꾼다. */
        SettingsPatch: {
            telemetry?: components["schemas"]["TelemetryPatch"];
            updates?: components["schemas"]["UpdateSettingsPatch"];
        };
        SettingsSpec: {
            /** @description 옵트인 텔레메트리. 기본값(꺼짐, 끝점 없음)이면 쓰지 않아 옛 노드도 설정을 계속 푼다. */
            telemetry?: components["schemas"]["TelemetrySettings"];
            updates?: components["schemas"]["UpdateSettings"];
        };
        SettingsStatus: {
            /** @description 텔레메트리 상태. 처음 켤 때 생기고, 없으면 쓰지 않는다. */
            telemetry?: components["schemas"]["TelemetryStatus"];
        };
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
        StoragePoolSpec: {
            backend: components["schemas"]["PoolBackend"];
            /** @description 백업 파일(`{루트}/backups/`)을 둘 수 있는 백업 풀인지. dir·NFS 풀만 된다. 켜도 VM 디스크는 그대로 둔다. */
            backups?: boolean;
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
            /** @enum {string} */
            kind: "volume-delete";
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
            name: string;
            pool?: string | null;
            /** Format: uuid */
            snapshot?: string | null;
        } | {
            /** @enum {string} */
            kind: "vm-migrate";
            /** Format: uuid */
            node: string;
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
        TokenView: {
            /** Format: uuid */
            id: string;
            name: string;
            /** Format: int64 */
            revision: number;
            /** Format: uuid */
            user: string;
        };
        UpdateRequest_NodeSpec: {
            labels?: {
                [key: string]: string;
            };
            spec: {
                /** @description API 주소 (`host:port`) */
                address: string;
                /** @description 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다. */
                maintenance?: boolean;
                /** @description 위트니스 노드: Raft 투표만 하고 VM을 돌리지 않는다. 합류할 때 정해지고 바뀌지 않는다. */
                witness?: boolean;
            };
        };
        UpdateRequest_VmSpec: {
            labels?: {
                [key: string]: string;
            };
            /** @description VM의 원하는 상태. */
            spec: {
                cpu: components["schemas"]["CpuSpec"];
                disks?: components["schemas"]["DiskSpec"][];
                firmware?: components["schemas"]["Firmware"];
                /** @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다. */
                ha?: boolean;
                machine: components["schemas"]["MachineType"];
                /** Format: int32 */
                memory_mib: number;
                nics?: components["schemas"]["NicSpec"][];
                /**
                 * Format: uuid
                 * @description 배치된 노드의 id. None이면 시작할 때 스케줄러가 고른다.
                 */
                node?: string | null;
                power?: components["schemas"]["PowerState"];
                restart?: components["schemas"]["RestartPolicy"];
                snapshot_schedule?: components["schemas"]["SnapshotSchedule"] | null;
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
        UserView: {
            /** Format: uuid */
            id: string;
            name: string;
            /** @description 외부 사용자의 영역. 로컬 사용자는 없다. */
            realm?: string;
            /** Format: int64 */
            revision: number;
            role: components["schemas"]["Role"];
        };
        VersionInfo: {
            /** @description 이 노드의 opld 빌드 버전 */
            version: string;
        };
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
        /** @enum {string} */
        VmPhase: "stopped" | "starting" | "running" | "stopping" | "failed" | "migrating";
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
            cpu: components["schemas"]["CpuSpec"];
            disks?: components["schemas"]["DiskSpec"][];
            firmware?: components["schemas"]["Firmware"];
            /** @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다. */
            ha?: boolean;
            machine: components["schemas"]["MachineType"];
            /** Format: int32 */
            memory_mib: number;
            nics?: components["schemas"]["NicSpec"][];
            /**
             * Format: uuid
             * @description 배치된 노드의 id. None이면 시작할 때 스케줄러가 고른다.
             */
            node?: string | null;
            power?: components["schemas"]["PowerState"];
            restart?: components["schemas"]["RestartPolicy"];
            snapshot_schedule?: components["schemas"]["SnapshotSchedule"] | null;
        };
        /** @description VM의 실제 상태. reconciler만 기록한다. */
        VmStatus: {
            /**
             * Format: uuid
             * @description 지금 UEFI 변수 파일을 가진 노드. UEFI VM을 켠 노드나 마이그레이션의 대상 노드가 쓴다.
             *     다른 노드에서 켤 때 이 노드에서 변수를 받아 온다. BIOS VM은 늘 없다.
             */
            firmware_node?: string;
            /** @default null */
            last_error: string | null;
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
            /** @default null */
            schedule_error: components["schemas"]["ScheduleError"] | null;
            /**
             * Format: uuid
             * @description 현재 위치: 마지막으로 만들었거나 되돌린 스냅샷. 다음 스냅샷의 부모가 된다.
             * @default null
             */
            snapshot: string | null;
        };
        /**
         * @description 볼륨 파일이 준비됐는지. VM은 준비된 볼륨만 붙일 수 있다.
         * @enum {string}
         */
        VolumePhase: "pending" | "ready";
        VolumeSpec: {
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
        /** @enum {string} */
        Weekday: "mon" | "tue" | "wed" | "thu" | "fri" | "sat" | "sun";
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
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
                    "application/json": components["schemas"]["Principal"];
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
    put_backup_file: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description id or name */
                key: string;
                /** @description disk file of the manifest or vars.fd */
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
                "application/json": components["schemas"]["UpdateRequest_NodeSpec"];
            };
        };
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
                /** @description start, stop or reboot */
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
    import_volume: {
        parameters: {
            query: {
                name: string;
                pool: string;
                /** @description 만들 볼륨의 형식 (생략하면 풀 종류로: lvm-thin·zfs는 raw, 그 밖은 qcow2) */
                format?: "qcow2" | "raw";
                source_format: "vmdk" | "qcow2" | "raw";
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
