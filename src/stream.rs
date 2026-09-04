//! Opening the devices and running a chain in the output callback.

use crate::denormals;
use crate::error::{Direction, Error};
use crate::frames::{Blocks, Fold};
use crate::ring::Ring;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, SampleRate, StreamConfig, SupportedBufferSize};
use std::sync::Arc;

/// The largest block [`run`] accepts, in frames.
///
/// Well above any block a driver is configured with in practice, and low
/// enough that a nonsense number in a settings file cannot ask for an
/// allocation that takes the process down.
pub const MAX_BLOCK: usize = 8192;

/// How many blocks of live input the ring between the two callbacks holds.
///
/// Enough that an input callback arriving late does not empty it, few enough
/// that it cannot quietly become latency.
const RING_BLOCKS: usize = 32;

/// Which of an input device's channels feeds the chain.
///
/// A device with only one channel gives that one whatever is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputChannel {
    /// The first channel.
    #[default]
    First,
    /// The second channel.
    Second,
    /// The mean of the first two.
    Both,
}

/// What to open.
///
/// There is deliberately no `Default`: a sample rate and a block size are the
/// two things a caller must have decided, and a default for either would be a
/// guess dressed up as a setting.
#[derive(Debug, Clone)]
pub struct Config {
    /// The rate the chain runs at, in Hz. Nothing here resamples, so this is
    /// the rate the chain itself needs, and the device must offer it or
    /// [`run`] fails.
    pub sample_rate: u32,
    /// The most frames the chain will be handed at once, between 1 and
    /// [`MAX_BLOCK`]. The device is asked for this too, but a device is free
    /// to hand back more, and whatever arrives is cut into pieces of at most
    /// this many frames before the chain sees it.
    pub block: usize,
    /// Also open an input device and feed the chain from it, one `block`
    /// behind. Without this the chain is handed silence and is expected to
    /// have a source of its own.
    pub live_input: bool,
    /// How many channels the chain produces, 1 or 2. The device's own
    /// channels are filled from them: a mono result goes to every channel, a
    /// stereo pair to the first two and the right of it to any beyond.
    pub output_channels: usize,
    /// The output device by name, from [`crate::DeviceInfo::name`], or the
    /// host's default.
    pub output_device: Option<String>,
    /// The input device by name, or the host's default. Ignored unless
    /// `live_input` is set.
    pub input_device: Option<String>,
    /// Which input channel feeds the chain.
    pub input_channel: InputChannel,
}

impl Config {
    fn check(&self) -> Result<(), Error> {
        if self.block == 0 || self.block > MAX_BLOCK {
            return Err(Error::Block(self.block));
        }
        if !matches!(self.output_channels, 1 | 2) {
            return Err(Error::OutputChannels(self.output_channels));
        }
        if self.sample_rate == 0 {
            return Err(Error::SampleRate);
        }
        Ok(())
    }
}

/// The open streams. Dropping it stops them.
pub struct Stream {
    _output: cpal::Stream,
    _input: Option<cpal::Stream>,
    /// The name of the output device that was opened, which is the default's
    /// name when [`Config::output_device`] was `None`.
    pub device: String,
}

fn has_configs(device: &cpal::Device, direction: Direction) -> bool {
    match direction {
        Direction::Input => device
            .supported_input_configs()
            .is_ok_and(|mut c| c.next().is_some()),
        Direction::Output => device
            .supported_output_configs()
            .is_ok_and(|mut c| c.next().is_some()),
    }
}

/// The device of that name that works in that direction, or the default.
///
/// A name can belong to two devices, the same box recording and playing, so
/// the direction is part of the search and not only of the error message.
fn find(
    host: &cpal::Host,
    name: Option<&str>,
    default: Option<cpal::Device>,
    direction: Direction,
) -> Result<cpal::Device, Error> {
    let Some(wanted) = name else {
        return default.ok_or(Error::NoDevice(direction));
    };
    host.devices()?
        .filter(|d| d.name().is_ok_and(|n| n == wanted))
        .find(|d| has_configs(d, direction))
        .ok_or_else(|| Error::NoSuchDevice(direction, wanted.to_string()))
}

/// An `f32` configuration at `rate`, with a fixed block where the device
/// allows one and the device's own choice where it does not.
fn pick(
    ranges: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
    direction: Direction,
    rate: u32,
    block: usize,
) -> Result<StreamConfig, Error> {
    let range = ranges
        .filter(|r| r.sample_format() == SampleFormat::F32)
        .find(|r| r.min_sample_rate().0 <= rate && rate <= r.max_sample_rate().0)
        .ok_or(Error::NoConfig(direction, rate))?;
    if range.channels() == 0 {
        return Err(Error::NoChannels(direction));
    }
    let buffer_size = match range.buffer_size() {
        SupportedBufferSize::Range { min, max }
            if *min as usize <= block && block <= *max as usize =>
        {
            BufferSize::Fixed(block as u32)
        }
        _ => BufferSize::Default,
    };
    Ok(StreamConfig {
        channels: range.channels(),
        sample_rate: SampleRate(rate),
        buffer_size,
    })
}

