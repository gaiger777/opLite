//! 시험용 가짜 OVSDB 서버(계획 65 §14): 메모리 표 위에서 JSON-RPC `transact`(`select`·`insert`·`update`·`mutate`·
//! `delete`·`wait`)·`echo`·`list_dbs`·`get_schema`를 답한다. 쓰는 표의 칸 종류, 강한 참조 검사(남은 참조가 있는 행을
//! 지우면 커밋 실패), 뿌리가 아닌 표의 정리(참조가 없는 `Logical_Switch_Port`는 커밋 때 사라짐), 약한 참조 비우기,
//! 이름 색인(`Logical_Switch_Port.name`·`Chassis.name`), 원소 하나인 집합을 원소로 쓰기는 실제 ovsdb-server(OVN
//! 24.03, `fixtures/ovsdb-rpc.jsonl`에 받아 둔 답)와 같게 한다. `_Server` DB의 `Database` 행은 리더 표시로 만든다.
//! 비활성 탐침(`probe`)을 켜면 조용한 연결에 `echo`를 보내고 다음 탐침까지 답이 없으면 끊는다.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

/// 칸 종류(쓰는 것만)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Str,
    Int,
    /// 문자열·정수 집합(선택 값 포함)
    Set,
    /// 문자열 맵
    Map,
    /// 다른 표의 행 참조 집합(강한·약한)
    Ref(&'static str, bool),
}

/// `table`의 칸들(없는 표는 None). 시험이 쓰는 칸만 둔다.
fn columns(table: &str) -> Option<&'static [(&'static str, Ty)]> {
    use Ty::*;
    Some(match table {
        "NB_Global" => &[
            ("name", Str),
            ("nb_cfg", Int),
            ("sb_cfg", Int),
            ("hv_cfg", Int),
            ("external_ids", Map),
            ("options", Map),
        ],
        "Logical_Switch" => &[
            ("name", Str),
            ("ports", Ref("Logical_Switch_Port", true)),
            ("acls", Set),
            ("external_ids", Map),
            ("other_config", Map),
        ],
        "Logical_Switch_Port" => &[
            ("name", Str),
            ("type", Str),
            ("addresses", Set),
            ("port_security", Set),
            ("dhcpv4_options", Ref("DHCP_Options", false)),
            ("dhcpv6_options", Ref("DHCP_Options", false)),
            ("enabled", Set),
            ("up", Set),
            ("tag", Set),
            ("options", Map),
            ("external_ids", Map),
        ],
        "DHCP_Options" => &[("cidr", Str), ("options", Map), ("external_ids", Map)],
        "SB_Global" => &[("nb_cfg", Int), ("external_ids", Map), ("options", Map)],
        "Chassis" => &[
            ("name", Str),
            ("hostname", Str),
            ("encaps", Ref("Encap", true)),
            ("external_ids", Map),
            ("other_config", Map),
        ],
        "Chassis_Private" => &[
            ("name", Str),
            ("chassis", Ref("Chassis", false)),
            ("nb_cfg", Int),
            ("external_ids", Map),
        ],
        "Encap" => &[
            ("type", Str),
            ("ip", Str),
            ("chassis_name", Str),
            ("options", Map),
        ],
        _ => return None,
    })
}

/// 참조가 없으면 커밋 때 사라지는 표
fn is_root(table: &str) -> bool {
    !matches!(table, "Logical_Switch_Port" | "Encap")
}

/// 이름 색인(표에서 하나)
fn index(table: &str) -> Option<&'static str> {
    match table {
        "Logical_Switch_Port" | "Chassis" | "Chassis_Private" => Some("name"),
        _ => None,
    }
}

fn column_ty(table: &str, column: &str) -> Option<Ty> {
    columns(table)?
        .iter()
        .find(|(c, _)| *c == column)
        .map(|(_, t)| *t)
}

fn default_value(ty: Ty) -> Value {
    match ty {
        Ty::Str => json!(""),
        Ty::Int => json!(0),
        Ty::Set | Ty::Ref(..) => json!(["set", []]),
        Ty::Map => json!(["map", []]),
    }
}

