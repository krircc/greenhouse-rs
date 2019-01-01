//! Concurrent multiple-producer multiple-consumer queues based on circular buffer.

/// Bounded MPMC queue based on fixed-sized concurrent circular buffer.
///
/// The implementation is based on Dmitry Vyukov's bounded MPMC queue:
///
/// * http://www.1024cores.net/home/lock-free-algorithms/queues/bounded-mpmc-queue
/// * https://docs.google.com/document/d/1yIAYmbvL3JxOKOjuCyon7JhW4cSv1wy5hC0ApeGMV9s/pub
///
/// # Examples
///
/// ```
/// use crossbeam_circbuf::mpmc::bounded::{Queue, TryRecv};
/// use std::thread;
/// use std::sync::Arc;
///
/// let q = Arc::new(Queue::<char>::new(16));
/// let r = q.clone();
///
/// q.send('a').unwrap();
/// r.send('b').unwrap();
///
/// assert_eq!(q.recv(), Some('a'));
/// assert_eq!(r.recv(), Some('b'));
/// ```
pub mod bounded {
    use std::marker::PhantomData;
    use std::mem::ManuallyDrop;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use crossbeam::utils::CachePadded;

    use crate::util::buffer::Buffer;
    pub use crate::util::buffer::TryRecv;

    /// A bounded MPMC queue.
    #[derive(Debug)]
    pub struct Queue<T> {
        /// The head index from which values are popped.
        ///
        /// The lap of the head index is always an odd number.
        head: CachePadded<AtomicUsize>,

        /// The tail index into which values are pushed.
        ///
        /// The lap in the tail is always an even number.
        tail: CachePadded<AtomicUsize>,

        /// The underlying buffer.
        buffer: Buffer<T>,

        /// The capacity of the queue.
        cap: usize,

        /// Indicates that dropping an `ArrayQueue<T>` may drop values of type `T`.
        _marker: PhantomData<T>,
    }

    unsafe impl<T: Send> Sync for Queue<T> {}
    unsafe impl<T: Send> Send for Queue<T> {}

    impl<T> Queue<T> {
        /// Creates a new queue of capacity at least `cap`.
        pub fn new(cap: usize) -> Self {
            // One lap is the smallest power of two greater than or equal to `cap`.
            let lap = cap.next_power_of_two();

            // Head is initialized to `{ lap: 1, offset: 0 }`.
            // Tail is initialized to `{ lap: 0, offset: 0 }`.
            let head = lap;
            let tail = 0;

            // Allocate a buffer of `lap` slots.
            let buffer = Buffer::new(lap);

            // Initialize stamps in the slots.
            for i in 0..lap {
                unsafe {
                    // Set the index to `{ lap: 0, offset: i }`.
                    buffer.write_index(i, Ordering::Relaxed);
                }
            }

            Self {
                head: CachePadded::new(AtomicUsize::new(head)),
                tail: CachePadded::new(AtomicUsize::new(tail)),
                buffer,
                cap,
                _marker: PhantomData,
            }
        }

        /// Returns the size of one lap.
        #[inline]
        pub fn lap(&self) -> usize {
            self.buffer.cap()
        }

        /// Returns the capacity of the queue.
        #[inline]
        pub fn cap(&self) -> usize {
            self.cap
        }

        /// Attempts to send a value to the queue.
        pub fn send(&self, value: T) -> Result<(), T> {
            loop {
                // Loads the tail and deconstruct it.
                let tail = self.tail.load(Ordering::Acquire);
                let offset = tail & (self.lap() - 1);
                let lap = tail & !(self.lap() - 1);

                // Inspects the corresponding slot.
                let index = unsafe { self.buffer.read_index(offset, Ordering::Acquire) };

                // If the tail and the stamp match, we may attempt to push.
                if tail == index {
                    let new_tail = if offset + 1 < self.cap() {
                        // Same lap, incremented index.
                        // Set to `{ lap: lap, offset: offset + 1 }`.
                        tail + 1
                    } else {
                        // Two laps forward, index wraps around to zero.
                        // Set to `{ lap: lap.wrapping_add(2), offset: 0 }`.
                        lap.wrapping_add(self.lap().wrapping_mul(2))
                    };

                    // Tries moving the tail.
                    if self
                        .tail
                        .compare_exchange_weak(tail, new_tail, Ordering::AcqRel, Ordering::Relaxed)
                        .is_ok()
                    {
                        // Writes the value into the slot and update the stamp.
                        unsafe { self.buffer.write(index.wrapping_add(self.lap()), value) };
                        return Ok(());
                    }
                // But if the slot lags one lap behind the tail...
                } else if index.wrapping_add(self.lap()) == tail {
                    let head = self.head.load(Ordering::Acquire);

                    // ...and if the head lags one lap behind the tail as well...
                    if head.wrapping_add(self.lap()) == tail {
                        // ...then the queue is full.
                        return Err(value);
                    }
                }
            }
        }

