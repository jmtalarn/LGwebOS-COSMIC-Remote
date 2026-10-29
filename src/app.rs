use std::sync::Arc;

use cosmic::{
    Element, Task, app,
    iced::platform_specific::shell::wayland::commands::popup::{destroy_popup, get_popup},
    iced::{Limits, window::Id},
    widget,
};
use serde_json::json;

use crate::webos::{self, Probe, TvSnapshot, WebOs};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Connection,
    Remote,
    Apps,
    Inputs,
    Keyboard,
    Picture,
}

fn control_button(
    icon: &'static str,
    label: &'static str,
    message: Message,
) -> Element<'static, Message> {
    widget::button::standard(label)
        .leading_icon(widget::icon::from_name(icon))
        .tooltip(label)
        .on_press(message)
        .width(88.0)
        .height(44.0)
        .into()
}

fn centered<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    widget::container(content)
        .center_x(cosmic::iced::Length::Fill)
        .into()
}

fn app_matches_search(title: &str, query: &str) -> bool {
    let query = query.trim();
    query.is_empty() || title.to_lowercase().contains(&query.to_lowercase())
}

#[derive(Default)]
pub struct RemoteApplet {
    core: app::Core,
    popup: Option<Id>,
    host: String,
    status: String,
    error: Option<String>,
    fingerprint: Option<String>,
    pending_probe: Option<Probe>,
    client: Option<Arc<WebOs>>,
    snapshot: TvSnapshot,
    app_filter: String,
    text_entry: String,
    picture_draft: std::collections::BTreeMap<String, u8>,
    tab: Tab,
    pointer_position: Option<cosmic::iced::Point>,
    busy: bool,
    discovering: bool,
    discovered_tvs: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum Message {
    TogglePopup,
    PopupClosed(Id),
    HostChanged(String),
    Connect,
    Discover,
    DiscoveryFinished(Result<Vec<String>, String>),
    SelectTv(String),
    ProbeFinished(Result<Probe, String>),
    ApproveCertificate,
    ConnectionFinished(Result<(Arc<WebOs>, TvSnapshot), String>),
    SnapshotFinished(Result<TvSnapshot, String>),
    ActionFinished(Result<TvSnapshot, String>),
    PairingForgotten(Result<(), String>),
    Press(String),
    Launch(String),
    SwitchInput(String),
    Refresh,
    ForgetPairing,
    AppFilterChanged(String),
    TextEntryChanged(String),
    SendText,
    SendEnter,
    PictureChanged(String, f32),
    ApplyPicture,
    PowerOff,
    PowerFinished(Result<(), String>),
    PointerMoved(cosmic::iced::Point),
    PointerClick,
    PointerExited,
    PointerFinished(Result<(), String>),
    SelectTab(Tab),
}

impl cosmic::Application for RemoteApplet {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "io.github.tvmenubar.LGWebOSRemote";

