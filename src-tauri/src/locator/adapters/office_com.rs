//! Late-bound Office COM (IDispatch) helpers — the shared layer that generalises the
//! Pass-0 adapter idea to the rest of Office (2026-07-18).
//!
//! Excel's adapter never needed this (its grid is reachable via UIA `GridPattern`), and
//! its doc comment deferred Office COM as "painful". PowerPoint forced the issue: the
//! slide canvas exposes **zero** UIA descendants (probed live — `a11y candidates=0`,
//! hit-test `uia=unknown`), so shapes/placeholders are only reachable through the object
//! model (`Application.ActiveWindow.View.Slide.Shapes` + `PointsToScreenPixelsX/Y`).
//! Word's document canvas is the same story for exact text ranges (`Find` +
//! `Window.GetPoint`).
//!
//! This module keeps the pain in one place: connect to the running instance via the ROT
//! (`GetActiveObject` — we never launch Office ourselves; no instance → adapter falls
//! through), then chain property gets / method calls through `IDispatch::Invoke`.
//! Remember the two classic Invoke traps, both handled here: `rgvarg` takes arguments in
//! **reverse** order, and property gets still need a valid (empty) `DISPPARAMS`.

#![cfg(windows)]

use anyhow::{anyhow, Context, Result};
use windows::core::{Interface, BSTR, GUID, PCWSTR};
use windows::Win32::System::Com::{
    CLSIDFromProgID, CoInitializeEx, IDispatch, COINIT_APARTMENTTHREADED, DISPATCH_METHOD,
    DISPATCH_PROPERTYGET, DISPPARAMS,
};
use windows::Win32::System::Ole::GetActiveObject;
use windows::Win32::System::Variant::{VariantClear, VARIANT};

/// Locale for `GetIDsOfNames`/`Invoke` — Office object-model member names are invariant.
const LOCALE_USER_DEFAULT: u32 = 0x0400;

/// Best-effort per-thread COM init. The locator's blocking threads usually already have
/// COM initialised (the `uiautomation` crate does it); a "wrong mode" failure here just
/// means that — safe to ignore, the thread is usable either way.
fn ensure_com() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

/// Connect to the RUNNING instance of an Office app via the Running Object Table.
/// No running instance (or one that hasn't registered yet) → `Err` → the adapter falls
/// through to the normal pipeline. We deliberately never CoCreate (launching Office on a
/// locate would be absurd).
pub fn get_active_object(prog_id: &str) -> Result<IDispatch> {
    ensure_com();
    let wide: Vec<u16> = prog_id.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let clsid: GUID = CLSIDFromProgID(PCWSTR(wide.as_ptr()))
            .with_context(|| format!("CLSIDFromProgID({prog_id})"))?;
        let mut unk = None;
        GetActiveObject(&clsid, None, &mut unk)
            .with_context(|| format!("GetActiveObject({prog_id}) — instance not running?"))?;
        let unk = unk.ok_or_else(|| anyhow!("GetActiveObject returned no object"))?;
        unk.cast::<IDispatch>().context("cast IUnknown → IDispatch")
    }
}

/// Resolve a member name to its DISPID.
fn dispid(obj: &IDispatch, name: &str) -> Result<i32> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut id = 0i32;
    unsafe {
        obj.GetIDsOfNames(
            &GUID::zeroed(),
            &PCWSTR(wide.as_ptr()),
            1,
            LOCALE_USER_DEFAULT,
            &mut id,
        )
        .with_context(|| format!("GetIDsOfNames({name})"))?;
    }
    Ok(id)
}

/// Core Invoke wrapper. `args` in NATURAL order (we reverse into `rgvarg` internally).
fn invoke(obj: &IDispatch, name: &str, flags: u16, args: &mut [VARIANT]) -> Result<VARIANT> {
    let id = dispid(obj, name)?;
    // rgvarg is right-to-left.
    args.reverse();
    let params = DISPPARAMS {
        rgvarg: if args.is_empty() {
            std::ptr::null_mut()
        } else {
            args.as_mut_ptr()
        },
        rgdispidNamedArgs: std::ptr::null_mut(),
        cArgs: args.len() as u32,
        cNamedArgs: 0,
    };
    let mut result = VARIANT::default();
    unsafe {
        obj.Invoke(
            id,
            &GUID::zeroed(),
            LOCALE_USER_DEFAULT,
            windows::Win32::System::Com::DISPATCH_FLAGS(flags),
            &params,
            Some(&mut result),
            None,
            None,
        )
        .with_context(|| format!("Invoke({name})"))?;
    }
    // Restore caller's slice order (byref out-args live in these VARIANTs).
    args.reverse();
    Ok(result)
}

