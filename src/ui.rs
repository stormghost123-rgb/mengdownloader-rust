//! Native window. The layout follows the previous download manager: slate canvas,
//! left navigation, rounded cards, and indigo actions.

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use egui::{Color32, Frame, Margin, RichText, Stroke};
use fastframe_shell::{Closed, Headless, Held, Resident, Waker};
use fastframe_tray::{Config, Event, MenuItem, Tray};

use crate::cmd::Cmd;
use crate::engine;
use crate::model::{
    clock, human_bytes, human_duration, human_eta, human_speed, LiveRecording, LiveRoom, ParseView,
    RecordingStatus, RoomStatus, Settings, Snapshot, Task, TaskStatus, UpdatePhase,
};
use crate::platform;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Tasks,
    Live,
    History,
    Dashboard,
    Settings,
}

impl Page {
    const ALL: [Self; 6] = [
        Self::Home,
        Self::Tasks,
        Self::Live,
        Self::History,
        Self::Dashboard,
        Self::Settings,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Home => "首页",
            Self::Tasks => "下载任务",
            Self::Live => "直播录制",
            Self::History => "历史记录",
            Self::Dashboard => "数据统计",
            Self::Settings => "设置",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskFilter {
    All,
    Status(TaskStatus),
}

impl TaskFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Status(TaskStatus::Downloading) => "下载中",
            Self::Status(TaskStatus::Waiting) => "等待中",
            Self::Status(TaskStatus::Paused) => "已暂停",
            Self::Status(TaskStatus::Completed) => "已完成",
            Self::Status(TaskStatus::Failed) => "失败",
            Self::Status(TaskStatus::Cancelled) => "已取消",
            Self::Status(TaskStatus::Parsing) => "解析中",
        }
    }

    fn matches(self, status: TaskStatus) -> bool {
        match self {
            Self::All => true,
            Self::Status(want) => status == want,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HistoryTab {
    Download,
    Live,
}

#[derive(Clone, Copy)]
struct Ink {
    dark: bool,
    canvas: Color32,
    panel: Color32,
    card: Color32,
    field: Color32,
    hover: Color32,
    line: Color32,
    text: Color32,
    muted: Color32,
    brand: Color32,
    brand_text: Color32,
    brand_soft: Color32,
    glow: Color32,
    rose: Color32,
    rose_soft: Color32,
    emerald: Color32,
    emerald_soft: Color32,
    amber: Color32,
    amber_soft: Color32,
    blue: Color32,
    blue_soft: Color32,
    slate_soft: Color32,
}

impl Ink {
    fn of(ui: &egui::Ui) -> Self {
        Self::from_dark(ui.visuals().dark_mode)
    }

    fn from_dark(dark: bool) -> Self {
        if dark {
            Self {
                dark,
                canvas: Color32::from_rgb(2, 6, 23),
                panel: Color32::from_rgb(15, 23, 42),
                card: Color32::from_rgb(15, 23, 42),
                field: Color32::from_rgb(30, 41, 59),
                hover: Color32::from_rgb(30, 41, 59),
                line: Color32::from_rgb(30, 41, 59),
                text: Color32::from_rgb(241, 245, 249),
                muted: Color32::from_rgb(148, 163, 184),
                brand: Color32::from_rgb(129, 140, 248),
                brand_text: Color32::from_rgb(199, 210, 254),
                brand_soft: Color32::from_rgba_unmultiplied(99, 102, 241, 42),
                glow: Color32::from_rgba_unmultiplied(99, 102, 241, 36),
                rose: Color32::from_rgb(251, 113, 133),
                rose_soft: Color32::from_rgba_unmultiplied(244, 63, 94, 40),
                emerald: Color32::from_rgb(52, 211, 153),
                emerald_soft: Color32::from_rgba_unmultiplied(16, 185, 129, 40),
                amber: Color32::from_rgb(251, 191, 36),
                amber_soft: Color32::from_rgba_unmultiplied(245, 158, 11, 40),
                blue: Color32::from_rgb(96, 165, 250),
                blue_soft: Color32::from_rgba_unmultiplied(59, 130, 246, 40),
                slate_soft: Color32::from_rgba_unmultiplied(148, 163, 184, 32),
            }
        } else {
            Self {
                dark,
                canvas: Color32::from_rgb(248, 250, 252),
                panel: Color32::WHITE,
                card: Color32::WHITE,
                field: Color32::from_rgb(248, 250, 252),
                hover: Color32::from_rgb(241, 245, 249),
                line: Color32::from_rgb(226, 232, 240),
                text: Color32::from_rgb(15, 23, 42),
                muted: Color32::from_rgb(100, 116, 139),
                brand: Color32::from_rgb(79, 70, 229),
                brand_text: Color32::from_rgb(67, 56, 202),
                brand_soft: Color32::from_rgb(238, 242, 255),
                glow: Color32::from_rgba_unmultiplied(99, 102, 241, 28),
                rose: Color32::from_rgb(225, 29, 72),
                rose_soft: Color32::from_rgb(255, 228, 230),
                emerald: Color32::from_rgb(5, 150, 105),
                emerald_soft: Color32::from_rgb(209, 250, 229),
                amber: Color32::from_rgb(180, 83, 9),
                amber_soft: Color32::from_rgb(254, 243, 199),
                blue: Color32::from_rgb(37, 99, 235),
                blue_soft: Color32::from_rgb(219, 234, 254),
                slate_soft: Color32::from_rgb(241, 245, 249),
            }
        }
    }
}

pub struct MengApp {
    tx: std::sync::mpsc::Sender<Cmd>,
    snap: Arc<Mutex<Snapshot>>,
    supervisor: Option<JoinHandle<()>>,
    tray: Option<Tray>,
    quit: bool,
    wants_show: bool,
    tray_ok: bool,
    page: Page,
    url: String,
    format_id: String,
    container: String,
    live_url: String,
    live_quality: String,
    live_format: String,
    history_query: String,
    history_tab: HistoryTab,
    task_filter: TaskFilter,
    settings_draft: Option<Settings>,
}

impl MengApp {
    pub fn new(waker: Waker) -> Self {
        let (tx, snap, supervisor) = engine::start(waker.clone());
        let wake = {
            let waker = waker.clone();
            move || waker.wake()
        };
        let tray = Tray::spawn(
            Config {
                id: "mengdownloader",
                title: "萌下载".into(),
                icon: app_icon_rgba,
                template_icon: None,
                themed_icon: false,
                menu_on_click: false,
                menu: vec![
                    MenuItem::action("show", "显示窗口"),
                    MenuItem::Separator,
                    MenuItem::action("quit", "退出"),
                ],
            },
            wake,
        );
        let tray_ok = tray.as_ref().is_some_and(Tray::is_shown);
        Self {
            tx,
            snap,
            supervisor: Some(supervisor),
            tray,
            quit: false,
            wants_show: false,
            tray_ok,
            page: Page::Home,
            url: String::new(),
            format_id: String::new(),
            container: "mp4".into(),
            live_url: String::new(),
            live_quality: "OD".into(),
            live_format: "mkv".into(),
            history_query: String::new(),
            history_tab: HistoryTab::Download,
            task_filter: TaskFilter::All,
            settings_draft: None,
        }
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    fn read_snap(&self) -> Snapshot {
        self.snap.lock().unwrap_or_else(|err| err.into_inner()).clone()
    }

    fn poll_tray(&mut self) {
        let Some(tray) = &self.tray else { return };
        for event in tray.events() {
            match event {
                Event::Show | Event::Toggle | Event::Menu("show") => self.wants_show = true,
                Event::Menu("quit") => self.quit = true,
                Event::Menu(_) => {}
            }
        }
    }

    fn hide_on_close(&self) -> bool {
        self.tray_ok
            && self
                .snap
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .settings
                .close_to_tray
    }

    fn frame(&mut self, ui: &mut egui::Ui) {
        self.poll_tray();
        let ctx = ui.ctx().clone();
        if self.quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.wants_show {
            self.wants_show = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        let snap = self.read_snap();
        if snap.request_quit {
            self.quit = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let dark = apply_theme(&ctx, &snap.settings.theme);
        let ink = Ink::from_dark(dark);
        if !snap.ready {
            self.boot_screen(ui, &ink, "正在启动…", None);
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        if !snap.boot_error.is_empty() {
            self.boot_screen(ui, &ink, "萌下载没有启动起来", Some(snap.boot_error.clone()));
            return;
        }
        if self.page == Page::Settings && self.settings_draft.is_none() {
            self.settings_draft = Some(snap.settings.clone());
        }
        self.sync_format(&snap);

        egui::Panel::left("meng-sidebar")
            .exact_size(248.0)
            .resizable(false)
            .show_separator_line(true)
            .frame(Frame::NONE.fill(ink.panel).inner_margin(Margin::symmetric(14, 16)))
            .show(ui, |ui| self.sidebar(ui, &ink));
        egui::Panel::top("meng-header")
            .show_separator_line(true)
            .frame(Frame::NONE.fill(ink.panel).inner_margin(Margin::symmetric(20, 12)))
            .show(ui, |ui| self.header(ui, &ink, &snap));
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(ink.canvas).inner_margin(Margin::symmetric(22, 16)))
            .show(ui, |ui| {
                if !snap.notice.is_empty() {
                    self.notice_bar(ui, &ink, &snap);
                    ui.add_space(12.0);
                }
                if self.update_bar(ui, &ink, &snap) {
                    ui.add_space(12.0);
                }
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    match self.page {
                        Page::Home => self.page_home(ui, &snap),
                        Page::Tasks => self.page_tasks(ui, &snap),
                        Page::Live => self.page_live(ui, &snap),
                        Page::History => self.page_history(ui, &snap),
                        Page::Dashboard => page_dashboard(ui, &snap),
                        Page::Settings => self.page_settings(ui, &snap),
                    }
                });
            });
        if snap.busy
            || matches!(snap.parse, ParseView::Busy)
            || matches!(snap.update.phase, UpdatePhase::Checking | UpdatePhase::Downloading)
        {
            ctx.request_repaint_after(Duration::from_millis(400));
        }
    }

    fn boot_screen(&self, ui: &mut egui::Ui, ink: &Ink, title: &str, detail: Option<String>) {
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(ink.canvas))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(120.0);
                    card(ink).show(ui, |ui| {
                        ui.set_min_width(360.0);
                        ui.label(RichText::new(title).size(20.0).strong().color(ink.text));
                        if let Some(detail) = detail {
                            ui.add_space(8.0);
                            ui.label(RichText::new(detail).color(ink.rose));
                        }
                    });
                });
            });
    }

    fn sync_format(&mut self, snap: &Snapshot) {
        let Some(parsed) = &snap.parsed else { return };
        if parsed.formats.iter().any(|fmt| fmt.id == self.format_id) {
            return;
        }
        if let Some(first) = parsed.formats.first() {
            self.format_id = first.id.clone();
            if !first.ext.is_empty() {
                self.container = first.ext.clone();
            }
        }
    }

    fn notice_bar(&mut self, ui: &mut egui::Ui, ink: &Ink, snap: &Snapshot) {
        let (fill, color) = if snap.notice_error {
            (ink.rose_soft, ink.rose)
        } else {
            (ink.emerald_soft, ink.emerald)
        };
        Frame::NONE
            .fill(fill)
            .stroke(Stroke::new(1.0, color.gamma_multiply(0.35)))
            .corner_radius(12.0)
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&snap.notice).color(color));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ghost(ui, ink, "知道了").clicked() {
                            self.send(Cmd::DismissNotice);
                        }
                    });
                });
            });
    }

    fn update_bar(&mut self, ui: &mut egui::Ui, ink: &Ink, snap: &Snapshot) -> bool {
        let downloading = snap.update.phase == UpdatePhase::Downloading;
        let available = snap.update.phase == UpdatePhase::Available && !snap.update.banner_dismissed;
        if !downloading && !available {
            return false;
        }
        Frame::NONE
            .fill(ink.brand_soft)
            .stroke(Stroke::new(1.0, ink.brand.gamma_multiply(0.35)))
            .corner_radius(12.0)
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                stretch(ui);
                ui.horizontal(|ui| {
                    let text = if downloading {
                        snap.update.message.clone()
                    } else {
                        format!("发现新版本 {}，下载后会自动重启。", snap.update.version)
                    };
                    ui.label(RichText::new(text).color(ink.brand_text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if available && ghost(ui, ink, "稍后").clicked() {
                            self.send(Cmd::DismissUpdate);
                        }
                        if available && primary(ui, ink, "下载并重启").clicked() {
                            self.send(Cmd::ApplyUpdate);
                        }
                    });
                });
            });
        true
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, ink: &Ink) {
        ui.horizontal(|ui| {
            mark(ui, ink.brand, 36.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("萌下载").strong().size(15.0).color(ink.text));
                ui.label(RichText::new("视频下载管理器").size(11.0).color(ink.muted));
            });
        });
        ui.add_space(18.0);
        for page in Page::ALL {
            if nav_item(ui, ink, page, self.page == page) {
                self.page = page;
            }
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).size(11.0).color(ink.muted));
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                let dot = if self.tray_ok { ink.emerald } else { ink.amber };
                ui.painter().circle_filled(rect.center(), 4.0, dot);
                ui.label(RichText::new(if self.tray_ok { "托盘运行中" } else { "窗口模式" }).size(12.0).color(ink.muted));
            });
            ui.add_space(8.0);
            ui.separator();
        });
    }

    fn header(&mut self, ui: &mut egui::Ui, ink: &Ink, snap: &Snapshot) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.page.title()).size(18.0).strong().color(ink.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ghost(ui, ink, if ink.dark { "浅色" } else { "深色" }).clicked() {
                    self.flip_theme(snap, ink.dark);
                }
                let port = if snap.api_port == 0 {
                    "接口未开".to_string()
                } else {
                    format!("端口 {}", snap.api_port)
                };
                badge(ui, ink.slate_soft, ink.muted, &port);
                badge(ui, ink.emerald_soft, ink.emerald, "● 在线");
            });
        });
    }

    fn page_home(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let ink = Ink::of(ui);
        card(&ink).show(ui, |ui| {
            let glow_at = ui.max_rect().center_top() + egui::vec2(0.0, 36.0);
            ui.painter().circle_filled(glow_at, 72.0, ink.glow);
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                mark(ui, ink.brand, 56.0);
                ui.add_space(10.0);
                ui.label(RichText::new("在线视频下载管理器").size(28.0).strong().color(ink.text));
                ui.label(RichText::new("下载视频  ·  录制直播  ·  统一管理").color(ink.muted));
                ui.add_space(8.0);
                if rose_button(ui, &ink, "去直播录制（抖音 / 快手 / B站）").clicked() {
                    self.page = Page::Live;
                }
                ui.add_space(14.0);
            });
            Frame::NONE
                .fill(ink.field)
                .stroke(Stroke::new(1.0, ink.line))
                .corner_radius(14.0)
                .inner_margin(Margin::same(8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let width = (ui.available_width() - 196.0).max(80.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.url)
                                .desired_width(width)
                                .hint_text("粘贴视频链接，或一段分享文字")
                                .frame(Frame::NONE)
                                .margin(Margin::symmetric(10, 8)),
                        );
                        if ghost(ui, &ink, "粘贴").clicked() {
                            self.paste_into_url();
                        }
                        let busy = matches!(snap.parse, ParseView::Busy);
                        let label = if busy { "解析中…" } else { "解析视频" };
                        if primary(ui, &ink, label).clicked() && !busy {
                            self.begin_parse();
                        }
                    });
                });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("支持").size(12.0).color(ink.muted));
                for name in ["YouTube", "Bilibili", "抖音", "快手", "Vimeo", "X", "TikTok", "Instagram"] {
                    badge(ui, ink.slate_soft, ink.muted, name);
                }
                if ghost(ui, &ink, "直接下载").clicked() {
                    let url = self.url.clone();
                    self.send(Cmd::EnqueueUrl { url, title: String::new() });
                    self.page = Page::Tasks;
                }
            });
        });
        ui.add_space(14.0);

        match &snap.parse {
            ParseView::Busy => {
                card(&ink).show(ui, |ui| {
                    ui.label(RichText::new("正在解析画质和标题…").color(ink.muted));
                });
                ui.add_space(14.0);
            }
            ParseView::Failed(err) => {
                card(&ink).show(ui, |ui| {
                    ui.label(RichText::new("解析没有完成").strong().color(ink.rose));
                    ui.label(RichText::new(err).color(ink.muted));
                });
                ui.add_space(14.0);
            }
            ParseView::Ready => {
                if let Some(parsed) = &snap.parsed {
                    card(&ink).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            badge(ui, ink.brand_soft, ink.brand_text, &parsed.platform_label);
                            if parsed.duration > 0 {
                                badge(ui, ink.slate_soft, ink.muted, &human_duration(parsed.duration));
                            }
                        });
                        ui.add_space(6.0);
                        ui.label(RichText::new(&parsed.title).size(18.0).strong().color(ink.text));
                        ui.label(RichText::new(if parsed.author.is_empty() { "未知作者" } else { parsed.author.as_str() }).color(ink.muted));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("画质").color(ink.muted));
                            egui::ComboBox::from_id_salt("home-format")
                                .selected_text(format_label(parsed, &self.format_id))
                                .width(280.0)
                                .show_ui(ui, |ui| {
                                    for fmt in &parsed.formats {
                                        let text = if fmt.filesize > 0 {
                                            format!("{} · {}", fmt.label, human_bytes(fmt.filesize.max(0) as u64))
                                        } else {
                                            fmt.label.clone()
                                        };
                                        ui.selectable_value(&mut self.format_id, fmt.id.clone(), text);
                                    }
                                });
                            ui.label(RichText::new("封装").color(ink.muted));
                            container_box(ui, "home-container", &mut self.container);
                        });
                        ui.add_space(8.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if primary(ui, &ink, "加入下载队列").clicked() {
                                self.send(Cmd::DownloadParsed {
                                    format_id: self.format_id.clone(),
                                    container: self.container.clone(),
                                });
                                self.page = Page::Tasks;
                            }
                        });
                    });
                    ui.add_space(14.0);
                }
            }
            ParseView::Idle => {}
        }

        let features = [
            ("智能识别", "自动识别主流视频平台和直链，粘贴就能解析。"),
            ("并发下载", "多任务排队，实时显示进度、速度和剩余时间。"),
            ("直播录制", "抖音、快手、B 站可以多路一起录，停录后封装。"),
        ];
        let width = ui.available_width();
        let cols = if width >= 720.0 { 3.0 } else { 1.0 };
        let tile = (width - 12.0 * (cols - 1.0)) / cols;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
            for (title, body) in features {
                ui.allocate_ui_with_layout(
                    egui::vec2(tile, 108.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        card(&ink).show(ui, |ui| {
                            ui.label(RichText::new(title).strong().color(ink.text));
                            ui.add_space(4.0);
                            ui.label(RichText::new(body).size(13.0).color(ink.muted));
                        });
                    },
                );
            }
        });
    }

    fn begin_parse(&mut self) {
        let Some(url) = platform::extract_url(&self.url) else { return };
        self.url = url.clone();
        self.format_id.clear();
        self.send(Cmd::Parse(url));
    }

    fn paste_into_url(&mut self) {
        let Ok(mut clipboard) = arboard::Clipboard::new() else { return };
        if let Ok(text) = clipboard.get_text() {
            self.url = text;
        }
    }

    fn flip_theme(&mut self, snap: &Snapshot, dark_now: bool) {
        let mut settings = snap.settings.clone();
        settings.theme = if dark_now { "light".into() } else { "dark".into() };
        if let Some(draft) = self.settings_draft.as_mut() {
            draft.theme = settings.theme.clone();
        }
        self.send(Cmd::SetSettings(settings));
    }

    fn page_tasks(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let ink = Ink::of(ui);
        let downloading = snap.tasks.iter().filter(|task| task.status == TaskStatus::Downloading).count();
        let waiting = snap.tasks.iter().filter(|task| task.status == TaskStatus::Waiting).count();
        let speed: f64 = snap.tasks.iter().filter(|task| task.status == TaskStatus::Downloading).map(|task| task.speed).sum();
        card(&ink).show(ui, |ui| {
            stretch(ui);
            ui.horizontal(|ui| {
                badge(ui, ink.brand_soft, ink.brand_text, &format!("并发 {}", snap.settings.max_concurrency));
                badge(ui, ink.blue_soft, ink.blue, &format!("下载中 {downloading}"));
                badge(ui, ink.amber_soft, ink.amber, &format!("等待 {waiting}"));
                if speed > 0.0 {
                    badge(ui, ink.brand_soft, ink.brand_text, &format!("总速度 {}", human_speed(speed)));
                }
            });
        });
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            let filters = [
                TaskFilter::All,
                TaskFilter::Status(TaskStatus::Downloading),
                TaskFilter::Status(TaskStatus::Waiting),
                TaskFilter::Status(TaskStatus::Paused),
                TaskFilter::Status(TaskStatus::Completed),
                TaskFilter::Status(TaskStatus::Failed),
                TaskFilter::Status(TaskStatus::Cancelled),
            ];
            for filter in filters {
                let count = if matches!(filter, TaskFilter::All) {
                    snap.tasks.len()
                } else {
                    snap.tasks.iter().filter(|task| filter.matches(task.status)).count()
                };
                if chip(ui, &ink, &format!("{} {count}", filter.label()), self.task_filter == filter).clicked() {
                    self.task_filter = filter;
                }
            }
        });
        ui.add_space(12.0);
        let shown: Vec<&Task> = snap.tasks.iter().filter(|task| self.task_filter.matches(task.status)).collect();
        if shown.is_empty() {
            empty_card(ui, &ink, "还没有下载任务", "回到首页，粘贴一个视频链接。");
            return;
        }
        for task in shown {
            self.task_card(ui, &ink, task);
            ui.add_space(10.0);
        }
    }

    fn task_card(&mut self, ui: &mut egui::Ui, ink: &Ink, task: &Task) {
        card(ink).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    let title = if task.title.is_empty() { task.url.as_str() } else { task.title.as_str() };
                    ui.label(RichText::new(title).strong().size(15.0).color(ink.text));
                    ui.horizontal(|ui| {
                        let (fill, color) = task_tone(ink, task.status);
                        badge(ui, fill, color, &format!("● {}", task.status.label()));
                        if !task.platform_label.is_empty() {
                            badge(ui, ink.slate_soft, ink.muted, &task.platform_label);
                        }
                        if !task.quality.is_empty() {
                            badge(ui, ink.slate_soft, ink.muted, &task.quality);
                        }
                    });
                });
            });
            if task.status == TaskStatus::Downloading {
                ui.add_space(6.0);
                ui.add(
                    egui::ProgressBar::new((task.percent / 100.0).clamp(0.0, 1.0) as f32)
                        .fill(ink.brand)
                        .desired_height(8.0)
                        .corner_radius(4.0)
                        .animate(true),
                );
                ui.label(RichText::new(format!(
                    "{:.0}%  ·  {}  ·  {}  ·  剩余 {}",
                    task.percent.clamp(0.0, 100.0),
                    human_bytes(task.downloaded_bytes.max(0) as u64),
                    human_speed(task.speed),
                    human_eta(task.eta)
                )).size(12.0).color(ink.muted));
            } else if !task.progress_text.is_empty() {
                ui.label(RichText::new(&task.progress_text).size(12.0).color(ink.muted));
            }
            if !task.error.is_empty() {
                ui.label(RichText::new(&task.error).color(ink.rose));
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if matches!(task.status, TaskStatus::Waiting | TaskStatus::Downloading | TaskStatus::Parsing)
                    && ghost(ui, ink, "暂停").clicked()
                {
                    self.send(Cmd::Pause(task.id.clone()));
                }
                if matches!(task.status, TaskStatus::Paused | TaskStatus::Failed | TaskStatus::Cancelled)
                    && ghost(ui, ink, "继续").clicked()
                {
                    self.send(Cmd::Resume(task.id.clone()));
                }
                if task.status.is_active() && task.status != TaskStatus::Paused && ghost(ui, ink, "取消").clicked() {
                    self.send(Cmd::Cancel(task.id.clone()));
                }
                if task.status == TaskStatus::Failed && ghost(ui, ink, "重试").clicked() {
                    self.send(Cmd::Retry(task.id.clone()));
                }
                if !task.output_path.is_empty() && ghost(ui, ink, "打开位置").clicked() {
                    self.send(Cmd::Reveal(task.output_path.clone()));
                }
                if ghost(ui, ink, "删除").clicked() {
                    self.send(Cmd::DeleteTask { id: task.id.clone(), delete_file: false });
                }
                if !task.output_path.is_empty() && danger(ui, ink, "删除并删文件").clicked() {
                    self.send(Cmd::DeleteTask { id: task.id.clone(), delete_file: true });
                }
            });
        });
    }

    fn page_live(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let ink = Ink::of(ui);
        card(&ink).show(ui, |ui| {
            ui.label(RichText::new("添加直播间").strong().color(ink.text));
            ui.add_space(6.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.live_url)
                    .desired_width(ui.available_width())
                    .hint_text("直播间链接、分享文案，或房间号")
                    .margin(Margin::symmetric(10, 8)),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("画质").color(ink.muted));
                quality_box(ui, "live-add-quality", &mut self.live_quality);
                ui.label(RichText::new("封装").color(ink.muted));
                live_format_box(ui, "live-add-format", &mut self.live_format);
                if ghost(ui, &ink, "只加入列表").clicked() {
                    self.add_room(false);
                }
                if rose_button(ui, &ink, "加入并录制").clicked() {
                    self.add_room(true);
                }
            });
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if primary(ui, &ink, "全部开始").clicked() {
                self.send(Cmd::StartAllLive);
            }
            if ghost(ui, &ink, "全部停止").clicked() {
                self.send(Cmd::StopAllLive);
            }
            if ghost(ui, &ink, "清掉未在录的房间").clicked() {
                self.send(Cmd::ClearIdleRooms);
            }
        });
        ui.add_space(12.0);
        if snap.rooms.is_empty() {
            empty_card(ui, &ink, "还没有直播间", "贴上抖音、快手或 B 站的直播链接。");
        }
        for room in &snap.rooms {
            self.room_card(ui, &ink, room);
            ui.add_space(10.0);
        }
        if !snap.recordings.is_empty() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("最近录制 {}", snap.recordings.len())).strong().color(ink.text));
                if ghost(ui, &ink, "在历史里查看").clicked() {
                    self.history_tab = HistoryTab::Live;
                    self.page = Page::History;
                }
            });
        }
    }

    fn add_room(&mut self, autostart: bool) {
        let url = self.live_url.clone();
        self.send(Cmd::AddRoom {
            url,
            quality: self.live_quality.clone(),
            format: self.live_format.clone(),
            autostart,
        });
    }

    fn room_card(&mut self, ui: &mut egui::Ui, ink: &Ink, room: &LiveRoom) {
        card(ink).show(ui, |ui| {
            let name = if room.anchor_name.is_empty() { room.url.as_str() } else { room.anchor_name.as_str() };
            ui.horizontal(|ui| {
                avatar(ui, ink, name);
                ui.vertical(|ui| {
                    ui.label(RichText::new(name).strong().size(15.0).color(ink.text));
                    ui.horizontal(|ui| {
                let (fill, color) = room_tone(ink, room.status);
                badge(ui, fill, color, &format!("● {}", room.status.label()));
                if !room.platform_label.is_empty() {
                    badge(ui, ink.slate_soft, ink.muted, &room.platform_label);
                }
                match room.live_on {
                    Some(true) => badge(ui, ink.rose_soft, ink.rose, "直播中"),
                    Some(false) => badge(ui, ink.slate_soft, ink.muted, "未开播"),
                    None => {}
                }
                    });
                });
            });
            if !room.title.is_empty() && room.title != name {
                ui.label(RichText::new(&room.title).color(ink.muted));
            }
            if room.status.is_running() {
                ui.label(RichText::new(format!("{}  ·  {}", room.message, human_bytes(room.filesize.max(0) as u64))).size(12.0).color(ink.muted));
            } else if !room.message.is_empty() {
                ui.label(RichText::new(&room.message).size(12.0).color(ink.muted));
            }
            if !room.error.is_empty() {
                ui.label(RichText::new(&room.error).color(ink.rose));
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if !room.status.is_running() && primary(ui, ink, "开始").clicked() {
                    self.send(Cmd::StartRoom(room.id.clone()));
                }
                if room.status.is_running() && danger(ui, ink, "停止").clicked() {
                    self.send(Cmd::StopRoom(room.id.clone()));
                }
                if ghost(ui, ink, "检查开播").clicked() {
                    self.send(Cmd::CheckRoom(room.id.clone()));
                }
                if !room.record_url.is_empty() && ghost(ui, ink, "预览").clicked() {
                    self.send(Cmd::Preview { url: room.record_url.clone(), platform: room.platform.clone() });
                }
                if !room.output_path.is_empty() && ghost(ui, ink, "打开位置").clicked() {
                    self.send(Cmd::Reveal(room.output_path.clone()));
                }
                if ghost(ui, ink, "移除").clicked() {
                    self.send(Cmd::DeleteRoom(room.id.clone()));
                }
            });
        });
    }

    fn page_history(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let ink = Ink::of(ui);
        let downloads = snap.tasks.iter().filter(|task| !task.status.is_active()).count();
        ui.horizontal(|ui| {
            if tab_button(ui, &ink, &format!("下载历史 {downloads}"), self.history_tab == HistoryTab::Download).clicked() {
                self.history_tab = HistoryTab::Download;
            }
            if tab_button(ui, &ink, &format!("录制历史 {}", snap.recordings.len()), self.history_tab == HistoryTab::Live).clicked() {
                self.history_tab = HistoryTab::Live;
            }
        });
        ui.add_space(12.0);
        match self.history_tab {
            HistoryTab::Download => {
                ui.add(
                    egui::TextEdit::singleline(&mut self.history_query)
                        .desired_width(ui.available_width())
                        .hint_text("搜索标题、作者或链接")
                        .margin(Margin::symmetric(10, 8)),
                );
                ui.add_space(12.0);
                let query = self.history_query.to_lowercase();
                let mut shown = 0;
                for task in &snap.tasks {
                    if task.status.is_active() {
                        continue;
                    }
                    if !query.is_empty() {
                        let hay = format!("{} {} {}", task.title, task.author, task.url).to_lowercase();
                        if !hay.contains(&query) {
                            continue;
                        }
                    }
                    shown += 1;
                    self.task_card(ui, &ink, task);
                    ui.add_space(10.0);
                }
                if shown == 0 {
                    empty_card(ui, &ink, "没有匹配的下载记录", "完成、失败或取消的任务会出现在这里。");
                }
            }
            HistoryTab::Live => {
                if snap.recordings.is_empty() {
                    empty_card(ui, &ink, "还没有录制记录", "直播页开始录制后，文件会记在这里。");
                    return;
                }
                for item in &snap.recordings {
                    self.recording_card(ui, &ink, item);
                    ui.add_space(10.0);
                }
            }
        }
    }

    fn recording_card(&mut self, ui: &mut egui::Ui, ink: &Ink, item: &LiveRecording) {
        card(ink).show(ui, |ui| {
            let name = if !item.anchor_name.is_empty() {
                item.anchor_name.as_str()
            } else if !item.title.is_empty() {
                item.title.as_str()
            } else {
                "录制"
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).strong().color(ink.text));
                let (fill, color) = recording_tone(ink, item.status);
                badge(ui, fill, color, &format!("● {}", item.status.label()));
                ui.label(RichText::new(clock(item.started_at)).size(12.0).color(ink.muted));
            });
            ui.label(RichText::new(format!(
                "{}  ·  {}",
                human_bytes(item.filesize.max(0) as u64),
                human_duration(item.duration_sec)
            )).size(12.0).color(ink.muted));
            if !item.error.is_empty() {
                ui.label(RichText::new(&item.error).color(ink.rose));
            }
            ui.horizontal_wrapped(|ui| {
                if !item.output_path.is_empty() && ghost(ui, ink, "打开位置").clicked() {
                    self.send(Cmd::Reveal(item.output_path.clone()));
                }
                if ghost(ui, ink, "删除记录").clicked() {
                    self.send(Cmd::DeleteRecording { id: item.id.clone(), delete_file: false });
                }
                if !item.output_path.is_empty() && danger(ui, ink, "删除并删文件").clicked() {
                    self.send(Cmd::DeleteRecording { id: item.id.clone(), delete_file: true });
                }
            });
        });
    }

    fn page_settings(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let ink = Ink::of(ui);
        let mut save = false;
        let mut reset = false;
        if let Some(draft) = self.settings_draft.as_mut() {
            card(&ink).show(ui, |ui| {
                stretch(ui);
                section(ui, &ink, "下载");
                path_row(ui, &ink, "下载目录", &mut draft.download_dir, true);
                slider_i64(ui, &ink, "同时下载", &mut draft.max_concurrency, 1, 10);
                slider_i64(ui, &ink, "分片线程", &mut draft.download_threads, 1, 16);
                slider_i64(ui, &ink, "超时秒", &mut draft.timeout_sec, 5, 300);
                slider_i64(ui, &ink, "重试次数", &mut draft.max_retries, 0, 10);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("限速 KB/s，0 表示不限").color(ink.muted));
                    let mut kb = (draft.max_speed / 1024).max(0);
                    if ui.add(egui::DragValue::new(&mut kb).range(0..=1024 * 1024)).changed() {
                        draft.max_speed = kb.saturating_mul(1024);
                    }
                });
                path_row(ui, &ink, "Cookie 文件", &mut draft.cookie_file, false);
            });
            ui.add_space(12.0);
            card(&ink).show(ui, |ui| {
                stretch(ui);
                section(ui, &ink, "直播");
                path_row(ui, &ink, "直播目录", &mut draft.live_dir, true);
                slider_i64(ui, &ink, "同时录制", &mut draft.live_max_concurrent, 1, 12);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("默认画质").color(ink.muted));
                    quality_box(ui, "settings-quality", &mut draft.live_quality);
                    ui.label(RichText::new("封装").color(ink.muted));
                    live_format_box(ui, "settings-live-format", &mut draft.live_format);
                });
            });
            ui.add_space(12.0);
            card(&ink).show(ui, |ui| {
                stretch(ui);
                section(ui, &ink, "外观");
                ui.horizontal(|ui| {
                    for (id, label) in [("system", "跟随系统"), ("light", "浅色"), ("dark", "深色")] {
                        if chip(ui, &ink, label, draft.theme == id).clicked() {
                            draft.theme = id.into();
                        }
                    }
                });
                ui.checkbox(&mut draft.close_to_tray, "关闭窗口时留在托盘");
            });
            ui.add_space(12.0);
            card(&ink).show(ui, |ui| {
                stretch(ui);
                section(ui, &ink, "工具");
                path_row(ui, &ink, "yt-dlp", &mut draft.ytdlp_path, false);
                path_row(ui, &ink, "ffmpeg", &mut draft.ffmpeg_path, false);
                tool_line(ui, &ink, "yt-dlp", &snap.tools.ytdlp, snap.tools.ytdlp_ok);
                tool_line(ui, &ink, "ffmpeg", &snap.tools.ffmpeg, snap.tools.ffmpeg_ok);
                tool_line(ui, &ink, "ffplay", &snap.tools.ffplay, snap.tools.ffplay_ok);
            });
        }
        ui.add_space(12.0);
        card(&ink).show(ui, |ui| {
            stretch(ui);
            section(ui, &ink, "手机和扩展");
            ui.label(RichText::new(format!("本机端口 {}", snap.api_port)).color(ink.text));
            ui.label(RichText::new("Chrome 扩展仍用 N:\\deep\\extension，依次试 4000、41777、18765。旧版如果占着 4000，扩展会先连到旧版。").size(12.0).color(ink.muted));
            for url in &snap.lan_urls {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(url).monospace().color(ink.text));
                    if ghost(ui, &ink, "复制").clicked() {
                        ui.ctx().copy_text(url.clone());
                    }
                });
            }
            ui.label(RichText::new("手机只发 POST /api/mobile/link。同一局域网时，防火墙可能要允许萌下载。").size(12.0).color(ink.muted));
        });
        ui.add_space(12.0);
        card(&ink).show(ui, |ui| {
            stretch(ui);
            section(ui, &ink, "更新");
            ui.label(RichText::new(format!("当前版本 {}", env!("CARGO_PKG_VERSION"))).color(ink.text));
            if !snap.update.message.is_empty() {
                let color = if snap.update.phase == UpdatePhase::Failed { ink.rose } else { ink.muted };
                ui.label(RichText::new(&snap.update.message).color(color));
            }
            if snap.update.phase == UpdatePhase::Available && !snap.update.notes.is_empty() {
                ui.label(RichText::new(&snap.update.notes).size(12.0).color(ink.muted));
            }
            ui.horizontal(|ui| {
                let busy = matches!(snap.update.phase, UpdatePhase::Checking | UpdatePhase::Downloading);
                if !busy && ghost(ui, &ink, "检查更新").clicked() {
                    self.send(Cmd::CheckUpdate);
                }
                if snap.update.phase == UpdatePhase::Available && primary(ui, &ink, "下载并重启").clicked() {
                    self.send(Cmd::ApplyUpdate);
                }
            });
            ui.label(RichText::new("启动时会检查 GitHub 上的新版本。正在下载或录制时不会替换程序。").size(12.0).color(ink.muted));
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            if primary(ui, &ink, "保存设置").clicked() {
                save = true;
            }
            if ghost(ui, &ink, "放弃修改").clicked() {
                reset = true;
            }
            if ghost(ui, &ink, "打开下载目录").clicked() {
                self.send(Cmd::OpenDir(snap.settings.download_dir.clone()));
            }
            if ghost(ui, &ink, "打开直播目录").clicked() {
                self.send(Cmd::OpenDir(snap.settings.live_dir.clone()));
            }
        });
        if save {
            if let Some(draft) = self.settings_draft.clone() {
                self.send(Cmd::SetSettings(draft));
            }
        }
        if reset {
            self.settings_draft = Some(snap.settings.clone());
        }
    }
}

