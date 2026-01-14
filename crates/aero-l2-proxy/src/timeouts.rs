use std::{future::Future, time::Duration};

/// Applies a Tokio `timeout` when `dur` is set, otherwise awaits the future normally.
pub(crate) async fn timeout_opt<T, F>(
    dur: Option<Duration>,
    fut: F,
) -> Result<T, tokio::time::error::Elapsed>
where
    F: Future<Output = T>,
{
    match dur {
        Some(dur) => tokio::time::timeout(dur, fut).await,
        None => Ok(fut.await),
    }
}

#[cfg(test)]
mod tests {
    use super::timeout_opt;
    use std::time::Duration;

    #[tokio::test(flavor = "current_thread")]
    async fn timeout_opt_times_out_when_enabled() {
        tokio::time::pause();

        let handle = tokio::spawn(timeout_opt(
            Some(Duration::from_secs(1)),
            tokio::time::sleep(Duration::from_secs(10)),
        ));

        // Ensure the spawned task is polled at least once so the timers are registered.
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_secs(2)).await;

        let res = handle.await.unwrap();
        assert!(res.is_err(), "expected timeout_opt to time out");
    }
}
