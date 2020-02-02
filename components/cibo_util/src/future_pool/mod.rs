mod builder;
mod metrics;

pub use self::builder::{Builder, Config};

use futures::future::FutureExt;
use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use actix_web::HttpResponse;
use actix_web::ResponseError;
use pin_project::pin_project;
use prometheus::{IntCounter, IntGauge};
use tokio::runtime::{Handle, Runtime};
use tokio::task::JoinHandle;

use crate::time::Instant;

const TICK_INTERVAL: Duration = Duration::from_secs(1);

thread_local! {
    static THREAD_LAST_TICK_TIME: Cell<Instant> = Cell::new(Instant::now_coarse());
}

#[pin_project]
pub struct FutureWrap<F>
where
    F: Future,
{
    #[pin]
    fut: F,
    #[pin]
    env: Arc<Env>,
}

pub fn wrap_future<F>(f: F, env: Arc<Env>) -> FutureWrap<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    FutureWrap { fut: f, env }
}

impl<F> Future for FutureWrap<F>
where
    F: Future,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        let env = this.env.clone();
        env.metrics_running_task_count.inc();

        let result = futures::ready!(this.fut.poll(ctx));
        env.metrics_handled_task_count.inc();
        env.metrics_running_task_count.dec();
        try_tick_thread(&env);
        Poll::Ready(result)
    }
}

pub struct Env {
    on_tick: Option<Box<dyn Fn() + Send + Sync>>,
    metrics_running_task_count: IntGauge,
    metrics_handled_task_count: IntCounter,
}

#[derive(Clone)]
pub struct FuturePool {
    pool: Arc<Runtime>,
    env: Arc<Env>,
    max_tasks: usize,
}

impl std::fmt::Debug for FuturePool {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(fmt, "FuturePool")
    }
}

impl crate::AssertSend for FuturePool {}
impl crate::AssertSync for FuturePool {}

impl FuturePool {
    /// Gets current running task count.
    #[inline]
    pub fn get_running_task_count(&self) -> usize {
        // As long as different future pool has different name prefix, we can safely use the value
        // in metrics.
        self.env.metrics_running_task_count.get() as usize
    }

    fn gate_spawn(&self) -> Result<(), ErrorPoolFull> {
        if self.max_tasks == std::usize::MAX {
            return Ok(());
        }

        let current_tasks = self.get_running_task_count();
        if current_tasks >= self.max_tasks {
            Err(ErrorPoolFull {
                current_tasks,
                max_tasks: self.max_tasks,
            })
        } else {
            Ok(())
        }
    }

    /*
    fn wrap_user_future<T, F, E>(&self, future_fn: T) -> T
    where
        T: FnOnce() -> impl Future<Output = Result<F, E>>,
    {
        let env = self.env.clone();
        env.metrics_running_task_count.inc();

        move || async {
            let result = future_fn().await;
            env.metrics_handled_task_count.inc();
            env.metrics_running_task_count.dec();
            try_tick_thread(&env);
            result
        }
    }
    */

    /// Spawns a future in the pool.
    pub fn spawn<T>(&self, future_fn: T) -> Result<JoinHandle<T::Output>, ErrorPoolFull>
    where
        T: Future + Send + 'static,
        T::Output: Send + 'static,
    {
        self.gate_spawn()?;
        let futures = wrap_future(future_fn, self.env.clone());
        // self.wrap_user_future(future_fn)
        Ok(self.pool.spawn(futures))
    }
}