/// 칸 값을 견줄 수 있는 꼴로: 집합은 `["set", 정렬]`, 맵은 `["map", 키 정렬]`
fn normalize(ty: Ty, v: &Value) -> Value {
    match ty {
        Ty::Set | Ty::Ref(..) => {
            let mut items = super::ovsdb::set_items(v);
            items.sort_by_key(|a| a.to_string());
            items.dedup();
            json!(["set", items])
        }
        Ty::Map => {
            let mut pairs: Vec<Value> = match v.as_array().map(Vec::as_slice) {
                Some([_, Value::Array(p)]) => p.clone(),
                _ => Vec::new(),
            };
            pairs.sort_by_key(|p| p[0].to_string());
            json!(["map", pairs])
        }
        _ => v.clone(),
    }
}

/// 내보내는 꼴: 원소가 하나인 집합은 그 원소다(실제 서버와 같다).
fn output(ty: Ty, v: &Value) -> Value {
    match ty {
        Ty::Set | Ty::Ref(..) => {
            let items = super::ovsdb::set_items(v);
            if items.len() == 1 {
                items[0].clone()
            } else {
                v.clone()
            }
        }
        _ => v.clone(),
    }
}

type Row = Map<String, Value>;
type Tables = BTreeMap<String, BTreeMap<String, Row>>;

/// 가짜 서버의 상태. 시험이 [`FakeOvsdb::with`]로 보고 바꾼다.
#[derive(Debug)]
pub struct FakeState {
    /// 내는 DB 이름(`OVN_Northbound`·`OVN_Southbound`)
    pub db: String,
    pub tables: Tables,
    /// `_Server`의 `Database` 행: Raft 리더인지, 클러스터에 붙었는지
    pub leader: bool,
    pub connected: bool,
    pub cid: String,
    /// 받은 `transact`의 params(`_Server` 읽기는 빼고)
    pub transacts: Vec<Value>,
    /// 무엇이든 바꾸고 커밋된 트랜잭션 수
    pub commits: usize,
    /// 이 간격 동안 조용한 연결에 `echo`를 보낸다.
    pub probe: Option<Duration>,
    /// 받은 `echo` 답의 수
    pub echo_replies: usize,
}

impl FakeState {
    fn new(db: &str) -> Self {
        Self {
            db: db.into(),
            tables: BTreeMap::new(),
            leader: true,
            connected: true,
            cid: Uuid::new_v4().to_string(),
            transacts: Vec::new(),
            commits: 0,
            probe: None,
            echo_replies: 0,
        }
    }

    /// `table`의 행들(내보내는 꼴, `_uuid` 포함)
    pub fn rows(&self, table: &str) -> Vec<Row> {
        self.tables
            .get(table)
            .map(|t| t.iter().map(|(u, r)| project(table, u, r, None)).collect())
            .unwrap_or_default()
    }

    /// 행을 곧바로 넣는다(시험이 관리자가 만든 행 따위를 둘 때). 참조 검사는 하지 않는다.
    pub fn insert(&mut self, table: &str, row: Value) -> String {
        let uuid = Uuid::new_v4().to_string();
        let row = fill(table, row.as_object().cloned().unwrap_or_default());
        self.tables
            .entry(table.into())
            .or_default()
            .insert(uuid.clone(), row);
        uuid
    }

    /// `table`에서 `column`이 `value`인 행들을 지운다(참조 검사 없이, `ovn-nbctl lsp-del` 따위를 흉내 낼 때는
    /// 부모의 참조도 시험이 지운다).
    pub fn delete_where(&mut self, table: &str, column: &str, value: &Value) -> usize {
        let Some(t) = self.tables.get_mut(table) else {
            return 0;
        };
        let before = t.len();
        t.retain(|_, r| r.get(column) != Some(value));
        before - t.len()
    }
}

