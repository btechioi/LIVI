// Provisions a CarlinKit dongle as LIVI Link without a UI; the app drives the same crate.
// Host: $LIVI_LINK_HOST (default 10.10.10.1). The stack it installs is baked into this binary.

mod bootstrap;

use std::path::PathBuf;
use std::time::Duration;

use livi_link_provision::payload::parts;
use livi_link_provision::shell::{self, DEFAULT_HOST, Shell};
use livi_link_provision::{Plan, Report, Status, apply, mtd, plan, verify};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let host = std::env::var("LIVI_LINK_HOST").unwrap_or_else(|_| DEFAULT_HOST.to_string());
    let Some(command) = args.first().map(String::as_str) else {
        return menu(&Shell::new(&host));
    };

    let sh = Shell::new(&host);

    let result = match command {
        "plan" => plan(&sh).map(|p| {
            print_plan(&p);
            true
        }),
        "apply" => {
            let reboot = args.iter().any(|a| a == "--reboot");
            apply(&sh, reboot, &|line| println!("== {line}")).map(|r| {
                print_report(&r);
                r.ok()
            })
        }
        "verify" => verify(&sh).map(|r| {
            print_report(&r);
            r.ok()
        }),
        "backup" => {
            let dir = args.get(1).map(PathBuf::from).unwrap_or_else(backup_dir);
            mtd::backup(&sh, &dir, &|line| println!("== {line}")).map(|dir| {
                println!("== backup in {}", livi_link_provision::tilde(&dir));
                true
            })
        }
        "push" => match &args[1..] {
            [local, remote] => std::fs::read(local)
                .map_err(|e| format!("{local}: {e}"))
                .and_then(|data| {
                    let md5 = livi_link_provision::payload::md5_hex(&data);
                    sh.push(&data, remote, shell::PUSH_PORT, &md5).map(|()| {
                        println!("pushed {} bytes to {remote} ({md5})", data.len());
                        true
                    })
                }),
            _ => Err("usage: push <local> <remote>".to_string()),
        },
        "bootstrap" => bootstrap::boot_hook().map(|()| {
            println!("bootstrap written, replug the dongle to start its shell");
            true
        }),
        "sh" => sh.run(&args[1..].join(" "), Duration::from_secs(120)).map(|out| {
            println!("{out}");
            true
        }),
        _ => {
            eprintln!("{}", usage());
            return std::process::ExitCode::from(2);
        }
    };

    match result {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Started without arguments the tool asks rather than expecting commands. The subcommands stay
/// for scripting.
fn menu(sh: &Shell) -> std::process::ExitCode {
    loop {
        let fresh = is_stock(sh).unwrap_or(true);
        println!("\nLIVI Link  ({})", describe(sh));
        println!("  1  {} LIVI Link", if fresh { "install" } else { "reinstall" });
        println!("  q  quit");
        print!("> ");
        let _ = std::io::Write::flush(&mut std::io::stdout());

        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return std::process::ExitCode::SUCCESS;
        }
        let outcome: Result<(), String> = match line.trim() {
            "1" => match install(sh) {
                Ok(()) => return std::process::ExitCode::SUCCESS,
                Err(e) => Err(e),
            },
            "q" | "quit" | "" => return std::process::ExitCode::SUCCESS,
            other => Err(format!("no such choice: {other}")),
        };
        if let Err(e) = outcome {
            eprintln!("error: {e}");
        }
    }
}

/// What the dongle is right now, so the menu is not a shot in the dark.
fn describe(sh: &Shell) -> String {
    if !sh.port_open(shell::TELNET_PORT) {
        return match bootstrap::on_bus() {
            bootstrap::OnBus::Stock => "a dongle that has not been set up yet, pick 1 to do it".into(),
            bootstrap::OnBus::Link => "a LIVI Link dongle that is not answering yet, give it a moment".into(),
            bootstrap::OnBus::Nothing => {
                format!("no dongle found, neither at {} nor on USB", sh.host())
            }
        };
    }
    // Neither file ends in a newline, so they are joined here rather than by cat.
    let box_info = sh
        .sh("echo \"$(cat /etc/box_product_type 2>/dev/null) $(cat /etc/software_version 2>/dev/null)\"")
        .unwrap_or_default();
    let mut fields = box_info.split_whitespace();
    let model = fields.next().unwrap_or("?").to_string();
    let firmware = fields.next().unwrap_or("?").to_string();
    format!("{model} {firmware}, {}", state(sh))
}

