// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal obs-websocket 5.7.x client — just enough to save the replay buffer.
//!
//! Wire format verified against the 5.7.3 sources, not against docs:
//!   * op codes: Hello=0 Identify=1 Identified=2 Reidentify=3 Event=5
//!     Request=6 RequestResponse=7
//!   * the server sends Hello first; a client-sent Hello is refused
//!   * rpcVersion 1 uses a FLAT envelope: d.requestType / d.requestData /
//!     d.requestStatus / d.responseData (not the older d.data.opType nesting)
//!   * requestStatus.result is a boolean; code 100 == success
//!   * auth: secret = b64(sha256(password + salt)), auth = b64(sha256(secret + challenge))

use std::net::TcpStream;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tungstenite::{Message, client};

use crate::config::ObsWsSettings;

const OP_IDENTIFY: u64 = 1;
const OP_IDENTIFIED: u64 = 2;
const OP_REQUEST: u64 = 6;
const OP_REQUEST_RESPONSE: u64 = 7;
const RPC_VERSION: u8 = 1;
const IO_TIMEOUT: Duration = Duration::from_secs(10);

fn b64_sha256(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(digest)
}

/// obs-websocket's two-step hash. Password is only ever held in memory here.
fn authentication_string(password: &str, salt: &str, challenge: &str) -> String {
    let secret = b64_sha256(&format!("{password}{salt}"));
    b64_sha256(&format!("{secret}{challenge}"))
}

pub struct Obs {
    socket: tungstenite::WebSocket<TcpStream>,
}

impl Obs {
    /// Connect and identify. `settings` comes from OBS's own config file.
    pub fn connect(settings: &ObsWsSettings) -> Result<Self> {
        let addr = ("127.0.0.1", settings.port());
        let tcp = TcpStream::connect(addr)
            .context("cannot reach OBS on localhost — is OBS running?")?;
        tcp.set_read_timeout(Some(IO_TIMEOUT))?;
        tcp.set_write_timeout(Some(IO_TIMEOUT))?;

        let url = format!("ws://127.0.0.1:{}", settings.port());
        let (mut socket, _) = client(url.as_str(), tcp)
            .context("websocket handshake with OBS failed")?;

        // 1. Server greets us with Hello (op 0), including salt + challenge.
        let hello = loop {
            let msg = socket.read()?;
            if let Some(v) = json_of(&msg)?
                && v["op"].as_u64() == Some(0)
            {
                break v;
            }
        };
        let auth = &hello["d"]["authentication"];

        // 2. Identify (op 1). Flat envelope: fields sit directly in `d`.
        let mut identify = json!({ "rpcVersion": RPC_VERSION });
        if let (Some(salt), Some(challenge)) = (auth["salt"].as_str(), auth["challenge"].as_str()) {
            let password = settings
                .server_password
                .as_deref()
                .context("OBS requires websocket auth but no password is set in its config")?;
            identify["authentication"] = json!(authentication_string(password, salt, challenge));
        }
        socket.send(Message::Text(
            json!({ "op": OP_IDENTIFY, "d": identify }).to_string(),
        ))?;

        // 3. Wait for Identified (op 2).
        loop {
            let msg = socket.read()?;
            let Some(v) = json_of(&msg)? else { continue };
            if v["op"].as_u64() == Some(OP_IDENTIFIED) {
                break;
            }
        }

        Ok(Self { socket })
    }

    /// Send one request and wait for its matching RequestResponse.
    fn request(&mut self, request_type: &str, data: Option<Value>) -> Result<Value> {
        let mut d = json!({
            "requestId": format!("clipper-{}", rand_tag()),
            "requestType": request_type,
            "rpcVersion": RPC_VERSION,
        });
        if let Some(data) = data {
            d["requestData"] = data;
        }
        self.socket
            .send(Message::Text(json!({ "op": OP_REQUEST, "d": d }).to_string()))?;

        let want = d["requestId"].as_str().unwrap_or_default().to_string();
        loop {
            let msg = self.socket.read()?;
            let Some(v) = json_of(&msg)? else { continue };
            if v["op"].as_u64() != Some(OP_REQUEST_RESPONSE) {
                continue; // events (op 5) and anything else: ignore
            }
            let resp = &v["d"];
            if resp["requestId"].as_str() != Some(want.as_str()) {
                continue;
            }
            let status = &resp["requestStatus"];
            if status["result"].as_bool() != Some(true) {
                bail!(
                    "{request_type} failed: code {} {}",
                    status["code"],
                    status["comment"].as_str().unwrap_or("")
                );
            }
            return Ok(resp["responseData"].clone());
        }
    }

    /// Ask OBS to write the replay buffer out as a clip file.
    pub fn save_replay(&mut self) -> Result<Value> {
        self.request("SaveReplayBuffer", None)
    }

