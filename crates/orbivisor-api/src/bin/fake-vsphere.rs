//! 개발·시험용 가짜 vSphere(계획 72 설계 §7.5). 떠서 연결 정보 한 줄(JSON)을 표준 출력에 쓰고 Ctrl-C까지 돈다.
//! svc01·브라우저 시험이 원본 연결을 더할 때 쓴다.
//!
//! `cargo run -p orbivisor-api --features fake-vsphere --bin fake-vsphere -- --port 8989`

use clap::Parser;
use orbivisor_api::vmware::fake::{FakeConfig, FakeVsphere};

#[derive(Parser)]
#[command(about = "Fake vCenter/ESXi for Orbivisor VMware migration tests")]
struct Args {
    /// 받을 포트(127.0.0.1, 0이면 빈 포트)
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// 받을 주소
    #[arg(long, default_value = "127.0.0.1")]
    address: std::net::IpAddr,
    #[arg(long)]
    user: Option<String>,
    #[arg(long)]
    password: Option<String>,
    /// 단독 ESXi로 흉내 낸다
    #[arg(long)]
    esxi: bool,
    /// 빈 디스크 VM을 이만큼 만든다(없으면 기본 VM 둘)
    #[arg(long)]
    vms: Option<usize>,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args = Args::parse();
    let mut config = match args.vms {
        Some(n) => FakeConfig::many(n),
        None => FakeConfig::default(),
    };
    config.esxi = args.esxi;
    config.listen = Some(std::net::SocketAddr::new(args.address, args.port));
    if let Some(u) = args.user {
        config.user = u;
    }
    if let Some(p) = args.password {
        config.password = p;
    }
    let (user, password) = (config.user.clone(), config.password.clone());
    let fake = FakeVsphere::start(config).await;
    println!(
        "{}",
        serde_json::json!({
            "host": fake.addr.ip().to_string(),
            "port": fake.addr.port(),
            "user": user,
            "password": password,
            "fingerprint": fake.fingerprint,
            "thumbprint_sha1": fake.thumbprint_sha1,
        })
    );
    let _ = tokio::signal::ctrl_c().await;
}
