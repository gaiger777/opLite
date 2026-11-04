//! ESXi `.vmx`와 VMDK 디스크립터 해석, 데이터스토어 안 디스크 경로.

use opl_api::vm_imports::vmx::{
    Descriptor, Extent, VmxDisk, check_flat, disk_path, parse_descriptor, parse_vmx, vmx_to_source,
};
use opl_types::vm::{Firmware, MAX_NICS, MacAddr};
use opl_types::vm_import::{SourceNic, SourceVm};

const ESXI7: &str = include_str!("fixtures/esxi7.vmx");
const ESXI8: &str = include_str!("fixtures/esxi8.vmx");

fn source(text: &str) -> Result<(SourceVm, Vec<String>, Vec<VmxDisk>), String> {
    let vmx = parse_vmx(text).map_err(|e| e.to_string())?;
    vmx_to_source(&vmx, "web01/web01.vmx").map_err(|e| e.to_string())
}

fn disks(text: &str) -> Vec<(String, String, String)> {
    source(text)
        .unwrap()
        .2
        .into_iter()
        .map(|d| (d.name, d.path, d.controller))
        .collect()
}

fn triple(name: &str, path: &str, controller: &str) -> (String, String, String) {
    (name.into(), path.into(), controller.into())
}

fn mac(s: &str) -> Option<MacAddr> {
    Some(s.parse().unwrap())
}

// --- .vmx ---

#[test]
fn parses_esxi8_vmx() {
    let (vm, warnings, disks) = source(ESXI8).unwrap();
    // `|22`·`|23` 이스케이프를 푼다
    assert_eq!(vm.name, "web01 \"prod\" # 1");
    assert_eq!(vm.guest_os, "ubuntu-64");
    assert_eq!(vm.firmware, Firmware::Uefi);
    assert_eq!((vm.vcpus, vm.cores_per_socket, vm.memory_mib), (2, 2, 2048));
    // 디스크는 디스크립터를 읽은 뒤 채운다
    assert!(vm.disks.is_empty());
    assert_eq!(
        disks
            .into_iter()
            .map(|d| (d.name, d.path, d.controller))
            .collect::<Vec<_>>(),
        [
            triple("disk0", "web01.vmdk", "nvme"),
            triple(
                "disk1",
                "/vmfs/volumes/datastore2/web01/web01_1.vmdk",
                "nvme"
            ),
        ]
    );
    assert_eq!(
        vm.nics,
        [
            // static이면 address, 대문자 키도 읽는다
            SourceNic {
                network: "VM Network".into(),
                adapter: "vmxnet3".into(),
                mac: mac("00:50:56:aa:bb:cc"),
            },
            // generated면 generatedAddress
            SourceNic {
                network: "Storage".into(),
                adapter: "vmxnet3".into(),
                mac: mac("00:0c:29:ab:cd:ef"),
            },
            // 꺼진 ethernet2는 빼고, 어댑터 종류가 없으면 e1000, MAC이 없으면 None
            SourceNic {
                network: "Lab".into(),
                adapter: "e1000".into(),
                mac: None,
            },
        ]
    );
    // CD-ROM 경고는 한 번만, 꺼진 사운드·플로피는 경고하지 않는다
    assert_eq!(
        warnings,
        [
            "CD-ROM drive is not imported",
            "USB controller is not imported"
        ]
    );
}

#[test]
fn parses_esxi7_vmx() {
    let (vm, warnings, disks) = source(ESXI7).unwrap();
    assert_eq!(vm.name, "win01");
    assert_eq!(vm.guest_os, "windows2019srvNext-64");
    assert_eq!(vm.firmware, Firmware::Uefi);
    assert_eq!((vm.vcpus, vm.cores_per_socket, vm.memory_mib), (4, 2, 8192));
    assert_eq!(
        disks
            .into_iter()
            .map(|d| (d.name, d.path, d.controller))
            .collect::<Vec<_>>(),
        [
            triple("disk0", "win01.vmdk", "scsi"),
            triple("disk1", "win01_1.vmdk", "scsi"),
        ]
    );
    // `vpx`(vCenter가 정한 주소)도 generatedAddress
    assert_eq!(
        vm.nics,
        [SourceNic {
            network: "VM Network".into(),
            adapter: "e1000e".into(),
            mac: mac("00:0c:29:12:34:56"),
        }]
    );
    assert_eq!(warnings, ["CD-ROM drive is not imported"]);
}

