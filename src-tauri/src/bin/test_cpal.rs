use cpal::traits::{DeviceTrait, HostTrait};
fn main() {
    let host = cpal::default_host();
    let device = host.default_input_device().unwrap();
    let config = device.default_input_config().unwrap();
    let sr: cpal::SampleRate = config.sample_rate();
    let val: u32 = sr;
}
