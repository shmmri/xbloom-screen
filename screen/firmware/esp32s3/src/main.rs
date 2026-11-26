//! xBloom Screen firmware for ESP32-S3 touch displays.
//!
//! The same product as the Raspberry Pi app — the touchscreen UI, the admin web panel, the
//! recipe library, dial presets, PIN lock and screensaver — on a microcontroller board. All of
//! it is the shared crates (`xbloom-core`, `xbloom-app`, `xbloom-ui`); this firmware only adds
//! the hardware: the RGB panel, GT911 touch, the board's control lines, NimBLE, Wi-Fi and the
//! flash file system.
//!
//! 🛑 Safety model (unchanged from the shared driver): loading a recipe only arms the machine.
//! A brew starts only from the "Hold to brew" gesture (when remote start is enabled) or the
//! machine's own button.

mod ble;
mod board;
mod display;
mod hooks;
mod http;
mod io;
mod net;
mod platform;
mod storage;
mod touch;

use std::path::Path;
use std::sync::{Arc, Mutex};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys;
use slint::ComponentHandle;
use xbloom_app::settings::Settings;
use xbloom_app::{App, Data, Event, PlatformInfo, VideoProfile};
use xbloom_core::sim::SimConfig;

use crate::board::{Light, BOARD};

/// Give every thread the app spawns (machine controller, screensaver decoder, simulator…) a
/// stack in PSRAM. Internal RAM is kept for Wi-Fi, Bluetooth, DMA and the UI task. This is
/// safe because code and constants run from PSRAM (`CONFIG_SPIRAM_XIP_FROM_PSRAM`), so flash
/// writes don't disable the cache under those stacks.
fn psram_thread_stacks() {
    // SAFETY: plain ESP-IDF configuration calls on the current task.
    unsafe {
        let mut cfg = sys::esp_pthread_get_default_config();
        cfg.stack_alloc_caps = sys::MALLOC_CAP_SPIRAM | sys::MALLOC_CAP_8BIT;
        cfg.inherit_cfg = true;
        if let Err(e) = sys::esp!(sys::esp_pthread_set_cfg(&cfg)) {
            log::warn!("pthread: can't move stacks to PSRAM: {e}");
        }
    }
}

/// Apply the POSIX time zone from the settings (`UTC0`, `CET-1CEST,M3.5.0,M10.5.0/3`, …).
fn apply_timezone(tz: &str) {
    std::env::set_var("TZ", if tz.is_empty() { "UTC0" } else { tz });
    xbloom_ui::clock::reload_zone();
}

/// Log free internal RAM (the scarce kind: task stacks, DMA, Wi-Fi, Bluetooth).
fn log_internal_heap(when: &str) {
    // SAFETY: read-only heap statistics.
    let (free, largest) = unsafe {
        (
            sys::heap_caps_get_free_size(sys::MALLOC_CAP_INTERNAL),
            sys::heap_caps_get_largest_free_block(sys::MALLOC_CAP_INTERNAL),
        )
    };
    log::info!("heap {when}: internal {} KB free, largest block {} KB", free / 1024, largest / 1024);
}

/// Print every failed heap allocation (size, requested capabilities, caller), so an
/// ESP_ERR_NO_MEM in the log says what ran out.
fn log_failed_allocations() {
    unsafe extern "C" fn on_fail(size: usize, caps: u32, function: *const core::ffi::c_char) {
        // esp_rom_printf doesn't allocate, which matters inside an allocation failure.
        // SAFETY: NUL-terminated format; `function` is a C string or null.
        unsafe {
            let function = if function.is_null() { c"?".as_ptr() } else { function };
            sys::esp_rom_printf(c"alloc failed: %u bytes, caps 0x%x, in %s\n".as_ptr(), size as u32, caps, function);
        }
    }
    // SAFETY: registers a plain function pointer once at start-up.
    if let Err(e) = sys::esp!(unsafe { sys::heap_caps_register_failed_alloc_callback(Some(on_fail)) }) {
        log::warn!("heap: can't register the failed-allocation hook: {e}");
    }
}