#[test]
fn defaults_for_a_minimal_vmx() {
    let (vm, warnings, disks) = source("memsize = 512\n").unwrap();
    // 이름이 없으면 .vmx 파일 이름
    assert_eq!(vm.name, "web01");
    assert_eq!(vm.guest_os, "other");
    assert_eq!(vm.firmware, Firmware::Bios);
    assert_eq!((vm.vcpus, vm.cores_per_socket, vm.memory_mib), (1, 1, 512));
    assert!(vm.nics.is_empty() && disks.is_empty() && warnings.is_empty());
    let (vm, _, _) = source("memsize = 512\nfirmware = \"bios\"\n").unwrap();
    assert_eq!(vm.firmware, Firmware::Bios);
}

#[test]
fn reads_comments_case_and_unquoted_values() {
    let text = "# 주석\n\n  MemSize=\"1024\"  \nNUMVCPUS = 8 \ncpuid.CoresPerSocket = \"4\"\n\
                displayname = \"a|7cb|0a\"\nnot a key value line\nfirmware = \"EFI\"\n";
    let (vm, _, _) = source(text).unwrap();
    assert_eq!((vm.vcpus, vm.cores_per_socket, vm.memory_mib), (8, 4, 1024));
    assert_eq!(vm.name, "a|b\n");
    assert_eq!(vm.firmware, Firmware::Uefi);
    // 같은 키가 또 나오면 뒤의 것
    let (vm, _, _) = source("memsize = 1\nmemSize = \"2\"\n").unwrap();
    assert_eq!(vm.memory_mib, 2);
}

#[test]
fn rejects_bad_vmx() {
    for (text, want) in [
        ("numvcpus = 2\n", "the .vmx has no memSize"),
        ("memsize = lots\n", "invalid memSize \"lots\""),
        ("memsize = 1\nnumvcpus = -1\n", "invalid numvcpus \"-1\""),
        ("memsize = 1\0\n", "not a .vmx file"),
        (
            "memsize = 1\nethernet1.present = \"TRUE\"\n",
            "ethernet1 has no networkName",
        ),
        (
            "memsize = 1\nscsi0:0.present = \"TRUE\"\n",
            "scsi0:0 has no fileName",
        ),
        (
            "memsize = 1\nide0:0.present = \"TRUE\"\nide0:0.fileName = \"x.iso\"\n",
            "disk ide0:0 is not a VMDK: \"x.iso\"",
        ),
    ] {
        assert_eq!(source(text).unwrap_err(), want, "{text:?}");
    }
    let big = format!("memsize = 1\n{}", "#".repeat(64 << 10));
    assert_eq!(
        source(&big).unwrap_err(),
        "the .vmx file is larger than 64 KiB"
    );
}

#[test]
fn orders_disks_by_controller_bus_and_unit() {
    let mut text = String::from("memsize = 1\n");
    for dev in [
        "nvme0:0", "scsi1:0", "scsi0:2", "scsi0:10", "sata0:1", "ide1:0", "ide0:1",
    ] {
        text += &format!("{dev}.present = \"TRUE\"\n{dev}.fileName = \"{dev}.vmdk\"\n");
    }
    // 꺼진 디스크와 디스크가 아닌 장치는 뺀다
    text += "scsi0:0.present = \"FALSE\"\nscsi0:0.fileName = \"off.vmdk\"\n";
    text += "scsi0:1.present = \"TRUE\"\nscsi0:1.deviceType = \"scsi-passthru\"\n";
    let (_, warnings, d) = source(&text).unwrap();
    let order: Vec<_> = d.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(
        order,
        [
            "ide0:1.vmdk",
            "ide1:0.vmdk",
            "sata0:1.vmdk",
            "scsi0:2.vmdk",
            "scsi0:10.vmdk",
            "scsi1:0.vmdk",
            "nvme0:0.vmdk",
        ]
    );
    let names: Vec<_> = d.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names[0], "disk0");
    assert_eq!(names[6], "disk6");
    assert_eq!(
        warnings,
        [
            "scsi0:1 (scsi-passthru) is not imported",
            "disks are on more than one controller; check that disk0 is the boot disk",
        ]
    );
}