    fn core(&self) -> &app::Core {
        &self.core
    }
    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn init(core: app::Core, _flags: ()) -> (Self, Task<cosmic::Action<Message>>) {
        (
            Self {
                core,
                status: "Enter your TV address".into(),
                tab: Tab::Connection,
                ..Self::default()
            },
            Task::none(),
        )
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn view(&self) -> Element<'_, Message> {
        self.core
            .applet
            .icon_button_from_handle(widget::icon::from_svg_bytes(include_bytes!(
                "../resources/io.github.tvmenubar.LGWebOSRemote.svg"
            )))
            .on_press(Message::TogglePopup)
            .into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Message> {
        let tab_button = |tab, icon, label| {
            widget::button::standard(label)
                .leading_icon(widget::icon::from_name(icon))
                .class(if self.tab == tab {
                    cosmic::theme::Button::Suggested
                } else {
                    cosmic::theme::Button::Standard
                })
                .on_press(Message::SelectTab(tab))
                .width(100.0)
                .height(48.0)
        };
        let tabs = widget::Column::new()
            .push(centered(
                widget::Row::new()
                    .push(tab_button(Tab::Remote, "input-gaming-symbolic", "Control"))
                    .push(tab_button(Tab::Apps, "view-grid-symbolic", "Apps"))
                    .push(tab_button(Tab::Inputs, "video-display-symbolic", "Inputs"))
                    .spacing(6.0),
            ))
            .push(centered(
                widget::Row::new()
                    .push(tab_button(
                        Tab::Keyboard,
                        "input-keyboard-symbolic",
                        "Pointer",
                    ))
                    .push(tab_button(
                        Tab::Picture,
                        "preferences-desktop-display-symbolic",
                        "Picture",
                    ))
                    .push(tab_button(
                        Tab::Connection,
                        "emblem-system-symbolic",
                        "Setup",
                    ))
                    .spacing(6.0),
            ))
            .spacing(6.0);
        let title = if self.snapshot.name.is_empty() {
            "LG webOS Remote"
        } else {
            self.snapshot.name.as_str()
        };
        let body = match self.tab {
            Tab::Remote => self.remote_tab(),
            Tab::Apps => self.apps_tab(),
            Tab::Inputs => self.inputs_tab(),
            Tab::Keyboard => self.keyboard_tab(),
            Tab::Picture => self.picture_tab(),
            Tab::Connection => self.connection_tab(),
        };
        let mut content = widget::Column::new()
            .push(widget::text(title).size(18))
            .push(widget::text(if self.snapshot.model.is_empty() {
                self.status.as_str()
            } else {
                self.snapshot.model.as_str()
            }))
            .push(tabs)
            .push(widget::scrollable(body).height(cosmic::iced::Length::Fixed(500.0)))
            .spacing(10)
            .padding(12);
        if let Some(error) = &self.error {
            content = content.push(widget::text(error.as_str()));
        }
        self.core.applet.popup_container(content).into()
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::TogglePopup => {
                if let Some(id) = self.popup.take() {
                    return destroy_popup(id);
                }
                let id = Id::unique();
                self.popup = Some(id);
                let mut settings = self.core.applet.get_popup_settings(
                    self.core.main_window_id().unwrap(),
                    id,
                    None,
                    None,
                    None,
                );
                settings.positioner.size_limits = Limits::NONE
                    .max_width(400.0)
                    .min_width(320.0)
                    .min_height(220.0)
                    .max_height(720.0);
                return get_popup(settings);
            }
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
            }
            Message::SelectTab(tab) => self.tab = tab,
            Message::HostChanged(host) => self.host = host,
            Message::Connect => {
                let host = self.host.trim().to_owned();
                return self.start_probe(host);
            }
            Message::Discover => {
                if self.discovering {
                    return Task::none();
                }
                self.discovering = true;
                self.error = None;
                return Task::perform(
                    async {
                        webos::discover_tvs()
                            .await
                            .map_err(|error| error.to_string())
                    },
                    |result| cosmic::Action::App(Message::DiscoveryFinished(result)),
                );
            }
            Message::DiscoveryFinished(Ok(hosts)) => {
                self.discovering = false;
                self.discovered_tvs = hosts;
                self.status = if self.discovered_tvs.is_empty() {
                    "No LG TVs found; enter the address manually".into()
                } else {
                    format!("Found {} LG TV(s)", self.discovered_tvs.len())
                };
            }
            Message::DiscoveryFinished(Err(error)) => {
                self.discovering = false;
                self.error = Some(error);
            }
            Message::SelectTv(host) => {
                self.host = host.clone();
                return self.start_probe(host);
            }
            Message::ProbeFinished(Err(error)) => {
                self.busy = false;
                self.status = "Could not connect".into();
                self.error = Some(error);
            }
            Message::ProbeFinished(Ok(probe)) => {
                if probe.stored_pin.is_none() {
                    self.fingerprint = Some(probe.fingerprint.clone());
                    self.pending_probe = Some(probe);
                    self.status = "Review the TV certificate".into();
                    self.busy = false;
                } else {
                    return self.start_connection(probe);
                }
            }
            Message::ApproveCertificate => {
                if let Some(probe) = self.pending_probe.take() {
                    self.fingerprint = None;
                    return self.start_connection(probe);
                }
            }
            Message::ConnectionFinished(Err(error)) => {
                self.busy = false;
                self.status = "Pairing or connection failed".into();
                self.error = Some(error);
            }
            Message::ConnectionFinished(Ok((client, snapshot))) => {
                self.busy = false;
                self.status = "Connected".into();
                self.error = None;
                self.client = Some(client);
                self.snapshot = snapshot;
                self.picture_draft = self.snapshot.picture.clone();
                self.pointer_position = None;
                self.tab = Tab::Remote;
            }
            Message::Press(button) => {
                if let Some(client) = self.client.clone() {
                    let (uri, payload) = match button.as_str() {
                        "VOLUME_UP" => ("audio/volumeUp", json!({})),
                        "VOLUME_DOWN" => ("audio/volumeDown", json!({})),
                        "MUTE" => ("audio/setMute", json!({"mute": !self.snapshot.muted})),
                        "MEDIA_REWIND" => ("media.controls/rewind", json!({})),
                        "MEDIA_PLAY_PAUSE" => ("media.controls/playPause", json!({})),
                        "MEDIA_FAST_FORWARD" => ("media.controls/fastForward", json!({})),
                        "CHANNELUP" | "CHANNELDOWN" => (
                            "com.webos.service.networkinput/sendButton",
                            json!({"button": button}),
                        ),
                        _ => (
                            "com.webos.service.networkinput/sendButton",
                            json!({"button": button}),
                        ),
                    };
                    return Self::request_task(client, uri, payload);
                }
            }
            Message::Launch(id) => {
                if let Some(client) = self.client.clone() {
                    return Self::request_task(client, "system.launcher/launch", json!({"id": id}));
                }
            }
            Message::SwitchInput(id) => {
                if let Some(client) = self.client.clone() {
                    return Self::request_task(client, "tv/switchInput", json!({"inputId": id}));
                }
            }
            Message::Refresh => {
                if let Some(client) = self.client.clone() {
                    return Task::perform(
                        async move { client.snapshot().await.map_err(|error| error.to_string()) },
                        |result| cosmic::Action::App(Message::SnapshotFinished(result)),
                    );
                }
            }
            Message::ForgetPairing => {
                let host = self.host.trim().to_owned();
                self.client = None;
                self.snapshot = TvSnapshot::default();
                self.status = "Removing saved TV credentials...".into();
                self.busy = true;
                return Task::perform(
                    async move {
                        webos::forget_pairing(&host)
                            .await
                            .map_err(|error| error.to_string())
                    },
                    |result| cosmic::Action::App(Message::PairingForgotten(result)),
                );
            }
            Message::AppFilterChanged(value) => self.app_filter = value,
            Message::TextEntryChanged(value) => self.text_entry = value,
            Message::SendText => {
                let text = std::mem::take(&mut self.text_entry);
                if !text.is_empty()
                    && let Some(client) = self.client.clone()
                {
                    return Self::request_task(
                        client,
                        "com.webos.service.ime/insertText",
                        json!({"text": text, "replace": 0}),
                    );
                }
            }
            Message::SendEnter => {
                if let Some(client) = self.client.clone() {
                    return Self::request_task(
                        client,
                        "com.webos.service.ime/sendEnterKey",
                        json!({}),
                    );
                }
            }
            Message::PictureChanged(key, value) => {
                self.picture_draft
                    .insert(key, value.round().clamp(0.0, 100.0) as u8);
            }
            Message::ApplyPicture => {
                if let Some(client) = self.client.clone() {
                    let settings = self.picture_draft.clone();
                    return Task::perform(
                        async move {
                            client
                                .request(
                                    "settings/setSystemSettings",
                                    json!({"category": "picture", "settings": settings}),
                                )
                                .await
                                .map_err(|error| error.to_string())?;
                            client.snapshot().await.map_err(|error| error.to_string())
                        },
                        |result| cosmic::Action::App(Message::ActionFinished(result)),
                    );
                }
            }
            Message::PowerOff => {
                if let Some(client) = self.client.clone() {
                    self.busy = true;
                    return Task::perform(
                        async move {
                            client
                                .request("system/turnOff", json!({}))
                                .await
                                .map(|_| ())
                                .map_err(|error| error.to_string())
                        },
                        |result| cosmic::Action::App(Message::PowerFinished(result)),
                    );
                }
            }
            Message::PowerFinished(Err(error)) => {
                self.busy = false;
                self.error = Some(error);
            }
            Message::PowerFinished(Ok(())) => {
                self.busy = false;
                self.client = None;
                self.snapshot = TvSnapshot::default();
                self.picture_draft.clear();
                self.status = "TV turned off".into();
            }
            Message::PointerMoved(position) => {
                if let Some(previous) = self.pointer_position.replace(position) {
                    let dx = ((position.x - previous.x) * 1.5).round() as i32;
                    let dy = ((position.y - previous.y) * 1.5).round() as i32;
                    if (dx != 0 || dy != 0)
                        && let Some(client) = self.client.clone()
                    {
                        return Task::perform(
                            async move {
                                client
                                    .pointer_move(dx, dy)
                                    .await
                                    .map_err(|error| error.to_string())
                            },
                            |result| cosmic::Action::App(Message::PointerFinished(result)),
                        );
                    }
                }
            }
            Message::PointerClick => {
                if let Some(client) = self.client.clone() {
                    return Task::perform(
                        async move {
                            client
                                .pointer_click()
                                .await
                                .map_err(|error| error.to_string())
                        },
                        |result| cosmic::Action::App(Message::PointerFinished(result)),
                    );
                }
            }
            Message::PointerExited => self.pointer_position = None,
            Message::PointerFinished(Err(error)) => self.error = Some(error),
            Message::PointerFinished(Ok(())) => self.error = None,
            Message::SnapshotFinished(Ok(snapshot)) => {
                self.picture_draft = snapshot.picture.clone();
                self.snapshot = snapshot;
            }
            Message::SnapshotFinished(Err(error)) => self.error = Some(error),
            Message::ActionFinished(Err(error)) => self.error = Some(error),
            Message::ActionFinished(Ok(snapshot)) => {
                self.picture_draft = snapshot.picture.clone();
                self.snapshot = snapshot;
                self.error = None;
            }
            Message::PairingForgotten(Err(error)) => {
                self.busy = false;
                self.error = Some(error);
            }
            Message::PairingForgotten(Ok(())) => {
                self.busy = false;
                self.status = "Saved pairing removed".into();
                self.error = None;
            }
        }
        Task::none()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

impl RemoteApplet {
    fn remote_tab(&self) -> Element<'_, Message> {
        if self.client.is_none() {
            return widget::Column::new()
                .push(widget::text("Connect to your TV to use the remote."))
                .push(
                    widget::button::standard("Open connection settings")
                        .leading_icon(widget::icon::from_name("network-wired-symbolic"))
                        .on_press(Message::SelectTab(Tab::Connection)),
                )
                .spacing(12)
                .padding(8)
                .into();
        }