impl Resident for MengApp {
    fn closed(&self) -> Closed {
        if self.quit || !self.hide_on_close() { Closed::Quit } else { Closed::Hide }
    }

    fn window_gone(&mut self) {
        self.wants_show = false;
    }

    fn headless_frame(&mut self, _ctx: &egui::Context) -> Headless {
        self.poll_tray();
        if self.quit {
            Headless::Quit
        } else if self.wants_show {
            Headless::Show
        } else {
            Headless::Wait
        }
    }

    fn shutdown(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(handle) = self.supervisor.take() {
            let _ = handle.join();
        }
    }
}

pub struct Window {
    pub app: Held<MengApp>,
    recovery_checked: bool,
}

impl Window {
    pub fn new(app: Held<MengApp>) -> Self {
        Self { app, recovery_checked: false }
    }
}

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if !self.recovery_checked {
            self.recovery_checked = true;
            fastframe_shell::window::recover_offscreen(ui.ctx(), frame);
            if let Some(tray) = self.app.tray.as_mut() {
                tray.attach();
            }
        }
        self.app.frame(ui);
    }
}

pub fn open_window(lease: fastframe_shell::Lease<MengApp>) -> eframe::Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_title("萌下载")
        .with_inner_size([1280.0, 820.0])
        .with_min_inner_size([980.0, 640.0])
        .with_icon(std::sync::Arc::new(egui::IconData {
            rgba: app_icon_rgba(64),
            width: 64,
            height: 64,
        }));
    let options = eframe::NativeOptions {
        viewport,
        persist_window: true,
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "mengdownloader",
        options,
        Box::new(move |cc| {
            fastframe_fonts::FontSetup::default().install(&cc.egui_ctx);
            Ok(Box::new(Window::new(lease.take(&cc.egui_ctx))))
        }),
    )
}

