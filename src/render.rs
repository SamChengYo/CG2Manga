//! 漫畫對話框繪製：字型載入、直排/橫排文字排版、各種框型與尾巴。
//! 預覽與匯出共用同一套繪製程式，確保畫面一致。

use crate::model::{Bubble, BubbleKind};
use ab_glyph::{Font, FontVec, OutlineCurve, PxScale, ScaleFont};
use anyhow::{Context, Result, anyhow};
use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

pub struct Fonts {
    pub regular: FontVec,
    pub bold: FontVec,
}

const FALLBACK_FONTS: &[&str] = &[
    r"C:\Windows\Fonts\msjh.ttc",
    r"C:\Windows\Fonts\msjhl.ttc",
    r"C:\Windows\Fonts\mingliu.ttc",
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\simsun.ttc",
];

fn load_font(path: &str) -> Result<FontVec> {
    let data = std::fs::read(path).with_context(|| format!("讀取字型失敗：{path}"))?;
    FontVec::try_from_vec_and_index(data, 0).map_err(|e| anyhow!("字型格式錯誤 {path}: {e}"))
}

impl Fonts {
    pub fn load(regular: &str, bold: &str) -> Result<Self> {
        let regular_font = load_font(regular).or_else(|e| {
            FALLBACK_FONTS
                .iter()
                .find_map(|p| load_font(p).ok())
                .ok_or(e)
        })?;
        let bold_font = match load_font(bold) {
            Ok(f) => f,
            Err(_) => load_font(regular).or_else(|e| {
                FALLBACK_FONTS.iter().find_map(|p| load_font(p).ok()).ok_or(e)
            })?,
        };
        Ok(Self { regular: regular_font, bold: bold_font })
    }

    fn for_kind(&self, kind: BubbleKind) -> &FontVec {
        match kind {
            BubbleKind::Shout | BubbleKind::Sfx | BubbleKind::Time => &self.bold,
            _ => &self.regular,
        }
    }
}

// ───────────────────────── 文字排版 ─────────────────────────

/// 不可放在行首的標點（簡易避頭點）
fn no_line_start(c: char) -> bool {
    "，。、！？；：」』）》〉…～—,.!?;:)]】".contains(c)
}

/// 直排時替換成直排標點
fn vertical_form(c: char) -> char {
    match c {
        '，' | ',' => '︐',
        '、' => '︑',
        '。' => '︒',
        '：' | ':' => '︓',
        '；' | ';' => '︔',
        '！' | '!' => '︕',
        '？' | '?' => '︖',
        '「' => '﹁',
        '」' => '﹂',
        '『' => '﹃',
        '』' => '﹄',
        '（' | '(' => '︵',
        '）' | ')' => '︶',
        '《' => '︽',
        '》' => '︾',
        '【' => '︻',
        '】' => '︼',
        '…' => '︙',
        '—' | '─' | 'ー' => '︱',
        '～' | '~' => '≀',
        _ => c,
    }
}

