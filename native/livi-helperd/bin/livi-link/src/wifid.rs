//! The access point, driven from the host over TCP. It reads the vendor config as its base and
//! writes what the host asks for into tmpfs, so a reboot always returns to the fallback AP.
//!
//! Settings are collected per connection and only take effect on `apply`, which puts the previous
//! config back when hostapd refuses the new one. The AP is never left down after a failed change.
//!
//! Every apply starts from the base config, so it says the whole state rather than a change to it.
//! A setting the host leaves out goes back to what the fallback AP uses. `save` then makes the
//! whole state the one the dongle boots with, so name, band, channel, country and passphrase
//! belong to the dongle rather than to a session. Only the boot script's recovery puts the default
//! name back, when neither the USB link nor the AP came up.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::Duration;

pub const PORT: u16 = 5001;

/// The config the dongle boots with. It names the fallback AP, and `save` writes the radio
/// settings into it.
const BASE: &str = "/etc/hostapd.conf";
/// What the host asked for, in tmpfs so it dies with the next boot. Two of them, because a config
/// that is refused must not have overwritten the one the AP is running on.
const LIVE: [&str; 2] = ["/tmp/livi/hostapd.conf", "/tmp/livi/hostapd.alt"];
const LOG: &str = "/tmp/livi/hostapd.log";
const HOSTAPD: &str = "/usr/sbin/hostapd";
const IFACE: &str = "wlan0";
/// The dongle's only Bluetooth controller, attached to the UART at boot.
const BT: &str = "hci0";

/// How long hostapd may take to report the radio is up. A 5 GHz start scans for neighbours first.
const START_TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(250);

pub fn run() -> ExitCode {
    let listener = match TcpListener::bind(("0.0.0.0", PORT)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[wifid] bind :{PORT}: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("[wifid] listening on :{PORT}");
    let mut ap = Ap { config: BASE.to_string(), hostapd: None };
    // One client at a time: there is one radio, and a change is not interruptible.
    for stream in listener.incoming().flatten() {
        let mut stream = stream;
        serve(&mut stream, &mut ap);
    }
    ExitCode::SUCCESS
}

/// Which config the AP runs on and the hostapd this daemon started. The one from the boot script
/// is nobody's child, so it is only ever stopped by name.
pub struct Ap {
    config: String,
    hostapd: Option<Child>,
}

/// What the host has asked for on this connection, applied as one change.
#[derive(Default)]
struct Wanted {
    ssid: Option<String>,
    country: Option<String>,
    channel: Option<u32>,
    passphrase: Option<String>,
}

pub fn serve<S: std::io::Read + Write>(io: &mut S, ap: &mut Ap) {
    let mut reader = BufReader::new(io);
    let mut wanted = Wanted::default();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let answer = match command(line.trim_end_matches(['\r', '\n'])) {
            Cmd::Channels => match crate::wifi::listing() {
                Ok(text) => format!("{text}ok\n"),
                Err(e) => format!("error {e}\n"),
            },
            Cmd::Status => status(ap),
            Cmd::Set(key, value) => match remember(&mut wanted, key, value) {
                Ok(()) => "ok\n".into(),
                Err(e) => format!("error {e}\n"),
            },
            Cmd::Apply => {
                let answer = match apply(ap, &wanted) {
                    Ok(()) => "ok\n".into(),
                    Err(e) => format!("error {e}\n"),
                };
                wanted = Wanted::default();
                answer
            }
            Cmd::On => match on(ap) {
                Ok(()) => "ok\n".into(),
                Err(e) => format!("error {e}\n"),
            },
            Cmd::Off => {
                off(ap);
                "ok\n".into()
            }
            Cmd::Save => match save(ap) {
                Ok(()) => "ok\n".into(),
                Err(e) => format!("error {e}\n"),
            },
            Cmd::Bt(up) => match bluetooth(up) {
                Ok(()) => "ok\n".into(),
                Err(e) => format!("error {e}\n"),
            },
            Cmd::Empty => continue,
            Cmd::Unknown(what) => format!("error unknown command {what}\n"),
        };
        if reader.get_mut().write_all(answer.as_bytes()).is_err() {
            return;
        }
    }
}