fn page_dashboard(ui: &mut egui::Ui, snap: &Snapshot) {
    let ink = Ink::of(ui);
    let stats = &snap.stats;
    let tiles = [
        ("今日任务", stats.today_tasks.to_string(), ink.brand),
        ("今日完成", stats.completed_today.to_string(), ink.emerald),
        ("下载中", stats.downloading.to_string(), ink.blue),
        ("今日失败", stats.failed_today.to_string(), ink.rose),
        ("累计体积", human_bytes(stats.total_bytes.max(0) as u64), ink.brand_text),
        ("累计任务", stats.total_tasks.to_string(), ink.amber),
    ];
    let full = ui.available_width();
    let cols = if full >= 760.0 { 3.0 } else { 2.0 };
    let gap = 12.0;
    let tile_w = (full - gap * (cols - 1.0) - 2.0) / cols;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
        for (label, value, tone) in tiles {
            show_sized_card(ui, &ink, egui::vec2(tile_w, 96.0), |ui| {
                ui.label(RichText::new(label).size(12.0).color(ink.muted));
                ui.add_space(6.0);
                ui.label(RichText::new(value).size(26.0).strong().color(tone));
            });
        }
    });
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        let full = ui.available_width();
        let left = ((full - gap) * 0.36).max(220.0);
        let right = (full - gap - left).max(220.0);
        show_sized_card(ui, &ink, egui::vec2(left, 248.0), |ui| {
            section(ui, &ink, "成功率");
            ui.vertical_centered(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(132.0, 132.0), egui::Sense::hover());
                let center = rect.center();
                ui.painter().circle_stroke(center, 52.0, Stroke::new(10.0, ink.field));
                let rate = stats.success_rate.clamp(0.0, 1.0) as f32;
                paint_arc(ui.painter(), center, 52.0, 10.0, rate * std::f32::consts::TAU, ink.brand);
                ui.painter().text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    format!("{:.0}%", rate * 100.0),
                    egui::FontId::proportional(22.0),
                    ink.text,
                );
            });
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(format!("完成 {} / 任务 {}", stats.total_completed, stats.total_tasks)).size(12.0).color(ink.muted));
            });
        });
        show_sized_card(ui, &ink, egui::vec2(right, 248.0), |ui| {
            section(ui, &ink, "各平台下载数量");
            if stats.by_platform.is_empty() {
                ui.label(RichText::new("还没有平台统计。").color(ink.muted));
                return;
            }
            let max = stats.by_platform.iter().map(|item| item.count).max().unwrap_or(1).max(1) as f32;
            for item in &stats.by_platform {
                let name = if item.label.is_empty() { item.platform.as_str() } else { item.label.as_str() };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(name).color(ink.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("{} 次 · {}", item.count, human_bytes(item.bytes.max(0) as u64))).size(12.0).color(ink.muted));
                    });
                });
                let bar_w = ui.available_width().max(8.0);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, 8.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 4.0, ink.field);
                let filled = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * (item.count as f32 / max), rect.height()));
                ui.painter().rect_filled(filled, 4.0, ink.brand);
                ui.add_space(6.0);
            }
        });
    });
}

