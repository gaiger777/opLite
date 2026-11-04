//! 시험용 작은 ustar 작성기. OVA(tar)를 만든다. 표준 라이브러리만 쓰므로 다른 크레이트의 시험도
//! `#[path = ".../opl-api/tests/common/ustar.rs"] mod ustar;`로 가져다 쓸 수 있다.
//!
//! ```ignore
//! let ova = Tar::new().file("vm.ovf", ovf.as_bytes()).file("vm-disk1.vmdk", &disk).finish();
//! ```

/// 헤더 하나를 마음대로 정할 때 쓴다(잘못된 tar를 만드는 시험).
pub struct Header<'a> {
    pub name: &'a str,
    pub prefix: &'a str,
    pub typeflag: u8,
    /// 헤더의 크기 칸에 쓸 값(본문 길이와 달라도 된다)
    pub size: u64,
    /// 크기를 8진수 대신 base-256으로 쓴다
    pub base256: bool,
}

#[derive(Default)]
pub struct Tar(Vec<u8>);

impl Tar {
    pub fn new() -> Self {
        Self::default()
    }

    /// 보통 파일. 이름이 100바이트를 넘으면 `/`에서 나누어 앞부분을 prefix에 쓴다.
    pub fn file(self, name: &str, data: &[u8]) -> Self {
        let (prefix, name) = match name.len() > 100 {
            true => name.rsplit_once('/').expect("long name needs a '/'"),
            false => ("", name),
        };
        self.raw(
            Header {
                name,
                prefix,
                typeflag: b'0',
                size: data.len() as u64,
                base256: false,
            },
            data,
        )
    }

    pub fn dir(self, name: &str) -> Self {
        self.raw(
            Header {
                name,
                prefix: "",
                typeflag: b'5',
                size: 0,
                base256: false,
            },
            &[],
        )
    }

    /// pax 확장 헤더(`x`). 다음 항목의 값을 덮는다.
    pub fn pax(self, records: &[(&str, &str)]) -> Self {
        let mut data = Vec::new();
        for (key, value) in records {
            // 길이는 자기 자신의 자릿수를 포함한다
            let body = format!(" {key}={value}\n");
            let mut len = body.len() + 1;
            while (len.to_string().len() + body.len()) != len {
                len += 1;
            }
            data.extend_from_slice(format!("{len}{body}").as_bytes());
        }
        self.special(b'x', &data)
    }

    /// GNU 긴 이름(`L`). 다음 항목의 이름이다.
    pub fn gnu_long_name(self, name: &str) -> Self {
        let mut data = name.as_bytes().to_vec();
        data.push(0);
        self.special(b'L', &data)
    }

    fn special(self, typeflag: u8, data: &[u8]) -> Self {
        self.raw(
            Header {
                name: "././@LongLink",
                prefix: "",
                typeflag,
                size: data.len() as u64,
                base256: false,
            },
            data,
        )
    }

    /// 헤더와 본문(512바이트로 채움)을 쓴다.
    pub fn raw(mut self, h: Header<'_>, data: &[u8]) -> Self {
        let mut block = [0u8; 512];
        block[..h.name.len()].copy_from_slice(h.name.as_bytes());
        block[100..108].copy_from_slice(b"0000644\0");
        block[108..116].copy_from_slice(b"0000000\0");
        block[116..124].copy_from_slice(b"0000000\0");
        if h.base256 {
            block[124] = 0x80;
            block[128..136].copy_from_slice(&h.size.to_be_bytes());
        } else {
            block[124..136].copy_from_slice(format!("{:011o}\0", h.size).as_bytes());
        }
        block[136..148].copy_from_slice(b"00000000000\0");
        block[156] = h.typeflag;
        block[257..263].copy_from_slice(b"ustar\0");
        block[263..265].copy_from_slice(b"00");
        block[345..345 + h.prefix.len()].copy_from_slice(h.prefix.as_bytes());
        // 체크섬은 체크섬 칸을 공백으로 두고 더한 값이다
        block[148..156].copy_from_slice(b"        ");
        let sum: u32 = block.iter().map(|&b| u32::from(b)).sum();
        block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        self.0.extend_from_slice(&block);
        self.0.extend_from_slice(data);
        self.0.resize(self.0.len().next_multiple_of(512), 0);
        self
    }

    /// 끝 표시(0 블록 둘)를 붙인다.
    pub fn finish(mut self) -> Vec<u8> {
        self.0.extend_from_slice(&[0u8; 1024]);
        self.0
    }

    /// 끝 표시 없이(파일 끝에서 끝나는 tar)
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}