/// Nothing installed, the same version as this tool, or a different one.
fn state(sh: &Shell) -> String {
    let installed = sh
        .sh(&format!("cat {} 2>/dev/null", livi_link_provision::payload::VERSION_FILE))
        .unwrap_or_default()
        .trim()
        .to_string();
    if installed.is_empty() {
        return match is_stock(sh) {
            Ok(true) => "not installed yet".into(),
            Ok(false) => "LIVI Link installed, version unknown".into(),
            Err(_) => "state unknown".into(),
        };
    }
    let ours = livi_link_provision::payload::current_version().trim().to_string();
    if installed == ours {
        format!("LIVI Link {installed}, up to date")
    } else {
        format!("LIVI Link {installed}, this tool has {ours}")
    }
}

/// The whole job in one go: a shell if the dongle has none, then the backup, then the install.
/// It refuses to strip a dongle whose original is not saved, because that is the way back.
fn install(sh: &Shell) -> Result<(), String> {
    // A stock dongle offers the host no network, so the bootstrap rides into the next boot.
    if !sh.port_open(shell::TELNET_PORT) {
        println!("== this dongle has no way in yet, so it needs one unplug and plug back in");
        println!("== writing the bootstrap over USB");
        bootstrap::boot_hook()?;
        ask("unplug the dongle, plug it back in, then press enter")?;
        println!("== waiting, it takes about half a minute after the dongle has booted");
        wait_for_shell(sh)?;
    }
    // Whoever put it there, it goes before the backup, so the image is the dongle's own again.
    if sh.sh(&format!("[ -e {} ] && echo yes || echo no", bootstrap::BOOT_HOOK))?.trim() == "yes" {
        sh.push(
            bootstrap::carrier_body().as_bytes(),
            bootstrap::CARRIER,
            shell::PUSH_PORT,
            &livi_link_provision::payload::md5_hex(bootstrap::carrier_body().as_bytes()),
        )?;
        sh.sh(&format!("chmod 755 {}; rm -f {}; sync", bootstrap::CARRIER, bootstrap::BOOT_HOOK))?;
        println!("== bootstrap removed again");
    }

    // From here on the dongle is being written to and must not be unplugged, so it says so.
    let blinking = blink(sh);

    // Only while the dongle is untouched. A backup of an already installed one is worthless and
    // would sit next to the real one, inviting a restore of the wrong image.
    if is_stock(sh)? {
        let dir = mtd::backup(sh, &backup_dir(), &report)?;
        println!("== backup in {}", livi_link_provision::tilde(&dir));
    } else {
        println!("== already installed, keeping the backup from the first time");
    }

    // Installed over whichever way in we had, but afterwards the dongle is LIVI Link and answers
    // over USB, so the restart and the check happen there.
    apply(sh, false, &report)?;
    drop(blinking);
    report("rebooting");
    sh.sh("sync; (sleep 1; reboot) >/dev/null 2>&1 &")?;
    std::thread::sleep(Duration::from_secs(5));
    let link = Shell::new(DEFAULT_HOST);
    wait_for_shell(&link)?;
    let outcome = verify(&link)?;
    print_report(&outcome);
    if outcome.ok() {
        let now = installed_version(&link).unwrap_or_else(|| "?".into());
        println!("\n== done, the dongle runs LIVI Link {} and is safe to unplug", parts(&now).0);
        Ok(())
    } else {
        Err("the dongle did not come back as expected".into())
    }
}

/// Alternates the two LEDs, the signal the vendor's updater gives while it writes. It runs
/// detached on the dongle and is stopped again however the install ends.
struct Blink<'a>(&'a Shell);

