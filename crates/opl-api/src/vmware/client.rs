//! vSphere 세션과 메서드(설계 §7.1·§7.2). `RetrieveServiceContent` → `Login`으로 세션 쿠키(`vmware_soap_session`)를
//! 받고, 요청마다 쿠키를 보낸다. `NotAuthenticated` 결함(세션 만료)이면 한 번 다시 로그인해 같은 요청을 보낸다.
//! 세션을 버리면 `Logout`을 띄워 보낸다(끝까지 기다리지 않음). vSphere Task는 1초에서 5초로 늘리며 기다린다.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http_body_util::Full;
use hyper::Request;
use hyper::body::Bytes;
use hyper::header::{CONTENT_TYPE, COOKIE, HeaderValue, SET_COOKIE};
use opl_types::vm_import::normalize_fingerprint;
use opl_types::vmware::{Secret, VmwareApiKind, VmwarePower, VmwareSourceSpec, VmwareSourceStatus};

use super::VsphereError;
use super::http::HttpClient;
use super::soap::{self, MoRef, Node, Reply, call_body, el, envelope};

/// SOAP 응답의 최대 크기(인벤토리 한 쪽, 설계 §7.1)
pub const MAX_RESPONSE: usize = 64 << 20;
/// 인벤토리 한 쪽의 객체 수
pub const PAGE_SIZE: usize = 500;
/// 세션 쿠키 이름
pub const SESSION_COOKIE: &str = "vmware_soap_session";

/// vSphere Task를 기다리는 한도(설계 §7.2)
pub const RECONFIG_LIMIT: Duration = Duration::from_secs(10 * 60);
pub const SNAPSHOT_CREATE_LIMIT: Duration = Duration::from_secs(60 * 60);
pub const SNAPSHOT_REMOVE_LIMIT: Duration = Duration::from_secs(4 * 60 * 60);
pub const POWER_LIMIT: Duration = Duration::from_secs(10 * 60);

/// `about`(서버 종류·판)
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct About {
    pub name: String,
    pub full_name: String,
    pub version: String,
    pub build: String,
    /// `VirtualCenter` | `HostAgent`
    pub api_type: String,
    pub api_version: String,
    pub instance_uuid: Option<String>,
    pub license_product_name: Option<String>,
}

/// 쓰는 관리 객체들(`ServiceContent`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceContent {
    pub root_folder: MoRef,
    pub property_collector: MoRef,
    pub view_manager: MoRef,
    pub session_manager: MoRef,
    pub about: About,
}

impl ServiceContent {
    /// `RetrieveServiceContentResponse` 요소에서
    pub fn parse(response: &Node) -> Result<Self, VsphereError> {
        let r = response
            .child("returnval")
            .ok_or_else(|| invalid("RetrieveServiceContent has no returnval"))?;
        let moref = |name: &str| {
            r.moref_at(&[name])
                .ok_or_else(|| invalid(format!("ServiceContent has no {name}")))
        };
        let about = r
            .child("about")
            .ok_or_else(|| invalid("ServiceContent has no about"))?;
        let s = |name: &str| about.string_at(&[name]).unwrap_or_default();
        Ok(Self {
            root_folder: moref("rootFolder")?,
            property_collector: moref("propertyCollector")?,
            view_manager: moref("viewManager")?,
            session_manager: moref("sessionManager")?,
            about: About {
                name: s("name"),
                full_name: s("fullName"),
                version: s("version"),
                build: s("build"),
                api_type: s("apiType"),
                api_version: s("apiVersion"),
                instance_uuid: about.string_at(&["instanceUuid"]),
                license_product_name: about.string_at(&["licenseProductName"]),
            },
        })
    }
}

pub(crate) fn invalid(message: impl Into<String>) -> VsphereError {
    VsphereError::Invalid(message.into())
}

/// `RetrievePropertiesEx`가 준 객체 하나(경로 → `val` 요소)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectContent {
    pub obj: MoRef,
    pub props: BTreeMap<String, Node>,
}

impl ObjectContent {
    pub fn parse(node: &Node) -> Option<Self> {
        let obj = node.moref_at(&["obj"])?;
        let props = node
            .children_named("propSet")
            .filter_map(|p| Some((p.string_at(&["name"])?, p.child("val")?.clone())))
            .collect();
        Some(Self { obj, props })
    }

