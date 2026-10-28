// 생성된 파일이다. 고치지 말고 `pnpm gen:api`로 다시 만든다.

export interface paths {
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
        /** 노드에 미리 만든 Linux 브리지를 가리키는 네트워크. 이름 `user`는 예약되어 있다. */
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
        /** 노드의 주소와 유지보수 모드를 바꾼다. 노드 리소스는 opld가 시작할 때 스스로 만든다. */
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
        /** 볼륨이 남아 있으면 409 */
        delete: operations["delete_pool"];
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
         * spec 전체를 바꾼다. 실행 중인 VM의 `node`와 `disks`는 바꿀 수 없다. 다른 변경은 다음 시작부터
         *     적용된다. `If-Match`가 없으면 검증한 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 검증한다.
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
         *     돌려준다(202). 시작·정지 중이면 409 `vm_busy`, qcow2가 아닌 디스크가 있으면 409
         *     `snapshot_unsupported`. 실행 중이 아니거나 디스크가 없는 VM의 메모리 스냅샷은 400.
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
         *     스냅샷 스펙의 볼륨이 모두 있고 다 만들어졌으며 다른 VM이 쓰지 않아야 한다.
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
         *     거기서 한다. 쓰는 VM이 없거나 정할 노드가 없으면 볼륨의 노드에서 한다.
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
        /**
         * @description CPU 아키텍처. 노드가 등록할 때 기록하고, 머신 타입마다 하나로 정해진다.
         * @enum {string}
         */
        Arch: "x86_64" | "aarch64";
        BridgeInfo: {
            name: string;
            /** @description VLAN-aware 브리지(`vlan_filtering 1`)인지. VLAN 태그를 쓰는 네트워크에 필요하다. */
            vlan_filtering: boolean;
        };
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
        CreateRequest_NetworkSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            spec: {
                /** @description 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`) */
                bridge: string;
                /**
                 * Format: int32
                 * @description VLAN 태그(1~4094). 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
                 */
                vlan?: number | null;
            };
        };
        CreateRequest_StoragePoolSpec: {
            labels?: {
                [key: string]: string;
            };
            name: string;
            spec: {
                backend: components["schemas"]["PoolBackend"];
                /** @description 이 풀을 쓸 수 있는 노드. 비어 있으면 모든 노드. */
                nodes?: string[];
                /** @description 모든 노드가 같은 데이터를 보는 공유 스토리지인지. NFS 풀은 늘 공유다(`is_shared`). */
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
        /** @enum {string} */
        Firmware: "bios" | "uefi";
        /**
         * Format: binary
         * @description 업로드 본문의 문서용 스키마: 바이너리 파일
         */
        ImageFile: string;
        /** @enum {string} */
        ImageFormat: "qcow2" | "raw";
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
                spec: {
                    /** @description 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`) */
                    bridge: string;
                    /**
                     * Format: int32
                     * @description VLAN 태그(1~4094). 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
                     */
                    vlan?: number | null;
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
                    /** @description 이 풀을 쓸 수 있는 노드. 비어 있으면 모든 노드. */
                    nodes?: string[];
                    /** @description 모든 노드가 같은 데이터를 보는 공유 스토리지인지. NFS 풀은 늘 공유다(`is_shared`). */
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
                     * @description 대상 리소스(VM 또는 Volume)의 id. 스냅샷·복제 Task는 (원본) VM이다.
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
                    /** @default null */
                    last_error: string | null;
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
                /** Format: int64 */
                revision: number;
                role: components["schemas"]["Role"];
            }[];
            /** Format: int64 */
            revision: number;
        };
        LoginRequest: {
            password: string;
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
            /** Format: uuid */
            id: string;
            /** @description Node 리소스 이름. 아직 없으면(합류 중) 비어 있다. */
            name?: string | null;
            online: boolean;
            /** @description 투표 멤버인지(아니면 learner) */
            voter: boolean;
        };
        NetworkSpec: {
            /** @description 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`) */
            bridge: string;
            /**
             * Format: int32
             * @description VLAN 태그(1~4094). 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
             */
            vlan?: number | null;
        };
        NetworkStatus: Record<string, never>;
        /** @enum {string} */
        NicModel: "virtio" | "e1000e";
        NicSpec: {
            mac?: components["schemas"]["MacAddr"] | null;
            model: components["schemas"]["NicModel"];
            /** @description Network 리소스 이름 */
            network: string;
        };
        NodeSpec: {
            /** @description API 주소 (`host:port`) */
            address: string;
            /** @description 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다. */
            maintenance?: boolean;
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
        ResizeRequest: {
            /**
             * Format: int64
             * @description 새 크기(바이트). 지금보다 크고 512의 배수여야 한다.
             */
            size_bytes: number;
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
            spec: {
                /** @description 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`) */
                bridge: string;
                /**
                 * Format: int32
                 * @description VLAN 태그(1~4094). 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
                 */
                vlan?: number | null;
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
                /** @description 이 풀을 쓸 수 있는 노드. 비어 있으면 모든 노드. */
                nodes?: string[];
                /** @description 모든 노드가 같은 데이터를 보는 공유 스토리지인지. NFS 풀은 늘 공유다(`is_shared`). */
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
                 * @description 대상 리소스(VM 또는 Volume)의 id. 스냅샷·복제 Task는 (원본) VM이다.
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
                /** @default null */
                last_error: string | null;
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
        /** @enum {string} */
        Role: "admin" | "operator" | "viewer";
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
        /**
         * @description 가져올 원본 이미지 형식
         * @enum {string}
         */
        SourceFormat: "vmdk" | "qcow2" | "raw";
        StoragePoolSpec: {
            backend: components["schemas"]["PoolBackend"];
            /** @description 이 풀을 쓸 수 있는 노드. 비어 있으면 모든 노드. */
            nodes?: string[];
            /** @description 모든 노드가 같은 데이터를 보는 공유 스토리지인지. NFS 풀은 늘 공유다(`is_shared`). */
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
             * @description 대상 리소스(VM 또는 Volume)의 id. 스냅샷·복제 Task는 (원본) VM이다.
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
            /** Format: int64 */
            revision: number;
            role: components["schemas"]["Role"];
        };
        /** @enum {string} */
        VmPhase: "stopped" | "starting" | "running" | "stopping" | "failed";
        /** @description VM의 원하는 상태. */
        VmSpec: {
            cpu: components["schemas"]["CpuSpec"];
            disks?: components["schemas"]["DiskSpec"][];
            firmware?: components["schemas"]["Firmware"];
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
            /** @default null */
            last_error: string | null;
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
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
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
                /** @description 만들 볼륨의 형식 (기본 qcow2) */
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
                /** @description 만들 볼륨의 형식 (기본 qcow2) */
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
