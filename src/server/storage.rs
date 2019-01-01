use crate::util::buffer::Buffer;

pub struct Inner();

pub struct SharededBuffer {
    shards: Vec<Buffer<Inner>>,
}
