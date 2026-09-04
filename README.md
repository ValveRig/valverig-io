# valverig-io

Opens an audio device and runs your chain in its callback. One closure, mono
in and one or two channels out, with no queue in between, because a queue is
latency and a guitarist hears it.

```rust
use valverig_io::{Config, InputChannel, run};

let stream = run(
    Config {
        sample_rate: 48_000,
        block: 128,
        live_input: true,
        output_channels: 2,
        output_device: None,   // the host's default
        input_device: None,
        input_channel: InputChannel::First,
    },
    // On the audio thread: no allocating, no locking, no logging, no waiting.
    move |input, outputs| amp.process(input, outputs),
)?;

println!("playing on {}", stream.device);
std::thread::sleep(std::time::Duration::from_secs(10));
drop(stream);   // and the device is released
```

The device decides how many frames it hands you, so whatever arrives is cut
into pieces of at most `Config::block`. Your chain never sees more than it
was prepared for.

## How to use

**Nothing here resamples.** `Config::sample_rate` is the rate the chain
itself needs. A device that does not offer it is an error rather than
something to convert around, so a chain at another rate has to be converted
at its own edges.

**Live input is exactly one block late**, and there is no way to ask for
less: input and output are two separate streams, and the ring between them is
primed with one block of silence so the output never waits on the input
callback that runs just after it.

**Input and output are two clocks.** Use one device for both, or accept that
two that disagree by even a few parts per million will slowly fill the ring
or empty it, and the frames it cannot supply arrive as silence. Nothing here
regulates that drift.

**Denormals are flushed to zero** on the audio thread, on aarch64 and on x86
with SSE. A reverb tail decaying into the denormal range costs tens of times
more per operation on some CPUs, and it shows up as a callback that
intermittently overruns rather than as anything audible.

**A non-finite sample never reaches the driver.** A chain that returns a NaN
has a poisoned feedback structure and will not recover on its own. Silence is
the only honest thing to send a speaker.

**Nothing is reported once a stream is running.** A device unplugged under
the stream raises an error cpal may deliver on the audio thread, where this
crate can neither log it nor allocate a message for it. Watch for the
callback going quiet, then drop the `Stream` and open another: there is no
reset and no route-change notification.

**Configuration is checked before the host is touched**, so a block size of
zero or a nonsense channel count out of a settings file is an error and not a
panic on the audio thread.

## Tests

```bash
cargo test
cargo test --release
cargo test --release -- --ignored --nocapture   # plays a tone on the default device
```

Everything but the last needs no sound hardware, so it runs anywhere. There
is no `assets/` directory: nothing here has a fixture to compare against.

## Licence

MIT. See [LICENSE](LICENSE).
