#![no_std]
#![no_main]

mod led;

use core::cell::Cell;
use critical_section::Mutex;
use embassy_futures::{
    join::{join, join3},
    select::{Either, select},
    yield_now,
};
use embassy_rp::{
    bind_interrupts,
    peripherals::{DMA_CH0, PIO0, UART0, USB},
    pio::Pio,
    pio_programs::ws2812::{PioWs2812, PioWs2812Program},
    uart::{self, BufferedUart, BufferedUartRx, BufferedUartTx},
    usb::{Driver, InterruptHandler},
};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_usb::{
    Handler,
    class::cdc_acm::{CdcAcmClass, Receiver, Sender, State},
    control::{InResponse, OutResponse, Recipient, Request, RequestType},
    driver::EndpointError,
};
use rp2040_zero_usb_uart::{LineCoding, Session};

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
    UART0_IRQ => uart::BufferedInterruptHandler<UART0>;
    PIO0_IRQ_0 => embassy_rp::pio::InterruptHandler<PIO0>;
    DMA_IRQ_0 => embassy_rp::dma::InterruptHandler<DMA_CH0>;
});

type UsbDriver<'d> = Driver<'d, USB>;

static SESSION: Mutex<Cell<Session>> = Mutex::new(Cell::new(Session::NEW));
static TX_CHANGED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static RX_CHANGED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

fn snapshot() -> Session {
    critical_section::with(|cs| SESSION.borrow(cs).get())
}

fn update(f: impl FnOnce(&mut Session)) {
    let session_changed = critical_section::with(|cs| {
        let cell = SESSION.borrow(cs);
        let mut session = cell.get();
        let epoch = session.epoch;

        f(&mut session);
        cell.set(session);
        session.epoch != epoch
    });

    TX_CHANGED.signal(());
    if session_changed {
        RX_CHANGED.signal(());
    }
}

/// Registered before CDC: reject unsupported framing before CDC can coerce it.
/// Own SET/GET_LINE_CODING so USB reset does not substitute CDC's 8000-baud default.
struct SerialControl;

impl Handler for SerialControl {
    fn enabled(&mut self, enabled: bool) {
        if !enabled {
            update(|s| s.set_configured(false));
        }
    }

    fn reset(&mut self) {
        update(|s| s.set_configured(false));
    }

    fn configured(&mut self, configured: bool) {
        update(|s| s.set_configured(configured));
    }

    fn control_out(&mut self, req: Request, data: &[u8]) -> Option<OutResponse> {
        if (req.request_type, req.recipient, req.index)
            != (RequestType::Class, Recipient::Interface, 0)
        {
            return None;
        }

        match req.request {
            0x20 => {
                // SET_LINE_CODING
                let Some(coding) = LineCoding::decode(data) else {
                    return Some(OutResponse::Rejected);
                };

                update(|s| s.coding = coding);
                Some(OutResponse::Accepted)
            }
            0x22 => {
                // SET_CONTROL_LINE_STATE
                update(|s| s.set_dtr(req.value & 1 != 0));
                None // Let CDC acknowledge DTR/RTS; neither controls a physical GPIO.
            }
            _ => None,
        }
    }

    fn control_in<'a>(&'a mut self, req: Request, buf: &'a mut [u8]) -> Option<InResponse<'a>> {
        if (req.request_type, req.recipient, req.index, req.request)
            != (RequestType::Class, Recipient::Interface, 0, 0x21)
        {
            return None;
        }

        buf[..7].copy_from_slice(&snapshot().coding.encode());
        Some(InResponse::Accepted(&buf[..7]))
    }
}

fn uart_config(coding: LineCoding) -> uart::Config {
    let mut config = uart::Config::default();
    config.baudrate = coding.baud;

    config.data_bits = match coding.data_bits {
        5 => uart::DataBits::DataBits5,
        6 => uart::DataBits::DataBits6,
        7 => uart::DataBits::DataBits7,
        _ => uart::DataBits::DataBits8,
    };

    config.parity = match coding.parity {
        1 => uart::Parity::ParityOdd,
        2 => uart::Parity::ParityEven,
        _ => uart::Parity::ParityNone,
    };

    config.stop_bits = if coding.stop_bits == 2 {
        uart::StopBits::STOP2
    } else {
        uart::StopBits::STOP1
    };

    config
}

async fn host_to_uart<'d>(mut usb: Receiver<'d, UsbDriver<'d>>, mut tx: BufferedUartTx<'d>) -> ! {
    let mut applied = LineCoding::DEFAULT;
    let mut packet = [0; 64];

    loop {
        let session = snapshot();
        if session.coding != applied {
            // Finish accepted bytes at the old framing, including hardware FIFO.
            tx.flush().await.unwrap();
            let requested = snapshot().coding;
            tx.set_config(uart_config(requested));
            applied = requested;
        }

        if !snapshot().active() {
            TX_CHANGED.wait().await;
            continue;
        }

        match select(TX_CHANGED.wait(), usb.read_packet(&mut packet)).await {
            Either::First(()) => continue,
            Either::Second(Ok(n)) => {
                // A control event can arrive in the same USB poll as this packet.
                let now = snapshot();
                if !now.active() || now.epoch != session.epoch {
                    continue;
                }

                if now.coding != applied {
                    tx.flush().await.unwrap();
                    tx.set_config(uart_config(now.coding));
                    applied = now.coding;
                }

                let mut offset = 0;
                while offset < n {
                    offset += tx.write(&packet[offset..n]).await.unwrap();
                }

                #[cfg(feature = "activity-led")]
                if n != 0 {
                    led::activity();
                }
            }
            Either::Second(Err(EndpointError::Disabled)) => usb.wait_connection().await,
            Either::Second(Err(EndpointError::BufferOverflow)) => unreachable!(),
        }

        yield_now().await;
    }
}

