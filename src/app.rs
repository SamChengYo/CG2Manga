//! egui 介面：角色、故事、頁面、生成與編輯、設定。

use crate::layout::auto_layout;
use crate::llm::{self, Job, WorkerMsg};
use crate::model::*;
use crate::render::{self, Fonts};
use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp"];
const THUMB_PX: u32 = 256;
const PREVIEW_PX: u32 = 1600;
const STYLE_PRESETS: &[&str] =
    &["輕鬆搞笑", "熱血戰鬥", "純愛戀愛", "懸疑推理", "日常療癒", "黑暗沉重", "奇幻冒險", "校園青春", "成人向"];

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Characters,
    Story,
    Pages,
    Editor,
    Settings,
}

enum Drag {
    Body { id: u64, grab: [f32; 2] },
    Tail { id: u64 },
}

struct Preview {
    page: usize,
    path: PathBuf,
    base: image::RgbaImage,
}

pub struct App {
    project: Project,
    project_path: Option<PathBuf>,
    settings: Settings,
    fonts: Option<Arc<Fonts>>,
    font_error: Option<String>,
    tab: Tab,

    thumbs: HashMap<PathBuf, TextureHandle>,
    thumb_pending: HashSet<PathBuf>,
    thumb_tx: Sender<PathBuf>,
    thumb_rx: Receiver<(PathBuf, Option<ColorImage>)>,

    current: usize,
    selected: Option<u64>,
    preview: Option<Preview>,
    preview_tex: Option<TextureHandle>,
    preview_dirty: bool,
    drag: Option<Drag>,

    worker_rx: Option<Receiver<WorkerMsg>>,
    export_rx: Option<Receiver<String>>,
    cancel: Arc<AtomicBool>,
    progress: f32,
    progress_text: String,
    log: Vec<String>,
    status: String,
}

fn is_image(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn to_color_image(img: &image::RgbaImage) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw())
}