enum Cmd<'a> {
    Channels,
    Status,
    Set(&'a str, &'a str),
    Apply,
    Save,
    On,
    Off,
    Bt(bool),
    Empty,
    Unknown(&'a str),
}

fn command(line: &str) -> Cmd<'_> {
    let line = line.trim();
    let (head, rest) = line.split_once(' ').unwrap_or((line, ""));
    match head {
        "" => Cmd::Empty,
        "channels" => Cmd::Channels,
        "status" => Cmd::Status,
        "apply" => Cmd::Apply,
        "save" => Cmd::Save,
        "on" => Cmd::On,
        "off" => Cmd::Off,
        "bt" => match rest.trim() {
            "on" => Cmd::Bt(true),
            "off" => Cmd::Bt(false),
            _ => Cmd::Unknown(line),
        },
        // The value is the rest of the line, so a name may hold spaces.
        "set" => match rest.split_once(' ') {
            Some((key, value)) => Cmd::Set(key, value),
            None => Cmd::Unknown(line),
        },
        _ => Cmd::Unknown(head),
    }
}

/// Checks a setting before it can reach the config. A value with a newline in it would write
/// hostapd directives of its own, so nothing unchecked is ever kept.
fn remember(wanted: &mut Wanted, key: &str, value: &str) -> Result<(), String> {
    if value.contains(['\n', '\r']) {
        return Err("a value holds a line break".into());
    }
    match key {
        "ssid" => {
            if value.is_empty() || value.len() > 32 {
                return Err("ssid must be 1 to 32 bytes".into());
            }
            wanted.ssid = Some(value.to_string());
        }
        "country" => {
            if value.len() != 2 || !value.bytes().all(|b| b.is_ascii_alphabetic()) {
                return Err("country must be two letters".into());
            }
            wanted.country = Some(value.to_ascii_uppercase());
        }
        "channel" => {
            let channel = value.parse::<u32>().map_err(|_| "channel must be a number")?;
            if !(1..=196).contains(&channel) {
                return Err("channel is out of range".into());
            }
            wanted.channel = Some(channel);
        }
        "passphrase" => {
            if !(8..=63).contains(&value.len()) {
                return Err("passphrase must be 8 to 63 bytes".into());
            }
            wanted.passphrase = Some(value.to_string());
        }
        other => return Err(format!("unknown setting {other}")),
    }
    Ok(())
}

/// The base config with the wanted settings replacing their lines. Everything else the vendor put
/// there stays, which is how the radio keeps working.
fn config(base: &str, wanted: &Wanted) -> String {
    let mut out = String::new();
    for line in base.lines() {
        let replaced = match setting(line) {
            Some("ssid") => wanted.ssid.is_some(),
            Some("country_code") => wanted.country.is_some(),
            Some("channel" | "hw_mode") => wanted.channel.is_some(),
            Some("wpa_passphrase") => wanted.passphrase.is_some(),
            _ => false,
        };
        if !replaced {
            out.push_str(line);
            out.push('\n');
        }
    }
    if let Some(country) = &wanted.country {
        out.push_str(&format!("country_code={country}\n"));
    }
    if let Some(channel) = wanted.channel {
        out.push_str(&format!("hw_mode={}\nchannel={channel}\n", band(channel)));
    }
    if let Some(ssid) = &wanted.ssid {
        out.push_str(&format!("ssid={ssid}\n"));
    }
    if let Some(passphrase) = &wanted.passphrase {
        out.push_str(&format!("wpa_passphrase={passphrase}\n"));
    }
    out
}

/// The name a config line sets, if it sets one.
fn setting(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    line.split_once('=').map(|(key, _)| key.trim())
}

