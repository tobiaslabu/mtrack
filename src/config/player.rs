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
use super::audio::Audio;
use super::controller::Controller;
use super::dmx::Dmx;
use super::midi::Midi;
use super::statusevents::StatusEvents;
use super::trackmappings::TrackMappings;
use config::{Config, File};
use serde::Deserialize;
use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use tracing::error;

/// The configuration for the multitrack player.
#[derive(Deserialize)]
pub struct Player {
    /// The controllers configuration.
    controllers: Option<Vec<Controller>>,
    /// The audio configuration section.
    audio: Audio,
    /// The track mappings for the player.
    track_mappings: TrackMappings,
    /// The MIDI configuration section.
    midi: Option<Midi>,
    /// The DMX configuration.
    dmx: Option<Dmx>,
    /// Events to emit to report status out via MIDI.
    status_events: Option<StatusEvents>,
    /// The path to the playlist.
    playlist: Option<String>,
    /// The path to the song definitions.
    songs: String,
}

impl Player {
    pub fn new(
        controllers: Vec<Controller>,
        audio: Audio,
        midi: Option<Midi>,
        dmx: Option<Dmx>,
        track_mappings: HashMap<String, Vec<u16>>,
        status_events: Option<StatusEvents>,
        songs: &str,
    ) -> Player {
        Player {
            controllers: Some(controllers),
            audio,
            track_mappings: TrackMappings { track_mappings },
            midi,
            dmx,
            status_events,
            playlist: None,
            songs: songs.to_string(),
        }
    }

    /// Deserializes a file from the path into a player configuration struct.
    pub fn deserialize(path: &Path) -> Result<Player, Box<dyn Error>> {
        Ok(Config::builder()
            .add_source(File::from(path))
            .build()?
            .try_deserialize::<Player>()?)
    }

    /// Gets the controllers configuration.
    pub fn controllers(&self) -> Vec<Controller> {
        match &self.controllers {
            Some(controllers) => controllers.clone(),
            None => vec![],
        }
    }

    /// Gets the audio configuration.
    pub fn audio(&self) -> Audio {
        self.audio.clone()
    }

    /// Gets the track mapping configuration.
    pub fn track_mappings(&self) -> &HashMap<String, Vec<u16>> {
        &self.track_mappings.track_mappings
    }

    /// Gets the MIDI configuration.
    pub fn midi(&self) -> Option<Midi> {
        self.midi.clone()
    }

    /// Gets the DMX configuration.
    pub fn dmx(&self) -> Option<&Dmx> {
        self.dmx.as_ref()
    }

    /// Gets the status events configuration.
    pub fn status_events(&self) -> Option<StatusEvents> {
        self.status_events.clone()
    }

    /// Gets the path to the playlist.
    pub fn playlist(&self) -> Option<PathBuf> {
        self.playlist.as_ref().map(PathBuf::from)
    }

    /// Gets the path to the song definitions.
    pub fn songs(&self, player_path: &Path) -> PathBuf {
        let songs_path_config = PathBuf::from(&self.songs);
        if songs_path_config.is_absolute() {
            return songs_path_config;
        }
        let player_path_directory = match player_path.parent() {
            Some(path) => path,
            None => {
                error!("Could not find parent of player path {player_path:?}");
                return songs_path_config;
            }
        };
        player_path_directory.join(&self.songs)
    }
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use crate::config::{audio::DEFAULT_AUDIO_PLAYBACK_DELAY, Player};


    #[test]
    fn test_deserialize_ok()
    {
        let path = Path::new("assets/test_data/player_ok.yml");
        let deserialized = match Player::deserialize(&path) {
            Ok(deserialized) => {
assert!(true);
                deserialized
            },
            Err(error) => {
                assert!(false, "Could not deserialize valid player configuration. {error}");
                return;
            }
        };
        assert_eq!(deserialized.controllers().len(), 0, "Expected not to find any controllers in minimal configuration");
        let playback_delay = match deserialized.audio().playback_delay() {
            Ok(playback_delay) => playback_delay,
            Err(_) => todo!(),
        };
        assert_eq!(playback_delay, DEFAULT_AUDIO_PLAYBACK_DELAY, "Expected default playback delay to be used");
    }

}
