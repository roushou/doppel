//! The web console's listener: its saved setting (`web.json` in the
//! data root), and the gateway that runs while it's on. A supervisor thread
//! of the daemon's applies the setting; requests change it and wait for it
//! to apply.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::net::{Ipv6Addr, SocketAddr};
use std::num::NonZeroU16;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use doppel_gateway::{Certificate, Connections, Fingerprint, Gateway, TokenId};
use serde::{Deserialize, Serialize};

/// The TCP port the listener takes: 1 to 65535.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WebPort(NonZeroU16);

impl WebPort {
    pub(crate) const fn get(self) -> u16 {
        self.0.get()
    }
}

impl TryFrom<u32> for WebPort {
    type Error = String;

    fn try_from(port: u32) -> Result<Self, Self::Error> {
        u16::try_from(port)
            .ok()
            .and_then(NonZeroU16::new)
            .map(Self)
            .ok_or_else(|| format!("{port} isn't a TCP port: one is 1 to 65535"))
    }
}

/// Whether the daemon listens for browsers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebSetting {
    Off,
    On(WebPort),
}

/// `web.json`, version 1: `{"version":1,"port":7443}`, or no port for off.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingFile {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u32>,
}

impl WebSetting {
    const VERSION: u32 = 1;

    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let file: SettingFile = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if file.version != Self::VERSION {
            return Err(format!(
                "version {} is newer than this doppel",
                file.version
            ));
        }
        file.port
            .map_or(Ok(Self::Off), |port| WebPort::try_from(port).map(Self::On))
    }

    fn to_json(self) -> String {
        let file = SettingFile {
            version: Self::VERSION,
            port: match self {
                Self::Off => None,
                Self::On(port) => Some(u32::from(port.get())),
            },
        };
        // Numbers only: serializing can't fail.
        let mut json = serde_json::to_string(&file).unwrap_or_default();
        json.push('\n');
        json
    }
}

