//! `orbivisorctl folder …`(인벤토리 폴더·할당량), `role …`(사용자 정의 역할), `group …`(로컬 그룹과 외부 그룹
//! 연결), `grant …`(권한 주기), `perm …`(유효 권한), `token create --scope`의 범위. 계획 62 설계 §5·§6·§9·§13.
//!
//! API는 폴더·사용자·그룹·역할을 id로 받는다. 이 모듈이 이름과 경로(`/teamA/prod`)를 id로 바꾼다. 폴더 인자는
//! `[KIND:]PATH`(`vm:/teamA`, `/`는 루트)나 폴더 id이고, 보는 사람이 부모를 볼 수 없는 폴더는 그 사람에게 맨 위
//! 폴더다(서버의 목록과 같다).

use std::collections::{BTreeMap, HashSet};
use std::io::Write;

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Subcommand, ValueEnum};
use serde_json::{Map, Value, json};

use crate::cli::{items, parse_size};
use crate::client::{Client, seg};
use crate::output::{bytes, cell, json as print_json, table};

// ---- 폴더 찾기 ----

/// 폴더 트리 셋의 API 이름
const TREES: [&str; 3] = ["vm", "network", "storage"];

/// 폴더 트리
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Tree {
    /// VM과 템플릿
    Vm,
    Network,
    /// 스토리지 풀
    Storage,
}

impl Tree {
    pub(crate) fn api(self) -> &'static str {
        match self {
            Tree::Vm => "vm",
            Tree::Network => "network",
            Tree::Storage => "storage",
        }
    }
}

/// `vm:/teamA`의 트리 접두사를 뗀다.
fn split_tree(s: &str) -> (Option<&'static str>, &str) {
    for t in TREES {
        if let Some(rest) = s.strip_prefix(t).and_then(|r| r.strip_prefix(':')) {
            return (Some(t), rest);
        }
    }
    (None, s)
}

/// 볼 수 있는 폴더 목록(`GET /folders`)
pub(crate) struct Folders(Vec<Value>);

impl Folders {
    pub(crate) async fn load(client: &Client) -> anyhow::Result<Self> {
        Ok(Self(items(&client.get("/folders").await?)))
    }

    fn by_id(&self, id: &str) -> Option<&Value> {
        self.0.iter().find(|f| f["id"] == id)
    }

    /// 보이는 부모. 부모가 없거나 볼 수 없으면 None(맨 위 폴더)
    fn parent(&self, f: &Value) -> Option<&Value> {
        f["spec"]["parent"].as_str().and_then(|p| self.by_id(p))
    }

