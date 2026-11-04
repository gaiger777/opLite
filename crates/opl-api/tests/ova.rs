//! OVA(tar) 항목 표, OVF 해석, `.mf` 해석, 디스크 추출과 해시 확인.

mod common;

use std::path::{Path, PathBuf};

use common::ustar::{Header, Tar};
use opl_api::vm_imports::ova::{
    Digest, OvaEntry, OvaError, extract, parse_manifest, parse_ova, read_index,
};
use opl_api::vm_imports::ovf::parse_ovf;
use opl_types::vm::{Firmware, MacAddr};
use opl_types::vm_import::{SourceDisk, SourceNic};
use sha2::Digest as _;
use tempfile::TempDir;

const VSPHERE: &str = include_str!("fixtures/vsphere.ovf");
const WORKSTATION: &str = include_str!("fixtures/workstation.ovf");
const GIB: u64 = 1 << 30;

fn write(dir: &TempDir, bytes: &[u8]) -> PathBuf {
    let path = dir.path().join("source.ova");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn index(bytes: Vec<u8>) -> Result<Vec<(String, u64, u64)>, OvaError> {
    let dir = TempDir::new().unwrap();
    let index = read_index(&write(&dir, &bytes))?;
    Ok(index
        .entries
        .into_iter()
        .map(|e| (e.name, e.offset, e.size))
        .collect())
}

fn err(r: Result<impl std::fmt::Debug, OvaError>) -> String {
    r.unwrap_err().to_string()
}

fn header(name: &str, typeflag: u8, size: u64) -> Header<'_> {
    Header {
        name,
        prefix: "",
        typeflag,
        size,
        base256: false,
    }
}

// --- tar 항목 표 ---

#[test]
fn indexes_plain_files() {
    let tar = Tar::new()
        .file("a.ovf", b"hello")
        .file("b.vmdk", &[7u8; 600])
        .finish();
    assert_eq!(
        index(tar).unwrap(),
        vec![
            ("a.ovf".into(), 512, 5),
            // 앞 항목: 헤더 512 + 본문 512(채움)
            ("b.vmdk".into(), 1536, 600),
        ]
    );
}

#[test]
fn joins_ustar_prefix_and_strips_dot_slash() {
    let long = format!("{}/disk.vmdk", "d".repeat(120));
    let tar = Tar::new().file(&long, b"x").file("./vm.ovf", b"y").finish();
    let names: Vec<String> = index(tar).unwrap().into_iter().map(|e| e.0).collect();
    assert_eq!(names, vec![long, "vm.ovf".to_string()]);
}

#[test]
fn pax_header_overrides_path_and_size() {
    let long = format!("{}.vmdk", "p".repeat(150));
    let tar = Tar::new()
        .pax(&[("path", &long), ("size", "700"), ("mtime", "1761811905.5")])
        // 헤더의 이름·크기 칸은 pax 값으로 덮인다
        .raw(header("short.vmdk", b'0', 0), &[1u8; 700])
        .file("after.ovf", b"z")
        .finish();
    let entries = index(tar).unwrap();
    assert_eq!(entries[0], (long, 1536, 700));
    // 다음 항목은 pax 값이 남지 않는다
    assert_eq!(entries[1], ("after.ovf".into(), 1536 + 1024 + 512, 1));
}

#[test]
fn skips_global_pax_and_directories() {
    let tar = Tar::new()
        .raw(
            header("pax_global_header", b'g', 20),
            b"20 comment=opLite!!\n",
        )
        .dir("vm/")
        .file("vm/vm.ovf", b"o")
        .finish();
    let names: Vec<String> = index(tar).unwrap().into_iter().map(|e| e.0).collect();
    assert_eq!(names, vec!["vm/vm.ovf".to_string()]);
}

#[test]
fn gnu_long_name_names_the_next_entry() {
    let long = format!("{}.vmdk", "g".repeat(200));
    let tar = Tar::new()
        .gnu_long_name(&long)
        .file("cut.vmdk", b"abc")
        .file("next.ovf", b"n")
        .finish();
    let names: Vec<String> = index(tar).unwrap().into_iter().map(|e| e.0).collect();
    assert_eq!(names, vec![long, "next.ovf".to_string()]);
}

