//! Cloudflare DNS API v4: 영역 찾기 `GET /zones?name=`, TXT 만들기 `POST /zones/{id}/dns_records`, 지우기 `DELETE`.

use serde_json::{Value, json};

use super::{candidates, http, read_json};

pub struct Cloudflare {
    pub base: String,
    pub token: String,
    pub zone_id: Option<String>,
}

fn errors(v: &Value) -> String {
    v["errors"]
        .as_array()
        .map(|e| {
            e.iter()
                .filter_map(|x| x["message"].as_str())
                .collect::<Vec<_>>()
                .join("; ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "request failed".into())
}

impl Cloudflare {
    async fn call(&self, req: reqwest::RequestBuilder) -> Result<Value, String> {
        let v = read_json(req.bearer_auth(&self.token)).await?;
        if v["success"].as_bool() != Some(true) {
            return Err(format!("Cloudflare: {}", errors(&v)));
        }
        Ok(v)
    }

    async fn zone(&self, fqdn: &str, zones: &[String]) -> Result<String, String> {
        if let Some(z) = &self.zone_id {
            return Ok(z.clone());
        }
        let client = http()?;
        for name in candidates(fqdn, zones) {
            let v = self
                .call(
                    client
                        .get(format!("{}/zones", self.base))
                        .query(&[("name", name.as_str())]),
                )
                .await?;
            if let Some(id) = v["result"][0]["id"].as_str() {
                return Ok(id.to_string());
            }
        }
        Err(format!("Cloudflare: no zone holds {fqdn}"))
    }

    /// 값마다 TXT 하나. (영역 id, 레코드 id들)
    pub async fn add(
        &self,
        fqdn: &str,
        values: &[String],
        zones: &[String],
    ) -> Result<(String, Vec<String>), String> {
        let zone = self.zone(fqdn, zones).await?;
        let client = http()?;
        let mut ids = Vec::new();
        for value in values {
            let v = self
                .call(
                    client
                        .post(format!("{}/zones/{zone}/dns_records", self.base))
                        .json(&json!({ "type": "TXT", "name": fqdn, "content": value, "ttl": 60 })),
                )
                .await;
            match v {
                Ok(v) => ids.push(v["result"]["id"].as_str().unwrap_or_default().to_string()),
                Err(e) => {
                    let _ = self.remove(&zone, &ids).await;
                    return Err(e);
                }
            }
        }
        Ok((zone, ids))
    }

    pub async fn remove(&self, zone: &str, ids: &[String]) -> Result<(), String> {
        let client = http()?;
        for id in ids.iter().filter(|i| !i.is_empty()) {
            self.call(client.delete(format!("{}/zones/{zone}/dns_records/{id}", self.base)))
                .await?;
        }
        Ok(())
    }
}
