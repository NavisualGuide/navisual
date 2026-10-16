//! Moving a file or folder to the Recycle Bin, so what the app removes on its own -- a pruned
//! session, its frames -- can be put back by the user, the way a delete in File Explorer can.

/// Send `path` (an absolute file or folder) to the Recycle Bin, silently: no confirmation, no
/// progress, no error dialog. An error is returned, never shown.
#[cfg(windows)]
pub fn to_recycle_bin(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE,
        SHFILEOPSTRUCTW,
    };
    if !path.is_absolute() {
        return Err(std::io::Error::other(format!("not an absolute path: {}", path.display())));
    }
    // pFrom is a list of NUL-terminated paths, ended by one more NUL.
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT).0 as u16,
        ..Default::default()
    };
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc != 0 || op.fAnyOperationsAborted.as_bool() {
        return Err(std::io::Error::other(format!(
            "SHFileOperationW failed for {}: {rc:#x}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    /// Puts one temp file in the Recycle Bin -- ignored, so a normal test run leaves the
    /// developer's Recycle Bin alone. `cargo test --lib recycle -- --ignored`
    #[test]
    #[ignore]
    fn a_file_goes_to_the_recycle_bin() {
        let p = std::env::temp_dir().join(format!("navisual-recycle-test-{}.txt", std::process::id()));
        std::fs::write(&p, "navisual recycle test").unwrap();
        super::to_recycle_bin(&p).unwrap();
        assert!(!p.exists());
    }
}
