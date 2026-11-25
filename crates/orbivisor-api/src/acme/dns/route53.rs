//! Amazon Route 53(SigV4 — `orbivisor-store` `backend::sigv4`): 영역 찾기 `hostedzonesbyname`, 지금 TXT rrset 읽기
//! `ListResourceRecordSets`, 우리 값만 더하고 빼는 `ChangeResourceRecordSets`(있던 rrset을 그대로 DELETE하고 고친 것을
//! CREATE — 그 사이 다른 쪽이 바꿨으면 Route 53이 거절하므로 다시 읽는다), `GetChange`가 `INSYNC`가 될 때까지. 다른
//! 주문·다른 ACME 클라이언트·CNAME으로 따라간 이름의 다른 TXT 값(SPF 등)은 그대로 둔다.

use std::time::Duration;

use orbivisor_store::backend::sigv4::{self, SignInput};

use super::{candidates, http};

const API: &str = "2013-04-01";

pub struct Route53 {
    pub base: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub hosted_zone_id: Option<String>,
    pub region: String,
}

/// XML 안 첫 `<tag>…</tag>`
fn xml_text<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&format!("</{tag}>"))?;
    Some(&xml[start..end])
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// TXT rrset 하나(값은 Route 53 그대로 — 따옴표 포함)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSet {
    pub ttl: u32,
    pub values: Vec<String>,
}

/// 우리 값(따옴표로 싼 것)
fn quoted(values: &[String]) -> Vec<String> {
    values.iter().map(|v| format!("\"{v}\"")).collect()
}

/// `ListResourceRecordSets` 응답에서 `fqdn`의 TXT rrset(첫 것이 그 이름·TXT가 아니면 없음)
pub fn parse_rrset(xml: &str, fqdn: &str) -> Option<RecordSet> {
    let set = xml_text(xml, "ResourceRecordSet")?;
    let name = xml_text(set, "Name")?;
    if !name
        .trim_end_matches('.')
        .eq_ignore_ascii_case(fqdn.trim_end_matches('.'))
        || xml_text(set, "Type") != Some("TXT")
    {
        return None;
    }
    let ttl = xml_text(set, "TTL")
        .and_then(|t| t.parse().ok())
        .unwrap_or(60);
    let mut values = Vec::new();
    let mut rest = set;
    while let Some(v) = xml_text(rest, "Value") {
        values.push(unescape(v));
        let end = rest
            .find("</Value>")
            .map_or(rest.len(), |i| i + "</Value>".len());
        rest = &rest[end..];
    }
    Some(RecordSet { ttl, values })
}

/// 넣기: 있던 것에 우리 값을 더한 것(이미 다 있으면 None)
pub fn with_values(old: Option<&RecordSet>, values: &[String]) -> Option<RecordSet> {
    let mut set = old.cloned().unwrap_or(RecordSet {
        ttl: 60,
        values: Vec::new(),
    });
    let before = set.values.len();
    for v in quoted(values) {
        if !set.values.contains(&v) {
            set.values.push(v);
        }
    }
    (set.values.len() != before || old.is_none()).then_some(set)
}

/// 지우기: 있던 것에서 우리 값을 뺀 것. 우리 값이 없으면 `Unchanged`, 다 비면 `Empty`.
#[derive(Debug, PartialEq, Eq)]
pub enum Removal {
    Unchanged,
    Empty,
    Keep(RecordSet),
}

pub fn without_values(old: Option<&RecordSet>, values: &[String]) -> Removal {
    let Some(old) = old else {
        return Removal::Unchanged;
    };
    let ours = quoted(values);
    let kept: Vec<String> = old
        .values
        .iter()
        .filter(|v| !ours.contains(v))
        .cloned()
        .collect();
    if kept.len() == old.values.len() {
        Removal::Unchanged
    } else if kept.is_empty() {
        Removal::Empty
    } else {
        Removal::Keep(RecordSet {
            ttl: old.ttl,
            values: kept,
        })
    }
}

fn change_xml(action: &str, fqdn: &str, set: &RecordSet) -> String {
    let records: String = set
        .values
        .iter()
        .map(|v| {
            format!(
                "<ResourceRecord><Value>{}</Value></ResourceRecord>",
                escape(v)
            )
        })
        .collect();
    format!(
        "<Change><Action>{action}</Action><ResourceRecordSet><Name>{}.</Name><Type>TXT</Type><TTL>{}</TTL><ResourceRecords>{records}</ResourceRecords></ResourceRecordSet></Change>",
        escape(fqdn.trim_end_matches('.')),
        set.ttl
    )
}

