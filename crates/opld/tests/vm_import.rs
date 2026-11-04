//! 실제 opld로 VMware OVA를 가져온다: 시험이 만든 OVA(표식을 쓴 디스크의 streamOptimized VMDK, vSphere
//! 모양 OVF, SHA256 `.mf`)를 `oplctl import ova`로 올리고 제안을 본 뒤 `import run`으로 꺼진 VM과 볼륨을
//! 만든다. 볼륨을 `qemu-img convert`로 raw로 빼서 표식이 있는지 본다. `.mf` 해시가 틀린 OVA는 실행이
//! 실패하고 볼륨을 남기지 않는다. 가져온 VM은 켜지 않는다(게스트 OS가 없고, aarch64 macOS 노드는 x86
//! 게스트를 돌리지 못한다).
//! `cargo test -p opld --features qemu-it --test vm_import`
//! QEMU(`qemu-it`와 같다)와 qemu-img가 필요하다.
#![cfg(feature = "qemu-it")]

mod common;
#[allow(dead_code)]
#[path = "../../opl-api/tests/common/ustar.rs"]
mod ustar;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::Daemon;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ustar::Tar;

/// 디스크 앞에 써 두는 표식의 길이
const MARK: usize = 64 * 1024;
/// 원본 디스크 크기(4 MiB)
const DISK_BYTES: usize = 4 << 20;
const MAC: &str = "00:50:56:8a:12:34";

/// 클러스터를 만들고 로그인해 dir 풀 `local`과 네트워크 `lan`을 만든다. VM은 켜지 않으므로 브리지는 없어도
/// 된다.
async fn setup(d: &mut Daemon) {
    d.start().await;
    d.init("import").await;
    d.login().await;
    let dir = d.path("local");
    std::fs::create_dir(&dir).unwrap();
    d.ctl(&["pool", "create", "local", "--path", dir.to_str().unwrap()])
        .await
        .unwrap();
    d.wait_pool_ready("local").await;
    d.ctl(&["network", "create", "lan", "--bridge", "br0"])
        .await
        .unwrap();
}

