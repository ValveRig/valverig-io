//! Between the device's interleaved buffers and the chain's mono blocks.
//!
//! A device hands the callback one interleaved buffer of whatever length it
//! feels like, and a chain wants one mono slice in and one slice per channel
//! out, of a bounded length. The two types here are that translation, and
//! they are the only part of this crate that can be tested without hardware.
//! Both allocate in `new` and never again.

use crate::ring::Ring;
use crate::stream::InputChannel;

/// Folds a device's input frames to the one mono channel a chain takes, and
/// hands them to the ring the output callback reads.
pub(crate) struct Fold {
    channels: usize,
    channel: InputChannel,
    mono: Vec<f32>,
}

impl Fold {
    /// `channels` is how many the input device interleaves, at least 1;
    /// `block` bounds how much is folded at a time and must be at least 1.
    pub(crate) fn new(channels: usize, channel: InputChannel, block: usize) -> Self {
        debug_assert!(channels >= 1 && block >= 1);
        Self {
            channels,
            channel,
            mono: vec![0.0; block],
        }
    }

    /// Fold one input callback's worth of frames into `ring`. Any frame the
    /// ring has no room for is dropped: the consumer is behind, and holding
    /// samples back would only turn that into latency.
    pub(crate) fn run(&mut self, data: &[f32], ring: &Ring) {
        for piece in data.chunks(self.channels * self.mono.len()) {
            let frames = piece.len() / self.channels;
            let mono = &mut self.mono[..frames];
            for (frame, out) in piece.chunks(self.channels).zip(mono.iter_mut()) {
                *out = match self.channel {
                    InputChannel::First => frame[0],
                    InputChannel::Second => frame.get(1).copied().unwrap_or(frame[0]),
                    InputChannel::Both => match frame.get(1) {
                        Some(second) => 0.5 * (frame[0] + second),
                        None => frame[0],
                    },
                };
            }
            ring.push(mono);
        }
    }
}

/// Cuts a device callback into blocks the chain can run, and interleaves the
/// result back across the device's channels.
pub(crate) struct Blocks {
    block: usize,
    channels: usize,
    out_channels: usize,
    mono: Vec<f32>,
    wet: [Vec<f32>; 2],
}

impl Blocks {
    /// `channels` is how many the output device interleaves, at least 1;
    /// `out_channels` is how many the chain produces, 1 or 2; `block` is the
    /// most frames the chain will be handed at once, at least 1.
    pub(crate) fn new(block: usize, channels: usize, out_channels: usize) -> Self {
        debug_assert!(channels >= 1 && block >= 1 && matches!(out_channels, 1 | 2));
        Self {
            block,
            channels,
            out_channels,
            mono: vec![0.0; block],
            wet: [vec![0.0; block], vec![0.0; block]],
        }
    }

