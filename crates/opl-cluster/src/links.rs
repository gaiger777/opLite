//! 노드 하나에 닿는 클러스터 링크 여럿 가운데 쓸 것을 고른다(설계 §7). 노드 간 gRPC(`opl-peer`의 `Peers`)와
//! memnet이 같은 규칙을 쓴다.
//!
//! 노드는 첫 링크(이 계획 전의 "그 노드의 주소", `Members`의 값)로 가리킨다. 처음에는 첫 링크를 쓰고, 연결하지
//! 못하면 곧바로, 제한 시간을 넘기면 지금 링크에서 연속 두 번 넘겼거나 그 링크의 마지막 성공에서
//! [`LINK_DEAD_AFTER`]가 지났을 때 다음 링크로 넘어간다(모두 실패하면 돌아가며). 우선 링크가 아닌 동안
//! [`LINK_RETRY`]마다 다음 호출 하나를 우선 링크로 보내 보고(시험 호출), 성공하면 돌아간다.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use tokio::time::Instant;

use crate::types::OrbNode;

/// 우선 링크가 아닌 동안 우선 링크를 다시 시도하는 간격
pub const LINK_RETRY: Duration = Duration::from_secs(30);

/// 제한 시간을 넘긴 링크를 죽었다고 보는, 그 링크의 마지막 성공(또는 그 링크로 넘어온 때)부터의 시간. 이 안의
/// 초과 한 번(하트비트 하나가 늦은 살아 있는 링크)으로는 넘어가지 않는다. 조용히 죽은 링크에서 마지막 성공 뒤 첫
/// 하트비트가 다음 링크로 가기까지(이것 + 하트비트 간격 200 ms)가 선거 제한 시간의 최솟값(1초)에서 하트비트
/// 간격을 뺀 것보다 짧아야 따르는 노드가 선거를 시작하지 않는다.
pub const LINK_DEAD_AFTER: Duration = Duration::from_millis(400);

/// 지금 링크에서 연속으로 이만큼 제한 시간을 넘기면 [`LINK_DEAD_AFTER`] 전이어도 넘어간다.
const TIMEOUTS_TO_SWITCH: u32 = 2;

/// 노드마다의 링크 목록(첫 링크 → 링크 목록). 링크가 하나인 노드는 넣지 않는다.
#[derive(Debug, Default)]
pub struct LinkBook {
    links: RwLock<HashMap<String, Vec<String>>>,
}

impl LinkBook {
    /// 첫 링크가 `addr`인 노드의 링크 목록. 모르는 노드는 `[addr]`이다.
    pub fn links(&self, addr: &str) -> Vec<String> {
        self.links
            .read()
            .unwrap()
            .get(addr)
            .cloned()
            .unwrap_or_else(|| vec![addr.to_string()])
    }

    /// 목록을 `nodes`로 바꾼다.
    pub fn replace<'a>(&self, nodes: impl IntoIterator<Item = &'a OrbNode>) {
        let links = nodes
            .into_iter()
            .filter(|n| n.addrs.len() > 1)
            .map(|n| (n.addr().to_string(), n.addrs.clone()))
            .collect();
        *self.links.write().unwrap() = links;
    }
}

/// 링크 하나로 보낸 호출이 실패한 까닭
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// 연결하지 못했다.
    Connect,
    /// 제한 시간 안에 답이 오지 않았다.
    Timeout,
}

/// 호출 하나가 쓸 링크
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub link: String,
    /// 지금 링크 대신 우선 링크로 보내 보는 시험 호출이다.
    pub trial: bool,
    /// 고른 때(호출을 보낸 때). 성공을 알릴 때 함께 준다([`LinkSelector::ok`]).
    pub sent: Instant,
}

/// 노드마다 지금 쓰는 링크. 링크가 둘 이상인 노드만 상태를 둔다.
#[derive(Debug)]
pub struct LinkSelector {
    retry: Duration,
    nodes: Mutex<HashMap<String, LinkState>>,
}

#[derive(Debug)]
struct LinkState {
    links: Vec<String>,
    /// 지금 쓰는 링크(`links`의 위치)
    active: usize,
    /// 지금 링크의 마지막 성공(그 링크로 넘어온 때부터 센다)
    last_ok: Instant,
    /// 지금 링크에서 마지막 성공 뒤 연속으로 넘긴 제한 시간
    timeouts: u32,
    /// 지금 링크로 넘어온 때, 또는 마지막 시험 호출을 보낸 때
    switched_at: Instant,
    /// 시험 호출이 우선 링크로 가 있다.
    trial: bool,
}