/// 바꾸기 본문: 바꿀 것들((동작, rrset) — 차례대로 한 묶음, 하나라도 맞지 않으면 모두 거절된다)
pub fn change_body(fqdn: &str, changes: &[(&str, &RecordSet)]) -> String {
    let changes: String = changes
        .iter()
        .map(|(action, set)| change_xml(action, fqdn, set))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ChangeResourceRecordSetsRequest xmlns="https://route53.amazonaws.com/doc/{API}/"><ChangeBatch><Changes>{changes}</Changes></ChangeBatch></ChangeResourceRecordSetsRequest>"#
    )
}

impl Route53 {
    /// SigV4로 서명해 보낸다. (상태, 본문)
    async fn send(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: String,
    ) -> Result<String, String> {
        let url: reqwest::Url = format!("{}{path}", self.base)
            .parse()
            .map_err(|e| format!("Route 53 URL: {e}"))?;
        let host = match url.port() {
            Some(p) => format!("{}:{p}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_string(),
        };
        let amz_date = sigv4::amz_date(crate::backups::now());
        let payload_hash = sigv4::sha256_hex(body.as_bytes());
        let headers = vec![
            ("host".to_string(), host),
            ("x-amz-date".to_string(), amz_date.clone()),
        ];
        let canonical_uri = sigv4::uri_encode(url.path(), false);
        let auth = sigv4::authorization(
            &SignInput {
                method,
                canonical_uri: &canonical_uri,
                query,
                headers: &headers,
                payload_hash: &payload_hash,
                amz_date: &amz_date,
                region: &self.region,
                service: "route53",
            },
            &self.access_key_id,
            &self.secret_access_key,
        );
        let client = http()?;
        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        let response = client
            .request(method, url)
            .query(query)
            .header("x-amz-date", amz_date)
            .header(reqwest::header::AUTHORIZATION, auth)
            .body(body)
            .send()
            .await
            .map_err(|e| format!("Route 53: {}", crate::telemetry::chain(&e.without_url())))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| format!("Route 53: {}", crate::telemetry::chain(&e.without_url())))?;
        if !status.is_success() {
            let msg = xml_text(&text, "Message").unwrap_or("request failed");
            return Err(format!("Route 53: HTTP {status}: {msg}"));
        }
        Ok(text)
    }

    async fn zone(&self, fqdn: &str, zones: &[String]) -> Result<String, String> {
        if let Some(z) = &self.hosted_zone_id {
            return Ok(z.trim_start_matches("/hostedzone/").to_string());
        }
        for name in candidates(fqdn, zones) {
            let q = vec![
                ("dnsname".to_string(), name.clone()),
                ("maxitems".to_string(), "1".to_string()),
            ];
            let xml = self
                .send(
                    "GET",
                    &format!("/{API}/hostedzonesbyname"),
                    &q,
                    String::new(),
                )
                .await?;
            if let (Some(id), Some(n)) = (xml_text(&xml, "Id"), xml_text(&xml, "Name"))
                && n.trim_end_matches('.').eq_ignore_ascii_case(&name)
            {
                return Ok(id.trim_start_matches("/hostedzone/").to_string());
            }
        }
        Err(format!("Route 53: no hosted zone holds {fqdn}"))
    }

    /// 지금 `fqdn`의 TXT rrset
    async fn current(&self, zone: &str, fqdn: &str) -> Result<Option<RecordSet>, String> {
        let q = vec![
            (
                "name".to_string(),
                format!("{}.", fqdn.trim_end_matches('.')),
            ),
            ("type".to_string(), "TXT".to_string()),
            ("maxitems".to_string(), "1".to_string()),
        ];
        let xml = self
            .send(
                "GET",
                &format!("/{API}/hostedzone/{zone}/rrset"),
                &q,
                String::new(),
            )
            .await?;
        Ok(parse_rrset(&xml, fqdn))
    }