fn qemu_img(args: &[&str]) {
    let out = Command::new("qemu-img").args(args).output().unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// 앞 `MARK` 바이트가 `mark`인 4 MiB 디스크의 streamOptimized VMDK(vSphere가 OVA에 넣는 형식)
fn vmdk(d: &Daemon, mark: u8) -> Vec<u8> {
    let raw = d.path("src.raw");
    let mut data = vec![0u8; DISK_BYTES];
    data[..MARK].fill(mark);
    std::fs::write(&raw, data).unwrap();
    let out = d.path("src.vmdk");
    qemu_img(&[
        "convert",
        "-f",
        "raw",
        "-O",
        "vmdk",
        "-o",
        "subformat=streamOptimized",
        raw.to_str().unwrap(),
        out.to_str().unwrap(),
    ]);
    let vmdk = std::fs::read(&out).unwrap();
    std::fs::remove_file(raw).unwrap();
    std::fs::remove_file(out).unwrap();
    vmdk
}

/// vSphere "Export OVF Template" 모양의 OVF: Linux 게스트, 2 vCPU(소켓당 코어 2), 1024 MiB, efi, SCSI
/// 디스크 `web01-disk1.vmdk`(4 MiB), "VM Network"의 vmxnet3 NIC([`MAC`])
fn ovf() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Envelope xmlns="http://schemas.dmtf.org/ovf/envelope/2" xmlns:ovf="http://schemas.dmtf.org/ovf/envelope/2" xmlns:rasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData" xmlns:sasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_StorageAllocationSettingData" xmlns:epasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_EthernetPortAllocationSettingData" xmlns:vmw="http://www.vmware.com/schema/ovf">
  <References><File ovf:href="web01-disk1.vmdk" ovf:id="file1"/></References>
  <DiskSection>
    <Info>Virtual disk information</Info>
    <Disk ovf:capacity="4" ovf:capacityAllocationUnits="byte * 2^20" ovf:diskId="vmdisk1" ovf:fileRef="file1" ovf:format="http://www.vmware.com/interfaces/specifications/vmdk.html#streamOptimized"/>
  </DiskSection>
  <NetworkSection><Info>Networks</Info><Network ovf:name="VM Network"/></NetworkSection>
  <VirtualSystem ovf:id="web01">
    <Info>A virtual machine</Info>
    <Name>web01</Name>
    <OperatingSystemSection ovf:id="101" vmw:osType="ubuntu64Guest"><Info>Guest OS</Info></OperatingSystemSection>
    <VirtualHardwareSection>
      <Info>Virtual hardware requirements</Info>
      <Item>
        <rasd:ElementName>2 virtual CPU(s)</rasd:ElementName>
        <rasd:InstanceID>1</rasd:InstanceID>
        <rasd:ResourceType>3</rasd:ResourceType>
        <rasd:VirtualQuantity>2</rasd:VirtualQuantity>
        <vmw:CoresPerSocket ovf:required="false">2</vmw:CoresPerSocket>
      </Item>
      <Item>
        <rasd:AllocationUnits>byte * 2^20</rasd:AllocationUnits>
        <rasd:ElementName>1024MB of memory</rasd:ElementName>
        <rasd:InstanceID>2</rasd:InstanceID>
        <rasd:ResourceType>4</rasd:ResourceType>
        <rasd:VirtualQuantity>1024</rasd:VirtualQuantity>
      </Item>
      <Item>
        <rasd:Address>0</rasd:Address>
        <rasd:ElementName>SCSI Controller 0</rasd:ElementName>
        <rasd:InstanceID>3</rasd:InstanceID>
        <rasd:ResourceSubType>VirtualSCSI</rasd:ResourceSubType>
        <rasd:ResourceType>6</rasd:ResourceType>
      </Item>
      <StorageItem>
        <sasd:AddressOnParent>0</sasd:AddressOnParent>
        <sasd:ElementName>Hard disk 1</sasd:ElementName>
        <sasd:HostResource>ovf:/disk/vmdisk1</sasd:HostResource>
        <sasd:InstanceID>4</sasd:InstanceID>
        <sasd:Parent>3</sasd:Parent>
        <sasd:ResourceType>17</sasd:ResourceType>
      </StorageItem>
      <EthernetPortItem>
        <epasd:Address>{MAC}</epasd:Address>
        <epasd:Connection>VM Network</epasd:Connection>
        <epasd:ElementName>Network adapter 1</epasd:ElementName>
        <epasd:InstanceID>5</epasd:InstanceID>
        <epasd:ResourceSubType>VmxNet3</epasd:ResourceSubType>
        <epasd:ResourceType>10</epasd:ResourceType>
      </EthernetPortItem>
      <vmw:Config ovf:required="false" vmw:key="firmware" vmw:value="efi"/>
    </VirtualHardwareSection>
  </VirtualSystem>
</Envelope>
"#
    )
}

