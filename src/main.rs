use ancs::attributes::app::AppAttributeID;
use ancs::attributes::category::CategoryID;
use ancs::attributes::command::CommandID;
use ancs::attributes::event::{EventID, EventFlag};
use ancs::attributes::notification::NotificationAttributeID;
use ancs::characteristics::control_point::{GetNotificationAttributesRequest, GetAppAttributesRequest};
use ancs::characteristics::data_source::{GetNotificationAttributesResponse, GetAppAttributesResponse, NotificationAttribute};
use ancs::characteristics::notification_source::GattNotification;
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType, Characteristic};
use btleplug::platform::{Manager, Peripheral};
use futures::stream::StreamExt;
use notify_rust::{Notification, NotificationHandle, Hint, Urgency};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::time::Duration;
use tokio::time;
use uuid::Uuid;

struct AppGlobals {
    peripheral: Peripheral,
    pending_notifs: HashMap<u32, Notification>,
    sent_notifs: HashMap<u32, NotificationHandle>,
    app_names: HashMap<String, String>,
    needs_appname: HashMap<String, HashSet<u32>>,
    ns_uuid: Uuid,
    ns_char: Characteristic,
    cp_uuid: Uuid,
    cp_char: Option<Characteristic>,
    ds_uuid: Uuid,
    ds_char: Option<Characteristic>
}

async fn write_details_request(app: &mut AppGlobals, notification_uid: u32, attribute_ids: Vec<(NotificationAttributeID, Option<u16>)>) -> Result<(), btleplug::Error> {
    let req = GetNotificationAttributesRequest {
        command_id: CommandID::GetNotificationAttributes,
        notification_uid: notification_uid,
        attribute_ids: attribute_ids
    };
    let out: Vec<u8> = req.into();
    println!("writing details request");
    app.peripheral.write(app.cp_char.as_ref().unwrap(), &out, WriteType::WithResponse).await?;
    Ok(())
}

async fn write_appinfo_request(app: &mut AppGlobals, app_identifier: &String, attribute_ids: Vec<AppAttributeID>) -> Result<(), btleplug::Error> {
    let req = GetAppAttributesRequest {
        command_id: CommandID::GetAppAttributes,
        app_identifier: app_identifier.clone(),
        attribute_ids: attribute_ids
    };
    let out: Vec<u8> = req.into();
    println!("writing appinfo request");
    app.peripheral.write(app.cp_char.as_ref().unwrap(), &out, WriteType::WithResponse).await?;
    Ok(())
}

async fn update_notif_with_notif_attributes(app: &mut AppGlobals, notification_uid: u32, attribute_list: &Vec<NotificationAttribute>) -> Result<(), btleplug::Error> {
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
                    //send.app_id(&appid);
                    if let Some(appname) = app.app_names.get(appid) {
                        send.appname(&appname);
                    }
                    else {
                        if !app.needs_appname.contains_key(appid) {
                            app.needs_appname.insert(appid.clone(), HashSet::new());
                        }
                        app.needs_appname.get_mut(appid).unwrap().insert(notification_uid);
                    }
                }
            },
            NotificationAttributeID::Title => {
                if let Some(title) = &attr.value {
                    send.summary(&title);
                }
            },
            NotificationAttributeID::Subtitle => {
                if let Some(subtitle) = &attr.value {
                    send.subtitle(&subtitle);
                }
            },
            NotificationAttributeID::Message => {
                if let Some(message) = &attr.value {
                    send.body(&message);
                }
            },
            NotificationAttributeID::MessageSize => {
            },
            NotificationAttributeID::Date => {
            },
            NotificationAttributeID::PositiveActionLabel => {
                if let Some(label) = &attr.value {
                    send.action("dialog-ok", &label);
                }
            },
            NotificationAttributeID::NegativeActionLabel => {
                if let Some(label) = &attr.value {
                    send.action("dialog-cancel", &label);
                }
            }
        }
    }
    for attr in attribute_list {
        match attr.id {
            NotificationAttributeID::AppIdentifier => {
                if let Some(appid) = &attr.value {
                    if !app.app_names.contains_key(appid) {
                        write_appinfo_request(app, appid, vec![AppAttributeID::DisplayName]).await?;
                    }
                }
            },
            _ => {
            }
        }
    }
    Ok(())
}

