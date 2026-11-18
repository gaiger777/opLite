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
         * 노드에 미리 만든 Linux 브리지를 가리키거나 orbivisord가 만드는 VXLAN 네트워크. 이름 `user`는 예약되어 있다.
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
         * 노드의 주소, 유지보수 모드, 역할(`roles`)을 바꾼다. 노드 리소스는 orbivisord가 시작할 때 스스로 만든다.
         *     위트니스 여부(`witness`)는 합류할 때 정해지므로 요청의 값을 무시하고 그대로 둔다. 위트니스는 API가
         *     없으므로 `api` 역할을 줄 수 없다. `roles`를 적으려면 모든 노드가 그 필드를 알아야 한다(`node-roles`).
         */
        put: operations["update_node"];
        post?: never;
        /**
         * 죽은 노드를 클러스터에서 뺀다. 살아 있는 노드는 그 노드에서 `orbivisorctl cluster leave`로 뺀다.
         *     그 노드에 배정된 VM은 그대로 남는다. VM들의 복제 대상(계획 55)에서는 그 노드를 뺀다. Ceph 데몬(mon·mgr·OSD)
         *     노드였으면 Orbivisor의 Ceph 기록에서도 빼고(Ceph는 바꾸지 않는다) 손으로 치울 명령을 경고로 돌려준다(200).
         */
        delete: operations["delete_node"];
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
         *     쓰이지 않게 하면 응답 헤더 `orbivisor-warning`으로 알린다.
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
         *     없음) `start`는 409 `tpm_state_unavailable`이다.
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
         *     스케줄러가 고를 수 없는 노드(풀, 대상 풀, 네트워크, 남은 메모리)면 409 `no_capacity`다. 대상 노드가 hard
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
         * @description 게스트 에이전트가 있는 VM을 끄는 방법
         * @enum {string}
         */
        AgentShutdown: "agent" | "acpi";
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
            vms?: components["schemas"]["VmSelector"];
        };
        BackupJobSpec: {
            /**
             * @description 끄면 예약 실행을 하지 않는다.
             * @default true
             */
            enabled: boolean;
            /**
             * Format: int32
             * @description 0보다 크면 증분으로 백업하되(할 수 있으면) 체인의 증분이 이만큼 쌓이면 다음은 전체로 한다.
             *     0이면 늘 전체다(뺀다).
             */
            full_every?: number;
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
        /** @description 백업 디렉터리의 `manifest.json`. Orbivisor 없이도 백업을 알아볼 수 있게 VM 스펙과 디스크 목록을 담는다. */
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
            /**
             * Format: uuid
             * @description 증분 백업의 부모 백업 id. 디스크 파일은 바뀐 블록만 담고, 부모는 헤더가 아니라 여기로만 잇는다.
             *     전체 백업이면 뺀다.
             */
            parent?: string;
            /**
             * @description TPM 상태 파일(`tpm2-00.permall`, 0600)을 담았는지(계획 59). 거짓이면 빼고 써서 형식 버전은 그대로다. 옛 노드는
             *     이 필드를 무시한다.
             */
            tpm_state?: boolean;
            uefi_vars: boolean;
            /**
             * Format: int32
             * @description 형식 버전([`MANIFEST_VERSION`], 증분 백업은 [`MANIFEST_VERSION_INCREMENTAL`])
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
            /** @description TPM 상태 파일(`tpm2-00.permall`)을 담았는지(계획 59). 거짓이면 뺀다. */
            tpm_state?: boolean;
            /**
             * @description UEFI 변수 파일(`vars.fd`)을 담았는지
             * @default false
             */
            uefi_vars: boolean;
        };
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
            /**
             * Format: int32
             * @default 2048
             */
            memory_mib: number;
            /** @description 템플릿 VM 이름 */
            name: string;
            /** @description NIC 하나의 네트워크 이름 */
            network: string;
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
         * @description 업데이트 채널. APT 소스 파일의 `Components`가 된다.
         * @enum {string}
         */
        Channel: "stable" | "testing" | "enterprise";
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
        /** @description 배포(복제) 때 원본 스펙에서 바꾸는 값. 비어 있는 칸은 원본 그대로다. */
        CloneOverrides: {
            /** @description 주어진·비지 않은 칸만 원본의 cloud-init 설정을 바꾼다. 비밀번호는 crypt 해시다. */
            cloud_init?: components["schemas"]["CloudInit"];
            /** @description 디스크 순번별 새 크기(바이트, 늘리기만). null은 그대로다. */
            disks?: (number | null)[];
            /** Format: int32 */
            memory_mib?: number;
            /** @description NIC 순번별 새 네트워크 이름. null은 그대로다. */
            nics?: (string | null)[];
            /**
             * Format: uuid
             * @description 새 VM을 켤 노드(`spec.node`). 디스크가 닿아야 한다.
             */
            node?: string;
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
            /** @description 디스크 순번별 새 크기(바이트, 늘리기만, 512의 배수). null은 그대로다. */
            disks?: (number | null)[];
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
            /** @description 새 VM을 켤 노드(이름이나 id, `spec.node`). 디스크가 닿아야 한다(공유 풀이거나 그 노드의 로컬 풀). */
            node?: string;
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
        /** @enum {string} */
        CpuModel: "host" | "max";
        CpuSpec: {
            /** Format: int32 */
            cores: number;
            /**
             * Format: int32
             * @description vCPU 핫플러그 상한(계획 54). 없으면 `sockets × cores`와 같다(핫플러그 꺼짐). 실행 중에는 `cores`를 이 안에서
             *     바꾼다(소켓마다 코어 `max_vcpus / sockets`개 자리).
             */
            max_vcpus?: number;
            model?: components["schemas"]["CpuModel"];
            /** Format: int32 */
            sockets?: number;
        };
        CreateBackupRequest: {
            /**
             * @description 할 수 있으면 증분으로(같은 풀의 바로 앞 백업을 부모로 바뀐 블록만) 백업한다. 할 수 없으면
             *     전체로 하고 그 이유를 `status.full_reason`에 적는다.
             */
            incremental?: boolean;
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
                /**
                 * Format: int32
                 * @description 0보다 크면 증분으로 백업하되(할 수 있으면) 체인의 증분이 이만큼 쌓이면 다음은 전체로 한다.
                 *     0이면 늘 전체다(뺀다).
                 */
                full_every?: number;
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
                 * @description 게스트 에이전트(계획 59): 채널을 달고 게스트 정보·에이전트 종료·fs-freeze에 쓴다. 없으면 쓰지 않아 옛 노드도
                 *     VM을 푼다(`ha.guest_watch`가 있으면 채널은 그대로 단다).
                 */
                guest_agent?: components["schemas"]["GuestAgentSpec"];
                /**
                 * @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다.
                 *     JSON은 옛 bool(`false` = 없음, `true` = 기본 정책)이나 정책 객체다.
                 */
                ha?: components["schemas"]["HaSetting"];
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
        CreateSnapshotRequest: {
            /** @description 최대 1024자 */
            description?: string;
            /** @description 메모리 상태도 담는다. 기본은 VM이 실행 중이고 디스크가 있으며 실행 중에 바꾼 하드웨어가 없으면 true다. */
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
            /** @description 새 노드에서 `orbivisorctl cluster join`에 넘길 URL. 다시 볼 수 없다. */
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
        /** @description 이 그룹(보통 DN)에 든 사용자는 이 역할 */
        GroupRole: {
            group: string;
            role: components["schemas"]["Role"];
        };
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
            /** @description 원본 네트워크 → Orbivisor 네트워크. 원본 NIC의 네트워크마다 있어야 한다. */
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
        /**
         * @description 인터페이스 종류(`ip -d`의 `info_kind`, 없으면 이더넷)
         * @enum {string}
         */
        InterfaceKind: "ethernet" | "bond" | "bridge" | "vlan" | "dummy" | "veth" | "other";
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
                    /**
                     * Format: int32
                     * @description 0보다 크면 증분으로 백업하되(할 수 있으면) 체인의 증분이 이만큼 쌓이면 다음은 전체로 한다.
                     *     0이면 늘 전체다(뺀다).
                     */
                    full_every?: number;
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
                    /** @description TPM 상태 파일(`tpm2-00.permall`)을 담았는지(계획 59). 거짓이면 뺀다. */
                    tpm_state?: boolean;
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
                    /** @description 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다. */
                    maintenance?: boolean;
                    /**
                     * @description 이 노드의 역할. 없으면 기본 역할(위트니스가 아니면 `api`)이다. 옛 노드는 이 필드를 모르므로 기본이면
                     *     쓰지 않는다.
                     */
                    roles?: components["schemas"]["NodeRole"][];
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
                    /** @description 이 노드의 HA 검사 결과(화면용, 바뀔 때만 쓴다). 리더는 리스 갱신에 실린 결과로 판단한다. */
                    checks?: components["schemas"]["CheckReport"][];
                    /**
                     * @description 이 노드가 원하는 클러스터 링크(`host:port`, 앞이 우선). 노드가 등록할 때 설정의 `cluster_addresses`로
                     *     쓴다(옛 노드는 비어 있다). 리더가 Raft 멤버십의 링크를 이것에 맞춘다.
                     */
                    cluster_addresses?: string[];
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
                     * @description 호스트가 꺼지는 중이라 이 노드가 종료 정책대로 정리하며 남긴 표시. 노드가 다시 등록할 때 지운다. 리더는
                     *     `freeze` 표시가 있는 오프라인 노드의 HA VM을 옮기지 않는다(옛 노드는 이 필드를 무시한다).
                     */
                    shutdown?: components["schemas"]["NodeShutdown"];
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
                     * @description 게스트 에이전트(계획 59): 채널을 달고 게스트 정보·에이전트 종료·fs-freeze에 쓴다. 없으면 쓰지 않아 옛 노드도
                     *     VM을 푼다(`ha.guest_watch`가 있으면 채널은 그대로 단다).
                     */
                    guest_agent?: components["schemas"]["GuestAgentSpec"];
                    /**
                     * @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다.
                     *     JSON은 옛 bool(`false` = 없음, `true` = 기본 정책)이나 정책 객체다.
                     */
                    ha?: components["schemas"]["HaSetting"];
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
        /**
         * @description 옮기는 이유
         * @enum {string}
         */
        MoveReason: "balance" | "rule";
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
            /** @description orbivisord가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만드는 네트워크 */
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
            /** @description 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다. */
            maintenance?: boolean;
            /**
             * @description 이 노드의 역할. 없으면 기본 역할(위트니스가 아니면 `api`)이다. 옛 노드는 이 필드를 모르므로 기본이면
             *     쓰지 않는다.
             */
            roles?: components["schemas"]["NodeRole"][];
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
            /** @description 이 노드의 HA 검사 결과(화면용, 바뀔 때만 쓴다). 리더는 리스 갱신에 실린 결과로 판단한다. */
            checks?: components["schemas"]["CheckReport"][];
            /**
             * @description 이 노드가 원하는 클러스터 링크(`host:port`, 앞이 우선). 노드가 등록할 때 설정의 `cluster_addresses`로
             *     쓴다(옛 노드는 비어 있다). 리더가 Raft 멤버십의 링크를 이것에 맞춘다.
             */
            cluster_addresses?: string[];
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
             * @description 호스트가 꺼지는 중이라 이 노드가 종료 정책대로 정리하며 남긴 표시. 노드가 다시 등록할 때 지운다. 리더는
             *     `freeze` 표시가 있는 오프라인 노드의 HA VM을 옮기지 않는다(옛 노드는 이 필드를 무시한다).
             */
            shutdown?: components["schemas"]["NodeShutdown"];
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
        /** @description 실행 중에 적용하지 못해 다음 재시작 때 적용할 변경 하나 */
        PendingChange: {
            /** @description 이유의 자세한 설명(예: 실패한 까닭) */
            detail?: string;
            /**
             * @description `vcpus`, `cpu`, `cpu.max_vcpus`, `memory_mib`, `max_memory_mib`, `firmware`, `machine`, `tpm`,
             *     `secure_boot`, `guest_agent`
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
        /** @description 인증된 요청의 주체. 요청마다 저장소의 User에서 다시 읽으므로 역할 변경과 삭제가 곧바로 반영된다. */
        Principal: {
            name: string;
            role: components["schemas"]["Role"];
            /** Format: uuid */
            user: string;
        };
        /** @description Prometheus 끝점 설정 */
        PrometheusSettings: {
            /** @description 끄면 끝점이 404다. */
            enabled?: boolean;
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
            /**
             * Format: uuid
             * @description 증분 백업이면 받는 쪽(이 클러스터)에 있는 부모 사본의 id. 전체 백업이면 뺀다.
             */
            parent?: string | null;
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
                /**
                 * Format: int32
                 * @description 0보다 크면 증분으로 백업하되(할 수 있으면) 체인의 증분이 이만큼 쌓이면 다음은 전체로 한다.
                 *     0이면 늘 전체다(뺀다).
                 */
                full_every?: number;
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
                /** @description TPM 상태 파일(`tpm2-00.permall`)을 담았는지(계획 59). 거짓이면 뺀다. */
                tpm_state?: boolean;
                /**
                 * @description UEFI 변수 파일(`vars.fd`)을 담았는지
                 * @default false
                 */
                uefi_vars: boolean;
            };
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
                /** @description 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다. */
                maintenance?: boolean;
                /**
                 * @description 이 노드의 역할. 없으면 기본 역할(위트니스가 아니면 `api`)이다. 옛 노드는 이 필드를 모르므로 기본이면
                 *     쓰지 않는다.
                 */
                roles?: components["schemas"]["NodeRole"][];
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
                /** @description 이 노드의 HA 검사 결과(화면용, 바뀔 때만 쓴다). 리더는 리스 갱신에 실린 결과로 판단한다. */
                checks?: components["schemas"]["CheckReport"][];
                /**
                 * @description 이 노드가 원하는 클러스터 링크(`host:port`, 앞이 우선). 노드가 등록할 때 설정의 `cluster_addresses`로
                 *     쓴다(옛 노드는 비어 있다). 리더가 Raft 멤버십의 링크를 이것에 맞춘다.
                 */
                cluster_addresses?: string[];
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
                 * @description 호스트가 꺼지는 중이라 이 노드가 종료 정책대로 정리하며 남긴 표시. 노드가 다시 등록할 때 지운다. 리더는
                 *     `freeze` 표시가 있는 오프라인 노드의 HA VM을 옮기지 않는다(옛 노드는 이 필드를 무시한다).
                 */
                shutdown?: components["schemas"]["NodeShutdown"];
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
                /**
                 * @description 앞단·VIP에서 API를 부르는 이름(DNS 이름·IP). 각 노드가 자기 API 인증서에 더한다(피어 인증서는 그대로).
                 *     비어 있으면 쓰지 않는다.
                 */
                api_names?: string[];
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
                 * @description 게스트 에이전트(계획 59): 채널을 달고 게스트 정보·에이전트 종료·fs-freeze에 쓴다. 없으면 쓰지 않아 옛 노드도
                 *     VM을 푼다(`ha.guest_watch`가 있으면 채널은 그대로 단다).
                 */
                guest_agent?: components["schemas"]["GuestAgentSpec"];
                /**
                 * @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다.
                 *     JSON은 옛 bool(`false` = 없음, `true` = 기본 정책)이나 정책 객체다.
                 */
                ha?: components["schemas"]["HaSetting"];
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
            /** @description 지금 QEMU에 붙은 디스크(순번 = QEMU의 `disk{i}`). 실행 중 붙이고 떼기를 스펙과 견준다. */
            disks?: components["schemas"]["DiskSpec"][];
            firmware: components["schemas"]["Firmware"];
            /**
             * @description 켤 때 게스트 에이전트 채널을 달았다(계획 59). 거짓이면 쓰지 않는다. 계획 59 전 노드의 기록에는 없으므로
             *     `ha.guest_watch`만 있는 VM은 거짓이어도 채널이 있을 수 있다.
             */
            guest_agent?: boolean;
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
            /** @description 노드 API 인증서에 더할 이름(DNS 이름·IP) 전체. `[]`는 지워 노드 이름만 쓴다. */
            api_names?: string[];
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
            /**
             * @description 호스트가 꺼지는 중(재부팅·전원 끄기)일 때 노드가 HA VM을 다루는 방법. `conditional`(기본: 재부팅이면
             *     `freeze`, 끄기면 `failover`), `freeze`, `failover`, `migrate`. `systemctl stop`에는 쓰지 않는다.
             */
            shutdown_policy?: components["schemas"]["ShutdownPolicy"];
            telemetry?: components["schemas"]["TelemetryPatch"];
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
        SettingsSpec: {
            /**
             * @description 앞단·VIP에서 API를 부르는 이름(DNS 이름·IP). 각 노드가 자기 API 인증서에 더한다(피어 인증서는 그대로).
             *     비어 있으면 쓰지 않는다.
             */
            api_names?: string[];
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
         * @description 호스트 종료 때 노드가 한 일
         * @enum {string}
         */
        ShutdownAction: "freeze" | "failover" | "migrate";
        /**
         * @description 노드 정상 종료 정책(설계 §8). `systemctl stop`·업그레이드 재시작이 아니라 호스트가 꺼지는 중일 때만 쓴다.
         * @enum {string}
         */
        ShutdownPolicy: "conditional" | "freeze" | "failover" | "migrate";
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
            /** @description 펌웨어. API가 카탈로그 항목의 것으로 채운다(없으면 UEFI). */
            firmware?: components["schemas"]["Firmware"];
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
        TokenView: {
            /** Format: uuid */
            id: string;
            name: string;
            /** Format: int64 */
            revision: number;
            /** Format: uuid */
            user: string;
        };
        UpdateRequest_HaCheckSpec: {
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
            labels?: {
                [key: string]: string;
            };
            spec: {
                /** @description API 주소 (`host:port`) */
                address: string;
                /** @description 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다. */
                maintenance?: boolean;
                /**
                 * @description 이 노드의 역할. 없으면 기본 역할(위트니스가 아니면 `api`)이다. 옛 노드는 이 필드를 모르므로 기본이면
                 *     쓰지 않는다.
                 */
                roles?: components["schemas"]["NodeRole"][];
                /** @description 위트니스 노드: Raft 투표만 하고 VM을 돌리지 않는다. 합류할 때 정해지고 바뀌지 않는다. */
                witness?: boolean;
            };
        };
        UpdateRequest_PlacementRuleSpec: {
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
                 * @description 게스트 에이전트(계획 59): 채널을 달고 게스트 정보·에이전트 종료·fs-freeze에 쓴다. 없으면 쓰지 않아 옛 노드도
                 *     VM을 푼다(`ha.guest_watch`가 있으면 채널은 그대로 단다).
                 */
                guest_agent?: components["schemas"]["GuestAgentSpec"];
                /**
                 * @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다.
                 *     JSON은 옛 bool(`false` = 없음, `true` = 기본 정책)이나 정책 객체다.
                 */
                ha?: components["schemas"]["HaSetting"];
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
        };
        VersionInfo: {
            /** @description 이 노드의 orbivisord 빌드 버전 */
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
        /** @enum {string} */
        VmPhase: "stopped" | "starting" | "running" | "stopping" | "failed" | "migrating";
        /** @description VM의 최적 배치 옵션 */
        VmPlacement: {
            /** @description 균형 조정기가 옮기지 않는다(HA 복구·비우기·사용자 마이그레이션은 그대로 옮긴다). */
            pinned?: boolean;
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
             * @description 게스트 에이전트(계획 59): 채널을 달고 게스트 정보·에이전트 종료·fs-freeze에 쓴다. 없으면 쓰지 않아 옛 노드도
             *     VM을 푼다(`ha.guest_watch`가 있으면 채널은 그대로 단다).
             */
            guest_agent?: components["schemas"]["GuestAgentSpec"];
            /**
             * @description HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다.
             *     JSON은 옛 bool(`false` = 없음, `true` = 기본 정책)이나 정책 객체다.
             */
            ha?: components["schemas"]["HaSetting"];
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
