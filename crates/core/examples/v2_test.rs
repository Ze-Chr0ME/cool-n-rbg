use coolrgb_core::devices::corsair::bragi::CorsairV2;
use coolrgb_core::effects::Rgb;

fn main() {
    let api = hidapi::HidApi::new().unwrap();
    for d in CorsairV2::open_all(&api) {
        let d = d.unwrap();
        println!("{} light_ctrl ok", d.name);
        for c in [Rgb(255, 0, 0), Rgb(0, 255, 0), Rgb(0, 0, 255)] {
            println!("{:?} -> {:?}", c, d.set_colors(&vec![c; d.leds]));
            std::thread::sleep(std::time::Duration::from_millis(800));
        }
    }
}
