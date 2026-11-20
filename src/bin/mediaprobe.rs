//! Dev tool: runs the media thread without the UI and sends play/pause twice.
use std::time::Duration;
use windows::Win32::Foundation::HWND;

fn show(label: &str) {
    match taskdeck::media::current() {
        Some(i) => println!("{label}: {} | playing={} | {} - {}", i.source.name, i.playing, i.artist, i.title),
        None => println!("{label}: (nothing)"),
    }
}

fn main() {
    let keys: Vec<String> = ["spotify", "brave", "chrome", "edge", "firefox"].iter().map(|s| s.to_string()).collect();
    taskdeck::media::set_sources(&keys);
    taskdeck::media::start(HWND::default(), 0);
    std::thread::sleep(Duration::from_secs(2));
    show("start");
    for i in 1..=2 {
        taskdeck::media::send(taskdeck::media::Cmd::PlayPause);
        std::thread::sleep(Duration::from_millis(1800));
        show(&format!("after play/pause {i}"));
        println!("   command: {}", taskdeck::media::last_command());
    }
}