#[test]
fn pax_values_skip_gnu_long_name_headers() {
    // pax 값은 사이의 GNU 긴 이름 헤더가 아니라 다음 파일에 쓴다. pax `path`가 GNU 이름보다 앞선다.
    let long = format!("{}.vmdk", "q".repeat(150));
    let tar = Tar::new()
        .pax(&[("path", &long), ("size", "700")])
        .gnu_long_name("gnu-name.vmdk")
        .raw(header("short.vmdk", b'0', 0), &[1u8; 700])
        .file("after.ovf", b"z")
        .finish();
    let entries = index(tar).unwrap();
    assert_eq!(entries[0], (long, 2560, 700));
    assert_eq!(entries[1].0, "after.ovf");
    // GNU 긴 이름 뒤의 pax 헤더도 이름을 가져가지 않는다.
    let tar = Tar::new()
        .gnu_long_name("gnu-name.vmdk")
        .pax(&[("mtime", "1")])
        .file("cut.vmdk", b"abc")
        .finish();
    let names: Vec<String> = index(tar).unwrap().into_iter().map(|e| e.0).collect();
    assert_eq!(names, vec!["gnu-name.vmdk".to_string()]);
}

#[test]
fn limits_name_length_and_entry_count() {
    let long = "n".repeat(4097);
    let tar = Tar::new().pax(&[("path", &long)]).file("x", b"x").finish();
    assert_eq!(
        err(index(tar)),
        "a file name in the OVA is longer than 4 KiB"
    );
    let tar = Tar::new().gnu_long_name(&long).file("x", b"x").finish();
    assert_eq!(
        err(index(tar)),
        "a file name in the OVA is longer than 4 KiB"
    );
    // 4 KiB까지는 된다.
    let tar = Tar::new()
        .gnu_long_name(&long[1..])
        .file("x", b"x")
        .finish();
    assert_eq!(index(tar).unwrap()[0].0, long[1..]);

    let many = (0..10_000).fold(Tar::new(), |t, i| t.file(&format!("f{i}"), b""));
    let dir = TempDir::new().unwrap();
    let path = write(&dir, &many.into_bytes());
    // 이름으로 찾는다.
    assert_eq!(read_index(&path).unwrap().get("f9999").unwrap().size, 0);
    let mut too_many = std::fs::read(&path).unwrap();
    too_many.extend(Tar::new().file("f10000", b"").finish());
    assert_eq!(err(index(too_many)), "the OVA has more than 10000 entries");
}

#[test]
fn reads_base256_sizes() {
    let tar = Tar::new()
        .raw(
            Header {
                base256: true,
                ..header("big.vmdk", b'0', 3)
            },
            b"abc",
        )
        .finish();
    assert_eq!(index(tar).unwrap(), vec![("big.vmdk".into(), 512, 3)]);
}

#[test]
fn ends_at_zero_block_or_end_of_file() {
    // 0 블록 뒤의 것은 읽지 않는다
    let mut tar = Tar::new().file("a.ovf", b"a").finish();
    tar.extend(Tar::new().file("b.ovf", b"b").finish());
    assert_eq!(index(tar).unwrap().len(), 1);
    // 끝 표시 없이 파일이 끝나도 된다
    let tar = Tar::new().file("a.ovf", b"a").into_bytes();
    assert_eq!(index(tar).unwrap().len(), 1);
}

#[test]
fn rejects_broken_archives() {
    // 크기가 파일을 넘는다
    let mut tar = Tar::new()
        .raw(header("disk.vmdk", b'0', 4096), &[0u8; 512])
        .into_bytes();
    assert_eq!(err(index(tar.clone())), "the OVA is truncated");
    // 헤더가 잘렸다
    tar = Tar::new().file("a.ovf", b"a").into_bytes();
    tar.extend_from_slice(&[1u8; 100]);
    assert_eq!(err(index(tar)), "the OVA is truncated");
    // tar가 아니다(체크섬이 맞지 않는다)
    assert_eq!(
        err(index(vec![b'x'; 2048])),
        "the file is not an OVA (tar) archive"
    );
    // 지원하지 않는 항목(심볼릭 링크)
    let tar = Tar::new().raw(header("link", b'2', 0), &[]).finish();
    assert_eq!(
        err(index(tar)),
        "unsupported tar entry type '2' for \"link\""
    );
    // 이름이 겹친다
    let tar = Tar::new()
        .file("a.vmdk", b"1")
        .file("a.vmdk", b"2")
        .finish();
    assert_eq!(err(index(tar)), "duplicate file \"a.vmdk\" in the OVA");
}

