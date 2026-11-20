//! NIC QoS(계획 67 §8): NIC마다 보내기·받기 속도 제한(폴리싱 — 넘으면 버림)과 DSCP 표시. SDN NIC는 리더 `OvnSync`가
//! OVN `QoS` 행([`nic_rows`])으로, 단순 NIC는 노드가 tap의 tc([`tc_commands`], 속도만)로 집행한다. 두 세계가 같은
//! 폴리싱이라 옮기기(단순 → SDN) 앞뒤의 같은 `qos` 칸이 같은 뜻이다.

use serde::{Deserialize, Serialize};

/// 속도의 상한(Mbit/s)
pub const MAX_MBPS: u32 = 100_000;
/// 버스트의 상한(kbit)
pub const MAX_BURST_KBIT: u32 = 10_000_000;
/// DSCP의 상한(6비트)
pub const MAX_DSCP: u8 = 63;
/// OVN `QoS` 행의 우선순위(NIC마다 match가 달라 겹치지 않는다)
pub const QOS_PRIORITY: u16 = 2000;
/// Orbivisor가 tap에 거는 tc 필터의 우선순위. 다른 우선순위의 필터는 건드리지 않는다.
pub const TC_PREF: u32 = 49152;

/// NIC 하나의 QoS(`NicSpec.qos`). 보내기·받기는 VM 쪽에서 본 방향이다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NicQos {
    /// VM이 보내는 속도 상한(Mbit/s, 1~100000)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub out_mbps: Option<u32>,
    /// VM이 받는 속도 상한(Mbit/s, 1~100000)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub in_mbps: Option<u32>,
    /// 버스트(kbit, 1~10000000). 없으면 속도(kbit/s)의 80%(Neutron 기본과 같다).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub burst_kbit: Option<u32>,
    /// VM이 보내는 IP 패킷에 적는 DSCP(0~63). SDN NIC만 된다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub dscp: Option<u8>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum QosError {
    #[error("qos needs out_mbps, in_mbps or dscp (leave qos out for none)")]
    Empty,
    #[error("qos {0} {1} is out of range: use 1-{MAX_MBPS} Mbit/s")]
    Rate(&'static str, u32),
    #[error("qos burst_kbit {0} is out of range: use 1-{MAX_BURST_KBIT}")]
    Burst(u32),
    #[error("qos burst_kbit needs out_mbps or in_mbps")]
    BurstWithoutRate,
    #[error("qos dscp {0} is out of range: use 0-{MAX_DSCP}")]
    Dscp(u8),
}

impl NicQos {
    /// 스펙만으로 보는 검사. 단순 NIC의 DSCP(400 `qos_dscp_needs_sdn`)는 네트워크를 아는 API가 본다.
    pub fn validate(&self) -> Result<(), QosError> {
        if self.out_mbps.is_none() && self.in_mbps.is_none() && self.dscp.is_none() {
            return Err(QosError::Empty);
        }
        for (name, rate) in [("out_mbps", self.out_mbps), ("in_mbps", self.in_mbps)] {
            if let Some(r) = rate
                && !(1..=MAX_MBPS).contains(&r)
            {
                return Err(QosError::Rate(name, r));
            }
        }
        if let Some(b) = self.burst_kbit {
            if !(1..=MAX_BURST_KBIT).contains(&b) {
                return Err(QosError::Burst(b));
            }
            if self.out_mbps.is_none() && self.in_mbps.is_none() {
                return Err(QosError::BurstWithoutRate);
            }
        }
        if let Some(d) = self.dscp
            && d > MAX_DSCP
        {
            return Err(QosError::Dscp(d));
        }
        Ok(())
    }

    /// 속도 `mbps`에 쓰는 버스트(kbit): 준 값, 없으면 속도 kbit/s의 80%
    pub fn burst_for(&self, mbps: u32) -> u32 {
        self.burst_kbit
            .unwrap_or_else(|| (u64::from(mbps) * 800).min(u64::from(u32::MAX)) as u32)
    }

    /// 속도 제한이 있다(단순 NIC의 tc가 할 일이 있다).
    pub fn has_rate(&self) -> bool {
        self.out_mbps.is_some() || self.in_mbps.is_some()
    }
}

/// OVN `QoS` 행의 방향(포트 쪽에서 본 것 — `from-lport`는 VM이 보냄)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QosDirection {
    FromLport,
    ToLport,
}

