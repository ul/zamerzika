#[macro_use]
extern crate vst;

use vst::{
    api::{Events, Supported},
    buffer::AudioBuffer,
    event::Event,
    plugin::{CanDo, Category, HostCallback, Info, Plugin},
};

/// Stereo should be enough for everyone ™
const CHANNELS: usize = 2;
/// MIDI Note 0 is ~8.176 Hz, and assuming max sample rate to be 96 kHz
/// that would correspond to ~11742 samples.
const MAX_WINDOW_SIZE: usize = 11742;
// Used to smooth out freezed loop, reducing saw component in the output,
// as well as to cross-fade on note off, reducing clicks.
const XFADE_FRAMES: usize = 64;

struct Zamerzika {
    sample_rate: f64,
    note: Option<u8>,
    input: [RingBuffer; CHANNELS],
    output: [RingBuffer; CHANNELS],
    window_size: usize,
    xfade_countdown: [usize; CHANNELS],
}

impl Zamerzika {
    fn process_sample(&mut self, channel: usize, sample: f64) -> f64 {
        self.input[channel].write(sample);
        if self.note.is_some() {
            self.output[channel].read()
        } else if self.xfade_countdown[channel] > 0 {
            let alpha = self.xfade_countdown[channel] as f64 / XFADE_FRAMES as f64;
            let mix = alpha * self.output[channel].read() + (1.0 - alpha) * sample;
            self.xfade_countdown[channel] -= 1;
            mix
        } else {
            sample
        }
    }
}

impl Plugin for Zamerzika {
    fn new(_host: HostCallback) -> Self {
        let mut input: [RingBuffer; CHANNELS] = Default::default();
        let mut output: [RingBuffer; CHANNELS] = Default::default();
        for channel in 0..CHANNELS {
            input[channel].resize(MAX_WINDOW_SIZE, 0.0);
            output[channel].resize(MAX_WINDOW_SIZE, 0.0);
        }
        Zamerzika {
            sample_rate: 48_000.0,
            note: None,
            input,
            output,
            window_size: 0,
            xfade_countdown: Default::default(),
        }
    }

    fn get_info(&self) -> Info {
        Info {
            name: "Zamerzika".to_string(),
            vendor: "Ruslan Prakapchuk".to_string(),
            inputs: CHANNELS as _,
            outputs: CHANNELS as _,
            midi_inputs: 1,
            unique_id: 1_804_198_802,
            version: 0001,
            category: Category::Effect,
            f64_precision: true,
            ..Default::default()
        }
    }

    fn can_do(&self, can_do: CanDo) -> Supported {
        match can_do {
            CanDo::ReceiveMidiEvent => Supported::Yes,
            _ => Supported::No,
        }
    }

    fn set_sample_rate(&mut self, rate: f32) {
        self.sample_rate = f64::from(rate);
    }

    fn process(&mut self, buffer: &mut AudioBuffer<f32>) {
        // For each input and output channel.
        for (channel, (input, output)) in buffer.zip().enumerate() {
            // For each input sample and output sample in buffer.
            for (in_sample, out_sample) in input.into_iter().zip(output.into_iter()) {
                *out_sample = self.process_sample(channel, *in_sample as _) as _;
            }
        }
    }

    fn process_f64(&mut self, buffer: &mut AudioBuffer<f64>) {
        // For each input and output channel.
        for (channel, (input, output)) in buffer.zip().enumerate() {
            // For each input sample and output sample in buffer.
            for (in_sample, out_sample) in input.into_iter().zip(output.into_iter()) {
                *out_sample = self.process_sample(channel, *in_sample);
            }
        }
    }

    fn process_events(&mut self, events: &Events) {
        for event in events.events() {
            match event {
                Event::Midi(ev) => match ev.data[0] {
                    0x80 => {
                        if let Some(note) = self.note {
                            if note == ev.data[1] {
                                self.note = None;
                                for channel in 0..CHANNELS {
                                    self.xfade_countdown[channel] = XFADE_FRAMES;
                                }
                            }
                        }
                    }
                    // TODO Moar time precision, freeze with `ev.delta_frame` delay.
                    // TODO Polyphony?
                    0x90 => {
                        let pitch = ev.data[1];
                        self.note = Some(pitch);
                        self.window_size =
                            (self.sample_rate / midi_pitch_to_freq(pitch)).round() as _;
                        for channel in 0..CHANNELS {
                            self.input[channel].open_window(self.window_size);
                            self.output[channel].resize(self.window_size, 0.0);
                            for _ in 0..self.window_size {
                                self.output[channel].write(self.input[channel].read());
                            }
                            self.output[channel].smooth(XFADE_FRAMES);
                        }
                    }
                    _ => (),
                },
                _ => (),
            }
        }
    }
}

/// Convert the midi note's pitch into the equivalent frequency.
///
/// This function assumes A4 is 440 Hz.
fn midi_pitch_to_freq(pitch: u8) -> f64 {
    const A4_PITCH: i8 = 69;
    const A4_FREQ: f64 = 440.0;

    // Midi notes can be 0-127
    ((f64::from(pitch as i8 - A4_PITCH)) / 12.).exp2() * A4_FREQ
}

#[derive(Default)]
struct RingBuffer {
    read_cursor: usize,
    write_cursor: usize,
    len: usize,
    data: Vec<f64>,
}

impl RingBuffer {
    fn write(&mut self, sample: f64) {
        self.data[self.write_cursor] = sample;
        self.write_cursor += 1;
        if self.write_cursor >= self.len {
            self.write_cursor = 0;
        }
    }

    fn read(&mut self) -> f64 {
        let result = self.data[self.read_cursor];
        self.read_cursor += 1;
        if self.read_cursor >= self.len {
            self.read_cursor = 0;
        }
        result
    }

    fn resize(&mut self, new_len: usize, value: f64) {
        self.read_cursor = 0;
        self.write_cursor = 0;
        self.len = new_len;
        self.data.resize(new_len, value);
    }

    fn open_window(&mut self, window_size: usize) {
        let end = self.write_cursor;
        let len = self.len;
        let start = (end + len - window_size) % len;
        self.read_cursor = start;
    }

    fn smooth(&mut self, depth: usize) {
        let depth = depth.min(self.len);
        let offset = self.read_cursor + self.len;
        for i in offset..(offset + depth) {
            let current = i % self.len;
            let previous = (i - 1) % self.len;
            self.data[current] = 0.5 * (self.data[current] + self.data[previous]);
        }
    }
}

plugin_main!(Zamerzika);
