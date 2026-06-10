//! Keep-warm — hold an active UIA event subscription on the target window so apps that build
//! their accessibility tree **lazily** keep it built for our locator.
//!
//! Some apps (Qt — VLC confirmed; and Chromium past its ~30 s auto-disable) only expose their
//! a11y tree while an *active assistive-technology client* is connected. Accessibility Insights is
//! such a client — which is why it sees VLC's menu while our one-shot UIA queries return 0
//! candidates. Registering a `StructureChanged` handler IS an active AT connection, so it makes the
//! app build and keep its tree. The handler itself is a no-op; we only want the live subscription.
//!
//! A single dedicated **MTA** thread owns the `UIAutomation` instance and the current registration
//! (so the COM objects aren't dropped) and re-targets it when the focused app changes. MTA means
//! OUTOFCONTEXT events arrive on UIA worker threads — no message pump is needed; this thread just
//! blocks waiting for re-target commands.

#[cfg(windows)]
mod imp {
    use parking_lot::Mutex;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::OnceLock;
    use uiautomation::events::{
        CustomStructureChangedEventHandlerFn, UIStructureChangeEventHandler,
    };
    use uiautomation::types::TreeScope;
    use uiautomation::{UIAutomation, UIElement};
    use windows::Win32::Foundation::HWND;

    enum Cmd {
        Warm(usize),
    }

    static TX: OnceLock<Mutex<Sender<Cmd>>> = OnceLock::new();

    /// Keep the a11y tree of `hwnd` warm (idempotent; re-targets off the previous window). Cheap —
    /// just sends to the keep-warm thread, which it starts on first use.
    pub fn warm(hwnd: usize) {
        if hwnd == 0 {
            return;
        }
        let tx = TX.get_or_init(|| Mutex::new(start_thread()));
        let _ = tx.lock().send(Cmd::Warm(hwnd));
    }

    fn start_thread() -> Sender<Cmd> {
        let (tx, rx) = channel::<Cmd>();
        let spawned = std::thread::Builder::new()
            .name("a11y-keepwarm".into())
            .spawn(move || {
                // UIAutomation::new() does CoInitializeEx(MTA); on MTA, OUTOFCONTEXT events are
                // delivered on UIA worker threads, so no message loop is needed here. This thread
                // exists only to OWN the automation + the live registration for the process'
                // lifetime (dropping them would tear down the subscription).
                let automation = match UIAutomation::new() {
                    Ok(a) => a,
                    Err(e) => {
                        log::warn!("keepwarm: UIAutomation init failed: {e}");
                        return;
                    }
                };
                let mut current: Option<(usize, UIElement, UIStructureChangeEventHandler)> = None;

                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        Cmd::Warm(hwnd) => {
                            if current.as_ref().map(|(h, _, _)| *h) == Some(hwnd) {
                                continue; // already subscribed to this window
                            }
                            if let Some((_, el, h)) = current.take() {
                                let _ = automation.remove_structure_changed_event_handler(&el, &h);
                            }
                            let raw = HWND(hwnd as *mut core::ffi::c_void);
                            let Ok(el) = automation.element_from_handle(raw.into()) else {
                                continue;
                            };
                            // No-op handler — we want the active subscription, not the events.
                            let cb: Box<CustomStructureChangedEventHandlerFn> =
                                Box::new(|_, _, _| Ok(()));
                            let handler: UIStructureChangeEventHandler = cb.into();
                            match automation.add_structure_changed_event_handler(
                                &el,
                                TreeScope::Subtree,
                                None,
                                &handler,
                            ) {
                                Ok(()) => {
                                    log::info!("keepwarm: subscribed {hwnd:#x}");
                                    current = Some((hwnd, el, handler));
                                }
                                Err(e) => log::warn!("keepwarm: subscribe {hwnd:#x} failed: {e}"),
                            }
                        }
                    }
                }
            });
        if spawned.is_err() {
            log::warn!("keepwarm: failed to spawn thread");
        }
        tx
    }
}

#[cfg(windows)]
pub fn warm(hwnd: usize) {
    imp::warm(hwnd);
}

#[cfg(not(windows))]
pub fn warm(_hwnd: usize) {}