    /// Change how far back the replay buffer reaches, in the profile OBS is
    /// actually using. This has to go through OBS: editing `basic.ini` while OBS
    /// is running just gets overwritten when it exits.
    pub fn set_replay_seconds(&mut self, secs: u32) -> Result<String> {
        let list = self.request("GetProfileList", None)?;
        let profile = list["currentProfileName"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("OBS did not report its active profile"))?
            .to_string();
        self.request(
            "SetProfileParameter",
            Some(json!({
                "profileName": &profile,
                "parameterCategory": "SimpleOutput",
                "parameterName": "RecRBTime",
                "parameterValue": secs.to_string(),
            })),
        )?;
        Ok(profile)
    }

    /// Read the current replay buffer length from OBS.
    pub fn replay_seconds(&mut self) -> Result<Option<u64>> {
        let list = self.request("GetProfileList", None)?;
        let profile = list["currentProfileName"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("OBS did not report its active profile"))?
            .to_string();
        let r = self.request(
            "GetProfileParameter",
            Some(json!({
                "profileName": &profile,
                "parameterCategory": "SimpleOutput",
                "parameterName": "RecRBTime",
            })),
        )?;
        let v = &r["parameterValue"];
        // OBS hands ini values back as strings, but do not assume it.
        Ok(v
            .as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| v.as_u64()))
    }

    /// Is the replay buffer currently recording?
    pub fn replay_active(&mut self) -> Result<bool> {
        let data = self.request("GetReplayBufferStatus", None)?;
        Ok(data["outputActive"].as_bool().unwrap_or(false))
    }

    /// Start the replay buffer (OBS does not auto-start it unless launched with
    /// --startreplaybuffer, which our systemd unit does).
    pub fn start_replay(&mut self) -> Result<()> {
        self.request("StartReplayBuffer", None)?;
        Ok(())
    }

    /// Stop the replay buffer. Used to apply a new buffer length: OBS only
    /// reads RecRBTime when the buffer starts.
    pub fn stop_replay(&mut self) -> Result<()> {
        self.request("StopReplayBuffer", None)?;
        Ok(())
    }

    /// Set the buffer length and restart the buffer so the new length applies
    /// immediately. Shared by `clipctl buffer` and the GUI dropdown so the two
    /// cannot drift apart.
    pub fn apply_buffer_seconds(secs: u32) -> Result<String> {
        let settings = crate::config::ObsWsSettings::load()?;
        let mut obs = Obs::connect(&settings)?;
        let profile = obs.set_replay_seconds(secs)?;
        if obs.replay_active()? {
            obs.stop_replay()?;
            // OBS tears the buffer down asynchronously; starting again too soon
            // comes back as an opaque code 500.
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if !obs.replay_active()? {
                    break;
                }
            }
        }
        obs.start_replay()?;
        Ok(profile)
    }

    /// Read the buffer length without a caller having to wire up a connection.
    pub fn current_buffer_seconds() -> Result<Option<u64>> {
        let settings = crate::config::ObsWsSettings::load()?;
        let mut obs = Obs::connect(&settings)?;
        obs.replay_seconds()
    }

    /// "OBS 32.1.1 / obs-websocket 5.7.3" — handy for `clipctl status`.
    pub fn versions(&mut self) -> Result<String> {
        let data = self.request("GetVersion", None)?;
        Ok(format!(
            "OBS {} / obs-websocket {} (rpc {})",
            data["obsVersion"].as_str().unwrap_or("?"),
            data["obsWebSocketVersion"].as_str().unwrap_or("?"),
            data["rpcVersion"].as_u64().unwrap_or(0),
        ))
    }

    /// Where OBS writes clips.
    pub fn record_dir(&mut self) -> Result<String> {
        let data = self.request("GetRecordDirectory", None)?;
        Ok(data["recordDirectory"].as_str().unwrap_or("").to_string())
    }
}

fn json_of(msg: &Message) -> Result<Option<Value>> {
    match msg {
        Message::Text(t) => Ok(Some(serde_json::from_str(t.as_str())?)),
        Message::Close(_) => Err(anyhow!("OBS closed the websocket connection")),
        _ => Ok(None),
    }
}

/// Tiny unique id without pulling in a rand dependency: clock + pid.
fn rand_tag() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    nanos ^ ((std::process::id() as u128) << 64)
}

#[cfg(test)]
mod tests {
    use super::authentication_string;

    #[test]
    fn auth_string_is_deterministic_and_two_stepped() {
        // Same inputs must always hash the same way, or OBS would reject every
        // reconnect. Values are throwaway, not a real password.
        let a = authentication_string("hunter2", "salt1", "chal1");
        let b = authentication_string("hunter2", "salt1", "chal1");
        assert_eq!(a, b);
        assert_ne!(a, authentication_string("hunter2", "salt2", "chal1"));
        assert_eq!(a, authentication_string("hunter2", "salt1", "chal1"));
    }
}