async fn uart_to_host<'d>(mut rx: BufferedUartRx<'d>, mut usb: Sender<'d, UsbDriver<'d>>) -> ! {
    let mut epoch = snapshot().epoch;

    loop {
        // Invalidate buffered output across close/reopen, even if both events
        // happened before this future was polled. Never replay another session.
        let session = snapshot();
        if epoch != session.epoch {
            while matches!(rx.read_ready(), Ok(true)) {
                if let Ok(bytes) = rx.fill_buf().await {
                    let n = bytes.len();
                    rx.consume(n);
                }
            }

            epoch = session.epoch;
        }

        let bytes = match select(RX_CHANGED.wait(), rx.fill_buf()).await {
            Either::First(()) => continue,
            Either::Second(Ok(bytes)) => bytes,
            Either::Second(Err(_)) => {
                yield_now().await;
                continue;
            }
        };

        let n = bytes.len().min(64);
        let session = snapshot();

        if session.active() && session.epoch == epoch {
            // A close/disconnect wakes this select even while USB is stalled.
            match select(RX_CHANGED.wait(), usb.write_packet(&bytes[..n])).await {
                Either::Second(Err(EndpointError::BufferOverflow)) => unreachable!(),
                Either::Second(Ok(())) => {
                    #[cfg(feature = "activity-led")]
                    led::activity();

                    if n == 64 && bytes.len() == n {
                        // Terminate exact-sized bursts; otherwise some host reads wait.
                        let _ = select(RX_CHANGED.wait(), usb.write_packet(&[])).await;
                    }
                }
                _ => {}
            }
        }

        rx.consume(n);
        yield_now().await;
    }
}

#[embassy_executor::main(
    executor = "embassy_rp::executor::Executor",
    entry = "cortex_m_rt::entry"
)]
async fn main(_spawner: embassy_executor::Spawner) {
    let p = embassy_rp::init(Default::default());

    let mut flash =
        embassy_rp::flash::Flash::<embassy_rp::mode::Blocking, { 2 * 1024 * 1024 }>::new_blocking(
            p.FLASH,
        );
    let mut unique_id = [0; 8];
    flash.blocking_unique_id(&mut unique_id).unwrap();

    let mut serial = [0; 16];
    for (i, byte) in unique_id.iter().enumerate() {
        serial[2 * i] = b"0123456789ABCDEF"[(byte >> 4) as usize];
        serial[2 * i + 1] = b"0123456789ABCDEF"[(byte & 15) as usize];
    }

    let mut config = embassy_usb::Config::new(0xc0de, 0xcafe);
    config.manufacturer = Some("Local development");
    config.product = Some("RP2040 USB UART Bridge");
    config.serial_number = Some(core::str::from_utf8(&serial).unwrap());

    // USB power declaration; not a current limiter.
    config.max_power = 150;
    config.max_packet_size_0 = 64;

    // Backing storage must be declared before the drivers that borrow it.
    let mut descriptors = [0; 256];
    let mut bos = [0; 256];
    let mut control_buf = [0; 64];
    let mut control = SerialControl;
    let mut state = State::new();

    let mut tx_buffer = [0; 1024];
    let mut rx_buffer = [0; 4096];

    let mut builder = embassy_usb::Builder::new(
        Driver::new(p.USB, Irqs),
        config,
        &mut descriptors,
        &mut bos,
        &mut [],
        &mut control_buf,
    );
    builder.handler(&mut control);
    let class = CdcAcmClass::new(&mut builder, &mut state, 64);
    let mut usb = builder.build();
    let (sender, receiver) = class.split();

    let uart = BufferedUart::new(
        p.UART0,
        p.PIN_0,
        p.PIN_1,
        Irqs,
        &mut tx_buffer,
        &mut rx_buffer,
        uart_config(LineCoding::DEFAULT),
    );
    let (tx, rx) = uart.split();

    let Pio {
        mut common, sm0, ..
    } = Pio::new(p.PIO0, Irqs);
    let program = PioWs2812Program::new(&mut common);
    let status_led = PioWs2812::new(&mut common, sm0, p.PIN_16, p.DMA_CH0, Irqs, &program);

    join(
        join3(
            usb.run(),
            host_to_uart(receiver, tx),
            uart_to_host(rx, sender),
        ),
        led::run(status_led),
    )
    .await;
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    cortex_m::asm::udf()
}
