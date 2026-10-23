//! API 문서(OpenAPI JSON)를 표준 출력으로 낸다. 웹 UI가 이것으로 TypeScript 타입을 만든다
//! (`pnpm -C web gen:api`).

fn main() {
    let doc = opl_api::openapi()
        .to_pretty_json()
        .expect("serialize the OpenAPI document");
    println!("{doc}");
}
