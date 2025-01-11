#![windows_subsystem = "windows"]

use ancs::attributes::action::ActionID;
use ancs::attributes::app::AppAttributeID;
use ancs::attributes::category::CategoryID;
use ancs::attributes::command::CommandID;
use ancs::attributes::event::{EventFlag, EventID};
use ancs::attributes::notification::NotificationAttributeID;
use ancs::attributes::NotificationAttribute;
use ancs::characteristics::control_point::*;
use ancs::characteristics::data_source::*;
use ancs::characteristics::notification_source::Notification as GattNotification;
use btleplug::api::{
    Central, CentralEvent, Characteristic, Manager as _, Peripheral as _, WriteType,
};
use btleplug::platform::{Manager, Peripheral};
use futures::stream::StreamExt;
#[cfg(not(windows))]
use notify_rust::NotificationHandle;
use notify_rust::{Hint, Notification, Timeout, Urgency};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::time::{Duration, Instant};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tokio::sync::{oneshot, watch};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::TrayIconBuilder;

// Windows does not have notification handles
#[cfg(windows)]
type NotificationHandle = Notification;

struct AppGlobals {
    peripheral: Peripheral,
    received_notifs: HashMap<u32, GattNotification>,
    pending_notifs: HashMap<u32, Notification>,
    sent_notifs: HashMap<u32, NotificationHandle>,
    app_names: HashMap<String, String>,
    needs_appname: HashMap<String, HashSet<u32>>,
    ns_char: Characteristic,
    cp_char: Option<Characteristic>,
    ds_char: Option<Characteristic>,
}

async fn write_details_request(
    app: &mut AppGlobals,
    notification_uid: u32,
    attribute_ids: Vec<(NotificationAttributeID, Option<u16>)>,
) -> Result<(), btleplug::Error> {
    let req = GetNotificationAttributesRequest {
        command_id: CommandID::GetNotificationAttributes,
        notification_uid,
        attribute_ids,
    };
    let out: Vec<u8> = req.into();
    app.peripheral
        .write(app.cp_char.as_ref().unwrap(), &out, WriteType::WithResponse)
        .await?;
    Ok(())
}

async fn write_appinfo_request(
    app: &mut AppGlobals,
    app_identifier: &str,
    attribute_ids: Vec<AppAttributeID>,
) -> Result<(), btleplug::Error> {
    let req = GetAppAttributesRequest {
        command_id: CommandID::GetAppAttributes,
        app_identifier: app_identifier.to_owned(),
        attribute_ids,
    };
    let out: Vec<u8> = req.into();
    app.peripheral
        .write(app.cp_char.as_ref().unwrap(), &out, WriteType::WithResponse)
        .await?;
    Ok(())
}

// only Windows can set the app ID per notification
#[cfg(windows)]
fn set_app_id(send: &mut Notification, appid: &str) {
    send.app_id(appid);
}
#[cfg(not(windows))]
fn set_app_id(_send: &mut Notification, _appid: &str) {}

fn action_id_for_notif(recv: Option<&GattNotification>, action: ActionID) -> &'static str {
    if recv.is_some() && recv.unwrap().category_id == CategoryID::IncomingCall {
        match action {
            ActionID::Positive => "call-start",
            ActionID::Negative => "call-stop",
        }
    } else {
        match action {
            ActionID::Positive => "dialog-ok",
            ActionID::Negative => "dialog-close",
        }
    }
}