/// Open an output device, and with [`Config::live_input`] an input device
/// too, then run `process` from the output callback.
///
/// `process(input, outputs)` is handed a mono `input` and one slice per
/// [`Config::output_channels`], all of one length of at most
/// [`Config::block`] frames. It runs on the audio thread and on no other:
/// it must not allocate, lock, log or wait. Denormal results are flushed to
/// zero for it, and a non-finite sample it returns is replaced with silence
/// before it reaches the device.
///
/// The streams run until the returned [`Stream`] is dropped.
///
/// # Errors
///
/// The configuration is checked before the host is touched, so a bad
/// [`Config::block`], [`Config::output_channels`] or [`Config::sample_rate`]
/// fails without opening anything. After that: the named device not existing,
/// the device offering no `f32` configuration at the rate, and the stream
/// failing to build or start.
///
/// A device that fails *after* it has started, unplugged or reconfigured
/// underneath the stream, is not reported. cpal may raise that from the
/// audio thread, where this crate can neither log it nor allocate a message,
/// and there is nowhere lock-free to put it. A caller that needs to know
/// should watch for the callback going quiet.
///
/// ```no_run
/// // Opening a device needs hardware, so this compiles but does not run
/// // under `cargo test`.
/// use valverig_io::{Config, InputChannel, run};
///
/// let mut phase = 0.0f32;
/// let stream = run(
///     Config {
///         sample_rate: 48_000,
///         block: 128,
///         live_input: false,
///         output_channels: 2,
///         output_device: None,
///         input_device: None,
///         input_channel: InputChannel::First,
///     },
///     move |_input, outputs| {
///         for i in 0..outputs[0].len() {
///             let sample = (phase * std::f32::consts::TAU).sin() * 0.1;
///             phase = (phase + 440.0 / 48_000.0).fract();
///             for out in outputs.iter_mut() {
///                 out[i] = sample;
///             }
///         }
///     },
/// )?;
///
/// println!("playing on {}", stream.device);
/// std::thread::sleep(std::time::Duration::from_secs(1));
/// # Ok::<(), valverig_io::Error>(())
/// ```
pub fn run<F>(config: Config, mut process: F) -> Result<Stream, Error>
where
    F: FnMut(&[f32], &mut [&mut [f32]]) + Send + 'static,
{
    config.check()?;
    let block = config.block;
    let host = cpal::default_host();

    let output_device = find(
        &host,
        config.output_device.as_deref(),
        host.default_output_device(),
        Direction::Output,
    )?;
    let output_config = pick(
        output_device.supported_output_configs()?,
        Direction::Output,
        config.sample_rate,
        block,
    )?;

    // Everything the callbacks touch is allocated here, before either starts.
    let ring = config
        .live_input
        .then(|| Arc::new(Ring::new(block * RING_BLOCKS)));
    let mut blocks = Blocks::new(
        block,
        output_config.channels as usize,
        config.output_channels,
    );
    let ring_out = ring.clone();
    let output = output_device.build_output_stream(
        &output_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            denormals::flush_to_zero();
            blocks.run(data, ring_out.as_deref(), &mut process);
        },
        // cpal is free to raise this from the audio thread, so it can do no
        // work at all: see the note on errors above.
        |_| {},
        None,
    )?;

    let input = match &ring {
        None => None,
        Some(ring) => {
            let device = find(
                &host,
                config.input_device.as_deref(),
                host.default_input_device(),
                Direction::Input,
            )?;
            let input_config = pick(
                device.supported_input_configs()?,
                Direction::Input,
                config.sample_rate,
                block,
            )?;
            let mut fold = Fold::new(input_config.channels as usize, config.input_channel, block);
            // One block of silence ahead, so the output callback never waits
            // on the input callback that runs just after it. This is the
            // whole of the live path's latency.
            ring.push(&vec![0.0f32; block]);
            let ring = ring.clone();
            let stream = device.build_input_stream(
                &input_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| fold.run(data, &ring),
                |_| {},
                None,
            )?;
            stream.play()?;
            Some(stream)
        }
    };
    output.play()?;

    Ok(Stream {
        _output: output,
        _input: input,
        device: output_device.name().unwrap_or_else(|_| "unnamed".into()),
    })
}
