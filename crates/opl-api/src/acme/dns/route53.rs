//! Amazon Route 53(SigV4 — `opl-store` `backend::sigv4`): 영역 찾기 `hostedzonesbyname`, `ChangeResourceRecordSets`
//! UPSERT/DELETE, `GetChange`가 `INSYNC`가 될 때까지.

use std::time::Duration;

use opl_store::backend::sigv4::{self, SignInput};

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

/// 바꾸기 본문(값마다 레코드 하나 — 와일드카드와 기본 이름이 같은 TXT 이름을 함께 쓴다)
pub fn change_body(action: &str, fqdn: &str, values: &[String]) -> String {
    let records: String = values
        .iter()
        .map(|v| {
            format!(
                "<ResourceRecord><Value>{}</Value></ResourceRecord>",
                escape(&format!("\"{v}\""))
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ChangeResourceRecordSetsRequest xmlns="https://route53.amazonaws.com/doc/{API}/"><ChangeBatch><Changes><Change><Action>{action}</Action><ResourceRecordSet><Name>{}.</Name><Type>TXT</Type><TTL>60</TTL><ResourceRecords>{records}</ResourceRecords></ResourceRecordSet></Change></Changes></ChangeBatch></ChangeResourceRecordSetsRequest>"#,
        escape(fqdn.trim_end_matches('.'))
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

    async fn change(
        &self,
        zone: &str,
        action: &str,
        fqdn: &str,
        values: &[String],
    ) -> Result<(), String> {
        let xml = self
            .send(
                "POST",
                &format!("/{API}/hostedzone/{zone}/rrset"),
                &[],
                change_body(action, fqdn, values),
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
        self.change(&zone, "UPSERT", fqdn, values).await?;
        Ok(zone)
    }

    pub async fn remove(&self, zone: &str, fqdn: &str, values: &[String]) -> Result<(), String> {
        self.change(zone, "DELETE", fqdn, values).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_body_quotes_values() {
        let b = change_body(
            "UPSERT",
            "_acme-challenge.a.example.com",
            &["v1".into(), "v2".into()],
        );
        assert!(b.contains("<Action>UPSERT</Action>"));
        assert!(b.contains("<Name>_acme-challenge.a.example.com.</Name>"));
        assert!(b.contains("<Value>&quot;v1&quot;</Value></ResourceRecord><ResourceRecord><Value>&quot;v2&quot;</Value>"));
        assert_eq!(
            xml_text("<a><Id>/change/C1</Id></a>", "Id"),
            Some("/change/C1")
        );
    }
}
