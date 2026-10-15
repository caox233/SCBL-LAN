//! One asynchronous invitation acceptance at a time; render never waits for RPC.
use crate::api::Error;
use crossbeam_channel::{Receiver, TryRecvError};
use server_api::misc::InviteEvent;
use std::collections::VecDeque;

type Reply = Result<(bool, i64), Error>;
struct Pending {
    event: InviteEvent,
    reply: Receiver<Reply>,
}
#[derive(Default)]
pub(super) struct AcceptanceQueue {
    waiting: VecDeque<InviteEvent>,
    active: Option<Pending>,
}
impl AcceptanceQueue {
    pub fn submit(&mut self, event: InviteEvent) {
        // IDs are local invitation generations, not vector indices. Redelivery
        // or a repeated F5 must not consume the same server invitation twice.
        if self.active.as_ref().is_some_and(|p| p.event.id == event.id)
            || self.waiting.iter().any(|p| p.id == event.id)
        {
            return;
        }
        self.waiting.push_back(event);
        self.start_next();
    }
    fn start_next(&mut self) {
        if self.active.is_some() {
            return;
        }
        let Some(event) = self.waiting.pop_front() else {
            return;
        };
        let id = event.id;
        let (tx, reply) = crossbeam_channel::bounded(1);
        self.active = Some(Pending { event, reply });
        match crate::api::runtime() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    let response = crate::api::accept_invite_async(id).await;
                    let _ = tx.send(response);
                });
            }
            Err(error) => {
                let _ = tx.send(Err(error));
            }
        }
    }
    pub fn poll(&mut self) -> Option<(InviteEvent, Reply)> {
        // The adapter applies the previous completion before polling again, so
        // native event dispatch keeps the original invitation ordering.
        self.start_next();
        let response = match self.active.as_ref()?.reply.try_recv() {
            Ok(response) => response,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(Error::IO(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "invitation acceptance worker ended",
            ))),
        };
        Some((
            self.active.take().expect("active reply existed").event,
            response,
        ))
    }
}

#[cfg(test)]
#[path = "acceptance_tests.rs"]
mod tests;
