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
            let span = span!(Level::DEBUG, "osc driver");
            let _enter = span.enter();

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
                                        Ok(_r) => {
                                            debug!("Sent play message");
                                        }
                                        Err(e) => {
                                            warn!("Failed to send play event to player! {e}");
                                        }
                                    },
                                    "/driver/stop" => match events_tx.blocking_send(Event::Stop) {
                                        Ok(_r) => {
                                            debug!("Sent stop message");
                                        }
                                        Err(e) => {
                                            warn!("Failed to send stop event to player! {e}");
                                        }
                                    },
                                    "/driver/prev" => match events_tx.blocking_send(Event::Prev) {
                                        Ok(_r) => {
                                            debug!("Sent prev message")
                                        }
                                        Err(e) => {
                                            warn!("Failed to send prev event to player! {e}");
                                        }
                                    },
                                    "/driver/next" => match events_tx.blocking_send(Event::Next) {
                                        Ok(_r) => {
                                            debug!("Sent next message!");
                                        }
                                        Err(e) => {
                                            warn!("Failed to send next event to player! {e}");
                                        }
                                    },
                                    "/driver/all_songs" => {
                                        match events_tx.blocking_send(Event::AllSongs) {
                                            Ok(_r) => {
                                                debug!(">Sent all songs message!");
                                            }
                                            Err(e) => {
                                                warn!(
                                                    "Failed to send all songs event to player! {e}"
                                                );
                                            }
                                        }
                                    }
                                    "/driver/playlist" => {
                                        match events_tx.blocking_send(Event::Playlist) {
                                            Ok(_r) => {
                                                debug!(">Sent playlist message!");
                                            }
                                            Err(e) => {
                                                warn!(
                                                    "Failed to send playlist event to player! {e}"
                                                );
                                            }
                                        }
                                    }
                                    s => {
                                        println!(">>>>>>>>>Unknown address {s}");
                                        warn!("Unknown address for OSC driver! {s}");
                                    }
                                };
                            }
                            OscPacket::Bundle(osc_bundle) => {
                                warn!("Hanlding OSC bundles is not supported yet! {osc_bundle:?}");
                            }
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

#[cfg(test)]
mod test {
    use std::{
        collections::HashMap,
        error::Error,
        io,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        path::PathBuf,
        sync::Arc,
        time::Duration,
    };

    use futures::{channel::mpsc, lock::Mutex, SinkExt, StreamExt};
    use rosc::{decoder::MTU, encoder, OscMessage, OscPacket};
    use tokio::{net::UdpSocket, select, sync::RwLock};
    use tracing::{debug, error, level_filters::LevelFilter, span, warn, Level};

    use crate::{
        audio,
        config::{self, osc::Config},
        controller::Controller,
        player::{Player, PlayerDevices},
        playlist::Playlist,
        test::eventually,
    };

    const MTRACK_LISTEN_PORT: u16 = 9876;
    const CLIENT_LISTEN_PORT: u16 = 9877;

    struct OscClient {
        socket: Arc<Mutex<UdpSocket>>,
        tx: mpsc::Sender<()>,
        current_song: Arc<Mutex<String>>,
        is_playing: Arc<RwLock<bool>>,
    }

    async fn receive_osc(socket: Arc<Mutex<UdpSocket>>) -> Result<OscPacket, String> {
        let mut buf = [0u8; MTU];
        let locked_socket = socket.lock().await;
        match locked_socket.recv(&mut buf).await {
            Ok(bytes_received) => {
                debug!("Received OSC message with {bytes_received} bytes");
                match rosc::decoder::decode_udp(&buf) {
                    Ok((_remaining, packet)) => {
                        debug!("Message: {packet:?}");
                        Ok(packet)
                    }

                    Err(error) => {
                        println!("Failed to decode OSC response! {error}");
                        assert!(false, "Failed to decode OSC response! {error}");
                        Err("Failed to decode OSC response".to_string())
                    }
                }
            }
            Err(error) => {
                assert!(false, "Did not receive response to osc message! {error}");
                Err("Did not receive response to osc message".to_string())
            }
        }
    }

    async fn receive_channel(rx: &mut mpsc::Receiver<()>) {
        match rx.next().await {
            Some(_received) => {
                debug!("Received via channel!");
            }
            None => return (),
        }
    }

