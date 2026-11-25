//! 시험·데모용 가짜 vSphere(기능 `fake-vsphere`, 설계 §7.5). 자체 서명 인증서의 HTTPS로 vCenter(또는 단독 ESXi)
//! SOAP(`/sdk`)과 데이터스토어 `/folder`(Range)를 흉내 내고, vCenter면 ESXi 호스트마다 따로 TLS 서버(인증서가 다름)를
//! 띄워 호스트 티켓(`vmware_cgi_ticket`) 읽기를 받는다. 응답 XML은 vcsim(govmomi)이 내는 꼴을 따른다.
//!
//! 디스크는 메모리의 **기반 + 델타 체인**(64 KiB 블록)이고 스냅샷 만들기·지우기(합치기)·CBT를 실제 의미대로 흉내
//! 낸다: 스냅샷 S를 만들면 그 아래 층이 얼고 쓰기는 새 델타로 간다. S(i)를 지우면 S(i) 위 델타가 그 아래 층으로
//! 합쳐진다 — 그래서 "S(n+1) 만들기 → S(n) 지우기"를 하면 기반 flat 파일이 늘 가장 최근 스냅샷 시점이다. CBT는
//! 켠 뒤 다음 stun(스냅샷·전원 켜기)부터 돌고, 스냅샷마다 `changeId = "{uuid}/{n}"`을 준다.
//!
//! 시험 손잡이: [`FakeVsphere::write`](켜진 VM의 게스트 쓰기), 전원, 결함 주입(다음 메서드 실패·CBT 초기화·범위 읽기
//! 중 끊기·느리게·티켓 거절·세션 만료·`ShutdownGuest` 무시·무료 ESXi), 요청 기록.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use orbivisor_types::vmware::{Secret, VmwarePower, VmwareSourceSpec};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::client::Session;
use super::http::fingerprint_of;
use super::soap::{MoRef, Node, escape, parse_call};

/// 디스크 블록(CBT 단위, 설계 §7.5)
pub const BLOCK: u64 = 64 << 10;
const NS: &str = "urn:vim25";

// ---- 설정 ----

/// 가짜 VM 하나
#[derive(Debug, Clone)]
pub struct FakeVmConfig {
    pub name: String,
    /// `vm` 폴더 아래 하위 폴더(없으면 바로 아래)
    pub folder: Option<String>,
    /// 호스트 순번
    pub host: usize,
    pub datastore: String,
    /// 디스크 크기(바이트, 64 KiB 배수가 아니어도 된다)
    pub disks: Vec<u64>,
    /// 처음 내용을 채운다(블록 셋 중 둘 — 나머지는 할당 안 됨)
    pub fill: bool,
    /// NIC마다 네트워크 이름(`VM Network`, `dvpg-web`, `nsx-seg`)
    pub nics: Vec<String>,
    pub power: VmwarePower,
    pub guest_id: String,
    pub efi: bool,
    pub tools: bool,
    pub cbt: bool,
    pub hw_version: u32,
    pub template: bool,
    pub cpus: u32,
    pub memory_mib: u64,
}

impl FakeVmConfig {
    pub fn linux(name: &str) -> Self {
        Self {
            name: name.into(),
            folder: None,
            host: 0,
            datastore: "ds1".into(),
            disks: vec![4 << 20],
            fill: true,
            nics: vec!["VM Network".into()],
            power: VmwarePower::PoweredOn,
            guest_id: "rhel9_64Guest".into(),
            efi: false,
            tools: true,
            cbt: false,
            hw_version: 19,
            template: false,
            cpus: 2,
            memory_mib: 2048,
        }
    }
}

/// 가짜 vSphere 설정
#[derive(Debug, Clone)]
pub struct FakeConfig {
    /// 단독 ESXi(`HostAgent`, 데이터센터 `ha-datacenter`, 호스트 하나·티켓 없이 세션 쿠키로 `/folder`)
    pub esxi: bool,
    pub user: String,
    pub password: String,
    /// vCenter의 ESXi 호스트 수(각자 TLS 서버)
    pub hosts: usize,
    pub vms: Vec<FakeVmConfig>,
    /// 원본 서버 주소(없으면 127.0.0.1의 빈 포트)
    pub listen: Option<SocketAddr>,
}

impl Default for FakeConfig {
    /// vCenter, 호스트 둘, VM 둘: `web01`(Prod 폴더, 켜짐, 4 MiB 디스크 둘, 표준·분산 NIC, Tools) ·
    /// `db01`(꺼짐, Windows·UEFI, 2 MiB 디스크 하나, e1000).
    fn default() -> Self {
        let mut web = FakeVmConfig::linux("web01");
        web.folder = Some("Prod".into());
        web.disks = vec![4 << 20, 4 << 20];
        web.nics = vec!["VM Network".into(), "dvpg-web".into()];
        let mut db = FakeVmConfig::linux("db01");
        db.host = 1;
        db.datastore = "ds2".into();
        db.disks = vec![2 << 20];
        db.power = VmwarePower::PoweredOff;
        db.guest_id = "windows2019srv_64Guest".into();
        db.efi = true;
        db.hw_version = 14;
        Self {
            esxi: false,
            user: "migr@vsphere.local".into(),
            password: "VMware1!".into(),
            hosts: 2,
            vms: vec![web, db],
            listen: None,
        }
    }
}

impl FakeConfig {
    /// VM이 `n`대인 큰 인벤토리(빈 1 MiB 디스크 — 인벤토리 시간·메모리 재기)
    pub fn many(n: usize) -> Self {
        let mut c = Self::default();
        c.vms = (0..n)
            .map(|i| {
                let mut v = FakeVmConfig::linux(&format!("vm{i:04}"));
                v.folder = Some(format!("f{}", i % 20));
                v.host = i % c.hosts;
                v.disks = vec![1 << 20];
                v.fill = false;
                v
            })
            .collect();
        c
    }
}

// ---- 기록·결함 ----

/// 받은 요청 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeRequest {
    /// 0 = 원본(vCenter·ESXi), i+1 = 호스트 i
    pub server: usize,
    /// SOAP 메서드 이름 또는 HTTP 메서드(`GET`)
    pub method: String,
    /// `/folder` 경로(퍼센트 인코딩 그대로, 쿼리 포함)
    pub path: String,
    pub range: Option<String>,
    /// 유효한 세션 쿠키가 있었나
    pub session: bool,
    /// 보낸 호스트 티켓
    pub ticket: Option<String>,
    /// 요청 본문에 설정한 비밀번호가 들었나(Login만 그래야 한다)
    pub has_password: bool,
}

#[derive(Debug, Default)]
struct Faults {
    /// 메서드 → 결함 형식(한 번)
    next: HashMap<String, VecDeque>,
    cut_reads: u32,
    reject_tickets: bool,
    ignore_shutdown: bool,
    restricted: bool,
    delay: Duration,
    task_polls: u32,
    window: Option<u64>,
}

type VecDeque = std::collections::VecDeque<String>;

// ---- 세계 ----

#[derive(Debug, Clone)]
struct Disk {
    key: i32,
    capacity: u64,
    /// 데이터스토어 안 디스크립터 경로(확장자 없음, `web01/web01`·`web01/web01_1`)
    stem: String,
    /// layers[0] = 기반, layers[i] = 스냅샷 i-1을 만들 때 생긴 델타. 블록 번호 → 64 KiB
    layers: Vec<BTreeMap<u64, Vec<u8>>>,
    /// CBT 구간(마지막이 열린 구간). 블록 번호들
    epochs: Vec<BTreeSet<u64>>,
    base_mtime: u64,
    uuid: String,
}

#[derive(Debug, Clone)]
struct Snap {
    moref: String,
    name: String,
    description: String,
    id: u64,
    created: u64,
    /// 디스크마다 changeId(CBT가 돌 때)
    change_ids: Vec<Option<String>>,
}

#[derive(Debug, Clone)]
struct Nic {
    key: i32,
    network: String,
    adapter: &'static str,
    mac: String,
    start_connected: bool,
}

#[derive(Debug, Clone)]
struct Vm {
    moref: String,
    name: String,
    folder: String,
    host: usize,
    datastore: String,
    power: VmwarePower,
    guest_id: String,
    efi: bool,
    tools: bool,
    cbt_enabled: bool,
    cbt_active: bool,
    cbt_uuid: String,
    hw_version: u32,
    template: bool,
    cpus: u32,
    memory_mib: u64,
    uuid: String,
    instance_uuid: String,
    annotation: String,
    disks: Vec<Disk>,
    nics: Vec<Nic>,
    snaps: Vec<Snap>,
}

#[derive(Debug, Clone)]
struct TaskInfo {
    polls_left: u32,
    /// 끝났을 때의 결과(`<val …>`) 또는 결함(형식, 메시지)
    result: Result<Option<String>, (String, String)>,
}

#[derive(Debug, Clone)]
struct Datastore {
    moref: String,
    name: String,
    kind: &'static str,
}

#[derive(Debug, Clone)]
struct Network {
    moref: String,
    kind: &'static str,
    name: String,
    uplink: bool,
    opaque_id: Option<String>,
}

struct World {
    vms: Vec<Vm>,
    folders: Vec<(String, String, String, Vec<&'static str>)>,
    datastores: Vec<Datastore>,
    networks: Vec<Network>,
    tasks: HashMap<String, TaskInfo>,
    sessions: HashSet<String>,
    tickets: HashMap<String, String>,
    views: HashMap<String, Vec<MoRef>>,
    /// 토큰 → (남은 객체, 쪽 크기)
    pages: HashMap<String, (Vec<String>, usize)>,
    next: u64,
    clock: u64,
}

impl World {
    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn vm(&self, moref: &str) -> Option<&Vm> {
        self.vms.iter().find(|v| v.moref == moref)
    }

    fn vm_mut(&mut self, moref: &str) -> Option<&mut Vm> {
        self.vms.iter_mut().find(|v| v.moref == moref)
    }