/// 빠진 칸을 기본값으로 채우고 값을 견줄 수 있는 꼴로
fn fill(table: &str, mut row: Row) -> Row {
    for (c, ty) in columns(table).unwrap_or_default() {
        let v = row.remove(*c).unwrap_or_else(|| default_value(*ty));
        row.insert((*c).into(), normalize(*ty, &v));
    }
    row
}

fn project(table: &str, uuid: &str, row: &Row, cols: Option<&[String]>) -> Row {
    let mut out = Map::new();
    let want = |c: &str| cols.is_none_or(|cols| cols.iter().any(|x| x == c));
    if want("_uuid") {
        out.insert("_uuid".into(), json!(["uuid", uuid]));
    }
    for (c, v) in row {
        if want(c) {
            let ty = column_ty(table, c).unwrap_or(Ty::Str);
            out.insert(c.clone(), output(ty, v));
        }
    }
    out
}

/// 가짜 OVSDB 서버. 놓으면 듣기와 모든 연결을 멈춘다.
pub struct FakeOvsdb {
    addr: SocketAddr,
    state: Arc<Mutex<FakeState>>,
    conns: Arc<Mutex<Vec<AbortHandle>>>,
    task: JoinHandle<()>,
}

impl Drop for FakeOvsdb {
    fn drop(&mut self) {
        self.task.abort();
        self.disconnect_all();
    }
}

impl FakeOvsdb {
    /// `127.0.0.1`의 빈 포트에서 평문 TCP로 `db`를 낸다.
    pub async fn start(db: &str) -> Self {
        Self::start_with(db, None).await
    }

    /// TLS(`config`가 클라이언트 인증서를 요구하면 mTLS)로 낸다.
    pub async fn start_tls(db: &str, config: rustls::ServerConfig) -> Self {
        Self::start_with(db, Some(tokio_rustls::TlsAcceptor::from(Arc::new(config)))).await
    }

    async fn start_with(db: &str, tls: Option<tokio_rustls::TlsAcceptor>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let state = Arc::new(Mutex::new(FakeState::new(db)));
        let conns = Arc::new(Mutex::new(Vec::new()));
        let (s, c) = (state.clone(), conns.clone());
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let s = s.clone();
                let tls = tls.clone();
                let handle = tokio::spawn(async move {
                    match tls {
                        None => serve(tcp, s).await,
                        Some(acceptor) => {
                            if let Ok(stream) = acceptor.accept(tcp).await {
                                serve(stream, s).await;
                            }
                        }
                    }
                });
                c.lock().unwrap().push(handle.abort_handle());
            }
        });
        Self {
            addr,
            state,
            conns,
            task,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 상태를 보거나 바꾼다.
    pub fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }

    /// `table`의 행들
    pub fn rows(&self, table: &str) -> Vec<Row> {
        self.with(|s| s.rows(table))
    }

    /// 열린 연결을 모두 끊는다(서버를 다시 띄운 것처럼 — 표는 남는다).
    pub fn disconnect_all(&self) {
        for c in self.conns.lock().unwrap().drain(..) {
            c.abort();
        }
    }
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(stream: S, state: Arc<Mutex<FakeState>>) {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 64 << 10];
    let mut probe_sent = false;
    loop {
        let probe = state.lock().unwrap().probe;
        let read = reader.read(&mut chunk);
        let n = match probe {
            Some(p) => match tokio::time::timeout(p, read).await {
                Ok(r) => r,
                Err(_) => {
                    // 탐침에 답이 없으면 끊는다(ovsdb-server와 같다).
                    if probe_sent {
                        return;
                    }
                    probe_sent = true;
                    let echo = json!({"id": "echo", "method": "echo", "params": []});
                    if writer.write_all(echo.to_string().as_bytes()).await.is_err() {
                        return;
                    }
                    continue;
                }
            },
            None => read.await,
        };
        let n = match n {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        probe_sent = false;
        buf.extend_from_slice(&chunk[..n]);
        let mut stream = serde_json::Deserializer::from_slice(&buf).into_iter::<Value>();
        let mut messages = Vec::new();
        let mut used = 0;
        loop {
            match stream.next() {
                Some(Ok(v)) => {
                    messages.push(v);
                    used = stream.byte_offset();
                }
                Some(Err(e)) if e.is_eof() => break,
                Some(Err(_)) => return,
                None => {
                    used = stream.byte_offset();
                    break;
                }
            }
        }
        buf.drain(..used);
        for m in messages {
            let reply = {
                let mut s = state.lock().unwrap();
                handle(&mut s, &m)
            };
            if let Some(reply) = reply
                && writer
                    .write_all(reply.to_string().as_bytes())
                    .await
                    .is_err()
            {
                return;
            }
        }
    }
}

