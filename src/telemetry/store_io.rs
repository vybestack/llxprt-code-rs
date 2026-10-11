//! Scoped byte accounting at existing artifact read/write boundaries.
use super::StoreOperation;
use std::cell::RefCell;
thread_local! { static CURRENT: RefCell<Option<StoreOperation>> = const { RefCell::new(None) }; }

pub(crate) fn capture<T>(f: impl FnOnce() -> T) -> (T, StoreOperation) {
    struct Restore(Option<StoreOperation>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let restore = Restore(CURRENT.with(|slot| slot.replace(Some(StoreOperation::default()))));
    let result = f();
    let operation = CURRENT.with(|slot| slot.borrow_mut().take().unwrap_or_default());
    drop(restore);
    (result, operation)
}

pub(crate) fn artifact(name: &str, bytes: usize, written: bool) {
    CURRENT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(operation) = slot.as_mut() else {
            return;
        };
        let counter = if name.ends_with(".log") {
            if written {
                &mut operation.segment_written_bytes
            } else {
                &mut operation.segment_read_bytes
            }
        } else if name == "session.manifest.json" {
            if written {
                &mut operation.manifest_written_bytes
            } else {
                &mut operation.manifest_read_bytes
            }
        } else if written {
            &mut operation.snapshot_written_bytes
        } else {
            &mut operation.snapshot_read_bytes
        };
        *counter = counter.saturating_add(bytes as u64);
    });
}