fn show_sized_card<R>(ui: &mut egui::Ui, ink: &Ink, size: egui::Vec2, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let frame = card(ink);
    let content = rect - frame.total_margin();
    if ui.is_rect_visible(rect) {
        ui.painter().add(frame.paint(content));
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    add(&mut child)
}

fn paint_arc(painter: &egui::Painter, center: egui::Pos2, radius: f32, thickness: f32, sweep: f32, color: Color32) {
    let sweep = sweep.clamp(0.0, std::f32::consts::TAU);
    if sweep <= 0.001 {
        return;
    }
    let steps = ((sweep / std::f32::consts::TAU) * 72.0).ceil().max(2.0) as usize;
    let start = -std::f32::consts::FRAC_PI_2;
    let mut points = Vec::with_capacity(steps + 1);
    for step in 0..=steps {
        let angle = start + sweep * (step as f32 / steps as f32);
        points.push(center + egui::vec2(angle.cos(), angle.sin()) * radius);
    }
    painter.add(egui::Shape::line(points, Stroke::new(thickness, color)));
}

fn card(ink: &Ink) -> Frame {
    Frame::NONE
        .fill(ink.card)
        .stroke(Stroke::new(1.0, ink.line))
        .corner_radius(16.0)
        .inner_margin(Margin::same(16))
        .shadow(egui::Shadow {
            offset: [0, 2],
            blur: 12,
            spread: 0,
            color: Color32::from_black_alpha(if ink.dark { 48 } else { 14 }),
        })
}

fn section(ui: &mut egui::Ui, ink: &Ink, text: &str) {
    ui.label(RichText::new(text).size(15.0).strong().color(ink.text));
    ui.add_space(8.0);
}

fn empty_card(ui: &mut egui::Ui, ink: &Ink, title: &str, body: &str) {
    card(ink).show(ui, |ui| {
        stretch(ui);
        ui.add_space(18.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(title).size(16.0).strong().color(ink.text));
            ui.label(RichText::new(body).color(ink.muted));
        });
        ui.add_space(14.0);
    });
}

