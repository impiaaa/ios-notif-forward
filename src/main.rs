use ancs::attributes::app::AppAttributeID;
use ancs::attributes::category::CategoryID;
use ancs::attributes::command::CommandID;
use ancs::attributes::event::{EventFlag, EventID};
use ancs::attributes::notification::NotificationAttributeID;
use ancs::attributes::NotificationAttribute;
use ancs::characteristics::control_point::{
    GetAppAttributesRequest, GetNotificationAttributesRequest,
};
use ancs::characteristics::data_source::{
    GetAppAttributesResponse, GetNotificationAttributesResponse,
};
use ancs::characteristics::notification_source::Notification as GattNotification;
use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Manager, Peripheral};
use futures::stream::StreamExt;
use notify_rust::{Hint, Notification, NotificationHandle, Timeout, Urgency};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::io::Write;
use std::time::Duration;
use tokio::time;

struct AppGlobals {
    peripheral: Peripheral,
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
    println!(
        "writing details request for notification {} attributes {:?}",
        &notification_uid, &attribute_ids
    );
    let req = GetNotificationAttributesRequest {
        command_id: CommandID::GetNotificationAttributes,
        notification_uid: notification_uid,
        attribute_ids: attribute_ids,
    };
    let out: Vec<u8> = req.into();
    app.peripheral
        .write(app.cp_char.as_ref().unwrap(), &out, WriteType::WithResponse)
        .await?;
    Ok(())
}