/// What the listener is doing.
#[derive(Debug, Clone)]
pub(crate) enum WebStatus {
    Off,
    Listening {
        port: WebPort,
        fingerprint: Fingerprint,
        names: Vec<String>,
    },
    Failed {
        port: WebPort,
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WebError {
    #[error("saving the web setting in {}: {source}", path.display())]
    Save {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug)]
struct State {
    /// The setting to apply, and how many times it was set.
    wanted: WebSetting,
    set: u64,
    /// The last setting the supervisor applied.
    applied: u64,
    status: WebStatus,
    gateway: Option<Arc<Gateway>>,
    /// The daemon stopped serving: the supervisor returns.
    ending: bool,
}

#[derive(Debug)]
pub(crate) struct Web {
    setting: PathBuf,
    tls: PathBuf,
    state: Mutex<State>,
    changed: Condvar,
}

impl Web {
    /// How long a change waits for the listener to start or stop.
    const SETTLE: Duration = Duration::from_secs(10);

    pub(crate) fn in_root(root: &Path) -> Self {
        Self {
            setting: root.join("web.json"),
            tls: root.join("tls"),
            state: Mutex::new(State {
                wanted: WebSetting::Off,
                set: 0,
                applied: 0,
                status: WebStatus::Off,
                gateway: None,
                ending: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The saved setting. One that can't be read is off, said in the log.
    fn saved(&self) -> WebSetting {
        match fs::read(&self.setting) {
            Ok(bytes) => WebSetting::parse(&bytes).unwrap_or_else(|error| {
                eprintln!(
                    "doppeld: web: {} can't be read, so the listener is off: {error}",
                    self.setting.display()
                );
                WebSetting::Off
            }),
            Err(_) => WebSetting::Off,
        }
    }

    pub(crate) fn status(&self) -> WebStatus {
        self.state().status.clone()
    }

    /// Saves `setting` and waits, briefly, for the supervisor to apply it.
    pub(crate) fn set(&self, setting: WebSetting) -> Result<WebStatus, WebError> {
        let staged = self.setting.with_extension("json.new");
        let save = || -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&staged)?;
            file.write_all(setting.to_json().as_bytes())?;
            file.sync_all()?;
            fs::rename(&staged, &self.setting)
        };
        save().map_err(|source| WebError::Save {
            path: self.setting.clone(),
            source,
        })?;
        let mut state = self.state();
        state.wanted = setting;
        state.set += 1;
        let ours = state.set;
        if let Some(gateway) = &state.gateway {
            gateway.stop();
        }
        self.changed.notify_all();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, Self::SETTLE, |state| state.applied < ours)
            .unwrap_or_else(PoisonError::into_inner);
        Ok(state.status.clone())
    }

    /// Closes the browser connections that hold a revoked token.
    pub(crate) fn revoke(&self, token: &TokenId) {
        if let Some(gateway) = &self.state().gateway {
            gateway.revoke(token);
        }
    }

    /// Stops the listener and the supervisor: the daemon stopped serving.
    pub(crate) fn end(&self) {
        let mut state = self.state();
        state.ending = true;
        if let Some(gateway) = &state.gateway {
            gateway.stop();
        }
        self.changed.notify_all();
    }

    /// Applies the setting, from the saved one, until [`Self::end`]:
    /// listens while it's on, with `connections` serving the browsers.
    pub(crate) fn supervise(&self, connections: &dyn Connections) {
        {
            let mut state = self.state();
            state.wanted = self.saved();
            state.set += 1;
        }
        loop {
            let (wanted, set) = {
                let state = self
                    .changed
                    .wait_while(self.state(), |state| {
                        !state.ending && state.applied == state.set
                    })
                    .unwrap_or_else(PoisonError::into_inner);
                if state.ending {
                    return;
                }
                (state.wanted, state.set)
            };
            let WebSetting::On(port) = wanted else {
                self.applied(set, WebStatus::Off, None);
                continue;
            };
            let started = Self::start(&self.tls, port, connections.now());
            let (gateway, listener, status) = match started {
                Ok(started) => started,
                Err(reason) => {
                    eprintln!(
                        "doppeld: web: not listening on port {}: {reason}",
                        port.get()
                    );
                    self.applied(set, WebStatus::Failed { port, reason }, None);
                    continue;
                }
            };
            eprintln!("doppeld: web: listening on port {}", port.get());
            self.applied(set, status, Some(Arc::clone(&gateway)));
            let served = gateway.serve(&listener, connections);
            let mut state = self.state();
            state.gateway = None;
            match served {
                // Stopped for a new setting, or the daemon's end.
                Ok(()) => state.status = WebStatus::Off,
                Err(error) => {
                    eprintln!("doppeld: web: the listener failed: {error}");
                    state.status = WebStatus::Failed {
                        port,
                        reason: error.to_string(),
                    };
                }
            }
            drop(state);
            self.changed.notify_all();
        }
    }

    fn applied(&self, set: u64, status: WebStatus, gateway: Option<Arc<Gateway>>) {
        let mut state = self.state();
        state.applied = set;
        state.status = status;
        state.gateway = gateway;
        // A change that came while this one started applies next.
        if state.set != set
            && let Some(gateway) = &state.gateway
        {
            gateway.stop();
        }
        drop(state);
        self.changed.notify_all();
    }

    fn start(
        tls: &Path,
        port: WebPort,
        now: SystemTime,
    ) -> Result<(Arc<Gateway>, std::net::TcpListener, WebStatus), String> {
        let names = names();
        let certificate =
            Certificate::load_or_make(tls, &names, now).map_err(|error| error.to_string())?;
        let gateway = Gateway::new(&certificate).map_err(|error| error.to_string())?;
        // Every address, IPv4 too: macOS's IPv6 sockets take both.
        let address = SocketAddr::from((Ipv6Addr::UNSPECIFIED, port.get()));
        let listener = Gateway::bind(address).map_err(|error| error.to_string())?;
        let status = WebStatus::Listening {
            port,
            fingerprint: certificate.fingerprint(),
            names,
        };
        Ok((Arc::new(gateway), listener, status))
    }
}

/// What a browser may reach this Mac as: its `.local` name, `localhost`,
/// and its addresses, link-local ones aside.
fn names() -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(host) = nix::unistd::gethostname() {
        let host = host.to_string_lossy().into_owned();
        if !host.is_empty() {
            let local = if host.ends_with(".local") {
                host.clone()
            } else {
                format!("{}.local", host.split('.').next().unwrap_or(&host))
            };
            names.push(local);
        }
    }
    names.push("localhost".into());
    if let Ok(interfaces) = nix::ifaddrs::getifaddrs() {
        for interface in interfaces {
            let Some(address) = interface.address else {
                continue;
            };
            let ip = if let Some(v4) = address.as_sockaddr_in() {
                std::net::IpAddr::V4(v4.ip())
            } else if let Some(v6) = address.as_sockaddr_in6() {
                std::net::IpAddr::V6(v6.ip())
            } else {
                continue;
            };
            let link_local = match ip {
                std::net::IpAddr::V4(v4) => v4.is_link_local(),
                std::net::IpAddr::V6(v6) => v6.is_unicast_link_local(),
            };
            let name = ip.to_string();
            if !link_local && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `web.json` as version 1 writes it.
    #[test]
    fn the_setting_file_is_pinned() {
        let on = WebSetting::On(WebPort::try_from(7443).unwrap());
        assert_eq!(on.to_json(), "{\"version\":1,\"port\":7443}\n");
        assert_eq!(WebSetting::Off.to_json(), "{\"version\":1}\n");
        assert_eq!(WebSetting::parse(on.to_json().as_bytes()), Ok(on));
        assert_eq!(
            WebSetting::parse(WebSetting::Off.to_json().as_bytes()),
            Ok(WebSetting::Off)
        );
        for wrong in [
            "{\"version\":2}",
            "{\"version\":1,\"port\":0}",
            "{\"version\":1,\"port\":65536}",
            "{\"version\":1,\"bind\":\"x\"}",
        ] {
            assert!(WebSetting::parse(wrong.as_bytes()).is_err(), "{wrong}");
        }
    }
}
