//! Dev tool: runs the Spotify audio capture for a few seconds and prints the bars.
fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(4);
    taskdeck::spectrum::set_target(Some(&std::env::args().nth(2).unwrap_or("spotify.exe".into())));
    taskdeck::spectrum::start();
    taskdeck::spectrum::set_active(true);
    for _ in 0..(secs * 4) {
        std::thread::sleep(std::time::Duration::from_millis(250));
        let l = taskdeck::spectrum::levels();
        let bars: String = l.iter().map(|v| format!("{:4.2} ", v)).collect();
        let (status, packets) = taskdeck::spectrum::status();
        println!("{bars} packets={packets} {status}");
    }
}
