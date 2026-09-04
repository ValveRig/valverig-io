//! A lock-free single-producer, single-consumer ring of samples.
//!
//! The hand-off from an input callback to an output callback. Samples are
//! stored as their bit patterns in `AtomicU32`s, which keeps the whole thing
//! safe code: the two counters are the only synchronisation, the producer
//! publishes with a release store of `head` and the consumer with a release
//! store of `tail`.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// A ring of `f32` samples for exactly one producer and one consumer.
///
/// [`push`](Ring::push) may be called from one thread and [`pop`](Ring::pop)
/// from one other, at the same time, and both are wait-free: neither ever
/// blocks, allocates or spins, which is what makes them safe to call from an
/// audio callback. Two threads pushing, or two popping, is not unsound, since
/// this is all safe code, but the samples come out wrong, so hold the
/// producing end in one place and the consuming end in another.
///
/// Neither end waits for the other. A push into a full ring takes what fits
/// and drops the rest; a pop from an empty one writes nothing. Both say how
/// many samples they moved, which is the only way to notice.
///
/// ```
/// use valverig_io::ring::Ring;
///
/// let ring = Ring::new(1000);
/// assert_eq!(ring.capacity(), 1024, "rounded up to a power of two");
///
/// assert_eq!(ring.push(&[0.1, 0.2, 0.3]), 3);
/// let mut block = [0.0f32; 4];
/// assert_eq!(ring.pop(&mut block), 3, "and the fourth is left alone");
/// assert_eq!(block, [0.1, 0.2, 0.3, 0.0]);
/// assert!(ring.is_empty());
/// ```
#[derive(Debug)]
pub struct Ring {
    buf: Box<[AtomicU32]>,
    mask: usize,
    /// Samples ever pushed.
    head: AtomicUsize,
    /// Samples ever popped.
    tail: AtomicUsize,
}

impl Ring {
    /// A ring holding at least `capacity` samples, rounded up to a power of
    /// two, and never fewer than two.
    ///
    /// Allocates and zeroes the whole ring, so build it before the streams
    /// start and never from a callback.
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(2).next_power_of_two();
        Self {
            buf: (0..cap).map(|_| AtomicU32::new(0)).collect(),
            mask: cap - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Samples the ring can hold.
    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Samples waiting to be popped.
    pub fn len(&self) -> usize {
        self.head.load(Ordering::Acquire) - self.tail.load(Ordering::Acquire)
    }

    /// Whether nothing is waiting.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Append as much of `src` as fits, and return how many samples were
    /// taken: fewer than `src.len()` when the consumer has fallen behind, and
    /// the rest of `src` is dropped rather than held back.
    ///
    /// The producing end only, and only one thread of it.
    pub fn push(&self, src: &[f32]) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let n = src.len().min(self.capacity() - (head - tail));
        for (i, &v) in src[..n].iter().enumerate() {
            self.buf[(head + i) & self.mask].store(v.to_bits(), Ordering::Relaxed);
        }
        self.head.store(head + n, Ordering::Release);
        n
    }

    /// Fill as much of `dst` as is waiting, and return how many samples were
    /// written. The rest of `dst` is left as it was, so a caller that needs
    /// silence there has to write it.
    ///
    /// The consuming end only, and only one thread of it.
    pub fn pop(&self, dst: &mut [f32]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let n = dst.len().min(head - tail);
        for (i, d) in dst[..n].iter_mut().enumerate() {
            *d = f32::from_bits(self.buf[(tail + i) & self.mask].load(Ordering::Relaxed));
        }
        self.tail.store(tail + n, Ordering::Release);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_come_out_in_order_and_wrap() {
        let r = Ring::new(6);
        assert_eq!(r.capacity(), 8);
        assert_eq!(r.push(&[1.0, 2.0, 3.0, 4.0, 5.0]), 5);
        let mut out = [0.0f32; 3];
        assert_eq!(r.pop(&mut out), 3);
        assert_eq!(out, [1.0, 2.0, 3.0]);
        // Wraps around the end of the storage.
        assert_eq!(r.push(&[6.0, 7.0, 8.0, 9.0, 10.0, 11.0]), 6);
        assert_eq!(r.len(), 8);
        assert_eq!(r.push(&[12.0]), 0, "full");
        let mut out = [0.0f32; 8];
        assert_eq!(r.pop(&mut out), 8);
        assert_eq!(out, [4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0]);
        assert!(r.is_empty());
        assert_eq!(r.pop(&mut out), 0, "empty leaves dst alone");
    }

    #[test]
    fn producer_and_consumer_on_two_threads_agree() {
        let r = std::sync::Arc::new(Ring::new(64));
        let total = 100_000usize;
        let producer = {
            let r = r.clone();
            std::thread::spawn(move || {
                let mut next = 0usize;
                while next < total {
                    let batch: Vec<f32> = (next..(next + 7).min(total)).map(|i| i as f32).collect();
                    let n = r.push(&batch);
                    next += n;
                    if n == 0 {
                        std::thread::yield_now();
                    }
                }
            })
        };
        let mut seen = 0usize;
        let mut buf = [0.0f32; 5];
        while seen < total {
            let n = r.pop(&mut buf);
            for &v in &buf[..n] {
                assert_eq!(v, seen as f32);
                seen += 1;
            }
            if n == 0 {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
    }
}
