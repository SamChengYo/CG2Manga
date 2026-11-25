//! 與 H200 Ollama（OpenAI 相容 API）溝通，執行兩階段生成：
//! 1. 看圖分析：分批送畫面＋角色參考圖，取得每格的場景、角色、臉部位置、空白處。
//! 2. 編劇：把全部分析（純文字）一次給模型，依風格寫出整體連貫的台詞。

use crate::model::{BubbleKind, Character, PanelAnalysis, ScriptLine, SeenCharacter, Settings, Story};
use anyhow::{Result, anyhow, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

pub enum WorkerMsg {
    Log(String),
    Progress(f32, String),
    Analysis(usize, PanelAnalysis),
    Script(usize, Vec<ScriptLine>),
    Finished,
    Failed(String),
}

pub struct Job {
    pub settings: Settings,
    pub characters: Vec<Character>,
    pub story: Story,
    pub pages: Vec<PathBuf>,
    /// 已有的分析（None 表示需要重新看圖）
    pub analyses: Vec<Option<PanelAnalysis>>,
    /// 只重寫這個範圍的台詞（含頭不含尾）；None 表示全部
    pub script_range: Option<(usize, usize)>,
    /// 前面已寫好的台詞，用來接續（只在 script_range 時使用）
    pub existing_script: Vec<Vec<ScriptLine>>,
    pub cancel: Arc<AtomicBool>,
}

// ───────────────────────── HTTP ─────────────────────────

struct Client {
    http: reqwest::blocking::Client,
    settings: Settings,
}

impl Client {
    fn new(settings: &Settings) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(settings.timeout_secs))
            .build()?;
        Ok(Self { http, settings: settings.clone() })
    }

    fn chat(&self, system: &str, content: Vec<Value>) -> Result<String> {
        let body = json!({
            "model": self.settings.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": content},
            ],
            "max_tokens": self.settings.max_tokens,
            "reasoning_effort": "none",
            "temperature": 0.7,
            "stream": false,
        });
        let resp = self
            .http
            .post(&self.settings.api_url)
            .bearer_auth(self.settings.api_key.trim())
            .json(&body)
            .send()?;
        let status = resp.status();
        let text = resp.text()?;
        if !status.is_success() {
            let hint = match status.as_u16() {
                401 => "（請檢查 API 金鑰）",
                413 => "（請求太大，請在設定中調小批次或圖片尺寸）",
                _ => "",
            };
            let snippet: String = text.chars().take(400).collect();
            bail!("HTTP {status}{hint}: {snippet}");
        }
        let v: Value = serde_json::from_str(&text)?;
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("回應中沒有 choices[0].message.content"))?;
        Ok(content.to_string())
    }
}

/// 以原圖原尺寸送出（不縮圖）。JPEG/PNG/WebP 直接送原始檔案；其他格式轉成 PNG。
fn encode_image(path: &Path) -> Result<Value> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        _ => None,
    };
    let (mime, bytes) = match mime {
        Some(m) => (m, std::fs::read(path).map_err(|e| anyhow!("無法讀取圖片 {}: {e}", path.display()))?),
        None => {
            let img = image::open(path).map_err(|e| anyhow!("無法讀取圖片 {}: {e}", path.display()))?;
            let mut buf = std::io::Cursor::new(Vec::new());
            img.write_to(&mut buf, image::ImageFormat::Png)?;
            ("image/png", buf.into_inner())
        }
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{b64}")}}))
}

fn text(s: impl Into<String>) -> Value {
    json!({"type": "text", "text": s.into()})
}

/// 從模型輸出中擷取 JSON（去掉 <think>、```json 等）
fn extract_json(raw: &str) -> Result<Value> {
    let mut s = raw.to_string();
    while let (Some(a), Some(b)) = (s.find("<think>"), s.find("</think>")) {
        if b < a {
            break;
        }
        s.replace_range(a..b + "</think>".len(), "");
    }
    let start = s.find('{').ok_or_else(|| anyhow!("輸出中找不到 JSON"))?;
    let end = s.rfind('}').ok_or_else(|| anyhow!("輸出中找不到 JSON 結尾"))?;
    if end <= start {
        bail!("JSON 格式不完整");
    }
    let slice = &s[start..=end];
    serde_json::from_str(slice).or_else(|_| {
        // 常見問題：尾端多餘逗號
        let fixed = slice.replace(",]", "]").replace(",}", "}").replace(", ]", "]").replace(", }", "}");
        serde_json::from_str(&fixed).map_err(|e| anyhow!("JSON 解析失敗：{e}"))
    })
}