    /// Fill one device callback's `data`, interleaved and `channels` wide.
    ///
    /// The device decides how much it asks for and it is not always what it
    /// was configured with; CoreAudio asks for 140 frames of a stream
    /// configured at 128. So `data` is cut into pieces of at most `block`
    /// frames and the chain sees one piece at a time. `input` is the live
    /// signal; frames the ring cannot supply arrive as silence, and with no
    /// ring the chain is fed silence throughout and is expected to have its
    /// own source.
    ///
    /// A chain that returns a NaN or an infinity has a poisoned feedback
    /// structure inside it and will not recover on its own. Silence is the
    /// only honest thing to hand a driver, so that is what non-finite samples
    /// become.
    pub(crate) fn run<F>(&mut self, data: &mut [f32], input: Option<&Ring>, process: &mut F)
    where
        F: FnMut(&[f32], &mut [&mut [f32]]),
    {
        for piece in data.chunks_mut(self.block * self.channels) {
            let frames = piece.len() / self.channels;
            let mono = &mut self.mono[..frames];
            match input {
                Some(ring) => {
                    let got = ring.pop(mono);
                    mono[got..].fill(0.0);
                }
                None => mono.fill(0.0),
            }

            let [left, right] = &mut self.wet;
            {
                let mut outs: [&mut [f32]; 2] = [&mut left[..frames], &mut right[..frames]];
                process(mono, &mut outs[..self.out_channels]);
            }
            for sample in left[..frames].iter_mut() {
                if !sample.is_finite() {
                    *sample = 0.0;
                }
            }
            if self.out_channels == 2 {
                for sample in right[..frames].iter_mut() {
                    if !sample.is_finite() {
                        *sample = 0.0;
                    }
                }
            }

            // A mono result goes to every channel the device has; a stereo
            // pair goes to the first two, and the right of it to any beyond.
            for (i, frame) in piece.chunks_mut(self.channels).enumerate() {
                for (channel, sample) in frame.iter_mut().enumerate() {
                    *sample = if channel == 0 || self.out_channels == 1 {
                        left[i]
                    } else {
                        right[i]
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chain that marks each output channel and each call, so the block
    /// schedule and the channel mapping are both visible in the result.
    fn marker(call: &mut usize) -> impl FnMut(&[f32], &mut [&mut [f32]]) + '_ {
        move |input, outs| {
            *call += 1;
            let k = *call as f32;
            for (c, out) in outs.iter_mut().enumerate() {
                for (i, o) in out.iter_mut().enumerate() {
                    *o = input[i] + k * 1000.0 + (c as f32) * 100.0;
                }
            }
        }
    }

    #[test]
    fn a_mono_result_goes_to_every_channel() {
        let mut blocks = Blocks::new(4, 3, 1);
        // Two frames of a three-channel device.
        let mut data = vec![-1.0f32; 2 * 3];
        let mut call = 0;
        blocks.run(&mut data, None, &mut marker(&mut call));
        assert_eq!(call, 1);
        assert_eq!(data, [1000.0, 1000.0, 1000.0, 1000.0, 1000.0, 1000.0]);
    }

    #[test]
    fn a_stereo_pair_goes_left_right_and_right_again() {
        let mut blocks = Blocks::new(4, 4, 2);
        // One frame of a four-channel device.
        let mut data = vec![-1.0f32; 4];
        let mut call = 0;
        blocks.run(&mut data, None, &mut marker(&mut call));
        assert_eq!(data, [1000.0, 1100.0, 1100.0, 1100.0]);
    }

    #[test]
    fn an_over_delivered_callback_is_cut_into_blocks() {
        // CoreAudio's 140 frames for a stream configured at 128.
        let mut blocks = Blocks::new(128, 1, 1);
        let mut data = vec![0.0f32; 140];
        let mut call = 0;
        blocks.run(&mut data, None, &mut marker(&mut call));
        assert_eq!(call, 2, "128 frames then 12");
        assert!(data[..128].iter().all(|&v| v == 1000.0));
        assert!(data[128..].iter().all(|&v| v == 2000.0));
    }

    #[test]
    fn an_empty_callback_never_reaches_the_chain() {
        let mut blocks = Blocks::new(64, 2, 2);
        let mut call = 0;
        blocks.run(&mut [], None, &mut marker(&mut call));
        assert_eq!(call, 0);
    }

    #[test]
    fn input_the_ring_cannot_supply_arrives_as_silence() {
        let ring = Ring::new(64);
        ring.push(&[7.0, 7.0, 7.0]);
        let mut blocks = Blocks::new(8, 1, 1);
        let mut data = vec![0.0f32; 5];
        let mut call = 0;
        blocks.run(&mut data, Some(&ring), &mut marker(&mut call));
        assert_eq!(data, [1007.0, 1007.0, 1007.0, 1000.0, 1000.0]);
    }

    #[test]
    fn a_non_finite_sample_leaves_as_silence() {
        let mut blocks = Blocks::new(4, 2, 2);
        let mut data = vec![-1.0f32; 2 * 2];
        blocks.run(&mut data, None, &mut |_input, outs| {
            outs[0][0] = f32::NAN;
            outs[0][1] = 0.25;
            outs[1][0] = f32::INFINITY;
            outs[1][1] = f32::NEG_INFINITY;
        });
        assert_eq!(data, [0.0, 0.0, 0.25, 0.0]);
    }

    #[test]
    fn folding_picks_the_channel_it_was_asked_for() {
        let stereo = [1.0, 2.0, 3.0, 4.0];
        for (channel, expected) in [
            (InputChannel::First, [1.0, 3.0]),
            (InputChannel::Second, [2.0, 4.0]),
            (InputChannel::Both, [1.5, 3.5]),
        ] {
            let ring = Ring::new(8);
            Fold::new(2, channel, 4).run(&stereo, &ring);
            let mut got = [0.0f32; 2];
            assert_eq!(ring.pop(&mut got), 2);
            assert_eq!(got, expected, "{channel:?}");
        }
    }

    #[test]
    fn folding_a_mono_device_falls_back_to_its_one_channel() {
        for channel in [
            InputChannel::First,
            InputChannel::Second,
            InputChannel::Both,
        ] {
            let ring = Ring::new(8);
            Fold::new(1, channel, 4).run(&[5.0, 6.0], &ring);
            let mut got = [0.0f32; 2];
            assert_eq!(ring.pop(&mut got), 2);
            assert_eq!(got, [5.0, 6.0], "{channel:?}");
        }
    }

    #[test]
    fn folding_more_frames_than_a_block_takes_several_passes() {
        let ring = Ring::new(512);
        let data: Vec<f32> = (0..300).map(|i| i as f32).collect();
        Fold::new(1, InputChannel::First, 64).run(&data, &ring);
        let mut got = vec![0.0f32; 300];
        assert_eq!(ring.pop(&mut got), 300);
        assert_eq!(got, data);
    }

    #[test]
    fn folding_drops_what_a_full_ring_has_no_room_for() {
        let ring = Ring::new(8);
        let data: Vec<f32> = (0..20).map(|i| i as f32).collect();
        Fold::new(1, InputChannel::First, 4).run(&data, &ring);
        let mut got = vec![0.0f32; 20];
        assert_eq!(
            ring.pop(&mut got),
            8,
            "the ring holds 8 and the rest is gone"
        );
        assert_eq!(got[..8], data[..8]);
    }

    /// The whole live path, over every shape a device is likely to produce.
    ///
    /// The chain must never be handed more than a block, must be primed with
    /// exactly one block of silence, and must see the input frames in order.
    /// Where the ring is big enough to hold a whole callback it must see all
    /// of them. Where it is not, and a one-frame block leaves room for 32
    /// samples against a 63-frame callback, the frames it cannot hold are
    /// dropped rather than reordered or invented.
    #[test]
    fn the_live_path_delivers_the_input_frames_one_block_late() {
        for device_channels in [1usize, 2, 4] {
            for out_channels in [1usize, 2] {
                for block in [1usize, 7, 64, 128] {
                    for delivered in [1usize, 63, 64, 70, 140, 256] {
                        let ring = Ring::new(block * 32);
                        let capacity = ring.capacity();
                        ring.push(&vec![0.0f32; block]);
                        let mut fold = Fold::new(device_channels, InputChannel::First, block);
                        let mut blocks = Blocks::new(block, device_channels, out_channels);
                        let shape = format!(
                            "dev={device_channels} out={out_channels} block={block} delivered={delivered}"
                        );

                        // Frames are numbered from one, so that a zero the
                        // chain sees can only be silence and never a sample.
                        let mut sent = Vec::new();
                        let mut seen = Vec::new();
                        for callback in 0..6 {
                            let input: Vec<f32> = (0..delivered * device_channels)
                                .map(|i| (callback * 10_000 + i + 1) as f32)
                                .collect();
                            sent.extend(input.iter().step_by(device_channels).copied());
                            fold.run(&input, &ring);
                            let mut data = vec![0.0f32; delivered * device_channels];
                            blocks.run(&mut data, Some(&ring), &mut |mono, outs| {
                                assert!(mono.len() <= block, "{} frames, {shape}", mono.len());
                                seen.extend_from_slice(mono);
                                for out in outs.iter_mut() {
                                    out.fill(0.0);
                                }
                            });
                        }

                        let primed = block.min(seen.len());
                        assert!(seen[..primed].iter().all(|&v| v == 0.0), "primed, {shape}");
                        let played: Vec<f32> = seen[primed..]
                            .iter()
                            .copied()
                            .filter(|&v| v != 0.0)
                            .collect();
                        let mut expected = sent.iter();
                        for sample in &played {
                            assert!(
                                expected.by_ref().any(|s| s == sample),
                                "{sample} is out of order or was never sent, {shape}"
                            );
                        }
                        if delivered + block <= capacity {
                            assert_eq!(played, sent[..played.len()], "nothing dropped, {shape}");
                            assert_eq!(seen[primed..], played[..], "no silence between, {shape}");
                        }
                    }
                }
            }
        }
    }
}
