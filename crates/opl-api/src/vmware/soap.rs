//! vSphere SOAP(vim25) 봉투를 손으로 쓰고 읽는다(설계 §7.2 — WSDL 코드 생성 없음). 쓰기는 이스케이프·
//! `ManagedObjectReference`·`xsi:type`을 넣는 작은 도움 함수, 읽기는 `roxmltree`로 푼 뒤 가진 나무([`Node`])로
//! 옮긴다. 요소는 로컬 이름으로, `xsi:type`은 네임스페이스 URI로 찾는다 — govmomi(vcsim)는 접두어를
//! `_XMLSchema-instance:type`처럼 쓰고 실제 vCenter는 `xsi:type`을 쓴다(둘 다 같은 URI).

use std::fmt::Write as _;

/// vim25 네임스페이스
pub const NS_VIM25: &str = "urn:vim25";
pub const NS_SOAPENV: &str = "http://schemas.xmlsoap.org/soap/envelope/";
pub const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
pub const NS_XSD: &str = "http://www.w3.org/2001/XMLSchema";

/// XML 글자 이스케이프(요소 내용·속성 값 둘 다)
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0에 쓸 수 없는 제어 문자는 뺀다(탭·줄바꿈은 둔다).
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// `<name>text</name>`
pub fn el(name: &str, text: &str) -> String {
    format!("<{name}>{}</{name}>", escape(text))
}

/// 관리 객체 참조(`<x type="VirtualMachine">vm-42</x>`)
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MoRef {
    pub kind: String,
    pub value: String,
}

impl MoRef {
    pub fn new(kind: &str, value: &str) -> Self {
        Self {
            kind: kind.to_string(),
            value: value.to_string(),
        }
    }

    /// `<tag type="Kind">value</tag>`
    pub fn xml(&self, tag: &str) -> String {
        format!(
            "<{tag} type=\"{}\">{}</{tag}>",
            escape(&self.kind),
            escape(&self.value)
        )
    }
}

impl std::fmt::Display for MoRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.kind, self.value)
    }
}

/// 메서드 호출 본문(`<Method xmlns="urn:vim25"><_this …/>{args}</Method>`)
pub fn call_body(method: &str, this: &MoRef, args: &str) -> String {
    format!(
        "<{method} xmlns=\"{NS_VIM25}\">{}{args}</{method}>",
        this.xml("_this")
    )
}

/// 본문을 SOAP 봉투로 감싼다. `xsi`·`xsd` 접두어를 선언한다(인자의 `xsi:type="xsd:string"`용).
pub fn envelope(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <soapenv:Envelope xmlns:soapenv=\"{NS_SOAPENV}\" xmlns:xsd=\"{NS_XSD}\" xmlns:xsi=\"{NS_XSI}\">\
         <soapenv:Body>{body}</soapenv:Body></soapenv:Envelope>"
    )
}

/// 가진 XML 요소. 이름은 로컬 이름, `xsi_type`은 값 그대로(`xsd:string`·`VirtualDisk`), `mo_type`은 접두어 없는
/// `type` 속성(관리 객체 참조의 종류), `text`는 바로 아래 글자를 이은 것이다.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Node {
    pub name: String,
    pub xsi_type: Option<String>,
    pub mo_type: Option<String>,
    pub text: String,
    pub children: Vec<Node>,
}

impl Node {
    fn from_xml(node: roxmltree::Node<'_, '_>) -> Node {
        let mut out = Node {
            name: node.tag_name().name().to_string(),
            xsi_type: node.attribute((NS_XSI, "type")).map(str::to_string),
            // 접두어 없는 `type`만(roxmltree는 `xsi:type`도 로컬 이름 `type`으로 찾는다)
            mo_type: node
                .attributes()
                .find(|a| a.name() == "type" && a.namespace().is_none())
                .map(|a| a.value().to_string()),
            ..Node::default()
        };
        for child in node.children() {
            if child.is_element() {
                out.children.push(Node::from_xml(child));
            } else if child.is_text()
                && let Some(t) = child.text()
            {
                out.text.push_str(t);
            }
        }
        out
    }