    fn snapshot_owner(&self, snap: &str) -> Option<(usize, usize)> {
        self.vms.iter().enumerate().find_map(|(vi, v)| {
            v.snaps
                .iter()
                .position(|s| s.moref == snap)
                .map(|si| (vi, si))
        })
    }
}

struct Server {
    config: FakeConfig,
    world: Mutex<World>,
    faults: Mutex<Faults>,
    requests: Mutex<Vec<FakeRequest>>,
    /// 호스트 이름(`127.0.0.1:포트`)과 PEM 인증서
    hosts: Mutex<Vec<(String, String)>>,
    datacenter: String,
}

/// 떠 있는 가짜 vSphere. 버리면 멈춘다.
pub struct FakeVsphere {
    pub addr: SocketAddr,
    /// 원본 서버 인증서의 SHA-256 지문(대문자 콜론 구분)
    pub fingerprint: String,
    /// 원본 서버 인증서의 SHA-1 썸프린트
    pub thumbprint_sha1: String,
    /// 호스트마다 (주소, 지문)
    pub hosts: Vec<(SocketAddr, String)>,
    server: Arc<Server>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for FakeVsphere {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

fn hex_id(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("system random source");
    hex::encode(b)
}

fn fake_uuid(seed: u64) -> String {
    let h = format!("{seed:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// 처음 내용: 블록 셋 중 둘을 채운다(바이트는 VM·디스크·위치로 정함).
fn initial_layer(vm: usize, disk: usize, capacity: u64, fill: bool) -> BTreeMap<u64, Vec<u8>> {
    let mut layer = BTreeMap::new();
    if !fill {
        return layer;
    }
    let blocks = capacity.div_ceil(BLOCK);
    for b in 0..blocks {
        if b % 3 == 2 {
            continue;
        }
        let data: Vec<u8> = (0..BLOCK)
            .map(|i| ((b * BLOCK + i) as usize * 7 + vm * 31 + disk * 13) as u8 | 1)
            .collect();
        layer.insert(b, data);
    }
    layer
}

async fn tls_server(
    listen: SocketAddr,
    names: Vec<String>,
) -> (TcpListener, rustls::ServerConfig, String, String, String) {
    let key = rcgen::generate_simple_self_signed(names).expect("self-signed certificate");
    let der = key.cert.der().clone();
    let fingerprint = fingerprint_of(&der);
    let sha1 = super::http::sha1_thumbprint_of(&der);
    let pem = key.cert.pem();
    let private = rustls_pki_types::PrivateKeyDer::Pkcs8(key.signing_key.serialize_der().into());
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("TLS versions")
    .with_no_client_auth()
    .with_single_cert(vec![der], private)
    .expect("TLS certificate");
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let tcp = TcpListener::bind(listen).await.expect("bind fake vSphere");
    (tcp, config, fingerprint, sha1, pem)
}

impl FakeVsphere {
    pub async fn start(config: FakeConfig) -> FakeVsphere {
        let host_count = if config.esxi { 1 } else { config.hosts.max(1) };
        let datacenter = if config.esxi { "ha-datacenter" } else { "DC1" }.to_string();
        let mut world = World {
            vms: Vec::new(),
            folders: Vec::new(),
            datastores: vec![
                Datastore {
                    moref: "datastore-30".into(),
                    name: "ds1".into(),
                    kind: "VMFS",
                },
                Datastore {
                    moref: "datastore-31".into(),
                    name: "ds2".into(),
                    kind: "VMFS",
                },
                Datastore {
                    moref: "datastore-32".into(),
                    name: "vsanDatastore".into(),
                    kind: "vsan",
                },
            ],
            networks: vec![
                Network {
                    moref: "network-40".into(),
                    kind: "Network",
                    name: "VM Network".into(),
                    uplink: false,
                    opaque_id: None,
                },
                Network {
                    moref: "dvportgroup-11".into(),
                    kind: "DistributedVirtualPortgroup",
                    name: "DSwitch-DVUplinks-10".into(),
                    uplink: true,
                    opaque_id: None,
                },
                Network {
                    moref: "dvportgroup-12".into(),
                    kind: "DistributedVirtualPortgroup",
                    name: "dvpg-web".into(),
                    uplink: false,
                    opaque_id: None,
                },
                Network {
                    moref: "network-o13".into(),
                    kind: "OpaqueNetwork",
                    name: "nsx-seg".into(),
                    uplink: false,
                    opaque_id: Some("seg-1".into()),
                },
            ],
            tasks: HashMap::new(),
            sessions: HashSet::new(),
            tickets: HashMap::new(),
            views: HashMap::new(),
            pages: HashMap::new(),
            next: 1000,
            clock: 0,
        };
        // 폴더: (moref, 이름, 부모, 담는 형식)
        world.folders.push((
            "group-v3".into(),
            "vm".into(),
            "datacenter-2".into(),
            vec!["Folder", "VirtualMachine"],
        ));
        world.folders.push((
            "group-h4".into(),
            "host".into(),
            "datacenter-2".into(),
            vec!["Folder", "ComputeResource"],
        ));
        world.folders.push((
            "group-s5".into(),
            "datastore".into(),
            "datacenter-2".into(),
            vec!["Folder", "Datastore"],
        ));
        world.folders.push((
            "group-n6".into(),
            "network".into(),
            "datacenter-2".into(),
            vec!["Folder", "Network"],
        ));
        for (i, c) in config.vms.iter().enumerate() {
            let folder = match &c.folder {
                Some(name) => {
                    let existing = world
                        .folders
                        .iter()
                        .find(|f| f.1 == *name && f.2 == "group-v3");
                    match existing {
                        Some(f) => f.0.clone(),
                        None => {
                            let moref = format!("group-v{}", 50 + world.folders.len());
                            world.folders.push((
                                moref.clone(),
                                name.clone(),
                                "group-v3".into(),
                                vec!["Folder", "VirtualMachine"],
                            ));
                            moref
                        }
                    }
                }
                None => "group-v3".into(),
            };
            let seed = 0x5000 + i as u64;
            let disks = c
                .disks
                .iter()
                .enumerate()
                .map(|(d, &capacity)| Disk {
                    key: 2000 + d as i32,
                    capacity,
                    stem: if d == 0 {
                        format!("{0}/{0}", c.name)
                    } else {
                        format!("{0}/{0}_{d}", c.name)
                    },
                    layers: vec![initial_layer(i, d, capacity, c.fill)],
                    epochs: vec![BTreeSet::new()],
                    base_mtime: 0,
                    uuid: fake_uuid(seed * 100 + d as u64),
                })
                .collect();
            let windows = c.guest_id.starts_with("windows");
            let nics = c
                .nics
                .iter()
                .enumerate()
                .map(|(n, network)| Nic {
                    key: 4000 + n as i32,
                    network: network.clone(),
                    adapter: if windows {
                        "VirtualE1000"
                    } else {
                        "VirtualVmxnet3"
                    },
                    mac: format!(
                        "00:50:56:{:02x}:{:02x}:{:02x}",
                        (i >> 8) as u8,
                        i as u8,
                        n as u8
                    ),
                    start_connected: true,
                })
                .collect();
            world.vms.push(Vm {
                moref: format!("vm-{}", 100 + i),
                name: c.name.clone(),
                folder,
                host: c.host.min(host_count - 1),
                datastore: c.datastore.clone(),
                power: c.power,
                guest_id: c.guest_id.clone(),
                efi: c.efi,
                tools: c.tools,
                cbt_enabled: c.cbt,
                cbt_active: c.cbt && c.power == VmwarePower::PoweredOn,
                cbt_uuid: fake_uuid(seed),
                hw_version: c.hw_version,
                template: c.template,
                cpus: c.cpus,
                memory_mib: c.memory_mib,
                uuid: fake_uuid(seed + 0x10000),
                instance_uuid: fake_uuid(seed + 0x20000),
                annotation: String::new(),
                disks,
                nics,
                snaps: Vec::new(),
            });
        }
        let listen = config
            .listen
            .unwrap_or_else(|| "127.0.0.1:0".parse().unwrap());
        let (tcp, tls, fingerprint, thumbprint_sha1, main_pem) =
            tls_server(listen, vec!["localhost".into(), "127.0.0.1".into()]).await;
        let addr = tcp.local_addr().expect("local address");
        let server = Arc::new(Server {
            config,
            world: Mutex::new(world),
            faults: Mutex::default(),
            requests: Mutex::default(),
            hosts: Mutex::default(),
            datacenter,
        });
        let mut tasks = vec![serve(tcp, tls, server.clone(), 0)];
        let mut hosts = Vec::new();
        if server.config.esxi {
            server
                .hosts
                .lock()
                .unwrap()
                .push((format!("127.0.0.1:{}", addr.port()), main_pem));
        } else {
            for i in 0..host_count {
                let (tcp, tls, fp, _, pem) = tls_server(
                    "127.0.0.1:0".parse().unwrap(),
                    vec![format!("esx{}.fake.local", i + 1)],
                )
                .await;
                let a = tcp.local_addr().expect("local address");
                server
                    .hosts
                    .lock()
                    .unwrap()
                    .push((format!("127.0.0.1:{}", a.port()), pem));
                tasks.push(serve(tcp, tls, server.clone(), i + 1));
                hosts.push((a, fp));
            }
        }
        FakeVsphere {
            addr,
            fingerprint,
            thumbprint_sha1,
            hosts,
            server,
            tasks,
        }
    }

    /// 이 서버로 가는 원본 연결(비밀번호는 설정 것)
    pub fn source_spec(&self) -> VmwareSourceSpec {
        VmwareSourceSpec {
            host: "127.0.0.1".into(),
            port: self.addr.port(),
            user: self.server.config.user.clone(),
            password: Secret::new(self.server.config.password.clone()),
            fingerprint: self.fingerprint.clone(),
            transport: Default::default(),
            datacenter_hint: None,
        }
    }

    /// 로그인한 세션(vSphere Task를 짧은 간격으로 기다림)
    pub async fn login(&self) -> Session {
        Session::login(&self.source_spec())
            .await
            .expect("login to the fake vSphere")
            .with_poll(Duration::from_millis(5), Duration::from_millis(20))
    }

    pub fn requests(&self) -> Vec<FakeRequest> {
        self.server.requests.lock().unwrap().clone()
    }

    pub fn clear_requests(&self) {
        self.server.requests.lock().unwrap().clear();
    }

    fn with_vm<T>(&self, name: &str, f: impl FnOnce(&mut Vm, &mut u64) -> T) -> T {
        let mut w = self.server.world.lock().unwrap();
        let World { vms, clock, .. } = &mut *w;
        let vm = vms
            .iter_mut()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("no fake VM {name}"));
        f(vm, clock)
    }

    /// VM의 moref
    pub fn moref(&self, name: &str) -> String {
        self.with_vm(name, |v, _| v.moref.clone())
    }

    /// 게스트 쓰기: 지금 층(스냅샷이 있으면 맨 위 델타)에 쓰고 CBT가 돌면 기록한다.
    pub fn write(&self, vm: &str, disk: usize, offset: u64, data: &[u8]) {
        self.with_vm(vm, |v, clock| {
            let active = v.cbt_active;
            let no_snaps = v.snaps.is_empty();
            let d = &mut v.disks[disk];
            assert!(
                offset + data.len() as u64 <= d.capacity,
                "write past the end of the disk"
            );
            let mut at = 0usize;
            while at < data.len() {
                let pos = offset + at as u64;
                let b = pos / BLOCK;
                let inner = (pos % BLOCK) as usize;
                let n = (BLOCK as usize - inner).min(data.len() - at);
                let mut block = logical_block(&d.layers, d.layers.len() - 1, b);
                block[inner..inner + n].copy_from_slice(&data[at..at + n]);
                d.layers.last_mut().unwrap().insert(b, block);
                if active {
                    d.epochs.last_mut().unwrap().insert(b);
                }
                at += n;
            }
            if no_snaps {
                *clock += 1;
                d.base_mtime = *clock;
            }
        })
    }

    /// 게스트가 보는 지금 디스크 내용
    pub fn disk_bytes(&self, vm: &str, disk: usize) -> Vec<u8> {
        self.with_vm(vm, |v, _| {
            let d = &v.disks[disk];
            read_layers(&d.layers, d.layers.len() - 1, 0, d.capacity)
        })
    }

    /// 기반 flat 파일 내용
    pub fn base_bytes(&self, vm: &str, disk: usize) -> Vec<u8> {
        self.with_vm(vm, |v, _| {
            let d = &v.disks[disk];
            read_layers(&d.layers, 0, 0, d.capacity)
        })
    }

    pub fn power(&self, vm: &str) -> VmwarePower {
        self.with_vm(vm, |v, _| v.power)
    }

    pub fn set_power(&self, vm: &str, power: VmwarePower) {
        self.with_vm(vm, |v, _| {
            if power == VmwarePower::PoweredOn && v.cbt_enabled {
                activate_cbt(v);
            }
            v.power = power;
        })
    }

    /// 스냅샷 (moref, 이름) — 체인 차례
    pub fn snapshots(&self, vm: &str) -> Vec<(String, String)> {
        self.with_vm(vm, |v, _| {
            v.snaps
                .iter()
                .map(|s| (s.moref.clone(), s.name.clone()))
                .collect()
        })
    }

    /// 사용자 스냅샷(백업 제품 등)을 만든다.
    pub fn add_user_snapshot(&self, vm: &str, name: &str) -> String {
        let id = {
            let mut w = self.server.world.lock().unwrap();
            w.id()
        };
        self.with_vm(vm, |v, clock| {
            *clock += 1;
            create_snapshot(v, format!("snapshot-{id}"), name, "user", id, *clock)
        })
    }

    pub fn cbt_enabled(&self, vm: &str) -> bool {
        self.with_vm(vm, |v, _| v.cbt_enabled)
    }

    pub fn annotation(&self, vm: &str) -> String {
        self.with_vm(vm, |v, _| v.annotation.clone())
    }

    pub fn nics_start_connected(&self, vm: &str) -> Vec<bool> {
        self.with_vm(vm, |v, _| {
            v.nics.iter().map(|n| n.start_connected).collect()
        })
    }

    pub fn set_tools(&self, vm: &str, running: bool) {
        self.with_vm(vm, |v, _| v.tools = running)
    }

    /// CBT를 초기화한다(Storage vMotion·`ctk` 손상처럼 — 옛 changeId가 무효가 된다).
    pub fn reset_cbt(&self, vm: &str) {
        self.with_vm(vm, |v, _| {
            v.cbt_uuid = fake_uuid(u64::from_str_radix(&hex_id(6), 16).unwrap_or(7));
            for d in &mut v.disks {
                d.epochs = vec![BTreeSet::new()];
            }
        })
    }

    /// 다음 `method` 호출을 `fault` 형식 결함으로 실패시킨다(`*_Task`면 Task가 그 결함으로 끝난다).
    pub fn fail_next(&self, method: &str, fault: &str) {
        self.server
            .faults
            .lock()
            .unwrap()
            .next
            .entry(method.to_string())
            .or_default()
            .push_back(fault.to_string());
    }

    /// `method`의 다음 `skip`번 호출은 그대로 두고 그다음 한 번을 `fault` 형식 결함으로 실패시킨다(여러 걸음 중 뒤 걸음의
    /// 실패 주입 — 앞에 쌓인 [`FakeVsphere::fail_next`] 뒤에 붙는다).
    pub fn fail_later(&self, method: &str, skip: usize, fault: &str) {
        let mut f = self.server.faults.lock().unwrap();
        let q = f.next.entry(method.to_string()).or_default();
        q.extend(std::iter::repeat_n(String::new(), skip));
        q.push_back(fault.to_string());
    }

    /// 다음 `n`번의 `/folder` 범위 읽기를 본문 도중에 끊는다.
    pub fn cut_reads(&self, n: u32) {
        self.server.faults.lock().unwrap().cut_reads = n;
    }

    /// 호스트가 티켓을 거절한다(401).
    pub fn reject_tickets(&self, on: bool) {
        self.server.faults.lock().unwrap().reject_tickets = on;
    }

    /// 모든 세션을 끝낸다(다음 요청이 `NotAuthenticated`).
    pub fn expire_sessions(&self) {
        self.server.world.lock().unwrap().sessions.clear();
    }

    /// `ShutdownGuest`를 받기만 하고 끄지 않는다.
    pub fn ignore_shutdown(&self, on: bool) {
        self.server.faults.lock().unwrap().ignore_shutdown = on;
    }

    /// 무료 ESXi: 쓰는 메서드가 `RestrictedVersion` 결함
    pub fn set_restricted(&self, on: bool) {
        self.server.faults.lock().unwrap().restricted = on;
    }

    /// 응답마다 늦춘다.
    pub fn set_delay(&self, delay: Duration) {
        self.server.faults.lock().unwrap().delay = delay;
    }

    /// vSphere Task가 끝나기 전에 `running`으로 보이는 조회 수
    pub fn set_task_polls(&self, n: u32) {
        self.server.faults.lock().unwrap().task_polls = n;
    }

    /// `QueryChangedDiskAreas` 한 번이 답하는 바이트 폭(기본 1 GiB)
    pub fn set_change_window(&self, bytes: u64) {
        self.server.faults.lock().unwrap().window = Some(bytes.max(BLOCK));
    }
}

fn logical_block(layers: &[BTreeMap<u64, Vec<u8>>], top: usize, b: u64) -> Vec<u8> {
    layers[..=top]
        .iter()
        .rev()
        .find_map(|l| l.get(&b))
        .cloned()
        .unwrap_or_else(|| vec![0; BLOCK as usize])
}

fn read_layers(layers: &[BTreeMap<u64, Vec<u8>>], top: usize, offset: u64, len: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len as usize);
    let mut pos = offset;
    let end = offset + len;
    while pos < end {
        let b = pos / BLOCK;
        let inner = (pos % BLOCK) as usize;
        let n = ((BLOCK as usize) - inner).min((end - pos) as usize);
        let block = layers[..=top].iter().rev().find_map(|l| l.get(&b));
        match block {
            Some(data) => out.extend_from_slice(&data[inner..inner + n]),
            None => out.resize(out.len() + n, 0),
        }
        pos += n as u64;
    }
    out
}

fn activate_cbt(v: &mut Vm) {
    if v.cbt_active {
        return;
    }
    v.cbt_active = true;
    v.cbt_uuid = fake_uuid(u64::from_str_radix(&hex_id(6), 16).unwrap_or(3));
    for d in &mut v.disks {
        d.epochs = vec![BTreeSet::new()];
    }
}

/// 스냅샷을 만든다: 열린 CBT 구간을 닫고 새 델타 층을 연다.
fn create_snapshot(
    v: &mut Vm,
    moref: String,
    name: &str,
    description: &str,
    id: u64,
    now: u64,
) -> String {
    if v.cbt_enabled {
        activate_cbt(v);
    }
    let mut change_ids = Vec::new();
    for d in &mut v.disks {
        if v.cbt_active {
            let n = d.epochs.len();
            change_ids.push(Some(format!("{}/{n}", change_id_prefix(&v.cbt_uuid))));
            d.epochs.push(BTreeSet::new());
        } else {
            change_ids.push(None);
        }
        d.layers.push(BTreeMap::new());
    }
    v.snaps.push(Snap {
        moref: moref.clone(),
        name: name.to_string(),
        description: description.to_string(),
        id,
        created: now,
        change_ids,
    });
    moref
}

/// `52 aa bb … -cc dd …` 꼴(실제 changeId 앞부분 모양)
fn change_id_prefix(uuid: &str) -> String {
    let hex: String = uuid.chars().filter(|c| *c != '-').collect();
    let pairs: Vec<&str> = (0..16).map(|i| &hex[i * 2..i * 2 + 2]).collect();
    format!(
        "52 {} {}-{}",
        pairs[1..8].join(" "),
        pairs[8],
        pairs[9..16].join(" ")
    )
}

fn remove_snapshot(v: &mut Vm, index: usize, now: u64) {
    for d in &mut v.disks {
        let upper = d.layers.remove(index + 1);
        let lower = &mut d.layers[index];
        for (b, data) in upper {
            lower.insert(b, data);
        }
        if index == 0 {
            d.base_mtime = now;
        }
    }
    v.snaps.remove(index);
}

fn serve(
    tcp: TcpListener,
    tls: rustls::ServerConfig,
    server: Arc<Server>,
    index: usize,
) -> JoinHandle<()> {
    let listener = orbivisor_peer::tls::TlsListener::new(tcp, tls).expect("TLS listener");
    let app = axum::Router::new().fallback(move |request: Request| {
        let server = server.clone();
        async move { server.handle(index, request).await }
    });
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    })
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (k, v) = pair.trim().split_once('=')?;
            (k == name).then(|| v.trim_matches('"').to_string())
        })
}