// --- OVF ---

#[test]
fn parses_vsphere_ovf() {
    let ovf = parse_ovf(VSPHERE).unwrap();
    let vm = ovf.vm;
    assert_eq!(vm.name, "web01");
    assert_eq!(vm.guest_os, "ubuntu64Guest");
    assert_eq!(vm.firmware, Firmware::Uefi);
    assert_eq!((vm.vcpus, vm.cores_per_socket), (4, 2));
    assert_eq!(vm.memory_mib, 8192);
    // 컨트롤러가 나온 순서(SCSI → SATA), 그 안에서 주소 순서
    assert_eq!(
        vm.disks,
        vec![
            SourceDisk {
                name: "disk0".into(),
                file: "web01-disk1.vmdk".into(),
                capacity_bytes: 40 * GIB,
                controller: "scsi".into(),
            },
            SourceDisk {
                name: "disk1".into(),
                file: "web01-disk2.vmdk".into(),
                capacity_bytes: 10 * GIB,
                controller: "scsi".into(),
            },
            SourceDisk {
                name: "disk2".into(),
                file: "web01-disk3.vmdk".into(),
                capacity_bytes: 2 * GIB,
                controller: "sata".into(),
            },
        ]
    );
    assert_eq!(
        vm.nics,
        vec![
            SourceNic {
                network: "VM Network".into(),
                adapter: "vmxnet3".into(),
                mac: Some("00:50:56:8a:12:34".parse::<MacAddr>().unwrap()),
            },
            SourceNic {
                network: "Backup Net".into(),
                adapter: "e1000e".into(),
                mac: Some("00:50:56:8a:56:78".parse::<MacAddr>().unwrap()),
            },
        ]
    );
    // 비디오 카드·VMCI는 경고 없이 뺀다. 디스크가 두 컨트롤러에 걸쳐 있다.
    assert_eq!(
        ovf.warnings,
        vec![
            "CD-ROM drive is not imported",
            "disks are on more than one controller; check that disk0 is the boot disk"
        ]
    );
    assert_eq!(
        ovf.files,
        vec![
            "web01-disk1.vmdk",
            "web01-disk2.vmdk",
            "web01-disk3.vmdk",
            "web01-file1.nvram"
        ]
    );
}

#[test]
fn parses_workstation_ovf() {
    let ovf = parse_ovf(WORKSTATION).unwrap();
    let vm = ovf.vm;
    // Name이 없으면 ovf:id
    assert_eq!(vm.name, "Windows 10 x64");
    assert_eq!(vm.guest_os, "windows9-64");
    assert_eq!(vm.firmware, Firmware::Bios);
    // CoresPerSocket이 없으면 1
    assert_eq!((vm.vcpus, vm.cores_per_socket), (2, 1));
    assert_eq!(vm.memory_mib, 4096);
    assert_eq!(
        vm.disks,
        vec![SourceDisk {
            name: "disk0".into(),
            file: "Windows 10 x64-disk1.vmdk".into(),
            capacity_bytes: 64 * GIB,
            controller: "sata".into(),
        }]
    );
    assert_eq!(
        vm.nics,
        vec![SourceNic {
            network: "nat".into(),
            adapter: "e1000e".into(),
            mac: None,
        }]
    );
    assert_eq!(
        ovf.warnings,
        vec![
            "USB controller is not imported",
            "CD-ROM drive is not imported",
            "floppy drive is not imported",
            "sound is not imported",
        ]
    );
}

#[test]
fn reads_other_memory_units_and_cim_os_ids() {
    // osType이 없으면 CIM 번호로 Windows인지만 안다
    let ovf = WORKSTATION
        .replace(" vmw:osType=\"windows9-64\"", "")
        .replace("ovf:id=\"1\"", "ovf:id=\"105\"")
        .replace(
            "<rasd:AllocationUnits>byte * 2^20</rasd:AllocationUnits>",
            "<rasd:AllocationUnits>MegaBytes</rasd:AllocationUnits>",
        );
    let vm = parse_ovf(&ovf).unwrap().vm;
    assert_eq!(vm.guest_os, "windows");
    assert_eq!(vm.memory_mib, 4096);

    let ovf = VSPHERE.replace(" vmw:osType=\"ubuntu64Guest\"", "");
    assert_eq!(
        parse_ovf(&ovf).unwrap().vm.guest_os,
        "Ubuntu Linux (64-bit)"
    );
}

