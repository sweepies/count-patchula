//! Which work sent credentials. What a request carrying an operator entry's
//! credentials or an account's token finds may be private, so the work that
//! made it records that, wherever in the work the request was made and
//! whether its answer came from the cache, whose entries are kept apart by
//! the credentials that fetched them.
use std::{cell::Cell, future::Future};

tokio::task_local! {
    static CREDENTIALED: Cell<bool>;
}

/// Run `work`, telling whether any request it made carried credentials. A
/// `tracked` inside another passes what it saw on to the outer one.
pub async fn tracked<F: Future>(work: F) -> (F::Output, bool) {
    let (output, credentialed) = CREDENTIALED
        .scope(Cell::new(false), async {
            let output = work.await;
            (output, CREDENTIALED.with(Cell::get))
        })
        .await;
    if credentialed {
        sent();
    }
    (output, credentialed)
}

/// Record that the current work sent credentials.
pub(crate) fn sent() {
    let _ = CREDENTIALED.try_with(|credentialed| credentialed.set(true));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn work_records_what_it_and_inner_work_sent() {
        let (_, plain) = tracked(async {}).await;
        assert!(!plain);
        let (_, outer) = tracked(async {
            let (_, inner) = tracked(async { sent() }).await;
            assert!(inner);
        })
        .await;
        assert!(outer);
        // Outside any work, recording does nothing.
        sent();
    }
}
