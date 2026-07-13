//! BYOK API keys in the Windows Credential Manager (design "worth considering"
//! #6, 2026-07-13 — the SDD had long flagged plaintext `.env` keys as future
//! hardening).
//!
//! How it works: `.env` stays the single settings file, but a secret's line
//! holds the [`SENTINEL`] instead of the raw key; the real value lives in the
//! Credential Manager under `Navisual/<ENV_NAME>` (visible in Windows'
//! "Credential Manager" control panel, per-user, DPAPI-encrypted at rest).
//! `Config::load` resolves sentinels back to real values; `save_settings`
//! stores typed keys here and writes the sentinel. A raw key pasted into
//! `.env` by hand still works — the startup migration (`migrate_env_secrets`
//! in lib.rs) moves it into the vault on the next launch, so the plaintext
//! window is one session at most.
//!
//! Not covered (deliberately, for now): `supabase_session.json` (auth tokens,
//! separate file/lifecycle) and non-secret settings.

/// `.env` placeholder meaning "the real value is in the Credential Manager".
/// Chosen to be self-explanatory to a user reading their `.env`.
pub const SENTINEL: &str = "stored-in-credential-manager";

/// The `.env` names treated as secrets. Keep in sync with `Config`'s
/// `*_api_key` fields + `save_settings`' key handling.
pub const SECRET_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "GEMINI_API_KEY",
    "OPENAI_API_KEY",
    "DEEPSEEK_API_KEY",
    "QWEN_API_KEY",
    "CUSTOM_API_KEY",
];

#[cfg(windows)]
mod imp {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::ERROR_NOT_FOUND;
    use windows::Win32::Security::Credentials::{
        CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE,
        CRED_TYPE_GENERIC,
    };

    fn target_for(env_name: &str) -> Vec<u16> {
        format!("Navisual/{env_name}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Store `value` under `Navisual/<env_name>`. Returns whether it succeeded —
    /// callers fall back to plaintext `.env` on failure rather than losing the key.
    pub fn store(env_name: &str, value: &str) -> bool {
        let mut target = target_for(env_name);
        // UTF-8 blob (CredentialBlob is opaque bytes; UTF-8 keeps read() trivial).
        let blob = value.as_bytes();
        let cred = CREDENTIALW {
            Flags: CRED_FLAGS(0),
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_ptr() as *mut u8,
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        let ok = unsafe { CredWriteW(&cred, 0) }.is_ok();
        if !ok {
            log::warn!("[credvault] CredWriteW failed for {env_name} — keeping plaintext fallback");
        }
        ok
    }

    /// Read the secret stored under `Navisual/<env_name>`; None if absent/unreadable.
    pub fn read(env_name: &str) -> Option<String> {
        let target = target_for(env_name);
        let mut pcred: *mut CREDENTIALW = std::ptr::null_mut();
        let res = unsafe {
            CredReadW(
                windows::core::PCWSTR(target.as_ptr()),
                CRED_TYPE_GENERIC,
                None,
                &mut pcred,
            )
        };
        if let Err(e) = res {
            if e.code() != ERROR_NOT_FOUND.to_hresult() {
                log::warn!("[credvault] CredReadW failed for {env_name}: {e}");
            }
            return None;
        }
        let out = unsafe {
            let cred = &*pcred;
            let bytes = std::slice::from_raw_parts(
                cred.CredentialBlob,
                cred.CredentialBlobSize as usize,
            );
            let s = String::from_utf8_lossy(bytes).into_owned();
            CredFree(pcred as *mut core::ffi::c_void);
            s
        };
        (!out.is_empty()).then_some(out)
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn store(_env_name: &str, _value: &str) -> bool {
        false
    }
    pub fn read(_env_name: &str) -> Option<String> {
        None
    }
}

pub use imp::{read, store};

#[cfg(all(test, windows))]
mod tests {
    // Round-trip against the real per-user Credential Manager, under a test-only
    // name so it can't collide with a live key.
    #[test]
    fn store_read_roundtrip() {
        let name = "TEST_CREDVAULT_ROUNDTRIP";
        assert!(super::store(name, "sk-test-123"));
        assert_eq!(super::read(name).as_deref(), Some("sk-test-123"));
        // Overwrite works.
        assert!(super::store(name, "sk-test-456"));
        assert_eq!(super::read(name).as_deref(), Some("sk-test-456"));
    }

    #[test]
    fn read_missing_is_none() {
        assert!(super::read("TEST_CREDVAULT_DOES_NOT_EXIST").is_none());
    }
}
