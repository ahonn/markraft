//! A bounded work queue between synchronous layout and background typesetting.

use std::{
    cell::RefCell,
    collections::{HashSet, VecDeque},
};

use crate::math::{MathCache, MathError, MathRequest, RenderedMath};

const MAX_PENDING: usize = 256;
const BATCH_SIZE: usize = 16;

#[derive(Default)]
pub(crate) struct Maths(RefCell<State>);

#[derive(Default)]
struct State {
    cache: MathCache,
    pending: HashSet<MathRequest>,
    queue: VecDeque<MathRequest>,
    busy: bool,
}

impl Maths {
    pub(crate) fn get(&self, request: &MathRequest) -> Option<Result<RenderedMath, MathError>> {
        let mut state = self.0.borrow_mut();
        if let Some(result) = state.cache.get(request) {
            return Some(result.clone());
        }
        if state.pending.len() < MAX_PENDING && state.pending.insert(request.clone()) {
            state.queue.push_back(request.clone());
        }
        None
    }

    pub(crate) fn take_requests(&self) -> Vec<MathRequest> {
        let mut state = self.0.borrow_mut();
        if state.busy || state.queue.is_empty() {
            return Vec::new();
        }
        state.busy = true;
        let count = state.queue.len().min(BATCH_SIZE);
        state.queue.drain(..count).collect()
    }

    pub(crate) fn finish(&self, results: Vec<(MathRequest, Result<RenderedMath, MathError>)>) {
        let mut state = self.0.borrow_mut();
        for (request, result) in results {
            state.pending.remove(&request);
            state.cache.insert(request, result);
        }
        state.busy = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_deduplicates_work_and_reuses_completed_errors() {
        let maths = Maths::default();
        let request = MathRequest::new("\\invalidCommand", false, 16., 1., gpui::black());
        assert!(maths.get(&request).is_none());
        assert!(maths.get(&request).is_none());
        assert_eq!(maths.take_requests(), vec![request.clone()]);
        assert!(maths.take_requests().is_empty());
        maths.finish(vec![(request.clone(), crate::math::render_math(&request))]);
        assert!(maths.get(&request).unwrap().is_err());
        assert!(maths.take_requests().is_empty());
    }

    #[test]
    fn pending_work_and_each_batch_are_bounded() {
        let maths = Maths::default();
        for index in 0..MAX_PENDING + 10 {
            maths.get(&MathRequest::new(
                index.to_string(),
                false,
                16.,
                1.,
                gpui::black(),
            ));
        }
        assert_eq!(maths.0.borrow().pending.len(), MAX_PENDING);
        assert_eq!(maths.take_requests().len(), BATCH_SIZE);
        assert!(maths.take_requests().is_empty());
    }
}