fn xml_response(status: StatusCode, body: String, cookie: Option<String>) -> Response {
    let mut r = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/xml; charset=utf-8");
    if let Some(c) = cookie {
        r = r.header(header::SET_COOKIE, c);
    }
    r.body(Body::from(body)).unwrap()
}

fn envelope(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<soapenv:Envelope xmlns:soapenc=\"http://schemas.xmlsoap.org/soap/encoding/\" \
         xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\" \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><soapenv:Body>{body}</soapenv:Body></soapenv:Envelope>"
    )
}

fn fault_xml(kind: &str, message: &str, inner: &str) -> String {
    envelope(&format!(
        "<soapenv:Fault><faultcode>ServerFaultCode</faultcode><faultstring>{}</faultstring><detail>\
         <{kind}Fault xmlns=\"{NS}\" xsi:type=\"{kind}\">{inner}</{kind}Fault></detail></soapenv:Fault>",
        escape(message)
    ))
}

fn fault_response(kind: &str, message: &str, inner: &str) -> Response {
    xml_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        fault_xml(kind, message, inner),
        None,
    )
}

fn ok_response(method: &str, inner: &str) -> Response {
    xml_response(
        StatusCode::OK,
        envelope(&format!(
            "<{method}Response xmlns=\"{NS}\">{inner}</{method}Response>"
        )),
        None,
    )
}