    /// `xsi:type`의 로컬 이름(`xsd:string` → `string`)
    pub fn ty(&self) -> Option<&str> {
        self.xsi_type
            .as_deref()
            .map(|t| t.rsplit_once(':').map_or(t, |(_, local)| local))
    }

    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// 경로를 따라 내려간 요소(각 이름의 첫 요소)
    pub fn get(&self, path: &[&str]) -> Option<&Node> {
        path.iter().try_fold(self, |n, name| n.child(name))
    }

    /// 경로의 글자(앞뒤 공백 뗌)
    pub fn text_at(&self, path: &[&str]) -> Option<&str> {
        self.get(path).map(|n| n.text.trim())
    }

    pub fn string_at(&self, path: &[&str]) -> Option<String> {
        self.text_at(path).map(str::to_string)
    }

    pub fn parse_at<T: std::str::FromStr>(&self, path: &[&str]) -> Option<T> {
        self.text_at(path).and_then(|t| t.parse().ok())
    }

    pub fn bool_at(&self, path: &[&str]) -> Option<bool> {
        match self.text_at(path)? {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        }
    }

    /// 이 요소를 관리 객체 참조로 본다(`type` 속성이 있어야 한다).
    pub fn moref(&self) -> Option<MoRef> {
        Some(MoRef {
            kind: self.mo_type.clone()?,
            value: self.text.trim().to_string(),
        })
    }

    pub fn moref_at(&self, path: &[&str]) -> Option<MoRef> {
        self.get(path).and_then(Node::moref)
    }

    /// 이 요소를 XML로 다시 쓴다(`xsi:type`·`type` 속성 포함). 받은 장치를 고쳐 `ReconfigVM`에 되돌려 보낼 때 쓴다.
    pub fn to_xml(&self) -> String {
        let mut out = String::new();
        self.write_xml(&mut out);
        out
    }

    fn write_xml(&self, out: &mut String) {
        let _ = write!(out, "<{}", self.name);
        if let Some(t) = &self.mo_type {
            let _ = write!(out, " type=\"{}\"", escape(t));
        }
        if let Some(t) = &self.xsi_type {
            let _ = write!(out, " xsi:type=\"{}\"", escape(t));
        }
        out.push('>');
        if self.children.is_empty() {
            out.push_str(&escape(&self.text));
        } else {
            for c in &self.children {
                c.write_xml(out);
            }
        }
        let _ = write!(out, "</{}>", self.name);
    }

    /// 경로의 요소를 찾아 고친다(없으면 None).
    pub fn get_mut(&mut self, path: &[&str]) -> Option<&mut Node> {
        let mut node = self;
        for name in path {
            node = node.children.iter_mut().find(|c| c.name == *name)?;
        }
        Some(node)
    }
}

/// SOAP 결함(`soapenv:Fault`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    /// `detail` 첫 요소의 `xsi:type` 로컬 이름(`InvalidLogin`), 없으면 요소 이름에서 `Fault`를 뗀 것, 그것도
    /// 없으면 `faultcode`
    pub kind: String,
    /// `faultstring`
    pub message: String,
    /// `detail` 첫 요소(결함 객체)
    pub detail: Option<Node>,
}

impl Fault {
    /// 결함 객체(`MethodFault`)에서 — Task의 `info.error.fault`처럼 봉투 밖에서 온 것
    pub fn from_method_fault(fault: &Node, message: &str) -> Fault {
        let kind = fault
            .ty()
            .map(str::to_string)
            .unwrap_or_else(|| fault.name.trim_end_matches("Fault").to_string());
        Fault {
            kind,
            message: message.to_string(),
            detail: Some(fault.clone()),
        }
    }
}

/// 응답을 읽은 결과
#[derive(Debug)]
pub enum Reply {
    /// `{Method}Response` 요소(그 아래 `returnval`이 있거나 없음)
    Ok(Node),
    Fault(Fault),
}

