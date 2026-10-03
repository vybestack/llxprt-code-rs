//! Guard the seeded identity against delegated test label/allocation drift.
use super::*;

thread_local! {
    static EXPECTED: std::cell::RefCell<Option<(String, bool)>> = const {
        std::cell::RefCell::new(None)
    };
}

pub(super) fn arm(id: &str) {
    EXPECTED.with(|expected| *expected.borrow_mut() = Some((id.to_owned(), false)));
}

pub(crate) fn observe(store: &SessionStore, cwd: &Path) {
    EXPECTED.with(|expected| {
        if let Some((id, seen)) = expected.borrow_mut().as_mut() {
            if !*seen {
                assert_eq!(
                    &store.session_id, id,
                    "delegated first reservation identity"
                );
                assert_eq!(store.session_dir, root().join("code-rs-sessions").join(id));
                super::trace(store, &root(), cwd);
                *seen = true;
            }
        }
    });
}

pub(super) fn finish() {
    EXPECTED.with(|expected| {
        let (_, seen) = expected.borrow_mut().take().expect("armed delegation");
        assert!(
            seen,
            "delegated case must reserve the seeded session identity"
        );
    });
}