fn main() {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::info!("xBloom Screen {} on {}", env!("CARGO_PKG_VERSION"), BOARD.name);
    psram_thread_stacks();
    log_failed_allocations();

    let peripherals = Peripherals::take().expect("peripherals");
    let sysloop = EspSystemEventLoop::take().expect("event loop");
    let nvs = EspDefaultNvsPartition::take().expect("nvs");

    // Data partition (settings, recipes, history, media).
    if let Err(e) = storage::mount() {
        log::error!("storage: can't mount the data partition: {e}");
    }
    let data = Path::new(storage::DATA_DIR);
    let settings: Settings =
        xbloom_app::util::read_json(&data.join("settings.json")).ok().flatten().unwrap_or_default();
    apply_timezone(&settings.timezone);

    // The touch I²C bus, also the I/O expander's on boards that have one.
    // SAFETY: the board reserves these GPIOs for the bus.
    let (sda, scl) = unsafe { (AnyIOPin::steal(BOARD.touch.sda), AnyIOPin::steal(BOARD.touch.scl)) };
    let cfg = I2cConfig::new().baudrate(Hertz(400_000)).sda_enable_pullup(true).scl_enable_pullup(true);
    let bus = Arc::new(Mutex::new(I2cDriver::new(peripherals.i2c0, sda, scl, &cfg).expect("i2c")));
    let lines = Arc::new(io::Lines::new(Arc::clone(&bus), BOARD.expander).expect("control lines"));

    // Wi-Fi (or the setup hotspot) first: it needs internal RAM for its task and buffers, and
    // on the ESP32-8048S043C it failed with ESP_ERR_NO_MEM when it started after the display
    // (its bounce buffers) and the UI.
    log_internal_heap("before wifi");
    // Wi-Fi is optional: if it can't start, the screen still comes up (the admin panel and the
    // setup hotspot are then off), instead of aborting into a boot loop.
    let net = match net::Network::start(peripherals.modem, sysloop, nvs, &settings.wifi, data, 80) {
        Ok(net) => Some(net),
        Err(e) => {
            log::error!("wifi: off, the screen runs without the admin panel: {e}");
            None
        }
    };
    log_internal_heap("after wifi");

    // Display, backlight, touch, and Slint on top.
    let backlight = match BOARD.backlight {
        Light::Pwm(gpio) => {
            display::Backlight::pwm(peripherals.ledc.timer0, peripherals.ledc.channel0, gpio).expect("backlight")
        }
        Light::Switch(line) => display::Backlight::switch(Arc::clone(&lines), line),
    };
    let backlight = Arc::new(Mutex::new(backlight));
    // Diagnosis: light the backlight at full power before the UI starts. The UI sets its own
    // brightness later, in `bind`, so this only shows whether the light itself can turn on.
    if let Ok(mut b) = backlight.lock() {
        b.set(100);
    }
    log::info!("backlight: set to 100% before the UI");
    if let Some(rst) = BOARD.lcd_reset {
        lines.pulse_low(rst, 10, 100);
    }
    let mut panel = display::Display::new(&BOARD).expect("display");
    // Diagnosis: paint full red, green and blue straight into the panel before any UI code
    // runs. If the colours show, the panel and its timing work, and the fault is further on.
    for (name, px) in [("red", 0xF800u16), ("green", 0x07E0), ("blue", 0x001F)] {
        for p in panel.back_buffer().iter_mut() {
            *p = slint::platform::software_renderer::Rgb565Pixel(px);
        }
        panel.present();
        log::info!("colour test: {name} shown");
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
    let touch = touch::Gt911::new(bus, &lines, &BOARD.touch, panel.size()).expect("touch");
    // The screen as the UI sees it: a quarter turn swaps the sides.
    let size = match panel.size() {
        (w, h) if settings.appearance.rotation % 180 == 90 => (h, w),
        wh => wh,
    };
    platform::EspPlatform::install(panel, touch, settings.appearance.rotation).expect("slint platform");

    // The data partition is ~8.9 MB with 16 MB flash and ~1.9 MB with 8 MB (partitions-8mb.csv):
    // keep screensaver videos to what fits.
    let small_flash = storage::usage().is_some_and(|(_, total)| total < 4 << 20);
    let info = PlatformInfo {
        name: "esp32s3".into(),
        screen: size,
        // Half-resolution video: the JPEG decoder keeps ~10 fps, and the screen scales it up.
        video: VideoProfile {
            width: size.0 / 2,
            height: size.1 / 2,
            fps: 10,
            quality: 0.7,
            max_seconds: if small_flash { 10 } else { 30 },
        },
        pin_iterations: 10_000,
        history_max: 200,
        history_points: 160,
        max_upload: if small_flash { 1_000_000 } else { 4_000_000 },
        version: env!("CARGO_PKG_VERSION").into(),
        can_rotate: true,
    };
    let hooks = hooks::EspHooks { net: net.clone(), backlight };
    let app = App::start(data, info, Box::new(ble::NimbleTransport::new()), Box::new(hooks), SimConfig::default());
    let weak = Arc::downgrade(&app);
    let rotation = settings.appearance.rotation;
    app.subscribe(move |ev| {
        if let (Event::Changed(Data::Settings), Some(app)) = (ev, weak.upgrade()) {
            let s = app.settings();
            apply_timezone(&s.timezone);
            // A new rotation is applied at start-up.
            if s.appearance.rotation != rotation {
                app.hooks().restart();
            }
        }
    });
    let _server = match net.as_ref().map(|net| http::start(Arc::clone(&app), Arc::clone(net))) {
        Some(Ok(s)) => Some(s),
        Some(Err(e)) => {
            log::error!("http: {e}");
            None
        }
        None => None,
    };

    log_internal_heap("before ui");
    let ui = xbloom_ui::AppWindow::new().expect("ui");
    log_internal_heap("after ui");
    xbloom_ui::bind(&ui, Arc::clone(&app));
    if let Err(e) = ui.run() {
        log::error!("ui: {e}");
    }
    app.shutdown();
    // SAFETY: plain reboot; the UI loop only ends on a fatal error.
    unsafe { sys::esp_restart() };
}