/// 응답 봉투를 푼다. `Body`의 첫 요소가 `Fault`면 결함, 아니면 그 요소다.
pub fn parse_reply(xml: &str) -> Result<Reply, String> {
    let options = roxmltree::ParsingOptions {
        allow_dtd: false,
        ..roxmltree::ParsingOptions::default()
    };
    let doc = roxmltree::Document::parse_with_options(xml, options)
        .map_err(|e| format!("invalid SOAP response: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "Envelope" {
        return Err("invalid SOAP response: no Envelope".into());
    }
    let body = root
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "Body")
        .ok_or("invalid SOAP response: no Body")?;
    let first = body
        .children()
        .find(roxmltree::Node::is_element)
        .ok_or("invalid SOAP response: empty Body")?;
    let first = Node::from_xml(first);
    if first.name != "Fault" {
        return Ok(Reply::Ok(first));
    }
    let message = first.string_at(&["faultstring"]).unwrap_or_default();
    let detail = first
        .child("detail")
        .and_then(|d| d.children.first())
        .cloned();
    let kind = match &detail {
        Some(d) => d
            .ty()
            .map(str::to_string)
            .unwrap_or_else(|| d.name.trim_end_matches("Fault").to_string()),
        None => first
            .string_at(&["faultcode"])
            .unwrap_or_else(|| "Fault".into()),
    };
    Ok(Reply::Fault(Fault {
        kind,
        message,
        detail,
    }))
}