/// Property get with no arguments (`obj.Name`).
pub fn get(obj: &IDispatch, name: &str) -> Result<VARIANT> {
    invoke(obj, name, DISPATCH_PROPERTYGET.0, &mut [])
}

/// Indexed property get / getter-with-args (`Shapes.Item(3)`).
pub fn get_indexed(obj: &IDispatch, name: &str, args: Vec<VARIANT>) -> Result<VARIANT> {
    let mut args = args;
    invoke(
        obj,
        name,
        DISPATCH_PROPERTYGET.0 | DISPATCH_METHOD.0,
        &mut args,
    )
}

/// Method call (`window.PointsToScreenPixelsX(72.0)`, `find.Execute("model")`).
pub fn call(obj: &IDispatch, name: &str, args: Vec<VARIANT>) -> Result<VARIANT> {
    let mut args = args;
    invoke(obj, name, DISPATCH_METHOD.0, &mut args)
}

/// Method call whose `args` include VT_BYREF out-parameters the callee writes into
/// (`Window.GetPoint(&x, &y, &w, &h, range)`). Same as [`call`] but the caller keeps
/// ownership of the slice so it can read the by-ref slots afterwards.
pub fn call_byref(obj: &IDispatch, name: &str, args: &mut [VARIANT]) -> Result<VARIANT> {
    invoke(obj, name, DISPATCH_METHOD.0, args)
}

/// Walk a property chain: `get_path(app, &["ActiveWindow", "View", "Slide"])`.
pub fn get_path(obj: &IDispatch, path: &[&str]) -> Result<IDispatch> {
    let mut cur = obj.clone();
    for name in path {
        let v = get(&cur, name)?;
        cur = as_dispatch(&v).with_context(|| format!("{name} is not an object"))?;
    }
    Ok(cur)
}

// ---- VARIANT extraction (tolerant of the numeric types Office actually emits) ----

pub fn as_dispatch(v: &VARIANT) -> Result<IDispatch> {
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        const VT_DISPATCH: u16 = 9;
        if inner.vt.0 != VT_DISPATCH {
            return Err(anyhow!("VARIANT vt={} is not IDispatch", inner.vt.0));
        }
        inner
            .Anonymous
            .pdispVal
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("null IDispatch"))
    }
}

pub fn as_f64(v: &VARIANT) -> Result<f64> {
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        match inner.vt.0 {
            4 => Ok(inner.Anonymous.fltVal as f64), // VT_R4 — Office points are Single
            5 => Ok(inner.Anonymous.dblVal),        // VT_R8
            2 => Ok(inner.Anonymous.iVal as f64),   // VT_I2
            3 => Ok(inner.Anonymous.lVal as f64),   // VT_I4
            vt => Err(anyhow!("VARIANT vt={vt} is not numeric")),
        }
    }
}

pub fn as_i32(v: &VARIANT) -> Result<i32> {
    Ok(as_f64(v)? as i32)
}

pub fn as_bool(v: &VARIANT) -> Result<bool> {
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        const VT_BOOL: u16 = 11;
        if inner.vt.0 == VT_BOOL {
            return Ok(inner.Anonymous.boolVal.as_bool());
        }
    }
    Ok(as_f64(v)? != 0.0)
}

pub fn as_string(v: &VARIANT) -> Result<String> {
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        const VT_BSTR: u16 = 8;
        if inner.vt.0 != VT_BSTR {
            return Err(anyhow!("VARIANT vt={} is not BSTR", inner.vt.0));
        }
        Ok(inner.Anonymous.bstrVal.to_string())
    }
}

// ---- VARIANT construction ----

pub fn v_i32(n: i32) -> VARIANT {
    VARIANT::from(n)
}

pub fn v_f32(f: f32) -> VARIANT {
    VARIANT::from(f)
}

pub fn v_str(s: &str) -> VARIANT {
    VARIANT::from(BSTR::from(s))
}

pub fn v_bool(b: bool) -> VARIANT {
    VARIANT::from(b)
}

pub fn v_dispatch(d: &IDispatch) -> VARIANT {
    VARIANT::from(d.clone())
}