    /// 읽고 고쳐 쓴다(`next`가 바꿀 것들을 정한다 — 없으면 할 일 없음). 그 사이 다른 쪽이 바꿔 거절되면 세 번까지 다시.
    async fn modify(
        &self,
        zone: &str,
        fqdn: &str,
        next: impl Fn(Option<&RecordSet>) -> Vec<(&'static str, RecordSet)>,
    ) -> Result<(), String> {
        let mut last = String::new();
        for _ in 0..3 {
            let old = self.current(zone, fqdn).await?;
            let changes = next(old.as_ref());
            if changes.is_empty() {
                return Ok(());
            }
            let refs: Vec<(&str, &RecordSet)> = changes.iter().map(|(a, s)| (*a, s)).collect();
            match self.change(zone, change_body(fqdn, &refs)).await {
                Ok(()) => return Ok(()),
                Err(e) if e.contains("HTTP 400") => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    async fn change(&self, zone: &str, body: String) -> Result<(), String> {
        let xml = self
            .send(
                "POST",
                &format!("/{API}/hostedzone/{zone}/rrset"),
                &[],
                body,
            )
            .await?;
        let Some(change) =
            xml_text(&xml, "Id").map(|i| i.trim_start_matches("/change/").to_string())
        else {
            return Ok(());
        };
        // 바뀜이 모든 권한 서버에 퍼질 때까지(보통 1분 안)
        for _ in 0..60 {
            let xml = self
                .send(
                    "GET",
                    &format!("/{API}/change/{change}"),
                    &[],
                    String::new(),
                )
                .await?;
            if xml_text(&xml, "Status") == Some("INSYNC") {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(format!("Route 53: change {change} did not become INSYNC"))
    }

    pub async fn add(
        &self,
        fqdn: &str,
        values: &[String],
        zones: &[String],
    ) -> Result<String, String> {
        let zone = self.zone(fqdn, zones).await?;
        self.modify(&zone, fqdn, |old| match (old, with_values(old, values)) {
            (_, None) => Vec::new(),
            (None, Some(new)) => vec![("CREATE", new)],
            (Some(old), Some(new)) => vec![("DELETE", old.clone()), ("CREATE", new)],
        })
        .await?;
        Ok(zone)
    }

    pub async fn remove(&self, zone: &str, fqdn: &str, values: &[String]) -> Result<(), String> {
        self.modify(zone, fqdn, |old| match (old, without_values(old, values)) {
            (Some(old), Removal::Empty) => vec![("DELETE", old.clone())],
            (Some(old), Removal::Keep(new)) => vec![("DELETE", old.clone()), ("CREATE", new)],
            _ => Vec::new(),
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_body_quotes_values() {
        let set = with_values(None, &["v1".into(), "v2".into()]).unwrap();
        let b = change_body("_acme-challenge.a.example.com", &[("CREATE", &set)]);
        assert!(b.contains("<Action>CREATE</Action>"));
        assert!(b.contains("<Name>_acme-challenge.a.example.com.</Name>"));
        assert!(b.contains("<Value>&quot;v1&quot;</Value></ResourceRecord><ResourceRecord><Value>&quot;v2&quot;</Value>"));
        assert_eq!(
            xml_text("<a><Id>/change/C1</Id></a>", "Id"),
            Some("/change/C1")
        );
    }

    /// 리뷰 1: 다른 TXT 값(다른 주문·SPF)은 남기고 우리 값만 더하고 뺀다.
    #[test]
    fn merges_only_our_values() {
        let xml = r#"<ListResourceRecordSetsResponse><ResourceRecordSets><ResourceRecordSet><Name>_acme-challenge.a.example.com.</Name><Type>TXT</Type><TTL>300</TTL><ResourceRecords><ResourceRecord><Value>&quot;keep&quot;</Value></ResourceRecord><ResourceRecord><Value>"v=spf1 -all"</Value></ResourceRecord></ResourceRecords></ResourceRecordSet></ResourceRecordSets></ListResourceRecordSetsResponse>"#;
        let old = parse_rrset(xml, "_acme-challenge.a.example.com").unwrap();
        assert_eq!(old.ttl, 300);
        assert_eq!(old.values, ["\"keep\"", "\"v=spf1 -all\""]);
        // 다른 이름의 rrset(목록은 그 이름 뒤의 것을 준다)은 없는 것
        assert_eq!(parse_rrset(xml, "_acme-challenge.b.example.com"), None);
        let ours = vec!["v1".to_string()];
        let added = with_values(Some(&old), &ours).unwrap();
        assert_eq!(added.values, ["\"keep\"", "\"v=spf1 -all\"", "\"v1\""]);
        assert_eq!(added.ttl, 300);
        assert_eq!(with_values(Some(&added), &ours), None);
        assert_eq!(
            without_values(Some(&added), &ours),
            Removal::Keep(old.clone())
        );
        assert_eq!(without_values(Some(&old), &ours), Removal::Unchanged);
        assert_eq!(without_values(None, &ours), Removal::Unchanged);
        let only = with_values(None, &ours).unwrap();
        assert_eq!(without_values(Some(&only), &ours), Removal::Empty);
        let body = change_body(
            "_acme-challenge.a.example.com",
            &[("DELETE", &old), ("CREATE", &added)],
        );
        assert!(body.contains("<Action>DELETE</Action><ResourceRecordSet><Name>_acme-challenge.a.example.com.</Name><Type>TXT</Type><TTL>300</TTL>"));
        assert!(body.contains("&quot;v=spf1 -all&quot;"));
    }
}