        let volume = self.snapshot.volume.map_or_else(
            || "Volume unavailable".to_owned(),
            |value| format!("Volume · {value}%"),
        );
        let content = widget::Column::new()
            .push(centered(
                widget::button::destructive("Turn TV off")
                    .leading_icon(widget::icon::from_name("system-shutdown-symbolic"))
                    .on_press(Message::PowerOff),
            ))
            .push(widget::text(volume).size(16))
            .push(centered(
                widget::Row::new()
                    .push(control_button(
                        "audio-volume-low-symbolic",
                        "Vol −",
                        Message::Press("VOLUME_DOWN".into()),
                    ))
                    .push(control_button(
                        if self.snapshot.muted {
                            "audio-volume-muted-symbolic"
                        } else {
                            "audio-volume-high-symbolic"
                        },
                        if self.snapshot.muted {
                            "Unmute"
                        } else {
                            "Mute"
                        },
                        Message::Press("MUTE".into()),
                    ))
                    .push(control_button(
                        "audio-volume-high-symbolic",
                        "Vol +",
                        Message::Press("VOLUME_UP".into()),
                    ))
                    .spacing(6.0),
            ))
            .push(centered(
                widget::Row::new()
                    .push(control_button(
                        "go-home-symbolic",
                        "Home",
                        Message::Press("HOME".into()),
                    ))
                    .push(control_button(
                        "go-previous-symbolic",
                        "Back",
                        Message::Press("BACK".into()),
                    ))
                    .spacing(6.0),
            ))
            .push(
                widget::Column::new()
                    .push(
                        widget::Row::new()
                            .push(control_button(
                                "go-up-symbolic",
                                "Up",
                                Message::Press("UP".into()),
                            ))
                            .width(cosmic::iced::Length::Shrink),
                    )
                    .push(
                        widget::Row::new()
                            .push(control_button(
                                "go-previous-symbolic",
                                "Left",
                                Message::Press("LEFT".into()),
                            ))
                            .push(control_button(
                                "emblem-ok-symbolic",
                                "OK",
                                Message::Press("ENTER".into()),
                            ))
                            .push(control_button(
                                "go-next-symbolic",
                                "Right",
                                Message::Press("RIGHT".into()),
                            ))
                            .spacing(6.0),
                    )
                    .push(
                        widget::Row::new()
                            .push(control_button(
                                "go-down-symbolic",
                                "Down",
                                Message::Press("DOWN".into()),
                            ))
                            .width(cosmic::iced::Length::Shrink),
                    )
                    .spacing(6.0)
                    .width(cosmic::iced::Length::Fill)
                    .align_x(cosmic::iced::Alignment::Center),
            )
            .push(centered(
                widget::Row::new()
                    .push(control_button(
                        "media-skip-backward-symbolic",
                        "Ch −",
                        Message::Press("CHANNELDOWN".into()),
                    ))
                    .push(control_button(
                        "media-playback-start-symbolic",
                        "Play",
                        Message::Press("MEDIA_PLAY_PAUSE".into()),
                    ))
                    .push(control_button(
                        "media-skip-forward-symbolic",
                        "Ch +",
                        Message::Press("CHANNELUP".into()),
                    ))
                    .spacing(6.0),
            ))
            .spacing(16)
            .padding(8);
        content.into()
    }