#[test]
fn hdd_order_puts_the_boot_disk_first() {
    let text = "memsize = 1\n\
                scsi0:0.present = \"TRUE\"\nscsi0:0.fileName = \"data.vmdk\"\n\
                scsi0:1.present = \"TRUE\"\nscsi0:1.fileName = \"boot.vmdk\"\n\
                sata0:0.present = \"TRUE\"\nsata0:0.fileName = \"other.vmdk\"\n\
                bios.hddOrder = \"scsi0:1\"\n";
    assert_eq!(
        disks(text),
        [
            triple("disk0", "boot.vmdk", "scsi"),
            triple("disk1", "other.vmdk", "sata"),
            triple("disk2", "data.vmdk", "scsi"),
        ]
    );
    let (_, warnings, _) = source(text).unwrap();
    // 부트 디스크를 정했으므로 여러 컨트롤러 경고는 없다
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn accepts_known_disk_device_types() {
    for t in ["disk", "scsi-hardDisk", "ata-hardDisk", "SCSI-HARDDISK"] {
        let text = format!(
            "memsize = 1\nsata0:0.present = \"TRUE\"\nsata0:0.deviceType = \"{t}\"\n\
             sata0:0.fileName = \"a.VMDK\"\n"
        );
        assert_eq!(disks(&text), [triple("disk0", "a.VMDK", "sata")], "{t}");
    }
}

#[test]
fn warns_about_devices_that_are_not_imported() {
    let text = "memsize = 1\nfloppy0.present = \"TRUE\"\nusb.present = \"true\"\n\
                sound.present = \"TRUE\"\nide1:0.present = \"TRUE\"\n\
                ide1:0.deviceType = \"cdrom-image\"\nide1:0.fileName = \"a.iso\"\n";
    let (_, warnings, _) = source(text).unwrap();
    assert_eq!(
        warnings,
        [
            "CD-ROM drive is not imported",
            "floppy drive is not imported",
            "USB controller is not imported",
            "sound card is not imported",
        ]
    );
}

#[test]
fn limits_disks_and_nics() {
    let with = |disks: usize, nics: usize| {
        let mut text = String::from("memsize = 1\n");
        for i in 0..disks {
            let dev = format!("scsi{}:{}", i / 15, i % 15);
            text += &format!("{dev}.present = \"TRUE\"\n{dev}.fileName = \"d{i}.vmdk\"\n");
        }
        for i in 0..nics {
            text += &format!("ethernet{i}.present = \"TRUE\"\nethernet{i}.networkName = \"n\"\n");
        }
        source(&text)
    };
    assert_eq!(with(32, MAX_NICS).unwrap().2.len(), 32);
    assert_eq!(
        with(33, 0).unwrap_err(),
        "the .vmx has 33 disks; at most 32 are supported"
    );
    assert_eq!(
        with(0, MAX_NICS + 1).unwrap_err(),
        format!(
            "the .vmx has {} network adapters; at most {MAX_NICS} are supported",
            MAX_NICS + 1
        )
    );
}

// --- 디스크립터 ---

const VMFS: &str = r#"# Disk DescriptorFile
version=1
encoding="UTF-8"
CID=fffffffe
parentCID=ffffffff
createType="vmfs"

# Extent description
RW 8192 VMFS "web01-flat.vmdk"

# The Disk Data Base
#DDB

ddb.adapterType = "lsilogic"
ddb.geometry.cylinders = "4"
ddb.thinProvisioned = "1"
ddb.virtualHWVersion = "20"
"#;

fn descriptor(create_type: &str, extents: &str, extra: &str) -> String {
    format!(
        "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\n{extra}\
         createType=\"{create_type}\"\n\n# Extent description\n{extents}\n\nddb.adapterType = \"lsilogic\"\n"
    )
}

fn flat(text: &str) -> Result<(String, u64), String> {
    let d = parse_descriptor(text).map_err(|e| e.to_string())?;
    check_flat(&d).map_err(|e| e.to_string())
}

#[test]
fn parses_vmfs_descriptor() {
    assert_eq!(
        parse_descriptor(VMFS).unwrap(),
        Descriptor {
            create_type: "vmfs".into(),
            extents: vec![Extent {
                access: "RW".into(),
                sectors: 8192,
                kind: "VMFS".into(),
                file: "web01-flat.vmdk".into(),
                offset: 0,
            }],
            parent: false,
        }
    );
    assert_eq!(flat(VMFS).unwrap(), ("web01-flat.vmdk".into(), 8192 * 512));
}

#[test]
fn accepts_monolithic_flat_with_zero_offset() {
    let text = descriptor("monolithicFlat", "RW 2097152 FLAT \"disk-flat.vmdk\" 0", "");
    assert_eq!(flat(&text).unwrap(), ("disk-flat.vmdk".into(), 1 << 30));
}

#[test]
fn rejects_snapshots_rdm_and_other_layouts() {
    let snap = "the VM has snapshots; consolidate or delete them in VMware first";
    let rdm = "raw device mappings are not supported";
    let one = "RW 8192 VMFS \"a-flat.vmdk\"";
    for (text, want) in [
        (
            descriptor(
                "vmfsSparse",
                "RW 8192 VMFSSPARSE \"a-000001-delta.vmdk\"",
                "",
            ),
            snap.to_string(),
        ),
        (
            descriptor(
                "seSparse",
                "RW 8192 SESPARSE \"a-000001-sesparse.vmdk\"",
                "",
            ),
            snap.into(),
        ),
        (
            descriptor(
                "twoGbMaxExtentSparse",
                "RW 4192256 SPARSE \"a-s001.vmdk\"",
                "",
            ),
            snap.into(),
        ),
        // 부모가 있으면 createType이 vmfs여도 스냅샷이다
        (
            descriptor("vmfs", one, "parentFileNameHint=\"a.vmdk\"\n"),
            snap.into(),
        ),
        (
            descriptor("vmfsRawDeviceMap", "RW 8192 VMFSRDM \"a-rdm.vmdk\"", ""),
            rdm.into(),
        ),
        (
            descriptor(
                "vmfsPassthroughRawDeviceMap",
                "RW 8192 VMFSRDM \"a-rdmp.vmdk\"",
                "",
            ),
            rdm.into(),
        ),
        (descriptor("vmfsRDM", one, ""), rdm.into()),
        (
            descriptor("streamOptimized", "RW 8192 SPARSE \"a.vmdk\"", ""),
            "unsupported VMDK type streamOptimized".into(),
        ),
        (
            descriptor(
                "monolithicFlat",
                "RW 8192 FLAT \"a-f001.vmdk\" 0\nRW 8192 FLAT \"a-f002.vmdk\" 0",
                "",
            ),
            "the VMDK descriptor has 2 extents; only one is supported".into(),
        ),
        (
            descriptor("vmfs", "", ""),
            "the VMDK descriptor has 0 extents; only one is supported".into(),
        ),
        (
            descriptor("monolithicFlat", "RW 8192 FLAT \"a-flat.vmdk\" 128", ""),
            "unsupported VMDK extent RW 8192 FLAT \"a-flat.vmdk\" 128".into(),
        ),
        (
            descriptor("vmfs", "RDONLY 8192 VMFS \"a-flat.vmdk\"", ""),
            "unsupported VMDK extent RDONLY 8192 VMFS \"a-flat.vmdk\" 0".into(),
        ),
        (
            descriptor("vmfs", "RW 8192 VMFS \"../b/a-flat.vmdk\"", ""),
            "invalid extent file name \"../b/a-flat.vmdk\"".into(),
        ),
        (
            descriptor("vmfs", "RW 8192 VMFS \"sub/a-flat.vmdk\"", ""),
            "invalid extent file name \"sub/a-flat.vmdk\"".into(),
        ),
        (
            descriptor("vmfs", "RW 8192 VMFS \"a\\b.vmdk\"", ""),
            "invalid extent file name \"a\\\\b.vmdk\"".into(),
        ),
        (
            descriptor("vmfs", "RW 8192 VMFS \"..\"", ""),
            "invalid extent file name \"..\"".into(),
        ),
        (
            descriptor("vmfs", "RW 8192 VMFS \".\"", ""),
            "invalid extent file name \".\"".into(),
        ),
    ] {
        assert_eq!(flat(&text).unwrap_err(), want, "{text}");
    }
}

/// 이름 안의 `..`는 경로가 아니다(`/`·`\`가 없으므로 디렉터리를 벗어나지 않는다).
#[test]
fn accepts_dots_inside_extent_names() {
    let text = descriptor("vmfs", "RW 8192 VMFS \"web..01-flat.vmdk\"", "");
    assert_eq!(
        flat(&text).unwrap(),
        ("web..01-flat.vmdk".into(), 8192 * 512)
    );
}

#[test]
fn rejects_non_descriptors() {
    let not = "not a VMDK descriptor";
    // 스파스 VMDK 헤더(KDMV)처럼 바이너리
    assert_eq!(flat("KDMV\u{1}\0\0\0\u{3}").unwrap_err(), not);
    assert_eq!(flat("hello world\n").unwrap_err(), not);
    assert_eq!(
        flat(&descriptor("vmfs", "RW lots VMFS \"a-flat.vmdk\"", "")).unwrap_err(),
        "invalid VMDK extent \"RW lots VMFS \\\"a-flat.vmdk\\\"\""
    );
    assert_eq!(
        flat(&descriptor("vmfs", "RW 8192 VMFS a-flat.vmdk", "")).unwrap_err(),
        "invalid VMDK extent \"RW 8192 VMFS a-flat.vmdk\""
    );
    let big = format!("{VMFS}{}", "#".repeat(64 << 10));
    assert_eq!(
        flat(&big).unwrap_err(),
        "the VMDK descriptor is larger than 64 KiB"
    );
}

// --- 디스크 경로 ---

#[test]
fn resolves_disk_paths_in_the_datastore() {
    let path = |file: &str| disk_path("web01/web01.vmx", "datastore1", file);
    assert_eq!(path("web01.vmdk").unwrap(), "web01/web01.vmdk");
    assert_eq!(path("sub/web01.vmdk").unwrap(), "web01/sub/web01.vmdk");
    assert_eq!(
        path("/vmfs/volumes/datastore1/other/x.vmdk").unwrap(),
        "other/x.vmdk"
    );
    // `.` 조각은 뺀다.
    assert_eq!(path("./web01.vmdk").unwrap(), "web01/web01.vmdk");
    assert_eq!(path("sub/./x.vmdk").unwrap(), "web01/sub/x.vmdk");
    assert_eq!(
        path("/vmfs/volumes/datastore1/./other/x.vmdk").unwrap(),
        "other/x.vmdk"
    );
    // 맨 위의 .vmx
    assert_eq!(
        disk_path("top.vmx", "datastore1", "top.vmdk").unwrap(),
        "top.vmdk"
    );
    // 디스크립터 옆의 extent도 같은 함수로
    assert_eq!(
        disk_path("web01/web01.vmdk", "datastore1", "web01-flat.vmdk").unwrap(),
        "web01/web01-flat.vmdk"
    );
    for (file, want) in [
        (
            "/vmfs/volumes/datastore2/web01/x.vmdk",
            "disk \"/vmfs/volumes/datastore2/web01/x.vmdk\" is on another datastore",
        ),
        (
            "/vmfs/volumes/62f3a1b2-0c1d2e3f-4a5b-001122334455/web01/x.vmdk",
            "disk \"/vmfs/volumes/62f3a1b2-0c1d2e3f-4a5b-001122334455/web01/x.vmdk\" uses a datastore UUID path; it is not supported",
        ),
        (
            "/data/x.vmdk",
            "disk path \"/data/x.vmdk\" is not supported",
        ),
        (
            "/vmfs/volumes/datastore1/",
            "disk path \"/vmfs/volumes/datastore1/\" is not supported",
        ),
        ("../x.vmdk", "disk path \"../x.vmdk\" is not supported"),
        (
            "/vmfs/volumes/datastore1/a/../x.vmdk",
            "disk path \"/vmfs/volumes/datastore1/a/../x.vmdk\" is not supported",
        ),
        ("a\\x.vmdk", "disk path \"a\\\\x.vmdk\" is not supported"),
        ("", "disk path \"\" is not supported"),
    ] {
        assert_eq!(path(file).unwrap_err().to_string(), want, "{file}");
    }
}