fn stretch(ui: &mut egui::Ui) {
    let width = ui.available_width();
    if width.is_finite() {
        ui.set_min_width(width);
    }
}

fn mark(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    ui.painter().rect_filled(rect, size * 0.28, color);
    let center = rect.center();
    let white = Color32::WHITE;
    let scale = size / 36.0;
    ui.painter().rect_filled(
        egui::Rect::from_center_size(center + egui::vec2(0.0, -4.0 * scale), egui::vec2(4.0 * scale, 12.0 * scale)),
        1.0,
        white,
    );
    let tip = center + egui::vec2(0.0, 8.0 * scale);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            tip + egui::vec2(-7.0 * scale, -6.0 * scale),
            tip + egui::vec2(7.0 * scale, -6.0 * scale),
            tip,
        ],
        white,
        Stroke::NONE,
    ));
}

fn avatar(ui: &mut egui::Ui, ink: &Ink, name: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 18.0, ink.brand_soft);
    let letter = name.chars().next().unwrap_or('播').to_string();
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        letter,
        egui::FontId::proportional(14.0),
        ink.brand_text,
    );
}

fn nav_item(ui: &mut egui::Ui, ink: &Ink, page: Page, active: bool) -> bool {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 40.0), egui::Sense::click());
    let fill = if active {
        ink.brand_soft
    } else if response.hovered() {
        ink.hover
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 12.0, fill);
    }
    if active {
        let bar = egui::Rect::from_min_size(egui::pos2(rect.left() + 4.0, rect.center().y - 8.0), egui::vec2(3.0, 16.0));
        ui.painter().rect_filled(bar, 2.0, ink.brand);
    }
    let color = if active { ink.brand_text } else { ink.muted };
    paint_nav_icon(ui.painter(), page, rect.left_center() + egui::vec2(22.0, 0.0), color);
    ui.painter().text(
        rect.left_center() + egui::vec2(40.0, 0.0),
        egui::Align2::LEFT_CENTER,
        page.title(),
        egui::FontId::proportional(14.0),
        color,
    );
    ui.add_space(4.0);
    response.clicked()
}

