//! Orbivisor 명령줄 클라이언트. `oplctl` 바이너리와 E2E 테스트가 함께 쓴다.

mod alerts;
mod ceph;
mod cli;
mod client;
mod config;
mod console;
#[cfg(test)]
mod fake_api;
mod firmware;
mod media;
mod metrics;
mod output;
mod template;

pub use cli::run;
pub use client::Client;
