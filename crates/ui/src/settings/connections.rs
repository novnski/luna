//! Control this Mac: host readiness and remote authority are separate from the
//! Devices registry. All changes use the current engine's local-owner RPC.
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Context, Entity, EventEmitter, Image, ImageFormat, Render, SharedString, Subscription, Task,
    Window, div, img, prelude::*,
};
use luna_proto::remote_control::{HostTicket, PairingTicket};
use luna_rpc::{
    methods,
    remote_auth::{IssuedPairing, SessionView},
};
use serde_json::{Value, json};

use crate::{
    composer::ComposerInput,
    settings::{self, widgets},
    state::{AppState, EngineHandle},
    theme::Theme,
};

#[derive(Clone, Copy)]
pub struct HostingChanged {
    pub ready: bool,
}
struct PairingSheet {
    ticket: PairingTicket,
    qr: Arc<Image>,
    engine: EngineHandle,
}

pub struct ConnectionsPage {
    state: Entity<AppState>,
    bind: Entity<ComposerInput>,
    status: Value,
    sessions: Vec<SessionView>,
    sheet: Option<PairingSheet>,
    busy: bool,
    refreshing: bool,
    pairing_epoch: u64,
    power_status: SharedString,
    error: Option<SharedString>,
    message: Option<SharedString>,
    _poll: Task<()>,
    _observe: Subscription,
}

