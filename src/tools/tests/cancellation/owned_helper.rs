//! Test verification owns its helper through the same pipe-backed scope as production
//! commands. Dropping a raw Child does not kill it; a scope also covers verifier death
//! (no Rust destructors) and cascades into a helper's independent tool scope.
use std::ops::{Deref, DerefMut};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

pub(super) struct OwnedHelper {
    child: Child,
    scope: Option<crate::process::TestScope>,
}

impl OwnedHelper {
    pub(super) fn spawn(command: &mut Command) -> Self {
        let scope = crate::process::TestScope::new().unwrap();
        scope.configure(command);
        let child = command.spawn().unwrap();
        Self {
            child,
            scope: Some(scope),
        }
    }

    pub(super) fn wait_until(&mut self, deadline: Instant) -> std::io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.child.try_wait()? {
                // Retain the scope until Drop, including after reap. Cancellation assertions
                // must observe production cleanup before the test owner's safety-net cleanup.
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "cancellation helper did not exit before deadline",
                ));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub(super) fn scope_pgid(&self) -> i32 {
        self.scope.as_ref().unwrap().pgid
    }
}

impl Deref for OwnedHelper {
    type Target = Child;
    fn deref(&self) -> &Self::Target {
        &self.child
    }
}

impl DerefMut for OwnedHelper {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}

impl Drop for OwnedHelper {
    fn drop(&mut self) {
        drop(self.scope.take());
        if let Err(error) = self.wait_until(Instant::now() + Duration::from_secs(5)) {
            eprintln!("cannot reap owned cancellation helper: {error}");
            std::process::abort();
        }
    }
}
