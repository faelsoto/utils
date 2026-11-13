use std::path::PathBuf;

use futures_util::StreamExt;
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{MenuItem, ToolTip, TrayMethods};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

const APP_ID: &str = "lantern";
const INHIBIT_SUSPEND: u32 = 4;
const INHIBIT_IDLE: u32 = 8;

#[zbus::proxy(
    interface = "org.gnome.SessionManager",
    default_service = "org.gnome.SessionManager",
    default_path = "/org/gnome/SessionManager"
)]
trait SessionManager {
    fn inhibit(&self, app_id: &str, toplevel_xid: u32, reason: &str, flags: u32) -> zbus::Result<u32>;
    fn uninhibit(&self, inhibit_cookie: u32) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower"
)]
trait UPower {
    #[zbus(property)]
    fn on_battery(&self) -> zbus::Result<bool>;
}

enum Command {
    Toggle,
    Quit,
}

struct LanternTray {
    enabled: bool,
    on_battery: bool,
    tx: mpsc::UnboundedSender<Command>,
}

impl LanternTray {
    fn status(&self) -> &'static str {
        match (self.enabled, self.on_battery) {
            (false, _) => "Off: normal sleep",
            (true, false) => "Keeping awake (plugged in)",
            (true, true) => "Paused: on battery",
        }
    }
}

impl ksni::Tray for LanternTray {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        "Lantern".into()
    }

    fn icon_name(&self) -> String {
        match (self.enabled, self.on_battery) {
            (false, _) => "lantern-off",
            (true, false) => "lantern-on",
            (true, true) => "lantern-paused",
        }
        .into()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Lantern".into(),
            description: self.status().into(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(Command::Toggle);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            CheckmarkItem {
                label: "Keep awake".into(),
                checked: self.enabled,
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send(Command::Toggle);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.status().into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send(Command::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn state_file() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    base.join(APP_ID).join("enabled")
}

fn load_enabled() -> bool {
    state_file().exists()
}

fn save_enabled(enabled: bool) {
    let path = state_file();
    let result = if enabled {
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, b""))
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    };
    if let Err(e) = result {
        eprintln!("{APP_ID}: could not save state to {}: {e}", path.display());
    }
}

async fn sync_inhibitor(manager: &SessionManagerProxy<'_>, cookie: &mut Option<u32>, wanted: bool) {
    match (wanted, *cookie) {
        (true, None) => match manager
            .inhibit(APP_ID, 0, "Lantern is on and the laptop is plugged in", INHIBIT_SUSPEND | INHIBIT_IDLE)
            .await
        {
            Ok(c) => *cookie = Some(c),
            Err(e) => eprintln!("{APP_ID}: inhibit failed: {e}"),
        },
        (false, Some(c)) => {
            if let Err(e) = manager.uninhibit(c).await {
                eprintln!("{APP_ID}: uninhibit failed: {e}");
            }
            *cookie = None;
        }
        _ => {}
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session = zbus::Connection::session().await?;
    let system = zbus::Connection::system().await?;
    let manager = SessionManagerProxy::new(&session).await?;
    let upower = UPowerProxy::new(&system).await?;

    let mut enabled = load_enabled();
    let mut on_battery = upower.on_battery().await.unwrap_or(false);
    let mut battery_changes = upower.receive_on_battery_changed().await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = LanternTray { enabled, on_battery, tx }
        .assume_sni_available(true)
        .spawn()
        .await?;

    let mut cookie = None;
    sync_inhibitor(&manager, &mut cookie, enabled && !on_battery).await;

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    loop {
        tokio::select! {
            Some(command) = rx.recv() => match command {
                Command::Toggle => {
                    enabled = !enabled;
                    save_enabled(enabled);
                }
                Command::Quit => break,
            },
            Some(change) = battery_changes.next() => {
                if let Ok(value) = change.get().await {
                    on_battery = value;
                }
            },
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
        }

        sync_inhibitor(&manager, &mut cookie, enabled && !on_battery).await;
        handle
            .update(|tray| {
                tray.enabled = enabled;
                tray.on_battery = on_battery;
            })
            .await;
    }

    sync_inhibitor(&manager, &mut cookie, false).await;
    handle.shutdown().await;
    Ok(())
}
