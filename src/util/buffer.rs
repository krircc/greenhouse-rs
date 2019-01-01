use std::cell::UnsafeCell;
use std::mem;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

/// A slot in buffer.
#[derive(Debug)]
pub struct Slot<T> {
    /// The index of this slot's data.
    ///
    /// An index consists of an offset into the buffer and a lap, but packed into a single `usize`.
    /// The lower bits represent the offset, while the upper bits represent the lap.  For example if
    /// the buffer capacity is 4, then index 6 represents offset 2 in lap 1.
    index: AtomicUsize,

    /// The value in this slot.
    data: UnsafeCell<mem::ManuallyDrop<T>>,
}

/// A buffer that holds values in a queue.
///
/// This is just a buffer---dropping an instance of this struct will *not* drop the internal values.
#[derive(Debug)]
pub struct Buffer<T> {
    /// Pointer to the allocated memory.
    ///
    /// The allocated memory exhibit data races by `read`, `write`, `read_index`, and `write_index`,
    /// which technically invoke undefined behavior.  We should be using relaxed accesses, but that
    /// would cost too much performance.  Hence, as a HACK, we use volatile accesses instead.
    /// Experimental evidence shows that this works.
    ptr: *mut Slot<T>,

    /// Capacity of the buffer. Always a power of two.
    cap: usize,
}

impl<T> Buffer<T> {
    /// Allocates a new buffer with the specified capacity.
    pub fn new(cap: usize) -> Self {
        // `cap` should be a power of two.
        debug_assert_eq!(cap, cap.next_power_of_two());

        // Creates a buffer.
        let mut v = Vec::<Slot<T>>::with_capacity(cap);
        let ptr = v.as_mut_ptr();
        mem::forget(v);

        // Marks all entries invalid.
        unsafe {
            for i in 0..cap {
                // Index `i + 1` for the `i`-th entry is invalid; only the indexes of the form `i +
                // N * cap` is valid.
                (*ptr.offset(i as isize)).index = AtomicUsize::new(i + 1);
            }
        }

        Buffer { ptr, cap }
    }
}

impl<T> Drop for Buffer<T> {
    fn drop(&mut self) {
        unsafe {
            drop(Vec::from_raw_parts(self.ptr, 0, self.cap));
        }
    }
}

impl<T> Buffer<T> {
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Returns a pointer to the slot at the specified `index`.
    pub unsafe fn at(&self, index: usize) -> *mut Slot<T> {
        // `array.size()` is always a power of two.
        self.ptr.offset((index & (self.cap - 1)) as isize)
    }

    /// Reads a value from the specified `index`.
    ///
    /// Returns `Some(v)` if `v` is at `index`; or `None` if there's no valid value for `index`.
    pub unsafe fn read(&self, index: usize) -> Option<mem::ManuallyDrop<T>> {
        let slot = self.at(index);

        // Reads the index with `Acquire`.
        let i = (*slot).index.load(Ordering::Acquire);

        // If the index in the buffer mismatches with the queried index, there's no valid value.
        if index != i {
            return None;
        }

        // Returns the value.
        Some((*slot).data.get().read_volatile())
    }

    /// Reads a value from the specified `index` without checking the index.
    ///
    /// Returns the value at `index` regardless or whether it's valid or not.
    pub unsafe fn read_unchecked(&self, index: usize) -> mem::ManuallyDrop<T> {
        let slot = self.at(index);

        // Returns the value.
        (*slot).data.get().read_volatile()
    }

    /// Writes `value` into the specified `index`.
    pub unsafe fn write(&self, index: usize, value: T) {
        let slot = self.at(index);

        // Writes the value.
        (*slot)
            .data
            .get()
            .write_volatile(mem::ManuallyDrop::new(value));

        // Writes the index with `Release`.
        (*slot).index.store(index, Ordering::Release);
    }

    /// Reads the index from the specified slot.
    pub unsafe fn read_index(&self, offset: usize, ord: Ordering) -> usize {
        let slot = self.at(offset);
        (*slot).index.load(ord)
    }

    /// Writes the specified `index` in the slot.
    pub unsafe fn write_index(&self, index: usize, ord: Ordering) {
        let slot = self.at(index);
        (*slot).index.store(index, ord);
    }

    /// Reads the value from the specified slot.
    pub unsafe fn read_value(&self, offset: usize) -> mem::ManuallyDrop<T> {
        let slot = self.at(offset);
        (*slot).data.get().read_volatile()
    }
}

/// The return type for `try_recv` methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryRecv<T> {
    /// Received a value.
    Data(T),
    /// Not received a value because the buffer is empty.
    Empty,
    /// Lost the race to a concurrent operation. Try again.
    Retry,
}

impl<T> TryRecv<T> {
    /// Applies a function to the content of `TryRecv::Data`.
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> TryRecv<U> {
        match self {
            TryRecv::Data(v) => TryRecv::Data(f(v)),
            TryRecv::Empty => TryRecv::Empty,
            TryRecv::Retry => TryRecv::Retry,
        }
    }
}