    /// 폴더의 경로(`/teamA/prod`). 모르는 id는 그대로 쓴다.
    pub(crate) fn path(&self, id: &str) -> String {
        let Some(mut f) = self.by_id(id) else {
            return id.to_string();
        };
        let mut parts = vec![cell(&f["spec"]["name"])];
        while let Some(p) = self.parent(f) {
            if parts.len() > opl_types::folder::MAX_FOLDER_DEPTH {
                break;
            }
            parts.push(cell(&p["spec"]["name"]));
            f = p;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    /// 트리 `tree`에서 `parent`(None = 맨 위)의 자식
    fn children<'a>(
        &'a self,
        tree: &'a str,
        parent: Option<&'a str>,
    ) -> impl Iterator<Item = &'a Value> + 'a {
        self.0.iter().filter(move |f| {
            f["spec"]["kind"] == tree && self.parent(f).and_then(|p| p["id"].as_str()) == parent
        })
    }

    /// `[KIND:]PATH|ID`를 (트리, 폴더 id)로 바꾼다. 루트(`/`)는 id가 None이다. `tree`는 명령이 정한 트리이고
    /// 없으면 모든 트리에서 하나만 맞아야 한다(루트는 트리가 정해져야 한다).
    pub(crate) fn resolve(
        &self,
        tree: Option<&'static str>,
        s: &str,
    ) -> anyhow::Result<(&'static str, Option<String>)> {
        let (prefix, rest) = split_tree(s);
        let tree = match (prefix, tree) {
            (Some(p), Some(t)) if p != t => bail!("{s} is a {p} folder; give a {t} folder"),
            (p, t) => p.or(t),
        };
        if let Ok(id) = uuid::Uuid::try_parse(rest) {
            let f = self
                .by_id(&id.to_string())
                .with_context(|| format!("folder {s} not found"))?;
            let kind = TREES
                .into_iter()
                .find(|t| f["spec"]["kind"] == *t)
                .context("unknown folder kind")?;
            if tree.is_some_and(|t| t != kind) {
                bail!("{s} is a {kind} folder; give a {} folder", tree.unwrap());
            }
            return Ok((kind, Some(id.to_string())));
        }
        if !rest.starts_with('/') {
            bail!("folder {s:?} must be a path like /teamA/prod (or vm:/teamA) or a folder id");
        }
        let names = opl_types::folder::parse_folder_path(rest)
            .map_err(|e| anyhow::anyhow!("bad folder path {s:?}: {e}"))?;
        let trees: Vec<&'static str> = match tree {
            Some(t) => vec![t],
            None => TREES.to_vec(),
        };
        let mut found: Vec<(&'static str, Option<String>)> = Vec::new();
        for t in trees {
            let mut at: Option<String> = None;
            let mut ok = true;
            for name in &names {
                let lower = name.to_lowercase();
                let next = self
                    .children(t, at.as_deref())
                    .find(|f| cell(&f["spec"]["name"]).to_lowercase() == lower);
                match next {
                    Some(f) => at = Some(cell(&f["id"])),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                found.push((t, at));
            }
        }
        match found.len() {
            0 => bail!("folder {s} not found"),
            1 if !names.is_empty() || tree.is_some() => Ok(found.remove(0)),
            _ if names.is_empty() => {
                bail!("give the tree of the root: vm:/, network:/ or storage:/")
            }
            _ => bail!(
                "folder {s} is in more than one tree; give vm:{rest}, network:{rest} or storage:{rest}"
            ),
        }
    }

    /// 권한 대상의 폴더: id는 어느 트리든, 경로는 접두사가 없으면 VM 트리(`network:/lan`·`storage:/fast`는 접두사로)
    fn resolve_target(&self, s: &str) -> anyhow::Result<(&'static str, Option<String>)> {
        let (prefix, rest) = split_tree(s);
        if uuid::Uuid::try_parse(rest).is_ok() {
            return self.resolve(prefix, s);
        }
        self.resolve(Some(prefix.unwrap_or("vm")), s)
    }

    /// `root`의 하위 폴더(자기 포함). `root`가 None(트리의 루트)이면 그 트리의 모든 폴더와 None
    fn subtree(&self, tree: &str, root: Option<&str>) -> HashSet<Option<String>> {
        let mut out: HashSet<Option<String>> = HashSet::new();
        out.insert(root.map(str::to_string));
        let mut stack: Vec<Option<String>> = vec![root.map(str::to_string)];
        while let Some(at) = stack.pop() {
            for c in self.children(tree, at.as_deref()) {
                let id = Some(cell(&c["id"]));
                if out.insert(id.clone()) {
                    stack.push(id);
                }
            }
        }
        out
    }
}

/// 폴더 인자 하나를 id로(루트는 None). 트리는 명령이 정한다.
pub(crate) async fn resolve_folder(
    client: &Client,
    tree: &'static str,
    s: &str,
) -> anyhow::Result<Option<String>> {
    Ok(Folders::load(client).await?.resolve(Some(tree), s)?.1)
}

/// 목록의 `--folder` 거르기
#[derive(Debug, Clone, Default, Args)]
pub(crate) struct FolderFilter {
    /// 이 폴더에 바로 든 것만(경로 `/teamA`, `/`는 루트, 또는 폴더 id)
    #[arg(long)]
    pub folder: Option<String>,
    /// `--folder`의 하위 폴더에 든 것도
    #[arg(long, requires = "folder")]
    pub recursive: bool,
}

impl FolderFilter {
    /// 목록(`{items: [...]}`)에서 고른 폴더(와 하위)에 든 것만 남긴다.
    pub(crate) async fn apply(
        &self,
        client: &Client,
        tree: &'static str,
        mut list: Value,
    ) -> anyhow::Result<Value> {
        let Some(folder) = &self.folder else {
            return Ok(list);
        };
        let folders = Folders::load(client).await?;
        let (_, id) = folders.resolve(Some(tree), folder)?;
        let keep = if self.recursive {
            folders.subtree(tree, id.as_deref())
        } else {
            HashSet::from([id])
        };
        if let Some(all) = list["items"].as_array_mut() {
            all.retain(|r| keep.contains(&r["spec"]["folder"].as_str().map(str::to_string)));
        }
        Ok(list)
    }
}

// ---- 폴더 명령 ----

/// 할당량 인자. 주지 않은 차원은 그대로, `none`은 그 차원을 지운다.
#[derive(Debug, Clone, Default, Args)]
pub(crate) struct QuotaArgs {
    /// vCPU 수 상한(`none`은 지우기)
    #[arg(long, value_name = "N|none")]
    vcpus: Option<String>,
    /// 메모리 상한(`128G`, 단위가 없으면 MiB, `none`은 지우기)
    #[arg(long, value_name = "SIZE|none")]
    memory: Option<String>,
    /// 붙은 디스크 크기 합의 상한(`2T`, 단위가 없으면 바이트, `none`은 지우기)
    #[arg(long, value_name = "SIZE|none")]
    disk: Option<String>,
    /// VM 수 상한(템플릿은 세지 않는다, `none`은 지우기)
    #[arg(long, value_name = "N|none")]
    vms: Option<String>,
    /// 전용 CPU VM의 vCPU 수 상한(핫플러그 상한까지, 에뮬레이터 `isolate`는 하나 더, `none`은 지우기)
    #[arg(long, value_name = "N|none")]
    dedicated_vcpus: Option<String>,
}

impl QuotaArgs {
    fn is_empty(&self) -> bool {
        self.vcpus.is_none()
            && self.memory.is_none()
            && self.disk.is_none()
            && self.vms.is_none()
            && self.dedicated_vcpus.is_none()
    }

    /// 지금 할당량(`null`이면 없음)에 준 차원을 덮은 새 할당량
    fn apply(&self, current: &Value) -> anyhow::Result<Value> {
        let mut q: Map<String, Value> = current.as_object().cloned().unwrap_or_default();
        let dims: [(&str, &Option<String>, ParseFn); 5] = [
            ("vcpus", &self.vcpus, parse_count),
            ("memory_mib", &self.memory, parse_mib),
            ("disk_bytes", &self.disk, parse_size),
            ("vms", &self.vms, parse_count),
            ("dedicated_vcpus", &self.dedicated_vcpus, parse_count),
        ];
        for (key, value, parse) in dims {
            match value.as_deref() {
                None => {}
                Some("none") => {
                    q.remove(key);
                }
                Some(v) => {
                    q.insert(key.into(), json!(parse(v).map_err(anyhow::Error::msg)?));
                }
            }
        }
        Ok(Value::Object(q))
    }
}

/// 할당량 값 하나를 읽는 함수
type ParseFn = fn(&str) -> Result<u64, String>;

fn parse_count(s: &str) -> Result<u64, String> {
    s.trim()
        .parse()
        .map_err(|_| format!("{s:?} is not a number"))
}

/// 메모리 크기를 MiB로(단위가 없으면 MiB)
fn parse_mib(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.bytes().all(|b| b.is_ascii_digit()) {
        return parse_count(s);
    }
    let b = parse_size(s)?;
    if b % (1 << 20) != 0 {
        return Err(format!("{s:?} is not a whole number of MiB"));
    }
    Ok(b >> 20)
}

/// 할당량을 사람이 읽게(`32 vCPUs, 128.0GiB memory, 2.0TiB disk, 20 VMs`). 없으면 `-`
fn quota_text(q: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(n) = q["vcpus"].as_u64() {
        parts.push(format!("{n} vCPUs"));
    }
    if let Some(n) = q["memory_mib"].as_u64() {
        parts.push(format!("{} memory", bytes(&json!(n << 20))));
    }
    if q["disk_bytes"].is_u64() {
        parts.push(format!("{} disk", bytes(&q["disk_bytes"])));
    }
    if let Some(n) = q["vms"].as_u64() {
        parts.push(format!("{n} VMs"));
    }
    if let Some(n) = q["dedicated_vcpus"].as_u64() {
        parts.push(format!("{n} dedicated vCPUs"));
    }
    if parts.is_empty() {
        "-".into()
    } else {
        parts.join(", ")
    }
}

#[derive(Subcommand)]
pub(crate) enum FolderCmd {
    /// 볼 수 있는 폴더(경로·트리·id·할당량)
    List {
        #[arg(long, value_enum)]
        kind: Option<Tree>,
    },
    /// 폴더를 트리로 보인다.
    Tree {
        #[arg(long, value_enum)]
        kind: Option<Tree>,
    },
    Get {
        folder: String,
        #[arg(long, value_enum, default_value = "vm")]
        kind: Tree,
    },
    /// 폴더를 만든다(`/teamA/prod`면 `/teamA` 아래 `prod`). 부모의 `inventory.folder`가 필요하다.
    Create {
        path: String,
        #[arg(long, value_enum, default_value = "vm")]
        kind: Tree,
        #[command(flatten)]
        quota: QuotaArgs,
    },
    /// 폴더 이름을 바꾼다.
    Rename {
        folder: String,
        name: String,
        #[arg(long, value_enum, default_value = "vm")]
        kind: Tree,
    },
    /// 폴더를 다른 부모 아래로 옮긴다(`--to /`는 트리의 맨 위).
    Move {
        folder: String,
        #[arg(long)]
        to: String,
        #[arg(long, value_enum, default_value = "vm")]
        kind: Tree,
    },
    /// 빈 폴더를 지운다(자식 폴더나 리소스가 있으면 409 `folder_not_empty`).
    Delete {
        folder: String,
        #[arg(long, value_enum, default_value = "vm")]
        kind: Tree,
    },
    /// 리소스를 폴더로 옮기기
    Members {
        #[command(subcommand)]
        cmd: MembersCmd,
    },
    /// 폴더 할당량(VM 폴더만). 부모의 `inventory.quota`가 필요하다.
    Quota {
        #[command(subcommand)]
        cmd: QuotaCmd,
    },
    /// 하위 트리 사용량과 이 폴더를 묶는 할당량
    Usage { folder: String },
}

#[derive(Subcommand)]
pub(crate) enum MembersCmd {
    /// 리소스를 폴더 `DEST`로 옮긴다(`/`는 트리의 맨 위). 항목은 `vm/NAME`·`network/NAME`·`storage-pool/NAME`
    /// (`pool/NAME`)이고 모두 같은 트리여야 한다. 항목마다 결과를 보인다.
    Move {
        dest: String,
        #[arg(required = true, value_name = "KIND/NAME")]
        items: Vec<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum QuotaCmd {
    /// 준 차원만 바꾼다(`none`은 그 차원을 지운다). 사용량보다 낮게 둘 수 있다(늘리는 쓰기만 막는다).
    Set {
        folder: String,
        #[command(flatten)]
        quota: QuotaArgs,
    },
    /// 할당량을 지운다.
    Clear { folder: String },
}

/// 리소스 항목 `vm/web`의 (API 종류, 트리, 이름)
fn member_item(s: &str) -> anyhow::Result<(&'static str, &'static str, &str)> {
    let (kind, key) = s.split_once('/').with_context(|| {
        format!("expected KIND/NAME (vm/web, network/lan, storage-pool/fast), got {s:?}")
    })?;
    let (api, tree) = match kind {
        "vm" => ("vm", "vm"),
        "network" => ("network", "network"),
        "storage-pool" | "pool" => ("storage-pool", "storage"),
        _ => bail!("unknown kind {kind:?} in {s:?}; use vm, network or storage-pool"),
    };
    if key.is_empty() {
        bail!("no name in {s:?}");
    }
    Ok((api, tree, key))
}

fn folder_row(folders: &Folders, f: &Value) -> Vec<String> {
    vec![
        folders.path(&cell(&f["id"])),
        cell(&f["spec"]["kind"]),
        cell(&f["id"]),
        quota_text(&f["spec"]["quota"]),
    ]
}

/// 트리를 들여 쓴 줄로
fn tree_lines(
    folders: &Folders,
    tree: &str,
    parent: Option<&str>,
    depth: usize,
    lines: &mut Vec<String>,
) {
    let mut kids: Vec<&Value> = folders.children(tree, parent).collect();
    kids.sort_by_key(|f| cell(&f["spec"]["name"]).to_lowercase());
    for f in kids {
        let quota = quota_text(&f["spec"]["quota"]);
        let mut line = format!("{}{}", "  ".repeat(depth), cell(&f["spec"]["name"]));
        if quota != "-" {
            line.push_str(&format!(" (quota: {quota})"));
        }
        lines.push(line);
        if depth < opl_types::folder::MAX_FOLDER_DEPTH {
            tree_lines(folders, tree, f["id"].as_str(), depth + 1, lines);
        }
    }
}

/// 사용량 한 줄(`vcpus: 2 of 32 (bound by /teamA)`)
fn usage_lines(view: &Value) -> Vec<String> {
    let mut lines = vec![format!("folder {}", cell(&view["path"]))];
    let usage = &view["usage"];
    let dims = [
        ("vcpus", "vCPUs"),
        ("memory_mib", "memory (MiB)"),
        ("disk_bytes", "disk"),
        ("vms", "VMs"),
        ("dedicated_vcpus", "dedicated vCPUs"),
    ];
    let paths: BTreeMap<String, String> = view["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| (cell(&l["folder"]), cell(&l["path"])))
        .collect();
    for (key, label) in dims {
        let shown = |v: &Value| {
            if key == "disk_bytes" {
                bytes(v)
            } else {
                cell(v)
            }
        };
        // 전용 vCPU(계획 64)는 쓰거나 할당량이 있을 때만(지금 출력 그대로)
        if key == "dedicated_vcpus" && usage[key].is_null() && view["binding"][key].is_null() {
            continue;
        }
        let mut line = format!("{label}: {}", shown(&usage[key]));
        if let Some(binding) = view["binding"][key].as_str() {
            let limit = view["limits"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|l| l["folder"] == binding)
                .map(|l| shown(&l["quota"][key]))
                .unwrap_or_else(|| "-".into());
            let at = paths
                .get(binding)
                .cloned()
                .unwrap_or_else(|| binding.into());
            line.push_str(&format!(" of {limit} (bound by {at})"));
        }
        lines.push(line);
    }
    lines
}

pub(crate) async fn folder(
    client: &Client,
    cmd: FolderCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        FolderCmd::List { kind } => {
            let folders = Folders::load(client).await?;
            let mut shown: Vec<&Value> = folders
                .0
                .iter()
                .filter(|f| kind.is_none_or(|k| f["spec"]["kind"] == k.api()))
                .collect();
            if json_out {
                print_json(out, &json!(shown))?;
            } else {
                shown.sort_by_key(|f| (cell(&f["spec"]["kind"]), folders.path(&cell(&f["id"]))));
                let rows: Vec<_> = shown.iter().map(|f| folder_row(&folders, f)).collect();
                table(out, &["PATH", "KIND", "ID", "QUOTA"], &rows)?;
            }
        }
        FolderCmd::Tree { kind } => {
            let folders = Folders::load(client).await?;
            for tree in TREES {
                if kind.is_some_and(|k| k.api() != tree) {
                    continue;
                }
                let mut lines = Vec::new();
                tree_lines(&folders, tree, None, 1, &mut lines);
                writeln!(out, "{tree}:/")?;
                for line in lines {
                    writeln!(out, "{line}")?;
                }
            }
        }
        FolderCmd::Get { folder, kind } => {
            let id = folder_id(client, kind, &folder).await?;
            print_json(out, &client.get(&format!("/folders/{id}")).await?)?;
        }
        FolderCmd::Create { path, kind, quota } => {
            let names = opl_types::folder::parse_folder_path(split_tree(&path).1)
                .map_err(|e| anyhow::anyhow!("bad folder path {path:?}: {e}"))?;
            let Some((name, parents)) = names.split_last() else {
                bail!("give the path of the new folder, like /teamA");
            };
            let tree = split_tree(&path).0.unwrap_or(kind.api());
            let parent = if parents.is_empty() {
                None
            } else {
                Folders::load(client)
                    .await?
                    .resolve(Some(tree), &format!("/{}", parents.join("/")))?
                    .1
            };
            let mut body = json!({ "name": name, "kind": tree });
            if let Some(p) = parent {
                body["parent"] = json!(p);
            }
            if !quota.is_empty() {
                body["quota"] = quota.apply(&Value::Null)?;
            }
            let created = client.post("/folders", &body).await?;
            if json_out {
                print_json(out, &created)?;
            } else {
                writeln!(
                    out,
                    "folder {tree}:{path} created ({})",
                    cell(&created["id"])
                )?;
            }
        }
        FolderCmd::Rename { folder, name, kind } => {
            let id = folder_id(client, kind, &folder).await?;
            client
                .patch(&format!("/folders/{id}"), &json!({ "name": name }))
                .await?;
            writeln!(out, "folder {folder} renamed to {name}")?;
        }
        FolderCmd::Move { folder, to, kind } => {
            let folders = Folders::load(client).await?;
            let (tree, id) = folders.resolve(Some(kind.api()), &folder)?;
            let id = id.context("the root cannot be moved")?;
            let (_, parent) = folders.resolve(Some(tree), &to)?;
            client
                .patch(&format!("/folders/{id}"), &json!({ "parent": parent }))
                .await?;
            writeln!(out, "folder {folder} moved to {to}")?;
        }
        FolderCmd::Delete { folder, kind } => {
            let id = folder_id(client, kind, &folder).await?;
            client.delete(&format!("/folders/{id}")).await?;
            writeln!(out, "folder {folder} deleted")?;
        }
        FolderCmd::Members {
            cmd: MembersCmd::Move { dest, items },
        } => members_move(client, &dest, &items, json_out, out).await?,
        FolderCmd::Quota { cmd } => {
            let (folder, quota) = match &cmd {
                QuotaCmd::Set { folder, quota } => {
                    if quota.is_empty() {
                        bail!("give --vcpus, --memory, --disk or --vms");
                    }
                    (folder, Some(quota))
                }
                QuotaCmd::Clear { folder } => (folder, None),
            };
            let id = folder_id(client, Tree::Vm, folder).await?;
            let quota = match quota {
                Some(q) => {
                    let current = client.get(&format!("/folders/{id}")).await?;
                    let q = q.apply(&current["spec"]["quota"])?;
                    // 모든 차원을 지웠으면 할당량을 지운다.
                    if q.as_object().is_some_and(Map::is_empty) {
                        Value::Null
                    } else {
                        q
                    }
                }
                None => Value::Null,
            };
            let updated = client
                .patch(&format!("/folders/{id}"), &json!({ "quota": quota }))
                .await?;
            writeln!(
                out,
                "folder {folder}: quota {}",
                quota_text(&updated["spec"]["quota"])
            )?;
        }
        FolderCmd::Usage { folder } => {
            let id = folder_id(client, Tree::Vm, &folder).await?;
            let view = client.get(&format!("/folders/{id}/usage")).await?;
            if json_out {
                print_json(out, &view)?;
            } else {
                for line in usage_lines(&view) {
                    writeln!(out, "{line}")?;
                }
            }
        }
    }
    Ok(())
}

/// 루트가 아닌 폴더 하나의 id
async fn folder_id(client: &Client, kind: Tree, s: &str) -> anyhow::Result<String> {
    resolve_folder(client, kind.api(), s)
        .await?
        .with_context(|| format!("{s} is the root of the tree, not a folder"))
}

async fn members_move(
    client: &Client,
    dest: &str,
    items: &[String],
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let parsed = items
        .iter()
        .map(|s| member_item(s))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let tree = parsed[0].1;
    if parsed.iter().any(|(_, t, _)| *t != tree) {
        bail!("move VMs, networks and storage pools separately (each has its own folder tree)");
    }
    let (_, id) = Folders::load(client).await?.resolve(Some(tree), dest)?;
    let body = json!({
        "items": parsed.iter().map(|(kind, _, key)| json!({ "kind": kind, "key": key })).collect::<Vec<_>>()
    });
    let path = format!("/folders/{}/members", id.as_deref().unwrap_or("root"));
    let result = client.post(&path, &body).await?;
    if json_out {
        print_json(out, &result)?;
        return Ok(());
    }
    let mut failed = 0;
    for r in result["items"].as_array().into_iter().flatten() {
        let what = format!("{} {}", cell(&r["kind"]), cell(&r["key"]));
        if r["ok"] == true {
            writeln!(out, "{what}: moved to {dest}")?;
        } else {
            failed += 1;
            writeln!(
                out,
                "{what}: {} ({})",
                cell(&r["error"]["message"]),
                cell(&r["error"]["code"])
            )?;
        }
    }
    if failed > 0 {
        bail!("{failed} of {} items were not moved", parsed.len());
    }
    Ok(())
}

// ---- 대상·주체·역할 ----

/// 내장 역할 이름
const BUILTIN_ROLES: [&str; 3] = ["admin", "operator", "viewer"];

/// 대상 인자(`root`, 폴더 `/teamA`(VM 트리)·`network:/lan`·`folder/<id|경로>`, `vm/NAME`, `network/NAME`, `storage-pool/NAME`)를
/// grant 대상 JSON으로(id로 찾는다).
pub(crate) async fn grant_target(client: &Client, s: &str) -> anyhow::Result<Value> {
    if s == "root" {
        return Ok(json!({ "type": "root" }));
    }
    if let Some(folder) = folder_arg(s) {
        let (_, id) = Folders::load(client).await?.resolve_target(folder)?;
        return Ok(match id {
            Some(id) => json!({ "type": "folder", "id": id }),
            None => json!({ "type": "root" }),
        });
    }
    let (kind, key) = object_arg(s)?;
    let path = match kind {
        "vm" => "vms",
        "network" => "networks",
        _ => "storage-pools",
    };
    let obj = client.get(&format!("/{path}/{}", seg(key))).await?;
    Ok(json!({ "type": "object", "kind": kind, "id": obj["id"] }))
}

/// 폴더를 가리키는 대상 인자면 그 폴더 인자(`folder/` 뗌)
fn folder_arg(s: &str) -> Option<&str> {
    if let Some(rest) = s.strip_prefix("folder/") {
        return Some(rest);
    }
    let (tree, rest) = split_tree(s);
    (rest.starts_with('/') || tree.is_some()).then_some(s)
}

/// 객체 인자 `vm/web`의 (API 종류, 이름)
fn object_arg(s: &str) -> anyhow::Result<(&'static str, &str)> {
    let (kind, key) = s.split_once('/').with_context(|| {
        format!("expected root, a folder (/teamA), vm/NAME, network/NAME or storage-pool/NAME, got {s:?}")
    })?;
    let kind = match kind {
        "vm" => "vm",
        "network" => "network",
        "storage-pool" | "pool" => "storage-pool",
        _ => bail!("unknown kind {kind:?} in {s:?}; use folder, vm, network or storage-pool"),
    };
    if key.is_empty() {
        bail!("no name in {s:?}");
    }
    Ok((kind, key))
}

/// `/permissions/effective`·`/who`의 `object` 값(`root`, `folder/<id>`, `vm/<이름>` …)
async fn permission_object(client: &Client, s: &str) -> anyhow::Result<String> {
    if s == "root" {
        return Ok("root".into());
    }
    if let Some(folder) = folder_arg(s) {
        let (_, id) = Folders::load(client).await?.resolve_target(folder)?;
        return Ok(match id {
            Some(id) => format!("folder/{id}"),
            None => "root".into(),
        });
    }
    let (kind, key) = object_arg(s)?;
    Ok(format!("{kind}/{key}"))
}

/// 이름이나 id로 사용자 id를 찾는다(사용자 목록은 `cluster.admin`만 읽으므로 그 밖에는 id를 준다).
async fn user_id(client: &Client, key: &str) -> anyhow::Result<String> {
    if let Ok(id) = uuid::Uuid::try_parse(key) {
        return Ok(id.to_string());
    }
    let me = client.get("/auth/whoami").await?;
    if me["name"] == key {
        return Ok(cell(&me["id"]));
    }
    let users = client
        .get("/users")
        .await
        .with_context(|| format!("cannot look up user {key}; give the user id (see `perm who`)"))?;
    items(&users)
        .into_iter()
        .find(|u| u["name"] == key)
        .map(|u| cell(&u["id"]))
        .with_context(|| format!("user {key} not found"))
}

async fn group_id(client: &Client, key: &str) -> anyhow::Result<String> {
    if let Ok(id) = uuid::Uuid::try_parse(key) {
        return Ok(id.to_string());
    }
    Ok(cell(
        &client.get(&format!("/groups/{}", seg(key))).await?["id"],
    ))
}

/// 역할 인자(내장 이름이나 사용자 정의 역할의 이름·id)를 grant의 역할 JSON으로
async fn role_ref(client: &Client, key: &str) -> anyhow::Result<Value> {
    if BUILTIN_ROLES.contains(&key) {
        return Ok(json!({ "type": "builtin", "role": key }));
    }
    let roles = items(&client.get("/access-roles").await?);
    roles
        .iter()
        .find(|r| r["builtin"] != true && (r["name"] == key || r["id"] == key))
        .map(|r| json!({ "type": "custom", "id": r["id"] }))
        .with_context(|| format!("role {key} not found"))
}

/// grant 표에 이름을 쓰려고 읽는 목록. 읽을 수 없는 것은 비워 두고 id를 보인다.
struct Names {
    users: Vec<Value>,
    groups: Vec<Value>,
    roles: Vec<Value>,
    folders: Folders,
    objects: Vec<(&'static str, Vec<Value>)>,
}

impl Names {
    async fn load(client: &Client, grants: &[Value]) -> Self {
        let list = |path: &'static str| async move {
            client
                .get(path)
                .await
                .map(|l| items(&l))
                .unwrap_or_default()
        };
        let mut objects = Vec::new();
        for (kind, path) in [
            ("vm", "/vms"),
            ("network", "/networks"),
            ("storage-pool", "/storage-pools"),
        ] {
            if grants.iter().any(|g| g["spec"]["target"]["kind"] == kind) {
                objects.push((kind, list(path).await));
            }
        }
        Self {
            users: list("/users").await,
            groups: list("/groups").await,
            roles: list("/access-roles").await,
            folders: Folders::load(client).await.unwrap_or(Folders(Vec::new())),
            objects,
        }
    }

    fn find(list: &[Value], id: &Value) -> String {
        list.iter()
            .find(|v| v["id"] == *id)
            .map_or_else(|| cell(id), |v| cell(&v["name"]))
    }

    fn principal(&self, p: &Value) -> String {
        match p["type"].as_str() {
            Some("group") => format!("group:{}", Self::find(&self.groups, &p["id"])),
            _ => format!("user:{}", Self::find(&self.users, &p["id"])),
        }
    }

    fn role(&self, r: &Value) -> String {
        match r["type"].as_str() {
            Some("builtin") => cell(&r["role"]),
            _ => Self::find(&self.roles, &r["id"]),
        }
    }

    fn target(&self, t: &Value) -> String {
        match t["type"].as_str() {
            Some("root") => "root".into(),
            Some("folder") => {
                let id = cell(&t["id"]);
                let kind = self
                    .folders
                    .by_id(&id)
                    .map(|f| cell(&f["spec"]["kind"]))
                    .unwrap_or_else(|| "folder".into());
                format!("{kind}:{}", self.folders.path(&id))
            }
            _ => {
                let kind = cell(&t["kind"]);
                let list = self
                    .objects
                    .iter()
                    .find(|(k, _)| *k == kind)
                    .map(|(_, l)| l.as_slice())
                    .unwrap_or_default();
                format!("{kind}/{}", Self::find(list, &t["id"]))
            }
        }
    }

    fn grant_row(&self, g: &Value) -> Vec<String> {
        let s = &g["spec"];
        vec![
            cell(&g["id"]),
            self.principal(&s["principal"]),
            self.role(&s["role"]),
            self.target(&s["target"]),
            (s["propagate"] != false).to_string(),
        ]
    }
}

const GRANT_HEADERS: &[&str] = &["ID", "PRINCIPAL", "ROLE", "TARGET", "PROPAGATE"];

// ---- 역할 ----

#[derive(Subcommand)]
pub(crate) enum RoleCmd {
    /// 내장 역할(admin·operator·viewer)과 사용자 정의 역할
    List,
    Get {
        role: String,
    },
    /// 권한 이름 목록(역할과 토큰 범위에 쓴다)
    Privileges,
    /// 사용자 정의 역할을 만든다(`cluster.admin`).
    Create {
        name: String,
        /// 권한 이름(`vm.power,vm.console`). `role privileges`가 목록을 보인다.
        #[arg(long, required = true, value_delimiter = ',')]
        privileges: Vec<String>,
        #[arg(long, default_value = "")]
        description: String,
    },
    /// 권한 목록을 바꾼다. 이 역할을 건 모든 grant가 바로 바뀐다.
    Set {
        role: String,
        #[arg(long, required = true, value_delimiter = ',')]
        privileges: Vec<String>,
        /// 설명(없으면 그대로)
        #[arg(long)]
        description: Option<String>,
    },
    /// 사용자 정의 역할을 지운다(grant가 쓰고 있으면 409 `role_in_use`).
    Delete {
        role: String,
    },
}

pub(crate) async fn role(
    client: &Client,
    cmd: RoleCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        RoleCmd::List => {
            let list = client.get("/access-roles").await?;
            if json_out {
                print_json(out, &json!(items(&list)))?;
            } else {
                let rows: Vec<_> = items(&list)
                    .iter()
                    .map(|r| {
                        let privs: Vec<String> = r["privileges"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(cell)
                            .collect();
                        vec![
                            cell(&r["name"]),
                            if r["builtin"] == true {
                                "builtin"
                            } else {
                                "custom"
                            }
                            .into(),
                            privs.join(","),
                        ]
                    })
                    .collect();
                table(out, &["NAME", "KIND", "PRIVILEGES"], &rows)?;
            }
        }
        RoleCmd::Get { role } => print_json(
            out,
            &client.get(&format!("/access-roles/{}", seg(&role))).await?,
        )?,
        RoleCmd::Privileges => {
            for p in opl_types::authz::Privilege::ALL {
                writeln!(out, "{}", p.as_str())?;
            }
        }
        RoleCmd::Create {
            name,
            privileges,
            description,
        } => {
            let body =
                json!({ "name": name, "privileges": privileges, "description": description });
            client.post("/access-roles", &body).await?;
            writeln!(out, "role {name} created")?;
        }
        RoleCmd::Set {
            role,
            privileges,
            description,
        } => {
            let path = format!("/access-roles/{}", seg(&role));
            let description = match description {
                Some(d) => d,
                None => cell(&client.get(&path).await?["description"]),
            };
            let body = json!({ "privileges": privileges, "description": description });
            client.put(&path, &body).await?;
            writeln!(out, "role {role} updated")?;
        }
        RoleCmd::Delete { role } => {
            client
                .delete(&format!("/access-roles/{}", seg(&role)))
                .await?;
            writeln!(out, "role {role} deleted")?;
        }
    }
    Ok(())
}

// ---- 그룹 ----

/// 외부 그룹 `REALM:GROUP`(LDAP 그룹 DN 또는 OIDC `groups` 값)
fn parse_external(s: &str) -> Result<Value, String> {
    match s.split_once(':') {
        Some((realm, group)) if !realm.is_empty() && !group.trim().is_empty() => {
            Ok(json!({ "realm": realm, "group": group }))
        }
        _ => Err(format!("expected REALM:GROUP, got {s:?}")),
    }
}

#[derive(Subcommand)]
pub(crate) enum GroupCmd {
    List,
    Get {
        group: String,
    },
    /// 로컬 그룹을 만든다(`cluster.admin`). 외부 그룹에 든 영역 사용자는 로그인 때 이 그룹에 든다.
    Create {
        name: String,
        /// 멤버 사용자(이름이나 id). 여러 번 줄 수 있다.
        #[arg(long = "member")]
        members: Vec<String>,
        /// 연결할 외부 그룹 `REALM:GROUP`(LDAP 그룹 DN, OIDC `groups` 값). 여러 번 줄 수 있다.
        #[arg(long = "external", value_parser = parse_external)]
        external: Vec<Value>,
        #[arg(long, default_value = "")]
        description: String,
    },
    /// 멤버·외부 그룹·설명을 바꾼다(준 것만).
    Set {
        group: String,
        #[arg(long)]
        add_member: Vec<String>,
        #[arg(long)]
        remove_member: Vec<String>,
        #[arg(long, value_parser = parse_external)]
        add_external: Vec<Value>,
        #[arg(long, value_parser = parse_external)]
        remove_external: Vec<Value>,
        #[arg(long)]
        description: Option<String>,
    },
    /// 그룹을 지운다(grant가 쓰고 있으면 409 `group_in_use`).
    Delete {
        group: String,
    },
}

/// 같은 외부 그룹인지(앞뒤 공백, ASCII 대소문자 무시 — 서버와 같은 비교)
fn same_external(a: &Value, b: &Value) -> bool {
    a["realm"] == b["realm"] && opl_types::realm::same_group(&cell(&a["group"]), &cell(&b["group"]))
}

pub(crate) async fn group(
    client: &Client,
    cmd: GroupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        GroupCmd::List => {
            let list = client.get("/groups").await?;
            if json_out {
                print_json(out, &json!(items(&list)))?;
            } else {
                let users = client
                    .get("/users")
                    .await
                    .map(|l| items(&l))
                    .unwrap_or_default();
                let rows: Vec<_> = items(&list)
                    .iter()
                    .map(|g| {
                        let members: Vec<String> = g["spec"]["members"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|m| Names::find(&users, m))
                            .collect();
                        let external: Vec<String> = g["spec"]["external"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|e| format!("{}:{}", cell(&e["realm"]), cell(&e["group"])))
                            .collect();
                        vec![cell(&g["name"]), members.join(","), external.join(";")]
                    })
                    .collect();
                table(out, &["NAME", "MEMBERS", "EXTERNAL"], &rows)?;
            }
        }
        GroupCmd::Get { group } => {
            print_json(out, &client.get(&format!("/groups/{}", seg(&group))).await?)?
        }
        GroupCmd::Create {
            name,
            members,
            external,
            description,
        } => {
            let mut ids = Vec::new();
            for m in &members {
                ids.push(user_id(client, m).await?);
            }
            let body = json!({
                "name": name,
                "spec": { "members": ids, "external": external, "description": description }
            });
            client.post("/groups", &body).await?;
            writeln!(out, "group {name} created")?;
        }
        GroupCmd::Set {
            group,
            add_member,
            remove_member,
            add_external,
            remove_external,
            description,
        } => {
            let path = format!("/groups/{}", seg(&group));
            let current = client.get(&path).await?;
            let mut spec = current["spec"].clone();
            let mut members: Vec<Value> = spec["members"].as_array().cloned().unwrap_or_default();
            for m in &add_member {
                let id = json!(user_id(client, m).await?);
                if !members.contains(&id) {
                    members.push(id);
                }
            }
            for m in &remove_member {
                let id = json!(user_id(client, m).await?);
                members.retain(|x| *x != id);
            }
            let mut external: Vec<Value> = spec["external"].as_array().cloned().unwrap_or_default();
            for e in add_external {
                if !external.iter().any(|x| same_external(x, &e)) {
                    external.push(e);
                }
            }
            external.retain(|x| !remove_external.iter().any(|e| same_external(x, e)));
            spec["members"] = json!(members);
            spec["external"] = json!(external);
            if let Some(d) = description {
                spec["description"] = json!(d);
            }
            let body = json!({ "labels": current["labels"], "spec": spec });
            let r = client
                .request(reqwest::Method::PUT, &path)
                .json(&body)
                .header("if-match", cell(&current["revision"]));
            client.send(r).await?;
            writeln!(out, "group {group} updated")?;
        }
        GroupCmd::Delete { group } => {
            client.delete(&format!("/groups/{}", seg(&group))).await?;
            writeln!(out, "group {group} deleted")?;
        }
    }
    Ok(())
}

// ---- grant ----

#[derive(Args)]
#[command(group(ArgGroup::new("who").args(["user", "group"])))]
pub(crate) struct Principal {
    /// 사용자(이름이나 id)
    #[arg(long)]
    user: Option<String>,
    /// 로컬 그룹(이름이나 id)
    #[arg(long)]
    group: Option<String>,
}

impl Principal {
    async fn value(&self, client: &Client) -> anyhow::Result<Option<Value>> {
        Ok(match (&self.user, &self.group) {
            (Some(u), _) => Some(json!({ "type": "user", "id": user_id(client, u).await? })),
            (_, Some(g)) => Some(json!({ "type": "group", "id": group_id(client, g).await? })),
            _ => None,
        })
    }
}

#[derive(Subcommand)]
pub(crate) enum GrantCmd {
    /// 볼 수 있는 grant(`cluster.admin`은 전부, 그 밖에는 자기·자기 그룹의 것과 `permission.modify`가 있는 대상의 것)
    List {
        /// 이 대상의 것만
        #[arg(long)]
        on: Option<String>,
        #[command(flatten)]
        who: Principal,
    },
    /// 주체에게 대상에서 역할을 준다. 루트는 `cluster.admin`, 그 밖에는 대상의 `permission.modify`가 필요하고 역할의
    /// 권한을 그 대상에서 가지고 있어야 한다.
    Add {
        #[command(flatten)]
        who: Principal,
        /// 내장 역할(admin·operator·viewer)이나 사용자 정의 역할
        #[arg(long)]
        role: String,
        /// `root`, 폴더(`/teamA`, `network:/lan`), `vm/NAME`, `network/NAME`, `storage-pool/NAME`
        #[arg(long)]
        on: String,
        /// 하위 폴더와 그 안의 객체로 내려가지 않는다(폴더 자체에만).
        #[arg(long)]
        no_propagate: bool,
    },
    Remove {
        id: String,
    },
}

pub(crate) async fn grant(
    client: &Client,
    cmd: GrantCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        GrantCmd::List { on, who } => {
            let mut query: Vec<(&str, String)> = Vec::new();
            if let Some(on) = &on {
                query.push(("target", target_query(&grant_target(client, on).await?)));
            }
            if let Some(p) = who.value(client).await? {
                query.push((
                    "principal",
                    format!("{}/{}", cell(&p["type"]), cell(&p["id"])),
                ));
            }
            let list = client
                .send(
                    client
                        .request(reqwest::Method::GET, "/grants")
                        .query(&query),
                )
                .await?;
            let grants = items(&list);
            if json_out {
                print_json(out, &json!(grants))?;
            } else {
                let names = Names::load(client, &grants).await;
                let rows: Vec<_> = grants.iter().map(|g| names.grant_row(g)).collect();
                table(out, GRANT_HEADERS, &rows)?;
            }
        }
        GrantCmd::Add {
            who,
            role,
            on,
            no_propagate,
        } => {
            let principal = who.value(client).await?.context("give --user or --group")?;
            let body = json!({
                "principal": principal,
                "role": role_ref(client, &role).await?,
                "target": grant_target(client, &on).await?,
                "propagate": !no_propagate,
            });
            let created = client.post("/grants", &body).await?;
            if json_out {
                print_json(out, &created)?;
            } else {
                writeln!(out, "grant {} added", cell(&created["id"]))?;
            }
        }
        GrantCmd::Remove { id } => {
            client.delete(&format!("/grants/{}", seg(&id))).await?;
            writeln!(out, "grant {id} removed")?;
        }
    }
    Ok(())
}

/// grant 대상 JSON을 `GET /grants?target=`의 값으로
fn target_query(t: &Value) -> String {
    match t["type"].as_str() {
        Some("root") => "root".into(),
        Some("folder") => format!("folder/{}", cell(&t["id"])),
        _ => format!("{}/{}", cell(&t["kind"]), cell(&t["id"])),
    }
}

// ---- 유효 권한 ----

#[derive(Subcommand)]
pub(crate) enum PermCmd {
    /// 내 권한: 루트, 폴더마다(폴더 자체와 그 안의 것), 객체마다
    Mine,
    /// 사용자의 대상 권한과 그 근거 grant(남의 것은 대상의 `permission.modify`가 필요하다)
    #[command(alias = "check")]
    Effective {
        /// `root`, 폴더(`/teamA`), `vm/NAME`, `network/NAME`, `storage-pool/NAME`
        object: String,
        /// 사용자(이름이나 id). 없으면 나
        #[arg(long)]
        user: Option<String>,
    },
    /// 대상에 닿는 grant와 그 사용자들(대상의 `permission.modify`가 필요하다)
    Who { object: String },
}

fn privilege_list(v: &Value) -> String {
    let list: Vec<String> = v.as_array().into_iter().flatten().map(cell).collect();
    if list.is_empty() {
        "-".into()
    } else {
        list.join(",")
    }
}

pub(crate) async fn perm(
    client: &Client,
    cmd: PermCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        PermCmd::Mine => {
            let mine = client.get("/permissions/mine").await?;
            if json_out {
                return Ok(print_json(out, &mine)?);
            }
            if mine["all"] == true {
                writeln!(out, "all privileges everywhere (admin)")?;
                return Ok(());
            }
            let folders = Folders::load(client).await?;
            let mut rows = vec![vec![
                "root".to_string(),
                privilege_list(&mine["root"]),
                privilege_list(&mine["root_contents"]),
            ]];
            let mut ids: Vec<&String> = mine["folders"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, _)| k)
                .chain(
                    mine["contents"]
                        .as_object()
                        .into_iter()
                        .flatten()
                        .map(|(k, _)| k),
                )
                .collect();
            ids.sort_by_key(|id| folders.path(id));
            ids.dedup();
            for id in ids {
                let kind = folders
                    .by_id(id)
                    .map(|f| cell(&f["spec"]["kind"]))
                    .unwrap_or_else(|| "folder".into());
                rows.push(vec![
                    format!("{kind}:{}", folders.path(id)),
                    privilege_list(&mine["folders"][id]),
                    privilege_list(&mine["contents"][id]),
                ]);
            }
            for o in mine["objects"].as_array().into_iter().flatten() {
                rows.push(vec![
                    format!("{}/{}", cell(&o["kind"]), cell(&o["id"])),
                    privilege_list(&o["privileges"]),
                    "-".into(),
                ]);
            }
            table(out, &["TARGET", "ON IT", "ON ITS CONTENTS"], &rows)?;
        }
        PermCmd::Effective { object, user } => {
            let mut query = vec![("object", permission_object(client, &object).await?)];
            if let Some(u) = user {
                query.push(("user", u));
            }
            let view = client
                .send(
                    client
                        .request(reqwest::Method::GET, "/permissions/effective")
                        .query(&query),
                )
                .await?;
            if json_out {
                return Ok(print_json(out, &view)?);
            }
            writeln!(
                out,
                "{} (role {}) on {object}: {}",
                cell(&view["name"]),
                cell(&view["role"]),
                privilege_list(&view["privileges"])
            )?;
            let grants = items(&json!({ "items": view["grants"] }));
            if !grants.is_empty() {
                let names = Names::load(client, &grants).await;
                let rows: Vec<_> = grants.iter().map(|g| names.grant_row(g)).collect();
                table(out, GRANT_HEADERS, &rows)?;
            }
        }
        PermCmd::Who { object } => {
            let query = [("object", permission_object(client, &object).await?)];
            let view = client
                .send(
                    client
                        .request(reqwest::Method::GET, "/permissions/who")
                        .query(&query),
                )
                .await?;
            if json_out {
                return Ok(print_json(out, &view)?);
            }
            let grants = items(&json!({ "items": view["grants"] }));
            let names = Names::load(client, &grants).await;
            let rows: Vec<_> = grants.iter().map(|g| names.grant_row(g)).collect();
            table(out, GRANT_HEADERS, &rows)?;
            writeln!(out)?;
            let users: Vec<_> = view["users"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|u| vec![cell(&u["name"]), cell(&u["id"]), cell(&u["role"])])
                .collect();
            table(out, &["USER", "ID", "ROLE"], &users)?;
        }
    }
    Ok(())
}