async fn update_notif_with_notif_attributes(
    app: &mut AppGlobals,
    notification_uid: u32,
    attribute_list: &Vec<NotificationAttribute>,
) -> Result<(), btleplug::Error> {
    let send = if app.pending_notifs.contains_key(&notification_uid) {
        app.pending_notifs.get_mut(&notification_uid).unwrap()
    } else if app.sent_notifs.contains_key(&notification_uid) {
        app.sent_notifs.get_mut(&notification_uid).unwrap()
    } else {
        return Ok(());
    };
    for attr in attribute_list {
        match attr.id {
            NotificationAttributeID::AppIdentifier => {
                if let Some(appid) = &attr.value {
                    set_app_id(send, appid);
                    if cfg!(all(unix, not(target_os = "macos"))) {
                        // only XDG will use the application name
                        if let Some(appname) = app.app_names.get(appid) {
                            send.appname(appname);
                        } else {
                            if !app.needs_appname.contains_key(appid) {
                                app.needs_appname.insert(appid.clone(), HashSet::new());
                            }
                            app.needs_appname
                                .get_mut(appid)
                                .unwrap()
                                .insert(notification_uid);
                        }
                    }
                }
            }
            NotificationAttributeID::Title => {
                if let Some(title) = &attr.value {
                    send.summary(title);
                }
            }
            NotificationAttributeID::Subtitle => {
                if let Some(subtitle) = &attr.value {
                    send.subtitle(subtitle);
                }
            }
            NotificationAttributeID::Message => {
                if let Some(message) = &attr.value {
                    send.body(message);
                }
            }
            NotificationAttributeID::MessageSize => {}
            NotificationAttributeID::Date => {}
            NotificationAttributeID::PositiveActionLabel => {
                if let Some(label) = &attr.value {
                    send.action(
                        action_id_for_notif(
                            app.received_notifs.get(&notification_uid),
                            ActionID::Positive,
                        ),
                        label,
                    );
                }
            }
            NotificationAttributeID::NegativeActionLabel => {
                if let Some(label) = &attr.value {
                    send.action(
                        action_id_for_notif(
                            app.received_notifs.get(&notification_uid),
                            ActionID::Negative,
                        ),
                        label,
                    );
                }
            }
        }
    }
    if cfg!(all(unix, not(target_os = "macos"))) {
        // only XDG will use the application name
        for attr in attribute_list {
            if attr.id == NotificationAttributeID::AppIdentifier {
                if let Some(appid) = &attr.value {
                    if !app.app_names.contains_key(appid) {
                        write_appinfo_request(app, appid, vec![AppAttributeID::DisplayName])
                            .await?;
                    }
                }
            }
        }
    }
    Ok(())
}

// only XDG can update notifications
#[cfg(all(unix, not(target_os = "macos")))]
fn update_handle(handle: &mut NotificationHandle) {
    handle.update();
}
#[cfg(not(all(unix, not(target_os = "macos"))))]
fn update_handle(_handle: &mut NotificationHandle) {}

// only XDG can handle actions
#[cfg(all(unix, not(target_os = "macos")))]
fn add_action_handlers(app: &AppGlobals, notif_id: u32, notification_uid: u32) {
    let received_notif = app.received_notifs.get(&notification_uid);
    let pos_action_id = action_id_for_notif(received_notif, ActionID::Positive);
    let neg_action_id = action_id_for_notif(received_notif, ActionID::Negative);
    let peripheral = app.peripheral.clone();
    let cp_char = app.cp_char.clone();

    if received_notif
        .unwrap()
        .event_flags
        .contains(EventFlag::PositiveAction)
        || received_notif
            .unwrap()
            .event_flags
            .contains(EventFlag::NegativeAction)
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        std::thread::spawn(move || {
            notify_rust::handle_action(notif_id, |result| {
                if let notify_rust::ActionResponse::Custom(action) = result {
                    if action == &pos_action_id || action == &neg_action_id {
                        let req = PerformNotificationActionRequest {
                            command_id: CommandID::PerformNotificationAction,
                            notification_uid,
                            action_id: if action == &pos_action_id {
                                ActionID::Positive
                            } else {
                                ActionID::Negative
                            },
                        };
                        let out: Vec<u8> = req.into();
                        rt.block_on(peripheral.write(
                            cp_char.as_ref().unwrap(),
                            &out,
                            WriteType::WithResponse,
                        ))
                        .unwrap();
                    }
                }
            });
        });
    }
}
#[cfg(not(all(unix, not(target_os = "macos"))))]
fn add_action_handlers(_app: &AppGlobals, _notif_id: u32, _notification_uid: u32) {}

// only XDG can get a handle's ID
#[cfg(all(unix, not(target_os = "macos")))]
fn get_handle_id(handle: &NotificationHandle) -> u32 {
    handle.id()
}
#[cfg(not(all(unix, not(target_os = "macos"))))]
fn get_handle_id(_handle: &NotificationHandle) -> u32 {
    0
}