    pub fn get(&self, path: &str) -> Option<&Node> {
        self.props.get(path)
    }

    pub fn text(&self, path: &str) -> Option<&str> {
        self.props.get(path).map(|n| n.text.trim())
    }

    pub fn string(&self, path: &str) -> Option<String> {
        self.text(path).map(str::to_string)
    }

    pub fn parse_num<T: std::str::FromStr>(&self, path: &str) -> Option<T> {
        self.text(path).and_then(|t| t.parse().ok())
    }

    pub fn bool(&self, path: &str) -> Option<bool> {
        match self.text(path)? {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        }
    }

    pub fn moref(&self, path: &str) -> Option<MoRef> {
        self.props.get(path).and_then(Node::moref)
    }
}

/// 속성 묶음(형식과 경로들)
#[derive(Debug, Clone, Copy)]
pub struct PropSpec<'a> {
    pub kind: &'a str,
    pub paths: &'a [&'a str],
}

fn prop_set(specs: &[PropSpec<'_>]) -> String {
    specs
        .iter()
        .map(|s| {
            let paths: String = s.paths.iter().map(|p| el("pathSet", p)).collect();
            format!("<propSet>{}{paths}</propSet>", el("type", s.kind))
        })
        .collect()
}

/// `AcquireGenericServiceTicket`의 답
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub id: String,
    pub host_name: Option<String>,
    pub ssl_thumbprint: Option<String>,
}

/// 스냅샷 시점의 디스크(스냅샷 `config.hardware.device`에서)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDisk {
    pub key: i32,
    /// 이 스냅샷 시점의 디스크 파일(델타일 수 있다)
    pub file: String,
    /// CBT `changeId`(CBT가 켜져 있을 때)
    pub change_id: Option<String>,
    pub capacity_bytes: u64,
}

/// 데이터스토어 파일 정보(`SearchDatastore_Task`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatastoreFile {
    pub path: String,
    pub size: Option<u64>,
    /// `modification`(xsd:dateTime 그대로 — 같은지만 본다)
    pub modification: Option<String>,
}

/// `QueryChangedDiskAreas`의 답 하나(`DiskChangeInfo`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskChangeInfo {
    pub start_offset: u64,
    pub length: u64,
    /// (시작, 길이)
    pub areas: Vec<(u64, u64)>,
}

impl DiskChangeInfo {
    pub fn parse(response: &Node) -> Result<Self, VsphereError> {
        let r = response
            .child("returnval")
            .ok_or_else(|| invalid("QueryChangedDiskAreas has no returnval"))?;
        let num = |n: &Node, name: &str| {
            n.parse_at::<u64>(&[name])
                .ok_or_else(|| invalid(format!("DiskChangeInfo has no {name}")))
        };
        let areas = r
            .children_named("changedArea")
            .map(|a| Ok((num(a, "start")?, num(a, "length")?)))
            .collect::<Result<_, VsphereError>>()?;
        Ok(Self {
            start_offset: num(r, "startOffset")?,
            length: num(r, "length")?,
            areas,
        })
    }
}

/// 다시 만드는 데 필요한 로그인 정보
struct Login {
    user: String,
    password: Secret,
}

/// 로그인한 vSphere 세션. 여러 Task가 `Arc`로 함께 쓴다.
pub struct Session {
    http: Arc<HttpClient>,
    login: Login,
    cookie: Mutex<Option<String>>,
    content: ServiceContent,
    action: String,
    poll_min: Duration,
    poll_max: Duration,
    page_size: AtomicUsize,
    closed: AtomicBool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("http", &self.http)
            .field("user", &self.login.user)
            .field("about", &self.content.about)
            .finish_non_exhaustive()
    }
}

/// 쿠키 헤더들에서 `name=value`(속성 뺌)
fn find_cookie(headers: &hyper::HeaderMap, name: &str) -> Option<String> {
    headers.get_all(SET_COOKIE).iter().find_map(|v| {
        let v = v.to_str().ok()?;
        let pair = v.split(';').next()?.trim();
        let (k, _) = pair.split_once('=')?;
        (k.trim() == name).then(|| pair.to_string())
    })
}