async fn write_appinfo_request(
    app: &mut AppGlobals,
    app_identifier: &String,
    attribute_ids: Vec<AppAttributeID>,
) -> Result<(), btleplug::Error> {
    println!(
        "writing appinfo request for application {} attributes {:?}",
        &app_identifier, &attribute_ids
    );
    let req = GetAppAttributesRequest {
        command_id: CommandID::GetAppAttributes,
        app_identifier: app_identifier.clone(),
        attribute_ids: attribute_ids,
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

async fn update_notif_with_notif_attributes(
    app: &mut AppGlobals,
    notification_uid: u32,
    attribute_list: &Vec<NotificationAttribute>,
) -> Result<(), btleplug::Error> {
    let mut send = if app.pending_notifs.contains_key(&notification_uid) {
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
                    set_app_id(&mut send, &appid);
                    if cfg!(all(unix, not(target_os = "macos"))) {
                        // only XDG will use the application name
                        if let Some(appname) = app.app_names.get(appid) {
                            send.appname(&appname);
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
                    send.summary(&title);
                }
            }
            NotificationAttributeID::Subtitle => {
                if let Some(subtitle) = &attr.value {
                    send.subtitle(&subtitle);
                }
            }
            NotificationAttributeID::Message => {
                if let Some(message) = &attr.value {
                    send.body(&message);
                }
            }
            NotificationAttributeID::MessageSize => {}
            NotificationAttributeID::Date => {}
            NotificationAttributeID::PositiveActionLabel => {
                if let Some(label) = &attr.value {
                    send.action("dialog-ok", &label);
                }
            }
            NotificationAttributeID::NegativeActionLabel => {
                if let Some(label) = &attr.value {
                    send.action("dialog-close", &label);
                }
            }
        }
    }
    for attr in attribute_list {
        match attr.id {
            NotificationAttributeID::AppIdentifier => {
                if cfg!(all(unix, not(target_os = "macos"))) {
                    // only XDG will use the application name
                    if let Some(appid) = &attr.value {
                        if !app.app_names.contains_key(appid) {
                            write_appinfo_request(app, appid, vec![AppAttributeID::DisplayName])
                                .await?;
                        }
                    }
                }
            }
            _ => {}
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

async fn handle_ds(app: &mut AppGlobals, value: Vec<u8>) -> Result<(), btleplug::Error> {
    if let Some(command_byte) = value.first() {
        match CommandID::try_from(command_byte.clone()) {
            Ok(CommandID::GetNotificationAttributes) => {
                if let Ok((_, recv)) = GetNotificationAttributesResponse::parse(&value) {
                    println!("{:?}", recv);
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
                        if let Ok(handle) = send.show() {
                            app.sent_notifs.insert(recv.notification_uid, handle);
                        }
                    }
                }
            }
            Ok(CommandID::GetAppAttributes) => {
                if let Ok((_, recv)) = GetAppAttributesResponse::parse(&value) {
                    println!("{:?}", recv);
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
                                                    send.appname(&appname);
                                                }
                                                if let Some(handle) =
                                                    app.sent_notifs.get_mut(&notification_uid)
                                                {
                                                    handle.appname(&appname);
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
                    println!("couldn't parse appinfo from message {:?}", &value);
                }
            }
            _ => {
                println!("unknown command in message {:?}", &value);
            }
        }
    } else {
        println!("missing a command byte");
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
    recv: &GattNotification,
    notification_uid: u32,
) -> Result<(), btleplug::Error> {
    let mut send = if app.pending_notifs.contains_key(&notification_uid) {
        app.pending_notifs.get_mut(&notification_uid).unwrap()
    } else if app.sent_notifs.contains_key(&notification_uid) {
        app.sent_notifs.get_mut(&notification_uid).unwrap()
    } else {
        return Ok(());
    };
    if recv.event_flags.contains(EventFlag::Silent) {
        add_hint(&mut send, Hint::SuppressSound(true));
    }
    if recv.event_flags.contains(EventFlag::Important) {
        set_urgency(&mut send, Urgency::Critical);
        send.timeout(Timeout::Never);
    }
    if recv.category_id != CategoryID::Other {
        add_hint(
            &mut send,
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
        send.icon(match recv.category_id {
            CategoryID::IncomingCall => "call-start",
            CategoryID::MissedCall => "call-missed",
            CategoryID::Voicemail => "media-tape",
            CategoryID::Social => "internet-group-chat",
            CategoryID::Schedule => "calendar-month",
            CategoryID::Email => "internet-mail",
            CategoryID::News => "application-rss+xml", // mime type not great
            CategoryID::HealthAndFitness => "applications-health",
            CategoryID::BusinessAndFinance => "applications-office",
            CategoryID::Location => "maps",
            CategoryID::Entertainment => "applications-multimedia",
            CategoryID::Other => unreachable!(),
        });
    }
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
        if cfg!(all(unix, not(target_os = "macos"))) {
            if has_capability("actions") {
                // only XDG will use action labels, and only if server supports it
                if recv.event_flags.contains(EventFlag::PositiveAction) {
                    attrs.push((NotificationAttributeID::PositiveActionLabel, None));
                }
                if recv.event_flags.contains(EventFlag::NegativeAction) {
                    attrs.push((NotificationAttributeID::NegativeActionLabel, None));
                }
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
        println!("{:?}", recv);
        match recv.event_id {
            EventID::NotificationAdded => {
                let mut send = Notification::new();
                add_hint(&mut send, Hint::ActionIcons(true));
                app.pending_notifs.insert(recv.notification_uid, send);
                set_notif_from_gatt(app, &recv, recv.notification_uid).await?;
            }
            EventID::NotificationModified => {
                if cfg!(all(unix, not(target_os = "macos"))) {
                    // only XDG can update notifications, so don't request details
                    set_notif_from_gatt(app, &recv, recv.notification_uid).await?;
                    if let Some(handle) = app.sent_notifs.get_mut(&recv.notification_uid) {
                        update_handle(handle);
                    }
                }
            }
            EventID::NotificationRemoved => {
                if let Some(handle) = app.sent_notifs.remove(&recv.notification_uid) {
                    close_handle(handle);
                }
                app.pending_notifs.remove(&recv.notification_uid);
            }
        }
    } else {
        println!("couldn't parse NS message {:?}", &value);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let manager = Manager::new().await?;

    // get the first bluetooth adapter
    let adapters = manager.adapters().await?;
    for a in &adapters {
        println!("{}", a.adapter_info().await?);
    }
    let central = if adapters.len() == 1 {
        &adapters[0]
    } else {
        print!("choose: ");
        std::io::stdout().flush()?;
        let mut line1 = String::new();
        std::io::stdin().read_line(&mut line1)?;
        &adapters[line1.trim().parse::<usize>()?]
    };

    // start scanning for devices
    central
        .start_scan(ScanFilter {
            services: vec![ancs::APPLE_NOTIFICATION_CENTER_SERVICE_UUID],
        })
        .await?;
    // instead of waiting, you can use central.events() to get a stream which will
    // notify you of new devices, for an example of that see examples/event_driven_discovery.rs
    time::sleep(Duration::from_secs(2)).await;
    central.stop_scan().await?;

    // find the device we're interested in
    let peripherals = central.peripherals().await?;
    for p in &peripherals {
        let local_name = match p.properties().await.unwrap().unwrap().local_name {
            Some(name) => name,
            None => p.address().to_string(),
        };
        println!("{}", local_name);
    }
    let peripheral = if peripherals.len() == 1 {
        &peripherals[0]
    } else {
        print!("choose: ");
        std::io::stdout().flush()?;
        let mut line2 = String::new();
        std::io::stdin().read_line(&mut line2)?;
        &peripherals[line2.trim().parse::<usize>()?]
    };

    // connect to the device
    peripheral.connect().await?;

    // discover services and characteristics
    peripheral.discover_services().await?;

    // find the characteristic we want
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

    println!("got NS characteristic {:?}", ns_char);
    peripheral.subscribe(&ns_char).await?;
    if let Some(ds_char_ok) = ds_char {
        println!("got DS characteristic {:?}", ds_char_ok);
        peripheral.subscribe(&ds_char_ok).await?;
    }

    let mut notification_stream = peripheral.notifications().await?;

    let mut app = AppGlobals {
        peripheral: peripheral.clone(),
        pending_notifs: HashMap::new(),
        sent_notifs: HashMap::new(),
        app_names: HashMap::new(),
        needs_appname: HashMap::new(),
        ns_char: ns_char.clone(),
        cp_char: cp_char.cloned(),
        ds_char: ds_char.cloned(),
    };

    println!("listening");

    // Process while the BLE connection is not broken or stopped.
    while let Some(data) = notification_stream.next().await {
        if data.uuid == ancs::characteristics::notification_source::NOTIFICATION_SOURCE_UUID {
            handle_ns(&mut app, data.value).await?;
        } else if data.uuid == ancs::characteristics::data_source::DATA_SOURCE_UUID {
            handle_ds(&mut app, data.value).await?;
        } else {
            println!(
                "got an unexpected uuid {:?} with message {:?}",
                data.uuid, data.value
            );
        }
    }

    if let Some(ds_char_ok) = app.ds_char {
        app.peripheral.unsubscribe(&ds_char_ok).await;
    }
    app.peripheral.unsubscribe(&app.ns_char).await;
    app.peripheral.disconnect().await?;

    Ok(())
}
