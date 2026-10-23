//! 웹 UI 빌드 결과(`web/dist`)의 절대 경로를 `OPL_WEB_DIST`로 넘긴다. rust-embed는 디버그 빌드에서 이 경로의
//! 파일을 실행 중에 읽는데, `..`가 섞인 경로를 받으면 컴파일 뒤에 생긴 폴더의 파일을 찾지 못한다.

fn main() {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest
        .parent()
        .and_then(|crates| crates.parent())
        .expect("opld lives at crates/opld in the workspace");
    println!(
        "cargo:rustc-env=OPL_WEB_DIST={}",
        workspace.join("web/dist").display()
    );
    println!("cargo:rerun-if-changed=build.rs");
}