// Windows does not have notification handles
#[cfg(not(windows))]
fn show_notification(send: &Notification) -> notify_rust::error::Result<NotificationHandle> {
    send.show()
}
#[cfg(windows)]
fn show_notification(send: &Notification) -> notify_rust::error::Result<NotificationHandle> {
    send.show()?;
    // ok to error, will prevent being added to sent list
    Err(notify_rust::error::ErrorKind::ImplementationMissing.into())
}

async fn handle_ds(app: &mut AppGlobals, value: Vec<u8>) -> Result<(), btleplug::Error> {
    if let Some(command_byte) = value.first() {
        match CommandID::try_from(*command_byte) {
            Ok(CommandID::GetNotificationAttributes) => {
                if let Ok((_, recv)) = GetNotificationAttributesResponse::parse(&value) {
                    update_notif_with_notif_attributes(
                        app,
                        recv.notification_uid,
                        &recv.attribute_list,
                    )
                    .await?;
                    if let Some(handle) = app.sent_notifs.get_mut(&recv.notification_uid) {
                        update_handle(handle);
                    }
                    if let Some(send) = app.pending_notifs.remove(&recv.notification_uid) {
                        if let Ok(handle) = show_notification(&send) {
                            let notif_id = get_handle_id(&handle);
                            let notification_uid = recv.notification_uid;
                            app.sent_notifs.insert(notification_uid, handle);
                            add_action_handlers(app, notif_id, notification_uid);
                        }
                    }
                }
            }
            Ok(CommandID::GetAppAttributes) => {
                if let Ok((_, recv)) = GetAppAttributesResponse::parse(&value) {
                    for attr in &recv.attribute_list {
                        match attr.id {
                            AppAttributeID::DisplayName => {
                                if cfg!(all(unix, not(target_os = "macos"))) {
                                    // only XDG will use the application name
                                    if let Some(appname) = &attr.value {
                                        app.app_names
                                            .insert(recv.app_identifier.clone(), appname.clone());
                                        if let Some(needs_appname) =
                                            app.needs_appname.remove(&recv.app_identifier)
                                        {
                                            for notification_uid in needs_appname {
                                                if let Some(send) =
                                                    app.pending_notifs.get_mut(&notification_uid)
                                                {
                                                    send.appname(appname);
                                                }
                                                if let Some(handle) =
                                                    app.sent_notifs.get_mut(&notification_uid)
                                                {
                                                    handle.appname(appname);
                                                    update_handle(handle);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    eprintln!("couldn't parse appinfo from message {:?}", &value);
                }
            }
            _ => {
                eprintln!("unknown command in message {:?}", &value);
            }
        }
    } else {
        eprintln!("missing a command byte");
    }
    Ok(())
}

// only XDG can set hints
#[cfg(all(unix, not(target_os = "macos")))]
fn add_hint(send: &mut Notification, hint: Hint) {
    send.hint(hint);
}
#[cfg(not(all(unix, not(target_os = "macos"))))]
fn add_hint(_send: &mut Notification, _hint: Hint) {}

// only XDG can set urgency
#[cfg(all(unix, not(target_os = "macos")))]
fn set_urgency(send: &mut Notification, urgency: Urgency) {
    send.urgency(urgency);
}
#[cfg(not(all(unix, not(target_os = "macos"))))]
fn set_urgency(_send: &mut Notification, _urgency: Urgency) {}

// only XDG has capability query
#[cfg(all(unix, not(target_os = "macos")))]
fn has_capability(cap: &str) -> bool {
    let capabilities = notify_rust::get_capabilities();
    capabilities.is_ok() && capabilities.unwrap().iter().any(|c| c == cap)
}
#[cfg(not(all(unix, not(target_os = "macos"))))]
fn has_capability(_cap: &str) -> bool {
    false
}

async fn set_notif_from_gatt(
    app: &mut AppGlobals,
    notification_uid: u32,
) -> Result<(), btleplug::Error> {
    let recv = app.received_notifs.get(&notification_uid).unwrap();
    let send = if app.pending_notifs.contains_key(&notification_uid) {
        app.pending_notifs.get_mut(&notification_uid).unwrap()
    } else if app.sent_notifs.contains_key(&notification_uid) {
        app.sent_notifs.get_mut(&notification_uid).unwrap()
    } else {
        return Ok(());
    };
    if recv.event_flags.contains(EventFlag::Silent) {
        add_hint(send, Hint::SuppressSound(true));
    }
    if recv.event_flags.contains(EventFlag::Important) {
        set_urgency(send, Urgency::Critical);
        send.timeout(Timeout::Never);
    }
    if recv.category_id != CategoryID::Other {
        add_hint(
            send,
            Hint::Category(
                match recv.category_id {
                    CategoryID::IncomingCall => "x-apple.call.incoming",
                    CategoryID::MissedCall => "x-apple.call.missed",
                    CategoryID::Voicemail => "x-apple.voicemail",
                    CategoryID::Social => "x-apple.social",
                    CategoryID::Schedule => "x-apple.schedule",
                    CategoryID::Email => "email",
                    CategoryID::News => "x-apple.news",
                    CategoryID::HealthAndFitness => "x-apple.health-and-fitness",
                    CategoryID::BusinessAndFinance => "x-apple.business-and-finance",
                    CategoryID::Location => "x-apple.location",
                    CategoryID::Entertainment => "x-apple.entertainment",
                    CategoryID::Other => unreachable!(),
                }
                .to_string(),
            ),
        );
        // what to do here... XDG is the only one that supports icons at the moment,
        // so we can assume freedesktop icons work. but not all of these are standard.
        // XDG also supports icons in file:///, and so should Windows too eventually
        // both XDG and Windows also support "images," but on Windows those are large
        // could ship with icons for each category. Windows app store categories
        // don't have icons. iOS app store does, but it doesn't cover all of them.
        // XDG should ideally use themed icons, but again they're not all standard,
        // or in the right categories.
        send.icon(match recv.category_id {
            CategoryID::IncomingCall => "call-start", // standard action
            CategoryID::MissedCall => "call-missed",  // nonstandard
            CategoryID::Voicemail => "media-tape",    // standard device
            CategoryID::Social => "internet-group-chat", // or internet-chat? nonstandard
            CategoryID::Schedule => "calendar-month", // or office-calendar? nonstandard
            CategoryID::Email => "internet-mail",     // or mail-unread? nonstandard
            CategoryID::News => "application-rss+xml", // nonstandard mime type
            CategoryID::HealthAndFitness => "applications-health", // nonstandard category
            CategoryID::BusinessAndFinance => "applications-office", // or money? standard category
            CategoryID::Location => "maps",           // or mark-location? nonstandard
            CategoryID::Entertainment => "applications-multimedia", // standard category
            CategoryID::Other => unreachable!(),
        });
    }
    // macOS should get the default app icon from the bundle, XDG should get it from the desktop file
    if app.cp_char.is_some() {
        let mut attrs = vec![(NotificationAttributeID::Title, Some(u16::MAX))];
        if cfg!(any(windows, all(unix, not(target_os = "macos")))) {
            // only Windows can set the app ID per notification
            // only XDG will use the application name
            attrs.push((NotificationAttributeID::AppIdentifier, None));
        }
        if cfg!(any(windows, target_os = "macos")) {
            // only macOS and Windows will use the subtitle
            attrs.push((NotificationAttributeID::Subtitle, Some(u16::MAX)));
        }
        if cfg!(any(windows, target_os = "macos")) || has_capability("body") {
            // macOS and Windows will use the body message, on XDG it is dependent on server capabilities
            attrs.push((NotificationAttributeID::Message, Some(u16::MAX)));
        }
        if cfg!(all(unix, not(target_os = "macos"))) && has_capability("actions") {
            // only XDG will use action labels, and only if server supports it
            if recv.event_flags.contains(EventFlag::PositiveAction) {
                attrs.push((NotificationAttributeID::PositiveActionLabel, None));
            }
            if recv.event_flags.contains(EventFlag::NegativeAction) {
                attrs.push((NotificationAttributeID::NegativeActionLabel, None));
            }
        }
        write_details_request(app, recv.notification_uid, attrs).await?;
    }
    Ok(())
}

// only XDG can remove notifications
#[cfg(all(unix, not(target_os = "macos")))]
fn close_handle(handle: NotificationHandle) {
    handle.close();
}
#[cfg(not(all(unix, not(target_os = "macos"))))]
fn close_handle(_handle: NotificationHandle) {}

async fn handle_ns(app: &mut AppGlobals, value: Vec<u8>) -> Result<(), btleplug::Error> {
    if let Ok((_, recv)) = GattNotification::parse(&value) {
        match recv.event_id {
            EventID::NotificationAdded => {
                let mut send = Notification::new();
                add_hint(&mut send, Hint::ActionIcons(true));
                add_hint(&mut send, Hint::DesktopEntry(env!("CARGO_PKG_NAME").into()));
                let notification_uid = recv.notification_uid;
                app.received_notifs.insert(notification_uid, recv);
                app.pending_notifs.insert(notification_uid, send);
                set_notif_from_gatt(app, notification_uid).await?;
            }
            EventID::NotificationModified => {
                if cfg!(all(unix, not(target_os = "macos"))) {
                    // only XDG can update notifications, so don't request details
                    let notification_uid = recv.notification_uid;
                    app.received_notifs.insert(notification_uid, recv);
                    set_notif_from_gatt(app, notification_uid).await?;
                    if let Some(handle) = app.sent_notifs.get_mut(&notification_uid) {
                        update_handle(handle);
                    }
                }
            }
            EventID::NotificationRemoved => {
                if let Some(handle) = app.sent_notifs.remove(&recv.notification_uid) {
                    close_handle(handle);
                }
                app.pending_notifs.remove(&recv.notification_uid);
                app.received_notifs.remove(&recv.notification_uid);
            }
        }
    } else {
        eprintln!("couldn't parse NS message {:?}", &value);
    }
    Ok(())
}

async fn watch_device(
    peripheral: Peripheral,
    mut quit_rx: watch::Receiver<()>,
    mut disconnect_rx: oneshot::Receiver<()>,
) -> Result<(), btleplug::Error> {
    // find the characteristics we want
    let chars = peripheral.characteristics();
    // Support for the Notification Source characteristic is mandatory
    let ns_char = chars
        .iter()
        .find(|c| c.uuid == ancs::characteristics::notification_source::NOTIFICATION_SOURCE_UUID)
        .unwrap();
    let cp_char = chars
        .iter()
        .find(|c| c.uuid == ancs::characteristics::control_point::CONTROL_POINT_UUID);
    let ds_char = chars
        .iter()
        .find(|c| c.uuid == ancs::characteristics::data_source::DATA_SOURCE_UUID);

    println!("subscribing to {ns_char:?} on {}", peripheral.address());
    peripheral.subscribe(ns_char).await?;
    if let Some(ds_char_ok) = ds_char {
        println!("subscribing to {ds_char_ok:?}");
        peripheral.subscribe(ds_char_ok).await?;
    }

    let mut notification_stream = peripheral.notifications().await?;

    let mut app = AppGlobals {
        peripheral: peripheral.clone(),
        received_notifs: HashMap::new(),
        pending_notifs: HashMap::new(),
        sent_notifs: HashMap::new(),
        app_names: HashMap::new(),
        needs_appname: HashMap::new(),
        ns_char: ns_char.clone(),
        cp_char: cp_char.cloned(),
        ds_char: ds_char.cloned(),
    };

    // Process while the BLE connection is not broken or stopped.
    loop {
        tokio::select! {
            _ = quit_rx.changed() => {
                break;
            },
            _ = &mut disconnect_rx => {
                break;
            },
            Some(data) = notification_stream.next() => {
                if data.uuid == ancs::characteristics::notification_source::NOTIFICATION_SOURCE_UUID {
                    handle_ns(&mut app, data.value).await?;
                } else if data.uuid == ancs::characteristics::data_source::DATA_SOURCE_UUID {
                    handle_ds(&mut app, data.value).await?;
                } else {
                    eprintln!(
                        "got an unexpected uuid {:?} with message {:?}",
                        data.uuid, data.value
                    );
                }
            }
        }
    }

    if let Some(ds_char_ok) = app.ds_char {
        if let Err(e) = app.peripheral.unsubscribe(&ds_char_ok).await {
            eprintln!("error unsubscribing from DS: {e:?}");
        }
    }
    if let Err(e) = app.peripheral.unsubscribe(&app.ns_char).await {
        eprintln!("error unsubscribing from NS: {e:?}");
    }
    Ok(())
}

fn load_icon() -> tray_icon::Icon {
    let (icon_rgba, icon_width, icon_height) = {
        let image = image::ImageReader::with_format(
            std::io::Cursor::new(include_bytes!("../icon-32-white.png")),
            image::ImageFormat::Png,
        )
        .decode()
        .unwrap()
        .into_rgba8();
        let (width, height) = image.dimensions();
        let rgba = image.into_raw();
        (rgba, width, height)
    };
    tray_icon::Icon::from_rgba(icon_rgba, icon_width, icon_height).unwrap()
}

fn main() {
    let event_loop = EventLoop::new();

    let quit_item = MenuItem::new("Quit", true, None);
    let quit_id = quit_item.id().clone();
    let tray_menu = Menu::with_items(&[
        &MenuItem::new(
            concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION")),
            false,
            None,
        ),
        &PredefinedMenuItem::separator(),
        &quit_item,
    ]).unwrap();
    let icon_tray = load_icon();
    let mut tray_icon = Some(
        TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip(env!("CARGO_BIN_NAME"))
            .with_icon(icon_tray)
            .build()
            .unwrap(),
    );
    tray_icon.as_mut().unwrap().set_icon_as_template(true);

    let (quit_tx, quit_rx) = watch::channel(());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut join_handle = Some(std::thread::spawn(move || {
        rt.block_on(inner_main(quit_rx.clone())).unwrap()
    }));

    let menu_channel = MenuEvent::receiver();

    event_loop.run(move |window_event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::new(1, 0));
        if join_handle.is_none() || join_handle.as_ref().unwrap().is_finished() {
            tray_icon.take();
            *control_flow = ControlFlow::Exit;
        }
        if let Ok(menu_event) = menu_channel.try_recv() {
            if menu_event.id == quit_id {
                quit_tx.send(()).unwrap();
                join_handle.take().unwrap().join().unwrap();
                tray_icon.take();
                *control_flow = ControlFlow::Exit;
            }
        }
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = window_event
        {
            quit_tx.send(()).unwrap();
            join_handle.take().unwrap().join().unwrap();
            tray_icon.take();
            *control_flow = ControlFlow::Exit;
        }
    });
}

async fn inner_main(mut quit_rx: watch::Receiver<()>) -> Result<(), Box<dyn Error>> {
    let manager = Manager::new().await?;

    // get the first bluetooth adapter
    let adapters = manager.adapters().await?;
    if adapters.is_empty() {
        println!("no adapters found");
        return Ok(());
    }
    let central = &adapters[0];
    println!("using adapter {}", central.adapter_info().await?);

    let mut tasks = tokio::task::JoinSet::new();
    let mut disconnect_txs = HashMap::new();

    let mut events = central.events().await?;
    loop {
        tokio::select! {
            _ = quit_rx.changed() => {
                break;
            },
            Some(event) = events.next() => {
                match event {
                    CentralEvent::DeviceConnected(id) => {
                        let peripheral = central.peripheral(&id).await?;
                        peripheral.discover_services().await?;
                        if peripheral.services().iter().any(|s| s.uuid == ancs::APPLE_NOTIFICATION_CENTER_SERVICE_UUID) {
                            let (disconnect_tx, disconnect_rx) = oneshot::channel();
                            disconnect_txs.insert(id.clone(), disconnect_tx);
                            tasks.spawn(watch_device(peripheral, quit_rx.clone(), disconnect_rx));
                        }
                    }
                    CentralEvent::DeviceDisconnected(id) => {
                        if let Some(disconnect_tx) = disconnect_txs.remove(&id) {
                            disconnect_tx.send(()).unwrap();
                        }
                    }
                    _ => {}
                }
            },
        }
    }
    while let Some(res) = tasks.join_next().await {
        res??;
    }
    Ok(())
}