/// Tries to trigger a tick in current thread.
///
/// This function is effective only when it is called in thread pool worker
/// thread.
#[inline]
fn try_tick_thread(env: &Env) {
    THREAD_LAST_TICK_TIME.with(|tls_last_tick| {
        let now = Instant::now_coarse();
        let last_tick = tls_last_tick.get();
        if now.duration_since(last_tick) < TICK_INTERVAL {
            return;
        }
        tls_last_tick.set(now);
        if let Some(f) = &env.on_tick {
            f();
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ErrorPoolFull {
    pub current_tasks: usize,
    pub max_tasks: usize,
}

impl std::fmt::Display for ErrorPoolFull {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(fmt, "future pool is full")
    }
}

impl ResponseError for ErrorPoolFull {
    fn error_response(&self) -> HttpResponse {
        HttpResponse::BadRequest().body("future pool is full".to_string())
    }
}

impl std::error::Error for ErrorPoolFull {
    fn description(&self) -> &str {
        "future pool is full"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread;

    use futures::future;

    fn spawn_future_and_wait(pool: &FuturePool, duration: Duration) {
        pool.spawn_handle(move || {
            thread::sleep(duration);
            future::ok::<_, ()>(())
        })
        .unwrap()
        .wait()
        .unwrap();
    }

    fn spawn_future_without_wait(pool: &FuturePool, duration: Duration) {
        pool.spawn(move || {
            thread::sleep(duration);
            future::ok::<_, ()>(())
        })
        .unwrap();
    }

    #[test]
    fn test_tick() {
        let tick_sequence = Arc::new(AtomicUsize::new(0));

        let tick_sequence2 = tick_sequence.clone();
        let (tx, rx) = mpsc::sync_channel(1000);

        let pool = Builder::new()
            .pool_size(1)
            .on_tick(move || {
                let seq = tick_sequence2.fetch_add(1, Ordering::SeqCst);
                tx.send(seq).unwrap();
            })
            .build();

        assert!(rx.try_recv().is_err());

        // Tick is not emitted since there is no task
        thread::sleep(TICK_INTERVAL * 2);
        assert!(rx.try_recv().is_err());

        // Tick is emitted because long enough time has elapsed since pool is created
        spawn_future_and_wait(&pool, TICK_INTERVAL / 20);
        assert!(rx.try_recv().is_err());

        spawn_future_and_wait(&pool, TICK_INTERVAL / 20);
        spawn_future_and_wait(&pool, TICK_INTERVAL / 20);
        spawn_future_and_wait(&pool, TICK_INTERVAL / 20);
        spawn_future_and_wait(&pool, TICK_INTERVAL / 20);

        // So far we have only elapsed TICK_INTERVAL * 0.2, so no ticks so far.
        assert!(rx.try_recv().is_err());

        // Even if long enough time has elapsed, tick is not emitted until next task arrives
        thread::sleep(TICK_INTERVAL * 2);
        assert!(rx.try_recv().is_err());

        spawn_future_and_wait(&pool, TICK_INTERVAL / 20);
        assert_eq!(rx.try_recv().unwrap(), 0);
        assert!(rx.try_recv().is_err());

        // Tick is not emitted if there is no task
        thread::sleep(TICK_INTERVAL * 2);
        assert!(rx.try_recv().is_err());

        // Tick is emitted since long enough time has passed
        spawn_future_and_wait(&pool, TICK_INTERVAL / 20);
        assert_eq!(rx.try_recv().unwrap(), 1);
        assert!(rx.try_recv().is_err());

        // Tick is emitted immediately after a long task
        spawn_future_and_wait(&pool, TICK_INTERVAL * 2);
        assert_eq!(rx.try_recv().unwrap(), 2);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_tick_multi_thread() {
        let tick_sequence = Arc::new(AtomicUsize::new(0));

        let tick_sequence2 = tick_sequence.clone();
        let (tx, rx) = mpsc::sync_channel(1000);

        let pool = Builder::new()
            .pool_size(2)
            .on_tick(move || {
                let seq = tick_sequence2.fetch_add(1, Ordering::SeqCst);
                tx.send(seq).unwrap();
            })
            .build();

        assert!(rx.try_recv().is_err());

        // Spawn two tasks, each will be processed in one worker thread.
        spawn_future_without_wait(&pool, TICK_INTERVAL / 2);
        spawn_future_without_wait(&pool, TICK_INTERVAL / 2);

        assert!(rx.try_recv().is_err());

        // Wait long enough time to trigger a tick.
        thread::sleep(TICK_INTERVAL * 2);

        assert!(rx.try_recv().is_err());

        // These two tasks should both trigger a tick.
        spawn_future_without_wait(&pool, TICK_INTERVAL);
        spawn_future_without_wait(&pool, TICK_INTERVAL / 2);

        // Wait until these tasks are finished.
        thread::sleep(TICK_INTERVAL * 2);

        assert_eq!(rx.try_recv().unwrap(), 0);
        assert_eq!(rx.try_recv().unwrap(), 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_handle_drop() {
        let pool = Builder::new().pool_size(1).build();

        let (tx, rx) = mpsc::sync_channel(10);

        let tx2 = tx.clone();
        pool.spawn(move || {
            thread::sleep(Duration::from_millis(200));
            tx2.send(11).unwrap();
            future::ok::<_, ()>(())
        })
        .unwrap();

        let tx2 = tx.clone();
        drop(
            pool.spawn_handle(move || {
                tx2.send(7).unwrap();
                future::ok::<_, ()>(())
            })
            .unwrap(),
        );

        thread::sleep(Duration::from_millis(500));

        assert_eq!(rx.try_recv().unwrap(), 11);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_handle_result() {
        let pool = Builder::new().pool_size(1).build();

        let handle = pool.spawn_handle(move || future::ok::<_, ()>(42));

        assert_eq!(handle.unwrap().wait().unwrap(), 42);
    }

    #[test]
    fn test_running_task_count() {
        let pool = Builder::new()
            .name_prefix("future_pool_for_running_task_test") // The name is important
            .pool_size(2)
            .build();

        assert_eq!(pool.get_running_task_count(), 0);

        spawn_future_without_wait(&pool, Duration::from_millis(500)); // f1
        assert_eq!(pool.get_running_task_count(), 1);

        spawn_future_without_wait(&pool, Duration::from_millis(1000)); // f2
        assert_eq!(pool.get_running_task_count(), 2);

        spawn_future_without_wait(&pool, Duration::from_millis(1500));
        assert_eq!(pool.get_running_task_count(), 3);

        thread::sleep(Duration::from_millis(700)); // f1 completed, f2 elapsed 700
        assert_eq!(pool.get_running_task_count(), 2);

        spawn_future_without_wait(&pool, Duration::from_millis(1500));
        assert_eq!(pool.get_running_task_count(), 3);

        thread::sleep(Duration::from_millis(2700));
        assert_eq!(pool.get_running_task_count(), 0);
    }

    fn spawn_long_time_future(
        pool: &FuturePool,
        id: u64,
        future_duration_ms: u64,
    ) -> Result<SpawnHandle<u64, ()>, ErrorPoolFull> {
        pool.spawn_handle(move || {
            thread::sleep(Duration::from_millis(future_duration_ms));
            future::ok::<u64, ()>(id)
        })
    }

    fn wait_on_new_thread<F>(
        sender: mpsc::Sender<std::result::Result<F::Item, F::Error>>,
        future: F,
    ) where
        F: Future + Send + 'static,
        F::Item: Send + 'static,
        F::Error: Send + 'static,
    {
        thread::spawn(move || {
            let r = future.wait();
            sender.send(r).unwrap();
        });
    }

    #[test]
    fn test_full() {
        let (tx, rx) = mpsc::channel();

        let read_pool = Builder::new()
            .name_prefix("future_pool_test_full")
            .pool_size(2)
            .max_tasks(4)
            .build();

        wait_on_new_thread(
            tx.clone(),
            spawn_long_time_future(&read_pool, 0, 5).unwrap(),
        );
        // not full
        assert_eq!(rx.recv().unwrap(), Ok(0));

        wait_on_new_thread(
            tx.clone(),
            spawn_long_time_future(&read_pool, 1, 100).unwrap(),
        );
        wait_on_new_thread(
            tx.clone(),
            spawn_long_time_future(&read_pool, 2, 200).unwrap(),
        );
        wait_on_new_thread(
            tx.clone(),
            spawn_long_time_future(&read_pool, 3, 300).unwrap(),
        );
        wait_on_new_thread(
            tx.clone(),
            spawn_long_time_future(&read_pool, 4, 400).unwrap(),
        );
        // no available results (running = 4)
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());

        // full
        assert!(spawn_long_time_future(&read_pool, 5, 100).is_err());

        // full
        assert!(spawn_long_time_future(&read_pool, 6, 100).is_err());

        // wait a future completes (running = 3)
        assert_eq!(rx.recv().unwrap(), Ok(1));

        // add new (running = 4)
        wait_on_new_thread(
            tx.clone(),
            spawn_long_time_future(&read_pool, 7, 5).unwrap(),
        );

        // full
        assert!(spawn_long_time_future(&read_pool, 8, 100).is_err());

        assert!(rx.recv().is_ok());
        assert!(rx.recv().is_ok());
        assert!(rx.recv().is_ok());
        assert!(rx.recv().is_ok());

        // no more results
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
    }
}