fn paint_nav_icon(painter: &egui::Painter, page: Page, center: egui::Pos2, color: Color32) {
    let stroke = Stroke::new(1.7, color);
    match page {
        Page::Home => {
            // Filled house. A stroked roof over a box reads as the character 合.
            painter.add(egui::Shape::convex_polygon(
                vec![
                    center + egui::vec2(-8.0, -0.2),
                    center + egui::vec2(0.0, -7.4),
                    center + egui::vec2(8.0, -0.2),
                ],
                color,
                Stroke::NONE,
            ));
            painter.rect_filled(
                egui::Rect::from_center_size(center + egui::vec2(0.0, 3.4), egui::vec2(12.0, 7.6)),
                1.2,
                color,
            );
            painter.rect_filled(
                egui::Rect::from_center_size(center + egui::vec2(0.0, 4.6), egui::vec2(3.4, 4.8)),
                0.6,
                Color32::from_black_alpha(80),
            );
        }
        Page::Tasks => {
            painter.line_segment([center + egui::vec2(0.0, -6.5), center + egui::vec2(0.0, 2.0)], stroke);
            painter.line_segment([center + egui::vec2(-4.5, 1.0), center + egui::vec2(0.0, 6.5)], stroke);
            painter.line_segment([center + egui::vec2(4.5, 1.0), center + egui::vec2(0.0, 6.5)], stroke);
        }
        Page::Live => {
            painter.circle_stroke(center, 2.6, stroke);
            painter.circle_stroke(center, 6.2, Stroke::new(1.4, color));
        }
        Page::History => {
            painter.circle_stroke(center, 6.2, stroke);
            painter.line_segment([center, center + egui::vec2(0.0, -3.5)], stroke);
            painter.line_segment([center, center + egui::vec2(3.0, 1.5)], stroke);
        }
        Page::Dashboard => {
            for (dx, height) in [(-5.0, 6.0), (0.0, 11.0), (5.0, 8.0)] {
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(center.x + dx - 1.7, center.y + 6.0 - height),
                        egui::vec2(3.4, height),
                    ),
                    1.0,
                    color,
                );
            }
        }
        Page::Settings => {
            for (dy, knob) in [(-4.5, -2.5), (0.0, 3.0), (4.5, -1.0)] {
                let y = center.y + dy;
                painter.line_segment(
                    [egui::pos2(center.x - 7.0, y), egui::pos2(center.x + 7.0, y)],
                    stroke,
                );
                painter.circle_filled(egui::pos2(center.x + knob, y), 2.2, color);
            }
        }
    }
}

