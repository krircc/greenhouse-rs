//! Concurrent single-producer queues based on circular buffer.
//!
//! [`CircBuf`] is a circular buffer, which is basically a fixed-sized array that has two ends: tx
//! and rx. A [`CircBuf`] can [`send`] values into the tx end and [`CircBuf::try_recv`] values from
//! the rx end. A [`CircBuf`] doesn't implement `Sync` so it cannot be shared among multiple
//! threads. However, it can create [`Receiver`]s, and those can be easily cloned, shared, and sent
//! to other threads. [`Receiver`]s can only [`Receiver::try_recv`] values from the rx end.
//!
//! Here's a visualization of a [`CircBuf`] of capacity 4, consisting of 2 values `a` and `b`.
//!
//! ```text
//!    ___
//!   | a | <- rx (CircBuf::try_recv, Receiver::try_recv)
//!   | b |
//!   |   | <- tx (CircBuf::send)
//!   |   |
//!    ¯¯¯
//! ```
//!
//! [`DynamicCircBuf`] is a dynamically growable and shrinkable circular buffer. Internally,
//! [`DynamicCircBuf`] has a [`CircBuf`] and resizes it when necessary.
//!
//!
//! # Usage: fair work-stealing schedulers
//!
//! This data structure can be used in fair work-stealing schedulers for multiple threads as
//! follows.
//!
//! Each thread owns a [`CircBuf`] (or [`DynamicCircBuf`]) and creates a [`Receiver`] that is shared
//! among all other threads (or creates one [`Receiver`] for each of the other threads).
//!
//! Each thread is executing a loop in which it attempts to [`CircBuf::try_recv`] some task from its
//! own [`CircBuf`] and perform it. If the buffer is empty, it attempts to [`Receiver::try_recv`]
//! work from other threads instead. When performing a task, a thread may produce more tasks by
//! [`send`]ing them to its buffer.
//!
//! It is worth noting that it is discouraged to use work-stealing deque for fair schedulers,
//! because its `pop()` may return a task that is just `push()`ed, effectively scheduling the same
//! work repeatedly.
//!
//! [`CircBuf`]: struct.CircBuf.html
//! [`DynamicCircBuf`]: struct.DynamicCircBuf.html
//! [`Receiver`]: struct.Receiver.html
//! [`send`]: struct.CircBuf.html#method.send
//! [`CircBuf::try_recv`]: struct.CircBuf.html#method.try_recv
//! [`Receiver::try_recv`]: struct.Receiver.html#method.try_recv

#[doc(hidden)] // for doc-tests
pub mod internal;

pub mod mc;
pub mod sc;
