//! What the host offers, for a caller that wants to put a list in front of
//! someone.

use cpal::SampleFormat;
use cpal::traits::{DeviceTrait, HostTrait};

/// The rates a device is asked about, in the order they are reported.
const COMMON_RATES: [u32; 4] = [44_100, 48_000, 88_200, 96_000];

/// A device the host offers, with what it supports in `f32`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// The name the host gives it, and what [`crate::Config`] selects by.
    pub name: String,
    /// Whether the host calls it the default for its direction.
    pub is_default: bool,
    /// The most channels any of its `f32` configurations offers.
    pub channels: u16,
    /// Which of 44100, 48000, 88200 and 96000 Hz it supports, in that order.
    /// A device may well support rates outside that list; this is the set a
    /// caller can reasonably offer as a choice.
    pub sample_rates: Vec<u32>,
}

/// Every device the host offers, by direction.
#[derive(Debug, Clone, Default)]
pub struct Devices {
    /// Devices that can be recorded from.
    pub inputs: Vec<DeviceInfo>,
    /// Devices that can be played to.
    pub outputs: Vec<DeviceInfo>,
}

fn describe(
    device: &cpal::Device,
    is_default: bool,
    ranges: Result<Vec<cpal::SupportedStreamConfigRange>, cpal::SupportedStreamConfigsError>,
) -> Option<DeviceInfo> {
    let ranges: Vec<_> = ranges
        .ok()?
        .into_iter()
        .filter(|r| r.sample_format() == SampleFormat::F32)
        .collect();
    if ranges.is_empty() {
        return None;
    }
    Some(DeviceInfo {
        name: device.name().ok()?,
        is_default,
        channels: ranges.iter().map(|r| r.channels()).max().unwrap_or(0),
        sample_rates: COMMON_RATES
            .into_iter()
            .filter(|rate| {
                ranges
                    .iter()
                    .any(|r| r.min_sample_rate().0 <= *rate && *rate <= r.max_sample_rate().0)
            })
            .collect(),
    })
}

/// Every device the default host offers in `f32`, inputs and outputs, with
/// the host's defaults flagged.
///
/// A device that cannot be asked its name or its configurations is left out
/// rather than reported half-known, and a host that cannot be asked at all
/// yields two empty lists. Enumeration talks to the driver, so it can take
/// milliseconds: call it from a worker thread, never from a callback.
///
/// ```no_run
/// // Needs a host with devices on it, so this compiles but does not run
/// // under `cargo test`.
/// let devices = valverig_io::devices();
/// for output in &devices.outputs {
///     println!("{} ({} channels) {:?}", output.name, output.channels, output.sample_rates);
/// }
/// ```
pub fn devices() -> Devices {
    let host = cpal::default_host();
    let default_input = host.default_input_device().and_then(|d| d.name().ok());
    let default_output = host.default_output_device().and_then(|d| d.name().ok());
    let mut all = Devices::default();
    let Ok(list) = host.devices() else {
        return all;
    };
    for device in list {
        let name = device.name().ok();
        let inputs = device.supported_input_configs().map(|c| c.collect());
        if let Some(info) = describe(&device, name == default_input, inputs) {
            all.inputs.push(info);
        }
        let outputs = device.supported_output_configs().map(|c| c.collect());
        if let Some(info) = describe(&device, name == default_output, outputs) {
            all.outputs.push(info);
        }
    }
    all
}