fn wrap(text: &str, max_len: usize) -> Vec<Vec<char>> {
    let max_len = max_len.max(1);
    let mut out = Vec::new();
    for para in text.split('\n') {
        let chars: Vec<char> = para.chars().filter(|c| *c != '\r').collect();
        if chars.is_empty() {
            continue;
        }
        let mut line: Vec<char> = Vec::new();
        for c in chars {
            if line.len() >= max_len && !no_line_start(c) {
                out.push(std::mem::take(&mut line));
            }
            line.push(c);
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    if out.is_empty() {
        out.push(vec![' ']);
    }
    out
}

pub struct TextLayout {
    pub lines: Vec<Vec<char>>,
    pub vertical: bool,
    pub px: f32,
    pub w: f32,
    pub h: f32,
}

const COL_GAP: f32 = 1.12; // 直排欄距（以字級為單位）
const CELL_H: f32 = 1.04; // 直排每字高度
const LINE_H: f32 = 1.28; // 橫排行高

fn char_count(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace()).count().max(1)
}

fn line_len_for(kind: BubbleKind, text: &str, vertical: bool) -> usize {
    let n = char_count(text) as f32;
    match (kind, vertical) {
        (BubbleKind::Sfx, _) => 8,
        (BubbleKind::Narration | BubbleKind::Time, true) => {
            ((n * 2.5).sqrt().ceil() as usize).clamp((n as usize).min(5), 11)
        }
        (BubbleKind::Narration | BubbleKind::Time, false) => {
            ((n * 4.0).sqrt().ceil() as usize).clamp((n as usize).min(6), 16)
        }
        (_, true) => ((n * 1.6).sqrt().ceil() as usize).clamp((n as usize).min(4), 9),
        (_, false) => ((n * 2.5).sqrt().ceil() as usize).clamp((n as usize).min(5), 12),
    }
}

fn advance(font: &FontVec, px: f32, c: char) -> f32 {
    let scaled = font.as_scaled(PxScale::from(px));
    scaled.h_advance(font.glyph_id(c))
}

pub fn layout_text(font: &FontVec, kind: BubbleKind, text: &str, px: f32, vertical: bool) -> TextLayout {
    let lines = wrap(text, line_len_for(kind, text, vertical));
    let (w, h) = if vertical {
        let cols = lines.len() as f32;
        let longest = lines.iter().map(|l| l.len()).max().unwrap_or(1) as f32;
        ((cols - 1.0) * px * COL_GAP + px, longest * px * CELL_H)
    } else {
        let widest = lines
            .iter()
            .map(|l| l.iter().map(|c| advance(font, px, *c)).sum::<f32>())
            .fold(0.0f32, f32::max);
        let rows = lines.len() as f32;
        (widest.max(px), (rows - 1.0) * px * LINE_H + px)
    };
    TextLayout { lines, vertical, px, w, h }
}

// ───────────────────────── 幾何 ─────────────────────────

pub struct Geom {
    pub cx: f32,
    pub cy: f32,
    /// 外框半寬／半高（橢圓半徑或矩形半邊）
    pub rx: f32,
    pub ry: f32,
    pub layout: TextLayout,
    pub tail: Option<(f32, f32)>,
}

impl Geom {
    pub fn contains(&self, x: f32, y: f32, kind: BubbleKind) -> bool {
        let dx = (x - self.cx) / self.rx.max(1.0);
        let dy = (y - self.cy) / self.ry.max(1.0);
        match kind {
            BubbleKind::Narration | BubbleKind::Time | BubbleKind::Sfx => dx.abs() <= 1.0 && dy.abs() <= 1.0,
            _ => dx * dx + dy * dy <= 1.0,
        }
    }
}

pub fn base_px(w: f32, h: f32) -> f32 {
    (w * h).sqrt()
}

pub fn font_px(b: &Bubble, w: f32, h: f32) -> f32 {
    let mult = match b.kind {
        BubbleKind::Time => 0.85,
        BubbleKind::Narration => 0.92,
        BubbleKind::Sfx => 1.9,
        BubbleKind::Shout => 1.1,
        _ => 1.0,
    };
    (b.font_size / 100.0 * base_px(w, h) * mult).max(6.0)
}

pub fn geometry(fonts: &Fonts, b: &Bubble, w: f32, h: f32) -> Geom {
    let px = font_px(b, w, h);
    let layout = layout_text(fonts.for_kind(b.kind), b.kind, &b.text, px, b.vertical);
    let (hw, hh) = (layout.w / 2.0, layout.h / 2.0);
    let (rx, ry) = match b.kind {
        BubbleKind::Speech => (hw * 1.3 + px * 0.8, hh * 1.25 + px * 0.6),
        BubbleKind::Thought => (hw * 1.35 + px * 0.8, hh * 1.35 + px * 0.8),
        BubbleKind::Shout => (hw * 1.45 + px * 0.9, hh * 1.45 + px * 0.9),
        BubbleKind::Narration => (hw + px * 0.6, hh + px * 0.6),
        BubbleKind::Time => (hw + px * 0.5, hh + px * 0.4),
        BubbleKind::Sfx => (hw + px * 0.2, hh + px * 0.2),
    };
    Geom {
        cx: b.pos[0] * w,
        cy: b.pos[1] * h,
        rx,
        ry,
        layout,
        tail: b.tail.map(|t| (t[0] * w, t[1] * h)),
    }
}

// ───────────────────────── 繪製 ─────────────────────────

fn paint(r: u8, g: u8, b: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(r, g, b, 255);
    p.anti_alias = true;
    p
}

fn stroke(width: f32, join: LineJoin) -> Stroke {
    Stroke { width, line_join: join, line_cap: LineCap::Round, miter_limit: 4.0, ..Stroke::default() }
}

fn oval(cx: f32, cy: f32, rx: f32, ry: f32) -> Option<Path> {
    PathBuilder::from_oval(Rect::from_xywh(cx - rx, cy - ry, rx * 2.0, ry * 2.0)?)
}

fn rect(cx: f32, cy: f32, rx: f32, ry: f32) -> Option<Path> {
    Some(PathBuilder::from_rect(Rect::from_xywh(cx - rx, cy - ry, rx * 2.0, ry * 2.0)?))
}

fn triangle_tail(g: &Geom) -> Option<Path> {
    let (tx, ty) = g.tail?;
    let (dx, dy) = (tx - g.cx, ty - g.cy);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return None;
    }
    let (px, py) = (-dy / len, dx / len);
    let half = g.rx.min(g.ry) * 0.26;
    // 底邊稍微往尾巴方向偏移，讓尾巴從框邊緣自然長出
    let (bx, by) = (g.cx + dx / len * g.rx.min(g.ry) * 0.3, g.cy + dy / len * g.rx.min(g.ry) * 0.3);
    let mut pb = PathBuilder::new();
    pb.move_to(bx + px * half, by + py * half);
    // 微彎的尾巴
    let (mx, my) = ((bx + tx) / 2.0 + px * half * 0.25, (by + ty) / 2.0 + py * half * 0.25);
    pb.quad_to(mx, my, tx, ty);
    pb.quad_to(mx - px * half * 0.35, my - py * half * 0.35, bx - px * half, by - py * half);
    pb.close();
    pb.finish()
}

