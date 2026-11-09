//! Where the replay library's progress and warning lines go: standard error, unless the
//! caller sets a hook, for example to send them through a writer that never blocks.
use std::{cell::Cell, sync::OnceLock};

type Hook = Box<dyn Fn(String) + Send + Sync>;
static HOOK: OnceLock<Hook> = OnceLock::new();
thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// Sends every later progress and warning line to `hook` instead of standard error.
///
/// The hook is process-wide and permanent: only the first call takes effect, and this returns
/// whether it did. It can run on several threads at once and must not block. A line logged
/// from inside the hook, and the line of a hook that panics, go to standard error instead.
pub(crate) fn set_log_hook(hook: impl Fn(String) + Send + Sync + 'static) -> bool {
    HOOK.set(Box::new(hook)).is_ok()
}

pub(crate) fn line(text: String) {
    let Some(hook) = HOOK.get().filter(|_| !IN_HOOK.with(Cell::get)) else {
        eprintln!("{text}");
        return;
    };
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            IN_HOOK.with(|flag| flag.set(false));
        }
    }
    IN_HOOK.with(|flag| flag.set(true));
    let _leave = Leave;
    let copy = text.clone();
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook(copy))).is_err() {
        eprintln!("{text}");
    }
}

/// Writes one line through the hook. Callers must not hold a lock while using it.
macro_rules! note {
    ($($arg:tt)*) => { $crate::replay::log::line(format!($($arg)*)) };
}
