use hidapi::HidApi;
fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ") }
fn main() {
    let api = HidApi::new().unwrap();
    let info = api.device_list().find(|d| d.vendor_id() == 0x1b1c && d.product_id() == 0x1b62 && d.usage_page() == 0xff42 && d.usage() == 1).unwrap();
    let dev = info.open_device(&api).unwrap();
    let x = |pre: u8, data: &[u8]| {
        let mut w = [0u8; 65]; w[1] = pre; w[2..2 + data.len()].copy_from_slice(data);
        dev.write(&w).unwrap();
        let mut r = [0u8; 64];
        let n = dev.read_timeout(&mut r, 300).unwrap_or(0);
        format!("{}", hex(&r[..n.min(24)]))
    };
    println!("dongle fw  {}", x(0x08, &[0x02, 0x13]));
    println!("dongle sw  {}", x(0x08, &[0x01, 0x03, 0x00, 0x02]));
    println!("close      {}", x(0x08, &[0x05, 0x01, 0x24]));
    println!("open 24    {}", x(0x08, &[0x0d, 0x00, 0x24]));
    println!("write 24   {}", x(0x08, &[0x09, 0x00, 0x24]));
    println!("read 24    {}", x(0x08, &[0x08, 0x00, 0x24]));
    println!("close      {}", x(0x08, &[0x05, 0x01]));
    for _ in 0..4 {
        println!("kb pid 09  {}", x(0x09, &[0x02, 0x12]));
    }
    println!("dongle hw  {}", x(0x08, &[0x01, 0x03, 0x00, 0x01]));
}