impl QosDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            QosDirection::FromLport => "from-lport",
            QosDirection::ToLport => "to-lport",
        }
    }
}

/// SDN NIC 하나의 OVN `QoS` 행(그 NIC 스위치의 `qos_rules`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QosRow {
    pub direction: QosDirection,
    pub priority: u16,
    /// OVN match(`inport == "<LSP>"` 따위)
    pub r#match: String,
    /// `bandwidth.rate`(kbit/s)
    pub rate_kbps: Option<u32>,
    /// `bandwidth.burst`(kbit)
    pub burst_kbit: Option<u32>,
    /// `action.dscp`
    pub dscp: Option<u8>,
}

/// SDN NIC(논리 포트 `port`)의 OVN `QoS` 행(최대 둘, 방향 순): VM이 보냄(`out_mbps`·`dscp`)은 `from-lport`
/// `inport == "<LSP>"`(DSCP만이면 `&& ip`), VM이 받음(`in_mbps`)은 `to-lport` `outport == "<LSP>"`.
pub fn nic_rows(port: &str, qos: &NicQos) -> Vec<QosRow> {
    let mut rows = Vec::new();
    if qos.out_mbps.is_some() || qos.dscp.is_some() {
        let r#match = match qos.out_mbps {
            Some(_) => format!("inport == \"{port}\""),
            None => format!("inport == \"{port}\" && ip"),
        };
        rows.push(QosRow {
            direction: QosDirection::FromLport,
            priority: QOS_PRIORITY,
            r#match,
            rate_kbps: qos.out_mbps.map(kbps),
            burst_kbit: qos.out_mbps.map(|m| qos.burst_for(m)),
            dscp: qos.dscp,
        });
    }
    if let Some(m) = qos.in_mbps {
        rows.push(QosRow {
            direction: QosDirection::ToLport,
            priority: QOS_PRIORITY,
            r#match: format!("outport == \"{port}\""),
            rate_kbps: Some(kbps(m)),
            burst_kbit: Some(qos.burst_for(m)),
            dscp: None,
        });
    }
    rows
}

fn kbps(mbps: u32) -> u32 {
    (u64::from(mbps) * 1000).min(u64::from(u32::MAX)) as u32
}

/// tap 한쪽(tc `ingress` = VM이 보냄, `egress` = VM이 받음)의 Orbivisor 폴리싱 필터(우선순위 [`TC_PREF`])
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcPolice {
    /// Mbit/s
    pub rate_mbps: u32,
    /// kbit
    pub burst_kbit: u32,
}

/// tap에서 읽은 tc 상태(드라이버가 `tc -j qdisc show`·`tc -j filter show`로 읽는다)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TcState {
    /// `clsact` qdisc가 있다.
    pub clsact: bool,
    /// 우선순위 [`TC_PREF`]의 ingress 필터(VM이 보냄)
    pub ingress: Option<TcPolice>,
    /// 우선순위 [`TC_PREF`]의 egress 필터(VM이 받음)
    pub egress: Option<TcPolice>,
    /// 다른 우선순위의 필터가 있다(남의 것 — `clsact`를 지우지 않는다).
    pub foreign: bool,
}