fn badge(ui: &mut egui::Ui, fill: Color32, color: Color32, text: &str) {
    Frame::NONE
        .fill(fill)
        .corner_radius(20.0)
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(12.0).strong().color(color));
        });
}

fn primary(ui: &mut egui::Ui, ink: &Ink, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
            .fill(ink.brand)
            .stroke(Stroke::NONE)
            .corner_radius(10.0)
            .min_size(egui::vec2(0.0, 34.0)),
    )
}

fn rose_button(ui: &mut egui::Ui, ink: &Ink, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
            .fill(ink.rose)
            .stroke(Stroke::NONE)
            .corner_radius(20.0)
            .min_size(egui::vec2(0.0, 34.0)),
    )
}

fn ghost(ui: &mut egui::Ui, ink: &Ink, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(ink.text))
            .fill(ink.card)
            .stroke(Stroke::new(1.0, ink.line))
            .corner_radius(10.0)
            .min_size(egui::vec2(0.0, 32.0)),
    )
}

fn danger(ui: &mut egui::Ui, ink: &Ink, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(ink.rose))
            .fill(ink.rose_soft)
            .stroke(Stroke::NONE)
            .corner_radius(10.0)
            .min_size(egui::vec2(0.0, 32.0)),
    )
}

fn chip(ui: &mut egui::Ui, ink: &Ink, text: &str, active: bool) -> egui::Response {
    let (fill, color, stroke) = if active {
        (ink.brand, Color32::WHITE, Stroke::NONE)
    } else {
        (ink.card, ink.muted, Stroke::new(1.0, ink.line))
    };
    ui.add(
        egui::Button::new(RichText::new(text).size(13.0).color(color))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(20.0)
            .min_size(egui::vec2(0.0, 30.0)),
    )
}

fn tab_button(ui: &mut egui::Ui, ink: &Ink, text: &str, active: bool) -> egui::Response {
    let (fill, color) = if active { (ink.card, ink.text) } else { (Color32::TRANSPARENT, ink.muted) };
    ui.add(
        egui::Button::new(RichText::new(text).color(color).strong())
            .fill(fill)
            .stroke(if active { Stroke::new(1.0, ink.line) } else { Stroke::NONE })
            .corner_radius(10.0),
    )
}

fn task_tone(ink: &Ink, status: TaskStatus) -> (Color32, Color32) {
    match status {
        TaskStatus::Downloading => (ink.blue_soft, ink.blue),
        TaskStatus::Parsing | TaskStatus::Paused => (ink.amber_soft, ink.amber),
        TaskStatus::Completed => (ink.emerald_soft, ink.emerald),
        TaskStatus::Failed => (ink.rose_soft, ink.rose),
        TaskStatus::Waiting | TaskStatus::Cancelled => (ink.slate_soft, ink.muted),
    }
}

