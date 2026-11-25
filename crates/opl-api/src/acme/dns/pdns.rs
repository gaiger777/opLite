//! PowerDNS 권한 서버 HTTP API: 영역 목록에서 가장 긴 접미사를 고르고 `PATCH /api/v1/servers/{id}/zones/{zone}`로 rrset을
//! 바꾼다(`REPLACE`·`DELETE`).

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

    pub async fn add(
        &self,
        fqdn: &str,
        values: &[String],
        zones: &[String],
    ) -> Result<String, String> {
        let zone = self.zone(fqdn, zones).await?;
        let records: Vec<Value> = values
            .iter()
            .map(|v| json!({ "content": format!("\"{v}\""), "disabled": false }))
            .collect();
        self.patch(
            &zone,
            json!({ "name": format!("{}.", fqdn.trim_end_matches('.')), "type": "TXT", "ttl": 60,
                    "changetype": "REPLACE", "records": records }),
        )
        .await?;
        Ok(zone)
    }

    pub async fn remove(&self, zone: &str, fqdn: &str) -> Result<(), String> {
        self.patch(
            zone,
            json!({ "name": format!("{}.", fqdn.trim_end_matches('.')), "type": "TXT", "changetype": "DELETE" }),
        )
        .await
    }
}