fn empty(status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .unwrap()
}

/// 쓰는 메서드(무료 ESXi가 막는다)
const WRITE_METHODS: [&str; 7] = [
    "ReconfigVM_Task",
    "CreateSnapshot_Task",
    "RemoveSnapshot_Task",
    "PowerOnVM_Task",
    "PowerOffVM_Task",
    "ShutdownGuest",
    "AcquireGenericServiceTicket",
];

fn s_val(text: &str) -> String {
    format!("<val xsi:type=\"xsd:string\">{}</val>", escape(text))
}
fn b_val(v: bool) -> String {
    format!("<val xsi:type=\"xsd:boolean\">{v}</val>")
}
fn i_val(v: impl std::fmt::Display) -> String {
    format!("<val xsi:type=\"xsd:int\">{v}</val>")
}
fn l_val(v: impl std::fmt::Display) -> String {
    format!("<val xsi:type=\"xsd:long\">{v}</val>")
}
fn ref_val(kind: &str, value: &str) -> String {
    format!(
        "<val type=\"{kind}\" xsi:type=\"ManagedObjectReference\">{}</val>",
        escape(value)
    )
}
fn enum_val(ty: &str, text: &str) -> String {
    format!("<val xsi:type=\"{ty}\">{text}</val>")
}
fn mo(tag: &str, kind: &str, value: &str) -> String {
    format!("<{tag} type=\"{kind}\">{}</{tag}>", escape(value))
}

fn power_text(p: VmwarePower) -> &'static str {
    match p {
        VmwarePower::PoweredOn => "poweredOn",
        VmwarePower::PoweredOff => "poweredOff",
        VmwarePower::Suspended => "suspended",
    }
}

/// `2026-11-25T00:00:00.NNNNNNZ`(시계 값을 마이크로초로)
fn time_text(t: u64) -> String {
    format!(
        "2026-11-25T00:{:02}:{:02}.{:06}Z",
        (t / 60_000_000) % 60,
        (t / 1_000_000) % 60,
        t % 1_000_000
    )
}

/// 서브타입 포함 형식 맞음(`ComputeResource` ⊃ 클러스터, `Network` ⊃ 분산 포트 그룹·NSX)
fn is_a(kind: &str, want: &str) -> bool {
    kind == want
        || (want == "ComputeResource" && kind == "ClusterComputeResource")
        || (want == "Network" && matches!(kind, "DistributedVirtualPortgroup" | "OpaqueNetwork"))
        || (want == "ManagedEntity")
}

fn glob(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((pre, post)) => {
            name.len() >= pre.len() + post.len() && name.starts_with(pre) && name.ends_with(post)
        }
    }
}

fn percent_decode(text: &str) -> String {
    crate::vm_imports::esxi::percent_decode(text)
}

fn descriptor(stem: &str, capacity: u64) -> String {
    let flat = format!("{}-flat.vmdk", stem.rsplit('/').next().unwrap_or(stem));
    format!(
        "# Disk DescriptorFile\nversion=1\nencoding=\"UTF-8\"\nCID=fffffffe\nparentCID=ffffffff\ncreateType=\"vmfs\"\n\n\
         # Extent description\nRW {} VMFS \"{flat}\"\n\n# The Disk Data Base\n#DDB\n\nddb.adapterType = \"lsilogic\"\nddb.virtualHWVersion = \"19\"\n",
        capacity.div_ceil(512)
    )
}

impl Server {
    fn record(&self, r: FakeRequest) {
        self.requests.lock().unwrap().push(r);
    }

