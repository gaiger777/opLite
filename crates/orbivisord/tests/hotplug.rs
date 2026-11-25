//! 실제 QEMU로 실행 중인 VM의 하드웨어를 CLI로 바꾼다(계획 54): 디스크·NIC 붙이기와 떼기, 다음 재시작 때 적용할
//! 변경(보류), 끄고 켜면 보류가 적용되는지, vCPU 핫플러그. 게스트에 OS가 없으므로(UEFI 펌웨어만) 게스트가 내려
//! 줘야 끝나는 떼기는 aarch64 `virt`(QEMU가 곧바로 뗌)에서만 성공하고, x86_64 `q35`(ACPI 핫플러그 — 게스트가
//! 받아야 함)는 시간 제한(`[test] hotplug_timeout_secs`)을 넘어 실패하고 장치가 `unplugging`에 남는지 본다. vCPU 핫플러그는
//! x86_64 노드만 하고 aarch64는 다음 재시작 때다.
//! `cargo test -p orbivisord --features qemu-it --test hotplug`
#![cfg(feature = "qemu-it")]

mod common;

use common::Daemon;
use serde_json::Value;

/// `vm get`의 실행 중 하드웨어(`status.hardware`): (켜진 vCPU, vCPU 상한, 메모리 MiB, 쓰는 핫플러그 포트 수)
fn hardware(vm: &Value) -> (u64, u64, u64, usize) {
    let hw = &vm["status"]["hardware"];
    let layout = &hw["layout"];
    let boot = hw["cpu"]["sockets"].as_u64().unwrap_or(1) * hw["cpu"]["cores"].as_u64().unwrap();
    let used = |key: &str| layout[key].as_array().map_or(0, Vec::len);
    (
        layout["vcpus"].as_u64().unwrap_or(boot),
        hw["cpu"]["max_vcpus"].as_u64().unwrap_or(boot),
        hw["memory_mib"].as_u64().unwrap() + layout["plugged_mib"].as_u64().unwrap_or(0),
        used("disk_ports") + used("nic_ports"),
    )
}