/// 세션 없이 SOAP 요청 하나(쿠키를 주면 보낸다). 응답의 세션 쿠키도 돌려준다.
async fn post(
    http: &HttpClient,
    action: &str,
    cookie: Option<&str>,
    body: String,
) -> Result<(Reply, Option<String>), VsphereError> {
    let mut request = Request::post("/sdk")
        .header(CONTENT_TYPE, "text/xml; charset=utf-8")
        .header(
            "SOAPAction",
            HeaderValue::from_str(action).map_err(|_| invalid("invalid SOAPAction"))?,
        );
    if let Some(c) = cookie {
        request = request.header(
            COOKIE,
            HeaderValue::from_str(c).map_err(|_| invalid("invalid session cookie"))?,
        );
    }
    let request = request
        .body(Full::new(Bytes::from(envelope(&body))))
        .map_err(|e| invalid(e.to_string()))?;
    let response = http.send(request, MAX_RESPONSE).await?;
    let new_cookie = find_cookie(&response.headers, SESSION_COOKIE);
    let text = String::from_utf8_lossy(&response.body);
    match soap::parse_reply(&text) {
        Ok(reply) => Ok((reply, new_cookie)),
        Err(_) if !response.status.is_success() => {
            Err(VsphereError::Http(response.status.as_u16()))
        }
        Err(e) => Err(invalid(e)),
    }
}

impl Session {
    /// 저장한 원본 연결로 로그인한다. 지문이 틀리면 비밀번호를 보내기 전에 끊긴다.
    pub async fn login(spec: &VmwareSourceSpec) -> Result<Session, VsphereError> {
        Self::login_to(
            &spec.host,
            spec.port,
            &spec.fingerprint,
            &spec.user,
            spec.password.clone(),
        )
        .await
    }

    pub async fn login_to(
        host: &str,
        port: u16,
        fingerprint: &str,
        user: &str,
        password: Secret,
    ) -> Result<Session, VsphereError> {
        // 모양이 틀린 지문은 어떤 인증서와도 맞지 않는다(그대로 두어 오류에 보인다).
        let fingerprint = normalize_fingerprint(fingerprint).unwrap_or_else(|| fingerprint.into());
        let http = Arc::new(HttpClient::new(host, port, &fingerprint));
        // 판을 모르는 첫 요청은 판 없는 urn:vim25로, 그다음은 서버가 알린 판으로 보낸다.
        let (reply, _) = post(
            &http,
            "urn:vim25",
            None,
            call_body(
                "RetrieveServiceContent",
                &MoRef::new("ServiceInstance", "ServiceInstance"),
                "",
            ),
        )
        .await?;
        let content = match reply {
            Reply::Ok(node) => ServiceContent::parse(&node)?,
            Reply::Fault(f) => return Err(VsphereError::from_fault(f)),
        };
        let action = if content.about.api_version.is_empty() {
            "urn:vim25".to_string()
        } else {
            format!("urn:vim25/{}", content.about.api_version)
        };
        let session = Session {
            http,
            login: Login {
                user: user.to_string(),
                password,
            },
            cookie: Mutex::new(None),
            content,
            action,
            poll_min: Duration::from_secs(1),
            poll_max: Duration::from_secs(5),
            page_size: AtomicUsize::new(PAGE_SIZE),
            closed: AtomicBool::new(false),
        };
        session.do_login().await?;
        Ok(session)
    }

    /// vSphere Task를 기다리는 간격(시험이 짧게 줄인다)
    pub fn with_poll(mut self, min: Duration, max: Duration) -> Self {
        self.poll_min = min;
        self.poll_max = max.max(min);
        self
    }

    /// 인벤토리 한 쪽의 객체 수(시험이 줄여 쪽 나눔을 본다)
    pub fn set_page_size(&self, n: usize) {
        self.page_size.store(n.max(1), Ordering::Relaxed);
    }

    async fn do_login(&self) -> Result<(), VsphereError> {
        let args = format!(
            "{}{}",
            el("userName", &self.login.user),
            el("password", self.login.password.expose())
        );
        let body = call_body("Login", &self.content.session_manager, &args);
        let (reply, cookie) = post(&self.http, &self.action, None, body).await?;
        if let Reply::Fault(f) = reply {
            return Err(VsphereError::from_fault(f));
        }
        let cookie = cookie.ok_or_else(|| invalid("Login did not return a session cookie"))?;
        *self.cookie.lock().unwrap_or_else(|e| e.into_inner()) = Some(cookie);
        Ok(())
    }