#[test]
fn orders_controllers_of_a_kind_by_address() {
    // SATA 컨트롤러를 SCSI 1번(Address 0)으로, 원래 SCSI 컨트롤러를 Address 1로 바꾼다.
    let ovf = VSPHERE
        .replace(
            "<rasd:Address>0</rasd:Address>\n        <rasd:Description>SCSI Controller",
            "<rasd:Address>1</rasd:Address>\n        <rasd:Description>SCSI Controller",
        )
        .replace(
            "<rasd:ResourceSubType>vmware.sata.ahci</rasd:ResourceSubType>\n        <rasd:ResourceType>20</rasd:ResourceType>",
            "<rasd:ResourceSubType>VirtualSCSI</rasd:ResourceSubType>\n        <rasd:ResourceType>6</rasd:ResourceType>",
        );
    let ovf = parse_ovf(&ovf).unwrap();
    let files: Vec<&str> = ovf.vm.disks.iter().map(|d| d.file.as_str()).collect();
    assert_eq!(
        files,
        ["web01-disk3.vmdk", "web01-disk1.vmdk", "web01-disk2.vmdk"]
    );
    assert!(ovf.vm.disks.iter().all(|d| d.controller == "scsi"));
    assert!(
        ovf.warnings
            .iter()
            .any(|w| w.starts_with("disks are on more than one controller"))
    );

    // 한 컨트롤러에만 디스크가 있으면 경고하지 않는다.
    let ovf = parse_ovf(WORKSTATION).unwrap();
    assert!(!ovf.warnings.iter().any(|w| w.starts_with("disks are on")));
}

