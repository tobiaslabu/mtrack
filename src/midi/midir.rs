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
    collections::HashMap,
    error::Error,
    fmt, mem,
    sync::{Arc, Mutex},
    thread,
};

use midir::{MidiInput, MidiInputConnection, MidiInputPort, MidiOutput, MidiOutputPort};
use midly::live::LiveEvent;
use nodi::{Connection, Player};
use tokio::sync::mpsc::Sender;
use tracing::{error, info, span, warn, Level};

use crate::{playsync::CancelHandle, songs::Song};

pub struct Device {
    name: String,
    input_port: Option<MidiInputPort>,
    output_port: Option<MidiOutputPort>,
    event_connection: Box<Mutex<Option<MidiInputConnection<()>>>>,
}

impl super::Device for Device {
    /// Returns the name of the device.
    fn name(&self) -> String {
        self.name.clone()
    }

    fn watch_events(&self, sender: Sender<Vec<u8>>) -> Result<(), Box<dyn Error>> {
        let span = span!(Level::INFO, "wait for event (midir)");
        let _enter = span.enter();

        let mut event_connection = self.event_connection.lock().expect("Unable to get lock");
        if event_connection.is_some() {
            return Err("Already watching events.".into());
        }

        info!("Watching MIDI events.");

        if self.input_port.is_none() {
            warn!("No MIDI output device configured, cannot listen for events.");
            return Ok(());
        }

        let input = MidiInput::new("mtrack player input")?;
        *event_connection = Some(input.connect(
            self.input_port.as_ref().unwrap(),
            "mtrack input watcher",
            move |_, raw_event, _| {
                if let Ok(event) = LiveEvent::parse(raw_event) {
                    info!(event = format!("{:?}", event), "Received MIDI event.");
                }
                if let Err(e) = sender.blocking_send(Vec::from(raw_event)) {
                    error!(
                        err = format!("{:?}", e),
                        "Error sending MIDI event to receiver."
                    );
                }
            },
            (),
        )?);

        Ok(())
    }

    /// Stops watching events.
    fn stop_watch_events(&self) {
        // Explicitly drop the connection.
        let event_connection = self
            .event_connection
            .lock()
            .expect("Error getting mutex.")
            .take();

        mem::drop(event_connection);
    }

    /// Plays the given song through the MIDI interface.
    fn play(&self, song: Arc<Song>, cancel_handle: CancelHandle) -> Result<(), Box<dyn Error>> {
        let span = span!(Level::INFO, "play song (midir)");
        let _enter = span.enter();

        if self.output_port.is_none() {
            warn!(
                song = song.name,
                "No MIDI output device configured, cannot play song."
            );
            return Ok(());
        }

        let maybe_sheet = song.midi_sheet()?;
        if maybe_sheet.is_none() {
            info!(song = song.name, "Song has no MIDI sheet.");
            return Ok(());
        }
        let midi_sheet = maybe_sheet.unwrap();
        let output = MidiOutput::new("mtrack player output")?;

        info!(
            device = self.name,
            song = song.name,
            duration = song.duration_string(),
            "Playing song MIDI."
        );

        let join_handle = {
            let cancel_handle = cancel_handle.clone();

            // Wrap the midir connection in a cancel connection so that we can stop playback.
            let midir_connection =
                output.connect(self.output_port.as_ref().unwrap(), "mtrack player")?;
            let connection = CancelConnection {
                connection: midir_connection,
                cancel_handle: cancel_handle.clone(),
            };
            let mut player = Player::new(midi_sheet.ticker, connection);

            thread::spawn(move || {
                player.play(&midi_sheet.sheet);
                cancel_handle.expire();
            })
        };

        cancel_handle.wait();

        if join_handle.join().is_err() {
            return Err("Error while joining thread!".into());
        }

        Ok(())
    }