/// 요청 하나의 답(답이 없는 메시지는 None)
fn handle(s: &mut FakeState, m: &Value) -> Option<Value> {
    let id = m.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = m.get("method").and_then(Value::as_str) else {
        // 우리 `echo`의 답
        if id == json!("echo") {
            s.echo_replies += 1;
        }
        return None;
    };
    let params = m.get("params").cloned().unwrap_or(json!([]));
    let (result, error) = match method {
        "echo" => (params, Value::Null),
        "list_dbs" => (json!([s.db, "_Server"]), Value::Null),
        "get_schema" => {
            if params[0] == json!(s.db) {
                (
                    json!({"name": s.db, "version": "fake", "tables": {}}),
                    Value::Null,
                )
            } else {
                (Value::Null, json!("unknown database"))
            }
        }
        "transact" => match transact(s, &params) {
            Ok(r) => (r, Value::Null),
            Err(e) => (Value::Null, json!(e)),
        },
        _ => (Value::Null, json!("unknown method")),
    };
    Some(json!({"id": id, "result": result, "error": error}))
}

fn op_error(error: &str, details: String) -> Value {
    json!({"error": error, "details": details})
}

fn transact(s: &mut FakeState, params: &Value) -> Result<Value, String> {
    let params = params.as_array().ok_or("params is not an array")?;
    let db = params.first().and_then(Value::as_str).unwrap_or_default();
    let ops = &params[1..];
    if db == "_Server" {
        let rows = [json!({
            "name": s.db, "model": "clustered", "connected": s.connected, "leader": s.leader,
            "cid": ["uuid", s.cid], "sid": ["uuid", Uuid::nil()], "index": 1,
        })];
        let results: Vec<Value> = ops
            .iter()
            .map(|op| {
                let matching: Vec<Value> = rows
                    .iter()
                    .filter(|r| {
                        op["where"].as_array().is_none_or(|w| {
                            w.iter().all(|c| {
                                c[1] == json!("==")
                                    && r.get(c[0].as_str().unwrap_or("")) == Some(&c[2])
                            })
                        })
                    })
                    .cloned()
                    .collect();
                json!({ "rows": matching })
            })
            .collect();
        return Ok(Value::Array(results));
    }
    if db != s.db {
        return Err("unknown database".into());
    }
    s.transacts.push(Value::Array(params.to_vec()));
    let mut tables = s.tables.clone();
    let mut named: BTreeMap<String, String> = BTreeMap::new();
    let mut results = Vec::new();
    let mut changed = false;
    for (i, op) in ops.iter().enumerate() {
        match run_op(&mut tables, &mut named, op) {
            Ok((r, wrote)) => {
                changed |= wrote;
                results.push(r);
            }
            Err(e) => {
                results.push(e);
                results.extend(std::iter::repeat_n(Value::Null, ops.len() - i - 1));
                return Ok(Value::Array(results));
            }
        }
    }
    if let Err(e) = commit(&mut tables) {
        results.push(e);
        return Ok(Value::Array(results));
    }
    if changed {
        s.tables = tables;
        s.commits += 1;
    }
    Ok(Value::Array(results))
}

