//! Open an audio device and run a chain in its callback.
//!
//! [`run`] opens an output device at the rate and block size the caller asks
//! for and calls one `process(input, outputs)` closure, mono in and one or
//! two channels out, from the device callback and nowhere else. There is no
//! queue between the device and the chain, because a queue is latency and a
//! guitarist hears it. The callback spreads the result across however many
//! channels the device turned out to have.
//!
//! Live input is the one place a buffer cannot be avoided: a host delivers
//! input and output on two separate streams, so the input callback folds its
//! frames to mono and hands them to the output callback through
//! [`ring::Ring`], a lock-free single-producer single-consumer ring, and the
//! output callback reads them one block later.
//!
//! # Playing a chain
//!
//! ```no_run
//! // Opening a device needs hardware, so this compiles but does not run
//! // under `cargo test`.
//! use valverig_io::{Config, InputChannel, run};
//!
//! let mut amp = my_amp();
//! let stream = run(
//!     Config {
//!         sample_rate: 48_000,
//!         block: 128,
//!         live_input: true,
//!         output_channels: 2,
//!         output_device: None,
//!         input_device: None,
//!         input_channel: InputChannel::First,
//!     },
//!     move |input, outputs| amp.process(input, outputs),
//! )?;
//!
//! println!("playing on {}", stream.device);
//! std::thread::sleep(std::time::Duration::from_secs(10));
//! drop(stream); // and the device is released
//! # fn my_amp() -> Amp { Amp }
//! # struct Amp;
//! # impl Amp { fn process(&mut self, _: &[f32], _: &mut [&mut [f32]]) {} }
//! # Ok::<(), valverig_io::Error>(())
//! ```
//!
//! # What a caller has to know
//!
//! Nothing here resamples. [`Config::sample_rate`] is the rate the chain
//! itself needs; a device that does not offer it is an error, not something
//! to convert around, so a chain at another rate must be converted at its
//! own edges.
//!
//! A device is free to hand the callback more frames than the stream was
//! configured with; CoreAudio asks for 140 frames of a stream configured at
//! 128. So [`run`] cuts whatever arrives into pieces of at most
//! [`Config::block`] frames. A chain therefore never has to handle more than
//! it was prepared for, but it does have to handle less.
//!
//! With [`Config::live_input`], the chain sees the input exactly one
//! [`Config::block`] late and there is no way to ask for less. Input and
//! output are also two clocks: pick one device for both, or accept that two
//! that disagree by even a few parts per million will slowly fill the ring
//! (latency that grows through a long session) or empty it (input arriving as
//! silence). Nothing here regulates that drift yet.
//!
//! There is no reset and no route-change notification. When a device
//! disappears or a headset is unplugged, drop the [`Stream`] and open a new
//! one; what the ring was holding goes with it.

#![deny(missing_docs)]

mod denormals;
mod devices;
mod error;
mod frames;
pub mod ring;
mod stream;

pub use devices::{DeviceInfo, Devices, devices};
pub use error::{Direction, Error, Result};
pub use stream::{Config, InputChannel, MAX_BLOCK, Stream, run};
