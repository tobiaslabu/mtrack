// Copyright (C) 2024 Michael Wilson <mike@mdwn.dev>
//
// This program is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the Free Software
// Foundation, version 3.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
// FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License along with
// this program. If not, see <https://www.gnu.org/licenses/>.
//
use std::{
    io::{self},
    sync::{Arc, Mutex},
};

use rosc::OscPacket;
use tokio::{
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};
use tracing::{debug, error, info, span, warn, Level};

use crate::osc::Handle;

use super::Event;

/// A controller that controls a player using OSC.
pub struct Driver {
    osc_handle: Arc<Mutex<Handle>>,
}

impl Driver {
    pub fn new(osc_handle: Arc<Mutex<Handle>>) -> Driver {
        let handle = osc_handle.clone();
        let enable_driver_result = handle.lock().map(|mutex| {
            mutex.enable_driver();
        });
        match enable_driver_result {
            Ok(_r) => {}
            Err(e) => {
                error!("Could not enable OSC driver! {e}");
            }
        };
        Driver { osc_handle }
    }
}

fn get_driver_rx(osc_handle: &Arc<Mutex<Handle>>) -> Option<Receiver<OscPacket>> {
    match osc_handle.lock() {
        Ok(mut locked) => locked.get_driver_rx(),
        Err(e) => {
            error!("Could not lock Mutex {e}");
            None
        }
    }
}

impl super::Driver for Driver {
    fn monitor_events(&self, events_tx: Sender<Event>) -> JoinHandle<Result<(), io::Error>> {
        info!("Monitoring events to manipulate the player through OSC");
        let mut driver_rx = match get_driver_rx(&self.osc_handle) {
            Some(d) => d,
            None => {
                return tokio::task::spawn_blocking(|| {
                    Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "Could not get receiver for OSC driver",
                    ))
                })
            }
        };

        let join_handle = tokio::task::spawn_blocking(move || {
            let span = span!(Level::INFO, "osc driver");
            let _enter = span.enter();

            info!("OSC driver started.");

            loop {
                if driver_rx.is_closed() {
                    warn!("Driver RX is closed");
                    break;
                }
                let receive_osc = driver_rx.blocking_recv();
                match receive_osc {
                    Some(osc_packet) => {
                        debug!("Received a packet in the driver");
                        debug!("packet: {osc_packet:?}");
                        match osc_packet {
                            OscPacket::Message(osc_message) => {
                                match osc_message.addr.as_str() {
                                    "/driver/play" => match events_tx.blocking_send(Event::Play) {
                                        Ok(_r) => {}
                                        Err(e) => {
                                            warn!("Failed to send play event to player! {e}");
                                        }
                                    },
                                    "/driver/stop" => match events_tx.blocking_send(Event::Stop) {
                                        Ok(_r) => {}
                                        Err(e) => {
                                            warn!("Failed to send stop event to player! {e}");
                                        }
                                    },
                                    "/driver/prev" => match events_tx.blocking_send(Event::Prev) {
                                        Ok(_r) => {}
                                        Err(e) => {
                                            warn!("Failed to send prev event to player! {e}");
                                        }
                                    },
                                    "/driver/next" => match events_tx.blocking_send(Event::Next) {
                                        Ok(_r) => {}
                                        Err(e) => {
                                            warn!("Failed to send next event to player! {e}");
                                        }
                                    },
                                    s => {
                                        warn!("Unknown address for OSC driver! {s}");
                                    }
                                };
                            }
                            OscPacket::Bundle(osc_bundle) => todo!(),
                        };
                    }
                    None => {
                        warn!("Received nothing?");
                    }
                };
            }
            Ok(())
        });
        join_handle
    }
}