    fn apps_tab(&self) -> Element<'_, Message> {
        let matching: Vec<_> = self
            .snapshot
            .apps
            .iter()
            .filter(|app| app_matches_search(&app.title, &self.app_filter))
            .collect();
        let mut content = widget::Column::new()
            .push(
                widget::text_input("Search installed apps", &self.app_filter)
                    .on_input(Message::AppFilterChanged),
            )
            .push(
                widget::button::standard("Refresh apps")
                    .leading_icon(widget::icon::from_name("view-refresh-symbolic"))
                    .width(cosmic::iced::Length::Fill)
                    .on_press(Message::Refresh),
            )
            .spacing(8)
            .padding(8);

        if self.client.is_none() {
            content = content.push(widget::text("Connect to your TV to load its apps."));
        } else if matching.is_empty() {
            if let Some(error) = &self.snapshot.apps_error {
                content = content.push(widget::text(error.as_str()));
            } else {
                content = content.push(widget::text(if self.app_filter.trim().is_empty() {
                    "No launchable apps were reported by this TV."
                } else {
                    "No apps match this search."
                }));
            }
        } else {
            for app in matching {
                content = content.push(
                    widget::button::standard(app.title.as_str())
                        .leading_icon(widget::icon::from_name("application-x-executable-symbolic"))
                        .width(cosmic::iced::Length::Fill)
                        .on_press(Message::Launch(app.id.clone())),
                );
            }
        }
        content.into()
    }