/// `count`개의 디스크(SCSI 컨트롤러 하나)와 NIC를 가진 OVF
fn ovf_with(disks: usize, nics: usize) -> String {
    let mut files = String::new();
    let mut section = String::new();
    let mut items = String::new();
    for i in 0..disks {
        files += &format!(r#"<File ovf:href="d{i}.vmdk" ovf:id="f{i}"/>"#);
        section += &format!(
            r#"<Disk ovf:capacity="1" ovf:capacityAllocationUnits="byte * 2^30" ovf:diskId="d{i}" ovf:fileRef="f{i}"/>"#
        );
        items += &format!(
            "<Item><rasd:AddressOnParent>{i}</rasd:AddressOnParent><rasd:HostResource>ovf:/disk/d{i}</rasd:HostResource><rasd:InstanceID>{}</rasd:InstanceID><rasd:Parent>3</rasd:Parent><rasd:ResourceType>17</rasd:ResourceType></Item>",
            100 + i
        );
    }
    for i in 0..nics {
        items += &format!(
            "<Item><rasd:Connection>net{i}</rasd:Connection><rasd:InstanceID>{}</rasd:InstanceID><rasd:ResourceSubType>VmxNet3</rasd:ResourceSubType><rasd:ResourceType>10</rasd:ResourceType></Item>",
            200 + i
        );
    }
    format!(
        r#"<Envelope xmlns="http://schemas.dmtf.org/ovf/envelope/1" xmlns:ovf="http://schemas.dmtf.org/ovf/envelope/1" xmlns:rasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData">
  <References>{files}</References>
  <DiskSection>{section}</DiskSection>
  <VirtualSystem ovf:id="vm">
    <VirtualHardwareSection>
      <Item><rasd:InstanceID>1</rasd:InstanceID><rasd:ResourceType>3</rasd:ResourceType><rasd:VirtualQuantity>1</rasd:VirtualQuantity></Item>
      <Item><rasd:InstanceID>2</rasd:InstanceID><rasd:ResourceType>4</rasd:ResourceType><rasd:VirtualQuantity>512</rasd:VirtualQuantity></Item>
      <Item><rasd:InstanceID>3</rasd:InstanceID><rasd:ResourceType>6</rasd:ResourceType></Item>
      {items}
    </VirtualHardwareSection>
  </VirtualSystem>
</Envelope>"#
    )
}

#[test]
fn limits_disks_and_nics() {
    let ovf = parse_ovf(&ovf_with(32, 16)).unwrap();
    assert_eq!((ovf.vm.disks.len(), ovf.vm.nics.len()), (32, 16));
    assert_eq!(
        err(parse_ovf(&ovf_with(33, 1))),
        "the OVF has 33 disks; at most 32 are supported"
    );
    assert_eq!(
        err(parse_ovf(&ovf_with(1, 17))),
        "the OVF has 17 network adapters; at most 16 are supported"
    );
}

#[test]
fn rejects_unsupported_ovf() {
    let cases = [
        (
            VSPHERE
                .replace("<VirtualSystem ovf:id=\"web01\">", "<VirtualSystemCollection ovf:id=\"vapp\"><VirtualSystem ovf:id=\"web01\">")
                .replace("</VirtualSystem>", "</VirtualSystem></VirtualSystemCollection>"),
            "only single-VM OVA files are supported",
        ),
        (
            VSPHERE.replace(
                "ovf:id=\"file1\"",
                "ovf:id=\"file1\" ovf:compression=\"gzip\"",
            ),
            "compressed disk files (ovf:compression) are not supported",
        ),
        (
            VSPHERE.replace(
                "ovf:id=\"file1\"",
                "ovf:id=\"file1\" ovf:chunkSize=\"2147483648\"",
            ),
            "split disk files (ovf:chunkSize) are not supported",
        ),
        (
            VSPHERE.replace(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
                "<?xml version=\"1.0\"?><!DOCTYPE Envelope [<!ENTITY x SYSTEM \"file:///etc/passwd\">]>",
            ),
            "invalid OVF XML",
        ),
        (
            VSPHERE.replace("ovf:/disk/vmdisk2", "ovf:/disk/nope"),
            "disk \"nope\" is not in the DiskSection",
        ),
        (
            VSPHERE.replace("ovf:fileRef=\"file2\"", "ovf:fileRef=\"file9\""),
            "disk \"vmdisk2\" refers to an unknown file \"file9\"",
        ),
        (
            VSPHERE.replace("vmdk.html#streamOptimized\" ovf:populatedSize=\"0\"/>\n    <Disk ovf:capacity=\"2048\"", "raw\" ovf:populatedSize=\"0\"/>\n    <Disk ovf:capacity=\"2048\""),
            "disk \"vmdisk2\" is not a VMDK",
        ),
    ];
    for (xml, message) in cases {
        let e = err(parse_ovf(&xml));
        assert!(
            e.starts_with(message),
            "{e:?} does not start with {message:?}"
        );
    }
}

// --- .mf ---

#[test]
fn parses_manifest_lines() {
    let sha256 = "a".repeat(64);
    let sha1 = "B".repeat(40);
    let text =
        format!("SHA256(web01.ovf)= {sha256}\r\nSHA1 (Windows 10 x64-disk1.vmdk) = {sha1}\n\n");
    let text = format!(
        "{text}SHA384(a.vmdk)= {}\nSHA512(b.vmdk)= {}\nMD5(c.vmdk)= 00\n",
        "c".repeat(96),
        "d".repeat(128)
    );
    let mf = parse_manifest(&text).unwrap();
    assert_eq!(mf.digests.len(), 4);
    assert_eq!(mf.digests["web01.ovf"], Digest::Sha256(sha256));
    // 16진수는 소문자로
    assert_eq!(
        mf.digests["Windows 10 x64-disk1.vmdk"],
        Digest::Sha1("b".repeat(40))
    );
    assert_eq!(mf.digests["a.vmdk"], Digest::Sha384("c".repeat(96)));
    assert_eq!(mf.digests["b.vmdk"], Digest::Sha512("d".repeat(128)));
    // 모르는 알고리즘은 오류가 아니라 확인하지 않는 파일이다.
    assert_eq!(mf.unsupported, ["c.vmdk".to_string()].into());

    for bad in [
        "SHA256(a.vmdk) abc".to_string(),
        format!("SHA256(a.vmdk)= {}", "a".repeat(40)),
        format!("SHA1(a.vmdk)= {}", "z".repeat(40)),
        format!("SHA512(a.vmdk)= {}", "a".repeat(64)),
    ] {
        let e = err(parse_manifest(&bad));
        assert!(e.starts_with("invalid manifest line"), "{bad}: {e}");
    }
}

// --- OVA 전체 ---

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(data))
}

