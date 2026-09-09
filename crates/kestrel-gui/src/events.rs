//! Engine event forwarding and system-tray setup.

use std::sync::{Arc, atomic::AtomicU32};

use kestrel_core::protocol::EngineEvent;
#[cfg(feature = "tray")]
use kestrel_core::protocol::{Command, CommandPayload, FrontendKind};
use kestrel_gui::AppWindow;
use slint::Model as _;

use crate::util::show_toast;

/// Wrapper that is `Send`-safe for the engine→GUI event-forwarding thread.
pub(crate) struct ForwardedEvent(pub EngineEvent);

/// Shared per-account id cache (index-aligned with the sidebar account
/// list) used to place re-auth badges on the right row.
pub(crate) type AccountIds = Arc<std::sync::Mutex<Vec<kestrel_core::ids::AccountId>>>;

impl ForwardedEvent {
    /// Dispatch an engine event onto the Slint UI thread.
    pub fn apply(
        self,
        app: &AppWindow,
        _vp: &crate::state::SharedViewportState,
        unread: &Arc<AtomicU32>,
        account_ids: &AccountIds,
    ) {
        match self.0 {
            EngineEvent::EngineStarted { version, .. } => {
                app.set_status_text(format!("Kestrel v{version} ready").into());
            }
            EngineEvent::AccountConnection { account, state } => {
                app.set_connection_state(format!("{state:?}").into());
                update_reauth_badges(app, account, state, account_ids);
            }
            EngineEvent::MailArrived { summary, .. } => {
                app.set_status_text(format!("{} new", summary.new).into());
                app.set_total_messages(
                    app.get_total_messages() + i32::try_from(summary.new).unwrap_or(0),
                );
                unread.store(
                    u32::try_from(summary.unread).unwrap_or(u32::MAX),
                    std::sync::atomic::Ordering::Relaxed,
                );
                if summary.new > 0 {
                    let notifications_enabled = app.get_settings_notifications_enabled();
                    if notifications_enabled
                        && let Err(e) = notify_rust::Notification::new()
                            .summary("Kestrel")
                            .body(&format!("{} new message(s)", summary.new))
                            .appname("Kestrel")
                            .show()
                    {
                        tracing::warn!("notification: {e}");
                    }
                }
            }
            EngineEvent::FolderTreeChanged { .. } => {
                app.set_status_text("folders synced".into());
            }
            EngineEvent::ServiceDegraded { service, error, .. } => {
                app.set_status_text(format!("degraded: {service}: {error}").into());
            }
            EngineEvent::OAuth2FlowCompleted {
                result: Err(error), ..
            } => {
                // Success is surfaced by the setup wizard's completion
                // watcher (which links the account); failures here catch
                // flows started outside the wizard.
                show_toast(app, &format!("sign-in failed: {error}"), "error");
            }
            EngineEvent::RemoteContentBlocked { count, .. } => {
                app.set_status_text(format!("{count} remote items blocked").into());
            }
            EngineEvent::EventStreamLagged { missed } => {
                app.set_status_text(format!("missed {missed} events").into());
            }
            EngineEvent::OutboxEnqueued { .. } => app.set_status_text("queued".into()),
            EngineEvent::MailSent { .. } => show_toast(app, "Message sent", "success"),
            EngineEvent::MailFailed { error, .. } => {
                show_toast(app, &format!("Send failed: {error}"), "error");
            }
            _ => {}
        }
    }
}