async fn handle_ds(app: &mut AppGlobals, value: Vec<u8>) -> Result<(), btleplug::Error> {
    if let Some(command_byte) = value.first() {
        match CommandID::try_from(command_byte.clone()) {
            Ok(CommandID::GetNotificationAttributes) => {
                if let Ok((_, recv)) = GetNotificationAttributesResponse::parse(&value) {
                    println!("{:?}", recv);
                    update_notif_with_notif_attributes(app, recv.notification_uid, &recv.attribute_list).await?;
                    if let Some(send) = app.sent_notifs.get_mut(&recv.notification_uid) {
                        send.update();
                    }
                    if let Some(send) = app.pending_notifs.remove(&recv.notification_uid) {
                        if let Ok(handle) = send.show() {
                            app.sent_notifs.insert(recv.notification_uid, handle);
                        }
                    }
                }
            },
            Ok(CommandID::GetAppAttributes) => {
                if let Ok((_, recv)) = GetAppAttributesResponse::parse(&value) {
                    println!("{:?}", recv);
                    /*for attr in &recv.attribute_list {
                        match attr.id {
                            AppAttributeID::DisplayName => {
                                if let Some(appname) = &attr.value {
                                    app.app_names.insert(recv.app_identifier, appname.clone());
                                    if let Some(needs_appname) = app.needs_appname.remove(&recv.app_identifier) {
                                        for notification_uid in needs_appname {
                                            if let Some(send) = app.pending_notifs.get(&notification_uid) {
                                                send.appname(&appname);
                                            }
                                            if let Some(send) = app.sent_notifs.get(&notification_uid) {
                                                send.appname(&appname);
                                                send.update();
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }*/
                }
            },
            _ => {
            }
        }
    }
    else {
        println!("but it's missing a command byte");
    }
    Ok(())
}

async fn update_notif(app: &mut AppGlobals, recv: &GattNotification, notification_uid: u32) -> Result<(), btleplug::Error> {
    let send = if app.pending_notifs.contains_key(&notification_uid) {
        app.pending_notifs.get_mut(&notification_uid).unwrap()
    } else if app.sent_notifs.contains_key(&notification_uid) {
        app.sent_notifs.get_mut(&notification_uid).unwrap()
    } else {
        return Ok(());
    };
    // FIXME this is a bitfield
    match recv.event_flags {
        EventFlag::Silent => {
            send.hint(Hint::SuppressSound(true));
        }
        EventFlag::Important => {
            send.urgency(Urgency::Critical);
        }
        _ => {
        }
    };
    if recv.category_id != CategoryID::Other {
        send.hint(Hint::Category(match recv.category_id {
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
            CategoryID::Other => unreachable!()
        }.to_string()));
        send.icon(match recv.category_id {
            CategoryID::IncomingCall => "call-start",
            CategoryID::MissedCall => "call-missed",
            CategoryID::Voicemail => "media-tape",
            CategoryID::Social => "system-users",
            CategoryID::Schedule => "task-due",
            CategoryID::Email => "mail-unread",
            CategoryID::News => "application-rss+xml",
            CategoryID::HealthAndFitness => "applications-health",
            CategoryID::BusinessAndFinance => "money",
            CategoryID::Location => "mark-location",
            CategoryID::Entertainment => "applications-multimedia",
            CategoryID::Other => unreachable!()
        });
    }
    if app.cp_char.is_some() {
        write_details_request(app, recv.notification_uid, vec![(NotificationAttributeID::AppIdentifier, None), (NotificationAttributeID::Title, Some(u16::MAX)), (NotificationAttributeID::Subtitle, Some(u16::MAX)), (NotificationAttributeID::Message, Some(u16::MAX))]).await?;
    }
    Ok(())
}

