//! 내장 이미지 카탈로그(계획 58 `orbivisor_types::image_catalog::BUILTIN`)의 링크 확인: 항목마다 이미지 URL이 HEAD(안 되면
//! 첫 바이트 GET)에 답하고, 체크섬 파일을 받아 이미지 파일 이름의 줄이 있는지 본다. 이미지는 받지 않는다. 바깥 서버에
//! 닿아야 하므로 기능 `catalog-it`로만 돌린다(CI `catalog.yml`, 주 1회).
//! `cargo test -p orbivisorctl --features catalog-it --test catalog -- --nocapture`
#![cfg(feature = "catalog-it")]

use std::time::Duration;

use orbivisor_types::image_catalog::{Checksum, builtin, file_name_of, parse_checksums};
use reqwest::StatusCode;
use reqwest::header::RANGE;

/// 한 요청을 다시 해 보는 횟수(바깥 서버의 일시 오류)
const TRIES: usize = 3;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("orbivisor-catalog-check")
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap()
}

/// URL이 있는지: HEAD가 성공하거나(리디렉션을 따라감), HEAD를 받지 않는 서버면 첫 바이트 GET이 성공한다.
async fn reachable(c: &reqwest::Client, url: &str) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..TRIES {
        match c.head(url).send().await {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) if r.status() == StatusCode::NOT_FOUND => return Err(format!("{url}: 404")),
            Ok(r) => last = format!("{url}: HEAD {}", r.status()),
            Err(e) => last = format!("{url}: {e}"),
        }
        match c.get(url).header(RANGE, "bytes=0-0").send().await {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) => last = format!("{last}; GET {}", r.status()),
            Err(e) => last = format!("{last}; GET {e}"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    Err(last)
}

/// 체크섬 파일의 내용
async fn text(c: &reqwest::Client, url: &str) -> Result<String, String> {
    let mut last = String::new();
    for _ in 0..TRIES {
        match c.get(url).send().await {
            Ok(r) if r.status().is_success() => {
                return r.text().await.map_err(|e| format!("{url}: {e}"));
            }
            Ok(r) => last = format!("{url}: {}", r.status()),
            Err(e) => last = format!("{url}: {e}"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    Err(last)
}

#[tokio::test]
async fn builtin_catalog_links_answer() {
    let c = client();
    let mut failures = Vec::new();
    for image in builtin() {
        let what = format!("{} ({:?})", image.id, image.arch);
        if let Err(e) = reachable(&c, &image.url).await {
            failures.push(format!("{what}: image {e}"));
            continue;
        }
        if let Checksum::Sums { url } = &image.checksum {
            let name = file_name_of(&image.url).expect("built-in URLs have a file name");
            match text(&c, url).await {
                Ok(body) if parse_checksums(&body, &name).is_some() => {
                    eprintln!("{what}: ok ({name} in {url})")
                }
                Ok(_) => failures.push(format!("{what}: {url} has no entry for {name}")),
                Err(e) => failures.push(format!("{what}: checksum file {e}")),
            }
        } else {
            eprintln!("{what}: ok (fixed hash)");
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
