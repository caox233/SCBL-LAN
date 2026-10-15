//! External issue45 send leaf only; no socket I/O when capture is active.
use std::cell::RefCell;
std::thread_local! {
    static CAPTURE: RefCell<Option<Vec<Vec<u8>>>> = const { RefCell::new(None) };
}
pub struct Capture;
impl Capture {
    pub fn begin() -> Self {
        CAPTURE.with(|slot| { let mut slot = slot.borrow_mut(); assert!(slot.is_none()); *slot = Some(Vec::new()); });
        Self
    }
    pub fn take(&self) -> Vec<Vec<u8>> {
        CAPTURE.with(|slot| std::mem::take(slot.borrow_mut().as_mut().expect("capture active")))
    }
}
impl Drop for Capture {
    fn drop(&mut self) { CAPTURE.with(|slot| { *slot.borrow_mut() = None; }); }
}
pub(super) fn collect(data: &[u8]) -> Option<usize> {
    CAPTURE.with(|slot| slot.borrow_mut().as_mut().map(|frames| {
        frames.push(data.to_vec());
        data.len()
    }))
}