/// 다음 재시작 때 적용할 변경의 (field, reason)
fn pending(vm: &Value) -> Vec<(String, String)> {
    vm["status"]["hardware"]["pending"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|p| {
                    let text = |k: &str| p[k].as_str().unwrap().to_string();
                    (text("field"), text("reason"))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn hardware_changes_of_a_running_vm_through_the_cli() {
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new(
        "node1",
        "[reconciler]\nacpi_timeout_secs = 5\n[test]\nhotplug_timeout_secs = 5\n",
    );
    d.start().await;
    d.init("hotplug").await;
    d.login().await;

    let pool = d.path("pool");
    std::fs::create_dir(&pool).unwrap();
    d.ctl(&["pool", "create", "local", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    d.wait_pool_ready("local").await;
    for (name, size) in [("root", "64M"), ("data", "16M")] {
        d.ctl(&["volume", "create", name, "--pool", "local", "--size", size])
            .await
            .unwrap();
    }
    let out = d
        .ctl(&[
            "vm", "create", "web", "--memory", "256", "--disk", "root", "--start",
        ])
        .await
        .unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(hardware(&vm), (1, 1, 256, 0), "{vm}");
    assert_eq!(vm["status"]["hardware"]["layout"]["ports"], 8, "{vm}");
    assert!(pending(&vm).is_empty(), "{vm}");

    // 끝에 붙이는 디스크·NIC는 바로 붙는다(빈 루트 포트).
    let out = d.ctl(&["vm", "disk", "add", "web", "data"]).await.unwrap();
    assert_eq!(
        out,
        "vm web: disk data added\nvm web: applied while running\n"
    );
    let out = d.ctl(&["vm", "nic", "add", "web", "user"]).await.unwrap();
    assert_eq!(
        out,
        "vm web: NIC on network user added\nvm web: applied while running\n"
    );
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(vm["spec"]["disks"].as_array().unwrap().len(), 2, "{vm}");
    assert_eq!(vm["spec"]["nics"].as_array().unwrap().len(), 1, "{vm}");
    let layout = &vm["status"]["hardware"]["layout"];
    assert_eq!(layout["disk_ports"], serde_json::json!([0]), "{vm}");
    assert_eq!(layout["nic_ports"], serde_json::json!([1]), "{vm}");
    assert_eq!(hardware(&vm), (1, 1, 256, 2), "{vm}");

    // 실행 중 붙인 것은 뗄 수 있다. aarch64 `virt`는 QEMU가 곧바로 떼고, x86_64 `q35`는 게스트가 받아야 해서 시간
    // 제한을 넘어 실패한다 — 스펙은 뗀 그대로이고 장치는 게스트가 내려 주기를 기다린다(`unplugging`).
    let removed = d.ctl(&["vm", "nic", "remove", "web", "0"]).await;
    let removed_disk = d.ctl(&["vm", "disk", "remove", "web", "data"]).await;
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    if cfg!(target_arch = "aarch64") {
        assert_eq!(
            removed.unwrap(),
            "vm web: NIC 0 removed\nvm web: applied while running\n"
        );
        assert_eq!(
            removed_disk.unwrap(),
            "vm web: disk data removed\nvm web: applied while running\n"
        );
        assert_eq!(vm["spec"]["disks"].as_array().unwrap().len(), 1, "{vm}");
        assert_eq!(vm["spec"]["nics"], Value::Array(Vec::new()), "{vm}");
        assert_eq!(
            vm["status"]["hardware"]["layout"]["disk_ports"],
            Value::Null,
            "{vm}"
        );
    } else {
        let err = removed.unwrap_err();
        assert!(err.to_string().contains("did not release"), "{err}");
        // 게스트가 NIC를 내려 주지 않는 동안 디스크·NIC 목록을 바꾸는 PUT은 409 `vm_busy`다(CLI가 다시 해 보다 그만둔다).
        let err = removed_disk.unwrap_err();
        assert!(
            err.to_string()
                .contains("the guest is still releasing NIC 0 on network \"user\""),
            "{err}"
        );
        // NIC는 뗀 그대로이고 디스크는 스펙에 남는다.
        assert_eq!(vm["spec"]["nics"], Value::Array(Vec::new()), "{vm}");
        assert_eq!(vm["spec"]["disks"].as_array().unwrap().len(), 2, "{vm}");
        assert_eq!(
            vm["status"]["hardware"]["unplugging"],
            serde_json::json!(["nic0"]),
            "{vm}"
        );
        // 게스트 OS가 없어 내려 주지 않으므로 다시 켜서 장치를 정리한다.
        d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
        d.ctl(&["vm", "start", "web"]).await.unwrap();
        let vm = d.ctl_json(&["vm", "get", "web"]).await;
        assert_eq!(hardware(&vm), (1, 1, 256, 0), "{vm}");
        assert_eq!(vm["status"]["hardware"]["unplugging"], Value::Null, "{vm}");
    }

    // 상한 없이 켠 VM의 메모리는 다음 재시작 때다.
    let out = d
        .ctl(&["vm", "set", "web", "--memory", "512"])
        .await
        .unwrap();
    assert!(
        out.starts_with("vm web: hardware updated\nvm web: pending until restart:\n"),
        "{out}"
    );
    assert!(out.contains("  memory_mib: 256 -> 512 (limit"), "{out}");
    // vCPU 상한은 켤 때 정한다. CPU 핫플러그가 없는 노드(aarch64)에서는 상한이 없으니 보류도 없다.
    let out = d
        .ctl(&["vm", "set", "web", "--max-vcpus", "4"])
        .await
        .unwrap();
    assert_eq!(
        out.contains("  cpu.max_vcpus: 1 -> 4 (restart)"),
        cfg!(target_arch = "x86_64"),
        "{out}"
    );
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert!(
        pending(&vm).contains(&("memory_mib".into(), "limit".into())),
        "{vm}"
    );

    // 끄고 켜면 보류가 적용된다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "start", "web"]).await.unwrap();
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    let max = if cfg!(target_arch = "x86_64") { 4 } else { 1 };
    assert_eq!(hardware(&vm), (1, max, 512, 0), "{vm}");
    assert!(pending(&vm).is_empty(), "{vm}");

    // vCPU는 x86_64 노드에서 상한 안의 `cores`로 바로 늘고, aarch64는 다음 재시작 때다.
    let out = d.ctl(&["vm", "set", "web", "--cores", "2"]).await.unwrap();
    if cfg!(target_arch = "x86_64") {
        assert_eq!(
            out,
            "vm web: hardware updated\nvm web: applied while running\n"
        );
        let vm = d.ctl_json(&["vm", "get", "web"]).await;
        assert_eq!(hardware(&vm).0, 2, "{vm}");
        assert_eq!(hardware(&vm).1, 4, "{vm}");
    } else {
        assert!(out.contains("  vcpus: 1 -> 2 (unsupported"), "{out}");
    }

    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    for name in ["root", "data"] {
        d.ctl(&["volume", "delete", name]).await.unwrap();
    }
    d.stop();
}