fn room_tone(ink: &Ink, status: RoomStatus) -> (Color32, Color32) {
    match status {
        RoomStatus::Recording => (ink.rose_soft, ink.rose),
        RoomStatus::Resolving | RoomStatus::Queued | RoomStatus::Stopping => (ink.amber_soft, ink.amber),
        RoomStatus::Done => (ink.emerald_soft, ink.emerald),
        RoomStatus::Error => (ink.rose_soft, ink.rose),
        RoomStatus::Idle => (ink.slate_soft, ink.muted),
    }
}

fn recording_tone(ink: &Ink, status: RecordingStatus) -> (Color32, Color32) {
    match status {
        RecordingStatus::Recording => (ink.rose_soft, ink.rose),
        RecordingStatus::Completed => (ink.emerald_soft, ink.emerald),
        RecordingStatus::Partial => (ink.amber_soft, ink.amber),
        RecordingStatus::Failed => (ink.rose_soft, ink.rose),
        RecordingStatus::Cancelled => (ink.slate_soft, ink.muted),
    }
}

fn path_row(ui: &mut egui::Ui, ink: &Ink, label: &str, value: &mut String, folder: bool) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(ink.muted));
        let width = (ui.available_width() - 78.0).max(40.0);
        ui.add(egui::TextEdit::singleline(value).desired_width(width).margin(Margin::symmetric(8, 6)));
        if ghost(ui, ink, "浏览").clicked() {
            let picked = if folder { rfd::FileDialog::new().pick_folder() } else { rfd::FileDialog::new().pick_file() };
            if let Some(path) = picked {
                *value = path.display().to_string();
            }
        }
    });
}

fn slider_i64(ui: &mut egui::Ui, ink: &Ink, label: &str, value: &mut i64, min: i64, max: i64) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(72.0, 28.0), egui::Sense::hover());
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(14.0),
            ink.muted,
        );
        ui.spacing_mut().slider_width = 220.0;
        let mut current = (*value).clamp(min, max) as i32;
        if ui
            .add(egui::Slider::new(&mut current, min as i32..=max as i32).trailing_fill(true))
            .changed()
        {
            *value = i64::from(current);
        }
    });
}

fn tool_line(ui: &mut egui::Ui, ink: &Ink, name: &str, path: &str, ok: bool) {
    let (fill, color, state) = if ok { (ink.emerald_soft, ink.emerald, "已找到") } else { (ink.rose_soft, ink.rose, "未找到") };
    ui.horizontal(|ui| {
        ui.label(RichText::new(name).color(ink.text));
        badge(ui, fill, color, state);
        ui.label(RichText::new(path).size(12.0).color(ink.muted));
    });
}

fn container_box(ui: &mut egui::Ui, salt: &str, value: &mut String) {
    egui::ComboBox::from_id_salt(salt).selected_text(value.as_str()).show_ui(ui, |ui| {
        for ext in ["mp4", "mkv", "webm", "flv"] {
            ui.selectable_value(value, ext.into(), ext);
        }
    });
}

fn live_format_box(ui: &mut egui::Ui, salt: &str, value: &mut String) {
    egui::ComboBox::from_id_salt(salt).selected_text(value.as_str()).show_ui(ui, |ui| {
        for ext in ["mkv", "mp4", "ts", "flv"] {
            ui.selectable_value(value, ext.into(), ext);
        }
    });
}

fn quality_box(ui: &mut egui::Ui, salt: &str, value: &mut String) {
    let label = match value.as_str() {
        "OD" => "原画 OD",
        "UHD" => "超清 UHD",
        "HD" => "高清 HD",
        "SD" => "标清 SD",
        "LD" => "流畅 LD",
        other => other,
    };
    egui::ComboBox::from_id_salt(salt).selected_text(label).show_ui(ui, |ui| {
        for (id, name) in [("OD", "原画 OD"), ("UHD", "超清 UHD"), ("HD", "高清 HD"), ("SD", "标清 SD"), ("LD", "流畅 LD")] {
            ui.selectable_value(value, id.into(), name);
        }
    });
}

fn format_label(parsed: &crate::model::ParseResult, id: &str) -> String {
    parsed.formats.iter().find(|fmt| fmt.id == id).map(|fmt| fmt.label.clone()).unwrap_or_else(|| "选择画质".into())
}

fn apply_theme(ctx: &egui::Context, theme: &str) -> bool {
    let dark = match theme {
        "light" => false,
        "dark" => true,
        _ => system_dark(),
    };
    let mode = egui::Theme::from_dark_mode(dark);
    ctx.set_theme(mode);
    let ink = Ink::from_dark(dark);
    let mut visuals = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    visuals.dark_mode = dark;
    visuals.panel_fill = ink.canvas;
    visuals.window_fill = ink.card;
    visuals.extreme_bg_color = ink.field;
    visuals.faint_bg_color = ink.hover;
    visuals.code_bg_color = ink.field;
    visuals.override_text_color = Some(ink.text);
    visuals.weak_text_color = Some(ink.muted);
    visuals.hyperlink_color = ink.brand;
    visuals.selection.bg_fill = ink.brand;
    visuals.selection.stroke = Stroke::new(1.0, ink.brand);
    visuals.warn_fg_color = ink.amber;
    visuals.error_fg_color = ink.rose;
    visuals.window_stroke = Stroke::new(1.0, ink.line);
    visuals.window_corner_radius = 12.0.into();
    visuals.menu_corner_radius = 10.0.into();
    visuals.window_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 70 } else { 18 }),
    };
    let radius = 8.0.into();
    for widgets in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widgets.corner_radius = radius;
    }
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, ink.line);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, ink.text);
    visuals.widgets.inactive.bg_fill = ink.field;
    visuals.widgets.inactive.weak_bg_fill = ink.card;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, ink.line);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, ink.text);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ink.brand);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, ink.text);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ink.brand);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    ctx.set_visuals_of(mode, visuals);
    ctx.style_mut_of(mode, |style| {
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.interact_size.y = 22.0;
        style.spacing.slider_width = 220.0;
    });
    dark
}

fn system_dark() -> bool {
    static DARK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DARK.get_or_init(read_system_dark)
}

fn read_system_dark() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
        let sub: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let name: Vec<u16> = "AppsUseLightTheme".encode_utf16().chain(std::iter::once(0)).collect();
        let mut data: u32 = 1;
        let mut size = std::mem::size_of::<u32>() as u32;
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                sub.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                &mut data as *mut u32 as *mut _,
                &mut size,
            )
        };
        if rc == 0 { data == 0 } else { true }
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Square RGBA icon: indigo tile, white download arrow.
fn app_icon_rgba(size: usize) -> Vec<u8> {
    let mut pixels = vec![0u8; size.saturating_mul(size).saturating_mul(4)];
    if size == 0 {
        return pixels;
    }
    let s = size as f32;
    let radius = s * 0.22;
    let half = s * 0.5;
    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let dx = (px - half).abs() - (half - radius);
            let dy = (py - half).abs() - (half - radius);
            let outside = dx.max(dy).min(0.0) + (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
            if outside > 0.6 {
                continue;
            }
            let i = (y * size + x) * 4;
            if download_arrow(px, py, s) {
                pixels[i] = 255;
                pixels[i + 1] = 255;
                pixels[i + 2] = 255;
            } else {
                pixels[i] = 79;
                pixels[i + 1] = 70;
                pixels[i + 2] = 229;
            }
            pixels[i + 3] = 255;
        }
    }
    pixels
}

fn download_arrow(x: f32, y: f32, s: f32) -> bool {
    let cx = s * 0.5;
    let shaft = (x - cx).abs() <= s * 0.05 && (s * 0.26..=s * 0.52).contains(&y);
    let base = s * 0.48;
    let tip = s * 0.74;
    let tri = y >= base && y <= tip && (x - cx).abs() <= (tip - y) / (tip - base) * s * 0.22;
    shaft || tri
}
