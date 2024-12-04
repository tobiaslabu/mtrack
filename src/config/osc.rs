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
use std::net::IpAddr;

use serde::Deserialize;

/// A YAML represetnation of the OSC config.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub struct Config {
    pub listen_host: IpAddr,
    pub listen_port: u16,
}

impl Config {}