/// vSphere 견본 OVF와 그 파일들
fn vsphere_ova(tweak: impl FnOnce(Tar) -> Tar) -> Vec<u8> {
    let disk = b"KDMV fake disk";
    let mf = format!(
        "SHA256(web01.ovf)= {}\nSHA256(web01-disk1.vmdk)= {}\n",
        sha256_hex(VSPHERE.as_bytes()),
        sha256_hex(disk)
    );
    let tar = Tar::new()
        .file("web01.ovf", VSPHERE.as_bytes())
        .file("web01.mf", mf.as_bytes())
        .file("web01-disk1.vmdk", disk)
        .file("web01-disk2.vmdk", disk)
        .file("web01-disk3.vmdk", disk)
        .file("web01-file1.nvram", b"nvram");
    tweak(tar).finish()
}

#[test]
fn parses_whole_ova() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, &vsphere_ova(|t| t.file("web01.cert", b"cert")));
    let ova = parse_ova(&path).unwrap();
    assert_eq!(ova.vm.name, "web01");
    assert_eq!(ova.vm.disks.len(), 3);
    // `.mf`에 없는 디스크는 확인하지 않는다고 알린다.
    assert_eq!(
        ova.warnings,
        vec![
            "CD-ROM drive is not imported",
            "disks are on more than one controller; check that disk0 is the boot disk",
            "the OVA signature (.cert) is not verified",
            "web01-disk2.vmdk is not listed in the manifest; it is not verified",
            "web01-disk3.vmdk is not listed in the manifest; it is not verified",
        ]
    );
    assert_eq!(ova.index.entries.len(), 7);
    assert_eq!(ova.index.get("web01-disk1.vmdk").unwrap().size, 14);
    assert_eq!(
        ova.manifest["web01-disk1.vmdk"],
        Digest::Sha256(sha256_hex(b"KDMV fake disk"))
    );
}

#[test]
fn ova_without_manifest_is_fine() {
    let dir = TempDir::new().unwrap();
    let tar = Tar::new()
        .file("Windows 10 x64.ovf", WORKSTATION.as_bytes())
        .file("Windows 10 x64-disk1.vmdk", b"disk")
        .finish();
    let ova = parse_ova(&write(&dir, &tar)).unwrap();
    assert!(ova.manifest.is_empty());
    assert_eq!(ova.vm.disks[0].file, "Windows 10 x64-disk1.vmdk");
    // `.mf`가 없으면 확인하지 않는다는 경고도 없다.
    assert!(!ova.warnings.iter().any(|w| w.contains("manifest")));
}

#[test]
fn unsupported_manifest_digest_is_a_warning() {
    let dir = TempDir::new().unwrap();
    let tar = Tar::new()
        .file("Windows 10 x64.ovf", WORKSTATION.as_bytes())
        .file("w.mf", b"MD5(Windows 10 x64-disk1.vmdk)= 00\n")
        .file("Windows 10 x64-disk1.vmdk", b"disk")
        .finish();
    let ova = parse_ova(&write(&dir, &tar)).unwrap();
    assert!(ova.manifest.is_empty());
    assert_eq!(
        ova.warnings.last().unwrap(),
        "the manifest uses an unsupported digest for Windows 10 x64-disk1.vmdk; it is not verified"
    );
}

