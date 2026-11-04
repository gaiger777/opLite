//! VM 가져오기 Task와 가져오기 지우기 Task: Mock 하이퍼바이저와 dir 풀로, 시험에서 `qemu-img`로 만든
//! streamOptimized VMDK를 담은 OVA를 가져와 꺼진 VM과 볼륨을 만든다. `.mf` 해시가 다르거나 VM을 만들지
//! 못하면 만든 볼륨을 지우고 가져오기를 `ready`로 되돌리며, 고친 값으로 다시 실행할 수 있다.
//! qemu-img와 qemu-io가 필요하다.
//! `cargo test -p opl-api --features storage-it --test vm_import_tasks`
#![cfg(feature = "storage-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::ustar::{Header, Tar};
use common::{Client, Env, MOUNT_ROOT};
use opl_driver::DirStorage;
use opl_types::network::NetworkSpec;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::vm::{CpuModel, DiskBus, Firmware, MachineType, NicModel, PowerState, Vm, VmSpec};
use opl_types::vm_import::{ImportPhase, VmImport, VmImportSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MIB: u64 = 1 << 20;
const MAC: &str = "00:50:56:8a:12:34";

fn qemu_img(args: &[&str]) {
    let out = Command::new("qemu-img").args(args).output().unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, format: &str, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", format, "-c", cmd, path.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// 4 MiB 디스크의 처음 64 KiB를 `mark`로, 1 MiB부터 64 KiB를 `0x5a`로 채운 streamOptimized VMDK
fn vmdk(dir: &Path, name: &str, mark: u8) -> Vec<u8> {
    let raw = dir.join(format!("{name}.raw"));
    let mut data = vec![0u8; 4 * MIB as usize];
    data[..64 << 10].fill(mark);
    data[MIB as usize..(MIB as usize + (64 << 10))].fill(0x5a);
    std::fs::write(&raw, data).unwrap();
    let out = dir.join(format!("{name}.vmdk"));
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
    std::fs::read(out).unwrap()
}

/// vSphere 모양의 OVF: 2 vCPU(소켓당 코어 2), 1024 MiB, efi, Linux, SCSI 디스크 `disks`(파일 이름, 4 MiB),
/// "VM Network"의 vmxnet3 NIC 하나([`MAC`])
fn ovf(disks: &[&str]) -> String {
    let mut files = String::new();
    let mut disk_section = String::new();
    let mut items = String::new();
    for (i, file) in disks.iter().enumerate() {
        files += &format!(r#"<File ovf:href="{file}" ovf:id="file{i}"/>"#);
        disk_section += &format!(
            r#"<Disk ovf:capacity="4" ovf:capacityAllocationUnits="byte * 2^20" ovf:diskId="vmdisk{i}" ovf:fileRef="file{i}" ovf:format="http://www.vmware.com/interfaces/specifications/vmdk.html#streamOptimized"/>"#
        );
        items += &format!(
            r#"<StorageItem>
        <sasd:AddressOnParent>{i}</sasd:AddressOnParent>
        <sasd:ElementName>Hard disk {i}</sasd:ElementName>
        <sasd:HostResource>ovf:/disk/vmdisk{i}</sasd:HostResource>
        <sasd:InstanceID>{}</sasd:InstanceID>
        <sasd:Parent>3</sasd:Parent>
        <sasd:ResourceType>17</sasd:ResourceType>
      </StorageItem>"#,
            10 + i
        );
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Envelope xmlns="http://schemas.dmtf.org/ovf/envelope/2" xmlns:ovf="http://schemas.dmtf.org/ovf/envelope/2" xmlns:rasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData" xmlns:sasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_StorageAllocationSettingData" xmlns:epasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_EthernetPortAllocationSettingData" xmlns:vmw="http://www.vmware.com/schema/ovf">
  <References>{files}</References>
  <DiskSection><Info>Virtual disk information</Info>{disk_section}</DiskSection>
  <NetworkSection><Info>Networks</Info><Network ovf:name="VM Network"/></NetworkSection>
  <VirtualSystem ovf:id="web">
    <Info>A virtual machine</Info>
    <Name>web</Name>
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
      {items}
      <EthernetPortItem>
        <epasd:Address>{MAC}</epasd:Address>
        <epasd:Connection>VM Network</epasd:Connection>
        <epasd:ElementName>Network adapter 1</epasd:ElementName>
        <epasd:InstanceID>4</epasd:InstanceID>
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

/// 디스크 `disks`(파일 이름, 내용)와 `.mf`를 담은 OVA. `wrong`인 파일은 `.mf`의 해시를 틀리게 적는다.
fn ova(disks: &[(&str, &[u8])], wrong: Option<&str>) -> Vec<u8> {
    let names: Vec<&str> = disks.iter().map(|(n, _)| *n).collect();
    let ovf = ovf(&names);
    let mut mf = format!("SHA256(web.ovf)= {}\n", sha256(ovf.as_bytes()));
    for (name, data) in disks {
        let digest = match wrong == Some(*name) {
            true => sha256(b"something else"),
            false => sha256(data),
        };
        mf += &format!("SHA256({name})= {digest}\n");
    }
    let mut tar = Tar::new()
        .file("web.ovf", ovf.as_bytes())
        .file("web.mf", mf.as_bytes());
    for (name, data) in disks {
        tar = tar.file(name, data);
    }
    tar.finish()
}

fn pool_dir(env: &Env) -> PathBuf {
    env.dir.path().join("local")
}

/// 네트워크 `lan`과 이 노드에서 준비된 dir 풀 `local`
async fn setup(env: &Env) {
    let network = Resource::new(
        "lan",
        NetworkSpec {
            bridge: Some("br0".into()),
            vlan: None,
            vxlan: None,
            mtu: None,
        },
    );
    env.cluster.create(&network).await.unwrap();
    std::fs::create_dir_all(pool_dir(env)).unwrap();
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: pool_dir(env),
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
}

/// OVA를 가져오기 `web`으로 올린다.
async fn upload(admin: &Client, ova: Vec<u8>) -> Value {
    let (status, body) = admin.upload("/vm-imports/ova?name=web", ova).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

/// 가져오기 `web`을 `body`로 실행하고 Task가 끝날 때까지 기다린다.
async fn run(admin: &Client, body: Value) -> Value {
    let (status, task) = admin.post("/vm-imports/web/run", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

fn run_request() -> Value {
    json!({ "name": "web", "pool": "local", "networks": { "VM Network": "lan" } })
}

fn import(env: &Env) -> Option<VmImport> {
    env.cluster.get_by_name::<VmImportSpec>("web").unwrap()
}

fn get_vm(env: &Env, name: &str) -> Option<Vm> {
    env.cluster.get_by_name::<VmSpec>(name).unwrap()
}

fn get_volume(env: &Env, name: &str) -> Option<Volume> {
    env.cluster.get_by_name::<VolumeSpec>(name).unwrap()
}

/// 원본 디렉터리 안의 파일 이름
fn source_files(env: &Env, import: &VmImport) -> Vec<String> {
    let dir = env
        .dir
        .path()
        .join("uploads/imports")
        .join(import.id.to_string());
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect(),
        Err(_) => vec![],
    }
}

/// 가져오기는 디스크를 qcow2 볼륨 `{name}-disk{i}`로 옮기고, 제안대로(Linux: virtio, efi: uefi, 2 vCPU를
/// 소켓 1·코어 2로) 원본 MAC을 쓰는 꺼진 VM을 만든다. 새 VM id는 Task의 대상이다. 끝나면 가져오기는
/// `done`이고 원본 디렉터리는 없다. 디스크 크기만큼 걸리므로 Task 시간 제한을 받지 않는다. 다 가져온
/// 가져오기를 지워도 VM은 남는다.
#[tokio::test]
async fn imports_an_ova_into_a_stopped_vm() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    env.stop_runner().await;
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.timeout = Duration::ZERO
    })
    .await;
    let disk = vmdk(env.dir.path(), "src", 0xaa);
    let uploaded = upload(&admin, ova(&[("web-disk1.vmdk", &disk)], None)).await;
    assert_eq!(uploaded["status"]["proposal"]["disk_bus"], "virtio-blk");

    let task = run(&admin, run_request()).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = get_vm(&env, "web").unwrap();
    assert_eq!(task["spec"]["target"], vm.id.to_string());
    let spec = &vm.spec;
    assert_eq!((spec.cpu.sockets, spec.cpu.cores), (1, 2));
    assert_eq!(spec.cpu.model, CpuModel::Host);
    assert_eq!(spec.memory_mib, 1024);
    assert_eq!(spec.firmware, Firmware::Uefi);
    assert_eq!(spec.machine, MachineType::Q35);
    assert_eq!(spec.power, PowerState::Stopped);
    assert_eq!(spec.node, None);
    assert_eq!(spec.disks.len(), 1);
    assert_eq!(spec.disks[0].volume, "web-disk0");
    assert_eq!(spec.disks[0].bus, DiskBus::VirtioBlk);
    assert_eq!(spec.disks[0].boot_order, Some(1));
    assert_eq!(spec.nics.len(), 1);
    assert_eq!(spec.nics[0].network, "lan");
    assert_eq!(spec.nics[0].model, NicModel::Virtio);
    assert_eq!(
        spec.nics[0].mac.map(|m| m.to_string()).as_deref(),
        Some(MAC)
    );

    let volume = get_volume(&env, "web-disk0").unwrap();
    assert_eq!(volume.spec.pool, "local");
    assert_eq!(volume.spec.format, ImageFormat::Qcow2);
    assert_eq!(volume.spec.size_bytes, 4 * MIB);
    assert_eq!(volume.status.phase, VolumePhase::Ready);
    assert_eq!(volume.status.node, Some(env.node));
    let path = pool_dir(&env).join(format!("{}.qcow2", volume.id));
    assert!(qemu_io(&path, "qcow2", "read -P 0xaa 0 64k"));
    assert!(qemu_io(&path, "qcow2", "read -P 0x5a 1M 64k"));

    let done = import(&env).unwrap();
    assert_eq!(done.status.phase, ImportPhase::Done);
    assert_eq!(done.status.vm, Some(vm.id));
    assert_eq!(done.status.error, None);
    assert!(source_files(&env, &done).is_empty());

    // 다 가져온 가져오기는 다시 실행하지 않는다. 지우면 리소스만 없어지고 VM과 볼륨은 남는다.
    let (status, body) = admin.post("/vm-imports/web/run", run_request()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "import_done");
    // 지우기 Task는 시간 제한을 받으므로 보통 설정으로 되돌린다.
    env.stop_runner().await;
    env.start_runner().await;
    let (status, task) = admin.delete("/vm-imports/web").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(import(&env).is_none());
    assert!(get_vm(&env, "web").is_some());
    assert!(get_volume(&env, "web-disk0").is_some());
}

/// `.mf`의 해시가 다른 디스크가 있으면 실행은 실패한다: 그때까지 만든 볼륨(파일과 리소스)과 추출한
/// 파일을 지우고 VM을 만들지 않으며, 가져오기는 `ready`로 돌아가 이유를 남긴다. 원본은 남는다.
#[tokio::test]
async fn checksum_mismatch_fails_and_leaves_no_volumes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let first = vmdk(env.dir.path(), "a", 0xaa);
    let second = vmdk(env.dir.path(), "b", 0xbb);
    let disks: [(&str, &[u8]); 2] = [("web-disk1.vmdk", &first), ("web-disk2.vmdk", &second)];
    upload(&admin, ova(&disks, Some("web-disk2.vmdk"))).await;

    let task = run(&admin, run_request()).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert_eq!(
        error,
        "cannot import disk \"disk1\": checksum mismatch for web-disk2.vmdk"
    );
    let failed = import(&env).unwrap();
    assert_eq!(failed.status.phase, ImportPhase::Ready);
    assert_eq!(failed.status.error.as_deref(), Some(error));
    assert!(get_vm(&env, "web").is_none());
    assert!(get_volume(&env, "web-disk0").is_none());
    assert!(get_volume(&env, "web-disk1").is_none());
    let left: Vec<_> = std::fs::read_dir(pool_dir(&env)).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(source_files(&env, &failed), ["source.ova"]);
}

/// 원본 MAC을 쓰는 실행은 그새 다른 VM이 그 MAC을 가져갔으면 디스크를 옮기기 전에 실패한다. 값을
/// 고쳐(새 MAC) 다시 실행하면 가져오고 이전 오류를 지운다.
#[tokio::test]
async fn failed_run_can_be_run_again_with_other_values() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let disk = vmdk(env.dir.path(), "src", 0xaa);
    upload(&admin, ova(&[("web-disk1.vmdk", &disk)], None)).await;

    // API가 확인한 뒤 Task가 돌기 전에 같은 MAC의 VM이 생긴다.
    env.stop_runner().await;
    let (status, task) = admin.post("/vm-imports/web/run", run_request()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let mut other = common::vm_request("other");
    other["spec"]["nics"][0]["mac"] = json!(MAC);
    let (status, body) = admin.post("/vms", other).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.start_runner().await;

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert_eq!(error, format!("MAC address {MAC} is already in use"));
    let failed = import(&env).unwrap();
    assert_eq!(failed.status.phase, ImportPhase::Ready);
    assert_eq!(failed.status.error.as_deref(), Some(error));
    assert!(get_vm(&env, "web").is_none());
    assert!(get_volume(&env, "web-disk0").is_none());
    let left: Vec<_> = std::fs::read_dir(pool_dir(&env)).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");

    let mut body = run_request();
    body["keep_macs"] = json!(false);
    let task = run(&admin, body).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = get_vm(&env, "web").unwrap();
    let mac = vm.spec.nics[0].mac.unwrap().to_string();
    assert_ne!(mac, MAC);
    let done = import(&env).unwrap();
    assert_eq!(done.status.phase, ImportPhase::Done);
    assert_eq!(done.status.error, None);
    assert!(qemu_io(
        &pool_dir(&env).join(format!(
            "{}.qcow2",
            get_volume(&env, "web-disk0").unwrap().id
        )),
        "qcow2",
        "read -P 0xaa 0 64k"
    ));
}

/// 디스크마다 추출하기 전에 업로드 디렉터리의 여유(디스크 크기 + 1 GiB)를 본다. 모자라면 볼륨을 남기지
/// 않고 실패해 가져오기는 `ready`로 돌아간다. 여기서는 API가 확인한 뒤 원본을 디스크가 아주 큰(빈 곳이 많은)
/// OVA로 바꾼다.
#[tokio::test]
async fn extract_needs_space_in_the_upload_directory() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let disk = vmdk(env.dir.path(), "src", 0xaa);
    let uploaded = upload(&admin, ova(&[("web-disk1.vmdk", &disk)], None)).await;
    env.stop_runner().await;
    let (status, task) = admin.post("/vm-imports/web/run", run_request()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");

    let source = env
        .dir
        .path()
        .join("uploads/imports")
        .join(uploaded["id"].as_str().unwrap())
        .join("source.ova");
    let available = fs4::available_space(source.parent().unwrap()).unwrap();
    let size = available.next_multiple_of(512);
    let ovf = ovf(&["web-disk1.vmdk"]);
    let tar = Tar::new()
        .file("web.ovf", ovf.as_bytes())
        .pax(&[("size", &size.to_string())])
        .raw(
            Header {
                name: "web-disk1.vmdk",
                prefix: "",
                typeflag: b'0',
                size: 0,
                base256: false,
            },
            &[],
        )
        .into_bytes();
    std::fs::write(&source, &tar).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .unwrap()
        .set_len(tar.len() as u64 + size)
        .unwrap();
    env.start_runner().await;

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert_eq!(
        error,
        "cannot import disk \"disk0\": not enough space in the upload directory to extract web-disk1.vmdk"
    );
    let failed = import(&env).unwrap();
    assert_eq!(failed.status.phase, ImportPhase::Ready);
    assert!(get_volume(&env, "web-disk0").is_none());
    assert_eq!(source_files(&env, &failed), ["source.ova"]);
}
