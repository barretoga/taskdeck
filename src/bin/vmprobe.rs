//! Read-only: prints Voicemeeter strip/bus names and mute states.
fn main() {
    let cfg = taskdeck::config::Config::default();
    let Some(vm) = taskdeck::voicemeeter::Voicemeeter::connect(&cfg.voicemeeter_dll) else {
        println!("could not load/login to {}", cfg.voicemeeter_dll);
        return;
    };
    std::thread::sleep(std::time::Duration::from_millis(300));
    vm.refresh();
    println!("type = {:?}", vm.kind());
    for i in 0..8 {
        let dev = vm.get_string(&format!("Strip[{i}].device.name"));
        let label = vm.get_string(&format!("Strip[{i}].Label"));
        let mute = vm.get(&format!("Strip[{i}].Mute"));
        if mute.is_some() {
            println!("Strip[{i}] device={dev:?} label={label:?} mute={mute:?}");
        }
    }
    for i in 0..8 {
        let dev = vm.get_string(&format!("Bus[{i}].device.name"));
        let mute = vm.get(&format!("Bus[{i}].Mute"));
        if mute.is_some() {
            println!("Bus[{i}] device={dev:?} mute={mute:?}");
        }
    }
    println!("auto mic -> {}", taskdeck::resolve_mic(&vm, "auto"));
}