fn chat_json(client: &Client, system: &str, content: Vec<Value>, tx: &Sender<WorkerMsg>, cancel: &AtomicBool) -> Result<Value> {
    let mut last_err = anyhow!("未知錯誤");
    for attempt in 1..=3 {
        if cancel.load(Ordering::Relaxed) {
            bail!("已取消");
        }
        match client.chat(system, content.clone()).and_then(|r| extract_json(&r)) {
            Ok(v) => return Ok(v),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("HTTP 401") || msg.contains("HTTP 413") {
                    return Err(e);
                }
                let _ = tx.send(WorkerMsg::Log(format!("第 {attempt} 次嘗試失敗：{msg}")));
                last_err = e;
            }
        }
    }
    Err(last_err)
}

// ───────────────────────── 解析 ─────────────────────────

fn num(v: &Value) -> Option<f32> {
    v.as_f64().map(|f| f as f32).or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn norm_coord(v: f32) -> f32 {
    // 模型應輸出 0–1000；若給 0–1 也能接受
    if v > 1.5 { (v / 1000.0).clamp(0.0, 1.0) } else { v.clamp(0.0, 1.0) }
}

fn parse_point(v: &Value) -> Option<[f32; 2]> {
    let a = v.as_array()?;
    if a.len() < 2 {
        return None;
    }
    Some([norm_coord(num(&a[0])?), norm_coord(num(&a[1])?)])
}

fn parse_box(v: &Value) -> Option<[f32; 4]> {
    let a = v.as_array()?;
    if a.len() < 4 {
        return None;
    }
    let (x1, y1, x2, y2) =
        (norm_coord(num(&a[0])?), norm_coord(num(&a[1])?), norm_coord(num(&a[2])?), norm_coord(num(&a[3])?));
    Some([x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)])
}

fn str_of(v: &Value) -> String {
    v.as_str().map(|s| s.trim().to_string()).unwrap_or_default()
}

fn panel_number(p: &Value) -> Option<usize> {
    num(&p["panel"]).or_else(|| num(&p["index"])).map(|f| f.round() as usize)
}