#[test]
fn warnings_are_unique_and_capped() {
    // 이름이 다른 가져오지 않는 장치 60개와 같은 이름 둘
    let devices: String = (0..60)
        .map(|i| format!("<Item><rasd:ElementName>dev{i}</rasd:ElementName><rasd:InstanceID>{}</rasd:InstanceID><rasd:ResourceType>1</rasd:ResourceType></Item>", 1000 + i))
        .chain(["<Item><rasd:ElementName>dev0</rasd:ElementName><rasd:InstanceID>2000</rasd:InstanceID><rasd:ResourceType>1</rasd:ResourceType></Item>".to_string()])
        .collect();
    let ovf = WORKSTATION.replace(
        "</VirtualHardwareSection>",
        &format!("{devices}</VirtualHardwareSection>"),
    );
    let dir = TempDir::new().unwrap();
    let tar = Tar::new()
        .file("w.ovf", ovf.as_bytes())
        .file("Windows 10 x64-disk1.vmdk", b"disk")
        .finish();
    let warnings = parse_ova(&write(&dir, &tar)).unwrap().warnings;
    // 견본의 경고 넷 + 장치 60개 = 64개 중 50개와 나머지 개수
    assert_eq!(warnings.len(), 51);
    assert_eq!(warnings[4], "dev0 is not imported");
    assert_eq!(warnings[50], "… and 14 more");
}

/// OVA에 넣을 (이름, 내용)
type Files<'a> = Vec<(&'a str, &'a [u8])>;

#[test]
fn rejects_bad_ova_layouts() {
    let ovf = WORKSTATION.as_bytes();
    let disk = ("Windows 10 x64-disk1.vmdk", &b"disk"[..]);
    let big = vec![b' '; (4 << 20) + 1];
    let cases: Vec<(Files, &str)> = vec![
        (vec![disk], "the OVA has no .ovf file"),
        (
            vec![("a.ovf", ovf), ("b.ovf", ovf), disk],
            "the OVA has more than one .ovf file",
        ),
        (
            vec![("a.ovf", &big[..]), disk],
            "the .ovf file is larger than 4 MiB",
        ),
        (
            vec![("a.ovf", ovf), ("a.mf", &b""[..]), ("b.mf", &b""[..]), disk],
            "the OVA has more than one .mf file",
        ),
        (
            vec![("a.ovf", ovf)],
            "file \"Windows 10 x64-disk1.vmdk\" is not in the OVA",
        ),
        (
            vec![("a.ovf", ovf), ("a.mf", &b"garbage"[..]), disk],
            "invalid manifest line",
        ),
    ];
    for (files, message) in cases {
        let dir = TempDir::new().unwrap();
        let tar = files
            .iter()
            .fold(Tar::new(), |t, (name, data)| t.file(name, data))
            .finish();
        let e = err(parse_ova(&write(&dir, &tar)));
        assert!(
            e.starts_with(message),
            "{e:?} does not start with {message:?}"
        );
    }
}

// --- 추출 ---

fn entry_of(path: &Path, name: &str) -> OvaEntry {
    read_index(path).unwrap().get(name).unwrap().clone()
}

#[test]
fn extracts_and_checks_digest() {
    let dir = TempDir::new().unwrap();
    let data: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
    let tar = Tar::new()
        .file("x.ovf", b"o")
        .file("disk.vmdk", &data)
        .finish();
    let path = write(&dir, &tar);
    let entry = entry_of(&path, "disk.vmdk");
    let dest = dir.path().join("disk-out.vmdk");

    extract(
        &path,
        &entry,
        &dest,
        Some(&Digest::Sha256(sha256_hex(&data))),
    )
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);

    let sha1 = hex::encode(sha1::Sha1::digest(&data));
    extract(&path, &entry, &dest, Some(&Digest::Sha1(sha1))).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);

    let sha384 = hex::encode(sha2::Sha384::digest(&data));
    extract(&path, &entry, &dest, Some(&Digest::Sha384(sha384))).unwrap();
    let sha512 = hex::encode(sha2::Sha512::digest(&data));
    extract(&path, &entry, &dest, Some(&Digest::Sha512(sha512))).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);

    // 해시가 없으면 복사만
    std::fs::remove_file(&dest).unwrap();
    extract(&path, &entry, &dest, None).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);
}

#[test]
fn digest_mismatch_removes_the_copy() {
    let dir = TempDir::new().unwrap();
    let tar = Tar::new().file("disk.vmdk", b"payload").finish();
    let path = write(&dir, &tar);
    let entry = entry_of(&path, "disk.vmdk");
    let dest = dir.path().join("disk-out.vmdk");

    let wrong = Digest::Sha256(sha256_hex(b"other"));
    assert_eq!(
        err(extract(&path, &entry, &dest, Some(&wrong))),
        "checksum mismatch for disk.vmdk"
    );
    assert!(!dest.exists());
}