    async fn handle(self: Arc<Self>, server: usize, request: Request) -> Response {
        let delay = self.faults.lock().unwrap().delay;
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let (parts, body) = request.into_parts();
        let path = parts
            .uri
            .path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_default();
        let session = cookie_value(&parts.headers, "vmware_soap_session")
            .is_some_and(|s| self.world.lock().unwrap().sessions.contains(&s));
        if parts.method == axum::http::Method::POST && parts.uri.path() == "/sdk" && server == 0 {
            let Ok(bytes) = axum::body::to_bytes(body, 64 << 20).await else {
                return empty(StatusCode::BAD_REQUEST);
            };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            return self.soap(&text, session);
        }
        if parts.method == axum::http::Method::GET && parts.uri.path().starts_with("/folder") {
            let range = parts
                .headers
                .get(header::RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let ticket = cookie_value(&parts.headers, "vmware_cgi_ticket");
            self.record(FakeRequest {
                server,
                method: "GET".into(),
                path: path.clone(),
                range: range.clone(),
                session,
                ticket: ticket.clone(),
                has_password: false,
            });
            let allowed = if server == 0 {
                session
            } else {
                let reject = self.faults.lock().unwrap().reject_tickets;
                let mut w = self.world.lock().unwrap();
                match ticket.and_then(|t| w.tickets.remove(&t)) {
                    Some(url) => !reject && url.ends_with(&path),
                    None => false,
                }
            };
            if !allowed {
                return Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .header(
                        header::WWW_AUTHENTICATE,
                        "Basic realm=\"VMware HTTP server\"",
                    )
                    .body(Body::empty())
                    .unwrap();
            }
            return self.folder(&parts.uri, range.as_deref());
        }
        empty(StatusCode::NOT_FOUND)
    }

    fn folder(&self, uri: &axum::http::Uri, range: Option<&str>) -> Response {
        let file = percent_decode(
            uri.path()
                .trim_start_matches("/folder")
                .trim_start_matches('/'),
        );
        let params: BTreeMap<String, String> = uri
            .query()
            .unwrap_or("")
            .split('&')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.to_string(), percent_decode(v)))
            .collect();
        if params.get("dcPath") != Some(&self.datacenter) {
            return empty(StatusCode::NOT_FOUND);
        }
        let Some(ds) = params.get("dsName") else {
            return empty(StatusCode::NOT_FOUND);
        };
        let w = self.world.lock().unwrap();
        let found = w.vms.iter().filter(|v| &v.datastore == ds).find_map(|v| {
            v.disks.iter().find_map(|d| {
                if file == format!("{}.vmdk", d.stem) {
                    Some((Some(descriptor(&d.stem, d.capacity).into_bytes()), d))
                } else if file == format!("{}-flat.vmdk", d.stem) {
                    Some((None, d))
                } else {
                    None
                }
            })
        });
        let Some((text, disk)) = found else {
            return empty(StatusCode::NOT_FOUND);
        };
        let size = text.as_ref().map_or(disk.capacity, |t| t.len() as u64);
        let (start, end) = match range
            .and_then(|r| r.strip_prefix("bytes="))
            .and_then(|r| r.split_once('-'))
        {
            Some((a, b)) => {
                let a: u64 = a.parse().unwrap_or(0);
                let b: u64 = b
                    .parse()
                    .unwrap_or(size.saturating_sub(1))
                    .min(size.saturating_sub(1));
                if a >= size {
                    return Response::builder()
                        .status(StatusCode::RANGE_NOT_SATISFIABLE)
                        .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                        .body(Body::empty())
                        .unwrap();
                }
                (a, b)
            }
            None => (0, size.saturating_sub(1)),
        };
        let data = match &text {
            Some(t) => t[start as usize..=end as usize].to_vec(),
            None => read_layers(&disk.layers, 0, start, end - start + 1),
        };
        drop(w);
        let cut = {
            let mut f = self.faults.lock().unwrap();
            let cut = text.is_none() && f.cut_reads > 0;
            if cut {
                f.cut_reads -= 1;
            }
            cut
        };
        let builder = Response::builder()
            .status(if range.is_some() {
                StatusCode::PARTIAL_CONTENT
            } else {
                StatusCode::OK
            })
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CONTENT_LENGTH, data.len())
            .header(header::ACCEPT_RANGES, "bytes");
        let builder = if range.is_some() {
            builder.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"))
        } else {
            builder
        };
        if cut {
            let half = data[..data.len() / 2].to_vec();
            let stream = futures_util::stream::iter(vec![
                Ok::<_, std::io::Error>(axum::body::Bytes::from(half)),
                Err(std::io::Error::other("connection cut by the fake vSphere")),
            ]);
            return builder.body(Body::from_stream(stream)).unwrap();
        }
        builder.body(Body::from(data)).unwrap()
    }

    fn soap(&self, text: &str, session: bool) -> Response {
        let Ok((method, this, call)) = parse_call(text) else {
            return fault_response("InvalidRequest", "cannot parse the request", "");
        };
        let password = &self.config.password;
        self.record(FakeRequest {
            server: 0,
            method: method.clone(),
            path: "/sdk".into(),
            range: None,
            session,
            ticket: None,
            has_password: !password.is_empty() && text.contains(&escape(password)),
        });
        if !matches!(method.as_str(), "RetrieveServiceContent" | "Login") && !session {
            return fault_response(
                "NotAuthenticated",
                "",
                &format!(
                    "{}<privilegeId>System.View</privilegeId>",
                    this.as_ref().map(|t| t.xml("object")).unwrap_or_default()
                ),
            );
        }
        let (restricted, injected) = {
            let mut f = self.faults.lock().unwrap();
            // 빈 항목은 그 호출을 그대로 둔다([`FakeVsphere::fail_later`]).
            let injected = f
                .next
                .get_mut(&method)
                .and_then(|q| q.pop_front())
                .filter(|k| !k.is_empty());
            (
                f.restricted && WRITE_METHODS.contains(&method.as_str()),
                injected,
            )
        };
        if restricted {
            return fault_response(
                "RestrictedVersion",
                "Current license or ESXi version prohibits execution of the requested operation.",
                "",
            );
        }
        let this = this.unwrap_or_else(|| MoRef::new("", ""));
        if let Some(kind) = &injected
            && !method.ends_with("_Task")
        {
            return fault_response(kind, &format!("injected {kind}"), "");
        }
        match method.as_str() {
            "RetrieveServiceContent" => ok_response(&method, &self.service_content()),
            "Login" => self.login(&call),
            "Logout" => ok_response(&method, ""),
            "CreateContainerView" => {
                let kinds: Vec<String> = call
                    .children_named("type")
                    .map(|t| t.text.trim().to_string())
                    .collect();
                let mut w = self.world.lock().unwrap();
                let objects: Vec<MoRef> = self
                    .all_objects(&w)
                    .into_iter()
                    .filter(|o| kinds.iter().any(|k| is_a(&o.kind, k)))
                    .collect();
                let id = format!("session[fake]{}", hex_id(8));
                w.views.insert(id.clone(), objects);
                ok_response(&method, &mo("returnval", "ContainerView", &id))
            }
            "DestroyView" => {
                self.world.lock().unwrap().views.remove(&this.value);
                ok_response(&method, "")
            }
            "RetrievePropertiesEx" => self.retrieve(&call),
            "ContinueRetrievePropertiesEx" => {
                let token = call.string_at(&["token"]).unwrap_or_default();
                let mut w = self.world.lock().unwrap();
                match w.pages.remove(&token) {
                    Some((objects, max)) => {
                        let id = w.id();
                        ok_response(&method, &page(&mut w, objects, max, id))
                    }
                    None => fault_response(
                        "InvalidArgument",
                        "unknown token",
                        "<invalidProperty>token</invalidProperty>",
                    ),
                }
            }
            "ReconfigVM_Task"
            | "CreateSnapshot_Task"
            | "RemoveSnapshot_Task"
            | "PowerOnVM_Task"
            | "PowerOffVM_Task"
            | "SearchDatastore_Task" => self.task(&method, &this, &call, injected),
            "ShutdownGuest" => {
                let ignore = self.faults.lock().unwrap().ignore_shutdown;
                let mut w = self.world.lock().unwrap();
                let Some(v) = w.vm_mut(&this.value) else {
                    return not_found(&this);
                };
                if v.power != VmwarePower::PoweredOn {
                    return fault_response(
                        "InvalidPowerState",
                        "The attempted operation cannot be performed in the current state (Powered off).",
                        "<requestedState>poweredOn</requestedState><existingState>poweredOff</existingState>",
                    );
                }
                if !v.tools {
                    return fault_response(
                        "ToolsUnavailable",
                        "Cannot complete operation because VMware Tools is not running in this virtual machine.",
                        "",
                    );
                }
                if !ignore {
                    v.power = VmwarePower::PoweredOff;
                }
                ok_response(&method, "")
            }
            "QueryChangedDiskAreas" => self.changed_areas(&this, &call),
            "AcquireGenericServiceTicket" => {
                let url = call.string_at(&["spec", "url"]).unwrap_or_default();
                let host = url
                    .strip_prefix("https://")
                    .and_then(|u| u.split('/').next())
                    .unwrap_or_default()
                    .to_string();
                let id = format!("{}-{}", hex_id(4), hex_id(12));
                self.world.lock().unwrap().tickets.insert(id.clone(), url);
                ok_response(
                    &method,
                    &format!(
                        "<returnval><id>{id}</id><hostName>{}</hostName></returnval>",
                        escape(&host)
                    ),
                )
            }
            _ => fault_response(
                "MethodNotFound",
                &format!("{}:{} does not implement: {method}", this.kind, this.value),
                &format!("{}<method>{method}</method>", this.xml("receiver")),
            ),
        }
    }

    fn service_content(&self) -> String {
        let (name, full, api_type, product, uuid) = if self.config.esxi {
            (
                "VMware ESXi",
                "VMware ESXi 8.0.2 build-22380479 (fake)",
                "HostAgent",
                "embeddedEsx",
                String::new(),
            )
        } else {
            (
                "VMware vCenter Server",
                "VMware vCenter Server 8.0.2 build-22617221 (fake)",
                "VirtualCenter",
                "vpx",
                "<instanceUuid>1f6b2a0e-5d4c-4b3a-9e8f-7a6b5c4d3e2f</instanceUuid>".to_string(),
            )
        };
        format!(
            "<returnval>{}{}{}<about><name>{name}</name><fullName>{full}</fullName><vendor>VMware, Inc.</vendor>\
             <version>8.0.2</version><build>{}</build><localeVersion>INTL</localeVersion><localeBuild>000</localeBuild>\
             <osType>linux-x64</osType><productLineId>{product}</productLineId><apiType>{api_type}</apiType>\
             <apiVersion>8.0.2.0</apiVersion>{uuid}<licenseProductName>VMware VirtualCenter Server</licenseProductName>\
             <licenseProductVersion>8.0</licenseProductVersion></about>{}</returnval>",
            mo("rootFolder", "Folder", "group-d1"),
            mo(
                "propertyCollector",
                "PropertyCollector",
                "propertyCollector"
            ),
            mo("viewManager", "ViewManager", "ViewManager"),
            if self.config.esxi {
                "22380479"
            } else {
                "22617221"
            },
            mo("sessionManager", "SessionManager", "SessionManager"),
        )
    }

    fn login(&self, call: &Node) -> Response {
        let user = call.text_at(&["userName"]).unwrap_or_default();
        let password = call.text_at(&["password"]).unwrap_or_default();
        if user != self.config.user || password != self.config.password {
            return fault_response(
                "InvalidLogin",
                "Cannot complete login due to an incorrect user name or password.",
                "",
            );
        }
        let key = format!("52{}", hex_id(15));
        self.world.lock().unwrap().sessions.insert(key.clone());
        let body = envelope(&format!(
            "<LoginResponse xmlns=\"{NS}\"><returnval><key>{key}</key><userName>{}</userName><fullName>{}</fullName>\
             <loginTime>2026-11-25T00:00:00Z</loginTime><lastActiveTime>2026-11-25T00:00:00Z</lastActiveTime>\
             <locale>en</locale><messageLocale>en</messageLocale><extensionSession>false</extensionSession>\
             <ipAddress>127.0.0.1</ipAddress><userAgent>orbivisor</userAgent><callCount>0</callCount></returnval></LoginResponse>",
            escape(user),
            escape(user)
        ));
        xml_response(
            StatusCode::OK,
            body,
            Some(format!(
                "vmware_soap_session=\"{key}\"; Path=/; HttpOnly; Secure;"
            )),
        )
    }

    fn host_count(&self) -> usize {
        self.hosts.lock().unwrap().len()
    }

    fn all_objects(&self, w: &World) -> Vec<MoRef> {
        let mut out = vec![MoRef::new("Datacenter", "datacenter-2")];
        out.extend(w.folders.iter().map(|f| MoRef::new("Folder", &f.0)));
        if self.config.esxi {
            out.push(MoRef::new("ComputeResource", "domain-s7"));
        } else {
            out.push(MoRef::new("ClusterComputeResource", "domain-c7"));
        }
        out.extend(
            (0..self.host_count()).map(|i| MoRef::new("HostSystem", &format!("host-{}", 20 + i))),
        );
        out.extend(
            w.datastores
                .iter()
                .map(|d| MoRef::new("Datastore", &d.moref)),
        );
        out.extend(w.networks.iter().map(|n| MoRef::new(n.kind, &n.moref)));
        out.extend(w.vms.iter().map(|v| MoRef::new("VirtualMachine", &v.moref)));
        out
    }

    fn retrieve(&self, call: &Node) -> Response {
        let Some(spec) = call.child("specSet") else {
            return fault_response(
                "InvalidArgument",
                "no specSet",
                "<invalidProperty>specSet</invalidProperty>",
            );
        };
        let props: Vec<(String, Vec<String>)> = spec
            .children_named("propSet")
            .map(|p| {
                (
                    p.string_at(&["type"]).unwrap_or_default(),
                    p.children_named("pathSet")
                        .map(|s| s.text.trim().to_string())
                        .collect(),
                )
            })
            .collect();
        let max = call
            .parse_at::<usize>(&["options", "maxObjects"])
            .unwrap_or(usize::MAX)
            .max(1);
        let mut w = self.world.lock().unwrap();
        let mut objects: Vec<MoRef> = Vec::new();
        for os in spec.children_named("objectSet") {
            let Some(obj) = os.moref_at(&["obj"]) else {
                continue;
            };
            if obj.kind == "ContainerView" {
                match w.views.get(&obj.value) {
                    Some(v) => objects.extend(v.iter().cloned()),
                    None => return not_found(&obj),
                }
            } else {
                objects.push(obj);
            }
        }
        let mut rendered = Vec::new();
        for obj in &objects {
            let paths: Vec<&str> = props
                .iter()
                .filter(|(t, _)| is_a(&obj.kind, t))
                .flat_map(|(_, p)| p.iter().map(String::as_str))
                .collect();
            if paths.is_empty() {
                continue;
            }
            match self.object(&mut w, obj, &paths) {
                Some(xml) => rendered.push(xml),
                // 직접 물은 객체가 없으면 결함(vcsim·vCenter와 같음)
                None if objects.len() == 1 => return not_found(obj),
                None => {}
            }
        }
        let id = w.id();
        ok_response("RetrievePropertiesEx", &page(&mut w, rendered, max, id))
    }

    /// 객체 하나의 `<objects>` (없는 객체면 None)
    fn object(&self, w: &mut World, obj: &MoRef, paths: &[&str]) -> Option<String> {
        let mut sets = String::new();
        let mut seen = HashSet::new();
        for path in paths {
            if !seen.insert(*path) {
                continue;
            }
            if let Some(val) = self.prop(w, obj, path)? {
                let _ = write!(sets, "<propSet><name>{path}</name>{val}</propSet>");
            }
        }
        Some(format!("<objects>{}{sets}</objects>", obj.xml("obj")))
    }

    /// 속성 값(`<val …>`). 바깥 None = 객체 없음, 안쪽 None = 설정 안 된 값(뺌)
    fn prop(&self, w: &mut World, obj: &MoRef, path: &str) -> Option<Option<String>> {
        let v = |s: String| Some(Some(s));
        match obj.kind.as_str() {
            "Datacenter" => match path {
                "name" => v(s_val(&self.datacenter)),
                "parent" => v(ref_val("Folder", "group-d1")),
                _ => Some(None),
            },
            "Folder" => {
                let f = w.folders.iter().find(|f| f.0 == obj.value)?;
                match path {
                    "name" => v(s_val(&f.1)),
                    "parent" => v(ref_val(
                        if f.2.starts_with("datacenter") {
                            "Datacenter"
                        } else {
                            "Folder"
                        },
                        &f.2,
                    )),
                    "childType" => v(format!(
                        "<val xsi:type=\"ArrayOfString\">{}</val>",
                        f.3.iter()
                            .map(|t| format!("<string>{t}</string>"))
                            .collect::<String>()
                    )),
                    _ => Some(None),
                }
            }
            "ClusterComputeResource" | "ComputeResource" => match path {
                "name" => v(s_val(if self.config.esxi {
                    "localhost.localdomain"
                } else {
                    "Cluster1"
                })),
                "parent" => v(ref_val("Folder", "group-h4")),
                _ => Some(None),
            },
            "HostSystem" => {
                let i: usize = obj
                    .value
                    .strip_prefix("host-")?
                    .parse::<usize>()
                    .ok()?
                    .checked_sub(20)?;
                let (name, pem) = self.hosts.lock().unwrap().get(i)?.clone();
                match path {
                    "name" => v(s_val(&name)),
                    "parent" => v(if self.config.esxi {
                        ref_val("ComputeResource", "domain-s7")
                    } else {
                        ref_val("ClusterComputeResource", "domain-c7")
                    }),
                    "runtime.connectionState" => {
                        v(enum_val("HostSystemConnectionState", "connected"))
                    }
                    "config.certificate" => v(format!(
                        "<val xsi:type=\"ArrayOfByte\">{}</val>",
                        pem.bytes()
                            .map(|b| format!("<byte>{}</byte>", b as i8))
                            .collect::<String>()
                    )),
                    _ => Some(None),
                }
            }
            "Datastore" => {
                let d = w.datastores.iter().find(|d| d.moref == obj.value)?;
                match path {
                    "name" => v(s_val(&d.name)),
                    "parent" => v(ref_val("Folder", "group-s5")),
                    "summary.type" => v(s_val(d.kind)),
                    "summary.capacity" => v(l_val(1u64 << 40)),
                    "summary.freeSpace" => v(l_val(1u64 << 39)),
                    "browser" => v(ref_val(
                        "HostDatastoreBrowser",
                        &format!("datastoreBrowser-{}", d.moref),
                    )),
                    _ => Some(None),
                }
            }
            "Network" | "DistributedVirtualPortgroup" | "OpaqueNetwork" => {
                let n = w.networks.iter().find(|n| n.moref == obj.value)?;
                match path {
                    "name" => v(s_val(&n.name)),
                    "parent" => v(ref_val("Folder", "group-n6")),
                    "key" if n.kind == "DistributedVirtualPortgroup" => v(s_val(&n.moref)),
                    "config.uplink" if n.kind == "DistributedVirtualPortgroup" => {
                        v(b_val(n.uplink))
                    }
                    "summary.opaqueNetworkId" => Some(n.opaque_id.as_deref().map(s_val)),
                    _ => Some(None),
                }
            }
            "VirtualMachine" => {
                let vm = w.vm(&obj.value)?.clone();
                Some(self.vm_prop(w, &vm, path))
            }
            "VirtualMachineSnapshot" => {
                let (vi, si) = w.snapshot_owner(&obj.value)?;
                let vm = w.vms[vi].clone();
                match path {
                    "config.hardware.device" => v(format!(
                        "<val xsi:type=\"ArrayOfVirtualDevice\">{}{}</val>",
                        controller_xml(&vm),
                        vm.disks
                            .iter()
                            .enumerate()
                            .map(|(di, d)| disk_xml(
                                w,
                                &vm,
                                d,
                                si,
                                vm.snaps[si].change_ids[di].as_deref()
                            ))
                            .collect::<String>()
                    )),
                    "name" => v(s_val(&vm.snaps[si].name)),
                    _ => Some(None),
                }
            }
            "Task" => {
                let t = w.tasks.get_mut(&obj.value)?;
                let running = t.polls_left > 0;
                if running && path == "info.state" {
                    t.polls_left -= 1;
                }
                match path {
                    "info.state" => v(enum_val(
                        "TaskInfoState",
                        if running {
                            "running"
                        } else if t.result.is_ok() {
                            "success"
                        } else {
                            "error"
                        },
                    )),
                    "info.progress" => v(i_val(if running { 50 } else { 100 })),
                    "info.result" if !running => Some(t.result.as_ref().ok().cloned().flatten()),
                    "info.error" if !running => Some(t.result.as_ref().err().map(|(kind, message)| {
                        format!(
                            "<val xsi:type=\"LocalizedMethodFault\"><fault xsi:type=\"{kind}\"></fault><localizedMessage>{}</localizedMessage></val>",
                            escape(message)
                        )
                    })),
                    _ => Some(None),
                }
            }
            _ => None,
        }
    }

    fn vm_prop(&self, w: &World, vm: &Vm, path: &str) -> Option<String> {
        Some(match path {
            "name" => s_val(&vm.name),
            "parent" => ref_val("Folder", &vm.folder),
            "config.uuid" => s_val(&vm.uuid),
            "config.instanceUuid" => s_val(&vm.instance_uuid),
            "config.guestId" => s_val(&vm.guest_id),
            "config.guestFullName" => s_val(&vm.guest_id),
            "config.firmware" => s_val(if vm.efi { "efi" } else { "bios" }),
            "config.version" => s_val(&format!("vmx-{}", vm.hw_version)),
            "config.template" => b_val(vm.template),
            "config.changeTrackingEnabled" => b_val(vm.cbt_enabled),
            "config.hardware.numCPU" => i_val(vm.cpus),
            "config.hardware.numCoresPerSocket" => i_val(1),
            "config.hardware.memoryMB" => i_val(vm.memory_mib),
            "config.hardware.device" => {
                let top = vm.snaps.len();
                format!(
                    "<val xsi:type=\"ArrayOfVirtualDevice\">{}{}{}</val>",
                    controller_xml(vm),
                    vm.disks
                        .iter()
                        .map(|d| {
                            let change = vm.cbt_active.then(|| format!("{}/{}", change_id_prefix(&vm.cbt_uuid), d.epochs.len()));
                            disk_xml(w, vm, d, top, change.as_deref())
                        })
                        .collect::<String>(),
                    vm.nics.iter().map(|n| nic_xml(w, n)).collect::<String>()
                )
            }
            "config.annotation" if !vm.annotation.is_empty() => s_val(&vm.annotation),
            "runtime.powerState" => enum_val("VirtualMachinePowerState", power_text(vm.power)),
            "runtime.host" => ref_val("HostSystem", &format!("host-{}", 20 + vm.host)),
            "runtime.faultToleranceState" => enum_val("VirtualMachineFaultToleranceState", "notConfigured"),
            "snapshot" if !vm.snaps.is_empty() => {
                // 체인을 나무로(각 스냅샷이 다음의 부모)
                let mut inner = String::new();
                for s in vm.snaps.iter().rev() {
                    let child = std::mem::take(&mut inner);
                    let child = if child.is_empty() { child } else { format!("<childSnapshotList>{child}</childSnapshotList>") };
                    inner = format!(
                        "{}{}<name>{}</name><description>{}</description><id>{}</id><createTime>{}</createTime>\
                         <state>{}</state><quiesced>false</quiesced>{child}<replaySupported>false</replaySupported>",
                        mo("snapshot", "VirtualMachineSnapshot", &s.moref),
                        mo("vm", "VirtualMachine", &vm.moref),
                        escape(&s.name),
                        escape(&s.description),
                        s.id,
                        time_text(s.created),
                        power_text(vm.power)
                    );
                }
                format!(
                    "<val xsi:type=\"VirtualMachineSnapshotInfo\">{}<rootSnapshotList>{inner}</rootSnapshotList></val>",
                    mo("currentSnapshot", "VirtualMachineSnapshot", &vm.snaps.last().unwrap().moref)
                )
            }
            "guest.toolsRunningStatus" => s_val(if vm.tools && vm.power == VmwarePower::PoweredOn {
                "guestToolsRunning"
            } else {
                "guestToolsNotRunning"
            }),
            "guest.net" if vm.tools && vm.power == VmwarePower::PoweredOn => format!(
                "<val xsi:type=\"ArrayOfGuestNicInfo\">{}</val>",
                vm.nics
                    .iter()
                    .enumerate()
                    .map(|(i, n)| format!(
                        "<GuestNicInfo><network>{}</network><ipAddress>10.0.{}.{}</ipAddress><macAddress>{}</macAddress>\
                         <connected>true</connected><deviceConfigId>{}</deviceConfigId></GuestNicInfo>",
                        escape(&n.network),
                        i,
                        10 + (vm.moref.trim_start_matches("vm-").parse::<u32>().unwrap_or(0) % 200),
                        n.mac,
                        n.key
                    ))
                    .collect::<String>()
            ),
            "summary.storage.committed" => l_val(
                vm.disks.iter().map(|d| d.layers.iter().map(|l| l.len() as u64 * BLOCK).sum::<u64>()).sum::<u64>(),
            ),
            "resourcePool" => ref_val("ResourcePool", "resgroup-8"),
            _ => return None,
        })
    }

    fn task(&self, method: &str, this: &MoRef, call: &Node, injected: Option<String>) -> Response {
        let polls = self.faults.lock().unwrap().task_polls;
        let mut w = self.world.lock().unwrap();
        let result: Result<Option<String>, (String, String)> = match injected {
            Some(kind) => Err((kind.clone(), format!("injected {kind}"))),
            None => match self.run(&mut w, method, this, call) {
                Ok(r) => Ok(r),
                Err(RunError::NotFound) => return not_found(this),
                Err(RunError::Fault(kind, message)) => Err((kind, message)),
            },
        };
        let id = format!("task-{}", w.id());
        w.tasks.insert(
            id.clone(),
            TaskInfo {
                polls_left: polls,
                result,
            },
        );
        ok_response(method, &mo("returnval", "Task", &id))
    }

    fn run(
        &self,
        w: &mut World,
        method: &str,
        this: &MoRef,
        call: &Node,
    ) -> Result<Option<String>, RunError> {
        let now = w.tick();
        match method {
            "ReconfigVM_Task" => {
                let v = w.vm_mut(&this.value).ok_or(RunError::NotFound)?;
                let spec = call.child("spec").cloned().unwrap_or_default();
                if let Some(on) = spec.bool_at(&["changeTrackingEnabled"]) {
                    if v.hw_version < 7 {
                        return Err(RunError::Fault(
                            "InvalidArgument".into(),
                            "changed block tracking needs hardware version 7".into(),
                        ));
                    }
                    v.cbt_enabled = on;
                    if !on {
                        v.cbt_active = false;
                    }
                }
                if let Some(a) = spec.child("annotation") {
                    v.annotation = a.text.clone();
                }
                for change in spec.children_named("deviceChange") {
                    let Some(key) = change.parse_at::<i32>(&["device", "key"]) else {
                        continue;
                    };
                    let Some(nic) = v.nics.iter_mut().find(|n| n.key == key) else {
                        return Err(RunError::Fault(
                            "InvalidDeviceSpec".into(),
                            format!("no device {key}"),
                        ));
                    };
                    if let Some(c) = change.bool_at(&["device", "connectable", "startConnected"]) {
                        nic.start_connected = c;
                    }
                }
                Ok(None)
            }
            "CreateSnapshot_Task" => {
                let id = w.id();
                let v = w.vm_mut(&this.value).ok_or(RunError::NotFound)?;
                if v.template {
                    return Err(RunError::Fault(
                        "NotSupported".into(),
                        "templates cannot have snapshots".into(),
                    ));
                }
                let name = call.string_at(&["name"]).unwrap_or_default();
                let description = call.string_at(&["description"]).unwrap_or_default();
                let moref =
                    create_snapshot(v, format!("snapshot-{id}"), &name, &description, id, now);
                Ok(Some(ref_val("VirtualMachineSnapshot", &moref)))
            }
            "RemoveSnapshot_Task" => {
                let (vi, si) = w.snapshot_owner(&this.value).ok_or(RunError::NotFound)?;
                remove_snapshot(&mut w.vms[vi], si, now);
                Ok(None)
            }
            "PowerOnVM_Task" | "PowerOffVM_Task" => {
                let v = w.vm_mut(&this.value).ok_or(RunError::NotFound)?;
                let want = if method == "PowerOnVM_Task" {
                    VmwarePower::PoweredOn
                } else {
                    VmwarePower::PoweredOff
                };
                if v.power == want {
                    return Err(RunError::Fault(
                        "InvalidPowerState".into(),
                        "The attempted operation cannot be performed in the current state.".into(),
                    ));
                }
                if v.template && want == VmwarePower::PoweredOn {
                    return Err(RunError::Fault(
                        "NotSupported".into(),
                        "templates cannot be powered on".into(),
                    ));
                }
                if want == VmwarePower::PoweredOn && v.cbt_enabled {
                    activate_cbt(v);
                }
                v.power = want;
                Ok(None)
            }
            "SearchDatastore_Task" => {
                let ds_moref = this
                    .value
                    .strip_prefix("datastoreBrowser-")
                    .ok_or(RunError::NotFound)?;
                let ds = w
                    .datastores
                    .iter()
                    .find(|d| d.moref == ds_moref)
                    .ok_or(RunError::NotFound)?
                    .clone();
                let folder = call.string_at(&["datastorePath"]).unwrap_or_default();
                let dir = folder
                    .split_once(']')
                    .map(|(_, p)| p.trim().trim_end_matches('/'))
                    .unwrap_or_default()
                    .to_string();
                let pattern = call
                    .string_at(&["searchSpec", "matchPattern"])
                    .unwrap_or_else(|| "*".into());
                let mut files = String::new();
                for v in w.vms.iter().filter(|v| v.datastore == ds.name) {
                    for d in &v.disks {
                        let (d_dir, stem) = d.stem.rsplit_once('/').unwrap_or(("", &d.stem));
                        if d_dir != dir {
                            continue;
                        }
                        for (name, size, mtime) in [
                            (
                                format!("{stem}.vmdk"),
                                descriptor(&d.stem, d.capacity).len() as u64,
                                0,
                            ),
                            (format!("{stem}-flat.vmdk"), d.capacity, d.base_mtime),
                        ] {
                            if glob(&pattern, &name) {
                                let _ = write!(
                                    files,
                                    "<file xsi:type=\"FileInfo\"><path>{}</path><fileSize>{size}</fileSize><modification>{}</modification></file>",
                                    escape(&name),
                                    time_text(mtime)
                                );
                            }
                        }
                    }
                }
                Ok(Some(format!(
                    "<val xsi:type=\"HostDatastoreBrowserSearchResults\">{}<folderPath>{}</folderPath>{files}</val>",
                    mo("datastore", "Datastore", &ds.moref),
                    escape(&folder)
                )))
            }
            _ => Err(RunError::Fault("NotSupported".into(), method.to_string())),
        }
    }

    fn changed_areas(&self, this: &MoRef, call: &Node) -> Response {
        let window = self.faults.lock().unwrap().window.unwrap_or(1 << 30);
        let w = self.world.lock().unwrap();
        let Some(vm) = w.vm(&this.value) else {
            return not_found(this);
        };
        let Some(snap_ref) = call.moref_at(&["snapshot"]) else {
            return fault_response(
                "InvalidArgument",
                "snapshot is required",
                "<invalidProperty>snapshot</invalidProperty>",
            );
        };
        let Some(si) = vm.snaps.iter().position(|s| s.moref == snap_ref.value) else {
            return not_found(&snap_ref);
        };
        let key = call.parse_at::<i32>(&["deviceKey"]).unwrap_or(-1);
        let Some(di) = vm.disks.iter().position(|d| d.key == key) else {
            return fault_response(
                "InvalidArgument",
                &format!("no disk {key}"),
                "<invalidProperty>deviceKey</invalidProperty>",
            );
        };
        let disk = &vm.disks[di];
        let start = call.parse_at::<u64>(&["startOffset"]).unwrap_or(0);
        let change_id = call.string_at(&["changeId"]).unwrap_or_default();
        let file_fault = |m: &str| {
            fault_response(
                "FileFault",
                &format!(
                    "Error caused by file /vmfs/volumes/{}/{}-ctk.vmdk: {m}",
                    vm.datastore, disk.stem
                ),
                &format!(
                    "<file>/vmfs/volumes/{}/{}-ctk.vmdk</file>",
                    vm.datastore, disk.stem
                ),
            )
        };
        let Some(snap_change) = vm.snaps[si].change_ids[di].as_deref() else {
            return file_fault("changed block tracking is not enabled for this snapshot");
        };
        let (snap_prefix, snap_n) = snap_change.rsplit_once('/').unwrap();
        if snap_prefix != change_id_prefix(&vm.cbt_uuid) || !vm.cbt_active {
            return file_fault("the change tracking data was reset");
        }
        let snap_n: usize = snap_n.parse().unwrap_or(0);
        let blocks: BTreeSet<u64> = if change_id == "*" {
            disk.layers[..=si]
                .iter()
                .flat_map(|l| l.keys().copied())
                .collect()
        } else {
            let Some((prefix, n)) = change_id.rsplit_once('/') else {
                return file_fault("invalid changeId");
            };
            let n: usize = match n.parse() {
                Ok(n) if prefix == change_id_prefix(&vm.cbt_uuid) && n <= snap_n => n,
                _ => return file_fault("the changeId is no longer valid"),
            };
            disk.epochs[n..snap_n].iter().flatten().copied().collect()
        };
        let length = window.min(disk.capacity.saturating_sub(start));
        let end = start + length;
        let mut areas = String::new();
        let mut run: Option<(u64, u64)> = None;
        let flush = |run: Option<(u64, u64)>, areas: &mut String| {
            if let Some((a, b)) = run {
                let a = a.max(start);
                let b = b.min(end).min(disk.capacity);
                if b > a {
                    let _ = write!(
                        areas,
                        "<changedArea><start>{a}</start><length>{}</length></changedArea>",
                        b - a
                    );
                }
            }
        };
        for b in blocks.range(start / BLOCK..end.div_ceil(BLOCK)) {
            let (s, e) = (b * BLOCK, (b + 1) * BLOCK);
            run = match run {
                Some((rs, re)) if re == s => Some((rs, e)),
                other => {
                    flush(other, &mut areas);
                    Some((s, e))
                }
            };
        }
        flush(run, &mut areas);
        ok_response(
            "QueryChangedDiskAreas",
            &format!(
                "<returnval><startOffset>{start}</startOffset><length>{length}</length>{areas}</returnval>"
            ),
        )
    }
}

