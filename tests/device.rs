//! What can be checked without hardware, and one thing that cannot.

use valverig_io::{Config, Error, InputChannel, MAX_BLOCK, run};

fn config() -> Config {
    Config {
        sample_rate: 48_000,
        block: 128,
        live_input: false,
        output_channels: 2,
        output_device: None,
        input_device: None,
        input_channel: InputChannel::First,
    }
}

/// A settings file is untrusted input, and every one of these reaches an
/// allocation size or a loop bound. `run` must refuse them before it opens
/// anything, which is also what makes this test runnable with no device.
#[test]
fn a_nonsense_configuration_is_refused_before_a_device_is_touched() {
    for (config, expected) in [
        (
            Config {
                block: 0,
                ..config()
            },
            "a block of 0 frames, which is not between 1 and 8192",
        ),
        (
            Config {
                block: MAX_BLOCK + 1,
                ..config()
            },
            "a block of 8193 frames, which is not between 1 and 8192",
        ),
        (
            Config {
                output_channels: 0,
                ..config()
            },
            "0 output channels, which is not 1 or 2",
        ),
        (
            Config {
                output_channels: 3,
                ..config()
            },
            "3 output channels, which is not 1 or 2",
        ),
        (
            Config {
                sample_rate: 0,
                ..config()
            },
            "a sample rate of zero",
        ),
    ] {
        let error = run(config.clone(), |_, _| {}).err().expect("refused");
        assert_eq!(error.to_string(), expected, "{config:?}");
        assert!(matches!(
            error,
            Error::Block(_) | Error::OutputChannels(_) | Error::SampleRate
        ));
    }
}

#[test]
fn a_device_that_does_not_exist_is_named_in_the_error() {
    let config = Config {
        output_device: Some("no such box".into()),
        ..config()
    };
    let error = run(config, |_, _| {}).err().expect("refused");
    assert_eq!(error.to_string(), r#"no output device named "no such box""#);
}

/// Plays a second of a quiet 440 Hz tone on the default output device.
///
/// Ignored because it needs hardware and makes a noise:
/// `cargo test --release -- --ignored --nocapture`.
#[test]
#[ignore = "needs an output device, and plays a tone through it"]
fn a_tone_reaches_the_default_output_device() {
    let rate = 48_000;
    let mut phase = 0.0f32;
    let stream = run(
        Config {
            sample_rate: rate,
            ..config()
        },
        move |input, outputs| {
            assert!(
                input.iter().all(|s| *s == 0.0),
                "silence without live input"
            );
            for i in 0..outputs[0].len() {
                let sample = (phase * std::f32::consts::TAU).sin() * 0.05;
                phase = (phase + 440.0 / rate as f32).fract();
                for out in outputs.iter_mut() {
                    out[i] = sample;
                }
            }
        },
    )
    .expect("the default output device opens at 48 kHz");
    println!("playing 440 Hz on {}", stream.device);
    std::thread::sleep(std::time::Duration::from_secs(1));
}
