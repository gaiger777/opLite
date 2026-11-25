//! PowerDNS 권한 서버 HTTP API: 영역 목록에서 가장 긴 접미사를 고르고, 지금 TXT rrset을 읽어(`GET …/zones/{zone}`)
//! 우리 값만 더하거나 뺀 것을 `PATCH /api/v1/servers/{id}/zones/{zone}`로 쓴다(`REPLACE` — 다 비면 `DELETE`). 다른 TXT
//! 값(다른 주문·SPF 등)은 그대로 둔다. 읽기와 쓰기 사이는 원자적이지 않다(PowerDNS API에 조건부 쓰기가 없다).

use serde_json::{Value, json};

use super::{http, read_json};

pub struct PowerDns {
    pub url: String,
    pub api_key: String,
    pub server_id: String,
}

impl PowerDns {
    fn base(&self) -> String {
        format!(
            "{}/api/v1/servers/{}",
            self.url.trim_end_matches('/'),
            self.server_id
        )
    }

    async fn zone(&self, fqdn: &str, zones: &[String]) -> Result<String, String> {
        let list: Vec<String> = if zones.is_empty() {
            let v = read_json(
                http()?
                    .get(format!("{}/zones", self.base()))
                    .header("X-API-Key", &self.api_key),
            )
            .await?;
            v.as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|z| z["name"].as_str().map(super::query::normalize))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            zones.iter().map(|z| super::query::normalize(z)).collect()
        };
        super::longest_zone(fqdn, &list)
            .map(|z| format!("{z}."))
            .ok_or_else(|| format!("PowerDNS: no zone holds {fqdn}"))
    }

    async fn patch(&self, zone: &str, rrset: Value) -> Result<(), String> {
        let response = http()?
            .patch(format!("{}/zones/{zone}", self.base()))
            .header("X-API-Key", &self.api_key)
            .json(&json!({ "rrsets": [rrset] }))
            .send()
            .await
            .map_err(|e| format!("PowerDNS: {}", crate::telemetry::chain(&e.without_url())))?;
        let status = response.status();
        if !status.is_success() {
            let body: Value = response.json().await.unwrap_or(Value::Null);
            return Err(format!(
                "PowerDNS: HTTP {status}: {}",
                body["error"].as_str().unwrap_or("request failed")
            ));
        }
        Ok(())
    }

    /// 지금 `fqdn`의 TXT rrset(ttl, 레코드들 — PowerDNS 그대로)
    async fn current(&self, zone: &str, fqdn: &str) -> Result<Option<(u64, Vec<Value>)>, String> {
        let name = format!("{}.", fqdn.trim_end_matches('.'));
        let v = read_json(
            http()?
                .get(format!("{}/zones/{zone}", self.base()))
                .query(&[("rrset_name", name.as_str()), ("rrset_type", "TXT")])
                .header("X-API-Key", &self.api_key),
        )
        .await?;
        Ok(find_rrset(&v, &name))
    }

    pub async fn add(
        &self,
        fqdn: &str,
        values: &[String],
        zones: &[String],
    ) -> Result<String, String> {
        let zone = self.zone(fqdn, zones).await?;
        let name = format!("{}.", fqdn.trim_end_matches('.'));
        let (ttl, mut records) = self.current(&zone, fqdn).await?.unwrap_or((60, Vec::new()));
        for c in quoted(values) {
            if !records.iter().any(|r| r["content"] == c.as_str()) {
                records.push(json!({ "content": c, "disabled": false }));
            }
        }
        self.patch(
            &zone,
            json!({ "name": name, "type": "TXT", "ttl": ttl, "changetype": "REPLACE", "records": records }),
        )
        .await?;
        Ok(zone)
    }

    pub async fn remove(&self, zone: &str, fqdn: &str, values: &[String]) -> Result<(), String> {
        let name = format!("{}.", fqdn.trim_end_matches('.'));
        let Some((ttl, records)) = self.current(zone, fqdn).await? else {
            return Ok(());
        };
        let ours = quoted(values);
        let kept: Vec<Value> = records
            .iter()
            .filter(|r| !ours.iter().any(|c| r["content"] == c.as_str()))
            .cloned()
            .collect();
        if kept.len() == records.len() {
            return Ok(());
        }
        let rrset = if kept.is_empty() {
            json!({ "name": name, "type": "TXT", "changetype": "DELETE" })
        } else {
            json!({ "name": name, "type": "TXT", "ttl": ttl, "changetype": "REPLACE", "records": kept })
        };
        self.patch(zone, rrset).await
    }
}

fn quoted(values: &[String]) -> Vec<String> {
    values.iter().map(|v| format!("\"{v}\"")).collect()
}

/// 영역 JSON에서 `name`(끝 점 포함)의 TXT rrset
pub fn find_rrset(zone: &Value, name: &str) -> Option<(u64, Vec<Value>)> {
    zone["rrsets"].as_array()?.iter().find_map(|r| {
        (r["type"] == "TXT"
            && r["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name)))
        .then(|| {
            (
                r["ttl"].as_u64().unwrap_or(60),
                r["records"].as_array().cloned().unwrap_or_default(),
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_txt_rrset() {
        let zone = json!({ "rrsets": [
            { "name": "_acme-challenge.a.example.com.", "type": "A", "ttl": 1, "records": [] },
            { "name": "_ACME-challenge.a.example.com.", "type": "TXT", "ttl": 300,
              "records": [{ "content": "\"keep\"", "disabled": false }] }
        ]});
        let (ttl, records) = find_rrset(&zone, "_acme-challenge.a.example.com.").unwrap();
        assert_eq!(ttl, 300);
        assert_eq!(records[0]["content"], "\"keep\"");
        assert!(find_rrset(&zone, "_acme-challenge.b.example.com.").is_none());
        assert!(find_rrset(&json!({}), "x.").is_none());
    }
}