fn sha256(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// OVF, `.mf`(SHA256), 디스크 순서의 OVA 파일 `web01.ova`를 만든다. `wrong_digest`면 `.mf`의 디스크
/// 해시를 틀리게 적는다.
fn write_ova(d: &Daemon, disk: &[u8], wrong_digest: bool) -> PathBuf {
    let ovf = ovf();
    let digest = match wrong_digest {
        true => sha256(b"something else"),
        false => sha256(disk),
    };
    let mf = format!(
        "SHA256(web01.ovf)= {}\nSHA256(web01-disk1.vmdk)= {digest}\n",
        sha256(ovf.as_bytes())
    );
    let ova = Tar::new()
        .file("web01.ovf", ovf.as_bytes())
        .file("web01.mf", mf.as_bytes())
        .file("web01-disk1.vmdk", disk)
        .finish();
    let path = d.path("web01.ova");
    std::fs::write(&path, ova).unwrap();
    path
}

/// 볼륨 `name`의 파일(풀 `local`)
async fn volume_file(d: &Daemon, name: &str) -> PathBuf {
    let volume = d.ctl_json(&["volume", "get", name]).await;
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let format = volume["spec"]["format"].as_str().unwrap();
    d.path("local")
        .join(format!("{}.{format}", volume["id"].as_str().unwrap()))
}

/// 이미지의 앞 `MARK` 바이트. raw로 빼서 읽는다.
fn head(d: &Daemon, file: &Path) -> Vec<u8> {
    let raw = d.path("head.raw");
    qemu_img(&[
        "convert",
        "-O",
        "raw",
        file.to_str().unwrap(),
        raw.to_str().unwrap(),
    ]);
    let bytes = std::fs::read(&raw).unwrap();
    std::fs::remove_file(&raw).unwrap();
    bytes[..MARK].to_vec()
}

/// 가져오기의 원본 디렉터리(올린 노드의 `{data_dir}/uploads/imports/{id}`)
fn import_dir(d: &Daemon, import: &Value) -> PathBuf {
    d.data_dir()
        .join("uploads/imports")
        .join(import["id"].as_str().unwrap())
}

/// OVA를 올리면 원본 VM을 읽어 제안을 만들고(Linux라 virtio, efi라 uefi, 2 vCPU를 소켓 1·코어 2로, 같은
/// 이름 네트워크가 없어 매핑 없음), 실행하면 디스크를 볼륨 `web-disk0`으로 옮기고 원본 MAC을 쓰는 꺼진
/// VM `web`을 만든다. 끝나면 가져오기는 `done`이고 원본 디렉터리는 없다. 가져오기를 지워도 VM은 남는다.
#[tokio::test]
async fn ova_import_creates_a_stopped_vm_with_the_disk_contents() {
    let mut d = Daemon::new("node1", "");
    setup(&mut d).await;
    let disk = vmdk(&d, 0xab);
    let ova = write_ova(&d, &disk, false);

    let out = d
        .ctl(&["import", "ova", ova.to_str().unwrap(), "--name", "web01"])
        .await
        .unwrap();
    assert_eq!(
        out,
        "vm-import web01 uploaded; review it with `oplctl import get web01`\n"
    );
    let import = d.ctl_json(&["import", "get", "web01"]).await;
    let status = &import["status"];
    assert_eq!(status["phase"], "ready", "{import}");
    assert_eq!(
        import["spec"]["source"]["ova"],
        json!({ "file_name": "web01.ova", "size_bytes": std::fs::metadata(&ova).unwrap().len() }),
        "{import}"
    );
    assert_eq!(status["source_vm"]["guest_os"], "ubuntu64Guest", "{import}");
    assert_eq!(
        status["source_vm"]["disks"][0]["capacity_bytes"], DISK_BYTES,
        "{import}"
    );
    assert_eq!(
        status["proposal"]["networks"],
        json!({ "VM Network": null })
    );
    let warnings = status["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("NVRAM")),
        "{import}"
    );
    assert!(
        warnings.iter().any(|w| w.as_str().unwrap().contains("MAC")),
        "{import}"
    );
    let dir = import_dir(&d, &import);
    assert!(dir.join("source.ova").exists());
    let text = d.ctl(&["import", "get", "web01"]).await.unwrap();
    assert!(
        text.contains(
            "proposal: 1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs\n"
        ),
        "{text}"
    );

    // 원본 네트워크가 하나뿐이라 `--net lan`이 그것을 잇는다.
    let out = d
        .ctl(&[
            "import", "run", "web01", "--name", "web", "--pool", "local", "--net", "lan",
        ])
        .await
        .unwrap();
    assert_eq!(out, "vm web imported\n");

    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    let spec = &vm["spec"];
    assert_eq!(spec["power"], "stopped", "{vm}");
    assert_eq!(spec["firmware"], "uefi", "{vm}");
    assert_eq!(spec["machine"], "q35", "{vm}");
    assert_eq!(spec["memory_mib"], 1024, "{vm}");
    assert_eq!(
        (&spec["cpu"]["sockets"], &spec["cpu"]["cores"]),
        (&json!(1), &json!(2))
    );
    let disks = spec["disks"].as_array().unwrap();
    assert_eq!(disks.len(), 1, "{vm}");
    assert_eq!(disks[0]["volume"], "web-disk0", "{vm}");
    assert_eq!(disks[0]["bus"], "virtio-blk", "{vm}");
    assert_eq!(disks[0]["boot_order"], 1, "{vm}");
    let nics = spec["nics"].as_array().unwrap();
    assert_eq!(nics.len(), 1, "{vm}");
    assert_eq!(nics[0]["network"], "lan", "{vm}");
    assert_eq!(nics[0]["model"], "virtio", "{vm}");
    assert_eq!(nics[0]["mac"], MAC, "{vm}");

    // 볼륨에 원본 디스크의 내용이 있다.
    let volume = d.ctl_json(&["volume", "get", "web-disk0"]).await;
    assert_eq!(volume["spec"]["size_bytes"], DISK_BYTES, "{volume}");
    let file = volume_file(&d, "web-disk0").await;
    assert_eq!(head(&d, &file), vec![0xab; MARK]);

    let done = d.ctl_json(&["import", "get", "web01"]).await;
    assert_eq!(done["status"]["phase"], "done", "{done}");
    assert_eq!(done["status"]["vm"], vm["id"], "{done}");
    assert!(done["status"]["error"].is_null(), "{done}");
    assert!(!dir.exists(), "source directory left");
    let list = d.ctl(&["import", "list"]).await.unwrap();
    assert!(
        list.lines()
            .nth(1)
            .is_some_and(|l| l.split_whitespace().collect::<Vec<_>>()
                == ["web01", "node1", "web01", "done", "web"]),
        "{list}"
    );

    // 가져오기를 지워도 만든 VM과 볼륨은 남는다.
    let out = d.ctl(&["import", "delete", "web01"]).await.unwrap();
    assert_eq!(out, "vm-import web01 deleted\n");
    assert_eq!(d.ctl_json(&["import", "list"]).await, json!([]));
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "web-disk0"]).await.unwrap();
    d.stop();
}