/// The band a channel sits in, the way hostapd spells it.
fn band(channel: u32) -> &'static str {
    if channel <= 14 { "g" } else { "a" }
}

fn apply(ap: &mut Ap, wanted: &Wanted) -> Result<(), String> {
    let base = std::fs::read_to_string(BASE).map_err(|e| format!("{BASE}: {e}"))?;
    if let Some(parent) = std::path::Path::new(LIVE[0]).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let next = if ap.config == LIVE[0] { LIVE[1] } else { LIVE[0] };
    std::fs::write(next, config(&base, wanted)).map_err(|e| format!("{next}: {e}"))?;

    let previous = ap.config.clone();
    stop(ap);
    if let Err(refused) = start(ap, next) {
        // Back to what was running. Should even that not start, the fallback config always will.
        stop(ap);
        if start(ap, &previous).is_err() {
            stop(ap);
            let _ = start(ap, BASE);
            ap.config = BASE.to_string();
        }
        return Err(refused);
    }
    ap.config = next.to_string();
    Ok(())
}

/// Makes the running settings the ones the dongle boots with. Only what hostapd has just accepted
/// is written, and only when it differs, because this is flash.
fn save(ap: &Ap) -> Result<(), String> {
    if ap.config == BASE {
        return Ok(());
    }
    let live = std::fs::read_to_string(&ap.config).map_err(|e| format!("{}: {e}", ap.config))?;
    let base = std::fs::read_to_string(BASE).map_err(|e| format!("{BASE}: {e}"))?;
    let next = config(&base, &settings_of(&live));
    if next == base {
        return Ok(());
    }
    // Through a second name, so a power cut cannot leave the fallback config half written.
    let temp = format!("{BASE}.new");
    std::fs::write(&temp, next).map_err(|e| format!("{temp}: {e}"))?;
    std::fs::rename(&temp, BASE).map_err(|e| format!("{BASE}: {e}"))?;
    let _ = Command::new("sync").status();
    Ok(())
}

/// What a config sets, read back so it can be written into another one.
fn settings_of(text: &str) -> Wanted {
    let value = |key: &str| {
        text.lines()
            .rfind(|line| setting(line) == Some(key))
            .and_then(|line| line.split_once('='))
            .map(|(_, value)| value.trim().to_string())
    };
    Wanted {
        ssid: value("ssid"),
        country: value("country_code"),
        channel: value("channel").and_then(|c| c.parse().ok()),
        passphrase: value("wpa_passphrase"),
    }
}

fn on(ap: &mut Ap) -> Result<(), String> {
    if running() {
        return Ok(());
    }
    // The interface carries no address of its own, l2fwd bridges it onto the host's link.
    let _ = Command::new("ifconfig").args([IFACE, "up"]).status();
    let config = ap.config.clone();
    start(ap, &config)
}

fn off(ap: &mut Ap) {
    stop(ap);
    // Down as well, or the radio keeps the channel busy for a host that wanted it quiet.
    let _ = Command::new("ifconfig").args([IFACE, "down"]).status();
}

/// Takes the Bluetooth controller up or down. It shares the 2.4 GHz band with the AP, so a host
/// that does not use this dongle switches it off rather than leaving it talking.
fn bluetooth(up: bool) -> Result<(), String> {
    let what = if up { "up" } else { "down" };
    let status = Command::new("hciconfig")
        .args([BT, what])
        .status()
        .map_err(|e| format!("hciconfig: {e}"))?;
    if !status.success() {
        return Err(format!("{BT} would not go {what}"));
    }
    Ok(())
}

/// Whether the controller is up, from the flags hciconfig prints.
fn bt_up() -> bool {
    let Ok(out) = Command::new("hciconfig").arg(BT).output() else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout).split_whitespace().any(|word| word == "UP")
}