// ---- 토큰 범위 ----

/// `--scope PRIVS@TARGET`(여러 번). 범위는 권한 목록 하나와 대상 여럿이라 모든 `--scope`의 권한이 같아야 한다.
pub(crate) async fn token_scope(
    client: &Client,
    scopes: &[String],
) -> anyhow::Result<Option<Value>> {
    let mut privileges: Option<Vec<String>> = None;
    let mut targets = Vec::new();
    for s in scopes {
        let (privs, target) = s.rsplit_once('@').with_context(|| {
            format!("expected PRIVILEGES@TARGET (vm.power,vm.console@/teamA), got {s:?}")
        })?;
        let mut list: Vec<String> = privs
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        list.sort();
        list.dedup();
        match &privileges {
            Some(p) if *p != list => bail!(
                "a token scope has one privilege list for all its targets; give the same privileges in every --scope"
            ),
            _ => privileges = Some(list),
        }
        targets.push(grant_target(client, target).await?);
    }
    Ok(privileges.map(|p| json!({ "privileges": p, "targets": targets })))
}

/// 토큰 목록의 범위 칸(`vm.power,vm.console@2 targets`). 없으면 `-`
pub(crate) fn scope_cell(scope: &Value) -> String {
    if scope.is_null() {
        return "-".into();
    }
    let n = scope["targets"].as_array().map_or(0, Vec::len);
    let targets = if n == 1 {
        "1 target".to_string()
    } else {
        format!("{n} targets")
    };
    format!("{}@{targets}", privilege_list(&scope["privileges"]))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::fake_api::fake_api;

    const A: &str = "0b7f3a52-3f0e-4d5c-9a51-2b8f0c1d2e3a";
    const B: &str = "0b7f3a52-3f0e-4d5c-9a51-2b8f0c1d2e3b";
    const N: &str = "0b7f3a52-3f0e-4d5c-9a51-2b8f0c1d2e3c";
    const P: &str = "0b7f3a52-3f0e-4d5c-9a51-2b8f0c1d2e3d";

    /// teamA(VM, 할당량) > prod, network 트리의 teamA, 부모를 볼 수 없는 VM 폴더 orphan
    fn folders() -> Value {
        json!({ "revision": 1, "items": [
            { "id": A, "spec": { "name": "teamA", "kind": "vm", "quota": { "vcpus": 32, "vms": 2 } } },
            { "id": B, "spec": { "name": "prod", "kind": "vm", "parent": A } },
            { "id": N, "spec": { "name": "TeamA", "kind": "network" } },
            { "id": P, "spec": { "name": "orphan", "kind": "vm", "parent": "0b7f3a52-3f0e-4d5c-9a51-2b8f0c1d2e3f" } },
        ]})
    }

    #[test]
    fn resolves_paths_ids_and_trees() {
        let f = Folders(items(&folders()));
        assert_eq!(f.path(B), "/teamA/prod");
        assert_eq!(f.path(P), "/orphan");
        assert_eq!(
            f.resolve(Some("vm"), "/teama/PROD").unwrap(),
            ("vm", Some(B.into()))
        );
        assert_eq!(f.resolve(Some("vm"), "/").unwrap(), ("vm", None));
        assert_eq!(f.resolve(None, "vm:/").unwrap(), ("vm", None));
        assert_eq!(f.resolve(None, B).unwrap(), ("vm", Some(B.into())));
        assert_eq!(
            f.resolve(None, "network:/teamA").unwrap(),
            ("network", Some(N.into()))
        );
        assert_eq!(
            f.resolve(None, "/teamA/prod").unwrap(),
            ("vm", Some(B.into()))
        );
        assert_eq!(f.resolve(None, "/orphan").unwrap(), ("vm", Some(P.into())));
        assert_eq!(f.resolve_target("/teamA").unwrap(), ("vm", Some(A.into())));
        assert_eq!(
            f.resolve_target("network:/teamA").unwrap(),
            ("network", Some(N.into()))
        );
        assert_eq!(f.resolve_target(N).unwrap(), ("network", Some(N.into())));
        let err = |tree, s| f.resolve(tree, s).unwrap_err().to_string();
        assert!(err(None, "/teamA").contains("more than one tree"));
        assert!(err(None, "/").contains("give the tree of the root"));
        assert_eq!(err(Some("vm"), "/teamB"), "folder /teamB not found");
        assert!(err(Some("vm"), "network:/teamA").contains("is a network folder"));
        assert!(err(Some("network"), B).contains("is a vm folder"));
        assert!(err(Some("vm"), "teamA").contains("must be a path"));
        let all = f.subtree("vm", None);
        assert_eq!(all.len(), 4, "{all:?}");
        assert_eq!(
            f.subtree("vm", Some(A)),
            HashSet::from([Some(A.to_string()), Some(B.to_string())])
        );
    }

    #[test]
    fn quota_arguments_merge_and_print() {
        let q = QuotaArgs {
            vcpus: Some("none".into()),
            memory: Some("128G".into()),
            disk: Some("2T".into()),
            vms: None,
            dedicated_vcpus: Some("8".into()),
        };
        let merged = q.apply(&json!({ "vcpus": 32, "vms": 2 })).unwrap();
        assert_eq!(
            merged,
            json!({ "memory_mib": 131072, "disk_bytes": 2u64 << 40, "vms": 2, "dedicated_vcpus": 8 })
        );
        assert_eq!(
            quota_text(&merged),
            "128.0GiB memory, 2.0TiB disk, 2 VMs, 8 dedicated vCPUs"
        );
        assert_eq!(quota_text(&Value::Null), "-");
        assert_eq!(parse_mib("512"), Ok(512));
        assert!(parse_mib("1000K").is_err());
        let bad = QuotaArgs {
            vms: Some("many".into()),
            ..Default::default()
        };
        assert!(bad.apply(&Value::Null).is_err());
    }

    #[test]
    fn parses_items_targets_and_externals() {
        assert_eq!(
            member_item("pool/fast").unwrap(),
            ("storage-pool", "storage", "fast")
        );
        assert!(member_item("volume/x").is_err());
        assert!(member_item("vm/").is_err());
        assert_eq!(folder_arg("/teamA"), Some("/teamA"));
        assert_eq!(folder_arg("folder/vm:/teamA"), Some("vm:/teamA"));
        assert_eq!(folder_arg("network:/lan"), Some("network:/lan"));
        assert_eq!(folder_arg("vm/web"), None);
        assert_eq!(
            object_arg("storage-pool/fast").unwrap(),
            ("storage-pool", "fast")
        );
        assert!(object_arg("node/n1").is_err());
        assert_eq!(
            parse_external("corp:cn=ops,dc=example").unwrap(),
            json!({ "realm": "corp", "group": "cn=ops,dc=example" })
        );
        assert!(parse_external("corp").is_err());
        assert!(same_external(
            &json!({ "realm": "corp", "group": " CN=Ops " }),
            &json!({ "realm": "corp", "group": "cn=ops" })
        ));
        assert_eq!(scope_cell(&Value::Null), "-");
        assert_eq!(
            scope_cell(&json!({ "privileges": ["vm.power"], "targets": [{ "type": "root" }] })),
            "vm.power@1 target"
        );
    }

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: Top,
    }

    #[derive(Subcommand)]
    enum Top {
        Folder {
            #[command(subcommand)]
            cmd: FolderCmd,
        },
        Grant {
            #[command(subcommand)]
            cmd: GrantCmd,
        },
        Perm {
            #[command(subcommand)]
            cmd: PermCmd,
        },
        Group {
            #[command(subcommand)]
            cmd: GroupCmd,
        },
    }

    fn parse(args: &[&str]) -> Top {
        let mut argv = vec!["t"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).unwrap().cmd
    }

    /// 경로·목록 응답을 정한 가짜 API
    async fn api() -> (Client, crate::fake_api::SeenLog) {
        fake_api(|method, path| match (method, path) {
            ("GET", "/api/v1/folders") => (200, folders()),
            ("GET", p) if p.starts_with(&format!("/api/v1/folders/{A}/usage")) => (
                200,
                json!({
                    "folder": A, "path": "/teamA",
                    "usage": { "vcpus": 2, "memory_mib": 512, "disk_bytes": 0, "vms": 2 },
                    "limits": [{ "folder": A, "path": "/teamA", "quota": { "vcpus": 32, "vms": 2 },
                                 "usage": { "vcpus": 2, "memory_mib": 512, "disk_bytes": 0, "vms": 2 } }],
                    "binding": { "vcpus": A, "vms": A }
                }),
            ),
            ("GET", p) if p == format!("/api/v1/folders/{A}") => (
                200,
                json!({ "id": A, "revision": 3, "spec": { "name": "teamA", "kind": "vm", "quota": { "vcpus": 32, "vms": 2 } } }),
            ),
            ("PATCH", _) => (
                200,
                json!({ "id": A, "spec": { "name": "teamA", "kind": "vm", "quota": { "vcpus": 32, "vms": 3 } } }),
            ),
            ("POST", "/api/v1/folders") => (201, json!({ "id": "new-id" })),
            ("POST", p) if p.ends_with("/members") => (
                200,
                json!({ "items": [
                    { "kind": "vm", "key": "web", "ok": true },
                    { "kind": "vm", "key": "db", "ok": false,
                      "error": { "code": "quota_exceeded", "message": "folder /teamA would use 3 of 2 VMs" } }
                ]}),
            ),
            ("GET", "/api/v1/vms/web") => (200, json!({ "id": "vm-id", "name": "web" })),
            ("GET", "/api/v1/groups/team-a") => (200, json!({ "id": "group-id", "name": "team-a" })),
            ("GET", "/api/v1/access-roles") => (
                200,
                json!({ "items": [
                    { "name": "operator", "builtin": true, "privileges": [] },
                    { "id": "role-id", "name": "vm-power", "builtin": false, "privileges": ["vm.power"] }
                ]}),
            ),
            ("POST", "/api/v1/grants") => (201, json!({ "id": "grant-id" })),
            ("GET", p) if p.starts_with("/api/v1/permissions/effective") => (
                200,
                json!({ "name": "alice", "role": "none", "privileges": ["system.read", "vm.power"], "grants": [] }),
            ),
            _ => (404, json!({ "code": "not_found", "message": format!("{method} {path}") })),
        })
        .await
    }

    async fn run(client: &Client, args: &[&str]) -> anyhow::Result<String> {
        let mut out = Vec::new();
        match parse(args) {
            Top::Folder { cmd } => folder(client, cmd, false, &mut out).await?,
            Top::Grant { cmd } => grant(client, cmd, false, &mut out).await?,
            Top::Perm { cmd } => perm(client, cmd, false, &mut out).await?,
            Top::Group { cmd } => group(client, cmd, false, &mut out).await?,
        }
        Ok(String::from_utf8(out).unwrap())
    }

    #[tokio::test]
    async fn folder_commands_send_ids() {
        let (client, seen) = api().await;
        let out = run(&client, &["folder", "create", "/teamA/dev", "--vms", "5"])
            .await
            .unwrap();
        assert_eq!(out, "folder vm:/teamA/dev created (new-id)\n");
        assert_eq!(
            seen.lock().unwrap().last().unwrap().body,
            json!({ "name": "dev", "kind": "vm", "parent": A, "quota": { "vms": 5 } })
        );

        let out = run(&client, &["folder", "tree"]).await.unwrap();
        assert_eq!(
            out,
            "vm:/\n  orphan\n  teamA (quota: 32 vCPUs, 2 VMs)\n    prod\nnetwork:/\n  TeamA\nstorage:/\n"
        );
        let out = run(&client, &["folder", "list", "--kind", "vm"])
            .await
            .unwrap();
        assert!(out.starts_with("PATH"), "{out}");
        assert!(out.contains(&format!("/teamA/prod  vm    {B}  -")), "{out}");

        run(&client, &["folder", "quota", "set", "/teamA", "--vms", "3"])
            .await
            .unwrap();
        let body = seen.lock().unwrap().last().unwrap().body.clone();
        assert_eq!(body, json!({ "quota": { "vcpus": 32, "vms": 3 } }));
        run(&client, &["folder", "quota", "clear", "/teamA"])
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap().last().unwrap().body,
            json!({ "quota": null })
        );
        run(&client, &["folder", "move", "/teamA/prod", "--to", "/"])
            .await
            .unwrap();
        let last = seen.lock().unwrap().last().unwrap().clone();
        assert_eq!(last.line, format!("PATCH /api/v1/folders/{B} HTTP/1.1"));
        assert_eq!(last.body, json!({ "parent": null }));

        let out = run(&client, &["folder", "usage", "/teamA"]).await.unwrap();
        assert_eq!(
            out,
            "folder /teamA\nvCPUs: 2 of 32 (bound by /teamA)\nmemory (MiB): 512\ndisk: 0B\nVMs: 2 of 2 (bound by /teamA)\n"
        );

        let err = run(
            &client,
            &["folder", "members", "move", "/teamA", "vm/web", "vm/db"],
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "1 of 2 items were not moved");
        let last = seen.lock().unwrap().last().unwrap().clone();
        assert_eq!(
            last.line,
            format!("POST /api/v1/folders/{A}/members HTTP/1.1")
        );
        assert_eq!(
            last.body,
            json!({ "items": [{ "kind": "vm", "key": "web" }, { "kind": "vm", "key": "db" }] })
        );
        let err = run(
            &client,
            &["folder", "members", "move", "/", "vm/web", "network/lan"],
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("separately"), "{err}");
    }

    #[tokio::test]
    async fn grants_resolve_names() {
        let (client, seen) = api().await;
        run(
            &client,
            &[
                "grant", "add", "--group", "team-a", "--role", "operator", "--on", "/teamA",
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            seen.lock().unwrap().last().unwrap().body,
            json!({
                "principal": { "type": "group", "id": "group-id" },
                "role": { "type": "builtin", "role": "operator" },
                "target": { "type": "folder", "id": A },
                "propagate": true
            })
        );
        run(
            &client,
            &[
                "grant",
                "add",
                "--user",
                A,
                "--role",
                "vm-power",
                "--on",
                "vm/web",
                "--no-propagate",
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            seen.lock().unwrap().last().unwrap().body,
            json!({
                "principal": { "type": "user", "id": A },
                "role": { "type": "custom", "id": "role-id" },
                "target": { "type": "object", "kind": "vm", "id": "vm-id" },
                "propagate": false
            })
        );
        let err = run(&client, &["grant", "add", "--role", "x", "--on", "root"])
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "give --user or --group");

        let out = run(
            &client,
            &["perm", "check", "/teamA/prod", "--user", "alice"],
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            "alice (role none) on /teamA/prod: system.read,vm.power\n"
        );
        let line = seen.lock().unwrap().last().unwrap().line.clone();
        assert_eq!(
            line,
            format!("GET /api/v1/permissions/effective?object=folder%2F{B}&user=alice HTTP/1.1")
        );
    }
}
