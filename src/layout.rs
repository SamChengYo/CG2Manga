//! 自動擺放：依照角色臉部位置、空白區域、閱讀順序，把台詞放成對話框。

use crate::model::{Bubble, BubbleKind, PanelAnalysis, ScriptLine, Settings};
use crate::render::{Fonts, geometry};

struct Placed {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

fn overlap(a: &Placed, b: &Placed) -> f32 {
    let w = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
    let h = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
    w * h
}

fn names_match(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim().to_lowercase(), b.trim().to_lowercase());
    !a.is_empty() && !b.is_empty() && (a == b || a.contains(&b) || b.contains(&a))
}

/// 閱讀順序分數：越小越先讀
fn reading_rank(x: f32, y: f32, rtl: bool) -> f32 {
    let x = if rtl { 1.0 - x } else { x };
    y * 1.0 + x * 0.55
}

pub fn auto_layout(
    fonts: &Fonts,
    img_w: f32,
    img_h: f32,
    analysis: Option<&PanelAnalysis>,
    lines: &[ScriptLine],
    settings: &Settings,
    mut next_id: impl FnMut() -> u64,
) -> Vec<Bubble> {
    let faces: Vec<(String, f32, f32)> = analysis
        .map(|a| {
            a.characters
                .iter()
                .filter_map(|c| c.face.map(|f| (c.name.clone(), f[0] * img_w, f[1] * img_h)))
                .collect()
        })
        .unwrap_or_default();
    let empties: Vec<[f32; 4]> = analysis.map(|a| a.empty_areas.clone()).unwrap_or_default();
    let face_r = img_w.min(img_h) * 0.09;
    let diag = (img_w * img_w + img_h * img_h).sqrt();

    let mut placed: Vec<Placed> = Vec::new();
    let mut out = Vec::new();
    let mut prev_rank = -1.0f32;

    for line in lines {
        if line.text.trim().is_empty() {
            continue;
        }
        let vertical = match line.kind {
            BubbleKind::Sfx => false,
            BubbleKind::Time => false,
            _ => settings.default_vertical,
        };
        let mut b = Bubble {
            id: next_id(),
            kind: line.kind,
            speaker: line.speaker.clone(),
            text: line.text.clone(),
            pos: [0.5, 0.5],
            tail: None,
            font_size: settings.default_font_size,
            vertical,
        };
        let mut g = geometry(fonts, &b, img_w, img_h);
        // 太大就縮字
        while (g.rx * 2.0 > img_w * 0.6 || g.ry * 2.0 > img_h * 0.6) && b.font_size > 1.0 {
            b.font_size *= 0.88;
            g = geometry(fonts, &b, img_w, img_h);
        }
        let (rx, ry) = (g.rx * 1.12, g.ry * 1.12); // 外加尖刺/雲朵/邊框的餘裕
        let margin = img_w.min(img_h) * 0.015;
        let speaker_face = if line.kind.has_speaker() {
            faces.iter().find(|(n, _, _)| names_match(n, &line.speaker)).map(|(_, x, y)| (*x, *y))
        } else {
            None
        };

        let (x_lo, x_hi) = (rx + margin, (img_w - rx - margin).max(rx + margin));
        let (y_lo, y_hi) = (ry + margin, (img_h - ry - margin).max(ry + margin));
        let steps = 14;
        let mut best = (f32::MAX, img_w / 2.0, img_h / 2.0);
        for iy in 0..=steps {
            for ix in 0..=steps {
                let cx = x_lo + (x_hi - x_lo) * ix as f32 / steps as f32;
                let cy = y_lo + (y_hi - y_lo) * iy as f32 / steps as f32;
                let me = Placed { x0: cx - rx, y0: cy - ry, x1: cx + rx, y1: cy + ry };
                let area = (rx * ry * 4.0).max(1.0);
                let mut score = 0.0f32;

                // 與既有對話框重疊
                for p in &placed {
                    score += overlap(&me, p) / area * 12.0;
                }
                // 擋到任何人的臉
                for (_, fx, fy) in &faces {
                    let fp = Placed { x0: fx - face_r, y0: fy - face_r, x1: fx + face_r, y1: fy + face_r };
                    score += overlap(&me, &fp) / (face_r * face_r * 4.0) * 6.0;
                }
                // 放在模型建議的空白處
                let (nx, ny) = (cx / img_w, cy / img_h);
                if empties.iter().any(|e| nx >= e[0] && nx <= e[2] && ny >= e[1] && ny <= e[3]) {
                    score -= 0.6;
                }

                match line.kind {
                    BubbleKind::Speech | BubbleKind::Thought | BubbleKind::Shout => {
                        if let Some((fx, fy)) = speaker_face {
                            let d = ((cx - fx).powi(2) + (cy - fy).powi(2)).sqrt() / diag;
                            score += d * 4.0;
                            // 偏好放在頭部上方或側邊，不要放在身體下方
                            let head_top = fy - face_r * 0.3;
                            if cy > head_top {
                                score += (cy - head_top) / img_h * 5.0;
                            }
                        } else {
                            // 不知道是誰：偏好靠上方
                            score += ny * 1.2;
                        }
                    }
                    BubbleKind::Time => {
                        // 放在閱讀起點的角落
                        let corner_x = if settings.reading_rtl { img_w - rx - margin } else { rx + margin };
                        score += ((cx - corner_x).abs() / img_w + (cy - (ry + margin)).abs() / img_h) * 6.0;
                    }
                    BubbleKind::Narration => {
                        let edge = (cy - ry - margin).min(img_h - margin - ry - cy).max(0.0) / img_h;
                        score += edge * 5.0;
                    }
                    BubbleKind::Sfx => {
                        score += (0.5 - ((nx - 0.5).abs() + (ny - 0.5).abs())).max(0.0) * 0.5;
                    }
                }

                // 閱讀順序：後面的台詞要在前面台詞之後
                let rank = reading_rank(nx, ny, settings.reading_rtl);
                if prev_rank >= 0.0 && rank < prev_rank {
                    score += (prev_rank - rank) * 6.0 + 0.3;
                }

                if score < best.0 {
                    best = (score, cx, cy);
                }
            }
        }

        let (_, cx, cy) = best;
        b.pos = [cx / img_w, cy / img_h];
        if let Some((fx, fy)) = speaker_face {
            // 尾巴停在臉與框之間，不要戳到臉上
            let (dx, dy) = (fx - cx, fy - cy);
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            let (ux, uy) = (dx / len, dy / len);
            let edge = 1.0 / ((ux / g.rx).powi(2) + (uy / g.ry).powi(2)).sqrt();
            let reach = (len - face_r * 0.9).max(edge + g.layout.px * 0.8).min(edge + img_w.min(img_h) * 0.12);
            b.tail = Some([(cx + ux * reach) / img_w, (cy + uy * reach) / img_h]);
        } else if line.kind.has_speaker() {
            // 找不到說話者：給一個朝下的短尾巴
            b.tail = Some([cx / img_w, (cy + g.ry + g.layout.px * 1.2).min(img_h - 2.0) / img_h]);
        }
        prev_rank = reading_rank(b.pos[0], b.pos[1], settings.reading_rtl);
        placed.push(Placed { x0: cx - rx, y0: cy - ry, x1: cx + rx, y1: cy + ry });
        out.push(b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    #[test]
    fn layout_sample() {
        let fonts = Fonts::load(r"C:\Windows\Fonts\msjh.ttc", r"C:\Windows\Fonts\msjhbd.ttc").unwrap();
        let (w, h) = (1200u32, 1700u32);
        let analysis = PanelAnalysis {
            scene: "兩人在屋頂對峙".into(),
            mood: "緊張".into(),
            characters: vec![
                SeenCharacter { name: "小美".into(), face: Some([0.3, 0.45]), expression: "驚訝".into() },
                SeenCharacter { name: "阿傑".into(), face: Some([0.72, 0.5]), expression: "微笑".into() },
            ],
            empty_areas: vec![[0.0, 0.0, 1.0, 0.25]],
        };
        let l = |k, s: &str, t: &str| ScriptLine { kind: k, speaker: s.into(), text: t.into() };
        let script = vec![
            l(BubbleKind::Time, "", "放學後"),
            l(BubbleKind::Speech, "阿傑", "終於等到你了。"),
            l(BubbleKind::Speech, "阿傑", "有件事，我一定要告訴你。"),
            l(BubbleKind::Thought, "小美", "咦……他該不會要……"),
            l(BubbleKind::Speech, "小美", "什、什麼事？"),
            l(BubbleKind::Narration, "", "風吹過屋頂，兩人之間一片沉默。"),
        ];
        let mut id = 0;
        let bubbles = auto_layout(&fonts, w as f32, h as f32, Some(&analysis), &script, &Settings::default(), || { id += 1; id });
        let mut base = image::RgbaImage::from_pixel(w, h, image::Rgba([170, 190, 210, 255]));
        for c in &analysis.characters {
            let f = c.face.unwrap();
            let (cx, cy) = ((f[0] * w as f32) as i32, (f[1] * h as f32) as i32);
            for y in -90..90 { for x in -90..90 { if x * x + y * y < 8100 {
                base.put_pixel((cx + x) as u32, (cy + y) as u32, image::Rgba([230, 180, 150, 255]));
            }}}
        }
        let out = crate::render::compose(&base, &bubbles, &fonts);
        out.save(std::env::var("LAYOUT_OUT").unwrap_or_else(|_| "layout_sample.png".into())).unwrap();
    }
}