impl LinkState {
    fn new(links: &[String]) -> Self {
        let now = Instant::now();
        Self {
            links: links.to_vec(),
            active: 0,
            last_ok: now,
            timeouts: 0,
            switched_at: now,
            trial: false,
        }
    }
}

impl Default for LinkSelector {
    fn default() -> Self {
        Self::new(LINK_RETRY)
    }
}

impl LinkSelector {
    /// 우선 링크를 `retry`마다 다시 시도한다(시험은 짧게 준다).
    pub fn new(retry: Duration) -> Self {
        Self {
            retry,
            nodes: Mutex::new(HashMap::new()),
        }
    }

    /// 첫 링크가 `addr`이고 링크가 `links`인 노드로 보낼 호출 하나의 링크. 때가 되면 우선 링크로 보내는 시험
    /// 호출이다.
    pub fn pick(&self, addr: &str, links: &[String]) -> Pick {
        self.choose(addr, links, true)
    }

    /// [`pick`](Self::pick)과 같지만 시험 호출로 고르지 않는다(늘 지금 링크). 오래 걸리거나 큰 호출에 쓴다.
    pub fn current(&self, addr: &str, links: &[String]) -> Pick {
        self.choose(addr, links, false)
    }

    fn choose(&self, addr: &str, links: &[String], may_trial: bool) -> Pick {
        let mut nodes = self.nodes.lock().unwrap();
        let sent = Instant::now();
        if links.len() < 2 {
            nodes.remove(addr);
            return Pick {
                link: links.first().map_or(addr, String::as_str).to_string(),
                trial: false,
                sent,
            };
        }
        let state = nodes
            .entry(addr.to_string())
            .or_insert_with(|| LinkState::new(links));
        // 링크 목록이 바뀌었으면 처음부터 다시 센다.
        if state.links != links {
            *state = LinkState::new(links);
        }
        // 시험 호출을 보낸 뒤 결과 없이 `retry`가 지나면(버려진 호출) 다시 보낸다.
        if may_trial && state.active != 0 && state.switched_at.elapsed() >= self.retry {
            state.trial = true;
            state.switched_at = sent;
            return Pick {
                link: state.links[0].clone(),
                trial: true,
                sent,
            };
        }
        Pick {
            link: state.links[state.active].clone(),
            trial: false,
            sent,
        }
    }

    /// `sent`에 `link`로 보낸 호출에 상대가 답했다. 우선 링크면 그리로 돌아간다. 다만 지금 링크로 넘어오기(또는 마지막
    /// 시험 호출) 전에 보낸 호출의 늦은 성공으로는 돌아가지 않는다(그 뒤에 죽었을 수 있다).
    pub fn ok(&self, addr: &str, link: &str, sent: Instant) {
        let mut nodes = self.nodes.lock().unwrap();
        let Some(state) = nodes.get_mut(addr) else {
            return;
        };
        if state.active != 0 && link == state.links[0] {
            if sent < state.switched_at {
                return;
            }
            tracing::info!(node = addr, link, "back to the preferred cluster link");
            state.active = 0;
            state.trial = false;
            state.last_ok = Instant::now();
            state.timeouts = 0;
        } else if link == state.links[state.active] {
            state.last_ok = Instant::now();
            state.timeouts = 0;
        }
    }

    /// `link`로 보낸 호출이 `failure`로 실패했다. 지금 링크가 죽었다고 보면 다음 링크로 넘어간다.
    pub fn failed(&self, addr: &str, link: &str, failure: Failure) {
        let mut nodes = self.nodes.lock().unwrap();
        let Some(state) = nodes.get_mut(addr) else {
            return;
        };
        let active = state.links[state.active].clone();
        // 시험 호출이 실패했다. 지금 링크에 머물고 `retry`를 다시 잰다.
        if state.trial && state.active != 0 && link == state.links[0] {
            tracing::debug!(
                node = addr,
                link,
                ?failure,
                "the preferred cluster link is still down"
            );
            state.trial = false;
            state.switched_at = Instant::now();
            return;
        }
        // 넘어가기 전에 보낸 호출의 늦은 실패
        if link != active {
            return;
        }
        if failure == Failure::Timeout {
            state.timeouts += 1;
            if state.timeouts < TIMEOUTS_TO_SWITCH && state.last_ok.elapsed() < LINK_DEAD_AFTER {
                return;
            }
        }
        state.active = (state.active + 1) % state.links.len();
        let now = Instant::now();
        state.last_ok = now;
        state.timeouts = 0;
        state.switched_at = now;
        state.trial = false;
        tracing::warn!(
            node = addr,
            from = %active,
            to = %state.links[state.active],
            ?failure,
            "cluster link failed; switching to the next link"
        );
    }

