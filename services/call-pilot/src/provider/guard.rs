//! Parent revocation is a direct transport fence, independent of task scheduling.
use crate::engine::ProviderError;
use std::{
    future::{Future, poll_fn},
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
    time::Instant,
};
use tokio::sync::watch;

pub(super) struct ExternalRevocation {
    parents: Vec<watch::Receiver<bool>>,
    latched: AtomicBool,
}
impl ExternalRevocation {
    pub fn new(parents: Vec<watch::Receiver<bool>>) -> Result<Self, ProviderError> {
        if parents.len() > 2 {
            return Err(ProviderError::Protocol);
        }
        Ok(Self {
            parents,
            latched: AtomicBool::new(false),
        })
    }
    pub fn revoked(&self) -> bool {
        if self.latched.load(Ordering::Acquire) {
            return true;
        }
        if self
            .parents
            .iter()
            .any(|parent| *parent.borrow() || parent.has_changed().is_err())
        {
            self.latched.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }
    async fn cancelled(&self) {
        let mut parents = self.parents.clone();
        loop {
            if self.revoked() {
                return;
            }
            match parents.as_mut_slice() {
                [] => std::future::pending::<()>().await,
                [first] => {
                    let _ = first.changed().await;
                }
                [first, second] => {
                    tokio::select! { biased; _=first.changed()=>{}, _=second.changed()=>{} }
                }
                _ => unreachable!("construction bounds parent receivers"),
            }
        }
    }
}

#[cfg(test)]
pub(super) async fn active<F: Future>(
    at: Instant,
    local: &mut watch::Receiver<bool>,
    global: &mut watch::Receiver<bool>,
    external: &ExternalRevocation,
    future: F,
) -> Result<F::Output, ProviderError> {
    until_admitted(at, local, global, external, None, future).await
}

/// Each stream retains its startup fence until Ready is successfully released.
/// Passing None only after that point preserves its existing source drain.
pub(super) async fn until_admitted<F: Future>(
    at: Instant,
    local: &mut watch::Receiver<bool>,
    global: &mut watch::Receiver<bool>,
    external: &ExternalRevocation,
    admission: Option<&ExternalRevocation>,
    future: F,
) -> Result<F::Output, ProviderError> {
    tokio::pin!(future);
    let local_current = local.clone();
    let global_current = global.clone();
    let checked = poll_fn(|cx| {
        if external.revoked()
            || admission.is_some_and(ExternalRevocation::revoked)
            || *local_current.borrow()
            || *global_current.borrow()
            || Instant::now() >= at
        {
            Poll::Ready(Err(ProviderError::Deadline))
        } else {
            future.as_mut().poll(cx).map(Ok)
        }
    });
    let admission_cancelled = async {
        match admission {
            Some(fence) => fence.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! { biased;
        _=external.cancelled()=>Err(ProviderError::Deadline),
        _=admission_cancelled=>Err(ProviderError::Deadline),
        _=local.changed()=>Err(ProviderError::Deadline),
        _=global.changed()=>Err(ProviderError::Deadline),
        _=tokio::time::sleep_until(at.into())=>Err(ProviderError::Deadline),
        result=checked=>result,
    }
}
