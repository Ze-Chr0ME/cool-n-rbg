//! Read-only scan of Bragi properties on the Harpoon RGB Wireless (1B1C:1B5E): only `02 <id>` GETs.
use hidapi::HidApi;
fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ") }
fn main() {
    let api = HidApi::new().unwrap();
    for d in api.device_list().filter(|d| d.vendor_id() == 0x1b1c && d.product_id() == 0x1b5e) {
        println!("if {} page {:04x} usage {:02x}", d.interface_number(), d.usage_page(), d.usage());
    }
    let info = api.device_list().find(|d| d.vendor_id() == 0x1b1c && d.product_id() == 0x1b5e && d.usage_page() == 0xff42 && d.usage() == 1).unwrap();
    let dev = info.open_device(&api).unwrap();
    for id in 0u8..=0xff {
        let mut w = [0u8; 65];
        w[1] = 0x08;
        w[2] = 0x02;
        w[3] = id;
        dev.write(&w).unwrap();
        // Other apps share the handle's input stream: skip replies that are not a GET.
        let mut r = [0u8; 64];
        for _ in 0..5 {
            let n = dev.read_timeout(&mut r, 100).unwrap_or(0);
            if n > 0 && r[1] == 0x02 { break; }
            r = [0u8; 64];
        }
        if r[1] == 0x02 && r[2] == 0 {
            println!("{id:02x}: {}", hex(&r[3..11]));
        }
    }
}
