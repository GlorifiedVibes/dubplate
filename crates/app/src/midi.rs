use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};

use midir::{MidiInput, MidiInputConnection};

use crate::midi_learn::{self, Learn, LearnState};
use crate::midi_mapper::{AppCommand, MidiMapper};
use crate::midi_profile::{self, MidiSettings, ProfileEntry, OFF};

/// Newest-first log of recent MIDI messages, for the Preferences monitor.
pub type Monitor = Arc<Mutex<VecDeque<MonitorLine>>>;
const MONITOR_LINES: usize = 14;

#[derive(Clone, Debug)]
pub struct MonitorLine {
    pub text: String,
    /// What the message does, or None when the profile doesn't use it.
    pub function: Option<String>,
}

/// A MIDI input port and the profile driving it.
#[derive(Clone, Debug)]
pub struct PortStatus {
    /// Name without the session-specific ALSA ids; the key for saved choices.
    pub key: String,
    pub profile_id: Option<String>,
    pub profile_name: Option<String>,
    /// True when the profile was picked by name detection rather than by the user.
    pub auto: bool,
}

pub struct MidiHub {
    pub profiles: Vec<ProfileEntry>,
    pub settings: MidiSettings,
    pub ports: Vec<PortStatus>,
    pub monitor: Monitor,
    /// While set, the monitor stops recording new messages (controls keep working).
    pub monitor_paused: Arc<std::sync::atomic::AtomicBool>,
    /// While active, messages are captured for MIDI learn instead of being acted on.
    pub learn: Learn,
    pub error: Option<String>,
    conns: Vec<MidiInputConnection<()>>,
    tx: mpsc::SyncSender<AppCommand>,
}

impl MidiHub {
    pub fn new(tx: mpsc::SyncSender<AppCommand>) -> Self {
        let mut hub = Self {
            profiles: midi_profile::load_profiles(),
            settings: MidiSettings::load(),
            ports: Vec::new(),
            monitor: Arc::new(Mutex::new(VecDeque::new())),
            monitor_paused: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            learn: Arc::new(Mutex::new(LearnState::Off)),
            error: None,
            conns: Vec::new(),
            tx,
        };
        hub.reconnect();
        hub
    }

    /// Re-reads the profile folder.
    pub fn reload_profiles(&mut self) {
        self.profiles = midi_profile::load_profiles();
        self.reconnect();
    }

    /// Chooses a profile for a port: None = auto-detect, Some(OFF) = ignore it.
    pub fn choose(&mut self, port_key: &str, choice: Option<&str>) {
        match choice {
            Some(c) => {
                self.settings.ports.insert(port_key.to_owned(), c.to_owned());
            }
            None => {
                self.settings.ports.remove(port_key);
            }
        }
        self.settings.save();
        self.reconnect();
    }

    /// Drops all connections and reconnects every port with its resolved profile.
    pub fn reconnect(&mut self) {
        self.conns.clear();
        self.ports.clear();
        self.error = None;

        let midi_in = match MidiInput::new("dubplate") {
            Ok(m) => m,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        for port in midi_in.ports() {
            let name = midi_in.port_name(&port).unwrap_or_else(|_| "Unknown".into());
            // The kernel's loopback port carries nothing from hardware.
            if name.starts_with("Midi Through") {
                continue;
            }
            let key = midi_profile::normalise_port_name(&name);
            let chosen = self.settings.ports.get(&key).map(String::as_str);
            let entry = self.settings.resolve(&self.profiles, &name);
            let status = PortStatus {
                key,
                profile_id: entry.map(|e| e.id.clone()),
                profile_name: entry.map(|e| e.profile.device.name.clone()),
                auto: chosen.is_none() && entry.is_some(),
            };
            log::info!("MIDI port: {name} -> {}", status.profile_name.as_deref().unwrap_or("no profile"));

            if chosen == Some(OFF) {
                self.ports.push(status);
                continue;
            }
            let mut mapper = entry.and_then(|e| match MidiMapper::new(&e.profile) {
                Ok(m) => Some(m),
                Err(err) => {
                    log::warn!("profile {} unusable: {err}", e.id);
                    None
                }
            });
            let tx = self.tx.clone();
            let monitor = Arc::clone(&self.monitor);
            let paused = Arc::clone(&self.monitor_paused);
            let label = name.clone();
            let learn = Arc::clone(&self.learn);
            let port_key = status.key.clone();
            let connected = MidiInput::new("dubplate").map_err(|e| e.to_string()).and_then(|mi| {
                mi.connect(
                    &port,
                    "dubplate-in",
                    move |_ts, msg, _| {
                        // Skip clock, active sensing and other system messages.
                        if msg.len() < 2 || !(0x80..0xF0).contains(&msg[0]) {
                            return;
                        }
                        let learning = learn.lock().is_ok_and(|mut l| {
                            let active = !matches!(*l, LearnState::Off);
                            if active {
                                midi_learn::observe(&mut l, &port_key, msg, std::time::Instant::now());
                            }
                            active
                        });
                        let function = if learning {
                            Some("learning…".to_owned())
                        } else {
                            mapper.as_ref().and_then(|m| m.describe(msg))
                        };
                        // A control moved to be learned must not also do its old job.
                        let cmd = if learning { None } else { mapper.as_mut().and_then(|m| m.process(msg)) };
                        if let Some(cmd) = cmd {
                            log::debug!("MIDI [{label}] {msg:02X?} -> {cmd:?}");
                            let _ = tx.try_send(cmd);
                        }
                        if paused.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        if let Ok(mut log) = monitor.lock() {
                            let text = msg.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ");
                            log.push_front(MonitorLine { text, function });
                            log.truncate(MONITOR_LINES);
                        }
                    },
                    (),
                )
                .map_err(|e| e.to_string())
            });
            match connected {
                Ok(c) => self.conns.push(c),
                Err(e) => log::warn!("MIDI connect {name}: {e}"),
            }
            self.ports.push(status);
        }
    }
}
