// Read-only hardware probe. Prints HID interfaces and Commander Core / V2 peripheral info.
use hidapi::HidApi;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

fn xfer(dev: &hidapi::HidDevice, cmd: &[u8], data: &[u8]) -> Vec<u8> {
    let mut w = [0u8; 65];
    w[1] = 0x08;
    w[2..2 + cmd.len()].copy_from_slice(cmd);
    w[2 + cmd.len()..2 + cmd.len() + data.len()].copy_from_slice(data);
    dev.write(&w).expect("write");
    let mut r = [0u8; 64];
    let n = dev.read_timeout(&mut r, 500).unwrap_or(0);
    r[..n].to_vec()
}

fn cc_read(dev: &hidapi::HidDevice, ep: u8) -> Vec<u8> {
    xfer(dev, &[0x05, 0x01, 0x01], &[ep]);
    xfer(dev, &[0x0d, 0x01], &[ep]);
    let r = xfer(dev, &[0x08, 0x01], &[ep]);
    xfer(dev, &[0x05, 0x01, 0x01], &[ep]);
    r
}

fn main() {
    let api = HidApi::new().unwrap();
    for d in api.device_list() {
        if d.vendor_id() == 0x1b1c || d.vendor_id() == 0x320f {
            println!(
                "{:04x}:{:04x} if={} up={:04x} u={:04x} {:?} {:?}",
                d.vendor_id(), d.product_id(), d.interface_number(), d.usage_page(), d.usage(),
                d.product_string(), d.path()
            );
        }
    }
    let cc = api.device_list().find(|d| d.vendor_id() == 0x1b1c && d.product_id() == 0x0c32 && d.interface_number() == 0);
    if let Some(info) = cc {
        let dev = info.open_device(&api).unwrap();
        println!("CC fw: {}", hex(&xfer(&dev, &[0x02, 0x13], &[])));
        println!("CC pump ver: {}", hex(&xfer(&dev, &[0x02, 0x57], &[])));
        println!("CC radiator: {}", hex(&xfer(&dev, &[0x02, 0x58], &[])));
        println!("CC wake: {}", hex(&xfer(&dev, &[0x01, 0x03, 0x00, 0x02], &[])));
        for (name, ep) in [("leds", 0x20), ("fans", 0x1a), ("speeds", 0x17), ("temps", 0x21)] {
            println!("CC {name}: {}", hex(&cc_read(&dev, ep)));
        }
        println!("CC sleep: {}", hex(&xfer(&dev, &[0x01, 0x03, 0x00, 0x01], &[])));
    }
    for pid in [0x1b5eu16, 0x1b62] {
        for info in api.device_list().filter(|d| d.vendor_id() == 0x1b1c && d.product_id() == pid && d.usage_page() == 0xff42 && d.usage() == 1) {
            let dev = info.open_device(&api).unwrap();
            for wc in [0x08u8, 0x09] {
                let mut w = [0u8; 65];
                w[1] = wc;
                w[2] = 0x02;
                w[3] = 0x12;
                if let Err(e) = dev.write(&w) { println!("{pid:04x} wc={wc:02x} write err {e}"); continue; }
                let mut r = [0u8; 64];
                let n = dev.read_timeout(&mut r, 200).unwrap_or(0);
                println!("{pid:04x} if={} wc={wc:02x} get 0x12 -> {}", info.interface_number(), hex(&r[..n.min(16)]));
            }
        }
    }
}
