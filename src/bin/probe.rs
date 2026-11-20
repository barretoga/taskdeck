//! Dev tool (read-only): listens for input reports on every interface of a HID
//! device. Usage: probe <VID hex> <PID hex> <seconds>. Never writes to the device.
use std::time::{Duration, Instant};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let vid = u16::from_str_radix(args.get(1).map(String::as_str).unwrap_or("1D57"), 16).unwrap();
    let pid = u16::from_str_radix(args.get(2).map(String::as_str).unwrap_or("FA60"), 16).unwrap();
    let secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(20);
    let api = hidapi::HidApi::new().expect("hidapi init");
    let mut devs = Vec::new();
    for d in api.device_list().filter(|d| d.vendor_id() == vid && d.product_id() == pid) {
        // Skip keyboard/mouse collections: Windows owns them exclusively.
        if d.usage_page() == 0x0001 && matches!(d.usage(), 0x02 | 0x06) {
            continue;
        }
        let label = format!("if{} page=0x{:04X}", d.interface_number(), d.usage_page());
        match d.open_device(&api) {
            Ok(dev) => devs.push((label, dev)),
            Err(e) => println!("{label}: cannot open ({e})"),
        }
    }
    println!("Listening on {} interfaces for {secs}s", devs.len());
    let end = Instant::now() + Duration::from_secs(secs);
    let start = Instant::now();
    let mut buf = [0u8; 256];
    while Instant::now() < end {
        for (label, dev) in &devs {
            if let Ok(n) = dev.read_timeout(&mut buf, 10) {
                if n > 0 {
                    println!("{:6.2}s {label}: {}", start.elapsed().as_secs_f32(), hex(&buf[..n]));
                }
            }
        }
    }
    println!("done");
}
