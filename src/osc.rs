use std::{
    collections::HashSet,
    io::Error,
    net::{IpAddr, SocketAddr},
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

use futures::{
    channel::mpsc::{self, Sender},
    lock::Mutex,
    SinkExt, StreamExt,
};
use rosc::{encoder, OscMessage, OscPacket, OscType};
use tokio::{net::UdpSocket, select, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, span, warn, Level};

use crate::{config::osc::Config, player::PlayerMessage};

pub struct Handle {
    player_tx: Sender<PlayerMessage>,
    accept_driver_commands: AtomicBool,
    driver_rx: Option<tokio::sync::mpsc::Receiver<OscPacket>>,
}

struct OscPlayerState {
    pub(super) playlist: Vec<String>,
    pub(super) current_song: Option<String>,
    pub(super) is_playing: bool,
}

enum StateRequest {
    Playlist,
    CurrentSong,
    IsPlaying,
}

enum OscOutMessage {
    Player(PlayerMessage),
    OscPacket(OscPacket),
    State(StateRequest, SocketAddr),
    AddRecipient(SocketAddr),
}

struct Outlets {
    osc_out_tx: Sender<OscOutMessage>,
    driver_tx: tokio::sync::mpsc::Sender<OscPacket>,
}

///
/// Player message ---> OSC out to registered receivers
/// OSC in -----------> UDP Listen ---> OSC Driver  --> Player action
/// |-------------> OSC out to sender
///
impl Handle {
    pub fn new(config: &Config) -> Self {
        let (player_tx, player_rx) = mpsc::channel::<PlayerMessage>(4);
        let (driver_tx, driver_rx) = tokio::sync::mpsc::channel(4);
        let (osc_in_tx, osc_in_rx) = mpsc::channel::<()>(1);
        let osc_handle = Self {
            // join,
            player_tx,
            accept_driver_commands: AtomicBool::new(false),
            driver_rx: Some(driver_rx),
        };
        let (osc_out_tx, osc_out_rx) = mpsc::channel::<OscOutMessage>(4);
        let osc_out_tx_from_player = osc_out_tx.clone();
        let outlets = Arc::new(Mutex::new(Outlets {
            osc_out_tx,
            driver_tx,
        }));

        let token = CancellationToken::new();
        let socket: Arc<Mutex<Option<UdpSocket>>> = Arc::new(Mutex::new(None));
        let _osc_receiver = OscReceiver::new(
            osc_in_rx,
            socket.clone(),
            outlets,
            token.child_token(),
            config,
        );
        let _osc_sender =
            OscSender::new(osc_in_tx, osc_out_rx, socket.clone(), token.child_token());

        let _player_receiver = PlayerReceiver::new(player_rx, osc_out_tx_from_player);
        osc_handle
    }

    pub(crate) fn enable_driver(&self) {
        self.accept_driver_commands
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn get_player_tx(&self) -> Sender<PlayerMessage> {
        self.player_tx.clone()
    }

    pub fn get_driver_rx(&mut self) -> Option<tokio::sync::mpsc::Receiver<OscPacket>> {
        self.driver_rx.take()
    }
}

enum ResponseToOsc {
    Osc(OscPacket),
    State(StateRequest, SocketAddr),
    Driver(OscPacket, SocketAddr),
    Empty,
}

impl From<(StateRequest, Option<SocketAddr>)> for ResponseToOsc {
    fn from((state_request, socket_addr): (StateRequest, Option<SocketAddr>)) -> Self {
        match socket_addr {
            Some(socket_addr) => Self::State(state_request, socket_addr),
            None => Self::Empty,
        }
    }
}

struct OscReceiver {}
impl OscReceiver {
    fn new(
        mut osc_in_rx: futures::channel::mpsc::Receiver<()>,
        socket_receive: Arc<Mutex<Option<UdpSocket>>>,
        outlets: Arc<Mutex<Outlets>>,
        token_receive: CancellationToken,
        config: &Config,
    ) -> Self {
        let osc_receive = |listen_addr: IpAddr, listen_port: u16| async move {
            async fn initialize_socket(
                socket_arc: Arc<Mutex<Option<UdpSocket>>>,
                listen_addr: IpAddr,
                listen_port: u16,
            ) {
                let bind_addr = SocketAddr::new(listen_addr, listen_port);
                let s = UdpSocket::bind(bind_addr).await.unwrap();

                let mut locked_socket = socket_arc.lock().await;
                *locked_socket = Some(s);
            }

            async fn receive_socket(
                socket_arc: Arc<Mutex<Option<UdpSocket>>>,
            ) -> Option<(OscPacket, SocketAddr)> {
                let locked_socket = socket_arc.lock().await;
                let socket_ref = locked_socket.as_ref();
                match socket_ref {
                    Some(socket) => {
                        let mut buf = [0u8; rosc::decoder::MTU];

                        let recv_result = socket.recv_from(&mut buf).await;
                        match recv_result {
                            Ok((size, addr)) => {
                                let (remainder, packet) =
                                    rosc::decoder::decode_udp(&buf[..size]).unwrap();
                                assert_eq!(
                                    remainder.len(),
                                    0,
                                    "Did not parse all of the received bytes from the UDP socket"
                                );
                                Some((packet, addr))
                            }
                            Err(_e) => None,
                        }
                    }
                    None => None,
                }
            }

            initialize_socket(socket_receive.clone(), listen_addr, listen_port).await;
            loop {
                select! {
                    recv_result = receive_socket(socket_receive.clone()) => {
                        match recv_result{
                            Some((packet, recipient)) => {
                                Self::handle_packet(&packet, outlets.clone(), Some(recipient)).await;
                            },
                            None => todo!(),
                        };
                    }

                    _ = osc_in_rx.next() => {
                        // Give another task the opportunity to lock the Mutex on the socket
                        tokio::time::sleep(Duration::from_nanos(1)).await;
                    }

                    () = token_receive.cancelled() => {
                        break;
                    }
                }
            }
            info!("OSC receive task finished!");
            Ok(())
        };
        let _osc_in_handle: JoinHandle<Result<(), Error>> =
            tokio::spawn(osc_receive(config.listen_host, config.listen_port));
        Self {}
    }

    async fn handle_packet(
        packet: &OscPacket,
        outlets: Arc<Mutex<Outlets>>,
        recipient: Option<SocketAddr>,
    ) {
        let response = match packet {
            OscPacket::Message(osc_message) => {
                debug!("Got a Message {osc_message:?}");
                let osc_addr = &osc_message.addr;
                match osc_message.addr.as_str().split("/").nth(1) {
                    Some("driver") => match recipient {
                        Some(recipient) => ResponseToOsc::Driver(packet.to_owned(), recipient),
                        None => ResponseToOsc::Empty,
                    },
                    Some("song") => ResponseToOsc::from((StateRequest::CurrentSong, recipient)),
                    Some("playlist") => ResponseToOsc::from((StateRequest::Playlist, recipient)),
                    Some("is_playing") => ResponseToOsc::from((StateRequest::IsPlaying, recipient)),
                    Some(s) => {
                        debug!("Unknown OSC addr: {s}");
                        ResponseToOsc::Empty
                    }
                    _ => {
                        warn!("Unknown OSC addr: {osc_addr}");
                        ResponseToOsc::Empty
                    }
                }
            }
            OscPacket::Bundle(osc_bundle) => {
                debug!("Got a bundle {osc_bundle:?}");
                ResponseToOsc::Osc(packet.to_owned())
            }
        };
        match Self::deliver_response(response, outlets).await {
            Ok(_r) => {}
            Err(e) => warn!("Failed to deliver response! {e}"),
        };
    }

    async fn deliver_response(
        response: ResponseToOsc,
        outlets: Arc<Mutex<Outlets>>,
    ) -> Result<(), String> {
        match response {
            ResponseToOsc::Osc(osc_packet) => {
                match outlets
                    .lock()
                    .await
                    .osc_out_tx
                    .send(OscOutMessage::OscPacket(osc_packet))
                    .await
                {
                    Ok(_r) => {}
                    Err(e) => {
                        warn!("Failed to send response to OSC out endpoint! {e}");
                    }
                };
            }
            ResponseToOsc::State(state_request, recipient) => {
                debug!("Response to state request {recipient}");
                match outlets
                    .lock()
                    .await
                    .osc_out_tx
                    .send(OscOutMessage::State(state_request, recipient))
                    .await
                {
                    Ok(_r) => {}
                    Err(e) => {
                        warn!("Failed to send response to OSC out endpoint! {e}");
                    }
                };
            }
            ResponseToOsc::Driver(osc_packet, recipient) => {
                match outlets.lock().await.driver_tx.send(osc_packet).await {
                    Ok(_v) => {
                        info!("Successfully sent OSC packet to driver");
                    }
                    Err(e) => {
                        warn!("Failed to send OSC packet to driver! {e}");
                    }
                };
                match outlets
                    .lock()
                    .await
                    .osc_out_tx
                    .send(OscOutMessage::AddRecipient(recipient))
                    .await
                {
                    Ok(_r) => {}
                    Err(e) => {
                        warn!("Failed to send response to OSC out endpoint! {e}");
                    }
                };
            }
            ResponseToOsc::Empty => {
                info!("No response");
            }
        };
        Ok(())
    }
}

struct OscSender {}
impl OscSender {
    fn new(
        mut osc_in_tx: mpsc::Sender<()>,
        mut osc_out_rx: mpsc::Receiver<OscOutMessage>,
        socket_send: Arc<Mutex<Option<UdpSocket>>>,
        token_send: CancellationToken,
    ) -> Self {
        fn set_state(player_message: &PlayerMessage, state: &mut OscPlayerState) -> PlayerMessage {
            match &player_message {
                PlayerMessage::Select(song) => {
                    state.current_song = Some(song.clone());
                    PlayerMessage::Select(song.clone())
                }
                PlayerMessage::Play(song) => {
                    state.current_song = Some(song.clone());
                    PlayerMessage::Play(song.clone())
                }
                PlayerMessage::Prev => PlayerMessage::Prev,
                PlayerMessage::Next => {
                    let song = match &state.current_song {
                        Some(current_song) => current_song.to_string(),
                        None => "".to_string(),
                    };
                    state.is_playing = false;
                    match state.playlist.iter().position(|e| *e == song) {
                        Some(index) => {
                            if index + 1 < state.playlist.len() {
                                state.current_song = Some(state.playlist[index + 1].to_string());
                            } else {
                                state.current_song = None;
                            }
                        }
                        None => state.current_song = None,
                    };
                    PlayerMessage::Next
                }
                PlayerMessage::Stop => PlayerMessage::Stop,
                PlayerMessage::Playlist(playlist) => {
                    state.playlist = playlist.to_vec();
                    PlayerMessage::Playlist(playlist.clone())
                }
            }
        }

        fn response_for_state_request(
            state_request: &StateRequest,
            state: &OscPlayerState,
        ) -> OscPacket {
            tracing::info!("Gathering response");
            let message = match state_request {
                StateRequest::Playlist => OscMessage {
                    addr: "/playlist".to_string(),
                    args: state
                        .playlist
                        .iter()
                        .map(|e| OscType::String(e.to_string()))
                        .collect(),
                },
                StateRequest::CurrentSong => {
                    let song_title = match &state.current_song {
                        Some(title) => title,
                        None => "-",
                    };
                    OscMessage {
                        addr: "/song".to_string(),
                        args: vec![OscType::String(song_title.to_string())],
                    }
                }
                StateRequest::IsPlaying => OscMessage {
                    addr: "/is_playing".to_string(),
                    args: vec![OscType::Bool(state.is_playing)],
                },
            };
            OscPacket::Message(message)
        }

        let osc_send = || async move {
            let span = span!(Level::INFO, "OSC sender");
            let _enter = span.enter();
            let mut state = OscPlayerState {
                playlist: vec![],
                current_song: None,
                is_playing: false,
            };
            let mut known_recipients = HashSet::new();

            loop {
                select! {
                    send_result = osc_out_rx.next() => {
                        info!("Something to send was requested..");
                        match osc_in_tx.send(()).await{
                            Ok(_v) => debug!("Sent interrupt request"),
                            Err(e) => debug!("Failed to send interrupt request! {e}"),
                        };

                        match send_result {
                            Some(osc_out_message) => {
                                let (message_option, recipient_option) = match osc_out_message {
                                    OscOutMessage::Player(player_message) => {
                                        let player_message = set_state(&player_message, &mut state);
                                        (Some(Self::response_for_player_message(player_message, &state)), None)
                                    },
                                    OscOutMessage::State(state_request, recipient) => {
                                        (Some(response_for_state_request(&state_request, &state)), Some(recipient))
                                    },
                                    OscOutMessage::OscPacket(packet) => (Some(packet), None),
                                    OscOutMessage::AddRecipient(recipient) => {
                                        (None, Some(recipient))
                                    }
                                };

                                let recipient = match recipient_option {
                                    Some(recipient) => {
                                        known_recipients.insert(recipient);
                                        recipient
                                    },
                                    None => match known_recipients.iter().nth(0){
                                        Some(recipient) => {
                                            recipient.to_owned()
                                        },
                                        None => {
                                            warn!("Not sending, since we do not have a recipient address.");
                                            continue
                                        },
                                    },
                                };

                                let message = match message_option {
                                    Some(message) => message,
                                    None => {
                                        continue
                                    },
                                };
                                info!("Message created {message:?}");

                                let msg_buf = encoder::encode(&message).unwrap();
                                info!("Sending OSC message {message:?} to {recipient}");

                                loop {
                                    tracing::info!("Send loop");
                                    match osc_in_tx.send(()).await{
                                        Ok(_v) => debug!("Sent interrupt request"),
                                        Err(e) => warn!("Failed to send interrupt request! {e}"),
                                    };
                                    match socket_send.try_lock() {
                                        Some(mut locked_socket) => {
                                            locked_socket.as_mut().unwrap().send_to(&msg_buf, recipient).await.unwrap();
                                            break;
                                        },
                                        None => {

                                        },
                                    }
                                };
                                info!("Done");
                            },
                            None => {
                                warn!("Nothing to send..");
                            },
                        }
                    }

                    () = token_send.cancelled() => {
                        break;
                    }
                }
            }
        };
        let _osc_out_handle = tokio::spawn(osc_send());
        Self {}
    }

    fn response_for_player_message(
        player_message: PlayerMessage,
        state: &OscPlayerState,
    ) -> OscPacket {
        const SONG_PLAY_ADDR: &str = "/song/play";
        const SONG_SELECT_ADDR: &str = "/song/select";
        let osc = match player_message {
            PlayerMessage::Select(song) => OscMessage {
                addr: SONG_SELECT_ADDR.to_string(),
                args: vec![OscType::String(song.clone())],
            },
            PlayerMessage::Play(song) => OscMessage {
                addr: SONG_PLAY_ADDR.to_string(),
                args: vec![OscType::String(song.clone())],
            },
            PlayerMessage::Prev => {
                let next_song_title = match &state.current_song {
                    Some(song_title) => song_title.clone(),
                    None => "-".to_string(),
                };
                OscMessage {
                    addr: SONG_SELECT_ADDR.to_string(),
                    args: vec![OscType::String(next_song_title)],
                }
            }
            PlayerMessage::Next => {
                let next_song_title = match &state.current_song {
                    Some(song_title) => song_title.clone(),
                    None => "-".to_string(),
                };
                OscMessage {
                    addr: SONG_SELECT_ADDR.to_string(),
                    args: vec![OscType::String(next_song_title)],
                }
            }
            PlayerMessage::Stop => OscMessage {
                addr: "/stop".to_string(),
                args: [].to_vec(),
            },
            PlayerMessage::Playlist(playlist) => OscMessage {
                addr: "/playlist".to_string(),
                args: playlist
                    .iter()
                    .map(|e| OscType::String(e.to_string()))
                    .collect(),
            },
        };
        OscPacket::Message(osc)
    }
}

struct PlayerReceiver {}
impl PlayerReceiver {
    fn new(
        mut player_rx: mpsc::Receiver<PlayerMessage>,
        mut osc_out_tx_from_player: Sender<OscOutMessage>,
    ) -> Self {
        let _player_join = tokio::spawn(async move {
            loop {
                info!("Waiting for player message..");
                let player_message_option = player_rx.next().await;
                info!("Received a player message");
                match player_message_option {
                    Some(player_message) => match player_message {
                        PlayerMessage::Playlist(playlist) => {
                            let _osc_out_result = osc_out_tx_from_player
                                .send(OscOutMessage::Player(PlayerMessage::Playlist(playlist)))
                                .await;
                        }
                        PlayerMessage::Select(song) => {
                            info!("The player selected song {song}");
                            let _osc_out_result = osc_out_tx_from_player
                                .send(OscOutMessage::Player(PlayerMessage::Select(song)))
                                .await;
                        }
                        PlayerMessage::Play(song) => {
                            info!("The player is playing song {song}");
                            let _osc_out_result = osc_out_tx_from_player
                                .send(OscOutMessage::Player(PlayerMessage::Play(song)))
                                .await;
                        }
                        PlayerMessage::Prev => {
                            let _osc_out_result = osc_out_tx_from_player
                                .send(OscOutMessage::Player(PlayerMessage::Prev))
                                .await;
                        }
                        PlayerMessage::Next => {
                            let _osc_out_result = osc_out_tx_from_player
                                .send(OscOutMessage::Player(PlayerMessage::Next))
                                .await;
                        }
                        PlayerMessage::Stop => {
                            let _osc_out_result = osc_out_tx_from_player
                                .send(OscOutMessage::Player(PlayerMessage::Stop))
                                .await;
                        }
                    },
                    None => {
                        warn!("Nothing received from player");
                    }
                }
            }
        });
        Self {}
    }
}
