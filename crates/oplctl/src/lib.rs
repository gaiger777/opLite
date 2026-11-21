//! Orbivisor 명령줄 클라이언트. `oplctl` 바이너리와 E2E 테스트가 함께 쓴다.

mod access;
mod alerts;
mod ceph;
mod cli;
mod client;
mod config;
mod console;
mod cpu;
#[cfg(test)]
mod fake_api;
mod firmware;
mod lb;
mod media;
mod metrics;
mod migration;
mod numa;
mod output;
mod routing;
mod sdn;
mod template;
mod two_factor;

pub use cli::run;
pub use client::Client;
