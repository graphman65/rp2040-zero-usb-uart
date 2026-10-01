use embassy_rp::{
    peripherals::PIO0,
    pio_programs::ws2812::{Grb, PioWs2812},
};
use embassy_time::Timer;
use smart_leds::RGB8;

#[cfg(feature = "activity-led")]
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};

#[cfg(feature = "activity-led")]
static ACTIVITY: Signal<CriticalSectionRawMutex, ()> = Signal::new();

type Led<'d> = PioWs2812<'d, PIO0, 0, Grb>;

const OFF: RGB8 = RGB8 { r: 0, g: 0, b: 0 };
const STARTUP: RGB8 = RGB8 { r: 0, g: 16, b: 0 };
#[cfg(feature = "activity-led")]
const TRAFFIC: RGB8 = RGB8 { r: 0, g: 0, b: 16 };

#[cfg(feature = "activity-led")]
pub fn activity() {
    // Coalesce traffic notifications; never make forwarding wait for the LED.
    ACTIVITY.signal(());
}

async fn set_color(led: &mut Led<'_>, color: RGB8) {
    led.write_slice(&[color]).await;
    // Newer WS2812 revisions need 280 us reset, beyond the HAL's 55 us delay.
    Timer::after_micros(300).await;
}

pub async fn run(mut led: Led<'_>) {
    set_color(&mut led, OFF).await;

    for _ in 0..2 {
        set_color(&mut led, STARTUP).await;
        Timer::after_millis(150).await;
        set_color(&mut led, OFF).await;
        Timer::after_millis(150).await;
    }

    #[cfg(feature = "activity-led")]
    loop {
        ACTIVITY.wait().await;
        set_color(&mut led, TRAFFIC).await;
        Timer::after_millis(45).await;
        set_color(&mut led, OFF).await;
        Timer::after_millis(75).await;
    }
}
