//! Dev tool (read-only): lists every HID interface on the PC.
fn main() {
    let api = hidapi::HidApi::new().expect("hidapi");
    let mut v: Vec<_> = api.device_list().collect();
    v.sort_by_key(|d| (d.vendor_id(), d.product_id(), d.interface_number(), d.usage_page()));
    for d in v {
        println!(
            "{:04X}:{:04X} if={} page=0x{:04X} usage=0x{:04X} {:?} / {:?}",
            d.vendor_id(),
            d.product_id(),
            d.interface_number(),
            d.usage_page(),
            d.usage(),
            d.manufacturer_string().unwrap_or(""),
            d.product_string().unwrap_or("")
        );
    }
}
