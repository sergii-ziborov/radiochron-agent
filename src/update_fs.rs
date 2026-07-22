use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};

pub fn digest_file(path: &Path) -> anyhow::Result<String> {
    Ok(hex(&Sha256::digest(std::fs::read(path)?)))
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

pub fn atomic_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
    crate::private_fs::restrict_file(&temp)?;
    replace_file(&temp, path)
}

#[cfg(not(windows))]
pub fn replace_file(source: &Path, target: &Path) -> anyhow::Result<()> {
    std::fs::rename(source, target)?;
    Ok(())
}

#[cfg(windows)]
pub fn replace_file(source: &Path, target: &Path) -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(source: *const u16, target: *const u16, flags: u32) -> i32;
    }
    const REPLACE_EXISTING: u32 = 0x1;
    const WRITE_THROUGH: u32 = 0x8;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            REPLACE_EXISTING | WRITE_THROUGH,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