    impl OscClient {
        pub async fn new(target_ip: IpAddr, target_port: u16) -> Result<Self, io::Error> {
            let bind_addr =
                SocketAddr::new(IpAddr::from(Ipv4Addr::UNSPECIFIED), CLIENT_LISTEN_PORT);
            let socket = UdpSocket::bind(bind_addr).await?;
            let target_addr = SocketAddr::new(IpAddr::from(target_ip), target_port);
            match socket.connect(target_addr).await {
                Ok(_result) => debug!("Client connected to mtrack host"),
                Err(error) => assert!(false, "Could not connect to mtrack host! {error}"),
            };
            let socket = Arc::new(Mutex::new(socket));
            let socket_recv = socket.clone();
            let (tx, mut rx) = mpsc::channel(16);
            let current_song: Arc<Mutex<String>> = Arc::new(Mutex::new("".to_string()));
            let current_song_clone = current_song.clone();
            let is_playing = Arc::new(RwLock::new(false));
            let is_playing_clone = is_playing.clone();
            let listen_osc = move || async move {
                loop {
                    select! {
                        _channel_received = receive_channel(&mut rx) => {
                            debug!("Paused OSC for a moment");
                        }
                        socket_received = receive_osc(socket_recv.clone()) => {
                            debug!("Received from OSC socket");
                            match socket_received {
                                Ok(packet) => {
                                    match packet {
                                        OscPacket::Message(osc_message) => {
                                            match osc_message.addr.as_str() {
                                                "/start" => {
                                                    let mut locked_is_playing = is_playing_clone.write().await;
                                                    *locked_is_playing = true;
                                                },
                                                "/stop" => {
                                                    let mut locked_is_playing = is_playing_clone.write().await;
                                                    *locked_is_playing = false;
                                                },
                                                "/song/play" => {
                                                    let mut locked_current_song = current_song_clone.lock().await;
                                                    *locked_current_song = osc_message.args.into_iter().next().unwrap().string().unwrap().to_string();
                                                    let mut locked_is_playing = is_playing_clone.write().await;
                                                    *locked_is_playing = true;
                                                },
                                                "/song/select" => {
                                                    let mut locked_current_song = current_song_clone.lock().await;
                                                    *locked_current_song = osc_message.args.into_iter().next().unwrap().string().unwrap().to_string();

                                                },
                                                "/playlist" => {
                                                    let mut locked_current_song = current_song_clone.lock().await;
                                                    let playlist = osc_message.args.into_iter().filter_map(|e| {
                                                        match e {
                                                            rosc::OscType::String(s) => {
                                                                Some(s.to_string())
                                                            },
                                                            rosc::OscType::Int(_) |
                                                            rosc::OscType::Float(_) |
                                                            rosc::OscType::Blob(_) |
                                                            rosc::OscType::Time(_) |
                                                            rosc::OscType::Long(_) |
                                                            rosc::OscType::Double(_) |
                                                            rosc::OscType::Char(_) |
                                                            rosc::OscType::Color(_) |
                                                            rosc::OscType::Midi(_) |
                                                            rosc::OscType::Bool(_) |
                                                            rosc::OscType::Array(_) |
                                                            rosc::OscType::Nil |
                                                            rosc::OscType::Inf  => None
                                                        }
                                                    }).collect::<Vec<String>>();
                                                    if playlist.len() > 0 {
                                                        *locked_current_song = playlist[0].to_string()
                                                    };
                                                },
                                                s => {
                                                    warn!("Unhandled addr {s}");
                                                }
                                            }
                                        },
                                        OscPacket::Bundle(_osc_bundle) => {
                                            warn!("Handling OSC bundles is not supported yer! {_osc_bundle:?}");
                                        },
                                    }
                                },
                                Err(error) => {
                                    error!("An error occurred while receiving an OSC message! {error}");
                                },
                            };
                        }
                    }
                    tokio::time::sleep(Duration::from_micros(10)).await;
                }
            };
            let _listen_task = tokio::spawn(listen_osc());

            Ok(Self {
                socket,
                tx,
                current_song,
                is_playing,
            })
        }

        async fn get_state(&self) {
            debug!("Getting current state via OSC");
            println!("Getting current state via OSC");
            match self.send_osc("/playlist").await {
                Ok(_result) => {
                    debug!("Sent playlist request");
                }
                Err(error) => {
                    assert!(false, "Failed to send playlist request! {error}");
                }
            };
            match self.send_osc("/song").await {
                Ok(_result) => {
                    debug!("Sent song request");
                }
                Err(error) => {
                    assert!(false, "Failed to send song request! {error}");
                }
            };
        }