/// 단순 NIC tap `tap`에 `qos`(None이면 없음)를 맞추는 tc 명령(읽은 상태 `actual`에서 빠진 것만, 차례대로). 필터는
/// matchall + police(`conform-exceed drop` — OVN과 같은 폴리싱)이고 바꿀 때는 지우고 다시 넣는다(matchall은 제자리
/// 바꾸기를 받지 않는다). 우리 필터가 모두 빠지고 남의 필터가 없으면 `clsact`를 지운다. DSCP는 보지 않는다(SDN NIC만).
pub fn tc_commands(tap: &str, qos: Option<&NicQos>, actual: &TcState) -> Vec<Vec<String>> {
    let want = |rate: Option<u32>| {
        let q = qos?;
        let m = rate?;
        Some(TcPolice {
            rate_mbps: m,
            burst_kbit: q.burst_for(m),
        })
    };
    let ingress = want(qos.and_then(|q| q.out_mbps));
    let egress = want(qos.and_then(|q| q.in_mbps));
    let cmd = |args: &[&str]| -> Vec<String> {
        std::iter::once("tc")
            .chain(args.iter().copied())
            .map(String::from)
            .collect()
    };
    let pref = TC_PREF.to_string();
    let mut out = Vec::new();
    if ingress.is_none() && egress.is_none() {
        if !actual.clsact {
            return out;
        }
        if !actual.foreign {
            out.push(cmd(&["qdisc", "del", "dev", tap, "clsact"]));
            return out;
        }
    } else if !actual.clsact {
        out.push(cmd(&["qdisc", "replace", "dev", tap, "clsact"]));
    }
    // clsact를 새로 만들면 필터가 없다.
    let have = |x: Option<TcPolice>| if actual.clsact { x } else { None };
    for (hook, want, have) in [
        ("ingress", ingress, have(actual.ingress)),
        ("egress", egress, have(actual.egress)),
    ] {
        if want == have {
            continue;
        }
        if have.is_some() {
            out.push(cmd(&["filter", "del", "dev", tap, hook, "pref", &pref]));
        }
        if let Some(p) = want {
            let rate = format!("{}mbit", p.rate_mbps);
            // tc의 크기 단위 `k`는 킬로바이트, `kbit`은 1024비트다. OVN(1000비트)과 같게 바이트로 적는다.
            let burst = (u64::from(p.burst_kbit) * 125).to_string();
            out.push(cmd(&[
                "filter",
                "add",
                "dev",
                tap,
                hook,
                "pref",
                &pref,
                "matchall",
                "action",
                "police",
                "rate",
                &rate,
                "burst",
                &burst,
                "conform-exceed",
                "drop",
            ]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(v: serde_json::Value) -> NicQos {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn storage_format_round_trips_and_validates() {
        let json =
            serde_json::json!({ "out_mbps": 100, "in_mbps": 200, "burst_kbit": 5000, "dscp": 46 });
        let qos = q(json.clone());
        assert_eq!(serde_json::to_value(qos).unwrap(), json);
        assert_eq!(qos.validate(), Ok(()));
        let only = q(serde_json::json!({ "dscp": 0 }));
        assert_eq!(serde_json::to_string(&only).unwrap(), r#"{"dscp":0}"#);
        assert_eq!(only.validate(), Ok(()));
        assert!(serde_json::from_value::<NicQos>(serde_json::json!({ "min_mbps": 1 })).is_err());
        for (v, err) in [
            (serde_json::json!({}), QosError::Empty),
            (serde_json::json!({ "burst_kbit": 10 }), QosError::Empty),
            (
                serde_json::json!({ "dscp": 1, "burst_kbit": 10 }),
                QosError::BurstWithoutRate,
            ),
            (
                serde_json::json!({ "out_mbps": 0 }),
                QosError::Rate("out_mbps", 0),
            ),
            (
                serde_json::json!({ "in_mbps": 100_001 }),
                QosError::Rate("in_mbps", 100_001),
            ),
            (
                serde_json::json!({ "out_mbps": 1, "burst_kbit": 0 }),
                QosError::Burst(0),
            ),
            (
                serde_json::json!({ "out_mbps": 1, "burst_kbit": 10_000_001 }),
                QosError::Burst(10_000_001),
            ),
            (serde_json::json!({ "dscp": 64 }), QosError::Dscp(64)),
        ] {
            assert_eq!(q(v.clone()).validate(), Err(err), "{v}");
        }
        // 버스트 기본은 속도 kbit/s의 80%
        assert_eq!(
            q(serde_json::json!({ "out_mbps": 100 })).burst_for(100),
            80_000
        );
        assert_eq!(qos.burst_for(100), 5000);
        assert_eq!(
            q(serde_json::json!({ "out_mbps": 100_000 })).burst_for(100_000),
            80_000_000
        );
    }

    #[test]
    fn nic_rows_follow_the_fields() {
        let port = "opl-x-525400000001";
        // DSCP만: from-lport, ip만
        let rows = nic_rows(port, &q(serde_json::json!({ "dscp": 46 })));
        assert_eq!(
            rows,
            vec![QosRow {
                direction: QosDirection::FromLport,
                priority: QOS_PRIORITY,
                r#match: format!("inport == \"{port}\" && ip"),
                rate_kbps: None,
                burst_kbit: None,
                dscp: Some(46),
            }]
        );
        // 속도만(보내기·받기)
        let rows = nic_rows(
            port,
            &q(serde_json::json!({ "out_mbps": 100, "in_mbps": 30 })),
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].direction.as_str(), "from-lport");
        assert_eq!(rows[0].r#match, format!("inport == \"{port}\""));
        assert_eq!(
            (rows[0].rate_kbps, rows[0].burst_kbit, rows[0].dscp),
            (Some(100_000), Some(80_000), None)
        );
        assert_eq!(rows[1].direction.as_str(), "to-lport");
        assert_eq!(rows[1].r#match, format!("outport == \"{port}\""));
        assert_eq!(
            (rows[1].rate_kbps, rows[1].burst_kbit),
            (Some(30_000), Some(24_000))
        );
        // 둘 다: 보내기 행 하나에 속도와 DSCP
        let rows = nic_rows(
            port,
            &q(serde_json::json!({ "out_mbps": 10, "burst_kbit": 999, "dscp": 10 })),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].rate_kbps, rows[0].burst_kbit, rows[0].dscp),
            (Some(10_000), Some(999), Some(10))
        );
        assert_eq!(rows[0].r#match, format!("inport == \"{port}\""));
    }

    fn line(cmds: &[Vec<String>]) -> Vec<String> {
        cmds.iter().map(|c| c.join(" ")).collect()
    }

    #[test]
    fn tc_commands_add_change_and_remove_only_what_differs() {
        let tap = "op0123456789n0";
        // QoS가 없고 아무것도 없으면 명령이 없다(QoS 없는 VM의 tap 명령은 그대로).
        assert!(tc_commands(tap, None, &TcState::default()).is_empty());
        // 없음 → 있음
        let qos = q(serde_json::json!({ "out_mbps": 100, "in_mbps": 50, "dscp": 46 }));
        assert_eq!(
            line(&tc_commands(tap, Some(&qos), &TcState::default())),
            vec![
                format!("tc qdisc replace dev {tap} clsact"),
                format!(
                    "tc filter add dev {tap} ingress pref 49152 matchall action police rate 100mbit burst 10000000 conform-exceed drop"
                ),
                format!(
                    "tc filter add dev {tap} egress pref 49152 matchall action police rate 50mbit burst 5000000 conform-exceed drop"
                ),
            ]
        );
        // 이미 맞으면 명령이 없다.
        let applied = TcState {
            clsact: true,
            ingress: Some(TcPolice {
                rate_mbps: 100,
                burst_kbit: 80_000,
            }),
            egress: Some(TcPolice {
                rate_mbps: 50,
                burst_kbit: 40_000,
            }),
            foreign: false,
        };
        assert!(tc_commands(tap, Some(&qos), &applied).is_empty());
        // 바꿈: 받기만 지우고 다시 넣는다.
        let changed = q(serde_json::json!({ "out_mbps": 100, "in_mbps": 20 }));
        assert_eq!(
            line(&tc_commands(tap, Some(&changed), &applied)),
            vec![
                format!("tc filter del dev {tap} egress pref 49152"),
                format!(
                    "tc filter add dev {tap} egress pref 49152 matchall action police rate 20mbit burst 2000000 conform-exceed drop"
                ),
            ]
        );
        // 한쪽 뺌
        let out_only = q(serde_json::json!({ "out_mbps": 100 }));
        assert_eq!(
            line(&tc_commands(tap, Some(&out_only), &applied)),
            vec![format!("tc filter del dev {tap} egress pref 49152")]
        );
        // 모두 뺌: clsact째 지운다.
        assert_eq!(
            line(&tc_commands(tap, None, &applied)),
            vec![format!("tc qdisc del dev {tap} clsact")]
        );
        // DSCP만인 qos는 tc에 할 일이 없다.
        assert_eq!(
            line(&tc_commands(
                tap,
                Some(&q(serde_json::json!({ "dscp": 1 }))),
                &applied
            )),
            vec![format!("tc qdisc del dev {tap} clsact")]
        );
        // 남의 필터가 있으면 clsact를 두고 우리 필터만 지운다.
        let shared = TcState {
            foreign: true,
            ..applied
        };
        assert_eq!(
            line(&tc_commands(tap, None, &shared)),
            vec![
                format!("tc filter del dev {tap} ingress pref 49152"),
                format!("tc filter del dev {tap} egress pref 49152"),
            ]
        );
        let foreign_only = TcState {
            clsact: true,
            foreign: true,
            ..TcState::default()
        };
        assert!(tc_commands(tap, None, &foreign_only).is_empty());
        assert_eq!(
            line(&tc_commands(tap, Some(&out_only), &foreign_only)),
            vec![format!(
                "tc filter add dev {tap} ingress pref 49152 matchall action police rate 100mbit burst 10000000 conform-exceed drop"
            )]
        );
    }
}