impl EventEmitter<HostingChanged> for ConnectionsPage {}
impl ConnectionsPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let bind = cx.new(|cx| ComposerInput::new("LAN address and port", cx));
        if let Some(address) = lan_bind() {
            bind.update(cx, |input, cx| input.set_text(address, cx));
        }
        let observe = cx.observe(&state, |this, _, cx| {
            this.refresh(cx);
            cx.notify();
        });
        let poll = cx.spawn(async move |this, cx| {
            loop {
                this.update(cx, |page, cx| page.refresh(cx)).ok();
                cx.background_executor().timer(Duration::from_secs(3)).await;
            }
        });
        Self {
            state,
            bind,
            status: Value::Null,
            sessions: Vec::new(),
            sheet: None,
            busy: false,
            refreshing: false,
            pairing_epoch: 0,
            power_status: "Off".into(),
            error: None,
            message: None,
            _poll: poll,
            _observe: observe,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refreshing {
            return;
        }
        let generation = self.state.read(cx).engine_generation();
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.status = Value::Null;
            self.sessions.clear();
            self.cancel_pairing(cx);
            cx.emit(HostingChanged { ready: false });
            cx.notify();
            return;
        };
        self.refreshing = true;
        cx.spawn(async move |this,cx| {
            let result = async {
                let status = engine.client().call("GetRemoteAccess", json!({})).await?;
                let sessions: Vec<SessionView> = engine.client().call_as(methods::LIST_REMOTE_SESSIONS, json!({})).await?;
                Ok::<_, luna_rpc::RpcError>((status,sessions))
            }.await;
            this.update(cx, |page,cx| {
                page.refreshing = false;
                if page.state.read(cx).engine_generation() != generation {
                    page.status = Value::Null;
                    page.sessions.clear();
                    page.cancel_pairing(cx);
                    cx.emit(HostingChanged { ready: false });
                    cx.notify();
                    return;
                }
                match result {
                    Ok((status,sessions)) => {
                        let ready = status["listening"] == true;
                        if page.status["bind"] != status["bind"] {
                            if let Some(bind) = status["bind"].as_str() {
                                page.bind.update(cx, |input,cx| input.set_text(bind.to_owned(),cx));
                            }
                        }
                        if page.sheet.as_ref().is_some_and(|sheet| sessions.iter().any(|session|
                            session.pairing_id.as_deref() == Some(&sheet.ticket.pairing_id)
                            && session.last_used_at_ms.is_some())) {
                            page.sheet = None;
                            page.message = Some("Phone paired and access verified.".into());
                        } else if page.sheet.as_ref().is_some_and(|sheet| sheet.ticket.expires_at_ms <= now()) {
                            page.cancel_pairing(cx);
                            page.message = Some("Pairing code expired. Select Add for a new code.".into());
                        }
                        page.status = status; page.sessions = sessions;
                        cx.emit(HostingChanged {ready});
                    }
                    Err(_) => {
                        page.status = Value::Null;
                        cx.emit(HostingChanged { ready: false });
                        page.error = Some("Could not read this Mac's remote access. Reconnect the local engine and refresh.".into());
                    }
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    fn toggle_hosting(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let generation = self.state.read(cx).engine_generation();
        let enabled = self.status["enabled"] != true;
        let bind = self.bind.read(cx).text().trim().to_owned();
        if enabled && bind.parse::<std::net::SocketAddr>().is_err() {
            self.error = Some("Choose this Mac's LAN address and a free port.".into());
            cx.notify();
            return;
        }
        let pairing_id = if enabled {
            None
        } else {
            self.sheet.take().map(|s| s.ticket.pairing_id)
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Some(id) = pairing_id {
                let _ = engine
                    .client()
                    .call(methods::REVOKE_REMOTE_PAIRING, json!({"id":id}))
                    .await;
            }
            let result = engine
                .client()
                .call("SetRemoteAccess", json!({"enabled":enabled,"bind":bind}))
                .await;
            this.update(cx, |page, cx| {
                page.busy = false;
                if page.state.read(cx).engine_generation() != generation {
                    page.refresh(cx);
                    cx.notify();
                    return;
                }
                match result {
                    Ok(status) => {
                        cx.emit(HostingChanged {
                            ready: status["listening"] == true,
                        });
                        page.status = status;
                    }
                    Err(error) => {
                        page.error = Some(format!("Could not change remote access: {error}").into())
                    }
                }
                page.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.sheet.is_some() || self.status["listening"] != true {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(pin) = self.status["spkiSha256"].as_str().map(str::to_owned) else {
            return;
        };
        let Some(address) = self.status["address"].as_str().map(str::to_owned) else {
            return;
        };
        self.busy = true;
        self.error = None;
        self.message = None;
        let generation = self.state.read(cx).engine_generation();
        self.pairing_epoch = self.pairing_epoch.wrapping_add(1);
        let epoch = self.pairing_epoch;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<IssuedPairing>(
                    methods::CREATE_REMOTE_PAIRING,
                    json!({"label":"Phone", "scopes":["environment-read","tasks-read"],
                    "pairingTtlSecs":300,"sessionTtlSecs":2_592_000}),
                )
                .await;
            this.update(cx, |page, cx| {
                page.busy = false;
                if page.state.read(cx).engine_generation() != generation
                    || page.pairing_epoch != epoch
                {
                    if let Ok(issued) = result {
                        cx.spawn(async move |_, _| {
                            let _ = engine
                                .client()
                                .call(
                                    methods::REVOKE_REMOTE_PAIRING,
                                    json!({"id":issued.pairing.id}),
                                )
                                .await;
                        })
                        .detach();
                    }
                    cx.notify();
                    return;
                }
                match result {
                    Ok(issued)
                        if page.status["listening"] == true
                            && page.state.read(cx).engine_generation() == generation
                            && page.pairing_epoch == epoch =>
                    {
                        let ticket = PairingTicket {
                            version: 1,
                            host_name: page
                                .state
                                .read(cx)
                                .devices
                                .iter()
                                .find(|d| {
                                    Some(d.id.as_str())
                                        == page.state.read(cx).local_device_id.as_deref()
                                })
                                .map(|d| d.name.clone()),
                            host: HostTicket {
                                version: 1,
                                target: issued.target,
                                endpoints: vec![format!("https://{address}")],
                                spki_sha256: pin,
                            },
                            pairing_id: issued.pairing.id,
                            credential: issued.credential,
                            expires_at_ms: issued.pairing.expires_at_ms,
                            scopes: vec!["environment-read".into(), "tasks-read".into()],
                        };
                        match ticket
                            .encode()
                            .ok()
                            .and_then(|code| qrcode::QrCode::new(code).ok())
                        {
                            Some(qr) => {
                                let svg = qr
                                    .render::<qrcode::render::svg::Color>()
                                    .min_dimensions(256, 256)
                                    .build();
                                page.sheet = Some(PairingSheet {
                                    ticket,
                                    engine: engine.clone(),
                                    qr: Arc::new(Image::from_bytes(
                                        ImageFormat::Svg,
                                        svg.into_bytes(),
                                    )),
                                });
                            }
                            None => {
                                let id = ticket.pairing_id;
                                cx.spawn(async move |_, _| {
                                    let _ = engine
                                        .client()
                                        .call(methods::REVOKE_REMOTE_PAIRING, json!({"id":id}))
                                        .await;
                                })
                                .detach();
                                page.error = Some(
                                    "Could not render pairing code. Select Add to try again."
                                        .into(),
                                )
                            }
                        }
                    }
                    Ok(issued) => {
                        let id = issued.pairing.id;
                        cx.spawn(async move |_, _| {
                            let _ = engine
                                .client()
                                .call(methods::REVOKE_REMOTE_PAIRING, json!({"id":id}))
                                .await;
                        })
                        .detach();
                    }
                    Err(_) => {
                        page.status = Value::Null;
                        cx.emit(HostingChanged { ready: false });
                        page.error = Some(
                            "Could not create a pairing code. Refresh this Mac and try again."
                                .into(),
                        )
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn cancel_pairing(&mut self, cx: &mut Context<Self>) {
        self.pairing_epoch = self.pairing_epoch.wrapping_add(1);
        let Some(sheet) = self.sheet.take() else {
            return;
        };
        cx.spawn(async move |this,cx| {
            let result=sheet.engine.client().call(methods::REVOKE_REMOTE_PAIRING,json!({"id":sheet.ticket.pairing_id})).await;
            this.update(cx, |page,cx| {if result.is_err() {page.error=Some("Could not cancel this code on the Mac. It remains short-lived; disable connections if needed.".into());} cx.notify();}).ok();
        }).detach();
        cx.notify();
    }
    pub fn set_power_status(&mut self, status: SharedString, cx: &mut Context<Self>) {
        if self.power_status != status {
            self.power_status = status;
            cx.notify();
        }
    }
    fn revoke(&mut self, id: String, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::REVOKE_REMOTE_SESSION, json!({"id":id}))
                .await;
            this.update(cx, |page, cx| {
                page.busy = false;
                if result.is_err() {
                    page.error = Some("Could not revoke access. Refresh and try again.".into());
                }
                page.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for ConnectionsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let enabled = self.status["enabled"] == true;
        let ready = self.status["listening"] == true;
        let can_change = !self.busy && self.state.read(cx).engine().is_some();
        let refresh = widgets::ghost_action("connections-refresh", can_change, &theme)
            .aria_label("Refresh connections")
            .child("Refresh")
            .on_click(cx.listener(|page, _, _, cx| page.refresh(cx)))
            .into_any_element();
        let add = widgets::ghost_action(
            "connections-add",
            can_change && ready && self.sheet.is_none(),
            &theme,
        )
        .aria_label("Add phone")
        .child("Add")
        .on_click(cx.listener(|page, _, _, cx| page.add(cx)))
        .into_any_element();
        let mut card=widgets::section_card(&theme)
            .child(widgets::section_header(&theme,"Control this Mac",Some("Pair your phone with this Mac. The Mac runs the work and must stay reachable.".into()),vec![add]))
            .child(widgets::card_row(&theme,false)
                .child(widgets::row_text(&theme,"Allow connections",Some("Turning this off disconnects phones. Work already accepted by this Mac continues.".into())))
                .child(if can_change {
                    widgets::switch_control("connections-allow","Allow connections",enabled,&theme,cx,|page,cx| page.toggle_hosting(cx)).into_any_element()
                } else {widgets::switch_control_disabled("connections-allow","Allow connections",enabled,"Local engine unavailable or change in progress",&theme).into_any_element()}))
            .child(widgets::card_row(&theme,false)
                .child(widgets::row_text(&theme,"Nearby address",Some("Use a reachable LAN interface and a dedicated port. Pairing does not create an Internet route.".into())))
                .child(div().w(gpui::px(260.0)).child(self.bind.clone())))
            .child(widgets::card_row(&theme,false)
                .child(widgets::row_text(&theme,"Host service",Some(if ready {"Ready for nearby connections"} else if enabled {"Enabled, but not ready"} else {"Connections are off"}.into())))
                .child(widgets::badge(&theme,if ready {"Ready"} else {"Offline"})))
            .child(widgets::card_row(&theme,false)
                .child(widgets::row_text(&theme,"Phone connection",Some("Each phone verifies its identity, access and route after pairing. Registry presence does not grant access.".into()))));
        if let Some(error) = self.status["error"].as_str() {
            card = card.child(widgets::error_strip(&theme, error.to_owned()));
        }
        let keep = settings::current(cx).keep_awake_while_hosting;
        card=card.child(widgets::card_row(&theme,false)
            .child(widgets::row_text(&theme,"Keep this Mac awake",Some("Prevent idle sleep while hosting on external power. Display sleep stays available.".into())))
            .child(widgets::switch_control("connections-keep-awake","Keep this Mac awake while hosting",keep,&theme,cx,|page,cx| {
                settings::update(settings::SavePolicy::Immediate,cx,|s| s.keep_awake_while_hosting=!s.keep_awake_while_hosting);
                cx.emit(HostingChanged {ready:page.status["listening"]==true});cx.notify();
            })));
        if keep {
            card = card.child(widgets::row_description(&theme, self.power_status.clone()));
        }
        if let Some(sheet) = &self.sheet {
            let image = sheet.qr.clone();
            let remaining = sheet.ticket.expires_at_ms.saturating_sub(now()) / 1000;
            let code = sheet.ticket.encode().unwrap_or_default();
            card = card.child(
                div()
                    .p(gpui::px(20.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(gpui::px(12.0))
                    .child(
                        img(image)
                            .size(gpui::px(256.0))
                            .aria_label("Private one-use pairing QR"),
                    )
                    .child(format!(
                        "Scan in LUNA on your phone · expires in {}:{:02}",
                        remaining / 60,
                        remaining % 60
                    ))
                    .child(widgets::row_description(
                        &theme,
                        "Access: host information and task reads. No terminal or task changes.",
                    ))
                    .child(
                        div()
                            .flex()
                            .gap(gpui::px(8.0))
                            .child(
                                widgets::ghost_action("connections-copy-code", true, &theme)
                                    .aria_label("Copy pairing code")
                                    .child("Copy pairing code")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            code.clone(),
                                        ))
                                    }),
                            )
                            .child(
                                widgets::ghost_action("connections-cancel-code", true, &theme)
                                    .aria_label("Cancel pairing code")
                                    .child("Cancel")
                                    .on_click(
                                        cx.listener(|page, _, _, cx| page.cancel_pairing(cx)),
                                    ),
                            ),
                    ),
            );
        }
        let mut roster=widgets::section_card(&theme).child(widgets::section_header(&theme,"Paired access",
            Some("These are grants issued by this Mac. Forget on a phone removes its local copy; Revoke here ends its access.".into()),Vec::new()));
        if self.sessions.is_empty() {
            roster = roster.child(widgets::card_row(&theme, false).child(widgets::row_text(
                &theme,
                "No phones paired",
                Some("Enable connections and select Add.".into()),
            )));
        }
        for session in &self.sessions {
            let id = session.id.clone();
            let last = session
                .last_used_at_ms
                .map(|ms| {
                    chrono::DateTime::from_timestamp_millis(ms as i64)
                        .map(|date| {
                            crate::settings::devices::format_last_seen(
                                Some(date),
                                chrono::Utc::now(),
                            )
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_else(|| "Not connected yet".into());
            let expiry = chrono::DateTime::from_timestamp_millis(session.expires_at_ms as i64)
                .map(|d| d.format("%d %b %Y").to_string())
                .unwrap_or_default();
            let unavailable = session.revoked || session.expires_at_ms <= now();
            let description = format!(
                "{} · Last authenticated: {} · Expires {} · {}",
                &id[..id.len().min(8)],
                last,
                expiry,
                session
                    .scopes
                    .iter()
                    .map(|s| match s {
                        luna_rpc::remote_auth::RemoteScope::EnvironmentRead => "Host information",
                        luna_rpc::remote_auth::RemoteScope::TasksRead => "Read tasks",
                        luna_rpc::remote_auth::RemoteScope::FilesRead => "Read files",
                        luna_rpc::remote_auth::RemoteScope::TerminalOperate => "Terminal access",
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            roster = roster.child(
                widgets::card_row(&theme, false)
                    .child(widgets::row_text(
                        &theme,
                        session.client_label.clone(),
                        Some(description.into()),
                    ))
                    .child(if unavailable {
                        widgets::badge(
                            &theme,
                            if session.revoked {
                                "Revoked"
                            } else {
                                "Expired"
                            },
                        )
                        .into_any_element()
                    } else {
                        widgets::ghost_action(
                            format!("connections-revoke-{id}"),
                            can_change,
                            &theme,
                        )
                        .aria_label(SharedString::from(format!(
                            "Revoke access for {}",
                            session.client_label
                        )))
                        .child("Revoke access")
                        .on_click(cx.listener(move |page, _, _, cx| page.revoke(id.clone(), cx)))
                        .into_any_element()
                    }),
            );
        }
        div()
            .id("connections-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(widgets::page_title_bar(
                        &theme,
                        "Connections",
                        None,
                        vec![refresh],
                    ))
                    .when_some(self.error.clone(), |page, error| {
                        page.child(widgets::error_strip(&theme, error))
                    })
                    .when_some(self.message.clone(), |page, message| {
                        page.child(widgets::row_description(&theme, message))
                    })
                    .child(card)
                    .child(roster),
            )
    }
}
fn now() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}
fn lan_bind() -> Option<String> {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut list = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return None;
        }
        let mut cursor = list;
        let mut addresses = Vec::new();
        while !cursor.is_null() {
            let entry = &*cursor;
            if !entry.ifa_addr.is_null()
                && (*entry.ifa_addr).sa_family as i32 == libc::AF_INET
                && entry.ifa_flags as i32 & libc::IFF_LOOPBACK == 0
            {
                let name = std::ffi::CStr::from_ptr(entry.ifa_name).to_string_lossy();
                if name.starts_with("en") {
                    let address = &*(entry.ifa_addr as *const libc::sockaddr_in);
                    let ip = std::net::Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes());
                    if !ip.is_unspecified() && !ip.is_loopback() {
                        addresses.push((name.to_string(), ip));
                    }
                }
            }
            cursor = entry.ifa_next;
        }
        libc::freeifaddrs(list);
        addresses.sort();
        addresses.first().map(|(_, ip)| format!("{ip}:7744"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}