/// Keeps the sidebar `re-auth` badges in sync with the engine's account
/// connection states (#27): a `NeedsReauth` account shows the badge; any
/// other transition clears it. The affected row is resolved through the
/// index-aligned account id cache; unknown accounts are ignored.
fn update_reauth_badges(
    app: &AppWindow,
    account: kestrel_core::ids::AccountId,
    state: kestrel_core::protocol::ConnectionState,
    account_ids: &AccountIds,
) {
    let names = app.get_account_names();
    let count = names.row_count();
    let row = account_ids
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .position(|id| *id == account)
        .filter(|r| *r < count);
    let Some(row) = row else { return };
    let needs = state == kestrel_core::protocol::ConnectionState::NeedsReauth;
    let model = app.get_account_needs_reauth();
    let mut flags: Vec<bool> = (0..count)
        .map(|i| model.row_data(i).unwrap_or(false))
        .collect();
    if flags.len() < count {
        flags.resize(count, false);
    }
    if flags.get(row) == Some(&needs) {
        return;
    }
    if let Some(slot) = flags.get_mut(row) {
        *slot = needs;
    }
    app.set_account_needs_reauth(flags.as_slice().into());
}

/// Set up the system tray icon with a context menu.
///
/// Returns nothing; the `TrayIcon` is leaked for the process lifetime.
/// On platforms where the underlying toolkit is unavailable, creation
/// fails gracefully and the function returns early.
#[cfg(feature = "tray")]
#[allow(dead_code, unused_variables)]
pub(crate) fn setup_tray(
    app: &AppWindow,
    handle: &kestrel_engine::EngineHandle,
    unread_count: &Arc<AtomicU32>,
) {
    use tray_icon::{
        Icon, TrayIconBuilder,
        menu::{Menu, MenuItem},
    };

    let icon = match Icon::from_rgba(
        vec![
            0x1e, 0x1e, 0x2e, 0xff, 0x31, 0x32, 0x44, 0xff, 0x45, 0x47, 0x5a, 0xff, 0xc0, 0xc0,
            0xc0, 0xff,
        ],
        2,
        2,
    ) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("tray-icon: failed to create icon: {e}");
            return;
        }
    };

    let menu = Menu::new();
    let compose_item = MenuItem::new("Compose", true, None);
    let sync_item = MenuItem::new("Sync Now", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    let _ = menu.append(&compose_item);
    let _ = menu.append(&sync_item);
    let _ = menu.append(&quit_item);

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Kestrel — 0 unread")
        .with_icon(icon)
        .with_menu_on_left_click(false)
        .build();

    let tray = match tray {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("tray-icon: build failed (platform may not support tray): {e}");
            return;
        }
    };

    #[cfg(not(target_os = "linux"))]
    {
        use tray_icon::{TrayIconEvent, menu::MenuEvent};

        let gui_weak_ev = app.as_weak();
        TrayIconEvent::set_event_handler(Some(move |event| {
            if let TrayIconEvent::DoubleClick { .. } = event {
                let w = gui_weak_ev.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = w.upgrade() {
                        let visible = app.window().is_visible().unwrap_or(true);
                        let _ = app.window().set_visible(!visible);
                    }
                })
                .ok();
            }
        }));

        let gui_weak_menu = app.as_weak();
        let h_menu = handle.clone();
        let unread = Arc::clone(unread_count);
        MenuEvent::set_event_handler(Some(move |event| {
            if *event.id() == *compose_item.id() {
                let w = gui_weak_menu.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = w.upgrade() {
                        app.set_show_compose(true);
                        app.set_compose_error(slint::SharedString::default());
                    }
                })
                .ok();
            } else if *event.id() == *sync_item.id() {
                let h = h_menu.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Handle::current();
                    rt.block_on(async move {
                        let (tx, _rx) = tokio::sync::oneshot::channel();
                        let _ = h
                            .commands
                            .send(Command {
                                id: kestrel_core::ids::RequestId::from_uuid(uuid::Uuid::now_v7()),
                                origin: FrontendKind::Gui,
                                payload: CommandPayload::ResyncState { reply: tx },
                            })
                            .await;
                    });
                });
            } else if *event.id() == *quit_item.id() {
                slint::invoke_from_event_loop(|| {
                    slint::quit_event_loop().ok();
                })
                .ok();
            }
        }));
    }

    #[cfg(target_os = "linux")]
    {
        tracing::info!("tray-icon: menu events not forwarded on Linux (no GTK event loop)");
    }

    Box::leak(Box::new(tray));
}
