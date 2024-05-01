use std::{
    io::Read,
    sync::{
        atomic::{AtomicUsize, Ordering::SeqCst},
        mpsc, Arc, Mutex,
    },
    thread,
};

use anyhow::{anyhow, Result};
use log::info;

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{delay::FreeRtos, gpio::PinDriver, prelude::Peripherals, reset::restart},
    http::server::EspHttpServer,
    io::{Read as _, Write},
    nvs::EspDefaultNvsPartition,
    ota::EspOta,
    wifi::{BlockingWifi, ClientConfiguration, EspWifi},
};
use multipart::server::ReadEntry;

const WIFI_SSID: &str = env!("WIFI_SSID");
const WIFI_PASS: &str = env!("WIFI_PASS");
const STACK_SIZE: usize = 10240;
const INDEX_HTML: &[u8] = include_bytes!("../web/index.html");
const FIRMWARE_HTML: &[u8] = include_bytes!("../web/firmware.html");
const STYLE_CSS: &[u8] = include_bytes!("../web/style.css");
const MAX_LEN: usize = 4096;

enum Update {
    Start,
    Chunk(Vec<u8>),
    Finish,
}

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?,
        sys_loop,
    )?;

    connect_wifi(&mut wifi)?;

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;

    info!("Wifi DHCP info: {:?}", ip_info);

    let bytes_written = AtomicUsize::new(0);

    let (update_tx, update_rx) = mpsc::channel::<Update>();
    let update_tx_finish = update_tx.clone();

    let update_mutex = Arc::new(Mutex::new(false));
    let update_mutex_write = update_mutex.clone();

    let mut server = create_server()?;

    let _ = thread::spawn(move || {
        let mut esp_ota = EspOta::new()?;

        while let Ok(message) = update_rx.recv() {
            match message {
                Update::Start => {
                    let mut esp_ota_update = Some(esp_ota.initiate_update()?);
                    while let Ok(message) = update_rx.recv() {
                        match message {
                            Update::Chunk(chunk) => {
                                if let Some(updt) = esp_ota_update.as_mut() {
                                    let mut mutex = update_mutex_write.lock().expect("mutex lock");
                                    dbg!("writing chunk", chunk.len());
                                    updt.write(&chunk)?;
                                    *mutex = true;
                                }
                            }
                            Update::Finish => {
                                if let Some(updt) = esp_ota_update.take() {
                                    updt.complete()?;
                                    restart()
                                }
                            }
                            _ => {
                                if let Some(updt) = esp_ota_update.take() {
                                    updt.abort()?;
                                }
                                break;
                            }
                        }
                    }
                }
                _ => {
                    break;
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    });

    server.fn_handler("/", esp_idf_svc::http::Method::Get, |req| {
        req.into_ok_response()?.write_all(INDEX_HTML).map(|_| ())
    })?;
    server.fn_handler("/style.css", esp_idf_svc::http::Method::Get, |req| {
        req.into_response(200, None, &[("Content-Type", "text/css")])?
            .write_all(STYLE_CSS)
            .map(|_| ())
    })?;
    server.fn_handler("/firmware", esp_idf_svc::http::Method::Get, |req| {
        req.into_ok_response()?.write_all(FIRMWARE_HTML).map(|_| ())
    })?;
    server.fn_handler::<anyhow::Error, _>(
        "/firmware/chunk",
        esp_idf_svc::http::Method::Post,
        |mut req| {
            use embedded_svc::http::Headers as _;
            let len = req.content_len().unwrap_or(0) as usize;
            if len > MAX_LEN {
                req.into_status_response(413)?
                    .write_all("Request too big".as_bytes())?;
                return Ok(());
            }

            let boundary = req
                .content_type()
                .and_then(|ct| ct.split_once("boundary="))
                .filter(|(ct, _)| ct.starts_with("multipart/form-data"))
                .map(|(_, boundary)| boundary)
                .ok_or_else(|| anyhow!("content type multipart/form-data required with boundary"))?
                .to_string();

            let mut buf = vec![0; len];
            req.read_exact(&mut buf)?;

            let mut mp = multipart::server::Multipart::with_body(buf.as_slice(), boundary);

            let mut chunk = None;
            let mut index = None;

            while let Some(mut entry) = mp.read_entry_mut().into_result()? {
                match &*entry.headers.name {
                    "chunk" => {
                        let mut d = Vec::new();
                        entry.data.read_to_end(&mut d)?;
                        chunk = Some(d);
                    }
                    "index" => {
                        let mut s = String::new();
                        entry.data.read_to_string(&mut s)?;
                        index = Some(s);
                    }
                    _ => (),
                }
            }

            let Some(index) = index.and_then(|i| i.parse().ok()) else {
                return Err(anyhow::anyhow!("index not found"));
            };

            if index == 0 {
                update_tx.send(Update::Start)?;
            }

            let Some(chunk) = chunk else {
                return Err(anyhow::anyhow!("chunk not found"));
            };

            let chunk_len = chunk.len();
            update_tx.send(Update::Chunk(chunk))?;

            while !*update_mutex.lock().expect("mutex lock") {
                dbg!("wait 10ms");
                FreeRtos::delay_ms(10);
            }
            dbg!("releasing mutex");
            *update_mutex.lock().expect("mutex lock") = false;

            let old_total = bytes_written.fetch_add(chunk_len, SeqCst);

            if old_total != index {
                dbg!("mismatch", old_total, index);
            }

            let total = bytes_written.load(SeqCst);

            req.into_ok_response()?
                .write_all(format!("{total}").as_bytes())
                .map(|_| ())
                .map_err(From::from)
        },
    )?;
    server.fn_handler::<anyhow::Error, _>(
        "/firmware/finish",
        esp_idf_svc::http::Method::Post,
        |req| {
            use embedded_svc::http::Headers as _;
            let len = req.content_len().unwrap_or(0) as usize;
            if len > MAX_LEN {
                req.into_status_response(413)?
                    .write_all("Request too big".as_bytes())?;
                return Ok(());
            }

            bytes_written.load(SeqCst);

            update_tx_finish.send(Update::Finish)?;

            req.into_ok_response()?
                .write_all("ok".as_bytes())
                .map(|_| ())
                .map_err(From::from)
        },
    )?;

    core::mem::forget(wifi);
    core::mem::forget(server);

    let mut led_red = PinDriver::output(peripherals.pins.gpio33)?;

    loop {
        led_red.set_high()?;
        FreeRtos::delay_ms(5000);
        led_red.set_low()?;
        FreeRtos::delay_ms(5000);
    }
}

fn connect_wifi(wifi: &mut BlockingWifi<EspWifi<'static>>) -> Result<()> {
    let wifi_configuration = esp_idf_svc::wifi::Configuration::Client(ClientConfiguration {
        ssid: WIFI_SSID
            .try_into()
            .map_err(|_| anyhow::anyhow!("wifi ssid"))?,
        password: WIFI_PASS
            .try_into()
            .map_err(|_| anyhow::anyhow!("wifi pass"))?,
        ..Default::default()
    });

    wifi.set_configuration(&wifi_configuration)?;

    wifi.start()?;

    wifi.connect()?;

    wifi.wait_netif_up()?;

    Ok(())
}

fn create_server() -> Result<EspHttpServer<'static>> {
    let server_configuration = esp_idf_svc::http::server::Configuration {
        stack_size: STACK_SIZE,
        ..Default::default()
    };

    Ok(EspHttpServer::new(&server_configuration)?)
}
