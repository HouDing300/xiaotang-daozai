//! 邮件正文的轻量排版：把邮件拆成「标题 / 段落 / 列表 / 引用 / 分隔线」，
//! 让阅读窗口不再是一整段纯文本。不引入 HTML 渲染引擎，只保留能提升可读性的结构。

use crate::mail;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// 正文里的大标题
    Title,
    /// 小标题
    Heading,
    Body,
    Bullet,
    Quote,
    /// 段落之间的空隙（不是一条线）
    Gap,
    /// 真正的分隔线
    Divider,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub kind: Kind,
    pub text: String,
}

/// 正文排版行。HTML 原文优先；纯图片/空壳 HTML 会退回 text/plain，
/// 避免营销邮件只剩一片空白。两者都按「实际文字量」择优。
pub fn lines(html: &str, text: &str) -> Vec<Line> {
    let plain = if text.trim().is_empty() { Vec::new() } else { from_text(text) };
    let rich = if html.trim().is_empty() { Vec::new() } else { from_html(html) };
    let score = |v: &[Line]| -> usize {
        v.iter()
            .filter(|l| !matches!(l.kind, Kind::Gap | Kind::Divider))
            .map(|l| l.text.chars().count())
            .sum()
    };
    // 只在 HTML 明显是空壳（纯图片、只剩追踪像素）时回退；结构化排版本身
    // 会丢掉标签与空白，短邮件里字符数天然略少于 text/plain，不能用 >= 比较。
    if rich.is_empty() || score(&rich) * 2 < score(&plain) { plain } else { rich }
}

/// 列表项与阅读窗口共用。真实 HTML 邮件必须走 `from_html`：
/// multipart 邮件的 text/plain 版本通常已把标题、列表和段落全部压平。
pub fn item_lines(item: &mail::Item) -> Vec<Line> {
    lines(&item.html, &item.summary)
}

fn from_text(text: &str) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let mut gap = false;
    for raw in text.replace("\r\n", "\n").lines() {
        let line = raw.trim();
        if line.is_empty() {
            gap = true;
            continue;
        }
        let (kind, body) = split_marked(line);
        if kind == Kind::Divider {
            if !out.is_empty() && !matches!(out.last(), Some(Line { kind: Kind::Divider, .. })) {
                out.push(Line { kind: Kind::Divider, text: String::new() });
            }
            gap = false;
            continue;
        }
        let body = body.trim();
        if body.is_empty() {
            gap = true;
            continue;
        }
        // 列表项之间不额外留白，其余段落之间留一点。
        if gap && !out.is_empty() && !matches!(out.last(), Some(Line { kind: Kind::Gap | Kind::Divider, .. })) {
            out.push(Line { kind: Kind::Gap, text: String::new() });
        }
        gap = false;
        out.push(Line { kind, text: body.to_string() });
    }
    while matches!(out.last(), Some(Line { kind: Kind::Gap | Kind::Divider, .. })) {
        out.pop();
    }
    out
}

/// 行首标记 → 块类型。`## ` 标题、`• ` 列表、`> ` 引用、`---` 分隔线。
fn split_marked(line: &str) -> (Kind, &str) {
    if let Some(rest) = line.strip_prefix("## ") {
        (Kind::Heading, rest)
    } else if let Some(rest) = line.strip_prefix("• ") {
        (Kind::Bullet, rest)
    } else if let Some(rest) = line.strip_prefix("> ") {
        (Kind::Quote, rest)
    } else if let Some(rest) = line.strip_prefix("# ") {
        (Kind::Title, rest)
    } else if line == "---" {
        (Kind::Divider, "")
    } else {
        (Kind::Body, line)
    }
}

fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        let tail = &rest[index..];
        let end = tail.find(';').filter(|end| *end <= 10);
        let Some(end) = end else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "nbsp" | "#160" => Some(' '),
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "middot" | "#183" => Some('·'),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "copy" => Some('©'),
            "reg" => Some('®'),
            other => other
                .strip_prefix("#x")
                .or_else(|| other.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| other.strip_prefix('#').and_then(|dec| dec.parse::<u32>().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn from_html(html: &str) -> Vec<Line> {
    use std::sync::OnceLock;
    static SCRIPT: OnceLock<regex::Regex> = OnceLock::new();
    static MARK: OnceLock<regex::Regex> = OnceLock::new();
    static STRONG: OnceLock<regex::Regex> = OnceLock::new();
    static CELL: OnceLock<regex::Regex> = OnceLock::new();
    static HARD: OnceLock<regex::Regex> = OnceLock::new();
    static SOFT: OnceLock<regex::Regex> = OnceLock::new();
    static TAGS: OnceLock<regex::Regex> = OnceLock::new();

    let script = SCRIPT.get_or_init(|| {
        regex::Regex::new(r"(?is)<(?:script|style|head)[^>]*>.*?</(?:script|style|head)>").expect("script")
    });
    let mark = MARK.get_or_init(|| {
        regex::Regex::new(r"(?is)<hr\s*/?>|<h[12][^>]*>|<h[3-6][^>]*>|<li[^>]*>|<blockquote[^>]*>").expect("mark")
    });
    let strong = STRONG.get_or_init(|| regex::Regex::new(r"(?i)</?(?:strong|b)\b[^>]*>").expect("strong"));
    let cell = CELL.get_or_init(|| regex::Regex::new(r"(?i)</t[dh]>").expect("cell"));
    let hard = HARD.get_or_init(|| {
        regex::Regex::new(r"(?i)</p>|</h[1-6]>|</blockquote>|</table>|</ul>|</ol>|</pre>").expect("hard")
    });
    let soft = SOFT.get_or_init(|| regex::Regex::new(r"(?i)<br\s*/?>|</div>|</tr>|</li>").expect("soft"));
    let tags = TAGS.get_or_init(|| regex::Regex::new(r"(?s)<[^>]+>").expect("tag"));

    let s = script.replace_all(html, " ");
    let s = mark.replace_all(&s, |caps: &regex::Captures| {
        let raw = caps.get(0).map(|m| m.as_str()).unwrap_or_default().to_ascii_lowercase();
        if raw.starts_with("<hr") {
            "\n\n---\n\n".to_string()
        } else if raw.starts_with("<h1") || raw.starts_with("<h2") {
            "\n\n# ".to_string()
        } else if raw.starts_with("<h") {
            "\n\n## ".to_string()
        } else if raw.starts_with("<li") {
            "\n• ".to_string()
        } else {
            "\n> ".to_string()
        }
    });
    let s = strong.replace_all(&s, "**");
    let s = cell.replace_all(&s, " · ");
    let s = hard.replace_all(&s, "\n\n");
    let s = soft.replace_all(&s, "\n");
    let s = tags.replace_all(&s, "");
    let s = decode(&s);

    let mut out: Vec<Line> = Vec::new();
    let mut gap = false;
    for raw in s.split('\n') {
        let joined = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        let line = joined.trim().trim_end_matches('·').trim();
        if line.is_empty() {
            gap = true;
            continue;
        }
        let (kind, body) = split_marked(line);
        if kind == Kind::Divider {
            if !out.is_empty() && !matches!(out.last(), Some(Line { kind: Kind::Divider, .. })) {
                out.push(Line { kind: Kind::Divider, text: String::new() });
            }
            gap = false;
            continue;
        }
        if gap && !out.is_empty() && !matches!(out.last(), Some(Line { kind: Kind::Gap | Kind::Divider, .. })) {
            out.push(Line { kind: Kind::Gap, text: String::new() });
        }
        gap = false;
        out.push(Line { kind, text: body.to_string() });
    }
    while matches!(out.last(), Some(Line { kind: Kind::Gap | Kind::Divider, .. })) {
        out.pop();
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_headings_lists_and_quotes_keep_their_kind() {
        let html = r#"<html><head><style>p{color:red}</style></head><body>
            <h1>本周周报</h1><p>完成 <b>三件事</b>：</p>
            <ul><li>修复登录</li><li>同步版本</li></ul>
            <blockquote>注意：周五前提交</blockquote><hr><p>谢谢</p></body></html>"#;
        let lines = lines(html, "");
        assert_eq!(lines[0].kind, Kind::Title);
        assert_eq!(lines[0].text, "本周周报");
        assert!(lines.iter().any(|l| l.kind == Kind::Bullet && l.text == "修复登录"));
        assert!(lines.iter().any(|l| l.kind == Kind::Quote && l.text.contains("周五前提交")));
        assert!(lines.iter().any(|l| l.kind == Kind::Divider));
        assert!(!lines.iter().any(|l| l.text.contains("color:red")), "style 应被丢弃");
    }

    #[test]
    fn entities_are_decoded() {
        let lines = lines("<p>价格 &lt; 100 &amp; 包邮&nbsp;·&nbsp;限今天</p>", "");
        assert_eq!(lines[0].text, "价格 < 100 & 包邮 · 限今天");
    }

    #[test]
    fn plain_text_marks_become_blocks() {
        let text = "## 今日摘要\n第一段\n\n• 事项一\n• 事项二\n\n> 提醒：明天截止\n---\n最后一行";
        let lines = lines("", text);
        assert_eq!(lines[0].kind, Kind::Heading);
        assert_eq!(lines[1].kind, Kind::Body);
        assert_eq!(lines[2].kind, Kind::Gap);
        assert_eq!(lines[3].kind, Kind::Bullet);
        assert!(lines.iter().any(|l| l.kind == Kind::Quote && l.text.starts_with("提醒")));
        assert!(lines.iter().any(|l| l.kind == Kind::Divider));
        assert_eq!(lines.last().unwrap().text, "最后一行");
    }

    #[test]
    fn real_newsletter_html_keeps_structure_and_bold() {
        let html = r#"<html><head><style>p{color:red}</style></head><body>
            <h1>订单确认</h1>
            <p>您好，<strong>订单已支付</strong>，明细如下：</p>
            <table><tr><td>商品</td><td>金额</td></tr><tr><td>会员</td><td>¥99</td></tr></table>
            <ul><li>发货时间：明天</li><li>发票：电子</li></ul>
            <blockquote>如有疑问请联系客服</blockquote><hr><p>感谢支持</p></body></html>"#;
        let out = lines(html, "订单确认 您好 订单已支付");
        assert_eq!(out[0].kind, Kind::Title, "h1 应是大标题");
        assert!(out.iter().any(|l| l.text.contains("**订单已支付**")), "strong 应保留粗体标记");
        assert!(out.iter().any(|l| l.text == "商品 · 金额"), "表格单元格应分隔而不是粘连");
        assert_eq!(out.iter().filter(|l| l.kind == Kind::Bullet).count(), 2);
        assert!(out.iter().any(|l| l.kind == Kind::Quote));
        assert!(out.iter().any(|l| l.kind == Kind::Divider));
        assert!(!out.iter().any(|l| l.text.contains("color:red")));
    }

    #[test]
    fn image_only_html_falls_back_to_plain_text() {
        let html = r#"<html><body><img src="cid:a"><img src="cid:b"></body></html>"#;
        let out = lines(html, "尊敬的客户，您的账单已生成。");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "尊敬的客户，您的账单已生成。");
    }

    #[test]
    fn item_lines_uses_html_even_when_plain_text_exists() {
        let item = mail::Item {
            account: "a@qq.com".into(), from: "团队".into(), subject: "周报".into(),
            time: "09:41".into(), code: None, received_unix: None,
            summary: "本周周报 完成三件事".into(),
            html: "<h2>本周周报</h2><ul><li>完成三件事</li></ul>".into(),
            links: vec![], meeting: None, uid: 7,
        };
        let out = item_lines(&item);
        assert_eq!(out[0].kind, Kind::Title, "不能退化成压平的 text/plain");
        assert!(out.iter().any(|l| l.kind == Kind::Bullet && l.text == "完成三件事"));
    }

    #[test]
    fn structures_free_html_stays_flat() {
        let lines = lines("<div>一</div><div>二</div><div>三</div>", "");
        assert_eq!(lines.iter().filter(|l| l.kind == Kind::Body).count(), 3);
        assert!(!lines.iter().any(|l| l.kind == Kind::Divider));
    }
}