enum RunError {
    NotFound,
    Fault(String, String),
}

fn not_found(obj: &MoRef) -> Response {
    fault_response(
        "ManagedObjectNotFound",
        "The object has already been deleted or has not been completely created",
        &obj.xml("obj"),
    )
}

/// 객체들을 한 쪽(`max`개)과 토큰으로
fn page(w: &mut World, mut objects: Vec<String>, max: usize, id: u64) -> String {
    if objects.is_empty() {
        return String::new();
    }
    let rest = if objects.len() > max {
        objects.split_off(max)
    } else {
        Vec::new()
    };
    let token = if rest.is_empty() {
        String::new()
    } else {
        let t = format!("token-{id}");
        w.pages.insert(t.clone(), (rest, max));
        format!("<token>{t}</token>")
    };
    format!("<returnval>{token}{}</returnval>", objects.concat())
}

fn controller_xml(vm: &Vm) -> String {
    format!(
        "<VirtualDevice xsi:type=\"ParaVirtualSCSIController\"><key>1000</key><deviceInfo xsi:type=\"Description\">\
         <label>SCSI controller 0</label><summary>VMware paravirtual SCSI</summary></deviceInfo><busNumber>0</busNumber>{}\
         <sharedBus>noSharing</sharedBus><scsiCtlrUnitNumber>7</scsiCtlrUnitNumber></VirtualDevice>",
        vm.disks
            .iter()
            .map(|d| format!("<device>{}</device>", d.key))
            .collect::<String>()
    )
}

