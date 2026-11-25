//! 專案資料模型：角色、故事設定、頁面、對話框，以及應用程式設定。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BubbleKind {
    /// 一般說話（橢圓對話框＋尾巴）
    Speech,
    /// 內心獨白（雲朵框＋小圓點尾巴）
    Thought,
    /// 大喊（爆炸框）
    Shout,
    /// 旁白（方框）
    Narration,
    /// 時間／地點字卡（黑底白字）
    Time,
    /// 狀聲詞（無框、描邊大字）
    Sfx,
}

impl BubbleKind {
    pub const ALL: [BubbleKind; 6] = [
        BubbleKind::Speech,
        BubbleKind::Thought,
        BubbleKind::Shout,
        BubbleKind::Narration,
        BubbleKind::Time,
        BubbleKind::Sfx,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BubbleKind::Speech => "對話",
            BubbleKind::Thought => "內心話",
            BubbleKind::Shout => "大喊",
            BubbleKind::Narration => "旁白",
            BubbleKind::Time => "時間/地點",
            BubbleKind::Sfx => "狀聲詞",
        }
    }

    pub fn from_llm(s: &str) -> BubbleKind {
        match s.trim().to_ascii_lowercase().as_str() {
            "thought" | "think" | "inner" | "monologue" => BubbleKind::Thought,
            "shout" | "yell" | "scream" => BubbleKind::Shout,
            "narration" | "narrator" | "caption" => BubbleKind::Narration,
            "time" | "timeline" | "place" | "location" => BubbleKind::Time,
            "sfx" | "sound" | "onomatopoeia" => BubbleKind::Sfx,
            _ => BubbleKind::Speech,
        }
    }

    pub fn has_speaker(self) -> bool {
        matches!(self, BubbleKind::Speech | BubbleKind::Thought | BubbleKind::Shout)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bubble {
    pub id: u64,
    pub kind: BubbleKind,
    pub speaker: String,
    pub text: String,
    /// 中心點（相對座標 0..1）
    pub pos: [f32; 2],
    /// 尾巴指向（相對座標 0..1），None 表示沒有尾巴
    pub tail: Option<[f32; 2]>,
    /// 字級，單位為「圖片 sqrt(寬×高) 的百分比」
    pub font_size: f32,
    pub vertical: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SeenCharacter {
    pub name: String,
    /// 臉部中心（相對座標 0..1）
    pub face: Option<[f32; 2]>,
    pub expression: String,
}

/// 第一階段（看圖）對單張畫面的分析結果
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PanelAnalysis {
    pub scene: String,
    pub mood: String,
    pub characters: Vec<SeenCharacter>,
    /// 適合放字的空白區域 [x1, y1, x2, y2]（相對座標 0..1）
    pub empty_areas: Vec<[f32; 4]>,
}

/// 第二階段（編劇）對單張畫面產生的一句台詞
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScriptLine {
    pub kind: BubbleKind,
    pub speaker: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub path: PathBuf,
    #[serde(default)]
    pub analysis: Option<PanelAnalysis>,
    #[serde(default)]
    pub script: Vec<ScriptLine>,
    #[serde(default)]
    pub bubbles: Vec<Bubble>,
}

impl Page {
    pub fn new(path: PathBuf) -> Self {
        Self { path, analysis: None, script: Vec::new(), bubbles: Vec::new() }
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Character {
    pub name: String,
    /// 個性、外觀、與其他角色的關係、說話口吻等
    pub description: String,
    pub images: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Density {
    Few,
    Normal,
    Many,
}

impl Density {
    pub fn label(self) -> &'static str {
        match self {
            Density::Few => "精簡",
            Density::Normal => "適中",
            Density::Many => "豐富",
        }
    }
    pub fn guidance(self) -> &'static str {
        match self {
            Density::Few => "每格 0～2 個文字元素，只在必要時說話，留白多。",
            Density::Normal => "每格 1～4 個文字元素，大部分畫面都要有台詞或旁白。",
            Density::Many => "每格 2～6 個文字元素，幾乎每格都要有對話，同一角色可連說兩三句，適度加入內心話、旁白與狀聲詞。",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Story {
    pub title: String,
    /// 風格（例如：輕鬆搞笑、熱血少年漫、懸疑、純愛）
    pub style: String,
    /// 劇情方向／大綱
    pub direction: String,
    pub language: String,
    pub density: Density,
}

impl Default for Story {
    fn default() -> Self {
        Self {
            title: String::new(),
            style: String::new(),
            direction: String::new(),
            language: "繁體中文（台灣用語）".into(),
            density: Density::Normal,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Project {
    pub characters: Vec<Character>,
    pub story: Story,
    pub pages: Vec<Page>,
    #[serde(default)]
    pub next_id: u64,
    /// 輸出資料夾：「原資料夾名稱(翻譯)」或「第一張圖片名稱(翻譯)」，與來源放在同一層
    #[serde(default)]
    pub output_dir: Option<PathBuf>,
}

/// 依來源（資料夾或圖片）決定輸出資料夾
pub fn output_dir_for(source: &Path) -> Option<PathBuf> {
    let parent = source.parent()?;
    let name = if source.is_dir() {
        source.file_name()?.to_string_lossy().into_owned()
    } else {
        source.file_stem()?.to_string_lossy().into_owned()
    };
    Some(parent.join(format!("{name}(翻譯)")))
}

impl Project {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn fresh_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub api_url: String,
    pub model: String,
    /// 只有勾選「記住金鑰」時才會寫入設定檔
    pub api_key: String,
    pub remember_key: bool,
    /// 每個角色最多送幾張參考圖
    pub ref_per_character: usize,
    /// 看圖階段每批幾張
    pub batch_size: usize,
    /// 編劇階段每批幾格
    pub script_batch: usize,
    pub max_tokens: u32,
    pub timeout_secs: u64,
    pub font_path: String,
    pub bold_font_path: String,
    /// 右到左閱讀（日式／台式漫畫）
    pub reading_rtl: bool,
    pub default_vertical: bool,
    pub default_font_size: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_url: "https://uat.heph-ai.net/h200-ollama/v1/chat/completions".into(),
            model: String::new(),
            api_key: String::new(),
            remember_key: false,
            ref_per_character: 2,
            batch_size: 6,
            script_batch: 12,
            max_tokens: 8192,
            timeout_secs: 900,
            font_path: r"C:\Windows\Fonts\msjh.ttc".into(),
            bold_font_path: r"C:\Windows\Fonts\msjhbd.ttc".into(),
            reading_rtl: true,
            default_vertical: true,
            default_font_size: 2.4,
        }
    }
}

impl Settings {
    fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("cg2manga").join("settings.json"))
    }

    pub fn load() -> Self {
        let mut s: Settings = Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if s.api_key.is_empty() {
            if let Ok(k) = std::env::var("H200_API_KEY") {
                s.api_key = k;
            }
        }
        s
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let Some(p) = Self::path() else { return Ok(()) };
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut copy = self.clone();
        if !copy.remember_key {
            copy.api_key.clear();
        }
        std::fs::write(p, serde_json::to_string_pretty(&copy)?)?;
        Ok(())
    }
}