/// 요청 봉투에서 메서드 이름·`_this`·인자 요소를 푼다(가짜 vSphere·시험이 쓴다).
pub fn parse_call(xml: &str) -> Result<(String, Option<MoRef>, Node), String> {
    match parse_reply(xml)? {
        Reply::Ok(node) => {
            let this = node.moref_at(&["_this"]);
            Ok((node.name.clone(), this, node))
        }
        Reply::Fault(_) => Err("a request cannot be a fault".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup_and_drops_control_characters() {
        assert_eq!(
            escape("a<b>&\"c'\u{1}\td"),
            "a&lt;b&gt;&amp;&quot;c&apos;\td"
        );
        assert_eq!(el("name", "web & db"), "<name>web &amp; db</name>");
        assert_eq!(
            MoRef::new("VirtualMachine", "vm-42").xml("_this"),
            "<_this type=\"VirtualMachine\">vm-42</_this>"
        );
    }

    #[test]
    fn call_bodies_match_govmomi() {
        // govc가 보내는 Login 본문과 같은 꼴(vcsim -trace로 잡은 것, 봉투 밖)
        let body = call_body(
            "Login",
            &MoRef::new("SessionManager", "SessionManager"),
            &format!("{}{}", el("userName", "user"), el("password", "pass")),
        );
        assert_eq!(
            body,
            "<Login xmlns=\"urn:vim25\"><_this type=\"SessionManager\">SessionManager</_this>\
             <userName>user</userName><password>pass</password></Login>"
        );
        let env = envelope("<X/>");
        assert!(env.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?><soapenv:Envelope"));
        assert!(env.ends_with("<soapenv:Body><X/></soapenv:Body></soapenv:Envelope>"));
    }

    #[test]
    fn reads_xsi_type_by_namespace_whatever_the_prefix() {
        let govmomi = r#"<?xml version="1.0" encoding="UTF-8"?>
<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><soapenv:Body><R xmlns="urn:vim25"><returnval><val xmlns:_XMLSchema-instance="http://www.w3.org/2001/XMLSchema-instance" _XMLSchema-instance:type="xsd:string">a</val><val xsi:type="ArrayOfVirtualDevice"/><val type="Folder">group-d1</val></returnval></R></soapenv:Body></soapenv:Envelope>"#;
        let Reply::Ok(r) = parse_reply(govmomi).unwrap() else {
            panic!("fault")
        };
        let vals: Vec<&Node> = r
            .child("returnval")
            .unwrap()
            .children_named("val")
            .collect();
        assert_eq!(vals[0].ty(), Some("string"));
        assert_eq!(vals[0].xsi_type.as_deref(), Some("xsd:string"));
        assert_eq!(vals[1].ty(), Some("ArrayOfVirtualDevice"));
        assert_eq!(vals[2].moref(), Some(MoRef::new("Folder", "group-d1")));
        assert_eq!(vals[2].ty(), None);
    }

    #[test]
    fn faults_take_the_detail_type() {
        // vcsim의 InvalidLogin 결함 그대로
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<soapenv:Envelope xmlns:soapenc="http://schemas.xmlsoap.org/soap/encoding/" xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><soapenv:Body><soapenv:Fault><faultcode>ServerFaultCode</faultcode><faultstring>Login failure</faultstring><detail><InvalidLoginFault xmlns="urn:vim25" xsi:type="InvalidLogin"></InvalidLoginFault></detail></soapenv:Fault></soapenv:Body></soapenv:Envelope>"#;
        let Reply::Fault(f) = parse_reply(xml).unwrap() else {
            panic!("not a fault")
        };
        assert_eq!(f.kind, "InvalidLogin");
        assert_eq!(f.message, "Login failure");
        // detail이 없으면 faultcode
        let bare = r#"<Envelope xmlns="http://schemas.xmlsoap.org/soap/envelope/"><Body><Fault><faultcode>ServerFaultCode</faultcode><faultstring>boom</faultstring></Fault></Body></Envelope>"#;
        let Reply::Fault(f) = parse_reply(bare).unwrap() else {
            panic!("not a fault")
        };
        assert_eq!(
            (f.kind.as_str(), f.message.as_str()),
            ("ServerFaultCode", "boom")
        );
        // xsi:type 없는 detail은 요소 이름에서
        let untyped = r#"<Envelope xmlns="http://schemas.xmlsoap.org/soap/envelope/"><Body><Fault><faultstring>x</faultstring><detail><NotAuthenticatedFault xmlns="urn:vim25"/></detail></Fault></Body></Envelope>"#;
        let Reply::Fault(f) = parse_reply(untyped).unwrap() else {
            panic!("not a fault")
        };
        assert_eq!(f.kind, "NotAuthenticated");
    }

    #[test]
    fn rejects_non_soap_and_dtds() {
        assert!(parse_reply("<html><body>502</body></html>").is_err());
        assert!(parse_reply("not xml").is_err());
        let dtd = r#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY a "aaaa">]><Envelope xmlns="http://schemas.xmlsoap.org/soap/envelope/"><Body><R>&a;</R></Body></Envelope>"#;
        assert!(parse_reply(dtd).is_err());
        assert!(
            parse_reply(
                r#"<Envelope xmlns="http://schemas.xmlsoap.org/soap/envelope/"><Body/></Envelope>"#
            )
            .is_err()
        );
    }

    #[test]
    fn nodes_round_trip_to_xml() {
        let xml = r#"<Envelope xmlns="http://schemas.xmlsoap.org/soap/envelope/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><Body><VirtualDevice xmlns="urn:vim25" xsi:type="VirtualVmxnet3"><key>4000</key><backing xsi:type="VirtualEthernetCardNetworkBackingInfo"><deviceName>VM &amp; Network</deviceName><network type="Network">network-7</network></backing><connectable><startConnected>true</startConnected></connectable></VirtualDevice></Body></Envelope>"#;
        let Reply::Ok(mut dev) = parse_reply(xml).unwrap() else {
            panic!("fault")
        };
        dev.get_mut(&["connectable", "startConnected"])
            .unwrap()
            .text = "false".into();
        assert_eq!(
            dev.to_xml(),
            "<VirtualDevice xsi:type=\"VirtualVmxnet3\"><key>4000</key><backing xsi:type=\"VirtualEthernetCardNetworkBackingInfo\">\
             <deviceName>VM &amp; Network</deviceName><network type=\"Network\">network-7</network></backing>\
             <connectable><startConnected>false</startConnected></connectable></VirtualDevice>"
        );
        assert_eq!(dev.bool_at(&["connectable", "startConnected"]), Some(false));
        assert_eq!(dev.parse_at::<i32>(&["key"]), Some(4000));
        assert_eq!(
            dev.moref_at(&["backing", "network"]),
            Some(MoRef::new("Network", "network-7"))
        );
    }

    #[test]
    fn parses_calls() {
        let (method, this, node) = parse_call(&envelope(&call_body(
            "PowerOnVM_Task",
            &MoRef::new("VirtualMachine", "vm-1"),
            "",
        )))
        .unwrap();
        assert_eq!(method, "PowerOnVM_Task");
        assert_eq!(this, Some(MoRef::new("VirtualMachine", "vm-1")));
        assert_eq!(node.children.len(), 1);
    }
}