fn parse_analysis(p: &Value) -> PanelAnalysis {
    PanelAnalysis {
        scene: str_of(&p["scene"]),
        mood: str_of(&p["mood"]),
        characters: p["characters"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| SeenCharacter {
                        name: str_of(&c["name"]),
                        face: parse_point(&c["face"]),
                        expression: str_of(&c["expression"]),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        empty_areas: p["empty_areas"].as_array().map(|a| a.iter().filter_map(parse_box).collect()).unwrap_or_default(),
    }
}

fn parse_script(p: &Value) -> Vec<ScriptLine> {
    p["captions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let t = str_of(&c["text"]);
                    if t.is_empty() {
                        return None;
                    }
                    Some(ScriptLine { kind: BubbleKind::from_llm(&str_of(&c["type"])), speaker: str_of(&c["speaker"]), text: t })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ───────────────────────── 提示詞 ─────────────────────────

fn character_roster(chars: &[Character]) -> String {
    if chars.is_empty() {
        return "（使用者沒有提供角色資料，請自行為角色取合適的名字並保持前後一致）".into();
    }
    chars
        .iter()
        .map(|c| {
            if c.description.trim().is_empty() {
                format!("- {}", c.name)
            } else {
                format!("- {}：{}", c.name, c.description.trim())
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const ANALYZE_SYSTEM: &str = "你是專業的漫畫分鏡分析師。你會看到角色參考圖與一系列依閱讀順序排列的連續畫面。\
請仔細比對髮型、髮色、服裝、配件與體型，辨認每格畫面中出現的是哪個已知角色；\
無法對應到已知角色的人物請標示為「路人」或依外觀取簡短代稱，並在之後的畫面保持同一代稱。\
座標一律使用 0～1000 的整數，x 由左到右、y 由上到下。只輸出 JSON，不要任何其他文字。";

fn analyze_instructions(story: &Story, roster: &str) -> String {
    format!(
        "【已知角色】\n{roster}\n\n【故事風格】{}\n【劇情方向】{}\n\n\
請對每一格畫面輸出：\n\
- scene：這一格發生什麼事（2～3 句，具體描述動作、互動、場景與時間感）\n\
- mood：氣氛\n\
- characters：畫面中每個人物，含 name（已知角色名）、face（臉部中心座標 [x,y]）、expression（表情與動作）\n\
- empty_areas：適合放對話框且不會擋到重點的空白區域 [x1,y1,x2,y2]，最多 4 個（例如天空、牆面、背景）\n\n\
輸出格式：\n\
{{\"panels\":[{{\"panel\":1,\"scene\":\"...\",\"mood\":\"...\",\"characters\":[{{\"name\":\"...\",\"face\":[500,300],\"expression\":\"...\"}}],\"empty_areas\":[[0,0,400,200]]}}]}}",
        if story.style.trim().is_empty() { "（未指定）" } else { story.style.trim() },
        if story.direction.trim().is_empty() { "（未指定，請依畫面推敲）" } else { story.direction.trim() },
    )
}

const SCRIPT_SYSTEM: &str = "你是經驗豐富的漫畫編劇，擅長為既有的分鏡畫面撰寫自然、有個性、節奏好的台詞。\
你會拿到每一格畫面的分析，必須讓整個故事前後連貫、角色口吻一致，並符合指定風格。只輸出 JSON。";

fn script_instructions(story: &Story, roster: &str, panels_text: &str, from: usize, to: usize, prior: &str) -> String {
    format!(
        "【作品名稱】{}\n【故事風格】{}\n【劇情方向】{}\n【語言】{}\n\n【角色】\n{roster}\n\n\
【全部畫面分析（依閱讀順序）】\n{panels_text}\n\n{prior}\
請為第 {from} 格到第 {to} 格撰寫漫畫文字。每格可以包含以下元素（不強求每格都有，但{}）：\n\
- time：時間或地點字卡，只在時間跳躍或換場景時使用，例如「三天後」「放學後的屋頂」\n\
- narration：旁白，交代背景、心境或推進劇情\n\
- speech：角色說話。同一角色在一格裡可以有兩個以上的對話框，一個框放一句話\n\
- thought：角色內心話\n\
- shout：大喊、驚叫\n\
- sfx：狀聲詞，例如「砰！」「咚咚」「唰——」\n\n\
規則：\n\
1. speech / thought / shout 必須填 speaker，且盡量是該格畫面中出現的角色名（與分析中的 name 一致）；畫面外的人說話時 speaker 填「畫外」。\n\
2. 每個框的文字要短，像真正的漫畫，單句以 4～18 字為佳，最長不超過 28 字。\n\
3. 同一格裡的元素依閱讀順序排列（先說的先列）。\n\
4. 台詞要推動劇情、表現角色個性，不要只是描述畫面。\n\n\
輸出格式：\n\
{{\"panels\":[{{\"panel\":{from},\"captions\":[{{\"type\":\"speech\",\"speaker\":\"角色名\",\"text\":\"...\"}}]}}]}}",
        if story.title.trim().is_empty() { "（未命名）" } else { story.title.trim() },
        if story.style.trim().is_empty() { "（未指定）" } else { story.style.trim() },
        if story.direction.trim().is_empty() { "（未指定，請依畫面發展合理的故事）" } else { story.direction.trim() },
        story.language,
        story.density.guidance(),
    )
}

fn describe_panel(i: usize, a: &PanelAnalysis) -> String {
    let chars = a
        .characters
        .iter()
        .map(|c| if c.expression.is_empty() { c.name.clone() } else { format!("{}（{}）", c.name, c.expression) })
        .collect::<Vec<_>>()
        .join("、");
    format!("第 {} 格：{}｜人物：{}｜氣氛：{}", i + 1, a.scene, if chars.is_empty() { "無".into() } else { chars }, a.mood)
}

fn describe_script(lines: &[ScriptLine]) -> String {
    lines
        .iter()
        .map(|l| {
            if l.kind.has_speaker() {
                format!("{}［{}］：{}", l.speaker, l.kind.label(), l.text)
            } else {
                format!("［{}］{}", l.kind.label(), l.text)
            }
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

// ───────────────────────── 流程 ─────────────────────────

pub fn run(job: Job, tx: Sender<WorkerMsg>) {
    match run_inner(&job, &tx) {
        Ok(()) => {
            let _ = tx.send(WorkerMsg::Finished);
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Failed(e.to_string()));
        }
    }
}

fn run_inner(job: &Job, tx: &Sender<WorkerMsg>) -> Result<()> {
    if job.settings.model.trim().is_empty() {
        bail!("尚未設定模型名稱（請到「設定」分頁填寫）");
    }
    if job.settings.api_url.trim().is_empty() {
        bail!("尚未設定 API 網址（請到「設定」分頁填寫）");
    }
    if job.settings.api_key.trim().is_empty() {
        bail!("尚未設定 API 金鑰（請到「設定」分頁輸入，或設定環境變數 H200_API_KEY）");
    }
    if job.pages.is_empty() {
        bail!("沒有任何頁面圖片");
    }
    let client = Client::new(&job.settings)?;
    let roster = character_roster(&job.characters);
    let n = job.pages.len();
    let mut analyses = job.analyses.clone();
    analyses.resize(n, None);

    // ── 第一階段：看圖 ──
    let todo: Vec<usize> = (0..n).filter(|i| analyses[*i].is_none()).collect();
    if !todo.is_empty() {
        let _ = tx.send(WorkerMsg::Log(format!("第一階段：分析 {} 張畫面…", todo.len())));
        // 角色參考圖只編碼一次
        let mut ref_content = Vec::new();
        for c in &job.characters {
            let imgs: Vec<&PathBuf> = c.images.iter().take(job.settings.ref_per_character).collect();
            if imgs.is_empty() {
                continue;
            }
            ref_content.push(text(format!("角色參考圖：「{}」", c.name)));
            for p in imgs {
                match encode_image(p) {
                    Ok(v) => ref_content.push(v),
                    Err(e) => {
                        let _ = tx.send(WorkerMsg::Log(format!("略過參考圖：{e}")));
                    }
                }
            }
        }
        // 原圖原尺寸送出，所以除了張數上限外，也依檔案大小分批（每批 JSON 約控制在 128 MiB 內）
        let batch = job.settings.batch_size.max(1);
        let ref_bytes: u64 = ref_content.iter().map(|v| v.to_string().len() as u64).sum();
        let budget: u64 = (128u64 << 20).saturating_sub(ref_bytes).max(16 << 20);
        let mut chunks: Vec<Vec<usize>> = Vec::new();
        let mut cur: Vec<usize> = Vec::new();
        let mut cur_bytes = 0u64;
        for &i in &todo {
            let size = std::fs::metadata(&job.pages[i]).map(|m| m.len()).unwrap_or(0) * 4 / 3 + 64;
            if !cur.is_empty() && (cur.len() >= batch || cur_bytes + size > budget) {
                chunks.push(std::mem::take(&mut cur));
                cur_bytes = 0;
            }
            cur.push(i);
            cur_bytes += size;
        }
        if !cur.is_empty() {
            chunks.push(cur);
        }
        let mut done_count = 0usize;
        for chunk in &chunks {
            let chunk = chunk.as_slice();
            if job.cancel.load(Ordering::Relaxed) {
                bail!("已取消");
            }
            let _ = tx.send(WorkerMsg::Progress(
                done_count as f32 / todo.len() as f32 * 0.6,
                format!("看圖中：第 {}～{} 格", chunk[0] + 1, chunk[chunk.len() - 1] + 1),
            ));
            let mut content = vec![text(analyze_instructions(&job.story, &roster))];
            content.extend(ref_content.iter().cloned());
            // 前情摘要（最近 8 格）
            let first = chunk[0];
            let prior: Vec<String> = (first.saturating_sub(8)..first)
                .filter_map(|i| analyses[i].as_ref().map(|a| describe_panel(i, a)))
                .collect();
            if !prior.is_empty() {
                content.push(text(format!("【前面畫面摘要】\n{}", prior.join("\n"))));
            }
            content.push(text("以下是本批要分析的畫面："));
            for &i in chunk {
                content.push(text(format!("【畫面 {}】", i + 1)));
                content.push(encode_image(&job.pages[i])?);
            }
            content.push(text(format!(
                "請輸出畫面 {} 的分析 JSON，panel 欄位使用上面標示的畫面編號。",
                chunk.iter().map(|i| (i + 1).to_string()).collect::<Vec<_>>().join("、")
            )));

            let v = chat_json(&client, ANALYZE_SYSTEM, content, tx, &job.cancel)?;
            let panels = v["panels"].as_array().cloned().unwrap_or_default();
            for (k, p) in panels.iter().enumerate() {
                let idx = panel_number(p)
                    .and_then(|num| num.checked_sub(1))
                    .filter(|i| chunk.contains(i))
                    .or_else(|| chunk.get(k).copied());
                if let Some(i) = idx {
                    let a = parse_analysis(p);
                    analyses[i] = Some(a.clone());
                    let _ = tx.send(WorkerMsg::Analysis(i, a));
                }
            }
            for &i in chunk {
                if analyses[i].is_none() {
                    let _ = tx.send(WorkerMsg::Log(format!("第 {} 格沒有拿到分析，將以空白分析繼續", i + 1)));
                    analyses[i] = Some(PanelAnalysis::default());
                }
            }
            done_count += chunk.len();
        }
    }

    // ── 第二階段：編劇 ──
    let panels_text = analyses
        .iter()
        .enumerate()
        .map(|(i, a)| describe_panel(i, a.as_ref().unwrap_or(&PanelAnalysis::default())))
        .collect::<Vec<_>>()
        .join("\n");
    let (from, to) = job.script_range.unwrap_or((0, n));
    let mut written: Vec<Vec<ScriptLine>> = job.existing_script.clone();
    written.resize(n, Vec::new());
    let step = job.settings.script_batch.max(1);
    let mut start = from;
    let _ = tx.send(WorkerMsg::Log("第二階段：撰寫台詞…".into()));
    while start < to {
        if job.cancel.load(Ordering::Relaxed) {
            bail!("已取消");
        }
        let end = (start + step).min(to);
        let _ = tx.send(WorkerMsg::Progress(
            0.6 + (start - from) as f32 / (to - from).max(1) as f32 * 0.4,
            format!("編劇中：第 {}～{} 格", start + 1, end),
        ));
        let prior_lines: Vec<String> = (start.saturating_sub(6)..start)
            .filter(|i| !written[*i].is_empty())
            .map(|i| format!("第 {} 格：{}", i + 1, describe_script(&written[i])))
            .collect();
        let after_lines: Vec<String> = if job.script_range.is_some() {
            (end..(end + 3).min(n))
                .filter(|i| !written[*i].is_empty())
                .map(|i| format!("第 {} 格：{}", i + 1, describe_script(&written[i])))
                .collect()
        } else {
            Vec::new()
        };
        let mut prior = String::new();
        if !prior_lines.is_empty() {
            prior.push_str(&format!("【前面已寫好的台詞，請自然接續】\n{}\n\n", prior_lines.join("\n")));
        }
        if !after_lines.is_empty() {
            prior.push_str(&format!("【後面已寫好的台詞，請與之銜接】\n{}\n\n", after_lines.join("\n")));
        }
        let content = vec![text(script_instructions(&job.story, &roster, &panels_text, start + 1, end, &prior))];
        let v = chat_json(&client, SCRIPT_SYSTEM, content, tx, &job.cancel)?;
        let panels = v["panels"].as_array().cloned().unwrap_or_default();
        let range: Vec<usize> = (start..end).collect();
        for (k, p) in panels.iter().enumerate() {
            let idx = panel_number(p)
                .and_then(|num| num.checked_sub(1))
                .filter(|i| range.contains(i))
                .or_else(|| range.get(k).copied());
            if let Some(i) = idx {
                let lines = parse_script(p);
                written[i] = lines.clone();
                let _ = tx.send(WorkerMsg::Script(i, lines));
            }
        }
        start = end;
    }
    let _ = tx.send(WorkerMsg::Progress(1.0, "完成".into()));
    Ok(())
}