    fn inputs_tab(&self) -> Element<'_, Message> {
        let mut content = widget::Column::new().spacing(10.0).padding(8);
        if self.client.is_none() {
            content = content.push(widget::text("Connect to your TV to see its inputs."));
        } else if self.snapshot.inputs.is_empty() {
            content = content.push(widget::text(
                self.snapshot
                    .inputs_error
                    .as_deref()
                    .unwrap_or("The TV did not report any inputs."),
            ));
        } else {
            for input in &self.snapshot.inputs {
                let label = if input.connected {
                    format!("{} · Active", input.label)
                } else {
                    input.label.clone()
                };
                content = content.push(
                    widget::button::standard(label)
                        .leading_icon(widget::icon::from_name("video-display-symbolic"))
                        .width(cosmic::iced::Length::Fill)
                        .on_press(Message::SwitchInput(input.id.clone())),
                );
            }
        }
        content.into()
    }

    fn keyboard_tab(&self) -> Element<'_, Message> {
        if self.client.is_none() {
            return widget::Column::new()
                .push(widget::text(
                    "Connect to your TV to use pointer and text input.",
                ))
                .padding(8)
                .into();
        }
        let pointer_surface = widget::mouse_area(
            widget::container(
                widget::Column::new()
                    .push(widget::icon::from_name("input-mouse-symbolic").size(34))
                    .push(widget::text("Move pointer here, click to select"))
                    .spacing(12.0)
                    .align_x(cosmic::iced::Alignment::Center),
            )
            .center(cosmic::iced::Length::Fill)
            .width(cosmic::iced::Length::Fill)
            .height(220.0)
            .class(cosmic::theme::Container::Card),
        )
        .on_move(Message::PointerMoved)
        .on_press(Message::PointerClick)
        .on_exit(Message::PointerExited);

        widget::Column::new()
            .push(pointer_surface)
            .push(widget::text("Keyboard").size(16))
            .push(
                widget::Row::new()
                    .push(
                        widget::text_input("Type into the open TV field", &self.text_entry)
                            .on_input(Message::TextEntryChanged)
                            .on_submit(|_| Message::SendText),
                    )
                    .push(control_button(
                        "mail-send-symbolic",
                        "Send",
                        Message::SendText,
                    ))
                    .push(control_button(
                        "input-keyboard-symbolic",
                        "Enter",
                        Message::SendEnter,
                    ))
                    .spacing(6.0),
            )
            .spacing(12.0)
            .padding(8)
            .into()
    }

    fn picture_tab(&self) -> Element<'_, Message> {
        let mut content = widget::Column::new()
            .push(widget::text("Picture settings").size(16))
            .spacing(14.0)
            .padding(8);
        if self.client.is_none() {
            content = content.push(widget::text("Connect to your TV to read picture settings."));
        } else if self.snapshot.picture.is_empty() {
            content = content.push(widget::text("This TV did not expose picture adjustments."));
        } else {
            for (key, title) in [
                ("backlight", "Backlight"),
                ("brightness", "Brightness"),
                ("contrast", "Contrast"),
                ("color", "Color"),
            ] {
                if let Some(value) = self
                    .picture_draft
                    .get(key)
                    .or_else(|| self.snapshot.picture.get(key))
                {
                    content = content.push(
                        widget::Column::new()
                            .push(
                                widget::Row::new()
                                    .push(widget::text(title).width(cosmic::iced::Length::Fill))
                                    .push(widget::text(value.to_string())),
                            )
                            .push(widget::slider(
                                0.0..=100.0,
                                f32::from(*value),
                                move |value| Message::PictureChanged(key.into(), value),
                            )),
                    );
                }
            }
            content = content.push(
                widget::button::suggested("Apply picture settings").on_press(Message::ApplyPicture),
            );
        }
        content.into()
    }

    fn connection_tab(&self) -> Element<'_, Message> {
        let mut content = widget::Column::new()
            .push(widget::text("TV connection").size(16))
            .push(
                widget::Row::new()
                    .push(
                        widget::text_input("TV IPv4 address", &self.host)
                            .on_input(Message::HostChanged),
                    )
                    .push(
                        widget::button::suggested(if self.busy { "Working…" } else { "Connect" })
                            .on_press(Message::Connect),
                    )
                    .spacing(6.0),
            )
            .push(
                widget::button::standard(if self.discovering {
                    "Searching for TVs…"
                } else {
                    "Find TVs on this network"
                })
                .leading_icon(widget::icon::from_name("network-wireless-symbolic"))
                .width(cosmic::iced::Length::Fill)
                .on_press(Message::Discover),
            )
            .push(
                widget::button::standard("Refresh apps and TV controls")
                    .leading_icon(widget::icon::from_name("view-refresh-symbolic"))
                    .width(cosmic::iced::Length::Fill)
                    .on_press(Message::Refresh),
            )
            .spacing(8)
            .padding(8);

        for host in &self.discovered_tvs {
            content = content.push(
                widget::button::standard(host.as_str())
                    .leading_icon(widget::icon::from_name("video-display-symbolic"))
                    .width(cosmic::iced::Length::Fill)
                    .on_press(Message::SelectTv(host.clone())),
            );
        }
        if let Some(fingerprint) = &self.fingerprint {
            content = content
                .push(widget::text("Verify this TV certificate before pairing."))
                .push(widget::text(fingerprint.as_str()).size(11))
                .push(
                    widget::button::suggested("Trust certificate and pair")
                        .on_press(Message::ApproveCertificate),
                );
        }
        if !self.host.is_empty() {
            content = content.push(
                widget::button::destructive("Forget saved pairing")
                    .leading_icon(widget::icon::from_name("edit-delete-symbolic"))
                    .on_press(Message::ForgetPairing),
            );
        }
        content.into()
    }

    fn start_probe(&mut self, host: String) -> Task<cosmic::Action<Message>> {
        if !webos::valid_local_host(&host) {
            self.error = Some("Enter the TV's private IPv4 address.".into());
            return Task::none();
        }
        self.error = None;
        self.fingerprint = None;
        self.pending_probe = None;
        self.status = "Checking TV certificate...".into();
        self.busy = true;
        Task::perform(
            async move { webos::probe(&host).await.map_err(|error| error.to_string()) },
            |result| cosmic::Action::App(Message::ProbeFinished(result)),
        )
    }

    fn start_connection(&mut self, probe: Probe) -> Task<cosmic::Action<Message>> {
        let host = self.host.trim().to_owned();
        self.status = "Pairing... accept the request on your TV".into();
        self.busy = true;
        Task::perform(
            async move {
                webos::connect(&host, probe)
                    .await
                    .map_err(|error| error.to_string())
            },
            |result| cosmic::Action::App(Message::ConnectionFinished(result)),
        )
    }

    fn request_task(
        client: Arc<WebOs>,
        uri: &'static str,
        payload: serde_json::Value,
    ) -> Task<cosmic::Action<Message>> {
        Task::perform(
            async move {
                client
                    .request(uri, payload)
                    .await
                    .map_err(|error| error.to_string())?;
                client.snapshot().await.map_err(|error| error.to_string())
            },
            |result| cosmic::Action::App(Message::ActionFinished(result)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::app_matches_search;

    #[test]
    fn app_search_is_case_insensitive_and_trims_whitespace() {
        assert!(app_matches_search("Netflix", "  NET  "));
        assert!(app_matches_search("Netflix", ""));
        assert!(!app_matches_search("Netflix", "youtube"));
    }
}