        /// Attempts to pop a value from the queue.
        pub fn recv(&self) -> Option<T> {
            loop {
                // Loads the head and deconstruct it.
                let head = self.head.load(Ordering::Acquire);
                let offset = head & (self.lap() - 1);
                let lap = head & !(self.lap() - 1);

                // Inspects the corresponding slot.
                let index = unsafe { self.buffer.read_index(offset, Ordering::Acquire) };

                // If the the head and the stamp match, we may attempt to pop.
                if head == index {
                    let new = if offset + 1 < self.cap() {
                        // Same lap, incremented index.
                        // Set to `{ lap: lap, offset: offset + 1 }`.
                        head + 1
                    } else {
                        // Two laps forward, index wraps around to zero.
                        // Set to `{ lap: lap.wrapping_add(2), offset: 0 }`.
                        lap.wrapping_add(self.lap().wrapping_mul(2))
                    };

                    // Tries moving the head.
                    if self
                        .head
                        .compare_exchange_weak(head, new, Ordering::AcqRel, Ordering::Relaxed)
                        .is_ok()
                    {
                        // Reads the value from the slot and update the stamp.
                        let value = unsafe { self.buffer.read_value(index) };
                        unsafe {
                            self.buffer
                                .write_index(index.wrapping_add(self.lap()), Ordering::Release)
                        };
                        return Some(ManuallyDrop::into_inner(value));
                    }
                // But if the slot lags one lap behind the head...
                } else if index.wrapping_add(self.lap()) == head {
                    let tail = self.tail.load(Ordering::Acquire);

                    // ...and if the tail lags one lap behind the head as well, that means the queue
                    // is empty.
                    if tail.wrapping_add(self.lap()) == head {
                        return None;
                    }
                }
            }
        }

        /// Returns `true` if the queue is empty.
        ///
        /// Inaccurate in the presence of concurrent method invocations.
        pub fn is_empty(&self) -> bool {
            let head = self.head.load(Ordering::Relaxed);
            let tail = self.tail.load(Ordering::Relaxed);

            // Is the tail lagging one lap behind head?
            //
            // Note: If the head changes just before we load the tail, that means there was a moment
            // when the queue was not empty, so it is safe to just return `false`.
            tail.wrapping_add(self.lap()) == head
        }

        /// Returns `true` if the queue is full.
        ///
        /// Inaccurate in the presence of concurrent method invocations.
        pub fn is_full(&self) -> bool {
            let tail = self.tail.load(Ordering::Relaxed);
            let head = self.head.load(Ordering::Relaxed);

            // Is the head lagging one lap behind tail?
            //
            // Note: If the tail changes just before we load the head, that means there was a moment
            // when the queue was not full, so it is safe to just return `false`.
            head.wrapping_add(self.lap()) == tail
        }

        /// Returns the current number of elements inside the queue.
        ///
        /// Inaccurate in the presence of concurrent method invocations.
        pub fn len(&self) -> usize {
            // Load the tail, then load the head.
            let tail = self.tail.load(Ordering::Relaxed);
            let head = self.head.load(Ordering::Relaxed);

            let hix = head & (self.lap() - 1);
            let tix = tail & (self.lap() - 1);

            if hix < tix {
                tix - hix
            } else if hix > tix {
                self.cap() - hix + tix
            } else if tail.wrapping_add(self.lap()) == head {
                0
            } else {
                self.cap()
            }
        }
    }

    impl<T> Drop for Queue<T> {
        fn drop(&mut self) {
            // Get the index of the head.
            let hix = self.head.load(Ordering::Relaxed) & (self.lap() - 1);

            // Loop over all slots that hold a message and drop them.
            for i in 0..self.len() {
                // Compute the index of the next slot holding a message.
                let index = if hix + i < self.cap() {
                    hix + i
                } else {
                    hix + i - self.cap()
                };

                unsafe {
                    let mut value = self.buffer.read_unchecked(index);
                    ManuallyDrop::drop(&mut value);
                }
            }
        }
    }
}