    fn emit(&self, song: Arc<Song>) -> Result<(), Box<dyn Error>> {
        let span = span!(Level::INFO, "emit (midir)");
        let _enter = span.enter();

        let event = match song.midi_event {
            Some(midi_event) => midi_event,
            // If there's no event, return early.
            None => return Ok(()),
        };

        let output_port = match &self.output_port {
            Some(output_port) => output_port,
            None => {
                warn!(
                    song = song.name,
                    "No MIDI output device configured, cannot emit event."
                );
                return Ok(());
            }
        };

        let output = MidiOutput::new("mtrack emit output")?;

        info!(
            device = self.name,
            event = format!("{:?}", event),
            "Emitting event."
        );

        // Choosing 8 here because that's what nodi does.
        let mut buf: Vec<u8> = Vec::with_capacity(8);
        event.write(&mut buf)?;
        let mut connection = output.connect(output_port, "mtrack player")?;

        connection.send(&buf)?;

        Ok(())
    }
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut capabilities: Vec<String> = Vec::new();
        if self.input_port.is_some() {
            capabilities.push(String::from("Input"));
        }
        if self.output_port.is_some() {
            capabilities.push(String::from("Output"));
        }

        write!(f, "{} ({})", self.name, capabilities.join("/"))
    }
}

/// Lists midir devices and produces the Device trait.
pub fn list() -> Result<Vec<Box<dyn super::Device>>, Box<dyn Error>> {
    Ok(list_midir_devices()?
        .into_iter()
        .map(|device| {
            let device: Box<dyn super::Device> = Box::new(device);
            device
        })
        .collect())
}

/// Lists midir devices.
fn list_midir_devices() -> Result<Vec<Device>, Box<dyn Error>> {
    let input = MidiInput::new("mtrack input listing")?;
    let output = MidiOutput::new("mtrack output listing")?;
    let input_ports = input.ports();
    let output_ports = output.ports();

    let mut devices: HashMap<String, Device> = HashMap::new();

    for port in input_ports {
        let name = input.port_name(&port)?;
        if !devices.contains_key(&name) {
            devices.insert(
                name.clone(),
                Device {
                    name: name.clone(),
                    input_port: Some(port),
                    output_port: None,
                    event_connection: Box::new(Mutex::new(None)),
                },
            );
        }
    }

    for port in output_ports {
        let name = output.port_name(&port)?;
        if !devices.contains_key(&name) {
            devices.insert(
                name.clone(),
                Device {
                    name: name.clone(),
                    input_port: None,
                    output_port: Some(port),
                    event_connection: Box::new(Mutex::new(None)),
                },
            );
        } else {
            devices.get_mut(&name).unwrap().output_port = Some(port);
        }
    }

    let mut sorted_devices = devices
        .into_iter()
        .map(|entry| entry.1)
        .collect::<Vec<Device>>();
    sorted_devices.sort_by_key(|device| device.name.clone());
    Ok(sorted_devices)
}

/// Gets the given midir device.
pub fn get(name: &String) -> Result<Device, Box<dyn Error>> {
    let mut matches = list_midir_devices()?
        .into_iter()
        .filter(|device| device.name.contains(name))
        .collect::<Vec<Device>>();

    if matches.is_empty() {
        return Err(format!("no device found with name {}", name).into());
    }
    if matches.len() > 1 {
        return Err(format!(
            "found too many devices that match ({}), use a less ambiguous device name",
            matches
                .iter()
                .map(|device| device.name.clone())
                .collect::<Vec<String>>()
                .join(", ")
        )
        .into());
    }

    // We've verified that there's only one element in the vector, so this should be safe.
    Ok(matches.swap_remove(0))
}

/// CancelConnection is a nodi connection that can be cancelled.
struct CancelConnection<C: Connection> {
    connection: C,
    cancel_handle: CancelHandle,
}

impl<C: Connection> Connection for CancelConnection<C> {
    fn play(&mut self, event: nodi::MidiEvent) -> bool {
        if self.cancel_handle.is_cancelled() {
            return false;
        };
        self.connection.play(event)
    }
}