    /// 다시 로그인한다(세션이 끝났을 때 — `/folder` 읽기가 401을 받은 뒤 등)
    pub async fn relogin(&self) -> Result<(), VsphereError> {
        self.do_login().await
    }

    pub fn content(&self) -> &ServiceContent {
        &self.content
    }

    pub fn about(&self) -> &About {
        &self.content.about
    }

    pub fn kind(&self) -> VmwareApiKind {
        if self.content.about.api_type == "HostAgent" {
            VmwareApiKind::Esxi
        } else {
            VmwareApiKind::Vcenter
        }
    }

    /// 원본 상태 칸(만들기·"다시 확인"이 쓴다)
    pub fn source_status(&self, now: u64) -> VmwareSourceStatus {
        let a = &self.content.about;
        let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
        VmwareSourceStatus {
            kind: Some(self.kind()),
            version: some(&a.version),
            build: some(&a.build),
            instance_uuid: a.instance_uuid.clone(),
            checked_at: Some(now),
            error: None,
            restricted: false,
        }
    }

    /// 이 세션의 HTTPS 클라이언트(데이터스토어 `/folder` 읽기도 같은 연결을 쓴다)
    pub fn http(&self) -> &Arc<HttpClient> {
        &self.http
    }

    /// 지금 세션 쿠키(`vmware_soap_session=…`)
    pub fn cookie(&self) -> Option<String> {
        self.cookie
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 메서드 하나를 부른다. 세션이 끝났으면 한 번 다시 로그인한다.
    pub async fn call(&self, method: &str, this: &MoRef, args: &str) -> Result<Node, VsphereError> {
        match self.call_once(method, this, args).await {
            Err(e) if e.fault_kind() == Some("NotAuthenticated") => {
                self.do_login().await?;
                self.call_once(method, this, args).await
            }
            other => other,
        }
    }

    async fn call_once(
        &self,
        method: &str,
        this: &MoRef,
        args: &str,
    ) -> Result<Node, VsphereError> {
        let cookie = self.cookie();
        let (reply, _) = post(
            &self.http,
            &self.action,
            cookie.as_deref(),
            call_body(method, this, args),
        )
        .await?;
        match reply {
            Reply::Ok(node) => Ok(node),
            Reply::Fault(f) => Err(VsphereError::from_fault(f)),
        }
    }

    /// 로그아웃(실패해도 괜찮다)
    pub async fn logout(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self
            .call_once("Logout", &self.content.session_manager, "")
            .await;
    }

    // ---- 속성 읽기 ----

    /// 객체 하나의 속성
    pub async fn retrieve_one(
        &self,
        obj: &MoRef,
        paths: &[&str],
    ) -> Result<ObjectContent, VsphereError> {
        let set = prop_set(&[PropSpec {
            kind: &obj.kind,
            paths,
        }]);
        let args = format!(
            "<specSet>{set}<objectSet>{}<skip>false</skip></objectSet></specSet><options/>",
            obj.xml("obj")
        );
        let r = self
            .call(
                "RetrievePropertiesEx",
                &self.content.property_collector,
                &args,
            )
            .await?;
        let objects = r
            .child("returnval")
            .map(|v| {
                v.children_named("objects")
                    .filter_map(ObjectContent::parse)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        objects
            .into_iter()
            .find(|o| o.obj == *obj)
            .ok_or_else(|| VsphereError::NotFound {
                kind: "ManagedObjectNotFound".into(),
                message: format!("{obj} has no properties"),
            })
    }

    /// 컨테이너 보기 안 객체들의 속성(쪽마다 `ContinueRetrievePropertiesEx`)
    pub async fn retrieve_view(
        &self,
        view: &MoRef,
        specs: &[PropSpec<'_>],
    ) -> Result<Vec<ObjectContent>, VsphereError> {
        let args = format!(
            "<specSet>{}<objectSet>{}<skip>true</skip><selectSet xsi:type=\"TraversalSpec\">\
             <name>view</name><type>ContainerView</type><path>view</path><skip>false</skip></selectSet>\
             </objectSet></specSet><options><maxObjects>{}</maxObjects></options>",
            prop_set(specs),
            view.xml("obj"),
            self.page_size.load(Ordering::Relaxed)
        );
        let pc = &self.content.property_collector;
        let mut out = Vec::new();
        let mut r = self.call("RetrievePropertiesEx", pc, &args).await?;
        while let Some(val) = r.child("returnval") {
            out.extend(
                val.children_named("objects")
                    .filter_map(ObjectContent::parse),
            );
            let Some(token) = val.string_at(&["token"]) else {
                break;
            };
            r = self
                .call("ContinueRetrievePropertiesEx", pc, &el("token", &token))
                .await?;
        }
        Ok(out)
    }

    pub async fn create_container_view(
        &self,
        container: &MoRef,
        kinds: &[&str],
    ) -> Result<MoRef, VsphereError> {
        let types: String = kinds.iter().map(|k| el("type", k)).collect();
        let args = format!(
            "{}{types}<recursive>true</recursive>",
            container.xml("container")
        );
        let r = self
            .call("CreateContainerView", &self.content.view_manager, &args)
            .await?;
        r.moref_at(&["returnval"])
            .ok_or_else(|| invalid("CreateContainerView has no returnval"))
    }

    pub async fn destroy_view(&self, view: &MoRef) -> Result<(), VsphereError> {
        self.call("DestroyView", view, "").await.map(|_| ())
    }

    // ---- Task ----

    /// `*_Task` 메서드를 부르고 끝날 때까지 기다린다. 결과(`info.result`)를 돌려준다.
    pub async fn run_task(
        &self,
        method: &str,
        this: &MoRef,
        args: &str,
        limit: Duration,
    ) -> Result<Option<Node>, VsphereError> {
        let r = self.call(method, this, args).await?;
        let task = r
            .moref_at(&["returnval"])
            .ok_or_else(|| invalid(format!("{method} did not return a Task")))?;
        self.wait_task(&task, limit, &mut |_| {}).await
    }

    /// vSphere Task를 기다린다(1초 → 5초 간격). `progress`는 0~100을 받는다.
    pub async fn wait_task(
        &self,
        task: &MoRef,
        limit: Duration,
        progress: &mut (dyn FnMut(u32) + Send),
    ) -> Result<Option<Node>, VsphereError> {
        let started = Instant::now();
        let mut delay = self.poll_min;
        loop {
            let info = self
                .retrieve_one(
                    task,
                    &["info.state", "info.progress", "info.error", "info.result"],
                )
                .await?;
            match info.text("info.state") {
                Some("success") => return Ok(info.get("info.result").cloned()),
                Some("error") => {
                    let error = info.get("info.error");
                    let message = error
                        .and_then(|e| e.string_at(&["localizedMessage"]))
                        .unwrap_or_default();
                    return Err(match error.and_then(|e| e.child("fault")) {
                        Some(fault) => VsphereError::from_fault(soap::Fault::from_method_fault(
                            fault, &message,
                        )),
                        None => VsphereError::Fault {
                            kind: "TaskError".into(),
                            message: if message.is_empty() {
                                format!("{task} failed")
                            } else {
                                message
                            },
                        },
                    });
                }
                Some("queued" | "running") | None => {}
                Some(other) => return Err(invalid(format!("{task} has state {other}"))),
            }
            if let Some(p) = info.parse_num::<u32>("info.progress") {
                progress(p.min(100));
            }
            if started.elapsed() >= limit {
                return Err(VsphereError::Timeout(format!(
                    "vSphere task {}",
                    task.value
                )));
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(self.poll_max);
        }
    }

    // ---- VM 설정·스냅샷·CBT ----

    /// `ReconfigVM_Task(spec)` — `spec`은 `VirtualMachineConfigSpec`의 안쪽 XML
    pub async fn reconfigure(&self, vm: &MoRef, spec: &str) -> Result<(), VsphereError> {
        self.run_task(
            "ReconfigVM_Task",
            vm,
            &format!("<spec>{spec}</spec>"),
            RECONFIG_LIMIT,
        )
        .await
        .map(|_| ())
    }

    /// CBT 켜기·끄기(다음 stun — 스냅샷·전원 켜기 — 부터 적용된다)
    pub async fn set_change_tracking(&self, vm: &MoRef, on: bool) -> Result<(), VsphereError> {
        self.reconfigure(vm, &el("changeTrackingEnabled", &on.to_string()))
            .await
    }

    /// VM 메모(`annotation`)를 바꾼다.
    pub async fn set_annotation(&self, vm: &MoRef, text: &str) -> Result<(), VsphereError> {
        self.reconfigure(vm, &el("annotation", text)).await
    }

    /// NIC들의 "켤 때 연결"(`connectable.startConnected`)을 바꾼다. 장치를 읽어 그 값만 고쳐 되돌려 보낸다
    /// (vSphere는 고칠 장치 전체를 받는다). 없는 키는 건너뛰고, 고친 키를 돌려준다.
    pub async fn set_nics_start_connected(
        &self,
        vm: &MoRef,
        keys: &[i32],
        connected: bool,
    ) -> Result<Vec<i32>, VsphereError> {
        let devices = self.retrieve_one(vm, &["config.hardware.device"]).await?;
        let Some(list) = devices.get("config.hardware.device") else {
            return Ok(Vec::new());
        };
        let mut changes = String::new();
        let mut done = Vec::new();
        for device in &list.children {
            let Some(key) = device.parse_at::<i32>(&["key"]) else {
                continue;
            };
            if !keys.contains(&key) {
                continue;
            }
            let mut d = device.clone();
            d.name = "device".into();
            set_start_connected(&mut d, connected);
            changes.push_str(&format!(
                "<deviceChange><operation>edit</operation>{}</deviceChange>",
                d.to_xml()
            ));
            done.push(key);
        }
        if !done.is_empty() {
            self.reconfigure(vm, &changes).await?;
        }
        Ok(done)
    }

    /// 스냅샷을 만든다(메모리·정지 없음 — 설계 §4). 만든 스냅샷 참조를 돌려준다.
    pub async fn create_snapshot(
        &self,
        vm: &MoRef,
        name: &str,
        description: &str,
    ) -> Result<MoRef, VsphereError> {
        let args = format!(
            "{}{}<memory>false</memory><quiesce>false</quiesce>",
            el("name", name),
            el("description", description)
        );
        let result = self
            .run_task("CreateSnapshot_Task", vm, &args, SNAPSHOT_CREATE_LIMIT)
            .await?;
        result
            .as_ref()
            .and_then(Node::moref)
            .ok_or_else(|| invalid("CreateSnapshot_Task returned no snapshot"))
    }

    /// 스냅샷 하나를 지운다(자식은 두고 합침 — 기반 디스크가 다음 스냅샷 시점이 된다).
    pub async fn remove_snapshot(&self, snapshot: &MoRef) -> Result<(), VsphereError> {
        self.run_task(
            "RemoveSnapshot_Task",
            snapshot,
            "<removeChildren>false</removeChildren><consolidate>true</consolidate>",
            SNAPSHOT_REMOVE_LIMIT,
        )
        .await
        .map(|_| ())
    }

    /// 스냅샷 시점의 디스크와 CBT `changeId`
    pub async fn snapshot_disks(
        &self,
        snapshot: &MoRef,
    ) -> Result<Vec<SnapshotDisk>, VsphereError> {
        let o = self
            .retrieve_one(snapshot, &["config.hardware.device"])
            .await?;
        Ok(o.get("config.hardware.device")
            .map(|list| {
                list.children
                    .iter()
                    .filter(|d| d.ty() == Some("VirtualDisk"))
                    .filter_map(|d| {
                        Some(SnapshotDisk {
                            key: d.parse_at(&["key"])?,
                            file: d.string_at(&["backing", "fileName"])?,
                            change_id: d.string_at(&["backing", "changeId"]),
                            capacity_bytes: disk_capacity(d),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// `QueryChangedDiskAreas` 한 번(이어 부르기는 [`super::cbt::changed_areas`])
    pub async fn query_changed_disk_areas(
        &self,
        vm: &MoRef,
        snapshot: &MoRef,
        device_key: i32,
        start_offset: u64,
        change_id: &str,
    ) -> Result<DiskChangeInfo, VsphereError> {
        let args = format!(
            "{}<deviceKey>{device_key}</deviceKey><startOffset>{start_offset}</startOffset>{}",
            snapshot.xml("snapshot"),
            el("changeId", change_id)
        );
        let r = self.call("QueryChangedDiskAreas", vm, &args).await?;
        DiskChangeInfo::parse(&r)
    }

    // ---- 전원 ----

    pub async fn power_state(&self, vm: &MoRef) -> Result<VmwarePower, VsphereError> {
        let o = self.retrieve_one(vm, &["runtime.powerState"]).await?;
        parse_power(o.text("runtime.powerState").unwrap_or_default())
            .ok_or_else(|| invalid(format!("{vm} has no power state")))
    }

    /// 게스트 종료(Tools가 필요하고 곧 돌아온다 — 꺼졌는지는 [`Session::power_state`]로 본다)
    pub async fn shutdown_guest(&self, vm: &MoRef) -> Result<(), VsphereError> {
        self.call("ShutdownGuest", vm, "").await.map(|_| ())
    }

    pub async fn power_off(&self, vm: &MoRef) -> Result<(), VsphereError> {
        self.run_task("PowerOffVM_Task", vm, "", POWER_LIMIT)
            .await
            .map(|_| ())
    }

    pub async fn power_on(&self, vm: &MoRef) -> Result<(), VsphereError> {
        self.run_task("PowerOnVM_Task", vm, "", POWER_LIMIT)
            .await
            .map(|_| ())
    }

    // ---- 티켓·데이터스토어 ----

    /// `url`(호스트의 `/folder/…`)에 한 번 쓸 GET 티켓(쿠키 `vmware_cgi_ticket`)
    pub async fn acquire_ticket(&self, url: &str) -> Result<Ticket, VsphereError> {
        let args = format!(
            "<spec xsi:type=\"SessionManagerHttpServiceRequestSpec\"><method>httpGet</method>{}</spec>",
            el("url", url)
        );
        let r = self
            .call(
                "AcquireGenericServiceTicket",
                &self.content.session_manager,
                &args,
            )
            .await?;
        let v = r
            .child("returnval")
            .ok_or_else(|| invalid("AcquireGenericServiceTicket has no returnval"))?;
        Ok(Ticket {
            id: v
                .string_at(&["id"])
                .ok_or_else(|| invalid("the service ticket has no id"))?,
            host_name: v.string_at(&["hostName"]),
            ssl_thumbprint: v.string_at(&["sslThumbprint"]),
        })
    }

    /// 데이터스토어 폴더(`[ds] dir`)에서 `pattern`에 맞는 파일의 크기·수정 시각
    pub async fn search_datastore(
        &self,
        browser: &MoRef,
        folder: &str,
        pattern: &str,
    ) -> Result<Vec<DatastoreFile>, VsphereError> {
        let args = format!(
            "{}<searchSpec><details><fileType>true</fileType><fileSize>true</fileSize>\
             <modification>true</modification><fileOwner>false</fileOwner></details>{}</searchSpec>",
            el("datastorePath", folder),
            el("matchPattern", pattern)
        );
        let result = self
            .run_task("SearchDatastore_Task", browser, &args, RECONFIG_LIMIT)
            .await?;
        Ok(result
            .map(|r| {
                r.children_named("file")
                    .filter_map(|f| {
                        Some(DatastoreFile {
                            path: f.string_at(&["path"])?,
                            size: f.parse_at(&["fileSize"]),
                            modification: f.string_at(&["modification"]),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(cookie) = self.cookie() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let http = self.http.clone();
        let action = self.action.clone();
        let body = call_body("Logout", &self.content.session_manager, "");
        handle.spawn(async move {
            let _ = post(&http, &action, Some(&cookie), body).await;
        });
    }
}

/// 장치의 `connectable.startConnected`를 넣거나 바꾼다.
fn set_start_connected(device: &mut Node, connected: bool) {
    let text = connected.to_string();
    if let Some(n) = device.get_mut(&["connectable", "startConnected"]) {
        n.text = text;
        return;
    }
    let leaf = Node {
        name: "startConnected".into(),
        text,
        ..Node::default()
    };
    match device.get_mut(&["connectable"]) {
        Some(c) => c.children.insert(0, leaf),
        None => device.children.push(Node {
            name: "connectable".into(),
            children: vec![
                leaf,
                Node {
                    name: "allowGuestControl".into(),
                    text: "true".into(),
                    ..Node::default()
                },
                Node {
                    name: "connected".into(),
                    text: "false".into(),
                    ..Node::default()
                },
            ],
            ..Node::default()
        }),
    }
}

/// 디스크 장치의 크기(`capacityInBytes`, 옛 서버는 `capacityInKB`)
pub fn disk_capacity(disk: &Node) -> u64 {
    disk.parse_at::<u64>(&["capacityInBytes"])
        .filter(|b| *b > 0)
        .or_else(|| disk.parse_at::<u64>(&["capacityInKB"]).map(|kb| kb * 1024))
        .unwrap_or(0)
}

pub fn parse_power(text: &str) -> Option<VmwarePower> {
    match text {
        "poweredOn" => Some(VmwarePower::PoweredOn),
        "poweredOff" => Some(VmwarePower::PoweredOff),
        "suspended" => Some(VmwarePower::Suspended),
        _ => None,
    }
}

/// `[ds] dir/file`를 (데이터스토어, 경로)로
pub fn split_datastore_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.trim().strip_prefix('[')?;
    let (ds, file) = rest.split_once(']')?;
    Some((ds, file.trim_start()))
}

/// `[ds] path`
pub fn datastore_path(datastore: &str, path: &str) -> String {
    format!("[{datastore}] {path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_service_content_from_vcsim() {
        let xml = include_str!("../../tests/fixtures/vsphere/retrieve-service-content.xml");
        let Reply::Ok(node) = soap::parse_reply(xml).unwrap() else {
            panic!("fault")
        };
        let sc = ServiceContent::parse(&node).unwrap();
        assert_eq!(sc.root_folder, MoRef::new("Folder", "group-d1"));
        assert_eq!(
            sc.session_manager,
            MoRef::new("SessionManager", "SessionManager")
        );
        assert_eq!(sc.about.api_type, "VirtualCenter");
        assert_eq!(sc.about.api_version, "6.5");
        assert_eq!(
            sc.about.instance_uuid.as_deref(),
            Some("b1d05f88-e13d-558d-93e2-afc536a0f88a")
        );
    }

    #[test]
    fn parses_esxi_service_content() {
        let xml = include_str!("../../tests/fixtures/vsphere/esxi-service-content.xml");
        let Reply::Ok(node) = soap::parse_reply(xml).unwrap() else {
            panic!("fault")
        };
        let sc = ServiceContent::parse(&node).unwrap();
        assert_eq!(sc.about.api_type, "HostAgent");
        assert_eq!(sc.about.version, "8.0.2");
        assert_eq!(sc.about.instance_uuid, None);
    }

    #[test]
    fn parses_disk_change_info() {
        let xml = include_str!("../../tests/fixtures/vsphere/query-changed-disk-areas.xml");
        let Reply::Ok(node) = soap::parse_reply(xml).unwrap() else {
            panic!("fault")
        };
        let info = DiskChangeInfo::parse(&node).unwrap();
        assert_eq!(info.start_offset, 0);
        assert_eq!(info.length, 10737418240);
        assert_eq!(info.areas, vec![(0, 65536), (1048576, 2097152)]);
    }

    #[test]
    fn finds_the_session_cookie() {
        let mut h = hyper::HeaderMap::new();
        h.append(SET_COOKIE, HeaderValue::from_static("other=1; Path=/"));
        h.append(
            SET_COOKIE,
            HeaderValue::from_static(
                "vmware_soap_session=\"52a1b2c3d4\"; Path=/; HttpOnly; Secure;",
            ),
        );
        assert_eq!(
            find_cookie(&h, SESSION_COOKIE).as_deref(),
            Some("vmware_soap_session=\"52a1b2c3d4\"")
        );
        assert_eq!(find_cookie(&h, "nothing"), None);
    }

    #[test]
    fn start_connected_is_set_or_added() {
        let mut d = Node {
            name: "device".into(),
            xsi_type: Some("VirtualVmxnet3".into()),
            ..Node::default()
        };
        set_start_connected(&mut d, false);
        assert_eq!(d.bool_at(&["connectable", "startConnected"]), Some(false));
        set_start_connected(&mut d, true);
        assert_eq!(d.bool_at(&["connectable", "startConnected"]), Some(true));
        assert!(d.to_xml().starts_with(
            "<device xsi:type=\"VirtualVmxnet3\"><connectable><startConnected>true</startConnected>"
        ));
    }

    #[test]
    fn datastore_paths_split() {
        assert_eq!(
            split_datastore_path("[ds 1] web/web.vmdk"),
            Some(("ds 1", "web/web.vmdk"))
        );
        assert_eq!(split_datastore_path("web.vmdk"), None);
        assert_eq!(datastore_path("ds", "a/b"), "[ds] a/b");
        assert_eq!(parse_power("poweredOn"), Some(VmwarePower::PoweredOn));
        assert_eq!(parse_power("x"), None);
    }
}