        async fn send_osc(&self, addr: &str) -> Result<(), String> {
            let packet = OscPacket::Message(OscMessage {
                addr: addr.to_string(),
                args: vec![],
            });
            let msg_buf = match encoder::encode(&packet) {
                Ok(buf) => buf,
                Err(error) => {
                    return Err(format!("Could not encode OSC packet! {error}"));
                }
            };

            match self.tx.clone().send(()).await {
                Ok(_s) => debug!("Channel send ok"),
                Err(error) => {
                    assert!(false, "Channel send error! {error}");
                    return Err(error.to_string());
                }
            };

            let locked_socket = self.socket.lock().await;
            match locked_socket.send(&msg_buf).await {
                Ok(bytes_sent) => debug!("Sent {bytes_sent} bytes via OSC"),
                Err(error) => {
                    return Err(format!("Failed to send packet to mtrack! {error}"));
                }
            };
            Ok(())
        }

        pub async fn emit_next(&self) {
            debug!("Sending next message");
            match self.send_osc("/driver/next").await {
                Ok(_result) => debug!("Sent next message"),
                Err(error) => {
                    assert!(false, "Failed to send next message! {error}");
                    return;
                }
            };
        }

        pub async fn emit_prev(&self) {
            match self.send_osc("/driver/prev").await {
                Ok(_result) => debug!("Sent prev message"),
                Err(error) => {
                    assert!(false, "Failed to send prev message! {error}");
                    return;
                }
            };
        }

        pub async fn emit_all_songs(&self) {
            match self.send_osc("/driver/all_songs").await {
                Ok(_result) => debug!("Sent all songs message"),
                Err(error) => {
                    assert!(false, "Failed to send all songs message! {error}");
                    return;
                }
            };
        }

        pub async fn emit_playlist(&self) {
            match self.send_osc("/driver/playlist").await {
                Ok(_result) => debug!("Sent playlist message"),
                Err(error) => {
                    assert!(false, "Failed to send playlist message! {error}");
                    return;
                }
            };
        }

        pub async fn emit_play(&self) {
            match self.send_osc("/driver/play").await {
                Ok(_result) => debug!("Sent play message"),
                Err(error) => {
                    assert!(false, "Failed to send play message! {error}");
                    return;
                }
            };
        }

        pub async fn emit_stop(&self) {
            match self.send_osc("/driver/stop").await {
                Ok(_result) => debug!("Sent stop message"),
                Err(error) => {
                    assert!(false, "Failed to send stop message! {error}");
                    return;
                }
            };
        }

        pub fn get_current_song(&self) -> String {
            let song = match self.current_song.try_lock() {
                Some(locked_song) => locked_song.to_string(),
                None => "".to_string(),
            };
            debug!("Current song: {song}");
            song
        }