fn status(ap: &Ap) -> String {
    let mut out = String::new();
    out.push_str(if running() { "state on\n" } else { "state off\n" });
    out.push_str(if bt_up() { "bt on\n" } else { "bt off\n" });
    out.push_str(if ap.config == BASE { "config fallback\n" } else { "config host\n" });
    if let Ok(text) = std::fs::read_to_string(&ap.config) {
        for line in text.lines() {
            if let Some(key @ ("ssid" | "country_code" | "channel" | "hw_mode")) = setting(line) {
                let value = line.split_once('=').map(|(_, v)| v).unwrap_or("");
                out.push_str(&format!("{key} {value}\n"));
            }
        }
    }
    out.push_str("ok\n");
    out
}

/// Starts hostapd and waits until the radio reports it is up. It runs as a child of this daemon
/// rather than detached, so its death is noticed at once instead of guessed from a process list.
/// No `-B` either, because daemonising closes the log before the interesting part.
fn start(ap: &mut Ap, config: &str) -> Result<(), String> {
    let _ = std::fs::remove_file(LOG);
    let log = std::fs::File::create(LOG).map_err(|e| format!("{LOG}: {e}"))?;
    let errors = log.try_clone().map_err(|e| e.to_string())?;
    let mut child = Command::new(HOSTAPD)
        .arg(config)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(errors)
        .spawn()
        .map_err(|e| format!("hostapd: {e}"))?;

    let deadline = std::time::Instant::now() + START_TIMEOUT;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(POLL);
        let text = std::fs::read_to_string(LOG).unwrap_or_default();
        if text.contains("AP-ENABLED") {
            ap.hostapd = Some(child);
            return Ok(());
        }
        if matches!(child.try_wait(), Ok(Some(_))) {
            return Err(complaint(&text));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("hostapd did not bring the radio up".into())
}

fn stop(ap: &mut Ap) {
    let _ = Command::new("killall").arg("hostapd").status();
    if let Some(mut child) = ap.hostapd.take() {
        let _ = child.wait();
    }
    for _ in 0..20 {
        if !running() {
            return;
        }
        std::thread::sleep(POLL);
    }
}

/// What hostapd objected to. Its last lines are the tear down, and the reason sits above them, so
/// the first line that reads like a complaint is the one worth passing on.
fn complaint(log: &str) -> String {
    const MARKERS: [&str; 5] = ["not allowed", "Could not", "Unable", "Invalid", "ailed"];
    log.lines()
        .map(str::trim)
        .find(|line| MARKERS.iter().any(|m| line.contains(m)))
        .or_else(|| log.lines().map(str::trim).rev().find(|line| !line.is_empty()))
        .unwrap_or("hostapd failed")
        .to_string()
}

fn running() -> bool {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm"))
            && comm.trim() == "hostapd"
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(pairs: &[(&str, &str)]) -> Wanted {
        let mut wanted = Wanted::default();
        for (key, value) in pairs {
            remember(&mut wanted, key, value).unwrap();
        }
        wanted
    }

    const BASE_CONF: &str = "interface=wlan0\n#channel=11\ncountry_code=US\nhw_mode=a\nchannel=36\n\
                             ssid=old name\nwpa_passphrase=12345678\nwmm_enabled=1\n";

    #[test]
    fn a_setting_replaces_its_line_and_leaves_the_rest_alone() {
        let out = config(BASE_CONF, &wanted(&[("ssid", "LIVI Link"), ("channel", "6")]));
        assert!(out.contains("interface=wlan0\n"));
        assert!(out.contains("wmm_enabled=1\n"));
        assert!(out.contains("ssid=LIVI Link\n"));
        assert!(!out.contains("ssid=old name"));
        // A 2.4 GHz channel has to take the band with it.
        assert!(out.contains("hw_mode=g\n"));
        assert!(out.contains("channel=6\n"));
        assert!(!out.contains("hw_mode=a\n"));
        // The commented out line is not a setting and stays.
        assert!(out.contains("#channel=11\n"));
        // Untouched settings keep their value.
        assert!(out.contains("country_code=US\n"));
        assert!(out.contains("wpa_passphrase=12345678\n"));
    }

    #[test]
    fn nothing_wanted_leaves_the_config_as_it_was() {
        assert_eq!(config(BASE_CONF, &Wanted::default()), BASE_CONF);
    }

    #[test]
    fn a_five_gigahertz_channel_picks_the_other_band() {
        let out = config(BASE_CONF, &wanted(&[("channel", "149")]));
        assert!(out.contains("hw_mode=a\n"));
        assert!(out.contains("channel=149\n"));
    }

    #[test]
    fn a_value_that_would_write_its_own_directives_is_refused() {
        let mut w = Wanted::default();
        assert!(remember(&mut w, "ssid", "evil\nchannel=1").is_err());
        assert!(remember(&mut w, "passphrase", "short").is_err());
        assert!(remember(&mut w, "country", "germany").is_err());
        assert!(remember(&mut w, "channel", "0").is_err());
        assert!(remember(&mut w, "channel", "many").is_err());
        assert!(remember(&mut w, "colour", "red").is_err());
        assert!(w.ssid.is_none());
    }

    #[test]
    fn a_country_is_kept_upper_case() {
        let mut w = Wanted::default();
        remember(&mut w, "country", "de").unwrap();
        assert_eq!(w.country.as_deref(), Some("DE"));
    }

    #[test]
    fn a_command_keeps_the_spaces_in_its_value() {
        match command("set ssid My Car (2)") {
            Cmd::Set(key, value) => {
                assert_eq!(key, "ssid");
                assert_eq!(value, "My Car (2)");
            }
            _ => panic!("not a set"),
        }
        assert!(matches!(command("apply"), Cmd::Apply));
        assert!(matches!(command("save"), Cmd::Save));
        assert!(matches!(command("off"), Cmd::Off));
        assert!(matches!(command("bt on"), Cmd::Bt(true)));
        assert!(matches!(command("bt off"), Cmd::Bt(false)));
        assert!(matches!(command("bt sideways"), Cmd::Unknown(_)));
        assert!(matches!(command(""), Cmd::Empty));
        assert!(matches!(command("fly"), Cmd::Unknown("fly")));
        assert!(matches!(command("set ssid"), Cmd::Unknown(_)));
    }

    #[test]
    fn saving_carries_the_whole_state_over() {
        let live = "interface=wlan0\ncountry_code=DE\nhw_mode=g\nchannel=6\n\
                    ssid=Volvo\nwpa_passphrase=geheim12\n";
        let next = config(BASE_CONF, &settings_of(live));
        assert!(next.contains("country_code=DE\n"));
        assert!(next.contains("hw_mode=g\n"));
        assert!(next.contains("channel=6\n"));
        assert!(next.contains("wpa_passphrase=geheim12\n"));
        assert!(next.contains("ssid=Volvo\n"));
        assert!(!next.contains("ssid=old name"));
    }

    #[test]
    fn the_complaint_is_the_reason_and_not_the_tear_down() {
        // Shortened from a real refusal on the dongle.
        let log = "Configuration file: /tmp/livi/hostapd.conf\n\
                   wlan0: interface state UNINITIALIZED->COUNTRY_UPDATE\n\
                   Channel 13 (primary) not allowed for AP mode\n\
                   Could not select hw_mode and channel. (-3)\n\
                   wlan0: AP-DISABLED \n\
                   nl80211: deinit ifname=wlan0 disabled_11b_rates=0\n";
        assert_eq!(complaint(log), "Channel 13 (primary) not allowed for AP mode");
        // Nothing that reads like a reason leaves the last word.
        assert_eq!(complaint("odd\nstop\n"), "stop");
        assert_eq!(complaint(""), "hostapd failed");
    }
}