/// `.mf`의 디스크 해시가 틀린 OVA는 올릴 때는 받지만(해시는 실행 때 본다) 실행이 실패한다: VM과 볼륨을
/// 남기지 않고, 가져오기는 `ready`로 돌아가 이유를 남기며 원본은 그대로다.
#[tokio::test]
async fn checksum_mismatch_fails_the_run_and_leaves_no_volume() {
    let mut d = Daemon::new("node1", "");
    setup(&mut d).await;
    let disk = vmdk(&d, 0xab);
    let ova = write_ova(&d, &disk, true);
    d.ctl(&["import", "ova", ova.to_str().unwrap(), "--name", "web01"])
        .await
        .unwrap();

    let err = d
        .ctl(&[
            "import",
            "run",
            "web01",
            "--name",
            "web",
            "--pool",
            "local",
            "--net",
            "VM Network=lan",
        ])
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("checksum mismatch for web01-disk1.vmdk"),
        "{err:#}"
    );

    let import = d.ctl_json(&["import", "get", "web01"]).await;
    assert_eq!(import["status"]["phase"], "ready", "{import}");
    assert!(
        import["status"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("checksum mismatch for web01-disk1.vmdk")),
        "{import}"
    );
    assert!(import["status"]["vm"].is_null(), "{import}");
    assert_eq!(d.ctl_json(&["vm", "list"]).await, json!([]));
    assert_eq!(d.ctl_json(&["volume", "list"]).await, json!([]));
    let left: Vec<_> = std::fs::read_dir(d.path("local")).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
    let files: Vec<_> = std::fs::read_dir(import_dir(&d, &import))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(files, ["source.ova"]);

    d.ctl(&["import", "delete", "web01"]).await.unwrap();
    assert!(!import_dir(&d, &import).exists(), "source directory left");
    d.stop();
}