        pub fn is_playing(&self) -> bool {
            match self.is_playing.try_read() {
                Ok(value) => *value,
                Err(error) => {
                    assert!(false, "Could not lock is_playing! {error}");
                    false
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn test_osc_controller() -> Result<(), Box<dyn Error>> {
        let subscriber = tracing_subscriber::fmt()
            .with_level(true)
            .with_target(true)
            .log_internal_errors(true)
            .with_max_level(LevelFilter::DEBUG)
            .finish();
        let _default_guard = tracing::subscriber::set_default(subscriber);
        let span = span!(Level::DEBUG, "Test OSC Controller");
        let _entered = span.enter();

        let device = Arc::new(audio::test::Device::get("mock-device"));
        let mappings: HashMap<String, Vec<u16>> = HashMap::new();
        let songs = config::get_all_songs(&PathBuf::from("assets/songs"))?;
        let playlist =
            config::parse_playlist(&PathBuf::from("assets/playlist.yaml"), songs.clone())?;
        let all_songs_playlist = Playlist::from_songs(songs.clone())?;
        let osc_handle = Arc::new(std::sync::Mutex::new(super::Handle::new(&Config {
            listen_host: IpAddr::from(Ipv4Addr::UNSPECIFIED),
            listen_port: MTRACK_LISTEN_PORT,
        })));
        let player_devices = PlayerDevices {
            audio: device.clone(),
            midi: None,
            osc: Some(osc_handle.clone()),
            dmx: None,
        };
        let player = Player::new(
            player_devices,
            mappings,
            playlist.clone(),
            all_songs_playlist.clone(),
            None,
        );
        let driver = Arc::new(super::Driver::new(osc_handle));
        let _controller = Controller::new(player, driver)?;

        println!("Playlist: {}", playlist);
        println!("AllSongs: {}", all_songs_playlist);
        tokio::time::sleep(Duration::from_millis(1000)).await;

        // Test the controller directing the player.
        let osc_client =
            OscClient::new(IpAddr::from(Ipv4Addr::LOCALHOST), MTRACK_LISTEN_PORT).await?;
        osc_client.get_state().await;

        println!("Playlist -> Song 1");
        eventually(
            || playlist.current().name == "Song 1",
            "Playlist never became Song 1",
        );
        eventually(
            || osc_client.get_current_song() == "Song 1",
            "Playlist never became Song 1 in client",
        );

        osc_client.emit_next().await;
        println!("Playlist -> Song 3");
        eventually(
            || playlist.current().name == "Song 3",
            "Playlist never became Song 3",
        );
        eventually(
            || osc_client.get_current_song() == "Song 3",
            "Playlist never became Song 3 in OSC client",
        );
        osc_client.emit_next().await;
        println!("Playlist -> Song 5");
        eventually(
            || playlist.current().name == "Song 5",
            "Playlist never became Song 5",
        );
        eventually(
            || osc_client.get_current_song() == "Song 5",
            "Playlist never became Song 5 in OSC client",
        );

        osc_client.emit_next().await;
        println!("Playlist -> Song 7");
        eventually(
            || playlist.current().name == "Song 7",
            "Playlist never became Song 7",
        );
        eventually(
            || osc_client.get_current_song() == "Song 7",
            "Playlist never became Song 7 in OSC client",
        );

        osc_client.emit_prev().await;
        println!("Playlist -> Song 5");
        eventually(
            || playlist.current().name == "Song 5",
            "Playlist never became Song 5",
        );
        eventually(
            || osc_client.get_current_song() == "Song 5",
            "Playlist never became Song 5 in OSC client",
        );

        println!("Switch to AllSongs");
        osc_client.emit_all_songs().await;
        eventually(
            || all_songs_playlist.current().name == "Song 1",
            "All Songs Playlist never became Song 1",
        );
        eventually(
            || osc_client.get_current_song() == "Song 1",
            "All Songs Playlist never became Song 1 in client",
        );

        println!("AllSongs -> Song 10");
        osc_client.emit_next().await;
        eventually(
            || all_songs_playlist.current().name == "Song 10",
            "All Songs Playlist never became Song 10",
        );
        eventually(
            || osc_client.get_current_song() == "Song 10",
            "Playlist never became Song 10 in OSC client",
        );

        println!("AllSongs -> Song 2");
        osc_client.emit_next().await;
        eventually(
            || all_songs_playlist.current().name == "Song 2",
            "All Songs Playlist never became Song 2",
        );
        eventually(
            || osc_client.get_current_song() == "Song 2",
            "Playlist never became Song 2 in OSC client",
        );

        println!("AllSongs -> Song 10");
        osc_client.emit_prev().await;
        eventually(
            || all_songs_playlist.current().name == "Song 10",
            "All Songs Playlist never became Song 10",
        );
        eventually(
            || osc_client.get_current_song() == "Song 10",
            "Playlist never became Song 10 in OSC client",
        );

        println!("Switch to Playlist");
        osc_client.emit_playlist().await;
        eventually(
            || playlist.current().name == "Song 5",
            "Playlist never became Song 5",
        );
        eventually(
            || osc_client.get_current_song() == "Song 5",
            "Playlist never became Song 5 in OSC client",
        );

        println!("Playlist -> Song 7");
        osc_client.emit_next().await;
        eventually(
            || playlist.current().name == "Song 7",
            "Playlist never became Song 7",
        );
        eventually(
            || osc_client.get_current_song() == "Song 7",
            "Playlist never became Song 7 in OSC client",
        );

        osc_client.emit_play().await;
        eventually(|| device.is_playing(), "Song never started playing");
        eventually(
            || osc_client.is_playing(),
            "Song never started playing according to OSC client",
        );
        osc_client.emit_stop().await;
        eventually(|| !device.is_playing(), "Song never stopped playing");
        eventually(
            || !osc_client.is_playing(),
            "Song never stopped playing according to OSC client",
        );

        Ok(())
    }
}
