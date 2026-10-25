// Commander Core live test: colors, then fans at floor-safe duty, then back to hardware mode.
use coolrgb_core::devices::commander_core::CommanderCore;
use coolrgb_core::effects::{Effect, Rgb};
use std::time::{Duration, Instant};

fn main() {
    let api = hidapi::HidApi::new().unwrap();
    let mut cc = CommanderCore::open(&api, &[]).expect("no commander core").expect("init");
    println!("fw {} leds {:?} total {} connected {:?}", cc.firmware, cc.port_leds, cc.led_count(), cc.connected);
    let n = cc.led_count();
    for c in [Rgb(255, 0, 0), Rgb(0, 255, 0), Rgb(0, 0, 255)] {
        cc.set_colors(&vec![c; n]).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
    }
    let wave = Effect::Wave { period_s: 3.0 };
    let mut buf = vec![Rgb::BLACK; n];
    let t0 = Instant::now();
    let mut frames = 0;
    while t0.elapsed() < Duration::from_secs(5) {
        wave.render(t0.elapsed().as_secs_f32(), 1.0, &mut buf);
        cc.set_colors(&buf).unwrap();
        frames += 1;
    }
    println!("wave fps {:.1}", frames as f32 / 5.0);
    println!("sensors before {:?}", cc.sensors().unwrap());
    cc.set_duty(&[80, 60, 60, 60, 60, 60, 60]).unwrap();
    for _ in 0..4 {
        std::thread::sleep(Duration::from_secs(2));
        cc.set_colors(&vec![Rgb(255, 255, 255); n]).unwrap();
        println!("sensors {:?}", cc.sensors().unwrap());
    }
    drop(cc);
    println!("hardware mode restored");
}