fn setup_egui_fonts(ctx: &egui::Context, path: &str) {
    let mut defs = egui::FontDefinitions::default();
    let candidates = [path, r"C:\Windows\Fonts\msjh.ttc", r"C:\Windows\Fonts\msyh.ttc", r"C:\Windows\Fonts\mingliu.ttc"];
    if let Some(bytes) = candidates.iter().find_map(|p| std::fs::read(p).ok()) {
        defs.font_data.insert("cjk".into(), Arc::new(egui::FontData::from_owned(bytes)));
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            defs.families.entry(fam).or_default().push("cjk".into());
        }
    }
    ctx.set_fonts(defs);
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings = Settings::load();
        setup_egui_fonts(&cc.egui_ctx, &settings.font_path);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        // 縮圖背景執行緒
        let (thumb_tx, req_rx) = channel::<PathBuf>();
        let (res_tx, thumb_rx) = channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            while let Ok(path) = req_rx.recv() {
                let img = image::open(&path).ok().map(|i| to_color_image(&i.thumbnail(THUMB_PX, THUMB_PX).to_rgba8()));
                if res_tx.send((path, img)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });

        let mut app = Self {
            project: Project::default(),
            project_path: None,
            settings,
            fonts: None,
            font_error: None,
            tab: Tab::Characters,
            thumbs: HashMap::new(),
            thumb_pending: HashSet::new(),
            thumb_tx,
            thumb_rx,
            current: 0,
            selected: None,
            preview: None,
            preview_tex: None,
            preview_dirty: true,
            drag: None,
            worker_rx: None,
            export_rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: 0.0,
            progress_text: String::new(),
            log: Vec::new(),
            status: "就緒".into(),
        };
        app.reload_fonts();
        app
    }

    fn reload_fonts(&mut self) {
        match Fonts::load(&self.settings.font_path, &self.settings.bold_font_path) {
            Ok(f) => {
                self.fonts = Some(Arc::new(f));
                self.font_error = None;
                self.preview_dirty = true;
            }
            Err(e) => self.font_error = Some(e.to_string()),
        }
    }

    fn busy(&self) -> bool {
        self.worker_rx.is_some()
    }

    fn push_log(&mut self, s: impl Into<String>) {
        let s = s.into();
        self.status = s.clone();
        self.log.push(s);
        if self.log.len() > 500 {
            self.log.drain(..100);
        }
    }

    // ───────────── 縮圖 ─────────────

    fn thumb(&mut self, path: &Path) -> Option<TextureHandle> {
        if let Some(t) = self.thumbs.get(path) {
            return Some(t.clone());
        }
        if self.thumb_pending.insert(path.to_path_buf()) {
            let _ = self.thumb_tx.send(path.to_path_buf());
        }
        None
    }

    fn poll_thumbs(&mut self, ctx: &egui::Context) {
        while let Ok((path, img)) = self.thumb_rx.try_recv() {
            if let Some(img) = img {
                let tex = ctx.load_texture(path.to_string_lossy(), img, TextureOptions::LINEAR);
                self.thumbs.insert(path, tex);
            }
        }
    }

    fn thumb_widget(&mut self, ui: &mut egui::Ui, path: &Path, max: f32) -> egui::Response {
        match self.thumb(path) {
            Some(tex) => {
                let s = tex.size_vec2();
                let k = (max / s.x).min(max / s.y);
                ui.add(egui::Image::new((tex.id(), s * k)).sense(Sense::click()))
            }
            None => {
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(max), Sense::click());
                ui.painter().rect_filled(r, 4.0, Color32::from_gray(40));
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "載入中", egui::FontId::proportional(12.0), Color32::GRAY);
                resp
            }
        }
    }

    // ───────────── 專案 ─────────────

    fn save_project(&mut self, save_as: bool) {
        let path = if save_as || self.project_path.is_none() {
            rfd::FileDialog::new().add_filter("CG2Manga 專案", &["cg2m.json", "json"]).set_file_name("project.cg2m.json").save_file()
        } else {
            self.project_path.clone()
        };
        if let Some(p) = path {
            match self.project.save(&p) {
                Ok(()) => {
                    self.push_log(format!("已儲存專案：{}", p.display()));
                    self.project_path = Some(p);
                }
                Err(e) => self.push_log(format!("儲存失敗：{e}")),
            }
        }
    }

    fn open_project(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("CG2Manga 專案", &["json"]).pick_file() {
            match Project::load(&p) {
                Ok(proj) => {
                    self.project = proj;
                    self.project_path = Some(p.clone());
                    self.current = 0;
                    self.selected = None;
                    self.preview = None;
                    self.preview_dirty = true;
                    self.push_log(format!("已開啟專案：{}", p.display()));
                }
                Err(e) => self.push_log(format!("開啟失敗：{e}")),
            }
        }
    }

    fn add_pages(&mut self, paths: Vec<PathBuf>) {
        let mut added = 0;
        if self.project.pages.is_empty() || self.project.output_dir.is_none() {
            if let Some(first) = paths.iter().find(|p| p.is_dir() || is_image(p)) {
                self.project.output_dir = output_dir_for(first);
            }
        }
        for p in paths {
            if p.is_dir() {
                let mut files: Vec<PathBuf> = std::fs::read_dir(&p)
                    .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| is_image(p)).collect())
                    .unwrap_or_default();
                files.sort();
                for f in files {
                    self.project.pages.push(Page::new(f));
                    added += 1;
                }
            } else if is_image(&p) {
                self.project.pages.push(Page::new(p));
                added += 1;
            }
        }
        self.push_log(format!("加入 {added} 張頁面圖片"));
    }

    // ───────────── 生成 ─────────────

    fn start_job(&mut self, ctx: &egui::Context, reanalyze: bool, range: Option<(usize, usize)>) {
        if self.busy() {
            return;
        }
        if reanalyze {
            for p in &mut self.project.pages {
                p.analysis = None;
            }
        }
        let job = Job {
            settings: self.settings.clone(),
            characters: self.project.characters.iter().filter(|c| !c.name.trim().is_empty()).cloned().collect(),
            story: self.project.story.clone(),
            pages: self.project.pages.iter().map(|p| p.path.clone()).collect(),
            analyses: self.project.pages.iter().map(|p| p.analysis.clone()).collect(),
            script_range: range,
            existing_script: self.project.pages.iter().map(|p| p.script.clone()).collect(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        self.cancel = job.cancel.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let (inner_tx, inner_rx) = channel();
            let handle = std::thread::spawn(move || llm::run(job, inner_tx));
            for msg in inner_rx {
                let _ = tx.send(msg);
                ctx.request_repaint();
            }
            let _ = handle.join();
            ctx.request_repaint();
        });
        self.worker_rx = Some(rx);
        self.progress = 0.0;
        self.progress_text = "開始…".into();
        self.push_log("開始生成");
    }

    fn poll_worker(&mut self) {
        let Some(rx) = self.worker_rx.take() else { return };
        let mut done = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                WorkerMsg::Log(s) => self.push_log(s),
                WorkerMsg::Progress(p, s) => {
                    self.progress = p;
                    self.progress_text = s.clone();
                    self.status = s;
                }
                WorkerMsg::Analysis(i, a) => {
                    if let Some(p) = self.project.pages.get_mut(i) {
                        p.analysis = Some(a);
                    }
                }
                WorkerMsg::Script(i, lines) => {
                    if let Some(p) = self.project.pages.get_mut(i) {
                        p.script = lines;
                    }
                    self.relayout_page(i);
                }
                WorkerMsg::Finished => {
                    self.push_log("生成完成！自動輸出圖片中…（之後修改對話框可再按「重新輸出」）");
                    done = true;
                    self.export(None);
                }
                WorkerMsg::Failed(e) => {
                    self.push_log(format!("生成失敗：{e}"));
                    done = true;
                }
            }
        }
        if !done {
            self.worker_rx = Some(rx);
        }
    }

    fn relayout_page(&mut self, i: usize) {
        let Some(fonts) = self.fonts.clone() else { return };
        let Some(page) = self.project.pages.get(i) else { return };
        let Ok((w, h)) = image::image_dimensions(&page.path) else {
            self.push_log(format!("無法讀取圖片尺寸：{}", page.path.display()));
            return;
        };
        let mut next = self.project.next_id;
        let bubbles = auto_layout(&fonts, w as f32, h as f32, page.analysis.as_ref(), &page.script, &self.settings, || {
            next += 1;
            next
        });
        self.project.next_id = next;
        self.project.pages[i].bubbles = bubbles;
        if i == self.current {
            self.preview_dirty = true;
            self.selected = None;
        }
    }

    fn export(&mut self, only: Option<usize>) {
        let Some(fonts) = self.fonts.clone() else { return };
        if self.project.output_dir.is_none() {
            self.project.output_dir = self.project.pages.first().and_then(|p| output_dir_for(&p.path));
        }
        let Some(dir) = self.project.output_dir.clone() else {
            self.push_log("無法決定輸出資料夾");
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.push_log(format!("無法建立輸出資料夾 {}：{e}", dir.display()));
            return;
        }
        self.push_log(format!("輸出到：{}", dir.display()));
        let pages: Vec<(usize, PathBuf, Vec<Bubble>)> = self
            .project
            .pages
            .iter()
            .enumerate()
            .filter(|(i, _)| only.is_none_or(|o| o == *i))
            .map(|(i, p)| (i, p.path.clone(), p.bubbles.clone()))
            .collect();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for (i, path, bubbles) in &pages {
                let res = image::open(path).map_err(|e| e.to_string()).and_then(|img| {
                    let out = render::compose(&img.to_rgba8(), bubbles, &fonts);
                    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let target = dir.join(format!("{stem}.png"));
                    out.save(&target).map_err(|e| e.to_string()).map(|_| target)
                });
                let _ = tx.send(match res {
                    Ok(t) => format!("已匯出：{}", t.display()),
                    Err(e) => format!("匯出第 {} 頁失敗：{e}", i + 1),
                });
            }
            let _ = tx.send(format!("匯出完成，共 {} 頁", pages.len()));
        });
        self.export_rx = Some(rx);
    }

    fn poll_export(&mut self) {
        let Some(rx) = self.export_rx.take() else { return };
        let mut alive = true;
        loop {
            match rx.try_recv() {
                Ok(s) => self.push_log(s),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(_) => {
                    alive = false;
                    break;
                }
            }
        }
        if alive {
            self.export_rx = Some(rx);
        }
    }

    // ───────────── 預覽 ─────────────

    fn ensure_preview(&mut self, ctx: &egui::Context) {
        let Some(page) = self.project.pages.get(self.current) else {
            self.preview = None;
            self.preview_tex = None;
            return;
        };
        let stale = self.preview.as_ref().is_none_or(|p| p.page != self.current || p.path != page.path);
        if stale {
            match image::open(&page.path) {
                Ok(img) => {
                    let img = if img.width().max(img.height()) > PREVIEW_PX {
                        img.resize(PREVIEW_PX, PREVIEW_PX, image::imageops::FilterType::Triangle)
                    } else {
                        img
                    };
                    self.preview = Some(Preview { page: self.current, path: page.path.clone(), base: img.to_rgba8() });
                }
                Err(e) => {
                    self.status = format!("無法開啟圖片：{e}");
                    self.preview = None;
                    self.preview_tex = None;
                    return;
                }
            }
            self.preview_dirty = true;
        }
        if self.preview_dirty {
            if let (Some(pv), Some(fonts)) = (&self.preview, &self.fonts) {
                let out = render::compose(&pv.base, &page.bubbles, fonts);
                let ci = to_color_image(&out);
                match &mut self.preview_tex {
                    Some(t) => t.set(ci, TextureOptions::LINEAR),
                    None => self.preview_tex = Some(ctx.load_texture("preview", ci, TextureOptions::LINEAR)),
                }
            }
            self.preview_dirty = false;
        }
    }

    fn selected_bubble(&mut self) -> Option<&mut Bubble> {
        let id = self.selected?;
        self.project.pages.get_mut(self.current)?.bubbles.iter_mut().find(|b| b.id == id)
    }

    // ───────────── 分頁：角色 ─────────────

    fn ui_characters(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("角色設定");
            if ui.button("＋ 新增角色").clicked() {
                self.project.characters.push(Character::default());
            }
        });
        ui.label("上傳每個角色的參考圖並填寫名字，模型會用參考圖辨認畫面中的人物。描述可以寫個性、口頭禪、與其他角色的關係。");
        ui.separator();
        let mut remove = None;
        let n = self.project.characters.len();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for ci in 0..n {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label("名字");
                        ui.add(egui::TextEdit::singleline(&mut self.project.characters[ci].name).desired_width(200.0).hint_text("例如：小美"));
                        if ui.button("加入參考圖…").clicked() {
                            if let Some(files) = rfd::FileDialog::new().add_filter("圖片", IMAGE_EXTS).pick_files() {
                                self.project.characters[ci].images.extend(files);
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("刪除角色").clicked() {
                                remove = Some(ci);
                            }
                        });
                    });
                    ui.add(
                        egui::TextEdit::multiline(&mut self.project.characters[ci].description)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .hint_text("個性、說話口吻、身分、與其他角色的關係…"),
                    );
                    let imgs = self.project.characters[ci].images.clone();
                    let mut remove_img = None;
                    ui.horizontal_wrapped(|ui| {
                        for (ii, p) in imgs.iter().enumerate() {
                            ui.vertical(|ui| {
                                self.thumb_widget(ui, p, 110.0).on_hover_text(p.display().to_string());
                                if ui.small_button("移除").clicked() {
                                    remove_img = Some(ii);
                                }
                            });
                        }
                        if imgs.is_empty() {
                            ui.weak("（尚未加入參考圖）");
                        }
                    });
                    if let Some(ii) = remove_img {
                        self.project.characters[ci].images.remove(ii);
                    }
                    if imgs.len() > self.settings.ref_per_character {
                        ui.weak(format!("提示：每個角色只會送出前 {} 張參考圖（可在設定調整）", self.settings.ref_per_character));
                    }
                });
                ui.add_space(6.0);
            }
        });
        if let Some(ci) = remove {
            self.project.characters.remove(ci);
        }
    }

    // ───────────── 分頁：故事 ─────────────

    fn ui_story(&mut self, ui: &mut egui::Ui) {
        ui.heading("故事風格與方向");
        ui.separator();
        let story = &mut self.project.story;
        egui::Grid::new("story").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
            ui.label("作品名稱");
            ui.add(egui::TextEdit::singleline(&mut story.title).desired_width(400.0));
            ui.end_row();

            ui.label("風格");
            ui.vertical(|ui| {
                ui.add(egui::TextEdit::singleline(&mut story.style).desired_width(400.0).hint_text("例如：輕鬆搞笑的校園日常"));
                ui.horizontal_wrapped(|ui| {
                    for p in STYLE_PRESETS {
                        if ui.small_button(*p).clicked() {
                            if !story.style.trim().is_empty() {
                                story.style.push('、');
                            }
                            story.style.push_str(p);
                        }
                    }
                });
            });
            ui.end_row();

            ui.label("劇情方向");
            ui.add(
                egui::TextEdit::multiline(&mut story.direction)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .hint_text("故事大綱、想要的走向、結局、重要事件…（可留白讓模型依圖發揮）"),
            );
            ui.end_row();

            ui.label("語言");
            ui.add(egui::TextEdit::singleline(&mut story.language).desired_width(300.0));
            ui.end_row();

            ui.label("文字量");
            ui.horizontal(|ui| {
                for d in [Density::Few, Density::Normal, Density::Many] {
                    ui.radio_value(&mut story.density, d, d.label());
                }
            });
            ui.end_row();

            ui.label("閱讀方向");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.settings.reading_rtl, true, "右→左（日式）");
                ui.radio_value(&mut self.settings.reading_rtl, false, "左→右（條漫/美式）");
            });
            ui.end_row();

            ui.label("對話框文字");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.settings.default_vertical, true, "直排");
                ui.radio_value(&mut self.settings.default_vertical, false, "橫排");
            });
            ui.end_row();

            ui.label("預設字級");
            ui.add(egui::Slider::new(&mut self.settings.default_font_size, 1.0..=5.0).step_by(0.1));
            ui.end_row();
        });
        ui.add_space(8.0);
        ui.weak(self.project.story.density.guidance());
    }

    // ───────────── 分頁：頁面 ─────────────

    fn ui_pages(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(format!("頁面圖片（{} 張，依順序閱讀）", self.project.pages.len()));
        });
        ui.horizontal(|ui| {
            if ui.button("加入圖片…").clicked() {
                if let Some(files) = rfd::FileDialog::new().add_filter("圖片", IMAGE_EXTS).pick_files() {
                    self.add_pages(files);
                }
            }
            if ui.button("加入資料夾…").clicked() {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    self.add_pages(vec![dir]);
                }
            }
            if ui.button("依檔名排序").clicked() {
                self.project.pages.sort_by(|a, b| a.path.cmp(&b.path));
            }
            if ui.button("清空").clicked() {
                self.project.pages.clear();
                self.project.output_dir = None;
                self.current = 0;
            }
            ui.weak("也可以直接把圖片或資料夾拖進視窗");
        });
        ui.separator();
        let mut action: Option<(usize, i32)> = None; // (index, -1 左移 / 1 右移 / 0 刪除)
        let n = self.project.pages.len();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for i in 0..n {
                    let path = self.project.pages[i].path.clone();
                    let name = self.project.pages[i].file_name();
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(170.0);
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.strong(format!("#{}", i + 1));
                                ui.label(egui::RichText::new(&name).small()).on_hover_text(path.display().to_string());
                            });
                            if self.thumb_widget(ui, &path, 160.0).double_clicked() {
                                self.current = i;
                                self.tab = Tab::Editor;
                            }
                            ui.horizontal(|ui| {
                                if ui.small_button("◀").clicked() && i > 0 {
                                    action = Some((i, -1));
                                }
                                if ui.small_button("▶").clicked() && i + 1 < n {
                                    action = Some((i, 1));
                                }
                                if ui.small_button("✕").clicked() {
                                    action = Some((i, 0));
                                }
                            });
                        });
                    });
                }
            });
        });
        if let Some((i, a)) = action {
            match a {
                -1 => self.project.pages.swap(i, i - 1),
                1 => self.project.pages.swap(i, i + 1),
                _ => {
                    self.project.pages.remove(i);
                }
            }
            self.preview = None;
        }
    }

    // ───────────── 分頁：設定 ─────────────

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("連線與輸出設定");
        ui.separator();
        let s = &mut self.settings;
        egui::Grid::new("settings").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("API 網址");
            ui.add(egui::TextEdit::singleline(&mut s.api_url).desired_width(520.0));
            ui.end_row();
            ui.label("模型");
            ui.add(egui::TextEdit::singleline(&mut s.model).desired_width(320.0));
            ui.end_row();
            ui.label("API 金鑰");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut s.api_key).password(true).desired_width(320.0));
                ui.checkbox(&mut s.remember_key, "記住金鑰（明文存在本機設定檔）");
            });
            ui.end_row();
            ui.label("");
            ui.weak("未填時會讀取環境變數 H200_API_KEY");
            ui.end_row();
            ui.label("每個角色參考圖張數");
            ui.add(egui::DragValue::new(&mut s.ref_per_character).range(1..=8));
            ui.end_row();
            ui.label("看圖每批張數");
            ui.add(egui::DragValue::new(&mut s.batch_size).range(1..=16));
            ui.end_row();
            ui.label("編劇每批格數");
            ui.add(egui::DragValue::new(&mut s.script_batch).range(1..=50));
            ui.end_row();
            ui.label("max_tokens");
            ui.add(egui::DragValue::new(&mut s.max_tokens).range(512..=32768));
            ui.end_row();
            ui.label("逾時（秒）");
            ui.add(egui::DragValue::new(&mut s.timeout_secs).range(30..=3600));
            ui.end_row();
            ui.label("字型（一般）");
            ui.add(egui::TextEdit::singleline(&mut s.font_path).desired_width(420.0));
            ui.end_row();
            ui.label("字型（粗體：大喊/狀聲詞）");
            ui.add(egui::TextEdit::singleline(&mut s.bold_font_path).desired_width(420.0));
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.button("重新載入字型").clicked() {
                self.reload_fonts();
            }
            if ui.button("儲存設定").clicked() {
                match self.settings.save() {
                    Ok(()) => self.push_log("設定已儲存"),
                    Err(e) => self.push_log(format!("設定儲存失敗：{e}")),
                }
            }
        });
        if let Some(e) = &self.font_error {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
    }

    // ───────────── 分頁：生成與編輯 ─────────────

    fn ui_editor(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // 上方工具列
        egui::Panel::top("gen_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                let busy = self.busy();
                let has_pages = !self.project.pages.is_empty();
                ui.add_enabled_ui(!busy && has_pages, |ui| {
                    if ui.button("▶ 全部生成（看圖＋編劇）").on_hover_text("重新分析所有畫面並撰寫台詞，會覆蓋現有對話框").clicked() {
                        self.start_job(&ctx, true, None);
                    }
                    if ui.button("只重寫全部台詞").on_hover_text("沿用已有的看圖分析，依目前風格重寫台詞（較快）").clicked() {
                        self.start_job(&ctx, false, None);
                    }
                    if ui.button("重寫本頁台詞").clicked() {
                        let c = self.current;
                        self.start_job(&ctx, false, Some((c, c + 1)));
                    }
                });
                if busy && ui.button("取消").clicked() {
                    self.cancel.store(true, Ordering::Relaxed);
                }
                if busy {
                    ui.add(egui::ProgressBar::new(self.progress).desired_width(260.0).text(&self.progress_text).animate(true));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let exporting = self.export_rx.is_some();
                    ui.add_enabled_ui(!exporting && has_pages, |ui| {
                        if ui.button("重新輸出全部").clicked() {
                            self.export(None);
                        }
                        if ui.button("重新輸出本頁").clicked() {
                            self.export(Some(self.current));
                        }
                    });
                    if let Some(dir) = self.project.output_dir.clone() {
                        if ui.small_button("變更…").clicked() {
                            if let Some(d) = rfd::FileDialog::new().set_title("選擇輸出資料夾").pick_folder() {
                                self.project.output_dir = Some(d);
                            }
                        }
                        if ui.small_button("開啟").clicked() {
                            let _ = std::fs::create_dir_all(&dir);
                            let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                        }
                        ui.weak(format!("輸出：{}", dir.display()));
                    }
                });
            });
        });

        // 左側頁面清單
        egui::Panel::left("page_list").resizable(true).default_size(150.0).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for i in 0..self.project.pages.len() {
                    let path = self.project.pages[i].path.clone();
                    let count = self.project.pages[i].bubbles.len();
                    let sel = i == self.current;
                    let frame = egui::Frame::new()
                        .inner_margin(4.0)
                        .corner_radius(4.0)
                        .fill(if sel { Color32::from_rgb(40, 70, 110) } else { Color32::TRANSPARENT });
                    let r = frame
                        .show(ui, |ui| {
                            ui.label(format!("#{}  {} 個框", i + 1, count));
                            self.thumb_widget(ui, &path, 120.0)
                        })
                        .inner;
                    if r.clicked() {
                        self.current = i;
                        self.selected = None;
                    }
                }
            });
        });

        // 右側屬性面板
        egui::Panel::right("inspector").resizable(true).default_size(300.0).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.ui_inspector(ui));
        });

        // 中央畫布
        egui::CentralPanel::default().show(ui, |ui| self.ui_canvas(ui, &ctx));
    }

    fn ui_inspector(&mut self, ui: &mut egui::Ui) {
        let Some(_) = self.project.pages.get(self.current) else {
            ui.label("請先到「頁面」分頁加入圖片。");
            return;
        };
        ui.heading(format!("第 {} 頁", self.current + 1));
        ui.horizontal_wrapped(|ui| {
            if ui.button("＋ 對話框").clicked() {
                self.add_bubble(BubbleKind::Speech, [0.5, 0.3]);
            }
            if ui.button("＋ 旁白").clicked() {
                self.add_bubble(BubbleKind::Narration, [0.5, 0.08]);
            }
            if ui.button("依台詞重新排版").on_hover_text("用目前頁面的台詞重新自動擺放（會重設位置）").clicked() {
                self.sync_script_from_bubbles();
                self.relayout_page(self.current);
            }
        });
        ui.weak("拖曳對話框移動；選取後拖曳黃色圓點調整尾巴；雙擊空白處新增；Delete 刪除。");
        ui.separator();

        let names: Vec<String> = self.project.characters.iter().map(|c| c.name.clone()).filter(|n| !n.is_empty()).collect();
        let mut changed = false;
        let mut delete = false;
        let mut reorder: i32 = 0;
        if let Some(b) = self.selected_bubble() {
            ui.strong("選取的文字框");
            egui::ComboBox::from_label("類型").selected_text(b.kind.label()).show_ui(ui, |ui| {
                for k in BubbleKind::ALL {
                    changed |= ui.selectable_value(&mut b.kind, k, k.label()).changed();
                }
            });
            if b.kind.has_speaker() {
                ui.horizontal(|ui| {
                    ui.label("說話者");
                    changed |= ui.add(egui::TextEdit::singleline(&mut b.speaker).desired_width(120.0)).changed();
                    ui.menu_button("▼", |ui| {
                        for n in &names {
                            if ui.button(n).clicked() {
                                b.speaker = n.clone();
                                changed = true;
                                ui.close();
                            }
                        }
                    });
                });
            }
            ui.label("文字（Enter 換行可手動斷行）");
            changed |= ui.add(egui::TextEdit::multiline(&mut b.text).desired_rows(3).desired_width(f32::INFINITY)).changed();
            changed |= ui.add(egui::Slider::new(&mut b.font_size, 0.6..=8.0).text("字級").step_by(0.05)).changed();
            ui.horizontal(|ui| {
                changed |= ui.checkbox(&mut b.vertical, "直排").changed();
                let mut has_tail = b.tail.is_some();
                if ui.checkbox(&mut has_tail, "尾巴").changed() {
                    b.tail = if has_tail { Some([b.pos[0], (b.pos[1] + 0.12).min(0.99)]) } else { None };
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                if ui.button("往前（先讀）").clicked() {
                    reorder = -1;
                }
                if ui.button("往後").clicked() {
                    reorder = 1;
                }
                if ui.button("刪除").clicked() {
                    delete = true;
                }
            });
        } else {
            ui.weak("點選畫面上的文字框來編輯。");
        }
        if changed {
            self.preview_dirty = true;
        }
        if delete {
            self.delete_selected();
        }
        if reorder != 0 {
            if let (Some(id), Some(page)) = (self.selected, self.project.pages.get_mut(self.current)) {
                if let Some(i) = page.bubbles.iter().position(|b| b.id == id) {
                    let j = i as i32 + reorder;
                    if j >= 0 && (j as usize) < page.bubbles.len() {
                        page.bubbles.swap(i, j as usize);
                        self.preview_dirty = true;
                    }
                }
            }
        }

        ui.separator();
        ui.strong("本頁文字（閱讀順序）");
        let list: Vec<(u64, String)> = self.project.pages[self.current]
            .bubbles
            .iter()
            .map(|b| {
                let who = if b.kind.has_speaker() && !b.speaker.is_empty() { format!("{}：", b.speaker) } else { String::new() };
                (b.id, format!("［{}］{}{}", b.kind.label(), who, b.text.replace('\n', " ")))
            })
            .collect();
        for (id, label) in list {
            if ui.selectable_label(self.selected == Some(id), label).clicked() {
                self.selected = Some(id);
            }
        }

        if let Some(a) = &self.project.pages[self.current].analysis {
            ui.separator();
            egui::CollapsingHeader::new("模型看圖結果").show(ui, |ui| {
                ui.label(format!("場景：{}", a.scene));
                ui.label(format!("氣氛：{}", a.mood));
                for c in &a.characters {
                    ui.label(format!("・{}：{}", c.name, c.expression));
                }
            });
        }

        ui.separator();
        egui::CollapsingHeader::new("執行紀錄").default_open(true).show(ui, |ui| {
            for l in self.log.iter().rev().take(40) {
                ui.label(egui::RichText::new(l).small());
            }
        });
    }

    fn sync_script_from_bubbles(&mut self) {
        if let Some(page) = self.project.pages.get_mut(self.current) {
            if !page.bubbles.is_empty() {
                page.script = page
                    .bubbles
                    .iter()
                    .map(|b| ScriptLine { kind: b.kind, speaker: b.speaker.clone(), text: b.text.clone() })
                    .collect();
            }
        }
    }

    fn add_bubble(&mut self, kind: BubbleKind, pos: [f32; 2]) {
        let id = self.project.fresh_id();
        let vertical = !matches!(kind, BubbleKind::Time | BubbleKind::Sfx) && self.settings.default_vertical;
        let b = Bubble {
            id,
            kind,
            speaker: String::new(),
            text: if kind == BubbleKind::Narration { "旁白".into() } else { "台詞".into() },
            pos,
            tail: if kind.has_speaker() { Some([pos[0], (pos[1] + 0.12).min(0.99)]) } else { None },
            font_size: self.settings.default_font_size,
            vertical,
        };
        if let Some(page) = self.project.pages.get_mut(self.current) {
            page.bubbles.push(b);
            self.selected = Some(id);
            self.preview_dirty = true;
        }
    }

    fn delete_selected(&mut self) {
        if let (Some(id), Some(page)) = (self.selected, self.project.pages.get_mut(self.current)) {
            page.bubbles.retain(|b| b.id != id);
            self.selected = None;
            self.preview_dirty = true;
        }
    }

    fn ui_canvas(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.project.pages.is_empty() {
            ui.centered_and_justified(|ui| ui.label("尚未加入頁面圖片"));
            return;
        }
        self.current = self.current.min(self.project.pages.len() - 1);
        self.ensure_preview(ctx);
        let (Some(tex), Some(pv), Some(fonts)) = (self.preview_tex.clone(), &self.preview, self.fonts.clone()) else {
            ui.label(&self.status);
            return;
        };
        let (iw, ih) = (pv.base.width() as f32, pv.base.height() as f32);
        let avail = ui.available_size();
        let k = (avail.x / iw).min(avail.y / ih).max(0.05);
        let size = Vec2::new(iw * k, ih * k);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);

        let to_norm = |p: Pos2| [((p.x - rect.min.x) / size.x), ((p.y - rect.min.y) / size.y)];
        let to_screen = |n: [f32; 2]| Pos2::new(rect.min.x + n[0] * size.x, rect.min.y + n[1] * size.y);
        let bubbles = self.project.pages[self.current].bubbles.clone();

        // 選取外框與尾巴把手
        let mut tail_handle: Option<Pos2> = None;
        if let Some(b) = bubbles.iter().find(|b| Some(b.id) == self.selected) {
            let g = render::geometry(&fonts, b, iw, ih);
            let r = Rect::from_center_size(
                Pos2::new(rect.min.x + g.cx * k, rect.min.y + g.cy * k),
                Vec2::new(g.rx * 2.0 * k, g.ry * 2.0 * k),
            );
            painter.rect_stroke(r.expand(3.0), 2.0, Stroke::new(2.0, Color32::from_rgb(80, 170, 255)), egui::StrokeKind::Outside);
            if let Some(t) = b.tail {
                let p = to_screen(t);
                painter.circle(p, 6.0, Color32::YELLOW, Stroke::new(1.5, Color32::BLACK));
                tail_handle = Some(p);
            }
        }

        let hit = |pos: Pos2| -> Option<u64> {
            let n = to_norm(pos);
            bubbles.iter().rev().find(|b| render::geometry(&fonts, b, iw, ih).contains(n[0] * iw, n[1] * ih, b.kind)).map(|b| b.id)
        };

        if resp.drag_started() {
            if let Some(pos) = resp.interact_pointer_pos() {
                if let (Some(h), Some(id)) = (tail_handle, self.selected) {
                    if h.distance(pos) < 10.0 {
                        self.drag = Some(Drag::Tail { id });
                    }
                }
                if self.drag.is_none() {
                    if let Some(id) = hit(pos) {
                        self.selected = Some(id);
                        let b = bubbles.iter().find(|b| b.id == id).unwrap();
                        let n = to_norm(pos);
                        self.drag = Some(Drag::Body { id, grab: [n[0] - b.pos[0], n[1] - b.pos[1]] });
                    }
                }
            }
        }
        if resp.dragged() {
            if let (Some(pos), Some(drag)) = (resp.interact_pointer_pos(), &self.drag) {
                let n = to_norm(pos);
                let n = [n[0].clamp(0.0, 1.0), n[1].clamp(0.0, 1.0)];
                let (id, is_tail, grab) = match drag {
                    Drag::Body { id, grab } => (*id, false, *grab),
                    Drag::Tail { id } => (*id, true, [0.0, 0.0]),
                };
                if let Some(b) = self.project.pages[self.current].bubbles.iter_mut().find(|b| b.id == id) {
                    if is_tail {
                        b.tail = Some(n);
                    } else {
                        b.pos = [n[0] - grab[0], n[1] - grab[1]];
                    }
                    self.preview_dirty = true;
                }
            }
        }
        if resp.drag_stopped() {
            self.drag = None;
        }
        if resp.double_clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                if hit(pos).is_none() {
                    self.add_bubble(BubbleKind::Speech, to_norm(pos));
                }
            }
        } else if resp.clicked() {
            self.selected = resp.interact_pointer_pos().and_then(hit);
        }
        if self.selected.is_some()
            && !ctx.egui_wants_keyboard_input()
            && ui.input(|i| i.key_pressed(egui::Key::Delete))
        {
            self.delete_selected();
        }
        // 左右鍵切換頁面
        if !ctx.egui_wants_keyboard_input() {
            let (prev, next) = ui.input(|i| (i.key_pressed(egui::Key::PageUp), i.key_pressed(egui::Key::PageDown)));
            if prev && self.current > 0 {
                self.current -= 1;
                self.selected = None;
            }
            if next && self.current + 1 < self.project.pages.len() {
                self.current += 1;
                self.selected = None;
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_thumbs(&ctx);
        self.poll_worker();
        self.poll_export();
        if self.busy() || self.export_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        // 拖放檔案
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.add_pages(dropped);
        }

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.strong("CG2Manga");
                ui.separator();
                if ui.button("新專案").clicked() && !self.busy() {
                    self.project = Project::default();
                    self.project_path = None;
                    self.preview = None;
                    self.selected = None;
                }
                if ui.button("開啟專案…").clicked() && !self.busy() {
                    self.open_project();
                }
                if ui.button("儲存專案").clicked() {
                    self.save_project(false);
                }
                if ui.button("另存新檔…").clicked() {
                    self.save_project(true);
                }
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Characters, format!("① 角色（{}）", self.project.characters.len()));
                ui.selectable_value(&mut self.tab, Tab::Story, "② 故事");
                ui.selectable_value(&mut self.tab, Tab::Pages, format!("③ 頁面（{}）", self.project.pages.len()));
                ui.selectable_value(&mut self.tab, Tab::Editor, "④ 生成與編輯");
                ui.selectable_value(&mut self.tab, Tab::Settings, "⚙ 設定");
            });
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.busy() {
                    ui.spinner();
                }
                ui.label(&self.status);
                if let Some(p) = &self.project_path {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.weak(p.display().to_string()));
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Characters => self.ui_characters(ui),
            Tab::Story => self.ui_story(ui),
            Tab::Pages => self.ui_pages(ui),
            Tab::Editor => self.ui_editor(ui),
            Tab::Settings => self.ui_settings(ui),
        });
    }

    fn on_exit(&mut self) {
        let _ = self.settings.save();
    }
}
