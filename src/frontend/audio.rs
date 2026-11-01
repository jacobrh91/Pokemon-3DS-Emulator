//! Speaker output. The emulator pushes 32768 Hz stereo samples; the device
//! callback resamples them to whatever rate the hardware runs at.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Error, ErrorKind, FromSample, OutputCallbackInfo, SampleFormat, SizedSample, Stream, StreamConfig};

use crate::capture::wav::SAMPLE_RATE;

/// Frames of audio to keep queued: about 90 ms. The resampler speeds up or
/// slows down slightly to hold the queue near this, absorbing clock drift
/// between the emulated frame rate and the sound card.
const TARGET_QUEUED: f32 = 3000.0;
/// Hard cap on queued frames; anything older is dropped.
const MAX_QUEUED: usize = 16384;

type Queue = Arc<Mutex<VecDeque<[f32; 2]>>>;

pub struct AudioOutput {
    queue: Queue,
    _stream: Stream,
}

/// Pulls frames from the queue at the device rate, interpolating linearly.
struct Resampler {
    current: [f32; 2],
    next: [f32; 2],
    fraction: f32,
    base_ratio: f32,
    /// Silent until enough audio is queued, so starting never underruns straight away.
    primed: bool,
}

impl Resampler {
    fn new(device_rate: u32) -> Resampler {
        Resampler { current: [0.0; 2], next: [0.0; 2], fraction: 1.0, base_ratio: SAMPLE_RATE as f32 / device_rate as f32, primed: false }
    }

    fn next_frame(&mut self, queue: &mut VecDeque<[f32; 2]>) -> [f32; 2] {
        if !self.primed {
            if (queue.len() as f32) < TARGET_QUEUED {
                return [0.0; 2];
            }
            self.primed = true;
        }
        // Nudge the rate by up to 1% to keep the queue near its target depth.
        let error = ((queue.len() as f32 - TARGET_QUEUED) / TARGET_QUEUED).clamp(-1.0, 1.0);
        let ratio = self.base_ratio * (1.0 + error * 0.01);
        while self.fraction >= 1.0 {
            self.fraction -= 1.0;
            self.current = self.next;
            // On underrun, fade toward silence instead of holding a DC level.
            self.next = match queue.pop_front() {
                Some(frame) => frame,
                None => {
                    let faded = [self.next[0] * 0.95, self.next[1] * 0.95];
                    self.primed = faded[0].abs() + faded[1].abs() > 0.001;
                    faded
                }
            };
        }
        let mix = |i: usize| self.current[i] + (self.next[i] - self.current[i]) * self.fraction;
        let frame = [mix(0), mix(1)];
        self.fraction += ratio;
        frame
    }
}

impl AudioOutput {
    pub fn new() -> Result<AudioOutput, Box<dyn std::error::Error>> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no audio output device")?;
        let config = device.default_output_config()?;
        let format = config.sample_format();
        let config: StreamConfig = config.into();
        let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
        let stream = match format {
            SampleFormat::I16 => build::<i16>(&device, config, queue.clone())?,
            SampleFormat::I32 => build::<i32>(&device, config, queue.clone())?,
            SampleFormat::F32 => build::<f32>(&device, config, queue.clone())?,
            other => return Err(format!("unsupported audio sample format {other}").into()),
        };
        stream.play()?;
        Ok(AudioOutput { queue, _stream: stream })
    }

    /// Throw away queued audio, after the emulator jumps to another moment in time.
    pub fn clear(&self) {
        self.queue.lock().unwrap().clear();
    }

    /// Queue interleaved left/right samples from the emulator.
    pub fn push(&self, samples: &[i16]) {
        let mut queue = self.queue.lock().unwrap();
        queue.extend(samples.chunks_exact(2).map(|s| [s[0] as f32 / 32768.0, s[1] as f32 / 32768.0]));
        if queue.len() > MAX_QUEUED {
            let excess = queue.len() - MAX_QUEUED;
            queue.drain(..excess);
        }
    }
}

fn build<T>(device: &cpal::Device, config: StreamConfig, queue: Queue) -> Result<Stream, Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let mut resampler = Resampler::new(config.sample_rate);
    device.build_output_stream(
        config,
        move |data: &mut [T], _: &OutputCallbackInfo| {
            let mut queue = queue.lock().unwrap();
            for frame in data.chunks_mut(channels) {
                let [left, right] = resampler.next_frame(&mut queue);
                for (i, out) in frame.iter_mut().enumerate() {
                    let value = match (channels, i) {
                        (1, _) => (left + right) * 0.5,
                        (_, 0) => left,
                        (_, 1) => right,
                        _ => 0.0,
                    };
                    *out = T::from_sample(value);
                }
            }
        },
        |err: Error| match err.kind() {
            ErrorKind::Xrun => {}
            _ => eprintln!("audio error: {err}"),
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_interpolates_between_input_frames() {
        let mut r = Resampler::new(SAMPLE_RATE * 2); // device runs twice as fast
        r.primed = true;
        let mut queue: VecDeque<[f32; 2]> = VecDeque::from(vec![[0.0, 0.0], [1.0, -1.0], [1.0, -1.0]]);
        let out: Vec<f32> = (0..6).map(|_| r.next_frame(&mut queue)[0]).collect();
        // Rising ramp from 0 to 1 over the first few output frames, never exceeding the input range.
        assert!(out.iter().all(|v| (0.0..=1.0).contains(v)), "{out:?}");
        assert!(out.windows(2).take(3).all(|w| w[1] >= w[0]), "{out:?}");
    }

    #[test]
    fn stays_silent_until_prebuffered() {
        let mut r = Resampler::new(SAMPLE_RATE);
        let mut queue: VecDeque<[f32; 2]> = VecDeque::from(vec![[1.0, 1.0]; 100]);
        assert_eq!(r.next_frame(&mut queue), [0.0, 0.0]);
        assert_eq!(queue.len(), 100, "nothing consumed while priming");
        queue.extend(vec![[1.0, 1.0]; TARGET_QUEUED as usize]);
        for _ in 0..4 {
            r.next_frame(&mut queue);
        }
        assert!(r.next_frame(&mut queue)[0] > 0.0);
    }

    #[test]
    fn underrun_fades_to_silence() {
        let mut r = Resampler::new(SAMPLE_RATE);
        r.primed = true;
        let mut queue: VecDeque<[f32; 2]> = VecDeque::from(vec![[1.0, 1.0], [1.0, 1.0]]);
        let mut last = 1.0;
        for _ in 0..200 {
            last = r.next_frame(&mut queue)[0];
        }
        assert!(last.abs() < 0.01, "expected silence after underrun, got {last}");
    }
}