    /// 첫 링크가 `addr`인 노드에 지금 쓰는 링크. 링크가 하나인(상태가 없는) 노드는 `None`이다.
    pub fn active(&self, addr: &str) -> Option<String> {
        let nodes = self.nodes.lock().unwrap();
        nodes.get(addr).map(|s| s.links[s.active].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links() -> Vec<String> {
        vec!["a".into(), "b".into(), "c".into()]
    }

    /// 고른 링크와 시험 호출인지
    fn chosen(p: Pick) -> (String, bool) {
        (p.link, p.trial)
    }

    fn pick(s: &LinkSelector) -> (String, bool) {
        chosen(s.pick("a", &links()))
    }

    fn on(link: &str, trial: bool) -> (String, bool) {
        (link.into(), trial)
    }

    /// 지금 보낸 호출의 성공
    fn ok(s: &LinkSelector, link: &str) {
        s.ok("a", link, Instant::now());
    }

    #[test]
    fn the_book_lists_only_nodes_with_several_links() {
        let book = LinkBook::default();
        book.replace(&[
            OrbNode::new(["a", "b"]),
            OrbNode::single("c"),
            OrbNode::default(),
        ]);
        assert_eq!(book.links("a"), vec!["a", "b"]);
        assert_eq!(book.links("c"), vec!["c"]);
        assert_eq!(book.links("x"), vec!["x"]);
        assert_eq!(book.links.read().unwrap().len(), 1);
        book.replace(&[]);
        assert_eq!(book.links("a"), vec!["a"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_single_link_keeps_no_state() {
        let s = LinkSelector::default();
        assert_eq!(chosen(s.pick("a", &["a".to_string()])), on("a", false));
        assert_eq!(chosen(s.pick("a", &[])), on("a", false));
        s.failed("a", "a", Failure::Connect);
        assert_eq!(s.active("a"), None);
        // 링크가 하나로 줄면 상태를 버린다.
        pick(&s);
        s.failed("a", "a", Failure::Connect);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        assert_eq!(chosen(s.pick("a", &["a".to_string()])), on("a", false));
        assert_eq!(s.active("a"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_connect_failure_switches_at_once_and_all_failing_rotates() {
        let s = LinkSelector::default();
        assert_eq!(pick(&s), on("a", false));
        s.failed("a", "a", Failure::Connect);
        assert_eq!(pick(&s), on("b", false));
        // 넘어가기 전에 보낸 호출의 늦은 실패는 세지 않는다.
        s.failed("a", "a", Failure::Connect);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        s.failed("a", "b", Failure::Connect);
        assert_eq!(pick(&s), on("c", false));
        s.failed("a", "c", Failure::Connect);
        assert_eq!(pick(&s), on("a", false));
    }

    #[tokio::test(start_paused = true)]
    async fn a_timeout_switches_after_two_in_a_row_or_400_ms_after_the_last_success() {
        let s = LinkSelector::default();
        pick(&s);
        // 넘어온(처음 고른) 때부터 400 ms 안의 초과 한 번은 넘기지 않는다.
        tokio::time::advance(Duration::from_millis(300)).await;
        s.failed("a", "a", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("a"));
        // 그 사이 성공이 있으면 다시 센다.
        ok(&s, "a");
        tokio::time::advance(Duration::from_millis(300)).await;
        s.failed("a", "a", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("a"));
        // 연속 두 번이면 400 ms 전이어도 넘어간다.
        s.failed("a", "a", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        // 새 링크는 넘어온 때부터 센다: 400 ms가 지난 초과 한 번이면 넘어간다.
        s.failed("a", "b", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        ok(&s, "b");
        tokio::time::advance(Duration::from_millis(399)).await;
        s.failed("a", "b", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        ok(&s, "b");
        tokio::time::advance(Duration::from_millis(400)).await;
        s.failed("a", "b", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("c"));
    }

    /// 조용히 죽은 링크(하트비트가 200 ms마다 가고 200 ms에 끊김): 마지막 성공 뒤 첫 하트비트가 다음 링크로 가기까지
    /// 선거 제한 시간의 최솟값(1초)에서 하트비트 간격을 뺀 것보다 짧다.
    #[tokio::test(start_paused = true)]
    async fn a_silent_link_is_left_before_an_election() {
        let s = LinkSelector::default();
        pick(&s);
        ok(&s, "a");
        let last_ok = Instant::now();
        loop {
            // 200 ms마다 하트비트를 보내고, 200 ms에 제한 시간을 넘긴다.
            tokio::time::advance(Duration::from_millis(200)).await;
            let (link, _) = pick(&s);
            if link != "a" {
                break;
            }
            tokio::time::advance(Duration::from_millis(200)).await;
            s.failed("a", &link, Failure::Timeout);
        }
        assert!(
            last_ok.elapsed() < Duration::from_millis(1000 - 200),
            "{:?}",
            last_ok.elapsed()
        );
    }

    /// 넘어가기 전에 보낸 호출이 우선 링크에서 늦게 성공해도 돌아가지 않는다. 넘어간 뒤 보낸 시험 호출의 성공만
    /// 돌아가게 한다.
    #[tokio::test(start_paused = true)]
    async fn a_late_success_from_before_the_switch_does_not_switch_back() {
        let s = LinkSelector::new(Duration::from_secs(30));
        let early = s.pick("a", &links());
        tokio::time::advance(Duration::from_millis(10)).await;
        s.failed("a", "a", Failure::Connect);
        s.ok("a", &early.link, early.sent);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        tokio::time::advance(Duration::from_secs(30)).await;
        let trial = s.pick("a", &links());
        assert!(trial.trial);
        // 시험 호출 전에 보낸 것(버려진 옛 시험 호출 등)도 세지 않는다.
        s.ok("a", "a", early.sent);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        s.ok("a", &trial.link, trial.sent);
        assert_eq!(s.active("a").as_deref(), Some("a"));
    }

    /// 시험 호출로 고르지 않는 호출은 재시도 간격이 지나도 지금 링크로 간다.
    #[tokio::test(start_paused = true)]
    async fn current_never_tries_the_preferred_link() {
        let s = LinkSelector::new(Duration::from_secs(30));
        pick(&s);
        s.failed("a", "a", Failure::Connect);
        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(chosen(s.current("a", &links())), on("b", false));
        assert_eq!(pick(&s), on("a", true));
    }

    #[tokio::test(start_paused = true)]
    async fn one_call_tries_the_preferred_link_after_the_retry_interval() {
        let s = LinkSelector::new(Duration::from_secs(30));
        pick(&s);
        s.failed("a", "a", Failure::Connect);
        tokio::time::advance(Duration::from_secs(29)).await;
        assert_eq!(pick(&s), on("b", false));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(pick(&s), on("a", true));
        // 시험 중인 동안 다른 호출은 지금 링크로 간다.
        assert_eq!(pick(&s), on("b", false));
        assert_eq!(pick(&s), on("b", false));

        // 시험이 실패하면 머물고 30초를 다시 잰다.
        s.failed("a", "a", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        tokio::time::advance(Duration::from_secs(29)).await;
        assert_eq!(pick(&s), on("b", false));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(pick(&s), on("a", true));
        // 결과 없이 버려진 시험은 30초 뒤 다시 보낸다.
        tokio::time::advance(Duration::from_secs(30)).await;
        assert_eq!(pick(&s), on("a", true));

        // 시험이 성공하면 돌아간다.
        ok(&s, "a");
        assert_eq!(s.active("a").as_deref(), Some("a"));
        assert_eq!(pick(&s), on("a", false));
        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(pick(&s), on("a", false));
    }

    #[tokio::test(start_paused = true)]
    async fn new_links_start_over() {
        let s = LinkSelector::default();
        pick(&s);
        s.failed("a", "a", Failure::Connect);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        let fewer = vec!["a".to_string(), "c".to_string()];
        assert_eq!(chosen(s.pick("a", &fewer)), on("a", false));
    }
}