/// 층 `layer`(0 = 기반) 시점의 디스크 장치. 델타면 `parent` 체인을 단다.
fn disk_xml(w: &World, vm: &Vm, d: &Disk, layer: usize, change_id: Option<&str>) -> String {
    let ds_ref = w
        .datastores
        .iter()
        .find(|x| x.name == vm.datastore)
        .map(|x| x.moref.clone())
        .unwrap_or_default();
    let file = |i: usize| {
        if i == 0 {
            format!("[{}] {}.vmdk", vm.datastore, d.stem)
        } else {
            format!("[{}] {}-{i:06}.vmdk", vm.datastore, d.stem)
        }
    };
    let mut parent = String::new();
    for i in 0..layer {
        parent = format!(
            "<parent xsi:type=\"VirtualDiskFlatVer2BackingInfo\"><fileName>{}</fileName>{}<diskMode>persistent</diskMode>\
             <thinProvisioned>true</thinProvisioned>{parent}</parent>",
            escape(&file(i)),
            mo("datastore", "Datastore", &ds_ref)
        );
    }
    let unit = (d.key - 2000) as u32;
    format!(
        "<VirtualDevice xsi:type=\"VirtualDisk\"><key>{}</key><deviceInfo xsi:type=\"Description\"><label>Hard disk {}</label>\
         <summary>{} KB</summary></deviceInfo><backing xsi:type=\"VirtualDiskFlatVer2BackingInfo\"><fileName>{}</fileName>{}\
         <diskMode>persistent</diskMode><split>false</split><writeThrough>false</writeThrough><thinProvisioned>true</thinProvisioned>\
         <eagerlyScrub>false</eagerlyScrub><uuid>{}</uuid>{}{parent}<digestEnabled>false</digestEnabled></backing>\
         <controllerKey>1000</controllerKey><unitNumber>{}</unitNumber><capacityInKB>{}</capacityInKB>\
         <capacityInBytes>{}</capacityInBytes></VirtualDevice>",
        d.key,
        unit + 1,
        d.capacity / 1024,
        escape(&file(layer)),
        mo("datastore", "Datastore", &ds_ref),
        d.uuid,
        change_id
            .map(|c| format!("<changeId>{}</changeId>", escape(c)))
            .unwrap_or_default(),
        if unit >= 7 { unit + 1 } else { unit },
        d.capacity / 1024,
        d.capacity
    )
}

