use core::sync::atomic::{AtomicU32, Ordering};

use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::{Input, Output};
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};

use personal_hopspot_core::InputEvent;

use crate::node::Mtx;

const BUTTON_LONG_PRESS: Duration = Duration::from_millis(500);
const BUTTON_DEBOUNCE: Duration = Duration::from_millis(25);

pub(crate) static EVENTS: Channel<Mtx, InputEvent, 4> = Channel::new();
static BUTTON_COUNT: AtomicU32 = AtomicU32::new(0);
static PANEL_LIGHT_WAKE: Signal<Mtx, ()> = Signal::new();

pub(crate) async fn drive_button(mut button: Input<'static>) -> ! {
    loop {
        button.wait_for_falling_edge().await;
        PANEL_LIGHT_WAKE.signal(());
        match select(
            button.wait_for_rising_edge(),
            Timer::after(BUTTON_LONG_PRESS),
        )
        .await
        {
            Either::First(()) => {
                BUTTON_COUNT.fetch_add(1, Ordering::Relaxed);
                EVENTS.send(InputEvent::ShortPress).await;
            }
            Either::Second(()) => {
                BUTTON_COUNT.fetch_add(1, Ordering::Relaxed);
                EVENTS.send(InputEvent::LongPress).await;
                button.wait_for_rising_edge().await;
            }
        }
        Timer::after(BUTTON_DEBOUNCE).await;
    }
}

/// Hold the panel light lit for `hold` after any press. Each board passes its own hold: the
/// constant is a policy about that board's panel, not about the button.
pub(crate) async fn drive_panel_light(mut light: Output<'static>, hold: Duration) -> ! {
    loop {
        PANEL_LIGHT_WAKE.wait().await;
        light.set_high();
        while let Either::First(()) = select(PANEL_LIGHT_WAKE.wait(), Timer::after(hold)).await {}
        light.set_low();
    }
}