/// 연산 하나: (결과, 바꾸었는지)
fn run_op(
    tables: &mut Tables,
    named: &mut BTreeMap<String, String>,
    op: &Value,
) -> Result<(Value, bool), Value> {
    let kind = op["op"].as_str().unwrap_or_default();
    if matches!(kind, "comment" | "assert") {
        return Ok((json!({}), false));
    }
    let table = op["table"].as_str().unwrap_or_default();
    if columns(table).is_none() {
        return Err(json!({
            "error": "syntax error",
            "details": format!("Parsing ovsdb operation failed: No table named {table}."),
            "syntax": op.to_string(),
        }));
    }
    let resolve = |v: &Value, named: &BTreeMap<String, String>| resolve_named(v, named);
    let conditions = op["where"].as_array().cloned().unwrap_or_default();
    let matches = |tables: &Tables| -> Result<Vec<String>, Value> {
        let mut out = Vec::new();
        for (u, r) in tables.get(table).into_iter().flatten() {
            if conditions
                .iter()
                .map(|c| condition(table, u, r, c))
                .collect::<Result<Vec<bool>, Value>>()?
                .into_iter()
                .all(|b| b)
            {
                out.push(u.clone());
            }
        }
        Ok(out)
    };
    match kind {
        "select" => {
            let cols: Option<Vec<String>> = op["columns"].as_array().map(|c| {
                c.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            });
            let t = tables.get(table);
            let rows: Vec<Value> = matches(tables)?
                .iter()
                .map(|u| Value::Object(project(table, u, &t.unwrap()[u], cols.as_deref())))
                .collect();
            Ok((json!({ "rows": rows }), false))
        }
        "insert" => {
            let row = resolve(&op["row"], named)?;
            let row = row.as_object().cloned().unwrap_or_default();
            for c in row.keys() {
                if column_ty(table, c).is_none() {
                    return Err(op_error(
                        "unknown column",
                        format!("No column {c} in table {table}."),
                    ));
                }
            }
            let uuid = Uuid::new_v4().to_string();
            if let Some(n) = op["uuid-name"].as_str() {
                named.insert(n.into(), uuid.clone());
            }
            tables
                .entry(table.into())
                .or_default()
                .insert(uuid.clone(), fill(table, row));
            Ok((json!({"uuid": ["uuid", uuid]}), true))
        }
        "update" => {
            let row = resolve(&op["row"], named)?;
            let row = row.as_object().cloned().unwrap_or_default();
            let hits = matches(tables)?;
            for u in &hits {
                let r = tables.get_mut(table).unwrap().get_mut(u).unwrap();
                for (c, v) in &row {
                    let ty = column_ty(table, c).ok_or_else(|| {
                        op_error("unknown column", format!("No column {c} in table {table}."))
                    })?;
                    r.insert(c.clone(), normalize(ty, v));
                }
            }
            Ok((json!({"count": hits.len()}), !hits.is_empty()))
        }
        "mutate" => {
            let hits = matches(tables)?;
            let mutations = resolve(&op["mutations"], named)?;
            for u in &hits {
                let r = tables.get_mut(table).unwrap().get_mut(u).unwrap();
                for m in mutations.as_array().into_iter().flatten() {
                    let c = m[0].as_str().unwrap_or_default();
                    let ty = column_ty(table, c).ok_or_else(|| {
                        op_error("unknown column", format!("No column {c} in table {table}."))
                    })?;
                    let cur = r.get(c).cloned().unwrap_or_else(|| default_value(ty));
                    r.insert(
                        c.into(),
                        mutate(ty, &cur, m[1].as_str().unwrap_or(""), &m[2])?,
                    );
                }
            }
            Ok((json!({"count": hits.len()}), !hits.is_empty()))
        }
        "delete" => {
            let hits = matches(tables)?;
            let t = tables.entry(table.into()).or_default();
            for u in &hits {
                t.remove(u);
            }
            Ok((json!({"count": hits.len()}), !hits.is_empty()))
        }
        "wait" => {
            if op["timeout"].as_u64().unwrap_or(0) != 0 {
                return Err(op_error(
                    "not supported",
                    "the fake server waits only with timeout 0".into(),
                ));
            }
            let cols: Vec<String> = op["columns"]
                .as_array()
                .map(|c| {
                    c.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let t = tables.get(table);
            let mut have: Vec<String> = matches(tables)?
                .iter()
                .map(|u| {
                    let r = &t.unwrap()[u];
                    let p: Map<String, Value> = cols
                        .iter()
                        .map(|c| (c.clone(), r.get(c).cloned().unwrap_or(Value::Null)))
                        .collect();
                    Value::Object(p).to_string()
                })
                .collect();
            let mut want: Vec<String> = op["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| {
                    let p: Map<String, Value> = cols
                        .iter()
                        .map(|c| {
                            let ty = column_ty(table, c).unwrap_or(Ty::Str);
                            (c.clone(), normalize(ty, &r[c]))
                        })
                        .collect();
                    Value::Object(p).to_string()
                })
                .collect();
            have.sort();
            want.sort();
            let equal = have == want;
            let ok = match op["until"].as_str() {
                Some("==") => equal,
                Some("!=") => !equal,
                _ => return Err(op_error("syntax error", "bad until".into())),
            };
            if ok {
                Ok((json!({}), false))
            } else {
                Err(op_error("timed out", "\"where\" clause test failed".into()))
            }
        }
        _ => Err(op_error(
            "syntax error",
            format!("unknown operation {kind}"),
        )),
    }
}

fn resolve_named(v: &Value, named: &BTreeMap<String, String>) -> Result<Value, Value> {
    Ok(match v {
        Value::Array(a) if a.len() == 2 && a[0] == json!("named-uuid") => {
            let n = a[1].as_str().unwrap_or_default();
            let u = named.get(n).ok_or_else(|| {
                op_error("syntax error", format!("named-uuid {n} is not declared"))
            })?;
            json!(["uuid", u])
        }
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| resolve_named(x, named))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| Ok((k.clone(), resolve_named(x, named)?)))
                .collect::<Result<_, Value>>()?,
        ),
        _ => v.clone(),
    })
}

