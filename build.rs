use std::path::PathBuf;

fn resource(out: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
    for v in [data.len() as u32, 32] { out.extend(v.to_le_bytes()) }
    for v in [0xFFFF, kind, 0xFFFF, id] { out.extend(v.to_le_bytes()) }
    out.extend(0u32.to_le_bytes());
    out.extend(flags.to_le_bytes());
    out.extend(0x0409u16.to_le_bytes());
    out.extend([0u8; 8]);
    out.extend(data);
    while out.len() % 4 != 0 { out.push(0) }
}

fn main() {
    println!("cargo:rerun-if-changed=netwatch.ico");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") { return }
    println!("cargo:rustc-link-arg-bins=/DELAYLOAD:pdh.dll");
    println!("cargo:rustc-link-arg-bins=delayimp.lib");
    let ico = std::fs::read("netwatch.ico").expect("netwatch.ico sits next to Cargo.toml");
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    let mut res = vec![0, 0, 0, 0, 0x20, 0, 0, 0, 0xFF, 0xFF, 0, 0, 0xFF, 0xFF, 0, 0];
    res.extend([0u8; 16]);
    let mut group = vec![0, 0, 1, 0, count as u8, 0];
    for i in 0..count {
        let entry = &ico[6 + 16 * i..6 + 16 * (i + 1)];
        let size = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
        resource(&mut res, 3, i as u16 + 1, 0x1010, &ico[offset..offset + size]);
        group.extend(&entry[..12]);
        group.extend((i as u16 + 1).to_le_bytes());
    }
    resource(&mut res, 14, 1, 0x1030, &group);
    let path = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("netwatch.res");
    std::fs::write(&path, res).unwrap();
    println!("cargo:rustc-link-arg-bins={}", path.display());
}