async fn handle_ns(app: &mut AppGlobals, value: Vec<u8>) -> Result<(), btleplug::Error> {
    if let Ok((_, recv)) = GattNotification::parse(&value) {
        println!("{:?}", recv);
        match recv.event_id {
            EventID::NotificationAdded => {
                let mut send = Notification::new();
                send.hint(Hint::ActionIcons(true));
                app.pending_notifs.insert(recv.notification_uid, send);
                update_notif(app, &recv, recv.notification_uid).await?;
            }
            EventID::NotificationModified => {
                update_notif(app, &recv, recv.notification_uid).await?;
                if let Some(handle) = app.sent_notifs.get_mut(&recv.notification_uid) {
                    handle.update();
                }
            }
            EventID::NotificationRemoved => {
                if let Some(handle) = app.sent_notifs.remove(&recv.notification_uid) {
                    handle.close();
                }
                app.pending_notifs.remove(&recv.notification_uid);
            }
        }
    }
    else {
        println!("but it couldn't be parsed");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let manager = Manager::new().await?;

    // get the first bluetooth adapter
    let adapters = manager.adapters().await?;
    let central = adapters.into_iter().nth(0).unwrap();

    // start scanning for devices
    central.start_scan(ScanFilter { services: vec![Uuid::try_parse(ancs::APPLE_NOTIFICATION_CENTER_SERVICE_UUID).unwrap()] }).await?;
    // instead of waiting, you can use central.events() to get a stream which will
    // notify you of new devices, for an example of that see examples/event_driven_discovery.rs
    time::sleep(Duration::from_secs(2)).await;
    central.stop_scan().await?;
    
    // find the device we're interested in
    let peripherals = central.peripherals().await?;
    let peripheral = peripherals.first().unwrap();
    let local_name = match peripheral.properties().await.unwrap().unwrap().local_name {
        Some(name) => name,
        None => peripheral.address().to_string()
    };
    println!("Connecting to {}", local_name);
    
    // connect to the device
    peripheral.connect().await?;
    
    println!("Connected to {}", local_name);

    // discover services and characteristics
    peripheral.discover_services().await?;

    // find the characteristic we want
    let chars = peripheral.characteristics();
    // Support for the Notification Source characteristic is mandatory
    let ns_uuid = Uuid::try_parse(ancs::characteristics::notification_source::NOTIFICATION_SOURCE_UUID).unwrap();
    let ns_char = chars.iter().find(|c| c.uuid == ns_uuid).unwrap();
    let cp_uuid = Uuid::try_parse(ancs::characteristics::control_point::CONTROL_POINT_UUID).unwrap();
    let cp_char = chars.iter().find(|c| c.uuid == cp_uuid);
    let ds_uuid = Uuid::try_parse(ancs::characteristics::data_source::DATA_SOURCE_UUID).unwrap();
    let ds_char = chars.iter().find(|c| c.uuid == ds_uuid);
    
    println!("got NS characteristic {:?}", ns_char);
    peripheral.subscribe(&ns_char).await?;
    if let Some(ds_char_ok) = ds_char {
        println!("got DS characteristic {:?}", ds_char_ok);
        peripheral.subscribe(&ds_char_ok).await?;
    }
    
    println!("subscribed");
    
    let mut notification_stream =
        peripheral.notifications().await?;
    
    println!("got notification stream");
    
    let mut app = AppGlobals {
        peripheral: peripheral.clone(),
        pending_notifs: HashMap::new(),
        sent_notifs: HashMap::new(),
        app_names: HashMap::new(),
        needs_appname: HashMap::new(),
        ns_uuid: ns_uuid,
        ns_char: ns_char.clone(),
        cp_uuid: cp_uuid,
        cp_char: cp_char.cloned(),
        ds_uuid: ds_uuid,
        ds_char: ds_char.cloned()
    };
    
    // Process while the BLE connection is not broken or stopped.
    while let Some(data) = notification_stream.next().await {
        println!(
            "Received data from {:?} [{:?}]: {:?}",
            local_name, data.uuid, data.value
        );
        if data.uuid == app.ns_uuid {
            handle_ns(&mut app, data.value).await?;
        }
        else if data.uuid == app.ds_uuid {
            handle_ds(&mut app, data.value).await?;
        }
        else {
            println!("got an unexpected uuid {:?}", data.uuid);
        }
    }
    
    if let Some(ds_char_ok) = app.ds_char {
        app.peripheral.unsubscribe(&ds_char_ok).await;
    }
    app.peripheral.unsubscribe(&app.ns_char).await;
    app.peripheral.disconnect().await?;

    Ok(())
}