fn nic_xml(w: &World, n: &Nic) -> String {
    let net = w.networks.iter().find(|x| x.name == n.network);
    let backing = match net {
        Some(x) if x.kind == "DistributedVirtualPortgroup" => format!(
            "<backing xsi:type=\"VirtualEthernetCardDistributedVirtualPortBackingInfo\"><port>\
             <switchUuid>50 2a 7b 1c 9d 3e 4f 50-61 72 83 94 a5 b6 c7 d8</switchUuid><portgroupKey>{}</portgroupKey></port></backing>",
            x.moref
        ),
        Some(x) if x.kind == "OpaqueNetwork" => format!(
            "<backing xsi:type=\"VirtualEthernetCardOpaqueNetworkBackingInfo\"><opaqueNetworkId>{}</opaqueNetworkId>\
             <opaqueNetworkType>nsx.LogicalSwitch</opaqueNetworkType></backing>",
            escape(x.opaque_id.as_deref().unwrap_or_default())
        ),
        _ => format!(
            "<backing xsi:type=\"VirtualEthernetCardNetworkBackingInfo\"><deviceName>{}</deviceName><useAutoDetect>false</useAutoDetect>{}</backing>",
            escape(&n.network),
            net.map(|x| mo("network", "Network", &x.moref))
                .unwrap_or_default()
        ),
    };
    format!(
        "<VirtualDevice xsi:type=\"{}\"><key>{}</key><deviceInfo xsi:type=\"Description\"><label>Network adapter {}</label>\
         <summary>{}</summary></deviceInfo>{backing}<connectable><startConnected>{}</startConnected>\
         <allowGuestControl>true</allowGuestControl><connected>true</connected><status>ok</status></connectable>\
         <controllerKey>100</controllerKey><unitNumber>{}</unitNumber><addressType>assigned</addressType>\
         <macAddress>{}</macAddress><wakeOnLanEnabled>true</wakeOnLanEnabled></VirtualDevice>",
        n.adapter,
        n.key,
        n.key - 3999,
        escape(&n.network),
        n.start_connected,
        7 + n.key - 4000,
        n.mac
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_chain_and_consolidation_keep_the_base_at_the_latest_snapshot() {
        let mut v = Vm {
            moref: "vm-1".into(),
            name: "t".into(),
            folder: "group-v3".into(),
            host: 0,
            datastore: "ds1".into(),
            power: VmwarePower::PoweredOn,
            guest_id: "x".into(),
            efi: false,
            tools: true,
            cbt_enabled: true,
            cbt_active: false,
            cbt_uuid: fake_uuid(1),
            hw_version: 19,
            template: false,
            cpus: 1,
            memory_mib: 1,
            uuid: fake_uuid(2),
            instance_uuid: fake_uuid(3),
            annotation: String::new(),
            disks: vec![Disk {
                key: 2000,
                capacity: 4 * BLOCK,
                stem: "t/t".into(),
                layers: vec![BTreeMap::from([(0, vec![1; BLOCK as usize])])],
                epochs: vec![BTreeSet::new()],
                base_mtime: 0,
                uuid: fake_uuid(4),
            }],
            nics: Vec::new(),
            snaps: Vec::new(),
        };
        create_snapshot(&mut v, "s1".into(), "s1", "", 1, 1);
        assert!(v.cbt_active);
        assert_eq!(
            v.snaps[0].change_ids[0]
                .as_deref()
                .unwrap()
                .rsplit('/')
                .next(),
            Some("1")
        );
        // S1 뒤 쓰기는 델타로
        v.disks[0]
            .layers
            .last_mut()
            .unwrap()
            .insert(2, vec![2; BLOCK as usize]);
        v.disks[0].epochs.last_mut().unwrap().insert(2);
        create_snapshot(&mut v, "s2".into(), "s2", "", 2, 2);
        assert_eq!(v.disks[0].layers.len(), 3);
        // 기반은 아직 S1 시점
        assert!(read_layers(&v.disks[0].layers, 0, 2 * BLOCK, 1) == vec![0]);
        remove_snapshot(&mut v, 0, 3);
        // S1을 지우면 기반이 S2 시점
        assert_eq!(read_layers(&v.disks[0].layers, 0, 2 * BLOCK, 1), vec![2]);
        assert_eq!(v.snaps.len(), 1);
        assert_eq!(v.disks[0].layers.len(), 2);
        assert_eq!(v.disks[0].epochs[1], BTreeSet::from([2]));
    }

    #[test]
    fn change_ids_look_like_vsphere() {
        let p = change_id_prefix(&fake_uuid(0xabc));
        assert_eq!(
            p.len(),
            "52 1d 3f 4a 52 7b 0e 2e-6a 4f 00 44 4d 28 58 0e".len()
        );
        assert!(p.starts_with("52 "));
        assert!(glob("*-flat.vmdk", "web01-flat.vmdk"));
        assert!(!glob("*-flat.vmdk", "web01.vmdk"));
        assert!(glob("web01.vmdk", "web01.vmdk"));
    }
}