fn hash(a: u64, b: u64) -> f32 {
    let mut x = a.wrapping_mul(0x9E3779B97F4A7C15) ^ b.wrapping_mul(0xC2B2AE3D27D4EB4F);
    x ^= x >> 31;
    x = x.wrapping_mul(0xBF58476D1CE4E5B9);
    x ^= x >> 29;
    (x % 10_000) as f32 / 10_000.0
}

fn spiky(g: &Geom, id: u64) -> Option<Path> {
    let n = 16usize;
    let mut pb = PathBuilder::new();
    for i in 0..(n * 2) {
        let a = i as f32 / (n * 2) as f32 * std::f32::consts::TAU;
        let jitter = hash(id, i as u64) * 0.14 - 0.07;
        let r = if i % 2 == 0 { 1.18 + jitter } else { 0.9 + jitter * 0.5 };
        let (x, y) = (g.cx + a.cos() * g.rx * r, g.cy + a.sin() * g.ry * r);
        if i == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    pb.close();
    pb.finish()
}

fn cloud_circles(g: &Geom, px: f32) -> Vec<Path> {
    let (a, b) = (g.rx * 0.86, g.ry * 0.86);
    let perimeter = std::f32::consts::PI * (3.0 * (a + b) - ((3.0 * a + b) * (a + 3.0 * b)).sqrt());
    let n = ((perimeter / (px * 1.7)).round() as usize).clamp(7, 30);
    let r = perimeter / n as f32 * 0.62;
    let mut v = Vec::new();
    for i in 0..n {
        let t = i as f32 / n as f32 * std::f32::consts::TAU;
        if let Some(p) = oval(g.cx + t.cos() * a, g.cy + t.sin() * b, r, r) {
            v.push(p);
        }
    }
    if let Some(p) = oval(g.cx, g.cy, a, b) {
        v.push(p);
    }
    v
}

fn thought_dots(g: &Geom, px: f32) -> Vec<(f32, f32, f32)> {
    let Some((tx, ty)) = g.tail else { return Vec::new() };
    let (dx, dy) = (tx - g.cx, ty - g.cy);
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let (ux, uy) = (dx / len, dy / len);
    // 框邊緣的位置
    let edge = {
        let k = 1.0 / ((ux / g.rx).powi(2) + (uy / g.ry).powi(2)).sqrt();
        k
    };
    let start = edge + px * 0.3;
    let span = (len - start).max(px);
    [(0.25, 0.42), (0.6, 0.3), (0.95, 0.2)]
        .iter()
        .map(|(f, r)| (g.cx + ux * (start + span * f), g.cy + uy * (start + span * f), px * r))
        .collect()
}

fn add_glyph(
    pb: &mut PathBuilder,
    font: &FontVec,
    px: f32,
    c: char,
    x: f32,
    baseline: f32,
) {
    let scaled = font.as_scaled(PxScale::from(px));
    let (sx, sy) = (scaled.h_scale_factor(), scaled.v_scale_factor());
    let id = font.glyph_id(c);
    let Some(outline) = font.outline(id) else { return };
    let map = |p: ab_glyph::Point| (x + p.x * sx, baseline - p.y * sy);
    let mut last: Option<(f32, f32)> = None;
    for curve in outline.curves {
        let (p0, rest): ((f32, f32), Vec<(f32, f32)>) = match curve {
            OutlineCurve::Line(a, b) => (map(a), vec![map(b)]),
            OutlineCurve::Quad(a, b, c) => (map(a), vec![map(b), map(c)]),
            OutlineCurve::Cubic(a, b, c, d) => (map(a), vec![map(b), map(c), map(d)]),
        };
        let need_move = match last {
            Some(l) => (l.0 - p0.0).abs() > 0.01 || (l.1 - p0.1).abs() > 0.01,
            None => true,
        };
        if need_move {
            if last.is_some() {
                pb.close();
            }
            pb.move_to(p0.0, p0.1);
        }
        match rest.as_slice() {
            [b] => pb.line_to(b.0, b.1),
            [b, c] => pb.quad_to(b.0, b.1, c.0, c.1),
            [b, c, d] => pb.cubic_to(b.0, b.1, c.0, c.1, d.0, d.1),
            _ => {}
        }
        last = rest.last().copied();
    }
    if last.is_some() {
        pb.close();
    }
}

fn text_path(font: &FontVec, layout: &TextLayout, cx: f32, cy: f32) -> Option<Path> {
    let px = layout.px;
    let scaled = font.as_scaled(PxScale::from(px));
    let ascent = scaled.ascent();
    let (ox, oy) = (cx - layout.w / 2.0, cy - layout.h / 2.0);
    let mut pb = PathBuilder::new();
    if layout.vertical {
        for (ci, col) in layout.lines.iter().enumerate() {
            // 直排由右至左
            let col_center = ox + layout.w - px / 2.0 - ci as f32 * px * COL_GAP;
            for (ri, &c) in col.iter().enumerate() {
                let vc = vertical_form(c);
                let ch = if vc != c && font.glyph_id(vc).0 != 0 { vc } else { c };
                let adv = scaled.h_advance(font.glyph_id(ch));
                let top = oy + ri as f32 * px * CELL_H;
                add_glyph(&mut pb, font, px, ch, col_center - adv / 2.0, top + ascent);
            }
        }
    } else {
        for (li, line) in layout.lines.iter().enumerate() {
            let lw: f32 = line.iter().map(|c| scaled.h_advance(font.glyph_id(*c))).sum();
            let mut x = ox + (layout.w - lw) / 2.0;
            let base = oy + li as f32 * px * LINE_H + ascent;
            for &c in line {
                add_glyph(&mut pb, font, px, c, x, base);
                x += scaled.h_advance(font.glyph_id(c));
            }
        }
    }
    pb.finish()
}

pub fn draw_bubble(pm: &mut Pixmap, fonts: &Fonts, b: &Bubble, w: f32, h: f32) {
    if b.text.trim().is_empty() {
        return;
    }
    let g = geometry(fonts, b, w, h);
    let px = g.layout.px;
    let sw = (px * 0.085).max(1.5);
    let black = paint(10, 10, 10);
    let white = paint(255, 255, 255);
    let t = Transform::identity();
    let font = fonts.for_kind(b.kind);

    match b.kind {
        BubbleKind::Speech | BubbleKind::Shout => {
            let body = if b.kind == BubbleKind::Speech { oval(g.cx, g.cy, g.rx, g.ry) } else { spiky(&g, b.id) };
            let tail = triangle_tail(&g);
            let join = if b.kind == BubbleKind::Shout { LineJoin::Miter } else { LineJoin::Round };
            // 先畫加粗外框，再填白，外框只留在外側 → 框與尾巴無縫相連
            if let Some(p) = &body {
                pm.stroke_path(p, &black, &stroke(sw * 2.0, join), t, None);
            }
            if let Some(p) = &tail {
                pm.stroke_path(p, &black, &stroke(sw * 2.0, LineJoin::Round), t, None);
            }
            if let Some(p) = &body {
                pm.fill_path(p, &white, FillRule::Winding, t, None);
            }
            if let Some(p) = &tail {
                pm.fill_path(p, &white, FillRule::Winding, t, None);
            }
        }
        BubbleKind::Thought => {
            let circles = cloud_circles(&g, px);
            for p in &circles {
                pm.stroke_path(p, &black, &stroke(sw * 2.0, LineJoin::Round), t, None);
            }
            for p in &circles {
                pm.fill_path(p, &white, FillRule::Winding, t, None);
            }
            for (x, y, r) in thought_dots(&g, px) {
                if let Some(p) = oval(x, y, r, r) {
                    pm.fill_path(&p, &white, FillRule::Winding, t, None);
                    pm.stroke_path(&p, &black, &stroke(sw, LineJoin::Round), t, None);
                }
            }
        }
        BubbleKind::Narration => {
            if let Some(p) = rect(g.cx, g.cy, g.rx, g.ry) {
                pm.fill_path(&p, &white, FillRule::Winding, t, None);
                pm.stroke_path(&p, &black, &stroke(sw, LineJoin::Miter), t, None);
            }
        }
        BubbleKind::Time => {
            if let Some(p) = rect(g.cx, g.cy, g.rx, g.ry) {
                pm.fill_path(&p, &paint(20, 20, 20), FillRule::Winding, t, None);
            }
        }
        BubbleKind::Sfx => {}
    }

    if let Some(p) = text_path(font, &g.layout, g.cx, g.cy) {
        match b.kind {
            BubbleKind::Time => {
                pm.fill_path(&p, &white, FillRule::Winding, t, None);
            }
            BubbleKind::Sfx => {
                pm.stroke_path(&p, &white, &stroke(px * 0.28, LineJoin::Round), t, None);
                pm.fill_path(&p, &black, FillRule::Winding, t, None);
            }
            _ => {
                pm.fill_path(&p, &black, FillRule::Winding, t, None);
            }
        }
    }
}

/// 把對話框畫到圖片上，回傳新圖片
pub fn compose(base: &image::RgbaImage, bubbles: &[Bubble], fonts: &Fonts) -> image::RgbaImage {
    let (w, h) = base.dimensions();
    let Some(mut pm) = Pixmap::new(w.max(1), h.max(1)) else { return base.clone() };
    {
        let data = pm.data_mut();
        for (dst, src) in data.chunks_exact_mut(4).zip(base.as_raw().chunks_exact(4)) {
            // 透明背景以白色合成，變成不透明圖
            let a = src[3] as u32;
            for i in 0..3 {
                dst[i] = ((src[i] as u32 * a + 255 * (255 - a)) / 255) as u8;
            }
            dst[3] = 255;
        }
    }
    for b in bubbles {
        draw_bubble(&mut pm, fonts, b, w as f32, h as f32);
    }
    image::RgbaImage::from_raw(w, h, pm.take()).unwrap_or_else(|| base.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    #[test]
    fn render_sample() {
        let fonts = Fonts::load(r"C:\Windows\Fonts\msjh.ttc", r"C:\Windows\Fonts\msjhbd.ttc").unwrap();
        let (w, h) = (1200u32, 1700u32);
        let base = image::RgbaImage::from_fn(w, h, |x, y| {
            let v = (120 + (x / 6 + y / 9) % 100) as u8;
            image::Rgba([v, v.saturating_sub(20), 180, 255])
        });
        let mk = |id, kind, text: &str, pos: [f32; 2], tail: Option<[f32; 2]>, vertical| Bubble {
            id, kind, speaker: "A".into(), text: text.into(), pos, tail, font_size: 2.2, vertical,
        };
        let bubbles = vec![
            mk(1, BubbleKind::Time, "三天後・放學後的屋頂", [0.8, 0.04], None, false),
            mk(2, BubbleKind::Speech, "你、你怎麼會在這裡？", [0.8, 0.25], Some([0.65, 0.45]), true),
            mk(3, BubbleKind::Speech, "我一直在等你喔……", [0.6, 0.22], Some([0.5, 0.42]), true),
            mk(4, BubbleKind::Thought, "糟了，被發現了！", [0.25, 0.3], Some([0.35, 0.5]), true),
            mk(5, BubbleKind::Shout, "不要過來！", [0.3, 0.62], Some([0.5, 0.75]), true),
            mk(6, BubbleKind::Narration, "那天的夕陽，比平常更紅。", [0.5, 0.92], None, false),
            mk(7, BubbleKind::Sfx, "砰！", [0.75, 0.7], None, false),
            mk(8, BubbleKind::Speech, "Hello, 這是橫排測試。", [0.7, 0.5], Some([0.9, 0.6]), false),
        ];
        let out = compose(&base, &bubbles, &fonts);
        let path = std::env::var("RENDER_OUT").unwrap_or_else(|_| "render_sample.png".into());
        out.save(path).unwrap();
    }
}