/// A VT_BYREF|VT_I4 out-parameter VARIANT pointing at `slot`. The caller must keep `slot`
/// alive across the Invoke and must NOT VariantClear this (the pointee is borrowed).
///
/// # Safety contract (internal)
/// `slot` must outlive the returned VARIANT's use in a single `call_byref`.
pub fn v_byref_i32(slot: *mut i32) -> VARIANT {
    const VT_BYREF: u16 = 0x4000;
    const VT_I4: u16 = 3;
    let mut v = VARIANT::default();
    unsafe {
        let inner = &mut v.Anonymous.Anonymous;
        inner.vt = windows::Win32::System::Variant::VARENUM(VT_BYREF | VT_I4);
        inner.Anonymous.plVal = slot;
    }
    v
}

/// Explicitly clear a VARIANT that owns resources (BSTR/IDispatch results we're done with).
/// The windows crate's VARIANT implements Drop, so this is only needed for the byref
/// slots we must NOT auto-drop — kept for symmetry/documentation.
#[allow(dead_code)]
pub fn clear(v: &mut VARIANT) {
    unsafe {
        let _ = VariantClear(v);
    }
}

/// Title text of a top-level window ("Document7 - Word").
fn window_title(hwnd: usize) -> String {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    unsafe {
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(HWND(hwnd as *mut _), &mut buf);
        if n <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

/// Does an Office window `Caption` correspond to the OS window title? Captions are the
/// title minus the " - App" suffix ("Document7" for "Document7 - Word"), and a split
/// view appends ":1"/":2" to the caption — so match caption-prefix-of-title (and
/// tolerate the split suffix), case-insensitively.
pub(crate) fn caption_matches_title(caption: &str, title: &str) -> bool {
    let caption = caption.trim().to_lowercase();
    let title = title.trim().to_lowercase();
    if caption.is_empty() || title.is_empty() {
        return false;
    }
    let base = caption
        .rsplit_once(':')
        .filter(|(_, n)| n.chars().all(|c| c.is_ascii_digit()))
        .map(|(b, _)| b.trim_end())
        .unwrap_or(&caption);
    title.starts_with(base)
}

#[cfg(test)]
mod tests {
    use super::caption_matches_title;

    #[test]
    fn caption_title_matching() {
        assert!(caption_matches_title("Document7", "Document7 - Word"));
        assert!(caption_matches_title("School days", "School days - PowerPoint"));
        // Split view appends :N to the caption.
        assert!(caption_matches_title("Document7:2", "Document7 - Word"));
        // Case-insensitive.
        assert!(caption_matches_title("document7", "Document7 - Word"));
        // Different documents must NOT match.
        assert!(!caption_matches_title("Document1", "Document7 - Word"));
        assert!(!caption_matches_title("", "Document7 - Word"));
        assert!(!caption_matches_title("Document7", ""));
    }
}

/// Resolve the app's COM window that corresponds to OUR target hwnd — never assume
/// `ActiveWindow`. Word is SDI (each document owns a top-level window) and PowerPoint can
/// hold several presentation windows; resolving a pinned-but-not-active window through
/// `ActiveWindow` would search the WRONG document and point into the wrong window's
/// screen space. Iterate `Application.Windows` and match `Caption` against the hwnd's
/// title; fall back to `ActiveWindow` only when its caption matches too.
pub fn resolve_app_window(app: &IDispatch, hwnd: usize) -> Result<IDispatch> {
    let title = window_title(hwnd);
    let windows_col = get_path(app, &["Windows"])?;
    let count = as_i32(&get(&windows_col, "Count")?).unwrap_or(0);
    for i in 1..=count {
        let Ok(win) = get_indexed(&windows_col, "Item", vec![v_i32(i)]).and_then(|v| as_dispatch(&v))
        else {
            continue;
        };
        let caption = get(&win, "Caption")
            .and_then(|v| as_string(&v))
            .unwrap_or_default();
        if caption_matches_title(&caption, &title) {
            return Ok(win);
        }
    }
    let active = get_path(app, &["ActiveWindow"])?;
    let caption = get(&active, "Caption")
        .and_then(|v| as_string(&v))
        .unwrap_or_default();
    if caption_matches_title(&caption, &title) {
        return Ok(active);
    }
    Err(anyhow!(
        "no COM window matches target title {title:?} — refusing to use another window's geometry"
    ))
}
