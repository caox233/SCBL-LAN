//! Process-owned startup work. A claim covers creation and the worker lifetime.
use std::sync::atomic::{AtomicBool, Ordering};

static OVERLAY: AtomicBool = AtomicBool::new(false);
static FRIEND_REFRESH: AtomicBool = AtomicBool::new(false);

pub(super) struct Claim(Option<&'static AtomicBool>);

impl Claim {
    /// Graphics installation can retain native state even on failure. Once
    /// entered, it must not be attempted a second time in this process.
    pub(super) fn keep_for_process(mut self) {
        self.0 = None;
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if let Some(owner) = self.0 {
            owner.store(false, Ordering::Release);
        }
    }
}

fn claim(owner: &'static AtomicBool) -> Option<Claim> {
    owner.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).ok()?;
    Some(Claim(Some(owner)))
}

pub(super) fn claim_overlay() -> Option<Claim> {
    claim(&OVERLAY)
}

pub(super) fn claim_friend_refresh() -> Option<Claim> {
    claim(&FRIEND_REFRESH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_owner_excludes_reentry_and_drop_allows_retry() {
        static OWNER: AtomicBool = AtomicBool::new(false);
        let first = claim(&OWNER).unwrap();
        assert!(claim(&OWNER).is_none());
        drop(first);
        assert!(claim(&OWNER).is_some());
    }

    #[test]
    fn rejected_worker_closure_releases_creation_claim() {
        static OWNER: AtomicBool = AtomicBool::new(false);
        let owner = claim(&OWNER).unwrap();
        let worker = move || drop(owner);
        drop(worker);
        assert!(claim(&OWNER).is_some());
    }

    #[test]
    fn entered_graphics_installation_retains_process_owner() {
        static OWNER: AtomicBool = AtomicBool::new(false);
        claim(&OWNER).unwrap().keep_for_process();
        assert!(claim(&OWNER).is_none());
    }

    #[test]
    fn concurrent_callers_share_one_owner() {
        static OWNER: AtomicBool = AtomicBool::new(false);
        let workers: Vec<_> = (0..12).map(|_| std::thread::spawn(|| claim(&OWNER))).collect();
        let owners: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert_eq!(owners.iter().filter(|owner| owner.is_some()).count(), 1);
        drop(owners);
        assert!(claim(&OWNER).is_some());
    }
}