fn condition(table: &str, uuid: &str, row: &Row, c: &Value) -> Result<bool, Value> {
    let column = c[0].as_str().unwrap_or_default();
    let function = c[1].as_str().unwrap_or_default();
    let (have, ty) = if column == "_uuid" {
        (json!(["uuid", uuid]), Ty::Str)
    } else {
        let ty = column_ty(table, column)
            .ok_or_else(|| op_error("syntax error", format!("No column {column}")))?;
        (
            row.get(column)
                .cloned()
                .unwrap_or_else(|| default_value(ty)),
            ty,
        )
    };
    let want = normalize(ty, &c[2]);
    let items = |v: &Value| -> Vec<Value> {
        match v.as_array().map(Vec::as_slice) {
            Some([tag, Value::Array(items)]) if tag == "set" || tag == "map" => items.clone(),
            _ => vec![v.clone()],
        }
    };
    Ok(match function {
        "==" => have == want,
        "!=" => have != want,
        "includes" => {
            let h = items(&have);
            items(&want).iter().all(|w| h.contains(w))
        }
        "excludes" => {
            let h = items(&have);
            items(&want).iter().all(|w| !h.contains(w))
        }
        _ => {
            return Err(op_error(
                "syntax error",
                format!("unknown function {function}"),
            ));
        }
    })
}

