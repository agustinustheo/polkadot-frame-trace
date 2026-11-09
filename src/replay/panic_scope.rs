//! Route the process hook by thread; only panics inside our execution catch are quiet.
use std::{cell::Cell, panic::AssertUnwindSafe, sync::Once};
thread_local! {
    static CAUGHT: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn catch<T>(run: impl FnOnce() -> T) -> std::thread::Result<T> {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !CAUGHT.with(Cell::get) {
                previous(info);
            }
        }));
    });
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            CAUGHT.with(|flag| flag.set(self.0));
        }
    }
    let _restore = Restore(CAUGHT.with(|flag| flag.replace(true)));
    std::panic::catch_unwind(AssertUnwindSafe(run))
}
