//! Scoped cancellation for release commands. The handler only stores an atomic flag;
//! normal control flow performs descriptor and child cleanup before returning failure.
use std::sync::atomic::{AtomicBool, Ordering};

static CANCELLED: AtomicBool = AtomicBool::new(false);
extern "C" fn mark_cancelled(_: i32) {
    CANCELLED.store(true, Ordering::Relaxed);
}
unsafe extern "C" {
    fn signal(number: i32, handler: usize) -> usize;
}

pub struct Cancellation(Vec<(i32, usize)>);
impl Cancellation {
    pub fn install() -> Result<Self, String> {
        CANCELLED.store(false, Ordering::Relaxed);
        let mut scope = Self(Vec::new());
        for number in [1, 2, 15] {
            // Unix signal() installs a handler whose only operation is a lock-free atomic store.
            let prior = unsafe { signal(number, mark_cancelled as usize) };
            if prior == usize::MAX {
                return Err("install release cancellation handler".into());
            }
            scope.0.push((number, prior));
        }
        Ok(scope)
    }
}
impl Drop for Cancellation {
    fn drop(&mut self) {
        for (number, prior) in &self.0 {
            // Restore the handler captured for this signal while this scope owned it.
            unsafe {
                signal(*number, *prior);
            }
        }
    }
}
pub fn check() -> Result<(), String> {
    if CANCELLED.load(Ordering::Relaxed) {
        Err("release command cancelled".into())
    } else {
        Ok(())
    }
}
