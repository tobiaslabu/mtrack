// Copyright (C) 2025 Michael Wilson <mike@mdwn.dev>
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
use std::{io, sync::Arc};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, span, warn, Level};

use crate::player::Player;

const PLAY: &str = "play";
const PREV: &str = "prev";
const NEXT: &str = "next";
const STOP: &str = "stop";
const ALL_SONGS: &str = "all_songs";
const PLAYLIST: &str = "playlist";

/// A controller that controls a player using the keyboard.
pub struct Driver {
    player: Arc<Player>,
}

impl Driver {
    pub fn new(player: Arc<Player>) -> Arc<Self> {
        Arc::new(Driver { player })
    }

    fn monitor_io<R, W>(player: Arc<Player>, mut reader: R, mut writer: W) -> Result<(), io::Error>
    where
        R: io::BufRead,
        W: io::Write,
    {
        write!(
            writer,
            "Command ({}, {}, {}, {}, {}, {}): ",
            PLAY, PREV, NEXT, STOP, ALL_SONGS, PLAYLIST,
        )?;
        writer.flush()?;
        let mut input: String = String::default();
        reader.read_line(&mut input)?;

        let player = player.clone();
        match input.trim().to_lowercase().as_str() {
            PLAY => {
                tokio::spawn(async move { player.play().await });
            }
            PREV => {
                tokio::spawn(async move { player.prev().await });
            }
            NEXT => {
                tokio::spawn(async move { player.next().await });
            }
            STOP => {
                tokio::spawn(async move { player.stop().await });
            }
            ALL_SONGS => {
                tokio::spawn(async move { player.switch_to_all_songs().await });
            }
            PLAYLIST => {
                tokio::spawn(async move { player.switch_to_playlist().await });
            }
            unrecognized => {
                warn!(input = input, "Unrecognized input: {unrecognized}");
            }
        }
        Ok(())
    }
}

impl super::Driver for Driver {
    fn monitor_events(&self, cancellation_token: CancellationToken) -> JoinHandle<Result<(), io::Error>> {
        let player = self.player.clone();
        tokio::task::spawn_blocking(move || {
            let span = span!(Level::INFO, "keyboard driver");
            let _enter = span.enter();

            info!("Keyboard driver started.");

            loop {
                if cancellation_token.is_cancelled() {
                    break;
                }
                Self::monitor_io(player.clone(),
                   io::stdin().lock(),
                   io::stdout())?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod test {
    use std::{
        collections::HashMap,
        error::Error,
        io::{BufReader, BufWriter},
        path::Path,
        sync::Arc,
    };

    use crate::{
        config,
        controller::keyboard::{Driver, ALL_SONGS, NEXT, PLAY, PLAYLIST, PREV, STOP},
        playlist::Playlist,
        songs,
        testutil::eventually,
    };

    use super::Player;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_keyboard() -> Result<(), Box<dyn Error>> {
        let songs = songs::get_all_songs(Path::new("assets/songs"))?;
        let player = Arc::new(Player::new(
            songs.clone(),
            Playlist::new(
                &config::Playlist::deserialize(Path::new("assets/playlist.yaml"))?,
                songs,
            )?,
            &config::Player::new(
                vec![config::Controller::Keyboard],
                config::Audio::new("mock-device"),
                Some(config::Midi::new("mock-midi-device", None)),
                None,
                HashMap::new(),
                None,
                "assets/songs",
            ),
        )?);
        let binding = player.audio_device();
        let device = binding.to_mock()?;

        let key_event = |event: &str| {
            Driver::monitor_io(
                player.clone(),
                BufReader::new(format!("{}\n", event).as_bytes()),
                BufWriter::new(Vec::new()),
            )
        };

        // Direct the player.
        println!("Playlist -> Song 1");
        assert_eq!(player.get_playlist().current().name(), "Song 1");

        key_event(NEXT)?;
        println!("Playlist -> Song 3");
        eventually(
            || player.get_playlist().current().name() == "Song 3",
            "Event not processed",
        );

        key_event(PREV)?;
        println!("Playlist -> Song 1");
        eventually(
            || player.get_playlist().current().name() == "Song 1",
            "Event not processed",
        );

        println!("Switch to AllSongs");
        key_event(ALL_SONGS)?;
        eventually(
            || player.get_playlist().current().name() == "Song 1",
            "Event not processed",
        );

        key_event(NEXT)?;
        println!("AllSongs -> Song 10");
        println!("-> {}", player.get_playlist().current().name());
        eventually(
            || player.get_playlist().current().name() == "Song 10",
            &format!("Event not processed {}", player.get_playlist().current().name())
        );

        key_event(NEXT)?;
        println!("AllSongs -> Song 2");
        eventually(
            || player.get_playlist().current().name() == "Song 2",
            "Event not processed",
        );

        key_event(NEXT)?;
        println!("AllSongs -> Song 3");
        eventually(
            || player.get_playlist().current().name() == "Song 3",
            "Event not processed",
        );

        key_event(PLAYLIST)?;
        println!("Switch to Playlist");
        eventually(
            || player.get_playlist().current().name() == "Song 1",
            "Event not processed",
        );

        key_event(NEXT)?;
        println!("Playlist -> Song 3");
        eventually(
            || player.get_playlist().current().name() == "Song 3",
            "Event not processed",
        );

        key_event(PLAY)?;

        // Playlist should have moved to next song.
        eventually(
            || player.get_playlist().current().name() == "Song 5",
            format!(
                "Song never moved to next, on song {}",
                player.get_playlist().current().name()
            )
            .as_str(),
        );

        // Play a song and cancel it.
        key_event(PLAY)?;
        println!("Play Song 5.");
        eventually(|| device.is_playing(), "Song never started playing");

        key_event(STOP)?;
        eventually(|| !device.is_playing(), &format!("Song never stopped playing. Current song: {}", player.get_playlist().current().name()));

        // Player should not have moved to the next song.
        assert_eq!(player.get_playlist().current().name(), "Song 5");

        Ok(())
    }
}