/// The loop itself. One line, because the shell on the dongle reads commands by line.
const BLINK_LOOP: &str = "echo $$ > /tmp/livi-blink.pid; \
                for g in 2 9; do \
                  [ -e /sys/class/gpio/gpio$g ] || echo $g > /sys/class/gpio/export; \
                  echo out > /sys/class/gpio/gpio$g/direction; \
                done; \
                while :; do \
                  echo 0 > /sys/class/gpio/gpio2/value; echo 1 > /sys/class/gpio/gpio9/value; sleep 0.25; \
                  echo 1 > /sys/class/gpio/gpio2/value; echo 0 > /sys/class/gpio/gpio9/value; sleep 0.25; \
                done";

fn blink(sh: &Shell) -> Blink<'_> {
    let _ = sh.sh(&format!("setsid sh -c '{BLINK_LOOP}' </dev/null >/dev/null 2>&1 &"));
    Blink(sh)
}

impl Drop for Blink<'_> {
    fn drop(&mut self) {
        // By its pid, because a pattern would match the shell that does the killing. Then back to
        // the steady red of normal operation.
        let _ = self.0.sh(
            "kill $(cat /tmp/livi-blink.pid 2>/dev/null) 2>/dev/null; rm -f /tmp/livi-blink.pid; \
             echo 1 > /sys/class/gpio/gpio9/value 2>/dev/null; \
             echo 0 > /sys/class/gpio/gpio2/value 2>/dev/null",
        );
    }
}

/// Whether the dongle still boots the vendor's script rather than ours.
fn is_stock(sh: &Shell) -> Result<bool, String> {
    let out = sh.sh(&format!(
        "grep -q '{}' {} 2>/dev/null && echo ours || echo stock",
        livi_link_provision::payload::BRINGUP_MARKER,
        livi_link_provision::payload::BRINGUP_REMOTE
    ))?;
    Ok(out.trim() == "stock")
}

/// Waits for the shell the bootstrap brings up.
fn wait_for_shell(sh: &Shell) -> Result<(), String> {
    for _ in 0..60 {
        if sh.port_open(shell::TELNET_PORT) {
            println!("== shell is up");
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Err("no shell after two minutes, see LIVI-LINK.md".into())
}

fn ask(what: &str) -> Result<String, String> {
    print!("{what}: ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line.trim().to_string())
}

fn report(line: &str) {
    println!("== {line}");
}

fn usage() -> &'static str {
    "usage: livi-link-provision plan | apply [--reboot] | verify | backup [dir] | push <local> <remote> | sh 'CMD'"
}

/// Where backups go when no directory is given: beside the app's own data.
fn backup_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let base = if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/LIVI")
    } else {
        std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(home).join(".local/share"))
            .join("LIVI")
    };
    base.join("dongle-backup")
}

fn print_plan(p: &Plan) {
    println!("== dongle: {}", p.identity);
    println!("== rootfs free: {}", kib(p.free_k));
    let raw_k: u64 = p.delete.iter().map(|(_, size)| size / 1024).sum();
    println!("== ballast to delete ({} files, {raw_k}K raw):", p.delete.len());
    for (path, size) in &p.delete {
        println!("   {:>8}K  {path}", size / 1024);
    }
    if !p.keep_libs.is_empty() {
        println!("== ballast libs kept — referenced by a kept ELF:");
        for lib in &p.keep_libs {
            println!("   keep      {lib}");
        }
    }
    println!("== files:");
    for f in &p.files {
        println!("   {:<7} {:>7}K  {}", f.status.label(), f.bytes / 1024, f.remote);
    }
    let todo = p.files.iter().filter(|f| f.status != Status::Current).count();
    println!("== to push: {todo} files, {}K", p.push_k());
}

fn print_report(r: &Report) {
    for check in &r.checks {
        println!("   {}  {}", if check.ok { "ok " } else { "BAD" }, check.what);
    }
    println!(
        "   pair records: {}",
        if r.pair_records.is_empty() { "(none)" } else { &r.pair_records }
    );
    println!("   rootfs free: {}", kib(r.free_k));
    println!("{}", if r.ok() { "== VERIFIED" } else { "== PROBLEMS — see BAD lines above" });
}

fn kib(v: Option<u64>) -> String {
    v.map(|k| format!("{k}K")).unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_blink_loop_stays_on_one_line() {
        assert!(!super::BLINK_LOOP.contains('\n'));
        assert!(!super::BLINK_LOOP.contains('\''));
        assert!(super::BLINK_LOOP.contains("gpio2/value"));
    }
}