fn mutate(ty: Ty, cur: &Value, mutator: &str, arg: &Value) -> Result<Value, Value> {
    let bad = || op_error("syntax error", format!("bad mutation {mutator}"));
    match (ty, mutator) {
        (Ty::Int, "+=") => Ok(json!(
            cur.as_i64().unwrap_or(0) + arg.as_i64().ok_or_else(bad)?
        )),
        (Ty::Int, "-=") => Ok(json!(
            cur.as_i64().unwrap_or(0) - arg.as_i64().ok_or_else(bad)?
        )),
        (Ty::Set | Ty::Ref(..), "insert") => {
            let mut items = super::ovsdb::set_items(cur);
            items.extend(super::ovsdb::set_items(arg));
            Ok(normalize(ty, &json!(["set", items])))
        }
        (Ty::Set | Ty::Ref(..), "delete") => {
            let gone = super::ovsdb::set_items(arg);
            let items: Vec<Value> = super::ovsdb::set_items(cur)
                .into_iter()
                .filter(|i| !gone.contains(i))
                .collect();
            Ok(normalize(ty, &json!(["set", items])))
        }
        (Ty::Map, "insert") => {
            let mut m = super::ovsdb::parse_map(cur);
            for (k, v) in super::ovsdb::parse_map(arg) {
                m.entry(k).or_insert(v);
            }
            Ok(normalize(ty, &super::ovsdb::map_value(&m)))
        }
        (Ty::Map, "delete") => {
            let mut m = super::ovsdb::parse_map(cur);
            if arg.as_array().and_then(|a| a.first()) == Some(&json!("map")) {
                for (k, v) in super::ovsdb::parse_map(arg) {
                    if m.get(&k) == Some(&v) {
                        m.remove(&k);
                    }
                }
            } else {
                for k in super::ovsdb::parse_strings(arg) {
                    m.remove(&k);
                }
            }
            Ok(normalize(ty, &super::ovsdb::map_value(&m)))
        }
        _ => Err(bad()),
    }
}

/// 커밋 검사: 강한 참조가 가리키는 행이 있어야 하고, 약한 참조는 사라진 행을 뺀다. 참조가 없는 뿌리가 아닌 행은
/// 사라지고, 이름 색인이 겹치면 안 된다.
fn commit(tables: &mut Tables) -> Result<(), Value> {
    let names: Vec<String> = tables.keys().cloned().collect();
    // 강한 참조
    for t in &names {
        for (u, r) in &tables[t] {
            for (c, ty) in columns(t).unwrap_or_default() {
                if let Ty::Ref(target, true) = ty {
                    for target_row in super::ovsdb::parse_uuids(r.get(*c).unwrap_or(&Value::Null)) {
                        if !tables
                            .get(*target)
                            .is_some_and(|x| x.contains_key(&target_row))
                        {
                            return Err(op_error(
                                "referential integrity violation",
                                format!(
                                    "{t} row {u} column {c} refers to a missing {target} row {target_row}"
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }
    // 참조가 없는 뿌리가 아닌 행
    loop {
        let mut referenced: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for t in &names {
            for r in tables[t].values() {
                for (c, ty) in columns(t).unwrap_or_default() {
                    if let Ty::Ref(target, true) = ty {
                        referenced
                            .entry(target)
                            .or_default()
                            .extend(super::ovsdb::parse_uuids(r.get(*c).unwrap_or(&Value::Null)));
                    }
                }
            }
        }
        let mut removed = false;
        for t in names.iter().filter(|t| !is_root(t)) {
            let keep = referenced.get(t.as_str()).cloned().unwrap_or_default();
            let table = tables.get_mut(t).unwrap();
            let before = table.len();
            table.retain(|u, _| keep.contains(u));
            removed |= table.len() != before;
        }
        if !removed {
            break;
        }
    }
    // 약한 참조
    let snapshot = tables.clone();
    for t in &names {
        for r in tables.get_mut(t).unwrap().values_mut() {
            for (c, ty) in columns(t).unwrap_or_default() {
                if let Ty::Ref(target, false) = ty {
                    let kept: Vec<Value> =
                        super::ovsdb::parse_uuids(r.get(*c).unwrap_or(&Value::Null))
                            .into_iter()
                            .filter(|u| snapshot.get(*target).is_some_and(|x| x.contains_key(u)))
                            .map(|u| json!(["uuid", u]))
                            .collect();
                    r.insert((*c).into(), normalize(*ty, &json!(["set", kept])));
                }
            }
        }
    }
    // 이름 색인
    for t in &names {
        if let Some(col) = index(t) {
            let mut seen = std::collections::BTreeSet::new();
            for r in tables[t].values() {
                let v = r.get(col).cloned().unwrap_or(Value::Null).to_string();
                if !seen.insert(v.clone()) {
                    return Err(op_error(
                        "constraint violation",
                        format!(
                            "Transaction causes multiple rows in \"{t}\" table to have identical values ({v}) for index on column \"{col}\"."
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}
